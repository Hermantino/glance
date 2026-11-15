//! Colours. glance has its own amber look; on Omarchy it follows the active
//! Omarchy theme instead, live, the way Omarchy's own apps do. Nothing here is
//! required: if the Omarchy theme files aren't there, glance stays amber.
use eframe::egui::{self, Color32, CornerRadius, Stroke};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

/// glance's own accent, used whenever no Omarchy theme is active.
pub const AMBER: Color32 = Color32::from_rgb(230, 180, 80);

static ACCENT: AtomicU32 = AtomicU32::new(pack(AMBER));
static IMAGE_BG: AtomicU32 = AtomicU32::new(pack(Color32::from_gray(16)));

const fn pack(c: Color32) -> u32 {
    u32::from_be_bytes([c.r(), c.g(), c.b(), 255])
}

fn unpack(v: u32) -> Color32 {
    let [r, g, b, _] = v.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

/// The current highlight colour (amber, or the Omarchy theme's accent).
pub fn accent() -> Color32 {
    unpack(ACCENT.load(Ordering::Relaxed))
}

/// Background behind photos: a darker shade of the panels.
pub fn image_bg() -> Color32 {
    unpack(IMAGE_BG.load(Ordering::Relaxed))
}

/// Colours taken from an Omarchy theme.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    pub accent: Color32,
    pub background: Color32,
    pub foreground: Color32,
    pub selection: Color32,
    /// Hyprland's window corner rounding, if known (Omarchy's default is 0).
    pub rounding: Option<u8>,
}

/// Where Omarchy keeps the active theme.
fn theme_dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("omarchy/current/theme"))
}

fn hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    let [_, r, g, b] = v.to_be_bytes();
    Some(Color32::from_rgb(r, g, b))
}

/// Parse an Omarchy `colors.toml`.
pub fn parse_colors(text: &str) -> Option<Palette> {
    let table: toml::Table = text.parse().ok()?;
    let get = |k: &str| table.get(k).and_then(|v| v.as_str()).and_then(hex);
    let background = get("background")?;
    let foreground = get("foreground")?;
    Some(Palette {
        accent: get("accent").or_else(|| get("color4")).unwrap_or(AMBER),
        background,
        foreground,
        selection: get("selection_background").unwrap_or(background),
        rounding: None,
    })
}

/// Hyprland's `decoration:rounding`, so corners match the windows.
fn hypr_rounding() -> Option<u8> {
    let out = std::process::Command::new("hyprctl")
        .args(["getoption", "decoration:rounding", "-j"])
        .output()
        .ok()?;
    parse_rounding(&String::from_utf8_lossy(&out.stdout))
}

/// Read `"int": N` from `hyprctl getoption … -j` output.
fn parse_rounding(json: &str) -> Option<u8> {
    let rest = &json[json.find("\"int\"")? + 5..];
    let digits: String = rest
        .trim_start_matches([':', ' '])
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok().map(|i| i.min(30) as u8)
}

/// The active Omarchy theme, or None when not running on Omarchy.
pub fn read_omarchy() -> Option<Palette> {
    let text = std::fs::read_to_string(theme_dir()?.join("colors.toml")).ok()?;
    let mut p = parse_colors(&text)?;
    p.rounding = hypr_rounding();
    Some(p)
}

/// A fingerprint of the theme files; it changes whenever the theme is switched.
fn stamp() -> Option<(SystemTime, Option<SystemTime>)> {
    let dir = theme_dir()?;
    let colors = std::fs::metadata(dir.join("colors.toml"))
        .ok()?
        .modified()
        .ok()?;
    let generation = dir
        .parent()
        .and_then(|d| std::fs::metadata(d.join("theme.generation")).ok())
        .and_then(|m| m.modified().ok());
    Some((colors, generation))
}

/// Check the theme files once a second on a background thread and call
/// `changed` with the new palette after a switch. Cheap: two `stat` calls.
pub fn watch(changed: impl Fn(Option<Palette>) + Send + 'static) {
    std::thread::spawn(move || {
        let mut last = stamp();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let now = stamp();
            if now != last {
                last = now;
                // A theme switch rewrites several files; let it settle.
                std::thread::sleep(Duration::from_millis(150));
                changed(read_omarchy());
            }
        }
    });
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Apply glance's own look (None) or an Omarchy palette.
pub fn apply(ctx: &egui::Context, palette: Option<Palette>) {
    let mut v = egui::Visuals::dark();
    match palette {
        None => {
            v.panel_fill = Color32::from_gray(24);
            ACCENT.store(pack(AMBER), Ordering::Relaxed);
            IMAGE_BG.store(pack(Color32::from_gray(16)), Ordering::Relaxed);
        }
        Some(p) => {
            let (bg, fg) = (p.background, p.foreground);
            v.panel_fill = bg;
            v.window_fill = mix(bg, fg, 0.06);
            v.extreme_bg_color = mix(bg, Color32::BLACK, 0.35);
            v.faint_bg_color = mix(bg, fg, 0.04);
            v.code_bg_color = mix(bg, fg, 0.08);
            v.hyperlink_color = p.accent;
            v.selection.bg_fill = mix(bg, p.accent, 0.55);
            v.selection.stroke = Stroke::new(1.0, fg);
            v.window_stroke = Stroke::new(1.0, mix(bg, fg, 0.15));
            let w = &mut v.widgets;
            w.noninteractive.bg_fill = bg;
            w.noninteractive.weak_bg_fill = bg;
            w.noninteractive.bg_stroke = Stroke::new(1.0, mix(bg, fg, 0.12));
            w.noninteractive.fg_stroke = Stroke::new(1.0, mix(bg, fg, 0.8));
            for (wv, k) in [
                (&mut w.inactive, 0.10),
                (&mut w.hovered, 0.18),
                (&mut w.active, 0.26),
                (&mut w.open, 0.14),
            ] {
                wv.bg_fill = mix(bg, fg, k);
                wv.weak_bg_fill = mix(bg, fg, k);
                wv.fg_stroke = Stroke::new(1.0, fg);
            }
            w.hovered.bg_stroke = Stroke::new(1.0, mix(bg, p.accent, 0.6));
            w.active.bg_stroke = Stroke::new(1.0, p.accent);
            if let Some(r) = p.rounding {
                let small = CornerRadius::same(r.min(4));
                for wv in [
                    &mut w.noninteractive,
                    &mut w.inactive,
                    &mut w.hovered,
                    &mut w.active,
                    &mut w.open,
                ] {
                    wv.corner_radius = small;
                }
                v.window_corner_radius = CornerRadius::same(r);
                v.menu_corner_radius = small;
            }
            ACCENT.store(pack(p.accent), Ordering::Relaxed);
            IMAGE_BG.store(pack(mix(bg, Color32::BLACK, 0.3)), Ordering::Relaxed);
        }
    }
    ctx.set_visuals(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omarchy_colors() {
        let p = parse_colors(
            "accent = \"#a277ff\"\nforeground = \"#edecee\"\nbackground = \"#15141b\"\nselection_background = \"#29263c\"\ncolor4 = \"#000000\"\n",
        )
        .unwrap();
        assert_eq!(p.accent, Color32::from_rgb(0xa2, 0x77, 0xff));
        assert_eq!(p.background, Color32::from_rgb(0x15, 0x14, 0x1b));
        assert_eq!(p.selection, Color32::from_rgb(0x29, 0x26, 0x3c));
    }

    #[test]
    fn missing_or_broken_theme_means_no_palette() {
        assert!(parse_colors("").is_none());
        assert!(parse_colors("background = \"nope\"\nforeground = \"#ffffff\"").is_none());
        // No accent: falls back to color4, then amber.
        let p = parse_colors("background = \"#000000\"\nforeground = \"#ffffff\"").unwrap();
        assert_eq!(p.accent, AMBER);
    }

    #[test]
    fn reads_hyprland_rounding() {
        assert_eq!(
            parse_rounding(r#"{"option": "decoration:rounding", "int": 0, "set": true }"#),
            Some(0)
        );
        assert_eq!(
            parse_rounding(r#"{"option": "decoration:rounding", "int": 10, "set": true }"#),
            Some(10)
        );
        assert_eq!(parse_rounding("error"), None);
    }
}
