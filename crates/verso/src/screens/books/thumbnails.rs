//! Decoded library covers: a byte-bounded cache that keeps the pages next to
//! the current one, a background decoder that prepares them, and the visible
//! strips of Stack covers behind the front one (docs/library.md#cover-drawing).
use crate::geom::Rect;
use crate::gfx::{Canvas, ColorMode};
use crate::library::{
    Book, Cover,
    covers::{self, CoverImage},
};
use crate::store::Store;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// How a decoded cover is fitted to its slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Fit {
    /// The whole cover, filling the slot: the tile thumbnail of that size once
    /// the library worker has made it, until then a larger one scaled down.
    Cover,
    /// A Stack's cover behind the one in front, shifted up and right by this
    /// many pixels: only the strips along its top and right show, sampled
    /// from the small thumbnail.
    Behind(i32),
}

/// Cover key, slot size and fit.
pub(super) type ThumbnailKey = (String, i32, i32, Fit);

/// A thumbnail to prepare in the background for a neighbouring page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CoverSpec {
    pub book_id: String,
    pub key: String,
    pub w: i32,
    pub h: i32,
    pub fit: Fit,
}

/// Decoded cover pixels (grayscale or RGB) ready to blit onto the canvas.
pub(super) enum CachedCover {
    Full(CoverImage),
    /// Only the visible top (`w × d`) and right (`d × (h - d)`) strips of a
    /// behind cover in Stack mode.
    Behind {
        top: Vec<u8>,
        right: Vec<u8>,
        w: u32,
        h: u32,
        d: u32,
        rgb: bool,
    },
}

impl CachedCover {
    pub(super) fn bytes(&self) -> usize {
        match self {
            Self::Full(image) => image.bytes(),
            Self::Behind { top, right, .. } => top.len() + right.len(),
        }
    }

    pub(super) fn blit(&self, canvas: &mut Canvas, rect: Rect) {
        match self {
            Self::Full(image) => image.blit(canvas, rect),
            Self::Behind { top, right, w, h, d, rgb } => {
                // Cached per slot size, so the strips match `rect`.
                let (w, h, d) = (*w as i32, *h as i32, *d as i32);
                let top_rect = Rect::new(rect.x, rect.y, w, d);
                let right_rect = Rect::new(rect.x + w - d, rect.y + d, d, h - d);
                if *rgb {
                    canvas.blit_rgb(top_rect, top, w as usize);
                    canvas.blit_rgb(right_rect, right, d as usize);
                } else {
                    canvas.blit_gray(top_rect, top, w as usize);
                    canvas.blit_gray(right_rect, right, d as usize);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn sample_pixel(&self, x: u32, y: u32) -> u8 {
        match self {
            Self::Full(CoverImage::Gray(image)) => image.get_pixel(x, y)[0],
            Self::Full(CoverImage::Rgb(image)) => {
                let p = image.get_pixel(x, y).0;
                crate::gfx::luma(p[0], p[1], p[2])
            }
            Self::Behind { top, right, w, d, rgb, .. } => {
                let bpp = if *rgb { 3 } else { 1 };
                let slice = if y < *d {
                    let i = (y * *w + x) as usize * bpp;
                    &top[i..i + bpp]
                } else {
                    let rx = x.saturating_sub(*w - *d);
                    let i = ((y - *d) * *d + rx) as usize * bpp;
                    &right[i..i + bpp]
                };
                if *rgb { crate::gfx::luma(slice[0], slice[1], slice[2]) } else { slice[0] }
            }
        }
    }
}

pub(super) struct Entry {
    pub cover: CachedCover,
    used: u64,
}

#[derive(Default)]
struct PrefetchShared {
    stop: bool,
    generation: u64,
    queue: VecDeque<CoverSpec>,
    ready: Vec<(ThumbnailKey, CachedCover)>,
    in_flight: Option<ThumbnailKey>,
}

/// Low-overhead background decoder for neighbouring library pages.
/// Never wakes the UI thread: decoded thumbnails sit in `ready` and are
/// absorbed by `Thumbnails` on the next page turn or draw.
struct Prefetcher {
    store: Store,
    shared: Arc<(Mutex<PrefetchShared>, Condvar)>,
    join: Option<JoinHandle<()>>,
}

impl Prefetcher {
    fn new(store: Store) -> Self {
        Self { store, shared: Arc::new((Mutex::new(PrefetchShared::default()), Condvar::new())), join: None }
    }

    fn ensure_started(&mut self) {
        if self.join.is_some() {
            return;
        }
        let store = self.store.clone();
        let shared = self.shared.clone();
        self.join = Some(
            std::thread::Builder::new()
                .name("verso-thumb".into())
                .spawn(move || {
                    loop {
                        let (generation, spec, key) = {
                            let mut s = shared.0.lock().unwrap();
                            while !s.stop && s.queue.is_empty() {
                                s = shared.1.wait(s).unwrap();
                            }
                            if s.stop {
                                break;
                            }
                            let spec = s.queue.pop_front().unwrap();
                            let key = (spec.key.clone(), spec.w, spec.h, spec.fit);
                            s.in_flight = Some(key.clone());
                            (s.generation, spec, key)
                        };
                        let decoded = decode_cover(&store, &spec.key, spec.w, spec.h, spec.fit);
                        let mut s = shared.0.lock().unwrap();
                        s.in_flight = None;
                        if let Ok(cover) = decoded
                            && !s.stop
                            && s.generation == generation
                        {
                            s.ready.push((key, cover));
                        }
                        shared.1.notify_all();
                    }
                })
                .expect("start thumbnail prefetcher"),
        );
    }

    fn schedule(&mut self, specs: Vec<CoverSpec>) {
        if specs.is_empty() {
            let mut s = self.shared.0.lock().unwrap();
            s.generation = s.generation.wrapping_add(1);
            s.queue.clear();
            return;
        }
        self.ensure_started();
        let mut s = self.shared.0.lock().unwrap();
        s.generation = s.generation.wrapping_add(1);
        s.queue = specs.into();
        self.shared.1.notify_all();
    }

    fn drain(&self) -> Vec<(ThumbnailKey, CachedCover)> {
        std::mem::take(&mut self.shared.0.lock().unwrap().ready)
    }

    /// Waits briefly if the worker is already decoding `key` (avoiding a
    /// duplicate decode on the UI thread), and drops `key` from the pending
    /// queue so the worker moves straight to the next cover.
    fn wait_or_claim(&self, key: &ThumbnailKey, max_wait: std::time::Duration) {
        let mut s = self.shared.0.lock().unwrap();
        let deadline = std::time::Instant::now() + max_wait;
        while !s.stop && s.in_flight.as_ref() == Some(key) {
            let rem = deadline.saturating_duration_since(std::time::Instant::now());
            if rem.is_zero() {
                break;
            }
            let (guard, timeout) = self.shared.1.wait_timeout(s, rem).unwrap();
            s = guard;
            if timeout.timed_out() {
                break;
            }
        }
        s.queue.retain(|spec| !(spec.key == key.0 && spec.w == key.1 && spec.h == key.2 && spec.fit == key.3));
    }

    fn cancel(&self) {
        let mut s = self.shared.0.lock().unwrap();
        s.generation = s.generation.wrapping_add(1);
        s.queue.clear();
        s.ready.clear();
    }

    #[cfg(test)]
    fn wait_idle(&self) {
        let mut s = self.shared.0.lock().unwrap();
        while !s.stop && (!s.queue.is_empty() || s.in_flight.is_some()) {
            s = self.shared.1.wait(s).unwrap();
        }
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        {
            let mut s = self.shared.0.lock().unwrap();
            s.stop = true;
            s.queue.clear();
            s.ready.clear();
        }
        self.shared.1.notify_all();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Decoded covers, bounded by their pixel bytes with the least recently drawn
/// dropped first, so the pages either side of the current one and the page a
/// folder was opened from stay decoded. A cache file that fails to load is
/// not retried on every redraw: the fallback is shown and the library worker
/// is asked once to repair it. A repair reply allows one more load.
#[derive(Default)]
pub(super) struct Thumbnails {
    pub(super) images: HashMap<ThumbnailKey, Entry>,
    bytes: usize,
    max_bytes: usize,
    clock: u64,
    /// (book id, cover key) pairs that failed to load.
    pub(super) failed: HashSet<(String, String)>,
    /// Book ids to send to the library worker after drawing.
    pub(super) requests: Vec<String>,
    /// Cover sizes drawn this session, and those not yet sent to the worker.
    sizes: HashSet<(u32, u32)>,
    /// Home's covers aren't paged through, so their sizes get no tile
    /// thumbnails: each would cost every book a file for a few covers.
    pub(super) home: bool,
    new_sizes: Vec<(u32, u32)>,
    prefetcher: Option<Prefetcher>,
    /// What the prefetcher was last asked for.
    pub(super) prefetching: HashSet<ThumbnailKey>,
}

impl Thumbnails {
    /// About five pages of 12 covers (a full portrait 4×3 page is about 0.82 MB):
    /// the current page, both neighbours and the page a folder was opened
    /// from, small next to the 60 MiB app target. Opening a book frees them.
    pub(super) const MAX_BYTES: usize = 6 * 1024 * 1024;
    /// Same ~5-page coverage for 24-bit RGB covers on color devices (1 GiB RAM).
    pub(super) const MAX_BYTES_COLOR: usize = 18 * 1024 * 1024;

    /// Unit tests decode on the calling thread only, so what is cached after
    /// a draw doesn't depend on timing; [`Self::prefetching`] opts in.
    pub(super) fn new(store: Store) -> Self {
        let max_bytes = Self::budget(store.color_mode());
        if cfg!(test) { Self { max_bytes, ..Self::default() } } else { Self::prefetching(store) }
    }

    pub(super) fn prefetching(store: Store) -> Self {
        let max_bytes = Self::budget(store.color_mode());
        Self { max_bytes, prefetcher: Some(Prefetcher::new(store)), ..Self::default() }
    }

    fn budget(color: ColorMode) -> usize {
        match color {
            ColorMode::Gray8 => Self::MAX_BYTES,
            ColorMode::Color32 => Self::MAX_BYTES_COLOR,
        }
    }

    fn limit(&self) -> usize {
        if self.max_bytes == 0 { Self::MAX_BYTES } else { self.max_bytes }
    }

    fn insert(&mut self, key: ThumbnailKey, cover: CachedCover) {
        let added = cover.bytes();
        let limit = self.limit();
        if let Some(old) = self.images.remove(&key) {
            self.bytes -= old.cover.bytes();
        }
        while self.bytes + added > limit {
            let Some(oldest) = self.images.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else {
                break;
            };
            let removed = self.images.remove(&oldest).unwrap();
            self.bytes -= removed.cover.bytes();
        }
        self.clock += 1;
        self.bytes += added;
        self.images.insert(key, Entry { cover, used: self.clock });
    }

    /// Takes what the prefetcher has decoded since the last call.
    fn absorb_prefetched(&mut self) {
        let ready = self.prefetcher.as_ref().map_or_else(Vec::new, Prefetcher::drain);
        for (key, cover) in ready {
            if !self.images.contains_key(&key) {
                self.insert(key, cover);
            }
        }
    }

    pub(super) fn clear(&mut self) {
        if let Some(p) = &self.prefetcher {
            p.cancel();
        }
        self.images.clear();
        self.bytes = 0;
        self.prefetching.clear();
    }

    pub(super) fn repaired(&mut self, ids: &[String]) {
        self.failed.retain(|(id, _)| !ids.contains(id));
    }

    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Cover sizes drawn for the first time this session.
    pub(super) fn take_new_sizes(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.new_sizes)
    }

    /// Decodes `specs` in the background, those not already cached or failed.
    /// A list the prefetcher already has is left alone, so a redraw doesn't
    /// restart its work.
    pub(super) fn prefetch(&mut self, specs: Vec<CoverSpec>) {
        if self.prefetcher.is_none() {
            return;
        }
        self.absorb_prefetched();
        let mut seen = HashSet::new();
        let wanted: Vec<_> = specs
            .into_iter()
            .filter(|spec| {
                let key = (spec.key.clone(), spec.w, spec.h, spec.fit);
                !self.images.contains_key(&key)
                    && !self.failed.contains(&(spec.book_id.clone(), spec.key.clone()))
                    && seen.insert(key)
            })
            .collect();
        if seen.is_subset(&self.prefetching) {
            return;
        }
        self.prefetching = seen;
        if let Some(p) = &mut self.prefetcher {
            p.schedule(wanted);
        }
    }

    /// The decoded cover of `book` sized `w` × `h`, loaded on first use. None
    /// without art, while pending, or when the cache file failed to load.
    pub(super) fn load(&mut self, store: &Store, book: &Book, w: i32, h: i32, fit: Fit) -> Option<&CachedCover> {
        let Cover::Ready(key) = &book.cover else { return None };
        let size = (w.max(1) as u32, h.max(1) as u32);
        if fit == Fit::Cover && !self.home && covers::tile_size(size) && self.sizes.insert(size) {
            self.new_sizes.push(size);
        }
        let cache_key = (key.clone(), w, h, fit);
        if !self.images.contains_key(&cache_key) {
            self.absorb_prefetched();
        }
        if !self.images.contains_key(&cache_key)
            && let Some(p) = &self.prefetcher
        {
            p.wait_or_claim(&cache_key, std::time::Duration::from_millis(50));
            self.absorb_prefetched();
        }
        if !self.images.contains_key(&cache_key) {
            let failure = (book.id.clone(), key.clone());
            if self.failed.contains(&failure) {
                return None;
            }
            match decode_cover(store, key, w, h, fit) {
                Ok(cover) => self.insert(cache_key.clone(), cover),
                Err(e) => {
                    log::warn!("cover cache for {} failed to load: {e:#}", book.id);
                    self.failed.insert(failure);
                    self.requests.push(book.id.clone());
                    return None;
                }
            }
        }
        self.clock += 1;
        let entry = self.images.get_mut(&cache_key)?;
        entry.used = self.clock;
        Some(&entry.cover)
    }

    #[cfg(test)]
    pub(super) fn wait_prefetch(&mut self) {
        if let Some(p) = &self.prefetcher {
            p.wait_idle();
        }
        self.absorb_prefetched();
    }
}

/// Decodes one cover from the on-disk cache and fits it to its slot.
pub(super) fn decode_cover(store: &Store, key: &str, w: i32, h: i32, fit: Fit) -> anyhow::Result<CachedCover> {
    let (w, h) = (w.max(1) as u32, h.max(1) as u32);
    if let Fit::Behind(d) = fit {
        return Ok(sample_behind_strip(&covers::load_small(store, key)?, w, h, d.max(1) as u32));
    }
    if covers::tile_size((w, h))
        && let Some(tile) = covers::load_tile(store, key, w, h)?
    {
        return Ok(CachedCover::Full(tile));
    }
    // Slots no larger than the small thumbnail decode it instead; a broken
    // small file is repaired like a full one.
    let image = if w <= covers::SMALL_WIDTH && h <= covers::SMALL_HEIGHT {
        covers::load_small(store, key)?
    } else {
        covers::load(&store.cover_path(key), store.color_mode())?
    };
    Ok(CachedCover::Full(fill(&image, w, h)))
}

/// Scales a cover with area averaging until it covers a `w` × `h` slot and
/// crops what's left over: centred across, kept from the top down, where
/// titles usually sit. Cover-shaped slots lose at most a rounding pixel.
pub(super) fn fill(image: &CoverImage, w: u32, h: u32) -> CoverImage {
    let (sw, sh) = covers::fill_size(image.width(), image.height(), w, h);
    if (sw, sh) == image.dimensions() && (sw, sh) == (w, h) {
        return image.clone();
    }
    match image {
        CoverImage::Gray(img) => {
            let scaled = if sw > img.width() || sh > img.height() {
                image::imageops::resize(img, sw, sh, image::imageops::FilterType::Triangle)
            } else {
                image::imageops::thumbnail(img, sw, sh)
            };
            let out = if (sw, sh) == (w, h) {
                scaled
            } else {
                image::imageops::crop_imm(&scaled, (sw - w) / 2, 0, w, h).to_image()
            };
            CoverImage::Gray(out)
        }
        CoverImage::Rgb(img) => {
            let scaled = if sw > img.width() || sh > img.height() {
                image::imageops::resize(img, sw, sh, image::imageops::FilterType::Triangle)
            } else {
                image::imageops::thumbnail(img, sw, sh)
            };
            let out = if (sw, sh) == (w, h) {
                scaled
            } else {
                image::imageops::crop_imm(&scaled, (sw - w) / 2, 0, w, h).to_image()
            };
            CoverImage::Rgb(out)
        }
    }
}

/// Bilinearly samples only the visible top (`w × d`) and right (`d × (h - d)`)
/// strips of a Stack behind cover directly from its small thumbnail, skipping
/// the ~82% hidden behind the cover in front.
pub(super) fn sample_behind_strip(src: &CoverImage, w: u32, h: u32, d: u32) -> CachedCover {
    let d = d.clamp(1, w.min(h));
    let (iw, ih) = (src.width().max(1) as u64, src.height().max(1) as u64);
    let (w64, h64) = (w as u64, h as u64);
    let (sw, sh) = if w64 * ih >= h64 * iw { (w64, (ih * w64).div_ceil(iw)) } else { ((iw * h64).div_ceil(ih), h64) };
    let crop_x = (sw - w64) / 2;

    let map_fp = |dst: u64, dst_len: u64, src_len: u64| -> (usize, usize, u32) {
        if src_len <= 1 || dst_len == 0 {
            return (0, 0, 0);
        }
        let num = (2 * dst + 1) * src_len * 128;
        let pos = (num / dst_len).saturating_sub(128).min((src_len - 1) * 256);
        let i0 = (pos >> 8) as usize;
        let i1 = (i0 + 1).min(src_len as usize - 1);
        (i0, i1, (pos & 255) as u32)
    };

    let x_lut: Vec<(usize, usize, u32)> = (0..w64).map(|dx| map_fp(dx + crop_x, sw, iw)).collect();
    let stride = src.width() as usize;
    let (raw, rgb, bpp): (&[u8], bool, usize) = match src {
        CoverImage::Gray(img) => (img.as_raw(), false, 1),
        CoverImage::Rgb(img) => (img.as_raw(), true, 3),
    };

    let sample_row = |dst: &mut Vec<u8>, dy: u64, x_range: std::ops::Range<usize>| {
        let (y0, y1, fy) = map_fp(dy, sh, ih);
        let row0 = y0 * stride * bpp;
        let row1 = y1 * stride * bpp;
        let inv_fy = 256 - fy;
        for &(x0, x1, fx) in &x_lut[x_range] {
            let inv_fx = 256 - fx;
            for c in 0..bpp {
                let top_v = raw[row0 + x0 * bpp + c] as u32 * inv_fx + raw[row0 + x1 * bpp + c] as u32 * fx;
                let bot_v = raw[row1 + x0 * bpp + c] as u32 * inv_fx + raw[row1 + x1 * bpp + c] as u32 * fx;
                dst.push(((top_v * inv_fy + bot_v * fy + 32768) >> 16) as u8);
            }
        }
    };

    let mut top = Vec::with_capacity((w * d) as usize * bpp);
    for dy in 0..d as u64 {
        sample_row(&mut top, dy, 0..w as usize);
    }

    let rem_h = h - d;
    let mut right = Vec::with_capacity((d * rem_h) as usize * bpp);
    let rx0 = (w - d) as usize;
    for dy in d as u64..h as u64 {
        sample_row(&mut right, dy, rx0..w as usize);
    }

    CachedCover::Behind { top, right, w, h, d, rgb }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_fill_crops_the_overflow_keeping_the_top_and_centre() {
        // A 2:3 cover: the top third white, the rest black, with a gray
        // middle column.
        let cover = CoverImage::Gray(image::GrayImage::from_fn(144, 216, |x, y| {
            image::Luma([if (66..78).contains(&x) {
                0x80
            } else if y < 72 {
                0xff
            } else {
                0x00
            }])
        }));
        // A wide slot crops the bottom and keeps the top whole.
        let CoverImage::Gray(wide) = fill(&cover, 120, 150) else { panic!("expected gray") };
        assert_eq!(wide.dimensions(), (120, 150));
        assert_eq!(wide.get_pixel(0, 0).0[0], 0xff);
        assert_eq!(wide.get_pixel(0, 59).0[0], 0xff, "a third of the scaled height, 180, is white");
        assert_eq!(wide.get_pixel(0, 149).0[0], 0x00);
        // A tall slot crops both sides evenly.
        let CoverImage::Gray(tall) = fill(&cover, 60, 216) else { panic!("expected gray") };
        assert_eq!(tall.dimensions(), (60, 216));
        assert_eq!(tall.get_pixel(30, 100).0[0], 0x80, "the middle column stays in the middle");
        assert_eq!(tall.get_pixel(0, 100).0[0], 0x00);
        // A cover-shaped slot is scaled, not cropped.
        let CoverImage::Gray(ref src_gray) = cover else { unreachable!() };
        assert_eq!(fill(&cover, 96, 144), CoverImage::Gray(image::imageops::thumbnail(src_gray, 96, 144)));
    }

    #[test]
    fn behind_covers_sample_only_their_visible_strips() {
        // A horizontal gradient small thumbnail, scaled up to a Stack slot.
        let small = CoverImage::Gray(image::GrayImage::from_fn(covers::SMALL_WIDTH, covers::SMALL_HEIGHT, |x, y| {
            image::Luma([(x + y / 4) as u8])
        }));
        let (w, h, d) = (209, 313, 24);
        let strip = sample_behind_strip(&small, w, h, d);
        assert_eq!(strip.bytes() as u32, w * d + d * (h - d), "about a fifth of the cover");
        // Close to the whole cover scaled the old way, where it shows.
        let CoverImage::Gray(whole) = fill(&small, w, h) else { panic!("expected gray") };
        for (x, y) in [(0, 0), (w / 2, d - 1), (w - 1, 0), (w - d, d), (w - 1, h - 1), (w - d / 2, h / 2)] {
            let (a, b) = (strip.sample_pixel(x, y), whole.get_pixel(x, y)[0]);
            assert!(a.abs_diff(b) <= 2, "({x}, {y}): {a} vs {b}");
        }
        // Blitting it leaves the part the cover in front hides untouched.
        let mut canvas = Canvas::new(300, 400);
        canvas.fill_rect(canvas.bounds(), 0xee);
        let rect = Rect::new(10, 20, w as i32, h as i32);
        strip.blit(&mut canvas, rect);
        let at = |canvas: &Canvas, x: i32, y: i32| canvas.gray_data()[(y * 300 + x) as usize];
        assert_eq!(at(&canvas, 10, 20), strip.sample_pixel(0, 0));
        assert_eq!(at(&canvas, rect.right() - 1, rect.bottom() - 1), strip.sample_pixel(w - 1, h - 1));
        assert_eq!(at(&canvas, 10, 20 + d as i32), 0xee, "below the top strip, left of the right one");
        assert_eq!(at(&canvas, rect.right() - d as i32 - 1, rect.bottom() - 1), 0xee);

        // Color behind strip blits true RGB channels onto a Color32 canvas.
        let color_small = CoverImage::Rgb(image::RgbImage::from_pixel(
            covers::SMALL_WIDTH,
            covers::SMALL_HEIGHT,
            image::Rgb([220, 40, 30]),
        ));
        let color_strip = sample_behind_strip(&color_small, w, h, d);
        assert_eq!(color_strip.bytes() as u32, (w * d + d * (h - d)) * 3);
        let mut color_canvas = Canvas::with_mode(300, 400, ColorMode::Color32);
        color_canvas.fill(0xee);
        color_strip.blit(&mut color_canvas, rect);
        let raw = color_canvas.raw();
        let top_px = &raw[((20 * 300 + 10) * 4) as usize..((20 * 300 + 10) * 4 + 4) as usize];
        assert_eq!(top_px, &[220, 40, 30, 255]);
        let hidden_px =
            &raw[(((20 + d as i32) * 300 + 10) * 4) as usize..(((20 + d as i32) * 300 + 10) * 4 + 4) as usize];
        assert_eq!(hidden_px, &[0xee, 0xee, 0xee, 255]);
    }

    fn fixture(label: &str) -> (Store, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("verso-thumbnails-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone());
        std::fs::create_dir_all(store.cover_path("x").parent().unwrap()).unwrap();
        (store, root)
    }
    fn book(id: &str) -> Book {
        Book {
            path: format!("/books/{id}.epub").into(),
            id: id.into(),
            annotation_id: String::new(),
            legacy_id: String::new(),
            fingerprint: String::new(),
            fingerprint_attempted: false,
            stamp: Default::default(),
            metadata: Default::default(),
            error: None,
            cover: Cover::Ready(id.into()),
            available: true,
            metadata_pending: false,
        }
    }

    #[test]
    fn the_least_recently_drawn_covers_go_first() {
        let (store, root) = fixture("budget");
        let (w, h) = (243, 365);
        let per_cover = (w * h) as usize;
        let fits = Thumbnails::MAX_BYTES / per_cover;
        for i in 0..=fits {
            let id = i.to_string();
            image::GrayImage::from_pixel(w as u32, h as u32, image::Luma([i as u8]))
                .save(store.tile_cover_path(&id, w as u32, h as u32))
                .unwrap();
        }
        let mut cache = Thumbnails::default();
        for i in 0..fits {
            assert!(cache.load(&store, &book(&i.to_string()), w, h, Fit::Cover).is_some());
        }
        // The first is drawn again, so the second is the one dropped.
        cache.load(&store, &book("0"), w, h, Fit::Cover);
        cache.load(&store, &book(&fits.to_string()), w, h, Fit::Cover);
        assert_eq!(cache.images.len(), fits);
        assert!(cache.bytes() <= Thumbnails::MAX_BYTES);
        let has = |cache: &Thumbnails, id: &str| cache.images.contains_key(&(id.to_owned(), w, h, Fit::Cover));
        assert!(has(&cache, "0") && !has(&cache, "1") && has(&cache, &fits.to_string()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn neighbour_pages_are_decoded_in_the_background() {
        let (store, root) = fixture("prefetch");
        let (w, h) = (209, 313);
        for id in ["a", "b"] {
            image::GrayImage::from_pixel(covers::WIDTH, covers::HEIGHT, image::Luma([128]))
                .save(store.cover_path(id))
                .unwrap();
            image::GrayImage::from_pixel(covers::SMALL_WIDTH, covers::SMALL_HEIGHT, image::Luma([50]))
                .save(store.small_cover_path(id))
                .unwrap();
        }
        let spec = |id: &str, fit| CoverSpec { book_id: id.into(), key: id.into(), w, h, fit };
        let mut cache = Thumbnails::prefetching(store.clone());
        // A missing file is skipped quietly; drawing it later reports it.
        cache.prefetch(vec![spec("a", Fit::Cover), spec("b", Fit::Behind(24)), spec("missing", Fit::Cover)]);
        cache.wait_prefetch();
        assert_eq!(cache.images.len(), 2);
        assert!(cache.failed.is_empty() && cache.requests.is_empty());
        // Drawing them reads nothing.
        std::fs::remove_dir_all(store.cover_path("a").parent().unwrap()).unwrap();
        assert_eq!(cache.load(&store, &book("a"), w, h, Fit::Cover).map(|c| c.sample_pixel(5, 5)), Some(128));
        assert_eq!(cache.load(&store, &book("b"), w, h, Fit::Behind(24)).map(|c| c.sample_pixel(5, 5)), Some(50));
        assert!(cache.failed.is_empty());
        assert!(cache.load(&store, &book("missing"), w, h, Fit::Cover).is_none());
        assert_eq!(cache.requests, ["missing"]);
        // Cleared covers aren't brought back by work already queued.
        cache.clear();
        cache.wait_prefetch();
        assert!(cache.images.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
