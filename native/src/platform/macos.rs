//! macOS presenter: a borderless, non-opaque floating NSWindow whose layer
//! contents is replaced with the rendered frame.
//!
//! `CALayer.contents = CGImage` is the cheapest way to get per-pixel alpha on
//! macOS without Metal: the compositor blends the image for us. Mouse events
//! are received on the whole window rectangle (macOS has no per-pixel
//! hit-testing), which only matters for the transparent corners of the pill.

use std::ptr::NonNull;
use std::sync::Arc;

use core_graphics::color_space::CGColorSpace;
use core_graphics::data_provider::CGDataProvider;
use core_graphics::image::{CGImage, CGImageAlphaInfo};
use foreign_types::ForeignType;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSColor, NSView, NSWindow, NSWindowCollectionBehavior};
use objc2_quartz_core::CALayer;
use tiny_skia::Pixmap;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// `NSFloatingWindowLevel`: above normal windows, below system alerts.
const NS_FLOATING_WINDOW_LEVEL: isize = 3;
/// `kCGRenderingIntentDefault`
const RENDERING_INTENT_DEFAULT: u32 = 0;

pub struct MacLayer {
    view: NonNull<NSView>,
    layer: Retained<CALayer>,
    scale: f64,
}

impl MacLayer {
    pub fn new(window: &Window) -> Option<MacLayer> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        let view = NonNull::new(h.ns_view.as_ptr() as *mut NSView)?;
        let layer;
        unsafe {
            let view_ref = view.as_ref();
            view_ref.setWantsLayer(true);
            layer = view_ref.layer()?;
            layer.setMasksToBounds(true);
            if let Some(ns_window) = view_ref.window() {
                configure_window(&ns_window);
            }
        }
        let scale = window.scale_factor();
        layer.setContentsScale(scale);
        Some(MacLayer { view, layer, scale })
    }

    pub fn present(&mut self, pm: &Pixmap) {
        let (w, h) = (pm.width() as usize, pm.height() as usize);
        if w == 0 || h == 0 {
            return;
        }
        // tiny-skia gives premultiplied RGBA, which is exactly what
        // kCGImageAlphaPremultipliedLast expects on a big-endian RGBA layout.
        let buffer = Arc::new(pm.data().to_vec());
        let provider = CGDataProvider::from_buffer(buffer);
        let colorspace = CGColorSpace::create_device_rgb();
        let image = CGImage::new(
            w,
            h,
            8,
            32,
            w * 4,
            &colorspace,
            CGImageAlphaInfo::CGImageAlphaPremultipliedLast as u32,
            &provider,
            true,
            RENDERING_INTENT_DEFAULT,
        );
        unsafe {
            let contents: &AnyObject = &*(image.as_ptr() as *const AnyObject);
            self.layer.setContents(Some(contents));
            self.layer.setContentsScale(self.scale);
            // keep the backing view sized to the window
            if let Some(ns_window) = self.view.as_ref().window() {
                let _ = ns_window;
            }
        }
    }
}

unsafe fn configure_window(window: &NSWindow) {
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setLevel(NS_FLOATING_WINDOW_LEVEL);
    window.setIgnoresMouseEvents(false);
    window.setMovableByWindowBackground(false);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
}
