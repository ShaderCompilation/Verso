//! Fixed-size cover cache (8-bit grayscale in `Gray8`, 24-bit RGB in `Color32`).
//! All source decoding happens on the library thread.
use super::{Book, Cover, LibraryIndex, epub};
use crate::geom::Rect;
use crate::gfx::{Canvas, ColorMode, luma};
use crate::store::{Store, atomic_bytes};
use anyhow::{Context, Result, ensure};
use image::{GrayImage, ImageReader, RgbImage, RgbaImage};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::Path;

pub const WIDTH: u32 = 288;
pub const HEIGHT: u32 = 432;
/// The small thumbnail for folder preview slots: half of each side.
pub const SMALL_WIDTH: u32 = WIDTH / 2;
pub const SMALL_HEIGHT: u32 = HEIGHT / 2;
const VERSION: u8 = 1;
const CACHE_LIMIT: u64 = 1024 * 1024;
/// Tile thumbnails kept per book: the browsing grid cover sizes the library
/// drew most recently. Portrait uses three (All books, books in a folder and
/// the folder preview's front cover), landscape three others; each costs
/// 32–46 KB a book on the Kindle.
pub const TILE_SIZES: usize = 6;

/// A decoded cover thumbnail or sleep cover: 8-bit grayscale on monochrome
/// devices, 24-bit RGB on color devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoverImage {
    Gray(GrayImage),
    Rgb(RgbImage),
}

impl CoverImage {
    pub fn width(&self) -> u32 {
        match self {
            Self::Gray(i) => i.width(),
            Self::Rgb(i) => i.width(),
        }
    }

    pub fn height(&self) -> u32 {
        match self {
            Self::Gray(i) => i.height(),
            Self::Rgb(i) => i.height(),
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    pub fn bytes(&self) -> usize {
        match self {
            Self::Gray(i) => i.as_raw().len(),
            Self::Rgb(i) => i.as_raw().len(),
        }
    }

    pub fn rotate90(&self) -> Self {
        match self {
            Self::Gray(i) => Self::Gray(image::imageops::rotate90(i)),
            Self::Rgb(i) => Self::Rgb(image::imageops::rotate90(i)),
        }
    }

    pub fn blit(&self, canvas: &mut Canvas, rect: Rect) {
        match self {
            Self::Gray(i) => canvas.blit_gray(rect, i.as_raw(), i.width() as usize),
            Self::Rgb(i) => canvas.blit_rgb(rect, i.as_raw(), i.width() as usize),
        }
    }

    fn write_to_png(&self, out: &mut Cursor<Vec<u8>>) -> Result<()> {
        match self {
            Self::Gray(i) => i.write_to(out, image::ImageFormat::Png)?,
            Self::Rgb(i) => i.write_to(out, image::ImageFormat::Png)?,
        }
        Ok(())
    }
}

/// Cache key for `book` in `color` mode. `Gray8` keeps the original hash so
/// existing monochrome caches stay valid; `Color32` hashes a distinct key so
/// switching simulator presets never mixes gray and color thumbnails.
pub fn key(book: &Book, color: ColorMode) -> String {
    let value = match color {
        ColorMode::Gray8 => serde_json::to_vec(&(VERSION, &book.id, &book.stamp)),
        ColorMode::Color32 => serde_json::to_vec(&(VERSION, "color", &book.id, &book.stamp)),
    }
    .expect("serializable cover key");
    format!("{:x}", Sha256::digest(value))
}
fn limits(width: u32, height: u32, alloc: u64) -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(alloc);
    limits
}
fn read_cache(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(CACHE_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= CACHE_LIMIT, "cover cache too large");
    Ok(bytes)
}
fn decode(bytes: Vec<u8>, width: u32, height: u32, color: ColorMode) -> Result<CoverImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let alloc = match color {
        ColorMode::Gray8 => 2 * 1024 * 1024,
        ColorMode::Color32 => 4 * 1024 * 1024,
    };
    reader.limits(limits(width, height, alloc));
    let image = reader.decode()?;
    ensure!(image.width() == width && image.height() == height, "invalid thumbnail size");
    Ok(match color {
        ColorMode::Gray8 => CoverImage::Gray(image.into_luma8()),
        ColorMode::Color32 => CoverImage::Rgb(image.into_rgb8()),
    })
}
pub fn load(path: &Path, color: ColorMode) -> Result<CoverImage> {
    decode(read_cache(path)?, WIDTH, HEIGHT, color)
}
fn load_small_file(path: &Path, color: ColorMode) -> Result<CoverImage> {
    decode(read_cache(path)?, SMALL_WIDTH, SMALL_HEIGHT, color)
}
fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}
/// The small thumbnail for a preview slot. While the worker hasn't written it
/// yet, the full thumbnail stands in, so nothing waits on the backfill; the
/// caller resizes either to its slot. A small file that exists but doesn't
/// decode is an error, and the caller asks for a repair as for a full one.
pub fn load_small(store: &Store, key: &str) -> Result<CoverImage> {
    let color = store.color_mode();
    match load_small_file(&store.small_cover_path(key), color) {
        Err(e) if is_not_found(&e) => load(&store.cover_path(key), color),
        result => result,
    }
}
/// Loads the thumbnail stored at tile size `width × height`, if the library
/// worker has written it. `Ok(None)` while it hasn't, so the caller resizes
/// a larger one instead; a file that exists but doesn't decode is an error,
/// and the caller asks for a repair as for a full one.
pub fn load_tile(store: &Store, key: &str, width: u32, height: u32) -> Result<Option<CoverImage>> {
    let color = store.color_mode();
    match read_cache(&store.tile_cover_path(key, width, height)) {
        Err(e) if is_not_found(&e) => Ok(None),
        result => result.and_then(|bytes| decode(bytes, width, height, color)).map(Some),
    }
}
/// Whether `size` gets tile thumbnails: larger than the small thumbnail,
/// which decodes cheaply enough, and not the full size itself.
pub fn tile_size(size: (u32, u32)) -> bool {
    (size.0 > SMALL_WIDTH || size.1 > SMALL_HEIGHT) && size != (WIDTH, HEIGHT) && size.0 <= 4096 && size.1 <= 4096
}
/// The size an `iw × ih` image scales to so it just covers a `w × h` slot.
pub fn fill_size(iw: u32, ih: u32, w: u32, h: u32) -> (u32, u32) {
    let (iw, ih) = (iw.max(1) as u64, ih.max(1) as u64);
    let (w, h) = (w as u64, h as u64);
    let (sw, sh) = if w * ih >= h * iw { (w, (ih * w).div_ceil(iw)) } else { ((iw * h).div_ceil(ih), h) };
    (sw as u32, sh as u32)
}
/// A thumbnail scaled until it covers a `w × h` slot, cropped centred across
/// and from the top down, where titles usually sit. Cover-shaped slots lose at
/// most a rounding pixel. Made on the library thread, so it uses the
/// smoother Triangle filter.
pub fn tile(full: &CoverImage, w: u32, h: u32) -> CoverImage {
    let (sw, sh) = fill_size(full.width(), full.height(), w, h);
    match full {
        CoverImage::Gray(img) => {
            let scaled = image::imageops::resize(img, sw, sh, image::imageops::FilterType::Triangle);
            let out = if (sw, sh) == (w, h) {
                scaled
            } else {
                image::imageops::crop_imm(&scaled, (sw - w) / 2, 0, w, h).to_image()
            };
            CoverImage::Gray(out)
        }
        CoverImage::Rgb(img) => {
            let scaled = image::imageops::resize(img, sw, sh, image::imageops::FilterType::Triangle);
            let out = if (sw, sh) == (w, h) {
                scaled
            } else {
                image::imageops::crop_imm(&scaled, (sw - w) / 2, 0, w, h).to_image()
            };
            CoverImage::Rgb(out)
        }
    }
}
fn shrink(full: &CoverImage) -> CoverImage {
    match full {
        CoverImage::Gray(img) => CoverImage::Gray(image::imageops::resize(
            img,
            SMALL_WIDTH,
            SMALL_HEIGHT,
            image::imageops::FilterType::Triangle,
        )),
        CoverImage::Rgb(img) => CoverImage::Rgb(image::imageops::resize(
            img,
            SMALL_WIDTH,
            SMALL_HEIGHT,
            image::imageops::FilterType::Triangle,
        )),
    }
}
/// The tile sizes to keep, most recently drawn first.
pub fn tile_sizes(store: &Store) -> Vec<(u32, u32)> {
    let sizes: Vec<(u32, u32)> = std::fs::read(store.tile_sizes_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    sizes.into_iter().filter(|&s| tile_size(s) && seen.insert(s)).take(TILE_SIZES).collect()
}
pub fn save_tile_sizes(store: &Store, sizes: &[(u32, u32)]) -> Result<()> {
    std::fs::create_dir_all(store.tile_sizes_path().parent().unwrap())?;
    atomic_bytes(&store.tile_sizes_path(), &serde_json::to_vec(sizes)?)
}
/// Deletes every book's tile thumbnail of `size`.
pub fn remove_tile_size(store: &Store, size: (u32, u32)) {
    let suffix = format!("-{}x{}.png", size.0, size.1);
    let dir = store.tile_sizes_path().parent().unwrap().to_owned();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_str().is_some_and(|name| name.ends_with(&suffix))
            && let Err(e) = std::fs::remove_file(entry.path())
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("removing an unused cover size: {e}");
        }
    }
}
fn write_png(path: &Path, image: &CoverImage) -> Result<()> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to_png(&mut bytes)?;
    atomic_bytes(path, bytes.get_ref())
}
/// Removes temp files left in `covers/` by interrupted writes (both current
/// `*.tmp` and legacy `*.json.tmp` names). Called once at worker startup,
/// before any cover writes begin.
pub fn sweep_temps(store: &Store) {
    let dir = store.tile_sizes_path().parent().unwrap().to_owned();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_str().is_some_and(|name| name.ends_with(".tmp"))
            && let Err(e) = std::fs::remove_file(entry.path())
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("removing interrupted cover temp file: {e}");
        }
    }
}
/// Deletes the cached thumbnails of a cover key: full, small and tiles.
pub fn remove(store: &Store, key: &str, sizes: &[(u32, u32)]) {
    let tiles = sizes.iter().map(|&(w, h)| store.tile_cover_path(key, w, h));
    for path in [store.cover_path(key), store.small_cover_path(key)].into_iter().chain(tiles) {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("removing deleted cover: {e}");
        }
    }
}
fn raster(bytes: &[u8], width: u32, height: u32) -> Result<RgbaImage> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    ensure!(
        matches!(reader.format(), Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)),
        "unsupported cover codec"
    );
    let (w, h) = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.into_dimensions()?;
    ensure!(w > 0 && h > 0 && w <= 4096 && h <= 4096 && w as u64 * h as u64 <= 6_000_000, "cover dimensions too large");
    let mut reader = reader;
    reader.limits(limits(4096, 4096, 24 * 1024 * 1024));
    let image = reader.decode()?.thumbnail(width, height);
    Ok(image.to_rgba8())
}
fn svg(bytes: &[u8], width: u32, height: u32) -> Result<RgbaImage> {
    ensure!(bytes.len() as u64 <= epub::XML_LIMIT, "SVG cover too large");
    epub::validate_svg(bytes)?;
    let options = resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(bytes, &options)?;
    let size = tree.size();
    ensure!(size.width() <= 4096.0 && size.height() <= 4096.0, "SVG dimensions too large");
    let scale = (width as f32 / size.width()).min(height as f32 / size.height());
    let w = (size.width() * scale).round().max(1.0) as u32;
    let h = (size.height() * scale).round().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).context("allocate SVG thumbnail")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny-skia stores premultiplied pixels; unpremultiply before compositing below.
    let mut image = RgbaImage::new(w, h);
    for (out, p) in image.pixels_mut().zip(pixmap.pixels()) {
        let p = p.demultiply();
        *out = image::Rgba([p.red(), p.green(), p.blue(), p.alpha()]);
    }
    Ok(image)
}
/// Fits the cover inside `width` x `height`, centred on a `background` gray.
pub fn sleep_cover(book: &Book, width: u32, height: u32, background: u8, color: ColorMode) -> Result<CoverImage> {
    let cover = book.metadata.cover.as_ref().context("no cover")?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&book.path)?)?;
    let bytes = epub::entry(&mut zip, cover, epub::IMAGE_LIMIT)?;
    let image = if cover.rsplit('.').next().is_some_and(|s| s.eq_ignore_ascii_case("svg")) {
        svg(&bytes, width, height)?
    } else {
        raster(&bytes, width, height)?
    };
    let dx = (width - image.width()) / 2;
    let dy = (height - image.height()) / 2;
    match color {
        ColorMode::Gray8 => {
            let mut gray = GrayImage::from_pixel(width, height, image::Luma([background]));
            for (x, y, p) in image.enumerate_pixels() {
                let a = p[3] as u32;
                let g = (luma(p[0], p[1], p[2]) as u32 * a + background as u32 * (255 - a) + 127) / 255;
                gray.put_pixel(dx + x, dy + y, image::Luma([g as u8]));
            }
            Ok(CoverImage::Gray(gray))
        }
        ColorMode::Color32 => {
            let mut rgb = RgbImage::from_pixel(width, height, image::Rgb([background, background, background]));
            for (x, y, p) in image.enumerate_pixels() {
                let a = p[3] as u32;
                let bg = background as u32 * (255 - a) + 127;
                let r = ((p[0] as u32 * a + bg) / 255) as u8;
                let g = ((p[1] as u32 * a + bg) / 255) as u8;
                let b = ((p[2] as u32 * a + bg) / 255) as u8;
                rgb.put_pixel(dx + x, dy + y, image::Rgb([r, g, b]));
            }
            Ok(CoverImage::Rgb(rgb))
        }
    }
}

/// Reuses current cache files after a size check, without decoding them. The
/// key includes the source stamp and color mode, so a changed book or preset
/// switch never matches an old entry. Missing small and tile thumbnails (one
/// per `sizes` entry) are made from the full one, without reading the book.
/// Visible-load failures go through [`repair`], which decodes the files. Cache
/// I/O failures are surfaced; absent/unsupported source art is a normal fallback.
pub fn generate(book: &Book, store: &Store, sizes: &[(u32, u32)]) -> Result<Cover> {
    let color = store.color_mode();
    let key = key(book, color);
    if matches!(&book.cover, Cover::Ready(old) if old == &key && cached(&store.cover_path(&key))) {
        let small = !cached(&store.small_cover_path(&key));
        let tiles: Vec<_> =
            sizes.iter().copied().filter(|&(w, h)| !cached(&store.tile_cover_path(&key, w, h))).collect();
        if !small && tiles.is_empty() {
            return Ok(book.cover.clone());
        }
        if let Ok(full) = load(&store.cover_path(&key), color) {
            if small {
                write_png(&store.small_cover_path(&key), &shrink(&full))?;
            }
            for (w, h) in tiles {
                write_png(&store.tile_cover_path(&key, w, h), &tile(&full, w, h))?;
            }
            return Ok(book.cover.clone());
        }
    }
    repair(book, store, sizes)
}
fn cached(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0 && m.len() <= CACHE_LIMIT)
}
/// Decodes the cached thumbnails and regenerates what is invalid: the small
/// and tile ones from the full one, and all from the source when the full one fails.
pub fn repair(book: &Book, store: &Store, sizes: &[(u32, u32)]) -> Result<Cover> {
    let color = store.color_mode();
    let key = key(book, color);
    if matches!(&book.cover, Cover::Unavailable(old) if old == &key) {
        return Ok(book.cover.clone());
    }
    let (path, small) = (store.cover_path(&key), store.small_cover_path(&key));
    if let Ok(full) = load(&path, color) {
        if load_small_file(&small, color).is_err() {
            write_png(&small, &shrink(&full))?;
        }
        for &(w, h) in sizes {
            if !matches!(load_tile(store, &key, w, h), Ok(Some(_))) {
                write_png(&store.tile_cover_path(&key, w, h), &tile(&full, w, h))?;
            }
        }
        return Ok(Cover::Ready(key));
    }
    let image = match sleep_cover(book, WIDTH, HEIGHT, 255, color) {
        Ok(image) => image,
        Err(e) => {
            log::debug!("title cover for {}: {e:#}", book.path.display());
            return Ok(Cover::Unavailable(key));
        }
    };
    write_png(&path, &image)?;
    write_png(&small, &shrink(&image))?;
    for &(w, h) in sizes {
        write_png(&store.tile_cover_path(&key, w, h), &tile(&image, w, h))?;
    }
    Ok(Cover::Ready(key))
}

/// Explicit opt-in diagnostic (`VERSO_COVER_BENCH=1`): the cost of a full and a
/// small thumbnail from file open to an image resized for a preview slot, over
/// the cached covers. Small files are made in a scratch folder next to the
/// cache and removed afterwards. Page-cache drops are advisory, so "cold" is
/// only as cold as the filesystem allows. Emits aggregate numbers only.
pub fn benchmark(store: &Store, index: &LibraryIndex) -> serde_json::Value {
    use std::time::Instant;
    const SAMPLE: usize = 200;
    const SLOT: (u32, u32) = (140, 210);
    fn percentile(values: &[f64], p: usize) -> f64 {
        let mut values = values.to_vec();
        values.sort_by(f64::total_cmp);
        values.get((values.len() * p).div_ceil(100).saturating_sub(1)).copied().unwrap_or(0.0)
    }
    fn drop_cached(path: &Path) {
        if let Ok(file) = std::fs::File::open(path) {
            use std::os::fd::AsRawFd;
            // SAFETY: a valid open descriptor; the call only advises the kernel.
            unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        }
    }
    /// Open + read, then decode, then total ms to a slot-sized image.
    fn time(path: &Path, width: u32, height: u32, color: ColorMode) -> Option<(f64, f64, f64, f64)> {
        let started = Instant::now();
        let bytes = read_cache(path).ok()?;
        let read = started.elapsed().as_secs_f64() * 1000.0;
        let image = decode(bytes, width, height, color).ok()?;
        let decoded = started.elapsed().as_secs_f64() * 1000.0;
        match &image {
            CoverImage::Gray(img) => {
                std::hint::black_box(image::imageops::resize(
                    img,
                    SLOT.0,
                    SLOT.1,
                    image::imageops::FilterType::Triangle,
                ));
            }
            CoverImage::Rgb(img) => {
                std::hint::black_box(image::imageops::resize(
                    img,
                    SLOT.0,
                    SLOT.1,
                    image::imageops::FilterType::Triangle,
                ));
            }
        }
        let total = started.elapsed().as_secs_f64() * 1000.0;
        let fast = Instant::now();
        match &image {
            CoverImage::Gray(img) => {
                std::hint::black_box(image::imageops::thumbnail(img, SLOT.0, SLOT.1));
            }
            CoverImage::Rgb(img) => {
                std::hint::black_box(image::imageops::thumbnail(img, SLOT.0, SLOT.1));
            }
        }
        Some((read, decoded - read, total, decoded + fast.elapsed().as_secs_f64() * 1000.0))
    }
    fn summary(times: &[(f64, f64, f64, f64)]) -> serde_json::Value {
        let fast: Vec<_> = times.iter().map(|t| t.3).collect();
        let read: Vec<_> = times.iter().map(|t| t.0).collect();
        let decode: Vec<_> = times.iter().map(|t| t.1).collect();
        let total: Vec<_> = times.iter().map(|t| t.2).collect();
        serde_json::json!({
            "n": times.len(),
            "read_p50_ms": percentile(&read, 50), "read_p90_ms": percentile(&read, 90),
            "decode_p50_ms": percentile(&decode, 50),
            "total_area_resize_p50_ms": percentile(&fast, 50), "total_area_resize_p90_ms": percentile(&fast, 90),
            "total_p50_ms": percentile(&total, 50), "total_p90_ms": percentile(&total, 90),
            "total_mean_ms": total.iter().sum::<f64>() / total.len().max(1) as f64,
        })
    }
    let cache_dir = store.cover_path("x").parent().unwrap().to_owned();
    let (mut cache_files, mut cache_bytes) = (0u64, 0u64);
    for entry in std::fs::read_dir(&cache_dir).into_iter().flatten().flatten() {
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            cache_files += 1;
            cache_bytes += meta.len();
        }
    }
    let scratch = cache_dir.with_file_name("cover-bench");
    let _ = std::fs::remove_dir_all(&scratch);
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        return serde_json::json!({ "error": format!("scratch folder: {e}") });
    }
    let keys: Vec<_> = index
        .books
        .iter()
        .filter(|b| b.available)
        .filter_map(|b| match &b.cover {
            Cover::Ready(key) if cached(&store.cover_path(key)) => Some(key.clone()),
            _ => None,
        })
        .take(SAMPLE)
        .collect();
    let (mut full_bytes, mut small_bytes) = (0u64, 0u64);
    let mut shrink_ms = Vec::new();
    let mut pairs = Vec::new();
    let color = store.color_mode();
    for key in &keys {
        let full = store.cover_path(key);
        let Ok(image) = load(&full, color) else { continue };
        let started = Instant::now();
        let small = shrink(&image);
        let mut bytes = Cursor::new(Vec::new());
        if small.write_to_png(&mut bytes).is_err() {
            continue;
        }
        shrink_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let path = scratch.join(format!("{key}-s.png"));
        let written = std::fs::File::create(&path).and_then(|mut f| {
            std::io::Write::write_all(&mut f, bytes.get_ref())?;
            f.sync_all()
        });
        if written.is_err() {
            continue;
        }
        full_bytes += std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        small_bytes += bytes.get_ref().len() as u64;
        pairs.push((full, path));
    }
    let run = |small: bool, cold: bool| {
        let times: Vec<_> = pairs
            .iter()
            .filter_map(|(full, path)| {
                let (path, width, height) =
                    if small { (path, SMALL_WIDTH, SMALL_HEIGHT) } else { (full, WIDTH, HEIGHT) };
                if cold {
                    drop_cached(path);
                }
                time(path, width, height, color)
            })
            .collect();
        summary(&times)
    };
    let result = serde_json::json!({
        "slot": [SLOT.0, SLOT.1],
        "cache_files": cache_files, "cache_bytes": cache_bytes,
        "sample": pairs.len(), "sample_full_bytes": full_bytes, "sample_small_bytes": small_bytes,
        "make_small_p50_ms": percentile(&shrink_ms, 50),
        "full_cold": run(false, true), "small_cold": run(true, true),
        "full_warm": run(false, false), "small_warm": run(true, false),
    });
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::worker::scan;
    #[test]
    fn png_svg_invalidation_and_corrupt_cache_recovery() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-covers-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let store = Store::at(root.join("data"));
        for version in [2, 3] {
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/library{version}.epub")),
                root.join(format!("{version}.epub")),
            )?;
        }
        let scan = scan(&root, &Default::default(), false, &|| false, |_| {}).unwrap();
        for mut book in scan.index.books {
            let Cover::Ready(k) = generate(&book, &store, &[])? else { panic!("fixture cover missing") };
            let CoverImage::Gray(image) = load(&store.cover_path(&k), ColorMode::Gray8)? else {
                panic!("expected gray thumbnail");
            };
            assert_eq!(image.dimensions(), (WIDTH, HEIGHT));
            assert!(image.pixels().any(|p| p[0] < 200));
            let CoverImage::Gray(sleep) = sleep_cover(&book, 600, 800, 0, ColorMode::Gray8)? else {
                panic!("expected gray sleep cover");
            };
            assert_eq!(sleep.get_pixel(0, 0)[0], 0, "padding is black");
            assert_eq!(sleep.dimensions(), (600, 800));
            assert!(sleep.pixels().any(|p| p[0] < 200));
            book.cover = Cover::Ready(k.clone());
            std::fs::write(store.cover_path(&k), b"broken")?;
            assert_eq!(generate(&book, &store, &[])?, Cover::Ready(k.clone()));
            assert!(load(&store.cover_path(&k), ColorMode::Gray8).is_err(), "unchanged confirmations do not decode");
            assert_eq!(repair(&book, &store, &[])?, Cover::Ready(k.clone()));
            assert!(load(&store.cover_path(&k), ColorMode::Gray8).is_ok());
            std::fs::write(store.cover_path(&k), b"")?;
            assert_eq!(generate(&book, &store, &[])?, Cover::Ready(k.clone()));
            assert!(load(&store.cover_path(&k), ColorMode::Gray8).is_ok(), "empty files are regenerated");
            std::fs::remove_file(store.cover_path(&k))?;
            assert_eq!(generate(&book, &store, &[])?, Cover::Ready(k.clone()));
            book.stamp.modified_nanos += 1;
            assert_ne!(key(&book, ColorMode::Gray8), k);
            book.metadata.cover = None;
            assert!(matches!(generate(&book, &store, &[])?, Cover::Unavailable(_)));
        }
        assert!(
            svg(br##"<svg xmlns="http://www.w3.org/2000/svg"><image href="/tmp/private.png"/></svg>"##, WIDTH, HEIGHT)
                .is_err()
        );
        assert!(raster(b"broken", WIDTH, HEIGHT).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn small_thumbnails_backfill_repair_and_fall_back_without_reading_the_book() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-small-covers-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let store = Store::at(root.join("data"));
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub"), root.join("3.epub"))?;
        let scan = scan(&root, &Default::default(), false, &|| false, |_| {}).unwrap();
        let mut book = scan.index.books[0].clone();
        // A new book gets both thumbnails from one source read.
        let Cover::Ready(k) = generate(&book, &store, &[])? else { panic!("fixture cover missing") };
        let (full, small) = (store.cover_path(&k), store.small_cover_path(&k));
        let CoverImage::Gray(image) = load_small_file(&small, ColorMode::Gray8)? else {
            panic!("expected gray small thumbnail");
        };
        assert_eq!(image.dimensions(), (SMALL_WIDTH, SMALL_HEIGHT));
        assert!(image.pixels().any(|p| p[0] < 200));
        book.cover = Cover::Ready(k.clone());
        // From here the book file is gone: everything below is made from the
        // full thumbnail. The key covers identity and stamp, not the path.
        std::fs::remove_file(root.join("3.epub"))?;
        // Missing: the slot shows the full thumbnail until the backfill writes it.
        std::fs::remove_file(&small)?;
        assert_eq!(load_small(&store, &k)?.dimensions(), (WIDTH, HEIGHT));
        assert_eq!(generate(&book, &store, &[])?, Cover::Ready(k.clone()));
        assert_eq!(load_small(&store, &k)?.dimensions(), (SMALL_WIDTH, SMALL_HEIGHT));
        // Corrupt: the pass only checks its size; a visible failure repairs it.
        std::fs::write(&small, b"broken")?;
        assert_eq!(generate(&book, &store, &[])?, Cover::Ready(k.clone()));
        assert!(load_small(&store, &k).is_err(), "a broken small file is not hidden by the full one");
        assert_eq!(repair(&book, &store, &[])?, Cover::Ready(k.clone()));
        assert_eq!(load_small(&store, &k)?.dimensions(), (SMALL_WIDTH, SMALL_HEIGHT));
        // A new key (changed file) has neither thumbnail.
        book.stamp.modified_nanos += 1;
        assert!(!store.small_cover_path(&key(&book, ColorMode::Gray8)).exists());
        // Deleting a book removes both files.
        remove(&store, &k, &[]);
        assert!(!full.exists() && !small.exists());
        remove(&store, &k, &[]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn tile_thumbnails_backfill_repair_and_go_with_their_size() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-tile-covers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let store = Store::at(root.join("data"));
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library3.epub"), root.join("3.epub"))?;
        let scan = scan(&root, &Default::default(), false, &|| false, |_| {}).unwrap();
        let mut book = scan.index.books[0].clone();
        let sizes = [(243, 365), (209, 313)];
        // A new book gets every tile from the same source read.
        let Cover::Ready(k) = generate(&book, &store, &sizes)? else { panic!("fixture cover missing") };
        book.cover = Cover::Ready(k.clone());
        let full = load(&store.cover_path(&k), ColorMode::Gray8)?;
        for (w, h) in sizes {
            assert_eq!(load_tile(&store, &k, w, h)?, Some(tile(&full, w, h)));
        }
        std::fs::remove_file(root.join("3.epub"))?;
        // A size added later is made from the full thumbnail.
        assert_eq!(load_tile(&store, &k, 180, 270)?, None, "not written yet");
        assert_eq!(generate(&book, &store, &[(180, 270)])?, Cover::Ready(k.clone()));
        assert_eq!(load_tile(&store, &k, 180, 270)?.map(|i| i.dimensions()), Some((180, 270)));
        // A broken tile is only size-checked by a pass and fixed by a repair.
        std::fs::write(store.tile_cover_path(&k, 243, 365), b"broken")?;
        assert_eq!(generate(&book, &store, &sizes)?, Cover::Ready(k.clone()));
        assert!(load_tile(&store, &k, 243, 365).is_err());
        assert_eq!(repair(&book, &store, &sizes)?, Cover::Ready(k.clone()));
        assert!(load_tile(&store, &k, 243, 365)?.is_some());
        // Dropping a size removes it for every book; deleting a book removes its tiles.
        remove_tile_size(&store, (180, 270));
        assert!(!store.tile_cover_path(&k, 180, 270).exists());
        assert!(store.tile_cover_path(&k, 209, 313).exists() && store.cover_path(&k).exists());
        remove(&store, &k, &sizes);
        assert!(std::fs::read_dir(store.cover_path(&k).parent().unwrap())?.next().is_none());
        // The size list keeps valid, distinct sizes, most recent first.
        assert!(tile_sizes(&store).is_empty());
        save_tile_sizes(&store, &[(243, 365), (100, 150), (WIDTH, HEIGHT), (243, 365), (209, 313)])?;
        assert_eq!(tile_sizes(&store), [(243, 365), (209, 313)]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn color_covers_use_distinct_keys_and_write_rgb_pngs_and_sleep_covers() -> Result<()> {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("verso-color-covers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        // Build a minimal EPUB with a red SVG cover so we can verify color survives end-to-end.
        let epub_path = root.join("color.epub");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&epub_path)?);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("META-INF/container.xml", opts)?;
        zip.write_all(
            br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        )?;
        zip.start_file("content.opf", opts)?;
        zip.write_all(
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Color Book</dc:title></metadata><manifest><item id="cover" href="cover.svg" media-type="image/svg+xml" properties="cover-image"/></manifest><spine/></package>"#,
        )?;
        zip.start_file("cover.svg", opts)?;
        zip.write_all(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="300"><rect width="200" height="300" fill="#d03020"/></svg>"##,
        )?;
        zip.finish()?;

        let scan = scan(&root, &Default::default(), false, &|| false, |_| {}).unwrap();
        let book = &scan.index.books[0];
        let gray_key = key(book, ColorMode::Gray8);
        let color_key = key(book, ColorMode::Color32);
        assert_ne!(gray_key, color_key, "color mode must be part of the cover cache key");

        let gray_store = Store::at(root.join("data")).with_color(ColorMode::Gray8);
        let color_store = Store::at(root.join("data")).with_color(ColorMode::Color32);
        assert_eq!(generate(book, &gray_store, &[(200, 300)])?, Cover::Ready(gray_key.clone()));
        assert_eq!(generate(book, &color_store, &[(200, 300)])?, Cover::Ready(color_key.clone()));

        // Verify the on-disk PNG color types: Gray8 writes L8, Color32 writes Rgb8.
        let gray_on_disk = image::open(gray_store.cover_path(&gray_key))?;
        assert!(matches!(gray_on_disk, image::DynamicImage::ImageLuma8(_)));
        let color_on_disk = image::open(color_store.cover_path(&color_key))?;
        assert!(matches!(color_on_disk, image::DynamicImage::ImageRgb8(_)));

        let CoverImage::Rgb(full) = load(&color_store.cover_path(&color_key), ColorMode::Color32)? else {
            panic!("expected RGB full thumbnail");
        };
        let p = full.get_pixel(WIDTH / 2, HEIGHT / 2).0;
        assert!(p[0] > 180 && p[1] < 80 && p[2] < 80, "expected red pixel, got {p:?}");

        let CoverImage::Rgb(small) = load_small(&color_store, &color_key)? else {
            panic!("expected RGB small thumbnail");
        };
        let p = small.get_pixel(SMALL_WIDTH / 2, SMALL_HEIGHT / 2).0;
        assert!(p[0] > 180 && p[1] < 80 && p[2] < 80, "expected red small pixel, got {p:?}");

        let Some(CoverImage::Rgb(tile_img)) = load_tile(&color_store, &color_key, 200, 300)? else {
            panic!("expected RGB tile thumbnail");
        };
        let p = tile_img.get_pixel(100, 150).0;
        assert!(p[0] > 180 && p[1] < 80 && p[2] < 80, "expected red tile pixel, got {p:?}");

        let CoverImage::Rgb(sleep) = sleep_cover(book, 600, 800, 0, ColorMode::Color32)? else {
            panic!("expected RGB sleep cover");
        };
        assert_eq!(sleep.get_pixel(0, 0).0, [0, 0, 0], "padding is black");
        let p = sleep.get_pixel(300, 400).0;
        assert!(p[0] > 180 && p[1] < 80 && p[2] < 80, "expected red sleep cover pixel, got {p:?}");

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn tiles_fill_their_slot_and_crop_like_previews() {
        let cover = CoverImage::Gray(GrayImage::from_fn(WIDTH, HEIGHT, |_, y| {
            image::Luma([if y < HEIGHT / 3 { 0xff } else { 0 }])
        }));
        assert_eq!(fill_size(WIDTH, HEIGHT, 240, 360), (240, 360));
        assert_eq!(fill_size(WIDTH, HEIGHT, 243, 365), (244, 365), "a rounding pixel is cropped");
        assert_eq!(fill_size(WIDTH, HEIGHT, 240, 300), (240, 360), "wide slots crop the bottom");
        assert_eq!(fill_size(WIDTH, HEIGHT, 120, 300), (200, 300), "tall slots crop the sides");
        let CoverImage::Gray(wide) = tile(&cover, 240, 300) else { panic!("expected gray tile") };
        assert_eq!(wide.dimensions(), (240, 300));
        assert_eq!((wide.get_pixel(0, 0)[0], wide.get_pixel(0, 299)[0]), (0xff, 0));
        assert!(tile_size((145, 200)) && tile_size((100, 217)));
        assert!(!tile_size((SMALL_WIDTH, SMALL_HEIGHT)) && !tile_size((WIDTH, HEIGHT)) && !tile_size((5000, 9000)));
    }

    #[test]
    fn sweep_temps_removes_interrupted_cover_temp_files_only() -> Result<()> {
        let root = std::env::temp_dir().join(format!("verso-cover-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone());
        let dir = store.tile_sizes_path().parent().unwrap().to_owned();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("abc.png"), b"png")?;
        std::fs::write(dir.join("abc.png.tmp"), b"partial")?;
        std::fs::write(dir.join("legacy.json.tmp"), b"legacy")?;
        std::fs::write(dir.join("tile-sizes.json"), b"[]")?;
        sweep_temps(&store);
        assert!(dir.join("abc.png").exists());
        assert!(dir.join("tile-sizes.json").exists());
        assert!(!dir.join("abc.png.tmp").exists());
        assert!(!dir.join("legacy.json.tmp").exists());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
