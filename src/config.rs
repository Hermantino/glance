//! User preferences, stored in ~/.config/glance/config.toml.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug, Default)]
pub enum SaveMode {
    /// Write `name-edit.jpg` next to the original; the original is never touched.
    #[default]
    Copy,
    /// Replace the original file.
    Overwrite,
    /// Replace the original, but first keep it in a `.originals` folder next to it.
    OverwriteBackup,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub save_mode: SaveMode,
    pub jpeg_quality: u8,
    /// Name of the RAW developer to launch without asking (None = ask each time).
    pub raw_developer: Option<String>,
    pub last_printer: Option<String>,
    pub thirds: bool,
    /// Printer options last used with each printer or preset (e.g. paper type, feeder).
    pub print_options: BTreeMap<String, BTreeMap<String, String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            save_mode: SaveMode::Copy,
            jpeg_quality: 95,
            raw_developer: None,
            last_printer: None,
            thirds: true,
            print_options: BTreeMap::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("glance").join("config.toml"))
}

impl Config {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}
