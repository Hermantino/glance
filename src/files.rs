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

/// HEIF family: Fujifilm `.HIF`, iPhone `.HEIC`, AVIF. Decoded through libheif.
pub const HEIF_EXTS: &[&str] = &["heic", "heif", "hif", "avif"];

pub fn is_heif(p: &Path) -> bool {
    HEIF_EXTS.contains(&ext(p).as_str())
}

/// Formats glance can open but not write back: edits are saved as a JPEG copy.
pub fn read_only_format(p: &Path) -> bool {
    is_raw(p) || is_heif(p)
}

pub fn is_raf(p: &Path) -> bool {
    ext(p) == "raf"
}

pub fn is_raw(p: &Path) -> bool {
    RAW_EXTS.contains(&ext(p).as_str())
}

pub fn is_supported(p: &Path) -> bool {
    let e = ext(p);
    IMAGE_EXTS.contains(&e.as_str())
        || RAW_EXTS.contains(&e.as_str())
        || HEIF_EXTS.contains(&e.as_str())
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
/// Returned by `load_wanted` when the photo was no longer wanted.
pub const SKIPPED: &str = "skipped: no longer needed";

pub fn load(path: &Path, max_side: Option<u32>) -> Result<Loaded, String> {
    load_wanted(path, max_side, &|| true)
}

/// Like `load`, but gives up with `SKIPPED` if `wanted` says the photo isn't
/// needed any more by the time it's its turn to be read (browsing moved on).
pub fn load_wanted(
    path: &Path,
    max_side: Option<u32>,
    wanted: &dyn Fn() -> bool,
) -> Result<Loaded, String> {
    let (img, orientation) = if is_raw(path) {
        load_raw_preview(path, wanted)?
    } else if is_heif(path) {
        (
            DynamicImage::ImageRgba8(crate::heic::decode(path)?),
            Orientation::NoTransforms,
        )
    } else {
        load_regular(path, wanted)?
    };
    // Shrink first, then turn: turning the small copy is much cheaper.
    let (mut full_w, mut full_h) = (img.width(), img.height());
    let mut img = match max_side {
        Some(m) => shrink(img, m),
        None => img,
    };
    img.apply_orientation(orientation);
    if swaps_sides(orientation) {
        std::mem::swap(&mut full_w, &mut full_h);
    }
    Ok(Loaded {
        img: img.into_rgba8(),
        full_w,
        full_h,
    })
}

fn swaps_sides(o: Orientation) -> bool {
    matches!(
        o,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// Files are read whole, one at a time: on an SD card or a network share,
/// parallel reads would make the photo you're looking at wait for the ones
/// being preloaded. Decoding still runs in parallel.
static READ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn read_file(path: &Path, wanted: &dyn Fn() -> bool) -> Result<Vec<u8>, String> {
    let _one_at_a_time = READ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !wanted() {
        return Err(SKIPPED.into());
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

/// Fujifilm RAF: the camera's JPEG preview is listed in the header (offset and
/// length at byte 84), so only that part is read: about 6 MB of a 47 MB file.
fn read_raf_preview(path: &Path, wanted: &dyn Fn() -> bool) -> Option<Result<Vec<u8>, String>> {
    use std::io::{Read, Seek, SeekFrom};
    let _one_at_a_time = READ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !wanted() {
        return Some(Err(SKIPPED.into()));
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 92];
    f.read_exact(&mut head).ok()?;
    if !head.starts_with(b"FUJIFILMCCD-RAW") {
        return None;
    }
    let off = u32::from_be_bytes(head[84..88].try_into().ok()?) as u64;
    let len = u32::from_be_bytes(head[88..92].try_into().ok()?) as usize;
    if !(4..=64 << 20).contains(&len) || off > f.metadata().ok()?.len() {
        return None;
    }
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut jpeg = vec![0u8; len];
    f.read_exact(&mut jpeg).ok()?;
    jpeg.starts_with(&[0xFF, 0xD8]).then_some(Ok(jpeg))
}

/// The bytes to look for a RAW file's previews and camera info in: just the
/// preview for Fujifilm RAF, otherwise the whole file.
pub fn raw_preview_bytes(path: &Path, wanted: &dyn Fn() -> bool) -> Result<Vec<u8>, String> {
    match read_raf_preview(path, wanted) {
        Some(r) => r,
        None => read_file(path, wanted),
    }
}

fn load_regular(
    path: &Path,
    wanted: &dyn Fn() -> bool,
) -> Result<(DynamicImage, Orientation), String> {
    let data = read_file(path, wanted)?;
    let reader = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    Ok((img, orientation))
}

/// Shrink `img` so its longest side is at most `max`, averaging the pixels each
/// output pixel covers (like `DynamicImage::thumbnail`), on all CPU cores.
pub fn shrink(img: DynamicImage, max: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= max {
        return img;
    }
    let s = max as f64 / w.max(h) as f64;
    let ow = ((w as f64 * s).round() as u32).clamp(1, max);
    let oh = ((h as f64 * s).round() as u32).clamp(1, max);
    match img {
        DynamicImage::ImageRgb8(i) => DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(ow, oh, area_average(i.as_raw(), w, h, 3, ow, oh)).unwrap(),
        ),
        DynamicImage::ImageRgba8(i) => DynamicImage::ImageRgba8(
            RgbaImage::from_raw(ow, oh, area_average(i.as_raw(), w, h, 4, ow, oh)).unwrap(),
        ),
        other => other.thumbnail(max, max),
    }
}

/// For each output position, the source pixels it covers and their weights.
fn spans(n: u32, on: u32) -> Vec<Vec<(usize, f32)>> {
    let scale = n as f64 / on as f64;
    (0..on)
        .map(|o| {
            let (a, b) = (o as f64 * scale, (o + 1) as f64 * scale);
            (a.floor() as usize..(b.ceil() as usize).min(n as usize))
                .filter_map(|i| {
                    let cover = b.min(i as f64 + 1.0) - a.max(i as f64);
                    (cover > 0.0).then_some((i, (cover / scale) as f32))
                })
                .collect()
        })
        .collect()
}

fn area_average(src: &[u8], w: u32, h: u32, c: usize, ow: u32, oh: u32) -> Vec<u8> {
    use rayon::prelude::*;
    let (xs, ys) = (spans(w, ow), spans(h, oh));
    let row_len = w as usize * c;
    let mut out = vec![0u8; ow as usize * oh as usize * c];
    out.par_chunks_mut(ow as usize * c)
        .zip(ys.par_iter())
        .for_each(|(row, ys)| {
            let mut acc = vec![0f32; ow as usize * c];
            for &(sy, wy) in ys {
                let srow = &src[sy * row_len..(sy + 1) * row_len];
                for (ox, xs) in xs.iter().enumerate() {
                    let a = &mut acc[ox * c..(ox + 1) * c];
                    for &(sx, wx) in xs {
                        let wgt = wx * wy;
                        for (k, v) in a.iter_mut().enumerate() {
                            *v += srow[sx * c + k] as f32 * wgt;
                        }
                    }
                }
            }
            for (o, a) in row.iter_mut().zip(&acc) {
                *o = (a + 0.5).clamp(0.0, 255.0) as u8;
            }
        });
    out
}

/// RAW files carry a full-size JPEG made by the camera. Show that instead of
/// developing the RAW: instant, and the same look as the camera JPEG.
fn load_raw_preview(
    path: &Path,
    wanted: &dyn Fn() -> bool,
) -> Result<(DynamicImage, Orientation), String> {
    let data = raw_preview_bytes(path, wanted)?;
    let mut candidates = embedded_jpegs(&data);
    // TIFF-based RAWs (ORF, NEF, CR2, ARW, DNG…) store the rotation in their
    // header. Fujifilm RAF keeps it only in the embedded JPEG's EXIF.
    let orientation = tiff_orientation(&data)
        .or_else(|| candidates.iter().find_map(|j| jpeg_orientation(j)))
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms);
    candidates.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for jpeg in candidates.into_iter().take(4) {
        if let Ok(img) = image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg) {
            if img.width() < 200 {
                continue;
            }
            return Ok((img, orientation));
        }
    }
    Err("this RAW file has no embedded preview".into())
}

/// EXIF orientation stored inside a JPEG stream.
fn jpeg_orientation(jpeg: &[u8]) -> Option<u8> {
    let ex = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(jpeg))
        .ok()?;
    let v = ex
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)?;
    u8::try_from(v).ok()
}

/// Does this RAW file start with a TIFF header (ORF, NEF, CR2, ARW, DNG…)?
pub fn is_tiff_based(d: &[u8]) -> bool {
    matches!(d.get(0..2), Some(b"II" | b"MM"))
}

/// Every complete JPEG stream (SOI … EOI) found in `d`.
pub fn embedded_jpegs(d: &[u8]) -> Vec<&[u8]> {
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
        if p + 2 > d.len() || d[p] != 0xFF {
            return None;
        }
        let m = d[p + 1];
        match m {
            0xFF => p += 1,
            0xD9 => return Some(p + 2),
            0x01 | 0xD0..=0xD7 => p += 2,
            _ if p + 4 > d.len() => return None,
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

/// Write `img` to `dst` in the format its extension names, through a temporary
/// file that replaces `dst` only once it is complete. With `meta_from`, the
/// camera information (EXIF, XMP, colour profile) is copied onto the temporary
/// file first. Returns whether that metadata copy succeeded.
pub fn write_image(
    img: &RgbaImage,
    dst: &Path,
    jpeg_quality: u8,
    meta_from: Option<&Path>,
) -> Result<bool, String> {
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
                    .map_err(|e| e.to_string())?;
                use std::io::Write;
                w.flush().map_err(|e| e.to_string())
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
    let meta = meta_from.is_some_and(|m| copy_metadata(m, &tmp));
    if let Err(e) = std::fs::rename(&tmp, dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(meta)
}

/// Is exiftool installed? Checked once.
pub fn exiftool_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("exiftool")
            .arg("-ver")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Copy EXIF/XMP metadata and the colour profile from `src` to `dst` with
/// exiftool, marking the orientation as normal since rotation is now baked into
/// the pixels. Returns false if exiftool is missing or failed.
fn copy_metadata(src: &Path, dst: &Path) -> bool {
    exiftool_available()
        && std::process::Command::new("exiftool")
            .args(["-q", "-q", "-overwrite_original", "-TagsFromFile"])
            .arg(src)
            .args([
                "-all:all",
                "-icc_profile",
                "-Orientation#=1",
                "-ThumbnailImage=",
                "-PreviewImage=",
            ])
            .arg(dst)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// When the photo was taken, from EXIF (with sub-seconds when the camera
/// records them). glance keeps this tag when it saves, so it identifies a
/// shot across edits.
fn capture_time(data: &[u8]) -> Option<String> {
    let ex = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(data))
        .ok()?;
    let get = |t| {
        ex.get_field(t, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
    };
    let date = get(exif::Tag::DateTimeOriginal)?;
    Some(format!(
        "{date}.{}",
        get(exif::Tag::SubSecTimeOriginal).unwrap_or_default()
    ))
}

/// Back up `path` into `.originals` next to it before it is overwritten, through
/// a temporary file so a failed copy never counts as a backup.
///
/// Only the first original of a photo is kept: saving the same photo again
/// (same bytes, or same capture time, since saves keep it) leaves the backup
/// alone. A different photo with the same name (a camera counter reset, a
/// second card) is recognised by its different capture time and gets a
/// numbered backup (`IMG_1-2.jpg`) instead of being skipped.
pub fn backup_original(path: &Path) -> Result<PathBuf, String> {
    let dir = path.parent().unwrap_or(Path::new(".")).join(".originals");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .ok_or("no file name")?
        .to_string_lossy()
        .into_owned();
    let (stem, dot_ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let current = std::fs::read(path).map_err(|e| e.to_string())?;
    let shot = capture_time(&current);
    let mut backup = dir.join(&name);
    let mut n = 2;
    while backup.exists() {
        let old = std::fs::read(&backup).map_err(|e| e.to_string())?;
        // Without a capture time on both sides there's no telling the photos
        // apart, so it's treated as the same photo and its first original kept.
        let same_photo = old == current
            || match (capture_time(&old), &shot) {
                (Some(a), Some(b)) => a == *b,
                _ => true,
            };
        if same_photo {
            return Ok(backup);
        }
        backup = dir.join(format!("{stem}-{n}{dot_ext}"));
        n += 1;
    }
    let tmp = dir.join(format!(".{name}.glance-tmp"));
    let res = std::fs::write(&tmp, &current)
        .and_then(|_| std::fs::File::open(&tmp)?.sync_all())
        .and_then(|_| std::fs::rename(&tmp, &backup));
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not back up the original: {e}"));
    }
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("glance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A JPEG of `w`×`h` with an EXIF block that holds only an orientation.
    fn jpeg_with_orientation(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        jpeg_with_exif(w, h, orientation, None)
    }

    /// A JPEG of `w`×`h` with an EXIF orientation and, optionally, the date it
    /// was taken ("YYYY:MM:DD HH:MM:SS").
    fn jpeg_with_exif(w: u32, h: u32, orientation: u16, taken: Option<&str>) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&img)
            .unwrap();
        // Little-endian TIFF: IFD0 at 8 with Orientation (and a pointer to the
        // Exif IFD at 38 holding DateTimeOriginal, whose text sits at 56).
        let entry = |tag: u16, typ: u16, count: u32, value: u32| {
            let mut e = tag.to_le_bytes().to_vec();
            e.extend_from_slice(&typ.to_le_bytes());
            e.extend_from_slice(&count.to_le_bytes());
            e.extend_from_slice(&value.to_le_bytes());
            e
        };
        let mut tiff = b"II*\0\x08\0\0\0".to_vec();
        let n: u16 = if taken.is_some() { 2 } else { 1 };
        tiff.extend_from_slice(&n.to_le_bytes());
        tiff.extend(entry(0x0112, 3, 1, orientation as u32));
        if let Some(date) = taken {
            tiff.extend(entry(0x8769, 4, 1, 38));
            tiff.extend_from_slice(&[0; 4]);
            tiff.extend_from_slice(&1u16.to_le_bytes());
            tiff.extend(entry(0x9003, 2, 20, 56));
            tiff.extend_from_slice(&[0; 4]);
            let mut text = date.as_bytes().to_vec();
            text.resize(20, 0);
            tiff.extend_from_slice(&text);
        } else {
            tiff.extend_from_slice(&[0; 4]);
        }
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    /// Fujifilm RAF files don't start with a TIFF header; the rotation of a
    /// portrait shot is only in the embedded JPEG.
    #[test]
    fn raf_style_portrait_preview_is_turned_upright() {
        let dir = temp_dir("raf");
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(160, 0);
        raf.extend_from_slice(&jpeg_with_orientation(400, 300, 6));
        raf.extend_from_slice(&[0u8; 64]);
        let path = dir.join("DSCF0001.RAF");
        std::fs::write(&path, &raf).unwrap();
        let l = load(&path, None).unwrap();
        assert_eq!((l.full_w, l.full_h), (300, 400), "rotated to portrait");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The multi-core shrink keeps the shape and averages colours like the
    /// single-core one it replaces.
    #[test]
    fn shrink_keeps_shape_and_colours() {
        let mut img = image::RgbImage::new(7728, 5152);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 3864 {
                image::Rgb([200, 40, 40])
            } else {
                image::Rgb([20, 60, 220])
            };
        }
        let fast = shrink(DynamicImage::ImageRgb8(img.clone()), 3840).into_rgb8();
        let slow = DynamicImage::ImageRgb8(img)
            .thumbnail(3840, 3840)
            .into_rgb8();
        assert_eq!(fast.dimensions(), (3840, 2560));
        assert_eq!(fast.dimensions(), slow.dimensions());
        assert_eq!(fast.get_pixel(100, 100).0, [200, 40, 40]);
        assert_eq!(fast.get_pixel(3700, 2000).0, [20, 60, 220]);
        // Smaller than the limit: untouched.
        let small = DynamicImage::ImageRgb8(image::RgbImage::new(800, 600));
        let s = shrink(small, 3840);
        assert_eq!((s.width(), s.height()), (800, 600));
    }

    /// On real photos (GLANCE_TEST_PHOTOS), the fast load gives the same size
    /// and orientation as decoding, turning and shrinking with the image
    /// crate, and its pixels are at least as close to a high-quality
    /// (Lanczos) resize as the image crate's own thumbnail.
    #[test]
    fn fast_load_matches_reference_on_real_photos() {
        let Some(dir) = crate::tests::test_photos() else {
            return;
        };
        // (A folder with only RAW or HEIF files has nothing to compare.)
        for p in list_dir(&dir)
            .into_iter()
            .filter(|p| !is_raw(p) && !is_heif(p))
            .take(4)
        {
            let fast = load(&p, Some(1024)).unwrap();
            let mut dec = image::ImageReader::open(&p)
                .unwrap()
                .with_guessed_format()
                .unwrap()
                .into_decoder()
                .unwrap();
            let o = dec.orientation().unwrap();
            let mut full = DynamicImage::from_decoder(dec).unwrap();
            full.apply_orientation(o);
            let thumb = full.thumbnail(1024, 1024).into_rgba8();
            assert_eq!(
                (fast.full_w, fast.full_h),
                (full.width(), full.height()),
                "{p:?}"
            );
            assert_eq!(fast.img.dimensions(), thumb.dimensions(), "{p:?}");
            let (w, h) = thumb.dimensions();
            let best = image::imageops::resize(
                &full.to_rgba8(),
                w,
                h,
                image::imageops::FilterType::Lanczos3,
            );
            let mse = |a: &RgbaImage| {
                a.as_raw()
                    .iter()
                    .zip(best.as_raw())
                    .map(|(x, y)| (*x as f64 - *y as f64).powi(2))
                    .sum::<f64>()
                    / best.as_raw().len() as f64
            };
            let (new, old) = (mse(&fast.img), mse(&thumb));
            assert!(new <= old * 1.05, "{p:?}: {new:.1} vs {old:.1}");
        }
    }

    /// A preview whose end marker is the last two bytes of the file is found.
    #[test]
    fn jpeg_at_the_very_end_is_found() {
        let jpeg = jpeg_with_orientation(400, 300, 1);
        let mut data = vec![0u8; 100];
        data.extend_from_slice(&jpeg);
        assert_eq!(embedded_jpegs(&data), vec![&jpeg[..]]);
    }

    #[test]
    fn copies_get_free_names() {
        let dir = temp_dir("copyname");
        let src = dir.join("photo.jpg");
        std::fs::write(&src, b"x").unwrap();
        assert_eq!(copy_path(&src), dir.join("photo-edit.jpg"));
        std::fs::write(dir.join("photo-edit.jpg"), b"x").unwrap();
        assert_eq!(copy_path(&src), dir.join("photo-edit-2.jpg"));
        assert_eq!(copy_path(&dir.join("x.RAF")), dir.join("x-edit.jpg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrite_replaces_the_file_and_leaves_no_temp_file() {
        let dir = temp_dir("overwrite");
        let path = dir.join("a.png");
        RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        let blue = RgbaImage::from_pixel(4, 6, image::Rgba([0, 0, 255, 255]));
        write_image(&blue, &path, 90, None).unwrap();
        let back = image::open(&path).unwrap().into_rgba8();
        assert_eq!(back.dimensions(), (4, 6));
        assert_eq!(back.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temp file left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn backups_keep_the_first_original_of_each_photo() {
        let dir = temp_dir("backup");
        let path = dir.join("IMG_1.jpg");
        let originals = dir.join(".originals");
        let first = jpeg_with_exif(400, 300, 1, Some("2026:05:04 10:00:00"));
        std::fs::write(&path, &first).unwrap();
        assert_eq!(backup_original(&path).unwrap(), originals.join("IMG_1.jpg"));
        // The same photo after a save (new pixels, same capture time): the
        // first original stays the backup, nothing piles up.
        let edited = jpeg_with_exif(300, 200, 1, Some("2026:05:04 10:00:00"));
        std::fs::write(&path, &edited).unwrap();
        assert_eq!(backup_original(&path).unwrap(), originals.join("IMG_1.jpg"));
        assert_eq!(std::fs::read(originals.join("IMG_1.jpg")).unwrap(), first);
        // A different photo with the same name (counter reset, second card).
        let other = jpeg_with_exif(400, 300, 1, Some("2026:11:16 18:30:00"));
        std::fs::write(&path, &other).unwrap();
        assert_eq!(
            backup_original(&path).unwrap(),
            originals.join("IMG_1-2.jpg")
        );
        // …and saving that one again keeps its first original too.
        std::fs::write(
            &path,
            jpeg_with_exif(200, 100, 1, Some("2026:11:16 18:30:00")),
        )
        .unwrap();
        assert_eq!(
            backup_original(&path).unwrap(),
            originals.join("IMG_1-2.jpg")
        );
        assert_eq!(std::fs::read(originals.join("IMG_1.jpg")).unwrap(), first);
        assert_eq!(std::fs::read(originals.join("IMG_1-2.jpg")).unwrap(), other);
        assert_eq!(
            std::fs::read_dir(&originals).unwrap().count(),
            2,
            "no temp file left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With exiftool installed, the camera info and the colour profile survive a save.
    #[test]
    fn save_keeps_camera_info_and_colour_profile() {
        if !exiftool_available() {
            return;
        }
        let dir = temp_dir("meta");
        let src = dir.join("src.jpg");
        std::fs::write(&src, jpeg_with_orientation(400, 300, 1)).unwrap();
        // An Adobe RGB profile, if one is on this system (ghostscript ships one).
        let icc = Path::new("/usr/share/ghostscript/iccprofiles/a98.icc");
        let mut args = vec![
            "-q".to_string(),
            "-overwrite_original".into(),
            "-Model=Test Camera".into(),
        ];
        if icc.exists() {
            args.push(format!("-icc_profile<={}", icc.display()));
        }
        let ok = std::process::Command::new("exiftool")
            .args(&args)
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok);
        let tag = |p: &Path, t: &str| {
            let out = std::process::Command::new("exiftool")
                .args(["-s3", t])
                .arg(p)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let dst = dir.join("dst.jpg");
        let img = RgbaImage::from_pixel(40, 30, image::Rgba([1, 2, 3, 255]));
        assert!(write_image(&img, &dst, 90, Some(&src)).unwrap());
        assert_eq!(tag(&dst, "-Model"), "Test Camera");
        if icc.exists() {
            let profile = tag(&src, "-ProfileDescription");
            assert!(!profile.is_empty());
            assert_eq!(
                tag(&dst, "-ProfileDescription"),
                profile,
                "colour profile kept"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
