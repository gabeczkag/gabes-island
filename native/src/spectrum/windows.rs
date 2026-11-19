//! Windows spectrum: WASAPI loopback of the default render endpoint.
//!
//! Loopback capture needs no virtual cable and no permissions: the endpoint's
//! render stream is delivered as a capture stream. A worker thread owns the
//! audio client and publishes 32 normalised bands; the UI only reads an
//! `Arc<Mutex<[f32; 32]>>`.
//!
//! Bluetooth latency is not auto-detected on Windows (there is no cheap,
//! documented API for the endpoint's codec delay); set `widget_latency_ms`
//! explicitly when using Bluetooth headphones.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use island_core::config::MAX_BARS;
use island_core::spectrum::Spectrum;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use super::SpectrumSource;

#[derive(Default)]
struct Shared {
    levels: [f32; MAX_BARS],
    sample_rate: f32,
}

pub struct WasapiLoopback {
    shared: Arc<Mutex<Shared>>,
    active: Arc<AtomicBool>,
    started: bool,
}

impl WasapiLoopback {
    pub fn new() -> WasapiLoopback {
        WasapiLoopback {
            shared: Arc::new(Mutex::new(Shared::default())),
            active: Arc::new(AtomicBool::new(false)),
            started: false,
        }
    }

    fn start(&mut self) {
        self.started = true;
        let shared = self.shared.clone();
        let active = self.active.clone();
        thread::Builder::new()
            .name("wasapi-loopback".into())
            .spawn(move || unsafe { capture_loop(shared, active) })
            .ok();
    }
}

impl SpectrumSource for WasapiLoopback {
    fn levels(&mut self, playing: bool, _now_ms: u128) -> Option<[f32; MAX_BARS]> {
        // Only touch the audio device while something plays.
        self.active.store(playing, Ordering::Relaxed);
        if playing && !self.started {
            self.start();
        }
        let guard = self.shared.lock().ok()?;
        Some(guard.levels)
    }

    fn bluetooth(&self) -> bool {
        false
    }

    fn backend(&self) -> &'static str {
        "wasapi-loopback"
    }
}

/// Runs until the process exits; idles cheaply while `active` is false.
unsafe fn capture_loop(shared: Arc<Mutex<Shared>>, active: Arc<AtomicBool>) {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let mut client: Option<(IAudioClient, IAudioCaptureClient, usize, usize, u16)> = None;

    loop {
        if !active.load(Ordering::Relaxed) {
            thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        if client.is_none() {
            client = open_loopback();
            if client.is_none() {
                thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        }
        let Some((audio_client, capture, channels, rate, tag)) = &client else {
            continue;
        };
        let packet = match capture.GetNextPacketSize() {
            Ok(p) => p,
            Err(_) => {
                client = None;
                continue;
            }
        };
        if packet == 0 {
            thread::sleep(std::time::Duration::from_millis(15));
            continue;
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        if capture
            .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
            .is_err()
        {
            client = None;
            continue;
        }
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        if !silent && !data.is_null() && frames > 0 {
            let pcm = decode(data, frames as usize, *channels, *tag);
            let mut spectrum = Spectrum::new(*rate as f32);
            spectrum.feed(&pcm);
            if let Ok(mut g) = shared.lock() {
                g.levels = *spectrum.bars();
                g.sample_rate = *rate as f32;
            }
        }
        let _ = capture.ReleaseBuffer(frames);
        let _ = audio_client;
    }
}

unsafe fn open_loopback() -> Option<(IAudioClient, IAudioCaptureClient, usize, usize, u16)> {
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
    let client: IAudioClient = device.Activate::<IAudioClient>(CLSCTX_ALL, None).ok()?;
    let fmt = client.GetMixFormat().ok()?;
    let format = *fmt;
    let channels = format.nChannels.max(1) as usize;
    let rate = format.nSamplesPerSec as usize;
    let tag = format.wBitsPerSample;
    client
        .Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000, // 200 ms buffer
            0,
            fmt,
            None,
        )
        .ok()?;
    CoTaskMemFree(Some(fmt as *const _));
    let capture: IAudioCaptureClient = client.GetService::<IAudioCaptureClient>().ok()?;
    client.Start().ok()?;
    Some((client, capture, channels, rate, tag))
}

/// Mix formats are float32 or PCM16/32; down-mix to mono.
unsafe fn decode(data: *mut u8, frames: usize, channels: usize, bits: u16) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    match bits {
        32 => {
            let ptr = data as *const f32;
            let total = frames * channels;
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..channels {
                    acc += *ptr.add(f * channels + c);
                }
                out.push(acc / channels as f32);
            }
            let _ = total;
        }
        16 => {
            let ptr = data as *const i16;
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..channels {
                    acc += *ptr.add(f * channels + c) as f32 / 32768.0;
                }
                out.push(acc / channels as f32);
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_interleaved_float_to_mono() {
        let stereo = [0.5f32, -0.5, 0.25, 0.25];
        let bytes: Vec<u8> = stereo.iter().flat_map(|v| v.to_le_bytes()).collect();
        let pcm = unsafe { decode(bytes.as_ptr() as *mut u8, 2, 2, 32) };
        assert_eq!(pcm.len(), 2);
        assert!(pcm[0].abs() < 1e-6);
        assert!((pcm[1] - 0.25).abs() < 1e-6);
    }
}
