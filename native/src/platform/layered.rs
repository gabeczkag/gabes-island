//! Windows presenter: a layered window with per-pixel alpha.
//!
//! winit owns the HWND (no decorations, always on top, tool window); we add
//! `WS_EX_LAYERED | WS_EX_NOACTIVATE` and push each frame through
//! `UpdateLayeredWindow`. Because the pixels carry real alpha, the rounded
//! pill's corners and the fully transparent parts of the window are
//! click-through without any hit-test code.

use std::ffi::c_void;
use std::ptr;

use tiny_skia::Pixmap;
use windows_sys::Win32::Foundation::{HWND, POINT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, UpdateLayeredWindow, GWL_EXSTYLE, ULW_ALPHA,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

const AC_SRC_OVER: u8 = 0;
const AC_SRC_ALPHA: u8 = 1;

#[repr(C)]
struct BlendFunction {
    blend_op: u8,
    blend_flags: u8,
    source_constant_alpha: u8,
    alpha_format: u8,
}

fn hwnd_of(window: &Window) -> Option<HWND> {
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        _ => None,
    }
}

/// Adds the layered/tool-window styles before the first frame is presented.
pub fn prepare(window: &Window) {
    let Some(hwnd) = hwnd_of(window) else { return };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let add = (WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | add);
    }
}

/// True when a fullscreen app owns the foreground (hide the island then).
pub fn fullscreen_foreground() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    unsafe {
        match SHQueryUserNotificationState() {
            Ok(state) => state == QUNS_BUSY || state == QUNS_RUNNING_D3D_FULL_SCREEN,
            Err(_) => false,
        }
    }
}

pub struct Layered {
    hwnd: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut c_void,
    w: i32,
    h: i32,
}

impl Layered {
    pub fn new(window: &Window) -> Option<Layered> {
        let hwnd = hwnd_of(window)?;
        Some(Layered {
            hwnd,
            dc: ptr::null_mut(),
            bitmap: ptr::null_mut(),
            old: ptr::null_mut(),
            bits: ptr::null_mut(),
            w: 0,
            h: 0,
        })
    }

    /// (Re)creates the DIB when the surface size changes.
    unsafe fn ensure(&mut self, w: i32, h: i32) -> bool {
        if self.dc != ptr::null_mut() && self.w == w && self.h == h {
            return true;
        }
        self.release();
        let dc = CreateCompatibleDC(ptr::null_mut());
        if dc.is_null() {
            return false;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // negative height: top-down rows, matching our RGBA buffer
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits: *mut c_void = ptr::null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            DeleteDC(dc);
            return false;
        }
        let old = SelectObject(dc, bitmap);
        self.dc = dc;
        self.bitmap = bitmap;
        self.old = old;
        self.bits = bits;
        self.w = w;
        self.h = h;
        true
    }

    unsafe fn release(&mut self) {
        if self.dc != ptr::null_mut() {
            if self.old != ptr::null_mut() {
                SelectObject(self.dc, self.old);
            }
            DeleteDC(self.dc);
        }
        if self.bitmap != ptr::null_mut() {
            DeleteObject(self.bitmap);
        }
        self.dc = ptr::null_mut();
        self.bitmap = ptr::null_mut();
        self.old = ptr::null_mut();
        self.bits = ptr::null_mut();
        self.w = 0;
        self.h = 0;
    }

    pub fn present(&mut self, pm: &Pixmap) {
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        unsafe {
            if !self.ensure(w, h) {
                return;
            }
            // tiny-skia stores premultiplied RGBA; GDI wants premultiplied BGRA.
            let src = pm.data();
            let dst = std::slice::from_raw_parts_mut(self.bits as *mut u8, (w * h * 4) as usize);
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
            let dst_size = SIZE { cx: w, cy: h };
            let src_pos = POINT { x: 0, y: 0 };
            let blend = BlendFunction {
                blend_op: AC_SRC_OVER,
                blend_flags: 0,
                source_constant_alpha: 255,
                alpha_format: AC_SRC_ALPHA,
            };
            let screen_dc = GetDC(ptr::null_mut());
            UpdateLayeredWindow(
                self.hwnd,
                screen_dc,
                ptr::null(),
                &dst_size,
                self.dc,
                &src_pos,
                0,
                &blend as *const BlendFunction as *const _,
                ULW_ALPHA,
            );
            ReleaseDC(ptr::null_mut(), screen_dc);
        }
    }
}

impl Drop for Layered {
    fn drop(&mut self) {
        unsafe { self.release() };
    }
}
