//! Linux/X11 development presenter (softbuffer).
//!
//! This is not the shipped Linux app — that remains the Cairo/XShape binary in
//! the repository root. It exists so the native renderer can be run, profiled
//! and screenshotted on X11 while developing the Windows/macOS backends.

use std::num::NonZeroU32;
use std::sync::Arc;

use tiny_skia::Pixmap;
use winit::window::Window;

pub struct Soft {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
}

impl Soft {
    pub fn new(window: &Arc<Window>) -> Option<Soft> {
        // The surface owns what it needs from the context, so the context can
        // be dropped here (this is what softbuffer's own examples do).
        let context = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&context, window.clone()).ok()?;
        Some(Soft { surface })
    }

    pub fn present(&mut self, pm: &Pixmap) {
        let (w, h) = (pm.width(), pm.height());
        let (Some(w), Some(h)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        if self.surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buffer) = self.surface.buffer_mut() else {
            return;
        };
        for (dst, src) in buffer.iter_mut().zip(pm.pixels()) {
            *dst = ((src.alpha() as u32) << 24)
                | ((src.red() as u32) << 16)
                | ((src.green() as u32) << 8)
                | src.blue() as u32;
        }
        let _ = buffer.present();
    }
}
