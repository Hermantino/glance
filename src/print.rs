//! Printing through CUPS: printer/option discovery, page layout, and job submission.
//!
//! The page is composed here at the exact paper size and sent as a one-page PDF
//! with `print-scaling=none`, so CUPS never rescales (and never stretches) the photo.
use crate::edit::{self, EditParams};
use crate::files;
use image::{DynamicImage, RgbImage, RgbaImage, imageops};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DPI: f32 = 300.0;

#[derive(Clone, Debug)]
pub struct PpdOption {
    pub key: String,
    pub label: String,
    pub values: Vec<String>,
    pub default: usize,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// All CUPS destinations including saved presets ("Printer/Preset"), and the default one.
pub fn destinations() -> (Vec<String>, Option<String>) {
    let list: Vec<String> = run("lpstat", &["-e"])
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let default =
        run("lpstat", &["-d"]).and_then(|s| s.split(": ").nth(1).map(|d| d.trim().to_string()));
    (list, default)
}

/// Options of a destination, with the preset's saved values as defaults.
pub fn options(dest: &str) -> Vec<PpdOption> {
    let Some(s) = run("lpoptions", &["-p", dest, "-l"]) else {
        return vec![];
    };
    s.lines()
        .filter_map(|l| {
            let (head, vals) = l.split_once(": ")?;
            let (key, label) = head.split_once('/').unwrap_or((head, head));
            let mut default = 0;
            let values = vals
                .split_whitespace()
                .enumerate()
                .map(|(i, v)| match v.strip_prefix('*') {
                    Some(v) => {
                        default = i;
                        v.to_string()
                    }
                    None => v.to_string(),
                })
                .collect();
            Some(PpdOption {
                key: key.into(),
                label: label.into(),
                values,
                default,
            })
        })
        .collect()
}

/// One-line printer state, e.g. "idle" or "disabled".
pub fn state(dest: &str) -> String {
    let base = dest.split('/').next().unwrap_or(dest);
    run("lpstat", &["-p", base])
        .and_then(|s| s.lines().next().map(|l| l.to_string()))
        .map(|l| {
            let l = l
                .strip_prefix(&format!("printer {base} "))
                .unwrap_or(&l)
                .to_string();
            let l = l.strip_prefix("is ").unwrap_or(&l).to_string();
            l.split(".  ")
                .next()
                .unwrap_or(&l)
                .trim_end_matches('.')
                .to_string()
        })
        .unwrap_or_else(|| "unknown (is CUPS running?)".into())
}

/// Paper size in mm (portrait) for a PageSize keyword, and whether it is borderless.
pub fn paper_mm(key: &str) -> Option<(f32, f32, bool)> {
    let mut k = key.to_string();
    let mut borderless = false;
    for suf in [
        ".Borderless",
        ".FB",
        ".Fullbleed",
        "Borderless",
        ".FullBleed",
    ] {
        if let Some(s) = k.strip_suffix(suf) {
            k = s.to_string();
            borderless = true;
        }
    }
    if let Some((w, h)) = known_size(&k) {
        return Some((w, h, borderless));
    }
    // Epson: a leading T means borderless ("T4X6FULL").
    if let Some(rest) = k.strip_prefix('T')
        && let Some((w, h)) = known_size(rest)
    {
        return Some((w, h, true));
    }
    // Generic "w288h432" (PostScript points).
    let lower = k.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix('w')
        && let Some((w, h)) = rest.split_once('h')
    {
        let pt = |s: &str| {
            s.trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse::<f32>()
                .ok()
        };
        if let (Some(w), Some(h)) = (pt(w), pt(h)) {
            return Some((w / 72.0 * 25.4, h / 72.0 * 25.4, borderless));
        }
    }
    None
}

fn known_size(k: &str) -> Option<(f32, f32)> {
    let mm = match k.to_ascii_lowercase().as_str() {
        "a3" => (297.0, 420.0),
        "a4" => (210.0, 297.0),
        "a5" => (148.0, 210.0),
        "a6" => (105.0, 148.0),
        "b5" => (182.0, 257.0),
        "b6" => (128.0, 182.0),
        "letter" => (215.9, 279.4),
        "legal" => (215.9, 355.6),
        "4x6full" | "4x6" | "4x6in" | "photo4x6" | "10x15cm" | "w288h432" => (101.6, 152.4),
        "2l" | "5x7" | "5x7in" | "photo5x7" | "13x18cm" => (127.0, 177.8),
        "l" | "3.5x5" | "3.5x5in" | "photol" | "9x13cm" => (88.9, 127.0),
        "8x10" | "8x10in" => (203.2, 254.0),
        "4x7" => (101.6, 177.8),
        "index5" | "5x8" | "5x8in" => (127.0, 203.2),
        "postcard" | "hagaki" => (100.0, 148.0),
        "16k" => (195.0, 270.0),
        "env10" => (104.8, 241.3),
        "envdl" => (110.0, 220.0),
        "envc6" => (114.0, 162.0),
        _ => return None,
    };
    Some(mm)
}

/// Human name for an option value.
pub fn value_label(key: &str, v: &str) -> String {
    let named = match (key, v) {
        ("PageSize", _) => {
            return match paper_mm(v) {
                Some((w, h, b)) => {
                    let base = match v.trim_start_matches('T') {
                        "4X6FULL" => "10×15 cm (4×6\")".to_string(),
                        "2L" => "13×18 cm (5×7\")".to_string(),
                        "L" => "9×13 cm (3.5×5\")".to_string(),
                        "8x10" => "20×25 cm (8×10\")".to_string(),
                        "4X7" => "10×18 cm (4×7\")".to_string(),
                        _ => format!("{v} ({:.0}×{:.0} mm)", w, h)
                            .replace("T", if b { "" } else { "T" }),
                    };
                    if b {
                        format!("{base} – borderless")
                    } else {
                        base
                    }
                }
                None => v.to_string(),
            };
        }
        ("InputSlot", "Auto") => "Automatic",
        ("InputSlot", "Cassette1") => "Cassette 1",
        ("InputSlot", "Cassette2") => "Cassette 2",
        ("InputSlot", "RearPaperFeed") => "Rear paper feed",
        ("MediaType", "PLAIN_HIGH") => "Plain paper – high",
        ("MediaType", "PLAIN_NORMAL") => "Plain paper – standard",
        ("MediaType", "PLAIN_DRAFT") => "Plain paper – draft",
        ("MediaType", "PMPHOTO_HIGH") => "Epson Premium Glossy – high",
        ("MediaType", "PMPHOTO_NORMAL") => "Epson Premium Glossy – standard",
        ("MediaType", "PSGLOS_HIGH") => "Epson Premium Semigloss – high",
        ("MediaType", "PSGLOS_NORMAL") => "Epson Premium Semigloss – standard",
        ("MediaType", "PLATINA_HIGH") => "Epson Ultra Glossy – max quality",
        ("MediaType", "PLATINA_NORMAL") => "Epson Ultra Glossy – high",
        ("MediaType", "PLATINA_DRAFT") => "Epson Ultra Glossy – standard",
        ("MediaType", "PMMATT_HIGH") => "Epson Matte – high",
        ("MediaType", "PMMATT_NORMAL") => "Epson Matte – standard",
        ("MediaType", "VELVET_FINE_HIGH") => "Epson Velvet Fine Art",
        ("MediaType", "LCPP_HIGH") => "Epson Photo Paper Glossy – high",
        ("MediaType", "LCPP_NORMAL") => "Epson Photo Paper Glossy – standard",
        ("MediaType", "SFINE_HIGH") => "Epson Bright White / Super Fine – high",
        ("MediaType", "SFINE_NORMAL") => "Epson Bright White / Super Fine – standard",
        ("MediaType", "MINIPHOTO_HIGH") => "Photo stickers",
        ("MediaType", "THICK1_HIGH") => "Thick paper 1",
        ("MediaType", "THICK2_HIGH") => "Thick paper 2",
        ("MediaType", "ENV_HIGH") => "Envelope – high",
        ("MediaType", "ENV_NORMAL") => "Envelope – standard",
        ("MediaType", "LETTERHEAD_HIGH") => "Letterhead – high",
        ("MediaType", "LETTERHEAD_NORMAL") => "Letterhead – standard",
        ("MediaType", "LETTERHEAD_DRAFT") => "Letterhead – draft",
        _ => return v.to_string(),
    };
    named.to_string()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scale {
    /// Whole photo visible; white bands where the shapes differ.
    Fit,
    /// Fill the printable area; trims the photo where the shapes differ.
    Fill,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Orient {
    Auto,
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, Debug)]
pub struct Sheet {
    /// Paper size in mm, portrait.
    pub paper: (f32, f32),
    /// Margin in mm on every side (0 for borderless).
    pub margin: f32,
    pub scale: Scale,
    pub orient: Orient,
}

/// The printable area of the (portrait) sheet, in mm: [x, y, w, h].
pub fn area(s: &Sheet) -> [f32; 4] {
    let (pw, ph) = s.paper;
    let m = s.margin.clamp(0.0, pw.min(ph) / 3.0);
    [m, m, pw - 2.0 * m, ph - 2.0 * m]
}

/// How a photo sits in its slot.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// The photo is turned 90° clockwise to match the slot.
    pub rotate: bool,
    /// Photo rectangle on the sheet in mm.
    pub dest: [f32; 4],
    /// Part of the (turned) photo used, normalised [x, y, w, h].
    pub src: [f32; 4],
}

pub fn place_in(
    photo_w: f32,
    photo_h: f32,
    cell: [f32; 4],
    scale: Scale,
    orient: Orient,
) -> Placed {
    let [cx, cy, cw, ch] = cell;
    let rotate = match orient {
        Orient::Auto => (cw - ch).abs() > 0.5 && (photo_w > photo_h) != (cw > ch),
        Orient::Portrait => false,
        Orient::Landscape => true,
    };
    let (w, h) = if rotate {
        (photo_h, photo_w)
    } else {
        (photo_w, photo_h)
    };
    let (pa, ca) = (w / h, cw / ch);
    match scale {
        Scale::Fit => {
            let (dw, dh) = if pa > ca {
                (cw, cw / pa)
            } else {
                (ch * pa, ch)
            };
            Placed {
                rotate,
                dest: [cx + (cw - dw) / 2.0, cy + (ch - dh) / 2.0, dw, dh],
                src: [0.0, 0.0, 1.0, 1.0],
            }
        }
        Scale::Fill => {
            let src = if pa > ca {
                let f = ca / pa;
                [(1.0 - f) / 2.0, 0.0, f, 1.0]
            } else {
                let f = pa / ca;
                [0.0, (1.0 - f) / 2.0, 1.0, f]
            };
            Placed {
                rotate,
                dest: cell,
                src,
            }
        }
    }
}

pub struct Job {
    pub items: Vec<(PathBuf, EditParams)>,
    pub sheet: Sheet,
    pub dest: String,
    pub options: Vec<(String, String)>,
    pub copies: u32,
}

/// Render every sheet at full quality and send them to CUPS as one job.
pub fn run_job(job: &Job, progress: impl Fn(String)) -> Result<String, String> {
    let (pdf, n) = render_pdf(job, &progress)?;
    progress("Sending to the printer…".into());
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let tmp = dir.join(format!("glance-print-{}.pdf", std::process::id()));
    std::fs::write(&tmp, pdf).map_err(|e| e.to_string())?;
    let title = match job.items.as_slice() {
        [(p, _)] => files::file_name(p),
        items => format!("{} photos", items.len()),
    };
    let res = submit(&job.dest, &job.options, job.copies, &title, &tmp);
    let _ = std::fs::remove_file(&tmp);
    res.map(|m| format!("{n} sheet{} — {m}", if n == 1 { "" } else { "s" }))
}

/// All sheets of a job as a PDF, and the number of sheets.
pub fn render_pdf(job: &Job, progress: &impl Fn(String)) -> Result<(Vec<u8>, usize), String> {
    let cell = area(&job.sheet);
    let px = |mm: f32| (mm / 25.4 * DPI).round().max(1.0) as u32;
    let (pw, ph) = (px(job.sheet.paper.0), px(job.sheet.paper.1));
    let n = job.items.len();
    let mut sheets = Vec::with_capacity(n);
    for (i, (path, params)) in job.items.iter().enumerate() {
        progress(format!("Preparing sheet {} of {n}…", i + 1));
        let full =
            files::load(path, None).map_err(|e| format!("{}: {e}", files::file_name(path)))?;
        let photo = edit::render(&full.img, params);
        let mut canvas = RgbImage::from_pixel(pw, ph, image::Rgb([255, 255, 255]));
        draw_into(&mut canvas, &photo, cell, job.sheet.scale, job.sheet.orient);
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&canvas)
            .map_err(|e| e.to_string())?;
        sheets.push((jpeg, pw, ph));
    }
    let pt = |mm: f32| mm / 25.4 * 72.0;
    Ok((
        write_pdf(&sheets, pt(job.sheet.paper.0), pt(job.sheet.paper.1)),
        n,
    ))
}

/// Draw `photo` into `cell` (mm) on a sheet rendered at `DPI`.
pub fn draw_into(
    canvas: &mut RgbImage,
    photo: &RgbaImage,
    cell: [f32; 4],
    scale: Scale,
    orient: Orient,
) {
    let pl = place_in(
        photo.width() as f32,
        photo.height() as f32,
        cell,
        scale,
        orient,
    );
    let rotated;
    let img: &RgbaImage = if pl.rotate {
        rotated = imageops::rotate90(photo);
        &rotated
    } else {
        photo
    };
    let (sw, sh) = (img.width() as f32, img.height() as f32);
    let [sx, sy, sw_n, sh_n] = pl.src;
    let crop = imageops::crop_imm(
        img,
        (sx * sw).round() as u32,
        (sy * sh).round() as u32,
        ((sw_n * sw).round() as u32).max(1),
        ((sh_n * sh).round() as u32).max(1),
    )
    .to_image();
    let rgb = DynamicImage::ImageRgba8(crop).into_rgb8();
    let px = |mm: f32| (mm / 25.4 * DPI).round().max(1.0) as u32;
    let resized = imageops::resize(
        &rgb,
        px(pl.dest[2]),
        px(pl.dest[3]),
        imageops::FilterType::CatmullRom,
    );
    imageops::overlay(
        canvas,
        &resized,
        px(pl.dest[0]) as i64,
        px(pl.dest[1]) as i64,
    );
}

/// Ink levels reported by the printer: (name, percent, colour as RGB).
pub fn ink_levels(dest: &str) -> Vec<(String, i32, [u8; 3])> {
    let base = dest.split('/').next().unwrap_or(dest);
    let Some(out) = run(
        "ipptool",
        &[
            "-tv",
            &format!("ipp://localhost/printers/{base}"),
            "get-printer-attributes.test",
        ],
    ) else {
        return vec![];
    };
    let attr = |name: &str| -> Vec<String> {
        out.lines()
            .find(|l| l.trim_start().starts_with(&format!("{name} ")))
            .and_then(|l| l.split_once("= "))
            .map(|(_, v)| v.split(',').map(|x| x.trim().to_string()).collect())
            .unwrap_or_default()
    };
    let (names, levels, colors) = (
        attr("marker-names"),
        attr("marker-levels"),
        attr("marker-colors"),
    );
    names
        .iter()
        .zip(&levels)
        .enumerate()
        .filter_map(|(i, (n, l))| {
            let level = l.parse().ok()?;
            let hex = colors
                .get(i)
                .map(|c| c.trim_start_matches('#'))
                .unwrap_or("888888");
            let c = |k: usize| {
                u8::from_str_radix(hex.get(k..k + 2).unwrap_or("88"), 16).unwrap_or(0x88)
            };
            let label = match n.as_str() {
                "MB" => "Matte black",
                "PB" => "Photo black",
                "C" => "Cyan",
                "M" => "Magenta",
                "Y" => "Yellow",
                "GY" => "Gray",
                "K" | "BK" => "Black",
                other => other,
            };
            Some((label.to_string(), level, [c(0), c(2), c(4)]))
        })
        .collect()
}

fn submit(
    dest: &str,
    options: &[(String, String)],
    copies: u32,
    title: &str,
    file: &Path,
) -> Result<String, String> {
    let mut cmd = Command::new("lp");
    cmd.env("LC_ALL", "C")
        .args(["-d", dest, "-n", &copies.max(1).to_string(), "-t", title]);
    for (k, v) in options {
        cmd.args(["-o", &format!("{k}={v}")]);
    }
    cmd.args([
        "-o",
        "print-scaling=none",
        "-o",
        "nopdfAutoRotate",
        "-o",
        "fit-to-page=false",
    ])
    .arg(file);
    let out = cmd.output().map_err(|e| format!("could not run lp: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// A minimal PDF with one full-page JPEG per sheet.
fn write_pdf(sheets: &[(Vec<u8>, u32, u32)], page_w: f32, page_h: f32) -> Vec<u8> {
    let mut pdf: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let obj = |pdf: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    };
    let n = sheets.len();
    // Objects: 1 catalog, 2 pages, then per sheet k: page 3+3k, image 4+3k, contents 5+3k.
    obj(&mut pdf, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..n).map(|k| format!("{} 0 R", 3 + 3 * k)).collect();
    obj(
        &mut pdf,
        &mut offsets,
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).as_bytes(),
    );
    for (k, (jpeg, w, h)) in sheets.iter().enumerate() {
        let (img, content) = (4 + 3 * k, 5 + 3 * k);
        obj(
            &mut pdf,
            &mut offsets,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {page_w:.3} {page_h:.3}] \
                 /Resources << /XObject << /Im0 {img} 0 R >> >> /Contents {content} 0 R >>"
            )
            .as_bytes(),
        );
        let mut body = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        body.extend_from_slice(jpeg);
        body.extend_from_slice(b"\nendstream");
        obj(&mut pdf, &mut offsets, &body);
        let c = format!("q {page_w:.3} 0 0 {page_h:.3} 0 0 cm /Im0 Do Q");
        obj(
            &mut pdf,
            &mut offsets,
            format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len()).as_bytes(),
        );
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1).as_bytes(),
    );
    for o in &offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a real 3-photo job as a PDF (nothing is printed).
    #[test]
    fn queue_job_pdf() {
        let Some(dir) = crate::tests::test_photos() else {
            return;
        };
        let items: Vec<_> = files::list_dir(&dir)
            .into_iter()
            .filter(|p| !files::is_raw(p))
            .take(3)
            .map(|p| (p, EditParams::default()))
            .collect();
        if items.len() < 3 {
            return;
        }
        let job = Job {
            items,
            sheet: Sheet {
                paper: (101.6, 152.4),
                margin: 3.0,
                scale: Scale::Fill,
                orient: Orient::Auto,
            },
            dest: String::new(),
            options: vec![],
            copies: 1,
        };
        let (pdf, n) = render_pdf(&job, &|_| {}).unwrap();
        assert_eq!(n, 3);
        let out = std::env::var("GLANCE_TEST_PDF").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("glance-test.pdf")
                .display()
                .to_string()
        });
        std::fs::write(&out, &pdf).unwrap();
        // Every xref offset must point at its object.
        let text = String::from_utf8_lossy(&pdf);
        let xref = text.rfind("xref\n").unwrap();
        for (i, line) in text[xref..]
            .lines()
            .skip(3)
            .take_while(|l| l.ends_with(" n "))
            .enumerate()
        {
            let off: usize = line[..10].parse().unwrap();
            assert!(
                text[off..].starts_with(&format!("{} 0 obj", i + 1)),
                "object {} offset",
                i + 1
            );
        }
    }
}
