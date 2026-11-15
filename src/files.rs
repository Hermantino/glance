//! Directory listing and fast image loading (including RAW embedded previews).
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, RgbaImage};
use std::path::{Path, PathBuf};

pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "tif", "tiff", "webp", "gif", "bmp",
];
pub const RAW_EXTS: &[&str] = &[
    "orf", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "rw2", "raf", "dng", "pef", "srw",
    "rwl", "3fr", "iiq", "erf", "kdc", "mrw", "x3f",
];

fn ext(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn is_raw(p: &Path) -> bool {
    RAW_EXTS.contains(&ext(p).as_str())
}

pub fn is_supported(p: &Path) -> bool {
    let e = ext(p);
    IMAGE_EXTS.contains(&e.as_str()) || RAW_EXTS.contains(&e.as_str())
}

/// A path for display, with the home folder shown as `~`.
pub fn display_path(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".into()
        } else {
            format!("~/{}", rest.display())
        };
    }
    p.display().to_string()
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// All supported images in `dir`, in natural order ("img2" before "img10").
pub fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| !file_name(p).starts_with('.') && is_supported(p) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| natord::compare_ignore_case(&file_name(a), &file_name(b)));
    v
}

/// A RAW file with the same name as `p` in `files` (or `p` itself if it is RAW).
pub fn raw_sibling(p: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    if is_raw(p) {
        return Some(p.to_path_buf());
    }
    let stem = p.file_stem()?;
    files
        .iter()
        .find(|f| is_raw(f) && f.file_stem() == Some(stem) && f.parent() == p.parent())
        .cloned()
}

pub struct Loaded {
    pub img: RgbaImage,
    /// Size of the full-resolution image (after EXIF rotation).
    pub full_w: u32,
    pub full_h: u32,
}

/// Load an image, applying its EXIF orientation. With `max_side`, the result is
/// downscaled (fast area averaging) so its longest side is at most that.
pub fn load(path: &Path, max_side: Option<u32>) -> Result<Loaded, String> {
    let img = if is_raw(path) {
        load_raw_preview(path)?
    } else {
        load_regular(path)?
    };
    let (full_w, full_h) = (img.width(), img.height());
    let img = match max_side {
        Some(m) if full_w.max(full_h) > m => img.thumbnail(m, m),
        _ => img,
    };
    Ok(Loaded {
        img: img.into_rgba8(),
        full_w,
        full_h,
    })
}

fn load_regular(path: &Path) -> Result<DynamicImage, String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// RAW files carry a full-size JPEG made by the camera. Show that instead of
/// developing the RAW: instant, and the same look as the camera JPEG.
fn load_raw_preview(path: &Path) -> Result<DynamicImage, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let orientation = tiff_orientation(&data)
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    let mut candidates = embedded_jpegs(&data);
    candidates.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for jpeg in candidates.into_iter().take(4) {
        if let Ok(mut img) = image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg) {
            if img.width() < 200 {
                continue;
            }
            img.apply_orientation(orientation);
            return Ok(img);
        }
    }
    Err("this RAW file has no embedded preview".into())
}

/// Every complete JPEG stream (SOI … EOI) found in `d`.
fn embedded_jpegs(d: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < d.len() {
        if d[i] == 0xFF
            && d[i + 1] == 0xD8
            && d[i + 2] == 0xFF
            && let Some(end) = jpeg_end(d, i)
        {
            out.push(&d[i..end]);
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Walk the JPEG marker structure from `start` and return the offset just past EOI.
fn jpeg_end(d: &[u8], start: usize) -> Option<usize> {
    let mut p = start + 2;
    loop {
        if p + 4 > d.len() || d[p] != 0xFF {
            return None;
        }
        let m = d[p + 1];
        match m {
            0xFF => p += 1,
            0xD9 => return Some(p + 2),
            0x01 | 0xD0..=0xD7 => p += 2,
            _ => {
                let len = u16::from_be_bytes([d[p + 2], d[p + 3]]) as usize;
                if len < 2 {
                    return None;
                }
                p += 2 + len;
                if m == 0xDA {
                    // Entropy-coded data: runs until a marker that isn't a stuffed byte or restart.
                    while p + 1 < d.len() {
                        if d[p] == 0xFF {
                            match d[p + 1] {
                                0x00 | 0xD0..=0xD7 => p += 2,
                                0xFF => p += 1,
                                _ => break,
                            }
                        } else {
                            p += 1;
                        }
                    }
                }
            }
        }
    }
}

/// EXIF orientation tag from IFD0 of a TIFF-based RAW (ORF, NEF, CR2, ARW, DNG…).
fn tiff_orientation(d: &[u8]) -> Option<u8> {
    let le = match d.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r16 = |o: usize| -> Option<u16> {
        let b = d.get(o..o + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let r32 = |o: usize| -> Option<u32> {
        let b = d.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    let ifd = r32(4)? as usize;
    let n = r16(ifd)? as usize;
    (0..n.min(512)).find_map(|k| {
        let e = ifd + 2 + k * 12;
        (r16(e)? == 0x0112)
            .then(|| r16(e + 8).map(|v| v as u8))
            .flatten()
    })
}

/// Where a saved copy goes: `name-edit.jpg`, `name-edit-2.jpg`, …
pub fn copy_path(src: &Path) -> PathBuf {
    let dir = src.parent().unwrap_or(Path::new("."));
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = if is_raw(src) || !IMAGE_EXTS.contains(&ext(src).as_str()) {
        "jpg".to_string()
    } else {
        ext(src)
    };
    let mut p = dir.join(format!("{stem}-edit.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem}-edit-{n}.{ext}"));
        n += 1;
    }
    p
}

/// Write `img` to `dst` in the format its extension names.
pub fn write_image(img: &RgbaImage, dst: &Path, jpeg_quality: u8) -> Result<(), String> {
    let e = ext(dst);
    let tmp = dst.with_extension(format!("{e}.glance-tmp"));
    let res = if e == "jpg" || e == "jpeg" || e == "jpe" {
        let rgb = DynamicImage::ImageRgba8(img.clone()).into_rgb8();
        std::fs::File::create(&tmp)
            .map_err(|e| e.to_string())
            .and_then(|f| {
                let mut w = std::io::BufWriter::new(f);
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, jpeg_quality)
                    .encode_image(&rgb)
                    .map_err(|e| e.to_string())
            })
    } else {
        let fmt = ImageFormat::from_extension(&e).ok_or("unsupported format")?;
        let out = if matches!(
            fmt,
            ImageFormat::Png | ImageFormat::WebP | ImageFormat::Tiff
        ) {
            DynamicImage::ImageRgba8(img.clone())
        } else {
            DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(img.clone()).into_rgb8())
        };
        out.save_with_format(&tmp, fmt).map_err(|e| e.to_string())
    };
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, dst).map_err(|e| e.to_string())
}

/// Copy EXIF/XMP metadata from `src` to `dst` with exiftool (if installed),
/// marking the orientation as normal since rotation is now baked into the pixels.
pub fn copy_metadata(src: &Path, dst: &Path) {
    let _ = std::process::Command::new("exiftool")
        .args(["-q", "-q", "-overwrite_original", "-TagsFromFile"])
        .arg(src)
        .args([
            "-all:all",
            "-Orientation#=1",
            "-ThumbnailImage=",
            "-PreviewImage=",
        ])
        .arg(dst)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
