//! The e-ink display, through FBInk.
//!
//! We write pixels straight into the memory-mapped framebuffer, then ask FBInk
//! to refresh just that rectangle with the waveform matching the refresh intent.

use std::ffi::{CStr, CString};

use anyhow::{Result, bail};
use fbink_sys as fb;

use crate::gfx::Canvas;
use crate::platform::flash::FlashFence;
use crate::platform::framebuffer::{Format, Layout, Writer};
use crate::platform::waveform::{DarkVariant, PanelCaps, plan_refresh};
use crate::platform::{FrameCheck, Orientation, Unsupported, Update};

/// Static display & FBInk facts logged in the startup session header.
pub struct DisplaySession {
    pub fbink_version: String,
    pub device_name: String,
    pub codename: String,
    pub platform: String,
    pub device_id: u16,
    pub view_width: u32,
    pub view_height: u32,
    pub view_hori_origin: u8,
    pub view_vert_origin: u8,
    pub screen_width: u32,
    pub screen_height: u32,
    pub stride: u32,
    pub bpp: u32,
    pub pixel_format: String,
    pub has_color_panel: bool,
    pub has_eclipse_wfm: bool,
    pub dpi: u32,
    pub is_mtk: bool,
    pub inverted: bool,
}

fn pixel_format_name(fmt: fb::FBINK_PXFMT_INDEX_T) -> String {
    match fmt {
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_UNKNOWN => "UNKNOWN".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_Y4 => "Y4".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_Y8 => "Y8".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_BGR565 => "BGR565".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_RGB565 => "RGB565".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_BGR24 => "BGR24".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_RGB24 => "RGB24".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_BGRA => "BGRA".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_RGBA => "RGBA".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_BGR32 => "BGR32".into(),
        fb::FBINK_PXFMT_INDEX_E_FBINK_PXFMT_RGB32 => "RGB32".into(),
        other => format!("PXFMT({other})"),
    }
}

pub struct Display {
    fd: i32,
    cfg: fb::FBInkConfig,
    fb: *mut u8,
    fb_len: usize,
    /// Pixel size, stride, viewport and inversion: how canvas pixels land
    /// in the mapping.
    writer: Writer,
    /// The panel's native (portrait) size.
    pub width: u32,
    pub height: u32,
    /// The app draws a landscape canvas that `present` turns onto the panel.
    pub orientation: Orientation,
    pub dpi: u32,
    pub name: String,
    pub session: DisplaySession,
    /// How to turn raw touch coordinates into screen coordinates.
    pub touch_swap_axes: bool,
    pub touch_mirror_x: bool,
    pub touch_mirror_y: bool,
    /// Tracks an unawaited flashing refresh so the next write or handoff waits for it.
    flash_fence: FlashFence,
    /// Temporary Phase 1 dark-mode waveform variant (`VERSO_DARK_VARIANT`).
    dark_variant: DarkVariant,
    /// Refreshes FBInk refused, for the exit line.
    pub failed_refreshes: u32,
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
            // Don't let FBInk shift view_vert_origin to centre its bitmap text rows.
            cfg.no_viewport = true;
            if fb::fbink_init(fd, &cfg) < 0 {
                fb::fbink_close(fd);
                bail!("FBInk failed to initialise");
            }
            let mut st: fb::FBInkState = std::mem::zeroed();
            fb::fbink_get_state(&cfg, &mut st);

            let origin = Layout::fbink_origin(st.view_hori_origin, st.view_vert_origin, st.view_vert_offset);
            let text = |chars: &[std::ffi::c_char]| CStr::from_ptr(chars.as_ptr()).to_string_lossy().into_owned();
            let name = text(&st.device_name);
            let session = DisplaySession {
                fbink_version: CStr::from_ptr(fb::fbink_version()).to_string_lossy().into_owned(),
                device_name: name.clone(),
                codename: text(&st.device_codename),
                platform: text(&st.device_platform),
                device_id: st.device_id,
                view_width: st.view_width,
                view_height: st.view_height,
                view_hori_origin: origin.0 as u8,
                view_vert_origin: origin.1 as u8,
                screen_width: st.screen_width,
                screen_height: st.screen_height,
                stride: st.scanline_stride,
                bpp: st.bpp,
                pixel_format: pixel_format_name(st.pixel_format),
                has_color_panel: st.has_color_panel,
                has_eclipse_wfm: st.has_eclipse_wfm,
                dpi: st.screen_dpi as u32,
                is_mtk: st.is_mtk,
                inverted: st.inverted_grayscale,
            };

            // A screen Verso can't drive yet: say so on it, since the log
            // isn't where a user looks.
            let fail = |msg: String| -> Result<Self> {
                log::error!(
                    "FBInk {}: {} ({}, {}, id={:#06x}), view {}x{}+{}+{}, screen {}x{}, stride {}, {} bpp, pxfmt={}, color={}, eclipse={}, {} dpi, mtk={}, inverted={}",
                    session.fbink_version,
                    session.device_name,
                    session.codename,
                    session.platform,
                    session.device_id,
                    session.view_width,
                    session.view_height,
                    session.view_hori_origin,
                    session.view_vert_origin,
                    session.screen_width,
                    session.screen_height,
                    session.stride,
                    session.bpp,
                    session.pixel_format,
                    session.has_color_panel,
                    session.has_eclipse_wfm,
                    session.dpi,
                    session.is_mtk,
                    session.inverted,
                );
                print_refusal(fd, &msg);
                fb::fbink_close(fd);
                Err(Unsupported(msg).into())
            };
            let Some(format) = Format::from_fbink(st.bpp, &session.pixel_format) else {
                return fail(format!(
                    "only 8-bit and 32-bit screens are supported, this one is {} bpp ({})",
                    st.bpp, session.pixel_format
                ));
            };
            let (width, height) = (st.view_width, st.view_height);
            let layout = Layout {
                format,
                stride: st.scanline_stride as usize,
                origin,
                width,
                height,
                inverted: st.inverted_grayscale,
            };
            if !layout.fits_screen(st.screen_width, st.screen_height) {
                return fail(format!(
                    "the screen's viewport ({width}x{height}+{}+{}) exceeds the {}x{} panel",
                    layout.origin.0, layout.origin.1, st.screen_width, st.screen_height
                ));
            }
            let Some(needed) = layout.required_len() else {
                return fail(format!(
                    "the screen's viewport ({width}x{height}+{}+{}) doesn't fit its {}-byte rows",
                    layout.origin.0, layout.origin.1, layout.stride
                ));
            };
            let mut fb_len = 0usize;
            let ptr = fb::fbink_get_fb_pointer(fd, &mut fb_len);
            if ptr.is_null() || fb_len < needed {
                return fail(format!("framebuffer mapping is missing or too small ({fb_len} bytes)"));
            }

            let dark_variant = DarkVariant::from_env();
            if dark_variant != DarkVariant::A {
                log::info!("dark mode waveform variant: {dark_variant:?}");
            }
            Ok(Self {
                fd,
                cfg,
                fb: ptr,
                fb_len,
                writer: Writer::new(layout),
                width,
                height,
                orientation: Orientation::Portrait,
                dpi: st.screen_dpi as u32,
                name,
                session,
                touch_swap_axes: st.touch_swap_axes,
                touch_mirror_x: st.touch_mirror_x,
                touch_mirror_y: st.touch_mirror_y,
                flash_fence: FlashFence::default(),
                dark_variant,
                failed_refreshes: 0,
            })
        }
    }

    /// Tells the user on screen that Verso can't run here, and why.
    pub fn show_refusal(&self, reason: &str) {
        print_refusal(self.fd, reason);
    }

    pub fn present(&mut self, canvas: &Canvas, updates: &[Update]) -> Result<()> {
        let (cw, ch) = self.orientation.screen_size(self.width, self.height);
        if (canvas.width(), canvas.height()) != (cw, ch) {
            bail!("canvas is {}x{}, expected {cw}x{ch}", canvas.width(), canvas.height());
        }
        let canvas_bounds = canvas.bounds();
        for u in updates {
            if u.rect.intersect(&canvas_bounds).is_none() {
                continue;
            }
            // Let any in-flight flash finish before writing new pixels over it.
            self.wait_for_pending_flash();
            // SAFETY: the mapping is valid and at least `fb_len` bytes for as long
            // as the FBInk session is open (checked in `open`), and only we use it.
            let pixels = unsafe { std::slice::from_raw_parts_mut(self.fb, self.fb_len) };
            // The refresh region is in framebuffer coordinates: FBInk's
            // refresh doesn't add the viewport origin itself.
            let Some(r) = self.writer.write(canvas, u.rect, self.orientation, pixels) else { continue };
            if self.session.has_color_panel {
                self.cfg.cfa_mode = fb::CFA_MODE_INDEX_E_CFA_DEFAULT;
            }
            let caps = PanelCaps {
                has_color_panel: self.session.has_color_panel,
                has_eclipse_wfm: self.session.has_eclipse_wfm,
                is_mtk: self.session.is_mtk,
            };
            let plan = plan_refresh(u.mode, canvas.is_dark(), caps, self.dark_variant);
            self.cfg.wfm_mode = plan.waveform.to_fbink();
            self.cfg.is_flashing = plan.is_flashing;
            // SAFETY: FFI call with a valid fd and config.
            let rc = unsafe { fb::fbink_refresh(self.fd, r.y as u32, r.x as u32, r.w as u32, r.h as u32, &self.cfg) };
            if rc < 0 {
                self.failed_refreshes += 1;
                log::warn!("refresh of {r:?} ({:?}) failed: {rc}", u.mode);
            } else if plan.mark_flash_fence {
                // SAFETY: reads FBInk's marker for the refresh just sent.
                let marker = unsafe { fb::fbink_get_last_marker() };
                self.flash_fence.mark_flash(marker);
            }
        }
        Ok(())
    }

    /// Whether the framebuffer still holds `canvas`, as last presented.
    pub fn check_frame(&mut self, canvas: &Canvas) -> FrameCheck {
        // SAFETY: as in `present`. Others can write the memory meanwhile, which
        // is what this looks for; a torn read only changes the answer.
        let pixels = unsafe { std::slice::from_raw_parts(self.fb, self.fb_len) };
        self.writer.check(canvas, self.orientation, pixels)
    }

    fn wait_for_marker(&self, marker: u32) {
        // SAFETY: FFI call with a valid fd; marker 0 means "the last refresh we sent".
        let rc = unsafe { fb::fbink_wait_for_complete(self.fd, marker) };
        if rc < 0 {
            log::warn!("waiting for refresh {marker} to complete failed: {rc}");
        }
    }

    fn wait_for_pending_flash(&mut self) {
        if let Some(marker) = self.flash_fence.take_before_write() {
            self.wait_for_marker(marker);
        }
    }

    pub fn wait_for_complete(&mut self) {
        // SAFETY: reads FBInk's last sent refresh marker.
        let last = unsafe { fb::fbink_get_last_marker() };
        let (pending, tail) = self.flash_fence.take_for_barrier(last);
        if let Some(marker) = pending {
            self.wait_for_marker(marker);
        }
        if let Some(marker) = tail {
            self.wait_for_marker(marker);
        }
    }
}

/// Clears the screen and prints, centred, that Verso can't run on this
/// Kindle and why. The launcher saves a problem report and keeps the message
/// up for a few seconds before the Kindle's home screen comes back.
fn print_refusal(fd: i32, reason: &str) {
    let lines = [
        "Verso can't run on this Kindle yet:",
        reason,
        "A problem report is saved in verso/reports.",
        "The Kindle's home screen comes back in a moment.",
    ];
    // SAFETY: FFI calls with an open FBInk fd and a valid, zero-initialised
    // config; the strings outlive the calls.
    unsafe {
        let mut cfg: fb::FBInkConfig = std::mem::zeroed();
        cfg.is_quiet = true;
        cfg.is_flashing = true;
        let rc = fb::fbink_cls(fd, &cfg, std::ptr::null(), false);
        if rc < 0 {
            log::warn!("refusal screen fbink_cls failed: {rc}");
        }
        cfg.is_flashing = false;
        cfg.is_centered = true;
        cfg.is_halfway = true;
        let mut row: i16 = -4;
        for line in lines {
            let text = CString::new(line.replace('\0', " ")).unwrap_or_default();
            cfg.row = row;
            let rows = fb::fbink_print(fd, text.as_ptr(), &cfg);
            if rows < 0 {
                log::warn!("refusal screen fbink_print failed: {rows}");
            }
            row = row.saturating_add(rows.clamp(1, 8) as i16 + 1);
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // Don't hand the display back to the launcher mid-flash on an error exit.
        self.wait_for_pending_flash();
        // SAFETY: closes the session opened in `open`; the mapping isn't used after this.
        unsafe { fb::fbink_close(self.fd) };
    }
}
