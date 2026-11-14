//! The e-ink display, through FBInk.
//!
//! We write pixels straight into the memory-mapped framebuffer, then ask FBInk
//! to refresh just that rectangle with the waveform matching the refresh intent.

use std::ffi::CStr;

use anyhow::{Result, bail};
use fbink_sys as fb;

use crate::geom::Rect;
use crate::gfx::Canvas;
use crate::platform::{Orientation, RefreshMode, Update};

pub struct Display {
    fd: i32,
    cfg: fb::FBInkConfig,
    fb: *mut u8,
    fb_len: usize,
    stride: usize,
    /// The panel's native (portrait) size.
    pub width: u32,
    pub height: u32,
    /// The app draws a landscape canvas that `present` turns onto the panel.
    pub orientation: Orientation,
    pub dpi: u32,
    pub name: String,
    inverted: bool,
    /// How to turn raw touch coordinates into screen coordinates.
    pub touch_swap_axes: bool,
    pub touch_mirror_x: bool,
    pub touch_mirror_y: bool,
}

impl Display {
    pub fn open() -> Result<Self> {
        // SAFETY: plain FFI calls. FBInkConfig and FBInkState are C structs for
        // which all-zero is a valid (default) value, as FBInk documents.
        unsafe {
            let fd = fb::fbink_open();
            if fd < 0 {
                bail!("can't open the framebuffer");
            }
            let mut cfg: fb::FBInkConfig = std::mem::zeroed();
            cfg.is_quiet = true;
            if fb::fbink_init(fd, &cfg) < 0 {
                fb::fbink_close(fd);
                bail!("FBInk failed to initialise");
            }
            let mut st: fb::FBInkState = std::mem::zeroed();
            fb::fbink_get_state(&cfg, &mut st);

            let text = |chars: &[std::ffi::c_char]| CStr::from_ptr(chars.as_ptr()).to_string_lossy().into_owned();
            let name = text(&st.device_name);
            log::info!(
                "FBInk {}: {} ({}, {}), view {}x{}, screen {}x{}, stride {}, {} bpp, {} dpi, mtk={}, inverted={}, touch swap={} mirror x={} y={}",
                CStr::from_ptr(fb::fbink_version()).to_string_lossy(),
                name,
                text(&st.device_codename),
                text(&st.device_platform),
                st.view_width,
                st.view_height,
                st.screen_width,
                st.screen_height,
                st.scanline_stride,
                st.bpp,
                st.screen_dpi,
                st.is_mtk,
                st.inverted_grayscale,
                st.touch_swap_axes,
                st.touch_mirror_x,
                st.touch_mirror_y,
            );

            let fail = |msg: String| -> Result<Self> {
                fb::fbink_close(fd);
                bail!(msg)
            };
            if st.bpp != 8 {
                return fail(format!("only 8-bit grayscale screens are supported, this one is {} bpp", st.bpp));
            }
            if st.view_hori_origin != 0 || st.view_vert_origin != 0 || st.view_width != st.screen_width {
                return fail("screens with an offset viewport aren't supported yet".into());
            }
            let mut fb_len = 0usize;
            let ptr = fb::fbink_get_fb_pointer(fd, &mut fb_len);
            let (width, height, stride) = (st.view_width, st.view_height, st.scanline_stride as usize);
            if ptr.is_null() || fb_len < stride * height as usize {
                return fail(format!("framebuffer mapping is missing or too small ({fb_len} bytes)"));
            }

            Ok(Self {
                fd,
                cfg,
                fb: ptr,
                fb_len,
                stride,
                width,
                height,
                orientation: Orientation::Portrait,
                dpi: st.screen_dpi as u32,
                name,
                inverted: st.inverted_grayscale,
                touch_swap_axes: st.touch_swap_axes,
                touch_mirror_x: st.touch_mirror_x,
                touch_mirror_y: st.touch_mirror_y,
            })
        }
    }

    pub fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()> {
        let screen = Rect::new(0, 0, self.width as i32, self.height as i32);
        // SAFETY: the mapping is valid and at least `fb_len` bytes for as long
        // as the FBInk session is open (checked in `open`), and only we use it.
        let pixels = unsafe { std::slice::from_raw_parts_mut(self.fb, self.fb_len) };

        let (cw, ch) = self.orientation.screen_size(self.width, self.height);
        if (canvas.width(), canvas.height()) != (cw, ch) {
            bail!("canvas is {}x{}, expected {cw}x{ch}", canvas.width(), canvas.height());
        }
        let mut flashed = false;
        for u in updates {
            let r = match self.orientation {
                Orientation::Portrait => {
                    let Some(r) = u.rect.intersect(&screen) else { continue };
                    canvas.copy_gray(r, pixels, self.stride);
                    r
                }
                Orientation::Landscape => {
                    let Some(r) = canvas.copy_gray_rotated(u.rect, pixels, self.stride) else { continue };
                    r
                }
            };
            if self.inverted {
                for y in r.y..r.bottom() {
                    let row = y as usize * self.stride;
                    pixels[row + r.x as usize..row + r.right() as usize].iter_mut().for_each(|v| *v = !*v);
                }
            }
            let (wfm, flash) = match u.mode {
                RefreshMode::Fast => (fb::WFM_MODE_INDEX_E_WFM_DU, false),
                RefreshMode::Ui => (fb::WFM_MODE_INDEX_E_WFM_GL16, false),
                RefreshMode::Page => (fb::WFM_MODE_INDEX_E_WFM_REAGL, false),
                RefreshMode::Full => (fb::WFM_MODE_INDEX_E_WFM_GC16, true),
            };
            self.cfg.wfm_mode = wfm;
            self.cfg.is_flashing = flash;
            flashed |= flash;
            // SAFETY: FFI call with a valid fd and config.
            let rc = unsafe { fb::fbink_refresh(self.fd, r.y as u32, r.x as u32, r.w as u32, r.h as u32, &self.cfg) };
            if rc < 0 {
                log::warn!("refresh of {r:?} ({:?}) failed: {rc}", u.mode);
            }
        }
        // Let a flash finish before anything else is drawn over it.
        if flashed {
            self.wait_for_complete();
        }
        Ok(())
    }

    pub fn wait_for_complete(&mut self) {
        // SAFETY: FFI call; marker 0 means "the last refresh we sent".
        let rc = unsafe { fb::fbink_wait_for_complete(self.fd, 0) };
        if rc < 0 {
            log::debug!("wait for complete returned {rc}");
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: closes the session opened in `open`; the mapping isn't used after this.
        unsafe { fb::fbink_close(self.fd) };
    }
}
