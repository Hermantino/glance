# Changelog

All notable changes to glance are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- HEIC / HEIF / AVIF photos, including Fujifilm `.HIF`, decoded with the system's libheif when it's installed. Edits to them are saved as a JPEG copy.
- Full-resolution zoom: zooming in past the display copy loads the photo at full size in the background (e.g. 7728 px for a 40 MP Fujifilm photo, instead of 3840 px).
- Zoom readout: the status bar shows the zoom as a percentage of the photo's real size (100% = one photo pixel per screen pixel), and a label over the photo shows it while zooming, with "loading full resolution…" and "✓ full resolution" as the full-size photo arrives.
- On Omarchy, glance follows the active theme live: accent, background and corner rounding. Elsewhere it keeps its own amber look.
- Undo and redo in Edit (Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y, and buttons). A whole slider or crop drag undoes in one step.

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

[Unreleased]: https://github.com/Hermantino/glance/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Hermantino/glance/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/Hermantino/glance/releases/tag/v0.1.0
