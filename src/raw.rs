//! Finding and launching an installed RAW developer. glance itself never develops RAW.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Known RAW developers: display name and the executables that start them.
const DEVELOPERS: &[(&str, &[&str])] = &[
    ("darktable", &["darktable"]),
    ("RapidRAW", &["RapidRAW", "rapidraw"]),
    ("RawTherapee", &["rawtherapee"]),
    ("ART", &["ART", "art"]),
    ("Ansel", &["ansel"]),
    ("digiKam", &["digikam"]),
    ("Adobe Lightroom", &["lightroom"]),
    ("Capture One", &["captureone", "CaptureOne"]),
];

fn find_in_path(bin: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Installed developers as (name, executable).
pub fn installed() -> Vec<(String, PathBuf)> {
    DEVELOPERS
        .iter()
        .filter_map(|(name, bins)| {
            bins.iter()
                .find_map(|b| find_in_path(b))
                .map(|p| (name.to_string(), p))
        })
        .collect()
}

/// What to suggest when nothing is installed: (name, how to get it).
pub fn suggestions() -> &'static [(&'static str, &'static str)] {
    if cfg!(target_os = "linux") {
        &[
            (
                "darktable",
                "sudo pacman -S darktable  (or your distribution's package manager)",
            ),
            ("RawTherapee", "sudo pacman -S rawtherapee"),
        ]
    } else if cfg!(target_os = "macos") {
        &[
            ("darktable", "https://www.darktable.org/install/"),
            (
                "Adobe Lightroom Classic",
                "https://www.adobe.com/products/photoshop-lightroom-classic.html",
            ),
        ]
    } else {
        &[
            ("darktable", "https://www.darktable.org/install/"),
            ("RawTherapee", "https://rawtherapee.com/downloads/"),
        ]
    }
}

pub fn launch(exe: &Path, file: &Path) -> Result<(), String> {
    let mut child = Command::new(exe)
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    // Reap it when it exits so it doesn't linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
