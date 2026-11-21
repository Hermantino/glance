# Changelog

All notable changes to glance are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.4] - 2026-11-21

### Added
- Drag the photo out of glance: drop it on a file manager to copy the file there, or on a terminal to type its path. Works when the photo fits the window (zoomed in, dragging still pans). Wayland only for now.

### Changed
- Print queue: with the queue panel open, the photo you're viewing is outlined in the list, and the list scrolls to it as you browse.

## [0.1.3] - 2026-11-16

### Changed
- Faster browsing, especially from an SD card: photos are shrunk to the display copy on all CPU cores (about 10× faster, and slightly more accurate), files are read one at a time so the photo you're looking at isn't slowed down by preloading, photos you've already browsed past are skipped instead of loaded, and preloading looks three photos ahead in the direction you're going.
- Fujifilm RAF files read only their embedded preview (about 6 MB of a 45 MB file), so they open several times faster.
- The details panel reads camera information in the background with the photo, so it no longer freezes glance briefly on slow storage.

## [0.1.2] - 2026-11-16

### Added
- HEIC / HEIF / AVIF photos, including Fujifilm `.HIF`, decoded with the system's libheif when it's installed. Edits to them are saved as a JPEG copy.
- Full-resolution zoom: zooming in loads the photo at full size in the background (e.g. 7728 px for a 40 MP Fujifilm photo, instead of the 3840 px display copy), with the edits applied for edited photos.
- Zoom readout and presets: the status bar shows the zoom as a percentage of the photo's real size and whether full resolution is loaded; click it for Fit, 50%, 100% or 200%, or press 0 / 1 / 2.
- On Omarchy, glance follows the active theme live: accent, background and corner rounding. Elsewhere it keeps its own amber look.
- Undo and redo in Edit (Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y, and buttons). A whole slider or crop drag undoes in one step.
- `glance --version` and `glance --help`; the version is also shown in Preferences.
- Preferences say whether exiftool is installed, and saving says so when camera information couldn't be kept.

### Fixed
- Straightened photos no longer get thin white slivers along the edges when saved or printed.
- Portrait Fujifilm RAF photos are turned upright, and their camera details are shown (RAF files keep both in the embedded preview, not in a TIFF header).
- Saving keeps the photo's colour profile (e.g. Adobe RGB).
- "Overwrite, keep the original": the backup is written safely, and a different photo with the same name gets its own backup instead of being skipped.
- Saving a photo again while the previous save is still running is refused instead of racing on the same file.
- After overwriting a photo, the view, print queue thumbnail and details show the new version.
- Borderless prints start at the very edge of the paper (they were shifted by one pixel).
- The print dialog explains what's wrong when no printer is found, when a printer's options can't be read, or when a paper size isn't recognised. More paper size names are recognised (e.g. `iso_a4_210x297mm`, `na_index-4x6_4x6in`, Tabloid, A3+).
- Graphics memory no longer grows with every edited photo you browse past.
- The "edited" marker in the toolbar showed an empty box.
- The "full resolution" label no longer flashes on the next photo.
- A garbled date in a photo's EXIF, or a printer option without values, can no longer crash glance.
- The print queue panel no longer mentions several photos per sheet.

## [0.1.1] - 2026-11-15

### Added
- Arch Linux `PKGBUILD` in `packaging/aur/` (package `glance-viewer`).
- Ready-to-run Linux download attached to each release.
- README: why glance exists, demo GIF, roadmap link.

### Fixed
- The print dialog froze glance for about 2 seconds while CUPS listed printers and their options. Printers are now read in the background at startup and remembered, so the dialog opens instantly.

## [0.1.0] - 2026-11-15

First public release.

### Added
- Browse every photo in a folder with the arrow keys; neighbouring photos are preloaded. Zoom, pan, fullscreen.
- Quick edits: crop with print ratio presets, free rotation (slider or drag a corner knob), rule-of-thirds guides, exposure and gamma, before/after comparison. Edits stay non-destructive until saved; saving makes a copy by default.
- Print dialog built for photo paper: any CUPS printer or preset, paper size, feeder, paper type, ink levels, fill or fit, orientation, margins, copies, and a true-to-size preview. Pages are composed at the exact paper size, so photos are never stretched. Remembers the options last used with each printer.
- Print queue: mark photos with Space while browsing and print them all in one job.
- Photo details panel: file facts plus camera, lens, exposure, date and location from EXIF, including RAW files.
- RAW files show the camera's embedded preview; "Develop RAW…" opens them in an installed RAW developer.

[Unreleased]: https://github.com/Hermantino/glance/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/Hermantino/glance/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/Hermantino/glance/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/Hermantino/glance/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/Hermantino/glance/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Hermantino/glance/releases/tag/v0.1.0
