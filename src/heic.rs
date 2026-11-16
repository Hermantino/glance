//! HEIC / HEIF / AVIF decoding through the system's libheif, loaded at runtime.
//!
//! glance doesn't link libheif: the download runs on any Linux, and HEIF photos
//! (e.g. Fujifilm `.HIF`, iPhone `.HEIC`) open wherever libheif is installed.
use image::RgbaImage;
use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::sync::OnceLock;

/// `struct heif_error`, returned by value from most libheif calls.
#[repr(C)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

// Values from libheif's headers (stable across versions).
const COLORSPACE_RGB: c_int = 1;
const CHROMA_INTERLEAVED_RGBA: c_int = 11;
const CHANNEL_INTERLEAVED: c_int = 10;

type Ptr = *mut c_void;

struct Api {
    _lib: Library,
    context_alloc: unsafe extern "C" fn() -> Ptr,
    context_free: unsafe extern "C" fn(Ptr),
    read_from_file: unsafe extern "C" fn(Ptr, *const c_char, *const c_void) -> HeifError,
    primary_handle: unsafe extern "C" fn(Ptr, *mut Ptr) -> HeifError,
    decode_image: unsafe extern "C" fn(Ptr, *mut Ptr, c_int, c_int, *const c_void) -> HeifError,
    plane_readonly: unsafe extern "C" fn(Ptr, c_int, *mut c_int) -> *const u8,
    image_width: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    image_height: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    image_release: unsafe extern "C" fn(Ptr),
    handle_release: unsafe extern "C" fn(Ptr),
}

// The function pointers are plain C functions; libheif is safe to call from
// several threads as long as each uses its own context, which we do.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib = ["libheif.so.1", "libheif.so"]
            .iter()
            .find_map(|n| Library::new(*n).ok())?;
        macro_rules! sym {
            ($name:literal) => {
                *lib.get(concat!($name, "\0").as_bytes()).ok()?
            };
        }
        Some(Api {
            context_alloc: sym!("heif_context_alloc"),
            context_free: sym!("heif_context_free"),
            read_from_file: sym!("heif_context_read_from_file"),
            primary_handle: sym!("heif_context_get_primary_image_handle"),
            decode_image: sym!("heif_decode_image"),
            plane_readonly: sym!("heif_image_get_plane_readonly"),
            image_width: sym!("heif_image_get_width"),
            image_height: sym!("heif_image_get_height"),
            image_release: sym!("heif_image_release"),
            handle_release: sym!("heif_image_handle_release"),
            _lib: lib,
        })
    })
    .as_ref()
}

/// Is libheif installed?
pub fn available() -> bool {
    api().is_some()
}

fn check(e: HeifError) -> Result<(), String> {
    if e.code == 0 {
        return Ok(());
    }
    let msg = if e.message.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(e.message) }
            .to_string_lossy()
            .into_owned()
    };
    Err(format!("libheif: {msg} (error {}/{})", e.code, e.subcode))
}

/// Decode the primary image of a HEIF file as 8-bit RGBA, rotated/mirrored as stored.
pub fn decode(path: &Path) -> Result<RgbaImage, String> {
    let api = api().ok_or(
        "HEIC/HEIF photos need libheif — install it with your package manager (e.g. libheif)",
    )?;
    let cpath = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    unsafe {
        let ctx = (api.context_alloc)();
        if ctx.is_null() {
            return Err("libheif: out of memory".into());
        }
        let mut handle: Ptr = std::ptr::null_mut();
        let mut img: Ptr = std::ptr::null_mut();
        let result = (|| {
            check((api.read_from_file)(ctx, cpath.as_ptr(), std::ptr::null()))?;
            check((api.primary_handle)(ctx, &mut handle))?;
            check((api.decode_image)(
                handle,
                &mut img,
                COLORSPACE_RGB,
                CHROMA_INTERLEAVED_RGBA,
                std::ptr::null(),
            ))?;
            let w = (api.image_width)(img, CHANNEL_INTERLEAVED);
            let h = (api.image_height)(img, CHANNEL_INTERLEAVED);
            let mut stride: c_int = 0;
            let data = (api.plane_readonly)(img, CHANNEL_INTERLEAVED, &mut stride);
            if data.is_null() || w <= 0 || h <= 0 || stride < w * 4 {
                return Err("libheif: no image data".to_string());
            }
            let (w, h, stride) = (w as usize, h as usize, stride as usize);
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                out.extend_from_slice(std::slice::from_raw_parts(data.add(y * stride), w * 4));
            }
            RgbaImage::from_raw(w as u32, h as u32, out)
                .ok_or_else(|| "libheif: bad image size".to_string())
        })();
        if !img.is_null() {
            (api.image_release)(img);
        }
        if !handle.is_null() {
            (api.handle_release)(handle);
        }
        (api.context_free)(ctx);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a small photo with heif-enc (8-bit and 10-bit, like a Fujifilm .HIF)
    /// and checks it decodes at the right size with sensible colours.
    #[test]
    fn decodes_heic_written_by_heif_enc() {
        if !available()
            || std::process::Command::new("heif-enc")
                .arg("--help")
                .output()
                .is_err()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("glance-heic-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("src.png");
        let mut img = RgbaImage::new(320, 200);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 160 {
                image::Rgba([220, 40, 40, 255])
            } else {
                image::Rgba([40, 60, 220, 255])
            };
        }
        img.save(&png).unwrap();
        let mut decoded = 0;
        // 8-bit HEIC (iPhone-like), 10-bit .hif (like a Fujifilm X100VI), and AVIF.
        let cases: [(&str, &[&str]); 3] = [
            ("a.heic", &[]),
            ("b.hif", &["-b", "10"]),
            ("c.avif", &["--avif"]),
        ];
        for (name, extra) in cases {
            let out = dir.join(name);
            let ok = std::process::Command::new("heif-enc")
                .args(["-q", "90"])
                .args(extra)
                .arg("-o")
                .arg(&out)
                .arg(&png)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !ok {
                // This system's heif-enc can't write this format (no encoder for it).
                eprintln!("skipping {name}: heif-enc couldn't encode it here");
                continue;
            }
            let d = crate::files::load(&out, None).expect("decodes");
            assert_eq!((d.full_w, d.full_h), (320, 200), "{name}");
            let left = d.img.get_pixel(40, 100).0;
            let right = d.img.get_pixel(280, 100).0;
            assert!(
                left[0] > 150 && left[2] < 110,
                "{name}: left half red, got {left:?}"
            );
            assert!(
                right[2] > 150 && right[0] < 110,
                "{name}: right half blue, got {right:?}"
            );
            decoded += 1;
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            decoded > 0,
            "heif-enc is installed but none of HEIC, 10-bit HIF or AVIF could be tested"
        );
    }
}
