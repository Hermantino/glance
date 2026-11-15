//! Photo details for the info panel: file facts plus EXIF (camera, lens, exposure, date, GPS).
use crate::files;
use exif::{In, Tag, Value};
use std::io::Read;
use std::path::Path;

pub struct Info {
    /// (section title, [(label, value)])
    pub sections: Vec<(&'static str, Vec<(&'static str, String)>)>,
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

pub fn read(path: &Path, full: (u32, u32), size: u64) -> Info {
    let mut file = vec![("Name", files::file_name(path))];
    if let Some(dir) = path.parent() {
        file.push(("Folder", files::display_path(dir)));
    }
    let mp = full.0 as f64 * full.1 as f64 / 1e6;
    file.push((
        "Dimensions",
        format!("{} × {} px  ({mp:.1} MP)", full.0, full.1),
    ));
    file.push(("File size", human_size(size)));
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
        .unwrap_or_default();
    file.push((
        "Format",
        if files::is_raw(path) {
            format!("{ext} (RAW, showing embedded preview)")
        } else {
            ext
        },
    ));

    let mut sections = vec![("File", file)];
    if let Some(ex) = read_exif(path) {
        let get = |t: Tag| -> Option<String> {
            let f = ex.get_field(t, In::PRIMARY)?;
            // Text fields can hold several NUL-padded strings; keep the first real one.
            if let Value::Ascii(v) = &f.value {
                let s = v
                    .iter()
                    .map(|b| {
                        String::from_utf8_lossy(b)
                            .trim_matches(char::from(0))
                            .trim()
                            .to_string()
                    })
                    .find(|s| !s.is_empty())?;
                return Some(s);
            }
            let s = f.display_value().with_unit(&ex).to_string();
            let s = s.trim().trim_matches('"').trim().to_string();
            (!s.is_empty() && s != "unknown").then_some(s)
        };
        let mut cam = Vec::new();
        let mut make = get(Tag::Make).unwrap_or_default();
        for suffix in [
            " IMAGING CORP.",
            " CORPORATION",
            " Corporation",
            " CORP.",
            " Co., Ltd.",
            " COMPANY, LIMITED",
        ] {
            if let Some(m) = make.strip_suffix(suffix) {
                make = m.to_string();
            }
        }
        let model = get(Tag::Model).unwrap_or_default();
        let camera = if model.to_lowercase().starts_with(&make.to_lowercase()) || make.is_empty() {
            model
        } else {
            format!("{make} {model}")
        };
        if !camera.is_empty() {
            cam.push(("Camera", camera));
        }
        if let Some(l) = get(Tag::LensModel) {
            cam.push(("Lens", l));
        }
        if let Some(fl) = get(Tag::FocalLength) {
            let eq = get(Tag::FocalLengthIn35mmFilm)
                .map(|e| format!("  (35 mm equiv. {e})"))
                .unwrap_or_default();
            cam.push(("Focal length", format!("{fl}{eq}")));
        }
        if let Some(f) = get(Tag::FNumber) {
            cam.push(("Aperture", f));
        }
        if let Some(t) = get(Tag::ExposureTime) {
            cam.push(("Shutter", t));
        }
        if let Some(i) = get(Tag::PhotographicSensitivity) {
            cam.push(("ISO", i));
        }
        if let Some(b) = get(Tag::ExposureBiasValue) {
            cam.push(("Exposure comp.", b));
        }
        if let Some(f) = get(Tag::Flash) {
            cam.push((
                "Flash",
                if f.starts_with("fired") {
                    "Fired".into()
                } else {
                    "Not fired".into()
                },
            ));
        }
        if let Some(d) = get(Tag::DateTimeOriginal).or_else(|| get(Tag::DateTime)) {
            cam.push(("Taken", pretty_date(&d)));
        }
        if let Some(gps) = gps(&ex) {
            cam.push(("Location", gps));
        }
        if !cam.is_empty() {
            sections.push(("Camera", cam));
        }
    }
    Info { sections }
}

/// "2024:07:11 14:03:22" → "2024-07-11 14:03"
fn pretty_date(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 16 && s.as_bytes()[4] == b':' {
        format!("{} {}", s[..10].replace(':', "-"), &s[11..16])
    } else {
        s.to_string()
    }
}

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let reader = exif::Reader::new();
    if files::is_raw(path) {
        // Most RAW formats are TIFF inside, some with their own magic number
        // (Olympus ORF uses "IIRO"/"IIRS"). Patch it to plain TIFF and parse.
        let mut data = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(16 << 20)
            .read_to_end(&mut data)
            .ok()?;
        if data.len() > 8 && &data[0..2] == b"II" {
            data[2] = 42;
            data[3] = 0;
        } else if data.len() > 8 && &data[0..2] == b"MM" {
            data[2] = 0;
            data[3] = 42;
        }
        reader.read_raw(data).ok()
    } else {
        let f = std::fs::File::open(path).ok()?;
        reader
            .read_from_container(&mut std::io::BufReader::new(f))
            .ok()
    }
}

fn gps(ex: &exif::Exif) -> Option<String> {
    let coord = |tag: Tag, rtag: Tag| -> Option<f64> {
        let f = ex.get_field(tag, In::PRIMARY)?;
        let Value::Rational(v) = &f.value else {
            return None;
        };
        if v.len() < 3 {
            return None;
        }
        let deg = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let r = ex.get_field(rtag, In::PRIMARY)?.display_value().to_string();
        Some(if r.contains('S') || r.contains('W') {
            -deg
        } else {
            deg
        })
    };
    let lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef)?;
    let lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef)?;
    Some(format!("{lat:.5}, {lon:.5}"))
}
