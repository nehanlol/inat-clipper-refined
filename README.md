<div align="center">

<img src="assets/inat.png" width="96" height="96" alt="INAT Logo" />

# INAT

**A clean, portable media downloader and clipper.**  
Powered by [yt-dlp](https://github.com/yt-dlp/yt-dlp) and [FFmpeg](https://ffmpeg.org/).

[![Platform](https://img.shields.io/badge/platform-Windows-0d9488.svg?style=flat-square)](https://github.com/stsuprim/inat-clipper/releases/latest)
[![Rust](https://img.shields.io/badge/built_with-Rust_2024-22c55e.svg?style=flat-square)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square)](LICENSE)

[**Download for Windows**](https://github.com/nehanlol/inat-clipper-refined/releases/download/v1.0.0/INAT_Setup_v1.0.0.exe) · Single file · Zero configuration

</div>

---

## Highlights

- **Zero Setup**: Portable `.exe` with `yt-dlp` and `FFmpeg` bundled inside. Nothing else to install.
- **Universal Support**: Works with YouTube, Twitch, Kick, Twitter/X, TikTok, SoundCloud, and [1000+ sites](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md).
- **Fast Precision Clipper**: Snip any section of a video directly with quick chips (`+15s`, `+30s`, `+1m`) without downloading the full file first.
- **Video & Audio**: Up to 4K / 60 FPS video, or clean audio extraction (MP3, AAC, OPUS, FLAC) with embedded thumbnail covers.
- **Queue & Resume**: Concurrent downloads with live speed/ETA and persistent pause/resume—even after closing the app.
- **In-App Updater**: One-click updates for yt-dlp directly inside the window when site extractors change.

---

## Download

Grab the latest build from [**Releases**](https://github.com/nehanlol/inat-clipper-refined/releases/tag/v1.0.0):

| File | Type | Description |
| :--- | :--- | :--- |
| **`INAT.exe`** | Portable | Just double-click to run anywhere. No installer needed. |
| **`INAT_Setup_v1.0.0.exe`** | Installer | Compact setup with Desktop & Start Menu shortcuts. |

---

## Build from Source

Prerequisites: Rust (2024 edition) and backend binaries placed in `tools/`:

```bash
# 1. Place yt-dlp.exe and ffmpeg.exe into the tools/ directory
tools/yt-dlp.exe
tools/ffmpeg.exe

# 2. Compile release binary
cargo build --release --bin inat-gui
```

The output executable is created at `target/release/inat-gui.exe`.
