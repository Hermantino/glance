//! glance — a fast photo viewer: browse a folder, make quick fixes, print.
mod config;
mod edit;
mod files;
mod heic;
mod info;
mod logo;
mod print;
mod raw;
mod theme;

use config::{Config, SaveMode};
use edit::{EditParams, Geo};
use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2,
};
use image::RgbaImage;
use print::{Orient, PpdOption, Scale, Sheet};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Longest side of the images kept for on-screen display.
const DISPLAY_MAX: u32 = 3840;
/// Longest side of the image used for live previews while editing.
const EDIT_MAX: u32 = 2048;
/// Images kept loaded on each side of the current one.
const KEEP: usize = 2;

const TEX: TextureOptions = TextureOptions {
    magnification: egui::TextureFilter::Linear,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(egui::TextureFilter::Linear),
};

// Phosphor icon font (MIT, assets/Phosphor-LICENSE-MIT.txt), used as a fallback font.
const ICON_FONT: &[u8] = include_bytes!("../assets/Phosphor.ttf");
const ICON_PREV: &str = "\u{E138}";
const ICON_NEXT: &str = "\u{E13A}";
const ICON_EDIT: &str = "\u{E3B4}";
const ICON_PRINT: &str = "\u{E3DC}";
const ICON_GEAR: &str = "\u{E272}";
const ICON_CHECK: &str = "\u{E182}";
const ICON_SAVE: &str = "\u{E248}";
const ICON_CW: &str = "\u{E036}";
const ICON_CCW: &str = "\u{E038}";
const ICON_ROTATE: &str = "\u{E036}";
const ICON_INFO: &str = "\u{E2CE}";
const ICON_CLOSE: &str = "\u{E4F6}";
const ICON_QUEUE: &str = "\u{E466}";
const ICON_IN_QUEUE: &str = "\u{E184}";
const ICON_TRASH: &str = "\u{E4A6}";
const ICON_DROP: &str = "\u{E210}";
const ICON_EYE: &str = "\u{E220}";
const ICON_UNDO: &str = "\u{E08A}";
const ICON_REDO: &str = "\u{E08C}";

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "phosphor".into(),
        Arc::new(egui::FontData::from_static(ICON_FONT)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("phosphor".into());
    }
    ctx.set_fonts(fonts);
}

const RATIOS: &[(&str, f32)] = &[
    ("Free", 0.0),
    ("Original", -1.0),
    ("1:1 square", 1.0),
    ("2:3 · 10×15 cm / 4×6\"", 2.0 / 3.0),
    ("3:4", 3.0 / 4.0),
    ("4:5 · 8×10\"", 4.0 / 5.0),
    ("5:7 · 13×18 cm / 5×7\"", 5.0 / 7.0),
    ("A4 / A-series", 0.70710677),
    ("9:16", 9.0 / 16.0),
];

struct Entry {
    tex: TextureHandle,
    img: Arc<RgbaImage>,
    full: (u32, u32),
    file_size: u64,
}

enum Slot {
    Loading,
    Ready(Arc<Entry>),
    Failed(String),
}

enum Msg {
    Loaded(PathBuf, Result<Entry, String>),
    Edited(PathBuf, EditParams, TextureHandle),
    Saved(PathBuf, Result<PathBuf, String>),
    Printed(Result<String, String>),
    PrintProgress(String),
    FullRes(PathBuf, Result<TextureHandle, String>),
    /// The Omarchy theme was switched (None: no longer on an Omarchy theme).
    Theme(Option<theme::Palette>),
    Ink(String, Vec<(String, i32, [u8; 3])>),
    /// CUPS destinations and the default one (asked once, in the background).
    Printers(Vec<String>, Option<String>),
    /// Options and status of one destination.
    PrinterOptions(String, Vec<PpdOption>, String),
    Thumb(PathBuf, Result<(RgbaImage, (u32, u32)), String>),
}

/// Small preview of a photo (for the print queue and print preview of photos not loaded).
struct Thumb {
    img: RgbaImage,
    full: (u32, u32),
    tex: TextureHandle,
    /// Edits the texture currently shows.
    shown: EditParams,
}

#[derive(Clone, Copy, PartialEq)]
enum Handle {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
    /// Drag outside the crop (or on a corner knob) to rotate.
    Rotate,
}

/// Everything undo/redo restores in the editor.
#[derive(Clone, Copy, PartialEq, Default)]
struct Snap {
    params: EditParams,
    crop: [f32; 4],
    ratio: usize,
    ratio_portrait: bool,
    crop_auto: bool,
}

struct EditState {
    path: PathBuf,
    params: EditParams,
    base: RgbaImage,
    tex: TextureHandle,
    tone_shown: (f32, f32),
    /// Crop in canvas pixels of `base`.
    crop: [f32; 4],
    crop_auto: bool,
    ratio: usize,
    ratio_portrait: bool,
    /// Handle, crop at drag start, pointer at drag start (canvas), angle at drag start.
    drag: Option<(Handle, [f32; 4], Pos2, f32)>,
    /// Full-resolution image size, before quarter turns.
    full: (f32, f32),
    /// The unedited photo, for the before/after comparison.
    orig_tex: TextureHandle,
    /// Showing the original (held \\ key or compare button).
    before: bool,
    /// Before/after forced on by the demo tour.
    hold_before: bool,
    history: edit::History<Snap>,
}

/// A photo in the print preview: path, edited size in pixels (if known), texture (if loaded).
type PreviewItem = (PathBuf, Option<(f32, f32)>, Option<TextureHandle>);

struct PrintUi {
    /// Photos to print, in order.
    items: Vec<PathBuf>,
    dests: Vec<String>,
    dest: usize,
    opts: Vec<PpdOption>,
    sel: Vec<usize>,
    state: String,
    /// None while the printer is being asked.
    ink: Option<Vec<(String, i32, [u8; 3])>>,
    /// Waiting for CUPS to list printers or this printer's options.
    loading: bool,
    scale: Scale,
    orient: Orient,
    margin: f32,
    copies: u32,
    /// Sheet shown in the preview.
    page: usize,
    busy: bool,
    progress: Option<String>,
    error: Option<String>,
}

struct App {
    ctx: egui::Context,
    config: Config,
    dir: PathBuf,
    files: Vec<PathBuf>,
    idx: usize,
    cache: HashMap<PathBuf, Slot>,
    edits: HashMap<PathBuf, EditParams>,
    edited_tex: HashMap<PathBuf, (EditParams, TextureHandle)>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    zoom: f32,
    pan: Vec2,
    editing: Option<EditState>,
    printing: Option<PrintUi>,
    prefs_open: bool,
    raw_prompt: Option<PathBuf>,
    raw_remember: bool,
    status: Option<(String, Instant)>,
    busy_saving: bool,
    title: String,
    #[cfg(feature = "demo")]
    demo: (Instant, usize),
    logo: TextureHandle,
    info_open: bool,
    info_cache: HashMap<PathBuf, Arc<info::Info>>,
    /// Photos marked for printing (Space), in the order they were added.
    queue: Vec<PathBuf>,
    queue_open: bool,
    /// Full-resolution texture of the current photo, loaded only when zoomed in
    /// past the display copy: (photo, None while loading).
    fullres: Option<(PathBuf, Option<TextureHandle>)>,
    thumbs: HashMap<PathBuf, Option<Thumb>>,
    /// CUPS answers can take a second each, so they're fetched in the background and kept.
    printers: Option<(Vec<String>, Option<String>)>,
    printer_options: HashMap<String, (Vec<PpdOption>, String)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, start: Option<PathBuf>) -> Self {
        // glance's own amber look, or the active Omarchy theme (followed live below).
        theme::apply(&cc.egui_ctx, theme::read_omarchy());
        install_fonts(&cc.egui_ctx);

        let (dir, files, idx) = resolve_start(start);
        let (tx, rx) = mpsc::channel();
        let mut app = App {
            ctx: cc.egui_ctx.clone(),
            config: Config::load(),
            dir,
            files,
            idx,
            cache: HashMap::new(),
            edits: HashMap::new(),
            edited_tex: HashMap::new(),
            tx,
            rx,
            zoom: 1.0,
            pan: Vec2::ZERO,
            editing: None,
            printing: None,
            prefs_open: false,
            raw_prompt: None,
            raw_remember: false,
            status: None,
            busy_saving: false,
            title: String::new(),
            #[cfg(feature = "demo")]
            demo: (Instant::now(), 0),
            logo: cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied([64, 64], &logo::rgba(64)),
                TEX,
            ),
            info_open: false,
            info_cache: HashMap::new(),
            queue: Vec::new(),
            queue_open: false,
            fullres: None,
            thumbs: HashMap::new(),
            printers: None,
            printer_options: HashMap::new(),
        };
        app.go(idx);
        let (tx, ctx) = (app.tx.clone(), app.ctx.clone());
        theme::watch(move |p| {
            let _ = tx.send(Msg::Theme(p));
            ctx.request_repaint();
        });
        // Warm up the printer list and the last printer's options, so Print opens instantly.
        let (tx, ctx, last) = (
            app.tx.clone(),
            app.ctx.clone(),
            app.config.last_printer.clone(),
        );
        std::thread::spawn(move || {
            let (dests, default) = print::destinations();
            let first = last
                .filter(|l| dests.contains(l))
                .or(default.clone())
                .or_else(|| dests.first().cloned());
            let _ = tx.send(Msg::Printers(dests, default));
            ctx.request_repaint();
            if let Some(d) = first {
                let (opts, state) = (print::options(&d), print::state(&d));
                let _ = tx.send(Msg::PrinterOptions(d, opts, state));
                ctx.request_repaint();
            }
        });
        app
    }

    fn current(&self) -> Option<&PathBuf> {
        self.files.get(self.idx)
    }

    fn current_entry(&self) -> Option<Arc<Entry>> {
        match self.cache.get(self.current()?)? {
            Slot::Ready(e) => Some(e.clone()),
            _ => None,
        }
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), Instant::now()));
    }

    // ---------------------------------------------------------------- loading

    fn go(&mut self, idx: usize) {
        if self.files.is_empty() {
            return;
        }
        self.idx = idx.min(self.files.len() - 1);
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        // Free the full-resolution copy of the previous photo.
        if self.fullres.as_ref().map(|f| &f.0) != self.files.get(self.idx) {
            self.fullres = None;
        }
        // Current first, then the next ones, then the previous ones.
        let n = self.files.len();
        let mut want = vec![self.idx];
        for d in 1..=KEEP {
            want.push((self.idx + d) % n);
            want.push((self.idx + n - d) % n);
        }
        for &i in &want {
            self.request(self.files[i].clone());
        }
        let keep: Vec<&PathBuf> = want.iter().map(|&i| &self.files[i]).collect();
        self.cache.retain(|p, _| keep.contains(&p));
    }

    fn request(&mut self, path: PathBuf) {
        if self.cache.contains_key(&path) {
            return;
        }
        self.cache.insert(path.clone(), Slot::Loading);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        rayon::spawn(move || {
            let res = files::load(&path, Some(DISPLAY_MAX)).map(|l| {
                let ci = egui::ColorImage::from_rgba_unmultiplied(
                    [l.img.width() as usize, l.img.height() as usize],
                    l.img.as_raw(),
                );
                let tex = ctx.load_texture(files::file_name(&path), ci, TEX);
                let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Entry {
                    tex,
                    img: Arc::new(l.img),
                    full: (l.full_w, l.full_h),
                    file_size,
                }
            });
            let _ = tx.send(Msg::Loaded(path, res));
            ctx.request_repaint();
        });
    }

    fn reload_dir(&mut self, select: Option<PathBuf>) {
        let keep = select.or_else(|| self.current().cloned());
        self.files = files::list_dir(&self.dir);
        if let Some(p) = keep
            && let Some(i) = self.files.iter().position(|f| *f == p)
        {
            self.idx = i;
        }
        self.go(self.idx);
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded(path, res) => {
                    // Ignore results for images that were evicted while loading.
                    if self.cache.contains_key(&path) {
                        let slot = match res {
                            Ok(e) => Slot::Ready(Arc::new(e)),
                            Err(e) => Slot::Failed(e),
                        };
                        self.cache.insert(path.clone(), slot);
                        if let Some(p) = self.edits.get(&path).copied()
                            && self.edited_tex.get(&path).map(|(q, _)| *q) != Some(p)
                        {
                            self.render_edited(path);
                        }
                    }
                }
                Msg::Edited(path, params, tex) => {
                    if self.edits.get(&path) == Some(&params) {
                        self.edited_tex.insert(path, (params, tex));
                    }
                }
                Msg::Saved(src, res) => {
                    self.busy_saving = false;
                    match res {
                        Ok(dst) => {
                            if dst == src {
                                // The edits are now baked into the file itself.
                                self.edits.remove(&src);
                                self.edited_tex.remove(&src);
                                self.cache.remove(&src);
                            }
                            self.set_status(format!("Saved {}", files::file_name(&dst)));
                            self.reload_dir(Some(src));
                        }
                        Err(e) => self.set_status(format!("Save failed: {e}")),
                    }
                }
                Msg::Printed(res) => match res {
                    Ok(m) => {
                        self.printing = None;
                        self.set_status(format!("Sent to printer: {m}"));
                    }
                    Err(e) => {
                        if let Some(p) = &mut self.printing {
                            p.busy = false;
                            p.progress = None;
                            p.error = Some(e);
                        }
                    }
                },
                Msg::Theme(p) => theme::apply(&self.ctx, p),
                Msg::FullRes(path, res) => {
                    // Keep it only if we're still on that photo.
                    if self.fullres.as_ref().map(|f| &f.0) == Some(&path) {
                        self.fullres = res.ok().map(|t| (path, Some(t)));
                    }
                }
                Msg::PrintProgress(m) => {
                    if let Some(p) = &mut self.printing {
                        p.progress = Some(m);
                    }
                }
                Msg::Printers(dests, default) => {
                    self.printers = Some((dests, default));
                    self.print_fill_dests();
                }
                Msg::PrinterOptions(dest, opts, state) => {
                    if let Some(pu) = self.printing.as_mut()
                        && pu.dests.get(pu.dest) == Some(&dest)
                    {
                        pu.apply_options(&dest, opts.clone(), state.clone(), &self.config);
                    }
                    self.printer_options.insert(dest, (opts, state));
                }
                Msg::Ink(dest, levels) => {
                    if let Some(p) = &mut self.printing
                        && p.dests.get(p.dest) == Some(&dest)
                    {
                        p.ink = Some(levels);
                    }
                }
                Msg::Thumb(path, res) => {
                    let t = res.ok().map(|(img, full)| {
                        let ci = egui::ColorImage::from_rgba_unmultiplied(
                            [img.width() as usize, img.height() as usize],
                            img.as_raw(),
                        );
                        let tex = self.ctx.load_texture("thumb", ci, TEX);
                        Thumb {
                            img,
                            full,
                            tex,
                            shown: EditParams::default(),
                        }
                    });
                    self.thumbs.insert(path, t);
                }
            }
        }
    }

    /// Build the browse-view texture showing `path` with its edits applied.
    fn render_edited(&mut self, path: PathBuf) {
        let Some(params) = self.edits.get(&path).copied() else {
            return;
        };
        let Some(Slot::Ready(e)) = self.cache.get(&path) else {
            return;
        };
        let img = e.img.clone();
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        rayon::spawn(move || {
            let out = edit::render(&img, &params);
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [out.width() as usize, out.height() as usize],
                out.as_raw(),
            );
            let tex = ctx.load_texture("edited", ci, TEX);
            let _ = tx.send(Msg::Edited(path, params, tex));
            ctx.request_repaint();
        });
    }

    fn shown_texture(&self, path: &Path, e: &Entry) -> TextureHandle {
        match (self.edits.get(path), self.edited_tex.get(path)) {
            (Some(p), Some((q, t))) if p == q => t.clone(),
            _ => e.tex.clone(),
        }
    }

    /// Start loading a small preview of `path` (for the queue and print preview).
    fn ensure_thumb(&mut self, path: &Path) {
        if self.thumbs.contains_key(path) {
            return;
        }
        self.thumbs.insert(path.to_path_buf(), None);
        let (tx, ctx, path) = (self.tx.clone(), self.ctx.clone(), path.to_path_buf());
        rayon::spawn(move || {
            let res = files::load(&path, Some(512)).map(|l| (l.img, (l.full_w, l.full_h)));
            let _ = tx.send(Msg::Thumb(path, res));
            ctx.request_repaint();
        });
    }

    /// Full-resolution size of `path`, if known yet.
    fn full_dims(&self, path: &Path) -> Option<(u32, u32)> {
        match self.cache.get(path) {
            Some(Slot::Ready(e)) => Some(e.full),
            _ => self.thumbs.get(path)?.as_ref().map(|t| t.full),
        }
    }

    /// A texture showing `path` with its current edits, from whatever is loaded.
    fn preview_tex(&mut self, path: &Path) -> Option<TextureHandle> {
        let params = self.edits.get(path).copied().unwrap_or_default();
        if let Some((q, t)) = self.edited_tex.get(path)
            && *q == params
        {
            return Some(t.clone());
        }
        if params.is_identity()
            && let Some(Slot::Ready(e)) = self.cache.get(path)
        {
            return Some(e.tex.clone());
        }
        self.ensure_thumb(path);
        let t = self.thumbs.get_mut(path)?.as_mut()?;
        if t.shown != params {
            let out = edit::render(&t.img, &params);
            t.tex.set(
                egui::ColorImage::from_rgba_unmultiplied(
                    [out.width() as usize, out.height() as usize],
                    out.as_raw(),
                ),
                TEX,
            );
            t.shown = params;
        }
        Some(t.tex.clone())
    }

    fn toggle_queue(&mut self) {
        let Some(p) = self.current().cloned() else {
            return;
        };
        if let Some(i) = self.queue.iter().position(|q| *q == p) {
            self.queue.remove(i);
            self.set_status(format!(
                "Removed from print queue ({} left)",
                self.queue.len()
            ));
        } else {
            self.queue.push(p.clone());
            self.ensure_thumb(&p);
            self.set_status(format!("Added to print queue ({})", self.queue.len()));
        }
    }

    // ---------------------------------------------------------------- actions

    fn start_edit(&mut self) {
        let (Some(path), Some(e)) = (self.current().cloned(), self.current_entry()) else {
            return;
        };
        let base = if e.img.width().max(e.img.height()) > EDIT_MAX {
            image::DynamicImage::ImageRgba8((*e.img).clone())
                .thumbnail(EDIT_MAX, EDIT_MAX)
                .into_rgba8()
        } else {
            (*e.img).clone()
        };
        let params = self.edits.get(&path).copied().unwrap_or_default();
        let ci = egui::ColorImage::from_rgba_unmultiplied(
            [base.width() as usize, base.height() as usize],
            base.as_raw(),
        );
        let tex = self.ctx.load_texture("edit", ci.clone(), TEX);
        let orig_tex = self.ctx.load_texture("edit-orig", ci, TEX);
        let (cw, ch) = edit::canvas_size(base.width() as f32, base.height() as f32, params.quarter);
        let crop = match params.crop {
            Some([x, y, w, h]) => [x * cw, y * ch, w * cw, h * ch],
            None => [0.0, 0.0, cw, ch],
        };
        self.editing = Some(EditState {
            path,
            params,
            base,
            tex,
            tone_shown: (0.0, 1.0),
            crop,
            crop_auto: params.crop.is_none(),
            ratio: 0,
            ratio_portrait: ch >= cw,
            drag: None,
            full: (e.full.0 as f32, e.full.1 as f32),
            orig_tex,
            before: false,
            hold_before: false,
            history: edit::History::new(Snap::default()),
        });
        if let Some(st) = self.editing.as_mut() {
            // Undo history starts from the state the photo opened with.
            st.history = edit::History::new(st.snap());
        }
    }

    /// Store the edit session's params for its image.
    fn commit_edit(&mut self) -> Option<(PathBuf, EditParams)> {
        let st = self.editing.take()?;
        let p = st.committed_params();
        if p.is_identity() {
            self.edits.remove(&st.path);
            self.edited_tex.remove(&st.path);
        } else {
            self.edits.insert(st.path.clone(), p);
            self.render_edited(st.path.clone());
        }
        Some((st.path, p))
    }

    fn save(&mut self, path: PathBuf, params: EditParams) {
        if params.is_identity() {
            self.set_status("Nothing to save: no edits");
            return;
        }
        let mode = if files::read_only_format(&path) {
            SaveMode::Copy
        } else {
            self.config.save_mode
        };
        let quality = self.config.jpeg_quality;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.busy_saving = true;
        self.set_status("Saving…");
        std::thread::spawn(move || {
            let res = (|| {
                let full = files::load(&path, None)?;
                let out = edit::render(&full.img, &params);
                let dst = match mode {
                    SaveMode::Copy => files::copy_path(&path),
                    SaveMode::Overwrite => path.clone(),
                    SaveMode::OverwriteBackup => {
                        let dir = path.parent().unwrap_or(Path::new(".")).join(".originals");
                        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                        let backup = dir.join(path.file_name().unwrap_or_default());
                        if !backup.exists() {
                            std::fs::copy(&path, &backup).map_err(|e| e.to_string())?;
                        }
                        path.clone()
                    }
                };
                // Keep the original's metadata: from the file itself, or from a backup when overwriting.
                let meta_src = if dst == path {
                    let tmp = std::env::temp_dir()
                        .join(format!("glance-meta-{}", files::file_name(&path)));
                    std::fs::copy(&path, &tmp).ok().map(|_| tmp)
                } else {
                    Some(path.clone())
                };
                files::write_image(&out, &dst, quality)?;
                if let Some(m) = meta_src {
                    files::copy_metadata(&m, &dst);
                    if m != path {
                        let _ = std::fs::remove_file(m);
                    }
                }
                Ok(dst)
            })();
            let _ = tx.send(Msg::Saved(path, res));
            ctx.request_repaint();
        });
    }

    fn open_print(&mut self, items: Vec<PathBuf>) {
        if items.is_empty() {
            return;
        }
        for p in &items {
            self.ensure_thumb(p);
        }
        let ui = PrintUi {
            items,
            dests: vec![],
            dest: 0,
            opts: vec![],
            sel: vec![],
            state: String::new(),
            ink: None,
            loading: true,
            scale: Scale::Fill,
            orient: Orient::Auto,
            margin: 3.0,
            copies: 1,
            page: 0,
            busy: false,
            progress: None,
            error: None,
        };
        self.printing = Some(ui);
        self.print_fill_dests();
    }

    /// Give the open print dialog the printer list, once known, and pick the printer.
    fn print_fill_dests(&mut self) {
        let Some((dests, default)) = self.printers.clone() else {
            return;
        };
        let Some(pu) = self.printing.as_mut() else {
            return;
        };
        if !pu.dests.is_empty() {
            return;
        }
        pu.dest = self
            .config
            .last_printer
            .as_ref()
            .and_then(|l| dests.iter().position(|d| d == l))
            .or_else(|| default.and_then(|d| dests.iter().position(|x| *x == d)))
            .unwrap_or(0);
        pu.dests = dests;
        if pu.dests.is_empty() {
            pu.loading = false;
        }
        self.print_select_dest();
    }

    /// Load the chosen printer's options (from the cache when possible) and ink levels.
    fn print_select_dest(&mut self) {
        let Some(pu) = self.printing.as_mut() else {
            return;
        };
        let Some(d) = pu.dests.get(pu.dest).cloned() else {
            return;
        };
        pu.ink = None;
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let d2 = d.clone();
        std::thread::spawn(move || {
            let levels = print::ink_levels(&d2);
            let _ = tx.send(Msg::Ink(d2, levels));
            ctx.request_repaint();
        });
        if let Some((opts, state)) = self.printer_options.get(&d).cloned() {
            pu.apply_options(&d, opts, state, &self.config);
        } else {
            pu.loading = true;
            pu.opts.clear();
            pu.sel.clear();
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                let (opts, state) = (print::options(&d), print::state(&d));
                let _ = tx.send(Msg::PrinterOptions(d, opts, state));
                ctx.request_repaint();
            });
        }
    }

    fn develop_raw(&mut self, raw_path: PathBuf) {
        let devs = raw::installed();
        if let Some(name) = &self.config.raw_developer
            && let Some((_, exe)) = devs.iter().find(|(n, _)| n == name)
        {
            match raw::launch(exe, &raw_path) {
                Ok(()) => self.set_status(format!("Opening in {name}…")),
                Err(e) => self.set_status(format!("Could not start {name}: {e}")),
            }
            return;
        }
        self.raw_remember = false;
        self.raw_prompt = Some(raw_path);
    }

    // ---------------------------------------------------------------- UI: browse

    fn keys_browse(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.printing.is_some() || self.raw_prompt.is_some() {
            return;
        }
        let n = self.files.len();
        let space = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Space));
        let (next, prev, home, end, e, p, f, esc, zero, q, info_key) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowRight)
                    || i.key_pressed(Key::ArrowDown)
                    || i.key_pressed(Key::PageDown),
                i.key_pressed(Key::ArrowLeft)
                    || i.key_pressed(Key::ArrowUp)
                    || i.key_pressed(Key::Backspace)
                    || i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::E),
                i.key_pressed(Key::P) || (i.modifiers.command && i.key_pressed(Key::P)),
                i.key_pressed(Key::F) || i.key_pressed(Key::F11),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Num0),
                i.key_pressed(Key::Q),
                i.key_pressed(Key::I),
            )
        });
        if n > 0 {
            if next {
                self.go((self.idx + 1) % n);
            }
            if prev {
                self.go((self.idx + n - 1) % n);
            }
            if home {
                self.go(0);
            }
            if end {
                self.go(n - 1);
            }
        }
        if e {
            self.start_edit();
        }
        if p {
            let shift = ctx.input(|i| i.modifiers.shift);
            if shift && !self.queue.is_empty() {
                self.open_print(self.queue.clone());
            } else if let Some(path) = self.current().cloned() {
                self.open_print(vec![path]);
            }
        }
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if f {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        if esc && fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if zero {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
        }
        if q {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if info_key {
            self.info_open = !self.info_open;
            self.queue_open &= !self.info_open;
        }
        if space {
            self.toggle_queue();
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::Image::new((self.logo.id(), Vec2::splat(20.0))));
            ui.label(
                egui::RichText::new("glance")
                    .strong()
                    .color(theme::accent()),
            );
            ui.separator();
            let n = self.files.len();
            if ui.button(ICON_PREV).on_hover_text("Previous (←)").clicked() && n > 0 {
                self.go((self.idx + n - 1) % n);
            }
            if ui.button(ICON_NEXT).on_hover_text("Next (→)").clicked() && n > 0 {
                self.go((self.idx + 1) % n);
            }
            if let Some(p) = self.current().cloned() {
                ui.label(egui::RichText::new(files::file_name(&p)).strong());
                ui.weak(format!("{} / {}", self.idx + 1, n));
                if self.edits.contains_key(&p) {
                    ui.colored_label(theme::accent(), "● edited");
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(ICON_GEAR).on_hover_text("Preferences").clicked() {
                    self.prefs_open = !self.prefs_open;
                }
                if ui
                    .selectable_label(self.info_open, ICON_INFO)
                    .on_hover_text("Photo details (I)")
                    .clicked()
                {
                    self.info_open = !self.info_open;
                    self.queue_open &= !self.info_open;
                }
                let qlabel = if self.queue.is_empty() {
                    ICON_QUEUE.to_string()
                } else {
                    format!("{ICON_QUEUE} {}", self.queue.len())
                };
                if ui
                    .selectable_label(self.queue_open, qlabel)
                    .on_hover_text("Print queue (Space adds the current photo)")
                    .clicked()
                {
                    self.queue_open = !self.queue_open;
                    self.info_open &= !self.queue_open;
                }
                let ready = self.current_entry().is_some();
                if ui
                    .add_enabled(ready, egui::Button::new(format!("{ICON_PRINT} Print")))
                    .on_hover_text("Print (P)")
                    .clicked()
                    && let Some(path) = self.current().cloned()
                {
                    self.open_print(vec![path]);
                }
                if ui
                    .add_enabled(ready, egui::Button::new(format!("{ICON_EDIT} Edit")))
                    .on_hover_text("Edit (E)")
                    .clicked()
                {
                    self.start_edit();
                }
                if let Some(p) = self.current()
                    && let Some(raw) = files::raw_sibling(p, &self.files)
                    && ui
                        .button("Develop RAW…")
                        .on_hover_text(files::file_name(&raw))
                        .clicked()
                {
                    self.develop_raw(raw);
                }
            });
        });
    }

    fn queue_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.heading(format!("Print queue ({})", self.queue.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(ICON_CLOSE).clicked() {
                    self.queue_open = false;
                }
            });
        });
        if self.queue.is_empty() {
            ui.add_space(8.0);
            ui.label("Press Space on a photo to add it here. Then print them all at once, one or several per sheet.");
            return;
        }
        ui.horizontal(|ui| {
            if ui
                .button(format!("{ICON_PRINT} Print {} photos…", self.queue.len()))
                .clicked()
            {
                self.open_print(self.queue.clone());
            }
            if ui.button(format!("{ICON_TRASH} Clear")).clicked() {
                self.queue.clear();
            }
        });
        ui.add_space(6.0);
        let mut remove = None;
        let mut goto = None;
        let items = self.queue.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, path) in items.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::click());
                    ui.painter().rect_filled(r, 4.0, Color32::from_gray(40));
                    if let Some(t) = self.preview_tex(path) {
                        let sz = t.size_vec2();
                        let k = (r.width() / sz.x).min(r.height() / sz.y);
                        let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                        ui.painter().image(
                            t.id(),
                            Rect::from_center_size(r.center(), sz * k),
                            full,
                            Color32::WHITE,
                        );
                    }
                    if resp.clicked() {
                        goto = Some(path.clone());
                    }
                    ui.vertical(|ui| {
                        ui.label(files::file_name(path));
                        if self.edits.contains_key(path) {
                            ui.colored_label(theme::accent(), "edited");
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button(ICON_CLOSE)
                            .on_hover_text("Remove from queue")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
            }
        });
        if let Some(i) = remove {
            self.queue.remove(i);
        }
        if let Some(p) = goto
            && let Some(i) = self.files.iter().position(|f| *f == p)
        {
            self.go(i);
        }
    }

    fn info_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.heading("Details");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(ICON_CLOSE)
                    .on_hover_text("Close (I)")
                    .clicked()
                {
                    self.info_open = false;
                }
            });
        });
        let (Some(path), Some(e)) = (self.current().cloned(), self.current_entry()) else {
            ui.weak("Loading…");
            return;
        };
        if self.info_cache.len() > 256 {
            self.info_cache.clear();
        }
        let inf = self
            .info_cache
            .entry(path.clone())
            .or_insert_with(|| Arc::new(info::read(&path, e.full, e.file_size)))
            .clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (title, rows) in &inf.sections {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(*title).strong().color(theme::accent()));
                egui::Grid::new(*title)
                    .num_columns(2)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        for (k, v) in rows {
                            ui.weak(*k);
                            ui.add(egui::Label::new(v.as_str()).wrap());
                            ui.end_row();
                        }
                    });
            }
            if inf.sections.len() == 1 {
                ui.add_space(8.0);
                ui.weak("No camera information (EXIF) in this file.");
            }
        });
    }

    fn statusbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(e) = self.current_entry() {
                ui.weak(format!("{} × {} px · {}", e.full.0, e.full.1, info::human_size(e.file_size)));
            }
            ui.weak(format!("· {}", files::display_path(&self.dir)));
            if let Some((s, t)) = &self.status
                && (t.elapsed() < Duration::from_secs(8) || self.busy_saving) {
                    ui.separator();
                    ui.label(s);
                }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.available_width() > 520.0 {
                    ui.weak(format!("{ICON_PREV}{ICON_NEXT} browse · Space queue · E edit · P print · Shift+P print queue · I info · F fullscreen"));
                }
            });
        });
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::image_bg());
        let Some(path) = self.current().cloned() else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("No photos in {}", files::display_path(&self.dir)),
                FontId::proportional(18.0),
                Color32::GRAY,
            );
            return;
        };
        match self.cache.get(&path) {
            Some(Slot::Ready(e)) => {
                let e = e.clone();
                let tex = self.shown_texture(&path, &e);
                let size = tex.size_vec2();
                let ppp = ui.ctx().pixels_per_point();
                // Fit to the window, but never enlarge past 1:1 screen pixels.
                let fit = (rect.width() / size.x)
                    .min(rect.height() / size.y)
                    .min(1.0 / ppp * e.full.0 as f32 / e.img.width() as f32);
                if resp.hovered() {
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        let old = self.zoom;
                        self.zoom = (self.zoom * (scroll * 0.003).exp()).clamp(0.2, 40.0);
                        if let Some(ptr) = resp.hover_pos() {
                            let rel = ptr - rect.center() - self.pan;
                            self.pan += rel - rel * (self.zoom / old);
                        }
                    }
                }
                if resp.dragged() {
                    self.pan += resp.drag_delta();
                }
                if resp.double_clicked() {
                    // Toggle between fit and 1:1 (one image pixel per screen pixel).
                    let one = (e.full.0 as f32 / ppp) / (size.x * fit)
                        * (size.x / e.img.width() as f32).min(1.0);
                    let one = if self.edits.contains_key(&path) {
                        1.0 / ppp / fit
                    } else {
                        one
                    };
                    if (self.zoom - 1.0).abs() < 0.01 {
                        self.zoom = one.max(1.0);
                    } else {
                        self.zoom = 1.0;
                        self.pan = Vec2::ZERO;
                    }
                }
                let img_rect =
                    Rect::from_center_size(rect.center() + self.pan, size * fit * self.zoom);
                // Zoomed in past the display copy's pixels: show the full-resolution
                // photo instead, loaded in the background (not for edited photos).
                let edited = self.edits.contains_key(&path);
                let needs_full = !edited
                    && e.full.0 > e.img.width()
                    && img_rect.width() * ppp > e.img.width() as f32 * 1.05;
                let mut tex = tex;
                let mut loading_full = false;
                match &self.fullres {
                    Some((p, Some(t))) if *p == path && !edited => tex = t.clone(),
                    Some((p, None)) if *p == path => loading_full = needs_full,
                    _ if needs_full => {
                        self.fullres = Some((path.clone(), None));
                        loading_full = true;
                        let max = ui.ctx().input(|i| i.max_texture_side) as u32;
                        let (tx, ctx, p) = (self.tx.clone(), ui.ctx().clone(), path.clone());
                        rayon::spawn(move || {
                            let res = files::load(&p, Some(max)).map(|l| {
                                let ci = egui::ColorImage::from_rgba_unmultiplied(
                                    [l.img.width() as usize, l.img.height() as usize],
                                    l.img.as_raw(),
                                );
                                ctx.load_texture("full", ci, TEX)
                            });
                            let _ = tx.send(Msg::FullRes(p, res));
                            ctx.request_repaint();
                        });
                    }
                    _ => {}
                }
                painter.image(
                    tex.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                if loading_full {
                    let r = Rect::from_min_size(
                        rect.left_bottom() + Vec2::new(12.0, -38.0),
                        Vec2::new(200.0, 26.0),
                    );
                    painter.rect_filled(r, 13.0, Color32::from_black_alpha(180));
                    painter.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        "Loading full resolution…",
                        FontId::proportional(13.0),
                        Color32::LIGHT_GRAY,
                    );
                }
                if self.queue.contains(&path) {
                    let r = Rect::from_min_size(
                        rect.min + Vec2::new(12.0, 12.0),
                        Vec2::new(150.0, 26.0),
                    );
                    painter.rect_filled(r, 13.0, Color32::from_black_alpha(180));
                    painter.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        format!("{ICON_IN_QUEUE} In print queue"),
                        FontId::proportional(13.0),
                        theme::accent(),
                    );
                }
            }
            Some(Slot::Failed(err)) => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("Can't open {}\n{err}", files::file_name(&path)),
                    FontId::proportional(16.0),
                    Color32::LIGHT_RED,
                );
            }
            _ => {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "Loading…",
                    FontId::proportional(16.0),
                    Color32::GRAY,
                );
            }
        }
    }

    // ---------------------------------------------------------------- UI: edit

    fn edit_ui(&mut self, ui: &mut egui::Ui) {
        let mut action = None::<&str>;
        {
            let ctx = ui.ctx().clone();
            let st = self.editing.as_mut().unwrap();
            if !ctx.egui_wants_keyboard_input() {
                ctx.input(|i| {
                    if i.key_pressed(Key::Escape) {
                        action = Some("cancel");
                    }
                    if i.key_pressed(Key::Enter) {
                        action = Some("done");
                    }
                });
                // Check redo first: egui also matches Ctrl+Shift+Z as Ctrl+Z.
                let (redo, undo) = ctx.input_mut(|i| {
                    let redo = i
                        .consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::Z)
                        || i.consume_key(egui::Modifiers::COMMAND, Key::Y);
                    (redo, i.consume_key(egui::Modifiers::COMMAND, Key::Z))
                });
                if redo {
                    st.redo();
                } else if undo {
                    st.undo();
                }
            }
            let thirds = &mut self.config.thirds;
            egui::Panel::right("tools")
                .exact_size(300.0)
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    // Slider + its value box must fit the panel: 300 - margins - value box.
                    ui.spacing_mut().slider_width = 196.0;
                    ui.set_max_width(284.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        action = st.tools(ui, thirds).or(action);
                    });
                });
            egui::CentralPanel::default().show(ui, |ui| st.canvas(ui, *thirds));
            // A settled change (nothing being dragged) becomes one undo step.
            if !ctx.input(|i| i.pointer.any_down()) {
                st.history.record(st.snap());
            }
            st.refresh_tone(&ctx);
        }
        match action {
            Some("cancel") => self.editing = None,
            Some("done") => {
                self.commit_edit();
            }
            Some("save") => {
                if let Some((p, params)) = self.commit_edit() {
                    self.save(p, params);
                }
            }
            Some("print") => {
                if let Some((p, _)) = self.commit_edit() {
                    self.open_print(vec![p]);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- UI: dialogs

    fn print_window(&mut self, ctx: &egui::Context) {
        let Some(mut pu) = self.printing.take() else {
            return;
        };
        // Photo sizes and textures for the preview.
        let items: Vec<PreviewItem> = pu
            .items
            .iter()
            .map(|p| {
                let params = self.edits.get(p).copied().unwrap_or_default();
                let px = self
                    .full_dims(p)
                    .map(|(w, h)| params.output_size(w as f32, h as f32));
                (p.clone(), px, self.preview_tex(p))
            })
            .collect();
        let mut close = false;
        let mut submit = false;
        let mut dest_changed = false;
        egui::Modal::new(egui::Id::unique("print")).show(ctx, |ui| {
            ui.set_width(860.0);
            ui.heading(if items.len() == 1 {
                "Print".to_string()
            } else {
                format!("Print {} photos", items.len())
            });
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(400.0), Sense::hover());
                    let n_pages = draw_print_preview(ui, rect, &pu, &items);
                    if n_pages > 1 {
                        ui.horizontal(|ui| {
                            ui.add_space(110.0);
                            if ui.button(ICON_PREV).clicked() && pu.page > 0 {
                                pu.page -= 1;
                            }
                            ui.label(format!("Sheet {} of {n_pages}", pu.page + 1));
                            if ui.button(ICON_NEXT).clicked() && pu.page + 1 < n_pages {
                                pu.page += 1;
                            }
                        });
                    }
                });

                ui.add_space(12.0);
                ui.vertical(|ui| {
                    egui::Grid::new("print-grid")
                        .num_columns(2)
                        .spacing([10.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Printer");
                            let cur = pu
                                .dests
                                .get(pu.dest)
                                .cloned()
                                .unwrap_or_else(|| "(no printers found)".into());
                            let mut new_dest = pu.dest;
                            egui::ComboBox::from_id_salt("dest")
                                .width(330.0)
                                .selected_text(dest_label(&cur))
                                .show_ui(ui, |ui| {
                                    for (i, d) in pu.dests.iter().enumerate() {
                                        ui.selectable_value(&mut new_dest, i, dest_label(d));
                                    }
                                });
                            if new_dest != pu.dest {
                                pu.dest = new_dest;
                                dest_changed = true;
                            }
                            ui.end_row();
                            ui.label("");
                            ui.vertical(|ui| {
                                ui.weak(format!("Status: {}", pu.state));
                                ink_bars(ui, pu.ink.as_deref());
                            });
                            ui.end_row();

                            for (key, name) in [
                                ("PageSize", "Paper size"),
                                ("InputSlot", "Feeder"),
                                ("MediaType", "Paper type"),
                            ] {
                                if let Some(i) = pu.opts.iter().position(|o| o.key == key) {
                                    ui.label(name);
                                    option_combo(ui, &pu.opts[i], &mut pu.sel[i]);
                                    ui.end_row();
                                }
                            }

                            ui.label("Scaling");
                            ui.vertical(|ui| {
                                ui.radio_value(
                                    &mut pu.scale,
                                    Scale::Fill,
                                    "Fill (crops what doesn't fit)",
                                );
                                ui.radio_value(
                                    &mut pu.scale,
                                    Scale::Fit,
                                    "Whole photo (white bands if shapes differ)",
                                );
                            });
                            ui.end_row();
                            ui.label("Orientation");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut pu.orient, Orient::Auto, "Auto");
                                ui.selectable_value(&mut pu.orient, Orient::Portrait, "Portrait");
                                ui.selectable_value(&mut pu.orient, Orient::Landscape, "Landscape");
                            });
                            ui.end_row();
                            let borderless = pu.borderless();
                            ui.label("Margins");
                            ui.add_enabled_ui(!borderless, |ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut pu.margin)
                                            .range(0.0..=30.0)
                                            .speed(0.1)
                                            .suffix(" mm"),
                                    );
                                    if borderless {
                                        ui.weak("borderless paper size");
                                    }
                                });
                            });
                            ui.end_row();
                            ui.label("Copies");
                            ui.add(egui::DragValue::new(&mut pu.copies).range(1..=99));
                            ui.end_row();
                        });

                    let others: Vec<usize> = (0..pu.opts.len())
                        .filter(|&i| {
                            !["PageSize", "InputSlot", "MediaType"]
                                .contains(&pu.opts[i].key.as_str())
                        })
                        .collect();
                    if !others.is_empty() {
                        ui.collapsing("More printer options", |ui| {
                            egui::Grid::new("more").num_columns(2).show(ui, |ui| {
                                for i in others {
                                    ui.label(&pu.opts[i].label);
                                    option_combo(ui, &pu.opts[i], &mut pu.sel[i]);
                                    ui.end_row();
                                }
                            });
                        });
                    }
                    if let Some(e) = &pu.error {
                        ui.colored_label(Color32::LIGHT_RED, e);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let sheets = if pu.sheet().is_some() { items.len() } else { 0 };
                        let can = !pu.busy && sheets > 0;
                        let label = if pu.busy {
                            "Printing…".to_string()
                        } else if sheets > 1 {
                            format!("{ICON_PRINT} Print {sheets} sheets")
                        } else {
                            format!("{ICON_PRINT} Print")
                        };
                        if ui.add_enabled(can, egui::Button::new(label)).clicked() {
                            submit = true;
                        }
                        if ui
                            .add_enabled(!pu.busy, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            close = true;
                        }
                        if pu.busy {
                            ui.spinner();
                            if let Some(m) = &pu.progress {
                                ui.weak(m);
                            }
                        }
                    });
                });
            });
            if ui.input(|i| i.key_pressed(Key::Escape)) && !pu.busy {
                close = true;
            }
        });
        if submit && let Some(sheet) = pu.sheet() {
            pu.busy = true;
            pu.error = None;
            let dest = pu.dests[pu.dest].clone();
            self.config.last_printer = Some(dest.clone());
            let chosen = pu
                .opts
                .iter()
                .zip(&pu.sel)
                .map(|(o, &s)| (o.key.clone(), o.values[s].clone()))
                .collect();
            self.config.print_options.insert(dest.clone(), chosen);
            self.config.save();
            let job = print::Job {
                items: pu
                    .items
                    .iter()
                    .map(|p| (p.clone(), self.edits.get(p).copied().unwrap_or_default()))
                    .collect(),
                sheet,
                dest,
                options: pu
                    .opts
                    .iter()
                    .zip(&pu.sel)
                    .map(|(o, &s)| (o.key.clone(), o.values[s].clone()))
                    .collect(),
                copies: pu.copies,
            };
            let (tx, ctx2) = (self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let (tx2, ctx3) = (tx.clone(), ctx2.clone());
                let res = print::run_job(&job, move |m| {
                    let _ = tx2.send(Msg::PrintProgress(m));
                    ctx3.request_repaint();
                });
                let _ = tx.send(Msg::Printed(res));
                ctx2.request_repaint();
            });
        }
        if !close {
            self.printing = Some(pu);
            if dest_changed {
                self.print_select_dest();
            }
        }
    }

    fn prefs_window(&mut self, ctx: &egui::Context) {
        let mut open = self.prefs_open;
        let mut changed = false;
        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.strong("When saving edits");
                changed |= ui
                    .radio_value(
                        &mut self.config.save_mode,
                        SaveMode::Copy,
                        "Save a copy (name-edit.jpg), keep the original",
                    )
                    .changed();
                changed |= ui
                    .radio_value(
                        &mut self.config.save_mode,
                        SaveMode::OverwriteBackup,
                        "Overwrite, keep the original in a .originals folder",
                    )
                    .changed();
                changed |= ui
                    .radio_value(
                        &mut self.config.save_mode,
                        SaveMode::Overwrite,
                        "Overwrite the original",
                    )
                    .changed();
                ui.weak("RAW and HEIC files are always saved as a JPEG copy.");
                if heic::available() {
                    ui.weak("HEIC / HEIF / AVIF support: on (libheif found).");
                } else {
                    ui.colored_label(
                        theme::accent(),
                        "HEIC / HEIF / AVIF support: install libheif to open these photos.",
                    );
                }
                ui.add_space(6.0);
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.config.jpeg_quality, 70..=100)
                            .text("JPEG quality"),
                    )
                    .changed();
                ui.add_space(10.0);
                ui.strong("RAW developer");
                let devs = raw::installed();
                let label = self
                    .config
                    .raw_developer
                    .clone()
                    .unwrap_or_else(|| "Ask each time".into());
                egui::ComboBox::from_id_salt("rawdev")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(&mut self.config.raw_developer, None, "Ask each time")
                            .changed();
                        for (name, _) in &devs {
                            changed |= ui
                                .selectable_value(
                                    &mut self.config.raw_developer,
                                    Some(name.clone()),
                                    name,
                                )
                                .changed();
                        }
                    });
                if devs.is_empty() {
                    ui.weak("No RAW developer found.");
                }
                ui.add_space(10.0);
                changed |= ui
                    .checkbox(
                        &mut self.config.thirds,
                        "Show rule-of-thirds guides when editing",
                    )
                    .changed();
            });
        self.prefs_open = open;
        if changed {
            self.config.save();
        }
    }

    fn raw_window(&mut self, ctx: &egui::Context) {
        let Some(raw_path) = self.raw_prompt.clone() else {
            return;
        };
        let devs = raw::installed();
        let mut close = false;
        let mut launch = None;
        egui::Modal::new(egui::Id::unique("raw")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Develop RAW");
            ui.label(files::file_name(&raw_path));
            ui.add_space(8.0);
            if devs.is_empty() {
                ui.label("No RAW developer is installed. Good options:");
                for (name, how) in raw::suggestions() {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(*name);
                        ui.monospace(*how);
                    });
                }
            } else {
                ui.label("Open it in:");
                for (name, exe) in &devs {
                    if ui
                        .add_sized([400.0, 30.0], egui::Button::new(name.as_str()))
                        .clicked()
                    {
                        launch = Some((name.clone(), exe.clone()));
                    }
                }
                ui.checkbox(
                    &mut self.raw_remember,
                    "Always use this app (change in Preferences)",
                );
            }
            ui.add_space(6.0);
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                close = true;
            }
        });
        if let Some((name, exe)) = launch {
            if self.raw_remember {
                self.config.raw_developer = Some(name.clone());
                self.config.save();
            }
            match raw::launch(&exe, &raw_path) {
                Ok(()) => self.set_status(format!("Opening in {name}…")),
                Err(e) => self.set_status(format!("Could not start {name}: {e}")),
            }
            close = true;
        }
        if close {
            self.raw_prompt = None;
        }
    }
}

fn dest_label(d: &str) -> String {
    match d.split_once('/') {
        Some((printer, preset)) => format!("{preset}   ({printer})"),
        None => d.replace('_', " "),
    }
}

fn option_combo(ui: &mut egui::Ui, o: &PpdOption, sel: &mut usize) {
    egui::ComboBox::from_id_salt(&o.key)
        .width(330.0)
        .selected_text(print::value_label(&o.key, &o.values[*sel]))
        .show_ui(ui, |ui| {
            for (i, v) in o.values.iter().enumerate() {
                ui.selectable_value(sel, i, print::value_label(&o.key, v));
            }
        });
}

/// Coloured ink level bars, like the printer's own display.
fn ink_bars(ui: &mut egui::Ui, ink: Option<&[(String, i32, [u8; 3])]>) {
    match ink {
        None => {
            ui.weak("Reading ink levels…");
        }
        Some([]) => {
            ui.weak("No ink information from this printer yet.");
        }
        Some(levels) => {
            ui.horizontal(|ui| {
                ui.weak(ICON_DROP);
                for (name, level, c) in levels {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(14.0, 30.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(r, 2.0, Color32::from_gray(50));
                    let h = r.height() * (*level as f32 / 100.0).clamp(0.0, 1.0);
                    let col = if *c == [0, 0, 0] {
                        Color32::from_gray(15)
                    } else {
                        Color32::from_rgb(c[0], c[1], c[2])
                    };
                    p.rect_filled(
                        Rect::from_min_max(Pos2::new(r.min.x, r.max.y - h), r.max),
                        2.0,
                        col,
                    );
                    p.rect_stroke(
                        r,
                        2.0,
                        Stroke::new(
                            1.0,
                            if *level <= 15 {
                                Color32::from_rgb(255, 120, 60)
                            } else {
                                Color32::from_gray(90)
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    resp.on_hover_text(format!("{name}: {level}%"));
                }
                if let Some((name, level, _)) = levels.iter().min_by_key(|l| l.1) {
                    let txt = format!("lowest: {name} {level}%");
                    if *level <= 15 {
                        ui.colored_label(Color32::from_rgb(255, 150, 80), txt)
                    } else {
                        ui.weak(txt)
                    };
                }
            });
        }
    }
}

/// Draw the current preview sheet; returns the number of sheets.
fn draw_print_preview(ui: &egui::Ui, rect: Rect, pu: &PrintUi, items: &[PreviewItem]) -> usize {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, Color32::from_gray(40));
    let Some(sheet) = pu.sheet() else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            if pu.loading {
                "Reading printer options…"
            } else {
                "Choose a paper size"
            },
            FontId::proportional(14.0),
            Color32::GRAY,
        );
        return 0;
    };
    if items.is_empty() {
        return 0;
    }
    // One photo per sheet.
    let ii = pu.page.min(items.len() - 1);
    let (w, h) = items[ii].1.unwrap_or((3.0, 2.0));
    let placed = [(
        ii,
        print::place_in(w, h, print::area(&sheet), sheet.scale, sheet.orient),
    )];
    // Show the sheet the way the photo reads.
    let turned = placed[0].1.rotate;
    let (pw, ph) = sheet.paper;
    let (vw, vh) = if turned { (ph, pw) } else { (pw, ph) };
    let s = (rect.width() - 30.0) / vw.max(vh);
    let origin = rect.center() - Vec2::new(vw, vh) * s / 2.0 - Vec2::new(0.0, 8.0);
    let tx = |x: f32, y: f32| -> Pos2 {
        let (x, y) = if turned { (y, pw - x) } else { (x, y) };
        origin + Vec2::new(x, y) * s
    };
    let mm_rect = |r: [f32; 4]| Rect::from_two_pos(tx(r[0], r[1]), tx(r[0] + r[2], r[1] + r[3]));
    let page_r = mm_rect([0.0, 0.0, pw, ph]);
    painter.rect_filled(
        page_r.translate(Vec2::splat(3.0)),
        0.0,
        Color32::from_black_alpha(120),
    );
    painter.rect_filled(page_r, 0.0, Color32::WHITE);
    if sheet.margin > 0.0 {
        let m = sheet.margin;
        painter.rect_stroke(
            mm_rect([m, m, pw - 2.0 * m, ph - 2.0 * m]),
            0.0,
            Stroke::new(0.5, Color32::from_rgba_unmultiplied(0, 120, 255, 90)),
            egui::StrokeKind::Inside,
        );
    }
    let mut min_dpi = f32::MAX;
    for (ii, pl) in &placed {
        let [dx, dy, dw, dh] = pl.dest;
        let corners = [(dx, dy), (dx + dw, dy), (dx + dw, dy + dh), (dx, dy + dh)];
        let [sx, sy, sw, sh] = pl.src;
        let uvs = [(sx, sy), (sx + sw, sy), (sx + sw, sy + sh), (sx, sy + sh)].map(|(u, v)| {
            // Undo the 90° clockwise turn to find the spot in the original texture.
            if pl.rotate {
                Pos2::new(v, 1.0 - u)
            } else {
                Pos2::new(u, v)
            }
        });
        match &items[*ii].2 {
            Some(t) => {
                let mut mesh = egui::Mesh::with_texture(t.id());
                for (c, uv) in corners.iter().zip(uvs) {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: tx(c.0, c.1),
                        uv,
                        color: Color32::WHITE,
                    });
                }
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(egui::Shape::mesh(mesh));
            }
            None => {
                painter.rect_filled(mm_rect(pl.dest), 0.0, Color32::from_gray(200));
            }
        }
        if let Some((w, h)) = items[*ii].1 {
            let src_w = if pl.rotate { h } else { w };
            min_dpi = min_dpi.min(src_w * sw / (dw / 25.4));
        }
    }
    let first = placed[0].1.dest;
    let (cw, chh) = if turned {
        (first[3], first[2])
    } else {
        (first[2], first[3])
    };
    let mut info = format!("{:.1} × {:.1} cm", cw / 10.0, chh / 10.0);
    let col = if min_dpi < 180.0 {
        Color32::from_rgb(255, 170, 60)
    } else {
        Color32::LIGHT_GRAY
    };
    if min_dpi < f32::MAX {
        info.push_str(&format!(" · {:.0} dpi", min_dpi));
    }
    painter.text(
        rect.center_bottom() - Vec2::new(0.0, 4.0),
        Align2::CENTER_BOTTOM,
        info,
        FontId::proportional(13.0),
        col,
    );
    items.len()
}

impl PrintUi {
    fn apply_options(&mut self, dest: &str, opts: Vec<PpdOption>, state: String, config: &Config) {
        self.opts = opts;
        self.sel = self.opts.iter().map(|o| o.default).collect();
        // Use what was chosen last time with this printer/preset.
        if let Some(saved) = config.print_options.get(dest) {
            for (o, sel) in self.opts.iter().zip(self.sel.iter_mut()) {
                if let Some(i) = saved
                    .get(&o.key)
                    .and_then(|v| o.values.iter().position(|x| x == v))
                {
                    *sel = i;
                }
            }
        }
        self.state = state;
        self.loading = false;
    }

    fn page_key(&self) -> Option<&str> {
        let i = self.opts.iter().position(|o| o.key == "PageSize")?;
        Some(self.opts[i].values[self.sel[i]].as_str())
    }

    fn borderless(&self) -> bool {
        self.page_key()
            .and_then(print::paper_mm)
            .map(|p| p.2)
            .unwrap_or(false)
    }

    /// Paper (portrait mm) and effective margin.
    fn paper(&self) -> Option<((f32, f32), f32)> {
        let (w, h, borderless) = print::paper_mm(self.page_key()?)?;
        Some((
            (w.min(h), w.max(h)),
            if borderless { 0.0 } else { self.margin },
        ))
    }

    fn sheet(&self) -> Option<Sheet> {
        let (paper, margin) = self.paper()?;
        Some(Sheet {
            paper,
            margin,
            scale: self.scale,
            orient: self.orient,
        })
    }
}

impl EditState {
    fn snap(&self) -> Snap {
        Snap {
            params: self.params,
            crop: self.crop,
            ratio: self.ratio,
            ratio_portrait: self.ratio_portrait,
            crop_auto: self.crop_auto,
        }
    }

    fn restore(&mut self, s: Snap) {
        self.params = s.params;
        self.crop = s.crop;
        self.ratio = s.ratio;
        self.ratio_portrait = s.ratio_portrait;
        self.crop_auto = s.crop_auto;
        self.drag = None;
    }

    fn undo(&mut self) {
        if let Some(s) = self.history.undo(self.snap()) {
            self.restore(s);
        }
    }

    fn redo(&mut self) {
        if let Some(s) = self.history.redo(self.snap()) {
            self.restore(s);
        }
    }

    fn geo(&self) -> Geo {
        let (cw, ch) = edit::canvas_size(
            self.base.width() as f32,
            self.base.height() as f32,
            self.params.quarter,
        );
        Geo {
            cw,
            ch,
            angle: self.params.angle.to_radians(),
        }
    }

    fn aspect(&self) -> Option<f32> {
        let r = RATIOS[self.ratio].1;
        let g = self.geo();
        if r == 0.0 {
            None
        } else if r < 0.0 {
            let (w, h) = (self.base.width() as f32, self.base.height() as f32);
            let (cw, ch) = if g.cw >= g.ch {
                (w.max(h), w.min(h))
            } else {
                (w.min(h), w.max(h))
            };
            Some(cw / ch)
        } else if self.ratio_portrait {
            Some(r)
        } else {
            Some(1.0 / r)
        }
    }

    fn committed_params(&self) -> EditParams {
        let mut p = self.params;
        let g = self.geo();
        let [x, y, w, h] = self.crop;
        let full =
            x.abs() < 0.5 && y.abs() < 0.5 && (w - g.cw).abs() < 0.5 && (h - g.ch).abs() < 0.5;
        p.crop = if full {
            None
        } else {
            Some([x / g.cw, y / g.ch, w / g.cw, h / g.ch])
        };
        p
    }

    /// Re-fit the crop after the rotation or ratio changed.
    fn refit(&mut self, reset: bool) {
        let g = self.geo();
        let aspect = self.aspect().unwrap_or(if reset || self.crop_auto {
            g.cw / g.ch
        } else {
            self.crop[2] / self.crop[3]
        });
        if reset || self.crop_auto {
            self.crop = if self.params.angle == 0.0 && self.aspect().is_none() {
                [0.0, 0.0, g.cw, g.ch]
            } else {
                g.max_rect(aspect)
            };
        } else {
            self.crop = g.fit_rect(self.crop);
        }
    }

    fn refresh_tone(&mut self, ctx: &egui::Context) {
        let want = (self.params.exposure, self.params.gamma);
        if want == self.tone_shown {
            return;
        }
        let mut img = self.base.clone();
        if !self.params.tone_is_identity() {
            edit::apply_lut(&mut img, &edit::lut(want.0, want.1));
        }
        let ci = egui::ColorImage::from_rgba_unmultiplied(
            [img.width() as usize, img.height() as usize],
            img.as_raw(),
        );
        self.tex.set(ci, TEX);
        self.tone_shown = want;
        ctx.request_repaint();
    }

    fn tools(&mut self, ui: &mut egui::Ui, thirds: &mut bool) -> Option<&'static str> {
        let mut action = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.history.can_undo(),
                    egui::Button::new(format!("{ICON_UNDO} Undo")),
                )
                .on_hover_text("Ctrl+Z")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(
                    self.history.can_redo(),
                    egui::Button::new(format!("{ICON_REDO} Redo")),
                )
                .on_hover_text("Ctrl+Shift+Z or Ctrl+Y")
                .clicked()
            {
                self.redo();
            }
        });
        ui.add_space(6.0);
        ui.heading("Crop");
        let old_ratio = (self.ratio, self.ratio_portrait);
        egui::ComboBox::from_id_salt("ratio")
            .width(270.0)
            .selected_text(RATIOS[self.ratio].0)
            .show_ui(ui, |ui| {
                for (i, (name, _)) in RATIOS.iter().enumerate() {
                    ui.selectable_value(&mut self.ratio, i, *name);
                }
            });
        if RATIOS[self.ratio].1 > 0.0 && RATIOS[self.ratio].1 != 1.0 {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.ratio_portrait, true, "Portrait");
                ui.selectable_value(&mut self.ratio_portrait, false, "Landscape");
            });
        }
        if (self.ratio, self.ratio_portrait) != old_ratio && self.aspect().is_some() {
            self.crop_auto = true;
            self.refit(false);
        }
        if ui.button("Reset crop").clicked() {
            self.ratio = 0;
            self.crop_auto = true;
            self.refit(true);
        }
        ui.checkbox(thirds, "Rule-of-thirds guides");

        ui.add_space(12.0);
        ui.heading("Rotate");
        let old_angle = self.params.angle;
        let old_q = self.params.quarter;
        ui.add(
            egui::Slider::new(&mut self.params.angle, -45.0..=45.0)
                .step_by(0.1)
                .fixed_decimals(1)
                .suffix("°"),
        );
        ui.horizontal(|ui| {
            if ui.button("−0.1°").clicked() {
                self.params.angle = (self.params.angle - 0.1).max(-45.0);
            }
            if ui.button("+0.1°").clicked() {
                self.params.angle = (self.params.angle + 0.1).min(45.0);
            }
            if ui.button("Level").on_hover_text("Back to 0°").clicked() {
                self.params.angle = 0.0;
            }
        });
        ui.horizontal(|ui| {
            if ui.button(format!("{ICON_CCW} 90°")).clicked() {
                self.params.quarter = (self.params.quarter + 3) % 4;
            }
            if ui.button(format!("{ICON_CW} 90°")).clicked() {
                self.params.quarter = (self.params.quarter + 1) % 4;
            }
        });
        if self.params.quarter != old_q {
            self.ratio_portrait = !self.ratio_portrait;
            self.crop_auto = true;
            self.refit(true);
        } else if self.params.angle != old_angle {
            self.refit(false);
        }

        ui.add_space(12.0);
        ui.heading("Light");
        ui.label("Exposure (EV)");
        ui.add(
            egui::Slider::new(&mut self.params.exposure, -3.0..=3.0)
                .step_by(0.05)
                .fixed_decimals(2),
        );
        ui.label("Gamma (midtones)");
        ui.add(
            egui::Slider::new(&mut self.params.gamma, 0.4..=2.5)
                .step_by(0.01)
                .logarithmic(true)
                .fixed_decimals(2),
        );
        if ui.button("Reset light").clicked() {
            self.params.exposure = 0.0;
            self.params.gamma = 1.0;
        }

        ui.add_space(16.0);
        ui.separator();
        let cmp = ui.add(
            egui::Button::new(format!("{ICON_EYE} Hold to compare")).sense(Sense::click_and_drag()),
        );
        self.before = cmp.is_pointer_button_down_on() || self.hold_before;
        cmp.on_hover_text("Shows the original while held (or hold B)");
        ui.add_space(4.0);
        if ui.button("Reset all").clicked() {
            self.params = EditParams::default();
            self.ratio = 0;
            self.crop_auto = true;
            self.refit(true);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(format!("{ICON_CHECK} Done")).min_size(Vec2::new(90.0, 28.0)),
                )
                .on_hover_text("Keep edits (Enter)")
                .clicked()
            {
                action = Some("done");
            }
            if ui
                .add(egui::Button::new("Cancel").min_size(Vec2::new(90.0, 28.0)))
                .on_hover_text("Discard changes (Esc)")
                .clicked()
            {
                action = Some("cancel");
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(format!("{ICON_SAVE} Save")).min_size(Vec2::new(90.0, 28.0)))
                .clicked()
            {
                action = Some("save");
            }
            if ui
                .add(
                    egui::Button::new(format!("{ICON_PRINT} Print…"))
                        .min_size(Vec2::new(90.0, 28.0)),
                )
                .clicked()
            {
                action = Some("print");
            }
        });
        action
    }

    fn canvas(&mut self, ui: &mut egui::Ui, thirds: bool) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::image_bg());
        if self.before || ui.input(|i| i.key_down(Key::B)) && !ui.ctx().egui_wants_keyboard_input()
        {
            // The untouched original, for comparison.
            let view = rect.shrink(44.0);
            let sz = Vec2::new(self.base.width() as f32, self.base.height() as f32);
            let k = (view.width() / sz.x).min(view.height() / sz.y);
            painter.image(
                self.orig_tex.id(),
                Rect::from_center_size(view.center(), sz * k),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            let r = Rect::from_min_size(rect.min + Vec2::new(12.0, 12.0), Vec2::new(96.0, 26.0));
            painter.rect_filled(r, 13.0, Color32::from_black_alpha(180));
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                format!("{ICON_EYE} Before"),
                FontId::proportional(13.0),
                theme::accent(),
            );
            ui.ctx().request_repaint();
            return;
        }
        let g = self.geo();
        let view = rect.shrink(44.0);
        let s = (view.width() / g.cw).min(view.height() / g.ch);
        let origin = view.center() - Vec2::new(g.cw, g.ch) * s / 2.0;
        let to_screen = |x: f32, y: f32| origin + Vec2::new(x, y) * s;
        let to_canvas = |p: Pos2| ((p - origin) / s).to_pos2();

        // The image, turned by quarter turns + free angle about the canvas centre.
        let (bw, bh) = (self.base.width() as f32 * s, self.base.height() as f32 * s);
        let center = to_screen(g.cw / 2.0, g.ch / 2.0);
        let mut mesh = egui::Mesh::with_texture(self.tex.id());
        mesh.add_rect_with_uv(
            Rect::from_center_size(center, Vec2::new(bw, bh)),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        mesh.rotate(
            egui::emath::Rot2::from_angle(self.params.total_rotation()),
            center,
        );
        painter.add(egui::Shape::mesh(mesh));

        // Darken everything outside the crop.
        let [cx, cy, cw, ch] = self.crop;
        let c = Rect::from_min_max(to_screen(cx, cy), to_screen(cx + cw, cy + ch));
        let shade = Color32::from_black_alpha(170);
        painter.rect_filled(
            Rect::from_min_max(rect.min, Pos2::new(rect.max.x, c.min.y)),
            0.0,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(rect.min.x, c.max.y), rect.max),
            0.0,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(rect.min.x, c.min.y), Pos2::new(c.min.x, c.max.y)),
            0.0,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(c.max.x, c.min.y), Pos2::new(rect.max.x, c.max.y)),
            0.0,
            shade,
        );
        painter.rect_stroke(
            c,
            0.0,
            Stroke::new(1.5, Color32::WHITE),
            egui::StrokeKind::Middle,
        );
        if thirds {
            let st = Stroke::new(1.0, Color32::from_white_alpha(140));
            for k in 1..3 {
                let fx = c.min.x + c.width() * k as f32 / 3.0;
                let fy = c.min.y + c.height() * k as f32 / 3.0;
                painter.line_segment([Pos2::new(fx, c.min.y), Pos2::new(fx, c.max.y)], st);
                painter.line_segment([Pos2::new(c.min.x, fy), Pos2::new(c.max.x, fy)], st);
            }
        }
        let rotating = matches!(self.drag, Some((Handle::Rotate, ..)));
        if rotating {
            // A finer grid helps line up horizons and verticals.
            let st = Stroke::new(1.0, Color32::from_white_alpha(70));
            for k in 1..8 {
                let fx = c.min.x + c.width() * k as f32 / 8.0;
                let fy = c.min.y + c.height() * k as f32 / 8.0;
                painter.line_segment([Pos2::new(fx, c.min.y), Pos2::new(fx, c.max.y)], st);
                painter.line_segment([Pos2::new(c.min.x, fy), Pos2::new(c.max.x, fy)], st);
            }
            painter.text(
                c.center(),
                Align2::CENTER_CENTER,
                format!("{:+.1}°", self.params.angle),
                FontId::proportional(22.0),
                Color32::WHITE,
            );
        }
        for p in [
            c.left_top(),
            c.right_top(),
            c.left_bottom(),
            c.right_bottom(),
        ] {
            painter.rect_filled(
                Rect::from_center_size(p, Vec2::splat(10.0)),
                1.0,
                Color32::WHITE,
            );
        }
        // Rotation knobs just outside each corner.
        let knobs: Vec<Pos2> = [
            c.left_top(),
            c.right_top(),
            c.left_bottom(),
            c.right_bottom(),
        ]
        .iter()
        .map(|&p| p + (p - c.center()).normalized() * 26.0)
        .collect();
        for &k in &knobs {
            painter.circle(
                k,
                10.0,
                Color32::from_black_alpha(160),
                Stroke::new(1.0, Color32::from_white_alpha(200)),
            );
            painter.text(
                k,
                Align2::CENTER_CENTER,
                ICON_ROTATE,
                FontId::proportional(13.0),
                Color32::WHITE,
            );
        }
        // Crop size in full-resolution pixels.
        let (fw, _) = edit::canvas_size(self.full.0, self.full.1, self.params.quarter);
        let k = fw / g.cw;
        let full_crop =
            cx.abs() < 0.5 && cy.abs() < 0.5 && (cw - g.cw).abs() < 0.5 && (ch - g.ch).abs() < 0.5;
        let [px, py] = if full_crop {
            let (a, b) = edit::canvas_size(self.full.0, self.full.1, self.params.quarter);
            [a, b]
        } else {
            [(cw * k).round(), (ch * k).round()]
        };
        painter.text(
            c.center_bottom() + Vec2::new(0.0, 6.0),
            Align2::CENTER_TOP,
            format!("{px:.0} × {py:.0}"),
            FontId::proportional(12.0),
            Color32::LIGHT_GRAY,
        );

        // Interaction
        let hit = |p: Pos2| -> Option<Handle> {
            let r = 12.0;
            if knobs.iter().any(|k| k.distance(p) < 14.0) {
                return Some(Handle::Rotate);
            }
            let near = |a: f32, b: f32| (a - b).abs() < r;
            let free = self.aspect().is_none();
            let (l, rr, t, b) = (
                near(p.x, c.min.x),
                near(p.x, c.max.x),
                near(p.y, c.min.y),
                near(p.y, c.max.y),
            );
            let inx = p.x > c.min.x - r && p.x < c.max.x + r;
            let iny = p.y > c.min.y - r && p.y < c.max.y + r;
            Some(match (l, rr, t, b) {
                (true, _, true, _) => Handle::NW,
                (_, true, true, _) => Handle::NE,
                (true, _, _, true) => Handle::SW,
                (_, true, _, true) => Handle::SE,
                (true, _, _, _) if free && iny => Handle::W,
                (_, true, _, _) if free && iny => Handle::E,
                (_, _, true, _) if free && inx => Handle::N,
                (_, _, _, true) if free && inx => Handle::S,
                _ if c.contains(p) => Handle::Move,
                _ if rect.contains(p) => Handle::Rotate,
                _ => return None,
            })
        };
        if let Some(p) = resp.hover_pos()
            && let Some(h) = self.drag.map(|d| d.0).or_else(|| hit(p))
        {
            ui.ctx().set_cursor_icon(match h {
                Handle::Move => CursorIcon::Grab,
                Handle::N | Handle::S => CursorIcon::ResizeVertical,
                Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
                Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
                Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
                Handle::Rotate => CursorIcon::Alias,
            });
        }
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            self.drag = hit(p).map(|h| (h, self.crop, to_canvas(p), self.params.angle));
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        if let (Some((h, start, p0, a0)), Some(p)) = (self.drag, resp.interact_pointer_pos()) {
            if h == Handle::Rotate {
                // Angle swept around the image centre; hold Shift for fine control.
                let v0 = to_screen(p0.x, p0.y) - center;
                let v1 = p - center;
                if v0.length() > 8.0 && v1.length() > 8.0 {
                    let mut delta = (v1.y.atan2(v1.x) - v0.y.atan2(v0.x)).to_degrees();
                    if delta > 180.0 {
                        delta -= 360.0
                    }
                    if delta < -180.0 {
                        delta += 360.0
                    }
                    if ui.input(|i| i.modifiers.shift) {
                        delta *= 0.2;
                    }
                    let a = ((a0 + delta) * 10.0).round() / 10.0;
                    let a = a.clamp(-45.0, 45.0);
                    if a != self.params.angle {
                        self.params.angle = a;
                        self.refit(false);
                    }
                }
            } else {
                let d = to_canvas(p) - p0;
                let target = self.drag_target(h, start, d);
                self.apply_crop(target);
            }
        }
    }

    fn drag_target(&self, h: Handle, s: [f32; 4], d: Vec2) -> [f32; 4] {
        let [x, y, w, hh] = s;
        let min = 24.0;
        if h == Handle::Move {
            return [x + d.x, y + d.y, w, hh];
        }
        // Edges/corners: move the dragged sides, keep the opposite ones.
        let (mut l, mut t, mut r, mut b) = (x, y, x + w, y + hh);
        match h {
            Handle::W | Handle::NW | Handle::SW => l = (l + d.x).min(r - min),
            Handle::E | Handle::NE | Handle::SE => r = (r + d.x).max(l + min),
            _ => {}
        }
        match h {
            Handle::N | Handle::NW | Handle::NE => t = (t + d.y).min(b - min),
            Handle::S | Handle::SW | Handle::SE => b = (b + d.y).max(t + min),
            _ => {}
        }
        if let Some(a) = self.aspect() {
            let (mut nw, mut nh) = (r - l, b - t);
            if nw / nh > a {
                nw = nh * a
            } else {
                nh = nw / a
            }
            // Anchor at the opposite corner.
            match h {
                Handle::NW => {
                    l = r - nw;
                    t = b - nh;
                }
                Handle::NE => {
                    r = l + nw;
                    t = b - nh;
                }
                Handle::SW => {
                    l = r - nw;
                    b = t + nh;
                }
                _ => {
                    r = l + nw;
                    b = t + nh;
                }
            }
        }
        [l, t, r - l, b - t]
    }

    /// Move the crop toward `target`, as far as it can go while staying on the image.
    fn apply_crop(&mut self, target: [f32; 4]) {
        let g = self.geo();
        self.crop_auto = false;
        if g.rect_inside(target) {
            self.crop = target;
            return;
        }
        let cur = self.crop;
        let lerp =
            |t: f32| -> [f32; 4] { std::array::from_fn(|i| cur[i] + (target[i] - cur[i]) * t) };
        // For a move, slide along each axis separately so it glides along edges.
        if (target[2] - cur[2]).abs() < 1e-3 && (target[3] - cur[3]).abs() < 1e-3 {
            for cand in [
                [target[0], cur[1], cur[2], cur[3]],
                [cur[0], target[1], cur[2], cur[3]],
            ] {
                if g.rect_inside(cand) {
                    self.crop = cand;
                    return;
                }
            }
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..16 {
            let m = (lo + hi) / 2.0;
            if g.rect_inside(lerp(m)) {
                lo = m
            } else {
                hi = m
            }
        }
        self.crop = lerp(lo);
    }
}

fn resolve_start(start: Option<PathBuf>) -> (PathBuf, Vec<PathBuf>, usize) {
    let start = start.map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    match start {
        Some(p) if p.is_file() => {
            let dir = p
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let mut list = files::list_dir(&dir);
            let idx = match list.iter().position(|f| *f == p) {
                Some(i) => i,
                None => {
                    list.insert(0, p);
                    0
                }
            };
            (dir, list, idx)
        }
        Some(d) if d.is_dir() => {
            let list = files::list_dir(&d);
            (d, list, 0)
        }
        _ => {
            let d = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let list = files::list_dir(&d);
            (d, list, 0)
        }
    }
}

#[cfg(feature = "demo")]
impl App {
    /// A timed tour of the main features, for recording the README demo.
    fn demo_step(&mut self, ctx: &egui::Context) {
        // GLANCE_DEMO=zoom: just zoom to 6× after a second (to check full-resolution loading).
        if std::env::var("GLANCE_DEMO").as_deref() == Ok("zoom") {
            if self.demo.1 == 0 && self.demo.0.elapsed().as_secs_f32() > 1.0 {
                self.zoom = 6.0;
                self.demo.1 = 1;
            }
            ctx.request_repaint();
            return;
        }
        // (time in seconds, event)
        const EVENTS: &[f32] = &[
            2.0, 3.2, 4.4, 5.6, 7.4, 8.0, 9.0, 10.0, 14.6, 15.8, 16.6, 18.2, 21.6, 22.4,
        ];
        ctx.request_repaint();
        let t = self.demo.0.elapsed().as_secs_f32();
        let ease = |a: f32, b: f32| ((t - a) / (b - a)).clamp(0.0, 1.0);
        // Continuous moves while editing: level the horizon, then brighten.
        if let Some(st) = self.editing.as_mut() {
            if (10.4..=12.8).contains(&t) {
                // Swing the rotation out and back to level, with the alignment grid showing.
                let k = ease(10.4, 12.4);
                st.params.angle = (2.0 * (std::f32::consts::PI * k).sin() * 10.0).round() / 10.0;
                st.drag =
                    (k < 1.0).then_some((Handle::Rotate, st.crop, Pos2::ZERO, st.params.angle));
                st.refit(false);
            }
            if (12.8..=14.4).contains(&t) {
                let k = ease(12.8, 14.2);
                st.params.exposure = (0.45 * k * 20.0).round() / 20.0;
                st.params.gamma = 1.0 + 0.08 * k;
            }
        }
        let i = self.demo.1;
        if i >= EVENTS.len() || t < EVENTS[i] {
            return;
        }
        let n = self.files.len().max(1);
        match i {
            0 | 1 => self.go((self.idx + 1) % n),
            2 => self.go((self.idx + n - 1) % n),
            3 => self.info_open = true,
            4 => self.info_open = false,
            5 => self.toggle_queue(),
            6 => self.start_edit(),
            7 => {
                // Crop to 10x15 (2:3), landscape.
                if let Some(st) = self.editing.as_mut() {
                    st.ratio = 3;
                    st.ratio_portrait = false;
                    st.crop_auto = true;
                    st.refit(false);
                }
            }
            8 => {
                if let Some(st) = self.editing.as_mut() {
                    st.hold_before = true;
                }
            }
            9 => {
                if let Some(st) = self.editing.as_mut() {
                    st.hold_before = false;
                }
            }
            10 => {
                self.commit_edit();
            }
            11 => {
                if let Some(p) = self.current().cloned() {
                    self.open_print(vec![p]);
                }
            }
            12 => self.printing = None,
            _ => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
        self.demo.1 += 1;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_messages();
        #[cfg(feature = "demo")]
        self.demo_step(&ctx);

        let title = match self.current() {
            Some(p) => format!("{} — glance", files::file_name(p)),
            None => "glance".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        if self.editing.is_some() {
            egui::Panel::top("edit-top").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Editing");
                    if let Some(st) = &self.editing {
                        ui.label(files::file_name(&st.path));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak("Ctrl+Z undo · Hold B to compare · Enter done · Esc cancel");
                    });
                });
            });
            self.edit_ui(ui);
        } else {
            self.keys_browse(&ctx);
            egui::Panel::top("top").show(ui, |ui| self.toolbar(ui));
            egui::Panel::bottom("status").show(ui, |ui| self.statusbar(ui));
            if self.info_open {
                egui::Panel::right("info")
                    .exact_size(320.0)
                    .show(ui, |ui| self.info_panel(ui));
            } else if self.queue_open {
                egui::Panel::right("queue")
                    .exact_size(320.0)
                    .show(ui, |ui| self.queue_panel(ui));
            }
            egui::CentralPanel::default().show(ui, |ui| self.view(ui));
        }
        self.print_window(&ctx);
        self.raw_window(&ctx);
        if self.prefs_open {
            self.prefs_window(&ctx);
        }
        if let Some((_, t)) = &self.status
            && t.elapsed() < Duration::from_secs(9)
        {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}

fn main() -> eframe::Result {
    let start = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("glance")
            .with_title("glance")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([640.0, 420.0])
            .with_icon(egui::IconData {
                rgba: logo::rgba(128),
                width: 128,
                height: 128,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "glance",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, start)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Folder with sample photos (JPEG and RAW) for the tests that need real files:
    /// `GLANCE_TEST_PHOTOS=/path/to/photos cargo test`. Those tests are skipped otherwise.
    pub fn test_photos() -> Option<PathBuf> {
        std::env::var_os("GLANCE_TEST_PHOTOS").map(PathBuf::from)
    }

    fn sheet(paper: (f32, f32), scale: Scale) -> Sheet {
        Sheet {
            paper,
            margin: 3.0,
            scale,
            orient: Orient::Auto,
        }
    }

    #[test]
    fn fit_keeps_shape_on_10x15() {
        // 4:3 portrait photo on 10x15 paper with 3 mm margins.
        let cell = print::area(&sheet((101.6, 152.4), Scale::Fit));
        let p = print::place_in(3000.0, 4000.0, cell, Scale::Fit, Orient::Auto);
        assert!(!p.rotate);
        assert!((p.dest[2] / p.dest[3] - 0.75).abs() < 1e-3);
        let p = print::place_in(3000.0, 4000.0, cell, Scale::Fill, Orient::Auto);
        let used = (3000.0 * p.src[2]) / (4000.0 * p.src[3]);
        assert!(
            (used - p.dest[2] / p.dest[3]).abs() < 1e-3,
            "fill crop matches area shape"
        );
    }

    #[test]
    fn landscape_photo_turned_into_cell() {
        // A landscape photo in a portrait whole-sheet slot gets turned, and the page stays portrait.
        let s = sheet((101.6, 152.4), Scale::Fill);
        let cell = print::area(&s);
        let photo = RgbaImage::from_pixel(1500, 1000, image::Rgba([10, 20, 30, 255]));
        assert!(print::place_in(1500.0, 1000.0, cell, Scale::Fill, Orient::Auto).rotate);
        let px = |mm: f32| (mm / 25.4 * print::DPI).round() as u32;
        let mut page =
            image::RgbImage::from_pixel(px(101.6), px(152.4), image::Rgb([255, 255, 255]));
        print::draw_into(&mut page, &photo, cell, Scale::Fill, Orient::Auto);
        let mid = page.get_pixel(page.width() / 2, page.height() / 2).0;
        assert_eq!(mid, [10, 20, 30], "photo covers the middle of the sheet");
    }

    #[test]
    fn quarter_turn_moves_corner() {
        let mut img = RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255])); // top-left red
        let p = EditParams {
            quarter: 1,
            ..Default::default()
        };
        let out = edit::render(&img, &p);
        assert_eq!((out.width(), out.height()), (2, 4));
        assert_eq!(
            out.get_pixel(1, 0).0,
            [255, 0, 0, 255],
            "top-left goes to top-right after 90° clockwise"
        );
    }

    #[test]
    fn rotated_crop_stays_on_image() {
        let g = Geo {
            cw: 4000.0,
            ch: 3000.0,
            angle: 10f32.to_radians(),
        };
        let r = g.max_rect(4.0 / 3.0);
        assert!(g.rect_inside(r));
        assert!(r[2] < 4000.0 && r[2] > 3000.0);
        assert!(!g.rect_inside([0.0, 0.0, 4000.0, 3000.0]));
    }

    #[test]
    fn lut_identity() {
        let t = edit::lut(0.0, 1.0);
        assert!(
            t.iter()
                .enumerate()
                .all(|(i, &v)| (v as i32 - i as i32).abs() <= 1)
        );
    }

    #[test]
    fn orf_preview_loads() {
        let Some(dir) = test_photos() else { return };
        let Some(orf) = files::list_dir(&dir).into_iter().find(|p| files::is_raw(p)) else {
            return;
        };
        let l = files::load(&orf, Some(1000)).expect("embedded preview");
        assert!(
            l.full_w >= 1000 && l.full_h >= 700,
            "got {}x{}",
            l.full_w,
            l.full_h
        );
    }
}

#[cfg(test)]
mod info_tests {
    #[test]
    fn exif_from_jpeg_and_orf() {
        let Some(dir) = crate::tests::test_photos() else {
            return;
        };
        let list = crate::files::list_dir(&dir);
        for p in list
            .iter()
            .filter(|p| !crate::files::is_raw(p))
            .take(1)
            .chain(list.iter().filter(|p| crate::files::is_raw(p)).take(1))
        {
            let inf = crate::info::read(p, (1, 1), 0);
            println!("== {}", p.display());
            for (t, rows) in &inf.sections {
                for (k, v) in rows {
                    println!("{t:7} {k:15} {v}");
                }
            }
        }
    }
}
