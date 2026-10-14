use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::settings::{Settings, work_dir};
use crate::tools::Tools;

/// Enough to keep two jobs started in the same moment apart.
pub fn unique_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0)
}

pub const PROGRESS_SENTINEL: &str = "@@INAT@@";
pub const PROGRESS_TEMPLATE: &str = concat!(
    "download:@@INAT@@%(progress.status)s|%(progress.downloaded_bytes)s|",
    "%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|",
    "%(progress.eta)s|%(progress.fragment_index)s|%(progress.fragment_count)s|",
    "%(info.playlist_index)s|%(info.n_entries)s|%(info.title)s"
);
pub const PROGRESS_FIELDS: usize = 11;

/// How long the title part of a filename may get. Windows caps one name at 255
/// characters, and the folder it sits in eats into that, so a long caption has
/// to be cut somewhere. Long enough to still recognize the video by.
pub const FILENAME_LIMIT: usize = 120;

pub const VIDEO_FORMATS: &[&str] = &["mp4", "mkv", "webm"];
pub const AUDIO_FORMATS: &[&str] = &["best", "mp3", "m4a", "opus", "flac", "wav"];
pub const QUALITY_CHOICES: &[&str] = &["best", "2160", "1440", "1080", "720", "480", "360"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaMode {
    Download,
    Clip,
    Audio,
}

impl MediaMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Download => "VIDEO",
            Self::Clip => "CLIP",
            Self::Audio => "AUDIO",
        }
    }

    /// How the mode is written down when a job is saved to disk.
    pub fn key(self) -> &'static str {
        match self {
            Self::Download => "video",
            Self::Clip => "clip",
            Self::Audio => "audio",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "video" | "download" => Some(Self::Download),
            "clip" => Some(Self::Clip),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }

    pub fn formats(self) -> &'static [&'static str] {
        match self {
            Self::Audio => AUDIO_FORMATS,
            _ => VIDEO_FORMATS,
        }
    }

    pub fn default_format(self) -> &'static str {
        self.formats()[0]
    }

    /// How a finished file is named. A video keeps just its title; audio is
    /// tagged so it does not collide with the video of the same name; a clip
    /// starts from the plain title and has its time range appended in
    /// [`JobSpec::plan`], once the range has been validated.
    pub fn default_filename(self) -> &'static str {
        match self {
            Self::Download | Self::Clip => "%(title)s.%(ext)s",
            Self::Audio => "%(title)s [AUDIO].%(ext)s",
        }
    }
}

/// Everything one download needs, independent of who filled it in. The CLI
/// builds this from argv, the GUI builds it from its form.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub mode: MediaMode,
    pub url: String,
    pub quality: String,
    pub format: String,
    pub audio_quality: String,
    pub output: Option<PathBuf>,
    pub filename: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub accurate: bool,
    pub playlist: bool,
    pub overwrite: bool,
}

impl JobSpec {
    /// Defaults for `mode`, seeded from the saved settings.
    pub fn new(mode: MediaMode, url: impl Into<String>, settings: &Settings) -> Self {
        Self {
            mode,
            url: normalize_url(&url.into()),
            quality: settings.quality.clone(),
            format: mode.default_format().to_owned(),
            audio_quality: "0".to_owned(),
            output: Some(settings.folder.clone()),
            filename: mode.default_filename().to_owned(),
            start: None,
            end: None,
            accurate: false,
            playlist: false,
            overwrite: false,
        }
    }

    /// `Ok(None)` means `--help` was asked for and the caller should print it.
    pub fn from_args(args: Vec<OsString>, mode: MediaMode) -> Result<Option<Self>, AppError> {
        let settings = Settings::load();
        let mut spec = Self::new(mode, "", &settings);
        let mut url = None;

        let mut iter = args.into_iter();
        while let Some(raw) = iter.next() {
            let arg = raw.to_string_lossy();
            match arg.as_ref() {
                "--quality" | "-q" => spec.quality = next_value(&mut iter, &arg)?,
                "--format" | "-f" => spec.format = next_value(&mut iter, &arg)?,
                "--audio-quality" => spec.audio_quality = next_value(&mut iter, &arg)?,
                "--output" | "-o" => spec.output = Some(PathBuf::from(next_value(&mut iter, &arg)?)),
                "--filename" => spec.filename = next_value(&mut iter, &arg)?,
                "--from" | "--start" | "-s" => spec.start = Some(next_value(&mut iter, &arg)?),
                "--to" | "--end" | "-e" => spec.end = Some(next_value(&mut iter, &arg)?),
                "--accurate" => spec.accurate = true,
                "--playlist" => spec.playlist = true,
                "--overwrite" | "--force" => spec.overwrite = true,
                "--help" | "-h" => return Ok(None),
                _ if arg.starts_with('-') => {
                    return Err(AppError::usage(format!("unknown option '{arg}'")));
                }
                _ if url.is_none() => url = Some(arg.into_owned()),
                _ => return Err(AppError::usage("only one URL may be provided")),
            }
        }

        spec.url = normalize_url(&url.ok_or_else(|| AppError::usage("a URL is required"))?);
        spec.validate()?;
        Ok(Some(spec))
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.url.is_empty() {
            return Err(AppError::usage("a URL is required"));
        }
        if !self.playlist
            && let Some(reason) = bulk_url_reason(&self.url)
        {
            return Err(AppError::usage(reason));
        }
        validate_format(self.mode, &self.format)?;
        validate_quality(&self.quality)?;
        validate_filename(&self.filename)?;
        Ok(())
    }

    /// Turns the spec into the exact command line to run. Consumes `self`
    /// because a clip rewrites its own filename template on the way through.
    pub fn plan(mut self, tools: &Tools) -> Result<Plan, AppError> {
        self.validate()?;
        let quality = self.quality.clone();
        let format = self.format.clone();
        let url = self.url.clone();
        // Where the site's own cover image lands. Its own folder per job, so
        // the picture that shows up in it can only belong to this download.
        let thumbs = work_dir().join(format!("thumb-{}", unique_suffix()));
        let thumbs = fs::create_dir_all(&thumbs).is_ok().then_some(thumbs);

        match self.mode {
            MediaMode::Download => {
                tools.require_ffmpeg("best-quality video merging and MP4 output")?;
                let mut args = self.common_args(tools, thumbs.as_deref())?;
                add_video_quality(&mut args, &quality);
                push_container(&mut args, &format);
                args.push(url.into());
                let mut plan = Plan::new(tools.ytdlp.clone(), args, None);
                plan.home = self.output.clone();
                plan.poster = tools.ffmpeg.clone();
                plan.thumbs = thumbs;
                Ok(plan)
            }
            MediaMode::Clip => {
                let start = self
                    .start
                    .clone()
                    .ok_or_else(|| AppError::usage("clip requires a start timestamp"))?;
                let range = ClipRange::new(&start, self.end.as_deref())?;
                let ffmpeg = tools
                    .ffmpeg
                    .clone()
                    .ok_or_else(|| AppError::runtime(
                        "FFmpeg is required for clipping. Put ffmpeg beside yt-dlp in the tools folder.",
                    ))?;
                let accurate = self.accurate;

                let folder = self.output.clone().unwrap_or_else(|| PathBuf::from("."));
                fs::create_dir_all(&folder).map_err(|error| {
                    AppError::runtime(format!(
                        "could not create download folder '{}': {error}",
                        folder.display()
                    ))
                })?;

                // Embed the clip range in the filename so the clip is labelled on disk
                let label = range.label();
                self.filename = format!("%(title)s [{label}].%(ext)s");

                let mut args = self.common_args(tools, thumbs.as_deref())?;
                args.push("--download-sections".into());
                args.push(range.section().into());
                if accurate {
                    args.push("--force-keyframes-at-cuts".into());
                }
                add_video_quality(&mut args, &quality);
                push_container(&mut args, &format);
                args.push(url.into());

                let mut plan = Plan::new(tools.ytdlp.clone(), args, range.duration());
                plan.home = Some(folder);
                plan.poster = Some(ffmpeg);
                plan.thumbs = thumbs;
                Ok(plan)
            }
            MediaMode::Audio => {
                tools.require_ffmpeg("audio conversion")?;
                let audio_quality = self.audio_quality.clone();

                let mut args = self.common_args(tools, thumbs.as_deref())?;
                args.extend([
                    "--format".into(),
                    "ba/b".into(),
                    "--extract-audio".into(),
                    "--audio-format".into(),
                    format.clone().into(),
                    "--audio-quality".into(),
                    audio_quality.into(),
                ]);
                // The cover art, written into the file itself. It is what a
                // music player shows, and it is also the only picture an audio
                // download has to offer the list it lands in.
                if embeds_cover_art(&format) {
                    args.push("--embed-thumbnail".into());
                }
                args.push(url.into());

                let mut plan = Plan::new(tools.ytdlp.clone(), args, None);
                plan.home = self.output.clone();
                plan.poster = tools.ffmpeg.clone();
                plan.thumbs = thumbs;
                Ok(plan)
            }
        }
    }

    fn common_args(&self, tools: &Tools, thumbs: Option<&Path>) -> Result<Vec<OsString>, AppError> {
        let mut args: Vec<OsString> = vec![
            "--newline".into(),
            "--progress".into(),
            "--no-warnings".into(),
            "--retries".into(),
            "3".into(),
            "--fragment-retries".into(),
            "3".into(),
            "--progress-template".into(),
            PROGRESS_TEMPLATE.into(),
            "--windows-filenames".into(),
            // A TikTok or Instagram "title" is the whole caption, hashtags and
            // all, which routinely runs past the 255 characters Windows allows
            // in one name. The file then cannot be created at all, and yt-dlp
            // reports it as an unhelpful "[Errno 22] Invalid argument".
            "--trim-filenames".into(),
            FILENAME_LIMIT.to_string().into(),
            // Captions often start or end with a space or a newline, which
            // would otherwise survive into the filename.
            "--replace-in-metadata".into(),
            "title".into(),
            r"^\s+|\s+$".into(),
            "".into(),
            "--output".into(),
            self.filename.clone().into(),
        ];

        // Skipping an existing file is the default; overwrite opts back in.
        if self.overwrite {
            args.push("--force-overwrites".into());
        } else {
            args.push("--no-overwrites".into());
        }

        if !self.playlist {
            // --no-playlist only helps when a video URL merely *belongs* to a
            // playlist. Feed-style URLs (a TikTok profile, a channel) resolve to
            // a pure playlist, so cap the item count as well.
            args.push("--no-playlist".into());
            args.push("--playlist-items".into());
            args.push("1".into());
        }
        if let Some(output) = &self.output {
            fs::create_dir_all(output).map_err(|error| {
                AppError::runtime(format!(
                    "could not create download folder '{}': {error}",
                    output.display()
                ))
            })?;
            args.push("--paths".into());
            args.push(output.as_os_str().to_owned());
        }

        // Everything half-finished is written somewhere else entirely, so the
        // download folder only ever gains the file the user asked for.
        let work = work_dir();
        if fs::create_dir_all(&work).is_ok() {
            let mut temp = OsString::from("temp:");
            temp.push(work.as_os_str());
            args.push("--paths".into());
            args.push(temp);
        }

        // The site's own cover image, fetched before a byte of video moves.
        // It is what puts a picture on the card while the download is still
        // running, instead of a grey box until it finishes. It goes to its own
        // folder under the working directory, so the download folder never
        // sees it - the temp path above means it is not even written there on
        // its way past.
        if let Some(thumbs) = thumbs {
            let mut path = OsString::from("thumbnail:");
            path.push(thumbs.as_os_str());
            args.push("--write-thumbnail".into());
            args.push("--paths".into());
            args.push(path);
        }
        if let Some(ffmpeg) = &tools.ffmpeg
            && let Some(directory) = ffmpeg.parent()
        {
            args.push("--ffmpeg-location".into());
            args.push(directory.as_os_str().to_owned());
        }
        Ok(args)
    }
}

/// A resolved command line, ready to hand to the runner.
#[derive(Clone, Debug)]
pub struct Plan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// How long the finished file should be, when that is known up front. Only
    /// a clip knows, and it is what turns ffmpeg's timecode into a percentage.
    pub clip_seconds: Option<f64>,
    /// A second pass to run once the download lands, cutting the range out of
    /// the file that just arrived.
    pub cut: Option<Cut>,
    /// Where finished files land. yt-dlp reports the path it *wrote*, which is
    /// inside the working folder, so this is what turns that into the path the
    /// file was moved to.
    pub home: Option<PathBuf>,
    /// Where FFmpeg lives, when a picture of the result is worth grabbing to
    /// show beside it.
    pub poster: Option<PathBuf>,
    /// Where the site's own cover image is dropped, watched while the download
    /// runs so the picture arrives early. Removed once the job is over.
    pub thumbs: Option<PathBuf>,
}

impl Plan {
    fn new(program: PathBuf, args: Vec<OsString>, clip_seconds: Option<f64>) -> Self {
        Self {
            program,
            args,
            clip_seconds,
            cut: None,
            home: None,
            poster: None,
            thumbs: None,
        }
    }
}

/// Everything the local cut needs. The runner fills in the input path, which
/// is only known once yt-dlp says where it put the file.
#[derive(Clone, Debug)]
pub struct Cut {
    pub ffmpeg: PathBuf,
    /// Where the finished clip belongs.
    pub folder: PathBuf,
    /// Where the whole video was parked; removed once the cut succeeds.
    pub scratch: PathBuf,
    /// The `01.20-03.45` fragment appended to the clip's name.
    pub label: String,
    pub extension: String,
    pub range: ClipRange,
    pub accurate: bool,
}

impl Cut {
    /// Where the finished clip goes: the downloaded file's name with the time
    /// range appended, in the folder the user actually chose.
    pub fn target(&self, source: &Path) -> PathBuf {
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "clip".to_owned());
        self.folder
            .join(format!("{stem} [{}].{}", self.label, self.extension))
    }

    /// A stream copy lands on the nearest keyframe and costs almost nothing.
    /// `accurate` re-encodes instead, so the range starts exactly where asked.
    pub fn args(&self, source: &Path, target: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["-hide_banner".into(), "-y".into()];

        if self.accurate {
            // Seeking after the input decodes up to the cut, which is what
            // makes it frame-exact.
            args.push("-i".into());
            args.push(source.as_os_str().to_owned());
            args.push("-ss".into());
            args.push(self.range.start.to_string().into());
            if let Some(end) = self.range.end {
                args.push("-to".into());
                args.push(end.to_string().into());
            }
            args.extend([
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                "veryfast".into(),
                "-crf".into(),
                "18".into(),
                "-c:a".into(),
                "aac".into(),
            ]);
        } else {
            args.push("-ss".into());
            args.push(self.range.start.to_string().into());
            if let Some(end) = self.range.end {
                args.push("-to".into());
                args.push(end.to_string().into());
            }
            args.push("-i".into());
            args.push(source.as_os_str().to_owned());
            args.extend([
                "-c".into(),
                "copy".into(),
                "-avoid_negative_ts".into(),
                "make_zero".into(),
            ]);
        }

        args.push(target.as_os_str().to_owned());
        args
    }
}

/// WAV has nowhere to put a picture, and asking anyway only earns a warning.
fn embeds_cover_art(format: &str) -> bool {
    !format.eq_ignore_ascii_case("wav")
}

fn push_container(args: &mut Vec<OsString>, format: &str) {
    args.push("--merge-output-format".into());
    args.push(format.to_owned().into());
    args.push("--remux-video".into());
    args.push(format.to_owned().into());
}

/// A validated start/end pair. `end` is `None` for "clip until the video ends".
#[derive(Clone, Copy, Debug)]
pub struct ClipRange {
    pub start: f64,
    pub end: Option<f64>,
}

impl ClipRange {
    pub fn new(start: &str, end: Option<&str>) -> Result<Self, AppError> {
        let start_seconds = parse_timestamp(start)
            .ok_or_else(|| AppError::usage(format!("invalid start timestamp '{start}'")))?;
        let end_seconds = match end {
            None => None,
            Some(value) if is_open_end(value) => None,
            Some(value) => Some(
                parse_timestamp(value)
                    .ok_or_else(|| AppError::usage(format!("invalid end timestamp '{value}'")))?,
            ),
        };
        if end_seconds.is_some_and(|value| value <= start_seconds) {
            return Err(AppError::usage("the end must be later than the start"));
        }
        Ok(Self {
            start: start_seconds,
            end: end_seconds,
        })
    }

    /// The `--download-sections` argument; `inf` means "through the end of the video".
    pub fn section(&self) -> String {
        match self.end {
            Some(end) => format!("*{}-{}", self.start, end),
            None => format!("*{}-inf", self.start),
        }
    }

    /// How long the finished clip should be, when the end is known.
    pub fn duration(&self) -> Option<f64> {
        self.end.map(|end| end - self.start)
    }

    /// The `[start-end]` fragment used in clip filenames.
    pub fn label(&self) -> String {
        match self.end {
            Some(end) => format!("{}-{}", stamp_label(self.start), stamp_label(end)),
            None => format!("{}-end", stamp_label(self.start)),
        }
    }
}

pub fn is_open_end(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "end" | "inf" | "eof" | "all"
    )
}

/// Renders seconds as a filename-safe timestamp such as `01.20` or `1.02.03`.
pub fn stamp_label(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}.{minutes:02}.{secs:02}")
    } else {
        format!("{minutes:02}.{secs:02}")
    }
}

pub fn parse_timestamp(value: &str) -> Option<f64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for (index, part) in parts.iter().enumerate() {
        let number = part.parse::<f64>().ok()?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        if index > 0 && number >= 60.0 {
            return None;
        }
        total = total * 60.0 + number;
    }
    Some(total)
}

/// Hosts whose query strings only ever carry share/tracking noise. Keeping that
/// noise can make yt-dlp resolve a share link to the poster's whole feed.
const STRIP_QUERY_HOSTS: &[&str] = &[
    "tiktok.com",
    "instagram.com",
    "facebook.com",
    "fb.watch",
    "twitter.com",
    "x.com",
];

pub fn normalize_url(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '<' || c == '>')
        .trim();

    let Some(cut) = trimmed.find(['?', '#']) else {
        return trimmed.to_owned();
    };
    let base = &trimmed[..cut];
    let host = base.to_ascii_lowercase();
    if STRIP_QUERY_HOSTS.iter().any(|needle| host.contains(needle)) {
        base.trim_end_matches('/').to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Detects links that stand for a whole account, feed, tag, or playlist rather
/// than one video, so a single download never turns into a bulk scrape.
pub fn bulk_url_reason(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let path = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest)
        .split_once('/')
        .map_or("", |(_, rest)| rest);
    let path = path.split(['?', '#']).next().unwrap_or("");

    if lower.contains("tiktok.com") {
        let single = path.contains("/video/") || path.contains("/photo/");
        if path.starts_with('@') && !single {
            return Some(format!(
                "'{url}' is a TikTok profile, which holds every video on that account. Open the video you want, copy its link (it contains '/video/'), and try again. Turn on Playlist if you really do want the whole account."
            ));
        }
        if path.starts_with("tag/") || path.starts_with("music/") || path.starts_with("discover") {
            return Some(format!(
                "'{url}' is a TikTok feed of many videos. Paste a single video link, or turn on Playlist."
            ));
        }
    }

    if lower.contains("youtube.com") || lower.contains("youtu.be") {
        let channel = path.starts_with('@')
            || path.starts_with("channel/")
            || path.starts_with("c/")
            || path.starts_with("user/")
            || path.starts_with("playlist");
        if channel {
            return Some(format!(
                "'{url}' is a YouTube channel or playlist, not a single video. Paste a link to one video, or turn on Playlist to download the whole list."
            ));
        }
    }

    None
}

pub fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn next_value(iter: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String, AppError> {
    iter.next()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::usage(format!("{option} requires a value")))
}

pub fn add_video_quality(args: &mut Vec<OsString>, quality: &str) {
    if quality.eq_ignore_ascii_case("best") {
        args.extend(["--format".into(), "bv*+ba/b".into()]);
        return;
    }
    // Highest stream at or below the cap. yt-dlp walks its format list from the
    // best match down, so a video with no 1080p quietly becomes 720p, then 480p.
    // The final branch keeps a video that only exists above the cap downloadable.
    let capped = format!("bv*[height<={quality}]+ba/b[height<={quality}]/bv*+ba/b");
    args.extend(["--format".into(), capped.into()]);
}

pub fn validate_quality(quality: &str) -> Result<(), AppError> {
    if quality.eq_ignore_ascii_case("best") || quality.parse::<u16>().is_ok_and(|q| q > 0) {
        Ok(())
    } else {
        Err(AppError::usage(
            "quality must be 'best' or a height such as 2160, 1080, or 720",
        ))
    }
}

pub fn validate_format(mode: MediaMode, format: &str) -> Result<(), AppError> {
    let allowed = mode.formats();
    if allowed.contains(&format.to_ascii_lowercase().as_str()) {
        Ok(())
    } else {
        Err(AppError::usage(format!(
            "unsupported format '{format}'. Allowed: {}",
            allowed.join(", ")
        )))
    }
}

pub fn validate_filename(filename: &str) -> Result<(), AppError> {
    let path = Path::new(filename);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        Err(AppError::usage(
            "the filename must be a template, not an absolute or parent path; set the folder separately",
        ))
    } else {
        Ok(())
    }
}
