# Changelog

All notable changes to glance are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Arch Linux `PKGBUILD` in `packaging/aur/` (package `glance-viewer`).

## [0.1.0] - 2026-11-15

First public release.

### Added
- Browse every photo in a folder with the arrow keys; neighbouring photos are preloaded. Zoom, pan, fullscreen.
- Quick edits: crop with print ratio presets, free rotation (slider or drag a corner knob), rule-of-thirds guides, exposure and gamma, before/after comparison. Edits stay non-destructive until saved; saving makes a copy by default.
- Print dialog built for photo paper: any CUPS printer or preset, paper size, feeder, paper type, ink levels, fill or fit, orientation, margins, copies, and a true-to-size preview. Pages are composed at the exact paper size, so photos are never stretched. Remembers the options last used with each printer.
- Print queue: mark photos with Space while browsing and print them all in one job.
- Photo details panel: file facts plus camera, lens, exposure, date and location from EXIF, including RAW files.
- RAW files show the camera's embedded preview; "Develop RAW…" opens them in an installed RAW developer.

[Unreleased]: https://github.com/Hermantino/glance/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Hermantino/glance/releases/tag/v0.1.0
