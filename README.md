# INAT

A simple frontend GUI for [yt-dlp](https://github.com/yt-dlp/yt-dlp).

Paste a link, pick video, audio or a clip, download.

## Download

### **[Download for Windows](https://github.com/stsuprim/inat-clipper/releases/latest)**

One file. Nothing to install — yt-dlp and FFmpeg are built into it.

## What it does

- Video, audio, or a clip cut out of any part of a video
- Several downloads at once
- Pause and resume, including after closing the window
- Retries a download that fails
- Updates yt-dlp from inside the app

## Build

Needs `tools/yt-dlp.exe` and `tools/ffmpeg.exe` present — they are compiled into
the executable.

```
cargo build --release
```

`target/release/inat-gui.exe` is the app. `inat.exe` is an optional terminal
front end over the same core.
