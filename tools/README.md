# Backend tools

These are **compiled into `inat-gui.exe`**, not shipped beside it. They must be
here before building, or the build will not link:

- `yt-dlp.exe`
- `ffmpeg.exe`

That is the whole list. `ffprobe` is not needed: yt-dlp does everything INAT
asks of it - merging, remuxing, extracting audio, embedding cover art - with
ffmpeg alone, and bundling ffprobe as well would have doubled the size of the
program for nothing.

On first run the window writes both into `%LOCALAPPDATA%\INAT\tools`, a hidden
folder, and runs them from there. Nothing is ever written beside the executable.

yt-dlp updates itself from that folder, through the **Update** button in the
window. Once it has, INAT stops putting its own copy back over it.

`INAT_YTDLP` and `INAT_FFMPEG` still override both, if you want to point a
development build at your own binaries.
