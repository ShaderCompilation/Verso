//! Fixed-size grayscale cover cache. All source decoding happens on the library thread.
use super::{Book, Cover, epub};
use crate::gfx::luma;
use crate::store::{Store, atomic_bytes};
use anyhow::{Context, Result, ensure};
use image::{GrayImage, ImageReader, RgbaImage};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use std::path::Path;

pub const WIDTH: u32 = 288;
pub const HEIGHT: u32 = 432;
const VERSION: u8 = 1;
const CACHE_LIMIT: u64 = 1024 * 1024;

pub fn key(book: &Book) -> String {
    let value = serde_json::to_vec(&(VERSION, &book.id, &book.stamp)).expect("serializable cover key");
    format!("{:x}", Sha256::digest(value))
}
fn limits(width: u32, height: u32, alloc: u64) -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(alloc);
    limits
}
pub fn load(path: &Path) -> Result<GrayImage> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(CACHE_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= CACHE_LIMIT, "cover cache too large");
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    reader.limits(limits(WIDTH, HEIGHT, 2 * 1024 * 1024));
    let image = reader.decode()?;
    ensure!(image.width() == WIDTH && image.height() == HEIGHT, "invalid thumbnail size");
    Ok(image.into_luma8())
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
pub fn sleep_cover(book: &Book, width: u32, height: u32) -> Result<GrayImage> {
    let cover = book.metadata.cover.as_ref().context("no cover")?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&book.path)?)?;
    let bytes = epub::entry(&mut zip, cover, epub::IMAGE_LIMIT)?;
    let image = if cover.rsplit('.').next().is_some_and(|s| s.eq_ignore_ascii_case("svg")) {
        svg(&bytes, width, height)?
    } else {
        raster(&bytes, width, height)?
    };
    let mut gray = GrayImage::from_pixel(width, height, image::Luma([255]));
    let dx = (width - image.width()) / 2;
    let dy = (height - image.height()) / 2;
    for (x, y, p) in image.enumerate_pixels() {
        let a = p[3] as u32;
        let g = (luma(p[0], p[1], p[2]) as u32 * a + 255 * (255 - a) + 127) / 255;
        gray.put_pixel(dx + x, dy + y, image::Luma([g as u8]));
    }
    Ok(gray)
}

/// Reuses a current cache file after a size check, without decoding it. The key
/// includes the source stamp, so a changed book never matches an old entry.
/// Visible-load failures go through [`repair`], which decodes the file.
/// Cache I/O failures are surfaced; absent/unsupported source art is a normal fallback.
pub fn generate(book: &Book, store: &Store) -> Result<Cover> {
    let key = key(book);
    if matches!(&book.cover, Cover::Ready(old) if old == &key && cached(&store.cover_path(&key))) {
        return Ok(book.cover.clone());
    }
    repair(book, store)
}
fn cached(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0 && m.len() <= CACHE_LIMIT)
}
/// Decodes the cached thumbnail and regenerates it from the source when invalid.
pub fn repair(book: &Book, store: &Store) -> Result<Cover> {
    let key = key(book);
    if matches!(&book.cover, Cover::Unavailable(old) if old == &key) {
        return Ok(book.cover.clone());
    }
    let path = store.cover_path(&key);
    if load(&path).is_ok() {
        return Ok(Cover::Ready(key));
    }
    let image = match sleep_cover(book, WIDTH, HEIGHT) {
        Ok(image) => image,
        Err(e) => {
            log::debug!("title cover for {}: {e:#}", book.path.display());
            return Ok(Cover::Unavailable(key));
        }
    };
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png)?;
    atomic_bytes(&path, bytes.get_ref())?;
    Ok(Cover::Ready(key))
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
            let Cover::Ready(k) = generate(&book, &store)? else { panic!("fixture cover missing") };
            let image = load(&store.cover_path(&k))?;
            assert_eq!(image.dimensions(), (WIDTH, HEIGHT));
            assert!(image.pixels().any(|p| p[0] < 200));
            let sleep = sleep_cover(&book, 600, 800)?;
            assert_eq!(sleep.dimensions(), (600, 800));
            assert!(sleep.pixels().any(|p| p[0] < 200));
            book.cover = Cover::Ready(k.clone());
            std::fs::write(store.cover_path(&k), b"broken")?;
            assert_eq!(generate(&book, &store)?, Cover::Ready(k.clone()));
            assert!(load(&store.cover_path(&k)).is_err(), "unchanged confirmations do not decode");
            assert_eq!(repair(&book, &store)?, Cover::Ready(k.clone()));
            assert!(load(&store.cover_path(&k)).is_ok());
            std::fs::write(store.cover_path(&k), b"")?;
            assert_eq!(generate(&book, &store)?, Cover::Ready(k.clone()));
            assert!(load(&store.cover_path(&k)).is_ok(), "empty files are regenerated");
            std::fs::remove_file(store.cover_path(&k))?;
            assert_eq!(generate(&book, &store)?, Cover::Ready(k.clone()));
            book.stamp.modified_nanos += 1;
            assert_ne!(key(&book), k);
            book.metadata.cover = None;
            assert!(matches!(generate(&book, &store)?, Cover::Unavailable(_)));
        }
        assert!(
            svg(br##"<svg xmlns="http://www.w3.org/2000/svg"><image href="/tmp/private.png"/></svg>"##, WIDTH, HEIGHT)
                .is_err()
        );
        assert!(raster(b"broken", WIDTH, HEIGHT).is_err());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
