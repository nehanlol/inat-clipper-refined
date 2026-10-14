use std::fs;
use std::path::Path;

use crate::job::{PROGRESS_FIELDS, PROGRESS_SENTINEL, parse_timestamp};

/// One `--progress-template` line, already split into typed fields.
#[derive(Clone, Debug, Default)]
pub struct ProgressUpdate {
    pub downloaded: Option<f64>,
    pub total: Option<f64>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
    pub fragment_index: Option<f64>,
    pub fragment_count: Option<f64>,
    pub playlist_index: Option<u64>,
    pub playlist_total: Option<u64>,
    pub title: String,
}

impl ProgressUpdate {
    pub fn parse(rest: &str) -> Option<Self> {
        let parts = rest.splitn(PROGRESS_FIELDS, '|').collect::<Vec<_>>();
        if parts.len() < PROGRESS_FIELDS {
            return None;
        }
        Some(Self {
            downloaded: numeric(parts[1]),
            total: numeric(parts[2]).or_else(|| numeric(parts[3])),
            speed: numeric(parts[4]),
            eta: numeric(parts[5]),
            fragment_index: numeric(parts[6]),
            fragment_count: numeric(parts[7]),
            playlist_index: numeric(parts[8]).map(|value| value as u64),
            playlist_total: numeric(parts[9]).map(|value| value as u64),
            title: parts[10].trim().to_owned(),
        })
    }

    /// Completion in 0.0..=1.0, by bytes when the size is known and by
    /// fragment count for streamed formats such as HLS.
    pub fn fraction(&self) -> Option<f64> {
        if let (Some(done), Some(total)) = (self.downloaded, self.total)
            && total > 0.0
        {
            return Some((done / total).clamp(0.0, 1.0));
        }
        if let (Some(index), Some(count)) = (self.fragment_index, self.fragment_count)
            && count > 0.0
        {
            return Some((index / count).clamp(0.0, 1.0));
        }
        None
    }

    /// The heading this update belongs under, prefixed with its place in a
    /// playlist when there is more than one item. `None` when yt-dlp has not
    /// resolved a title yet.
    pub fn heading(&self) -> Option<String> {
        if self.title.is_empty() || self.title == "NA" {
            return None;
        }
        Some(match (self.playlist_index, self.playlist_total) {
            (Some(index), Some(total)) if total > 1 => format!("[{index}/{total}] {}", self.title),
            _ => self.title.clone(),
        })
    }

    /// Size, speed and time left, as separate pieces so each front end can
    /// join them with whatever separator suits it.
    pub fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        match (self.downloaded, self.total) {
            (Some(done), Some(total)) => {
                parts.push(format!("{} / {}", human_bytes(done), human_bytes(total)));
            }
            (Some(done), None) => parts.push(human_bytes(done)),
            _ => {}
        }
        if let Some(speed) = self.speed {
            parts.push(format!("{}/s", human_bytes(speed)));
        }
        if let Some(eta) = self.eta {
            parts.push(format!("{} left", human_duration(eta)));
        }
        parts
    }

    /// The `12.4 MB / 24.0 MB   1.2 MB/s   00:09 left` tail the terminal draws.
    pub fn detail(&self) -> String {
        self.parts().join("   ")
    }
}

/// One ffmpeg status line, such as
/// `size=   10240kB time=00:00:41.00 bitrate=2045.6kbits/s speed=1.02x`.
#[derive(Clone, Debug)]
pub struct FfmpegProgress {
    pub seconds: f64,
    pub bytes: Option<f64>,
    pub speed: Option<String>,
}

impl FfmpegProgress {
    pub fn parse(line: &str) -> Option<Self> {
        let seconds = parse_timestamp(tagged(line, "time=")?)?;
        Some(Self {
            seconds,
            bytes: tagged(line, "size=").and_then(parse_ffmpeg_size),
            speed: tagged(line, "speed=").map(str::to_owned),
        })
    }

    /// How much of the requested range has been written, when the finished
    /// length is known. A clip knows; a plain conversion does not.
    pub fn fraction(&self, total: Option<f64>) -> Option<f64> {
        total
            .filter(|total| *total > 0.0)
            .map(|total| (self.seconds / total).clamp(0.0, 1.0))
    }

    pub fn parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(bytes) = self.bytes {
            parts.push(human_bytes(bytes));
        }
        parts.push(format!("{} cut", human_duration(self.seconds)));
        if let Some(speed) = &self.speed {
            parts.push(speed.clone());
        }
        parts
    }

    pub fn detail(&self) -> String {
        self.parts().join("   ")
    }
}

/// The whitespace-delimited value following `key` in an ffmpeg status line.
fn tagged<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = line[start..].trim_start();
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
}

fn parse_ffmpeg_size(value: &str) -> Option<f64> {
    let lower = value.to_ascii_lowercase();
    let (digits, scale) = if let Some(rest) = lower.strip_suffix("kib") {
        (rest, 1024.0)
    } else if let Some(rest) = lower.strip_suffix("kb") {
        (rest, 1024.0)
    } else if let Some(rest) = lower.strip_suffix("mib") {
        (rest, 1024.0 * 1024.0)
    } else if let Some(rest) = lower.strip_suffix("mb") {
        (rest, 1024.0 * 1024.0)
    } else if let Some(rest) = lower.strip_suffix('b') {
        (rest, 1.0)
    } else {
        (lower.as_str(), 1.0)
    };
    digits
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value * scale)
}

/// What one line of yt-dlp output turned out to mean. Anything unrecognized
/// yields nothing and is dropped on purpose.
#[derive(Clone, Debug)]
pub enum Signal {
    Progress(Box<ProgressUpdate>),
    Error(String),
    Duplicate(String),
    Stage(String),
    Destination(String),
    /// Where the site's own cover image was just written.
    Thumbnail(String),
}

/// One line can mean two things at once: `[Merger] Merging formats into
/// "clip.mp4"` is both a stage change *and* the name of the finished file, and
/// missing the second half is how the wrong path ends up on screen.
pub fn classify(line: &str) -> Vec<Signal> {
    let line = line.trim_end_matches(['\r', '\n']);
    let trimmed = line.trim_start();
    let mut signals = Vec::new();

    if let Some(rest) = trimmed.strip_prefix(PROGRESS_SENTINEL) {
        if let Some(update) = ProgressUpdate::parse(rest) {
            signals.push(Signal::Progress(Box::new(update)));
        }
        return signals;
    }
    if trimmed.starts_with("ERROR:") {
        signals.push(Signal::Error(trimmed.to_owned()));
        return signals;
    }
    if let Some(path) = already_downloaded_path(trimmed) {
        signals.push(Signal::Duplicate(path));
        return signals;
    }
    if let Some(path) = thumbnail_path(trimmed) {
        signals.push(Signal::Thumbnail(path));
        return signals;
    }

    if let Some(stage) = stage_label(trimmed) {
        signals.push(Signal::Stage(stage.to_owned()));
    } else if let Some(note) = activity(trimmed) {
        signals.push(Signal::Stage(note));
    }
    if let Some(path) = destination_path(trimmed) {
        signals.push(Signal::Destination(path));
    }
    signals
}

/// yt-dlp narrates every step it takes before a byte moves - reading the page,
/// listing formats, picking one. Those lines are already written for people, so
/// they are passed through rather than mapped, which is what keeps a job from
/// sitting on one frozen label for the whole extraction.
fn activity(line: &str) -> Option<String> {
    let close = line.strip_prefix('[')?.find("] ")? + 1;
    let tag = &line[1..close];
    let rest = line[close + 2..].trim();

    // `[download]` lines are progress or a destination, both handled elsewhere.
    if tag == "download" || rest.is_empty() || rest.starts_with("Extracting URL") {
        return None;
    }

    // `[youtube] ba6-_MMxF4E: Downloading webpage` -> `Downloading webpage`.
    // The head is only an id when it has no spaces in it.
    let text = match rest.split_once(": ") {
        Some((head, tail)) if !head.contains(' ') && !tail.is_empty() => tail,
        _ => rest,
    };
    (text.len() <= 90).then(|| text.to_owned())
}

/// `[info] Writing video thumbnail originCover to: C:\...\clip.image`
///
/// This lands before a byte of video moves, which is what makes it worth
/// reading: it is the earliest moment a card can be given a picture. The file
/// is later moved somewhere else entirely, so the path here is good for a
/// moment rather than forever.
pub fn thumbnail_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("[info] Writing ")?;
    let at = rest.find(" thumbnail")?;
    let path = rest[at..].split_once(" to: ")?.1.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// `[download] C:\...\clip.mp4 has already been downloaded`
pub fn already_downloaded_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("[download] ")?;
    let cut = rest.find(" has already been downloaded")?;
    Some(rest[..cut].trim().to_owned())
}

pub fn destination_path(line: &str) -> Option<String> {
    if !line.starts_with('[') {
        return None;
    }
    if let Some(index) = line.find("Destination: ") {
        let path = line[index + "Destination: ".len()..].trim();
        return (!path.is_empty()).then(|| path.to_owned());
    }
    // `[Merger] Merging formats into "C:\...\video.mp4"`
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

pub fn stage_label(line: &str) -> Option<&'static str> {
    const STAGES: &[(&str, &str)] = &[
        ("[Merger]", "Merging video and audio"),
        ("[ExtractAudio]", "Extracting audio"),
        ("[VideoRemuxer]", "Repackaging"),
        ("[VideoConvertor]", "Converting"),
        ("[Fixup", "Finishing up"),
        ("[SplitChapters]", "Splitting"),
        ("[Metadata]", "Writing metadata"),
    ];
    STAGES
        .iter()
        .find(|(prefix, _)| line.starts_with(prefix))
        .map(|(_, label)| *label)
}

/// What the run left behind, gathered as it went.
#[derive(Clone, Debug, Default)]
pub struct RunReport {
    pub destination: Option<String>,
    pub duplicate: Option<String>,
    pub errors: Vec<String>,
}

impl RunReport {
    pub fn absorb(&mut self, signal: &Signal) {
        match signal {
            Signal::Error(line) => self.errors.push(line.clone()),
            Signal::Duplicate(path) => self.duplicate = Some(path.clone()),
            Signal::Destination(path) => self.destination = Some(path.clone()),
            _ => {}
        }
    }

    /// Whether a second attempt could plausibly do any better. A dropped
    /// connection or a throttled host is worth retrying; a video that is
    /// private, gone, or that this build simply cannot read is not, and
    /// hammering it three times only makes the failure take longer to report.
    pub fn permanent(&self, stderr: &[String]) -> bool {
        const SETTLED: &[&str] = &[
            "unsupported url",
            "is not a valid url",
            "video unavailable",
            "private video",
            "this video is private",
            "account is private",
            "removed by the uploader",
            "members-only",
            "requested format is not available",
            "no video formats found",
            "sign in to confirm",
            "unable to extract",
            "not available in your country",
            "geo restricted",
            "does not exist",
            "no such file or directory",
            "http error 403",
            "http error 404",
            "http error 410",
            "http error 429",
            "too many requests",
            "bot-detection",
            "confirm you’re not a bot",
            "confirm you're not a bot",
        ];
        self.errors.iter().chain(stderr.iter()).any(|line| {
            let lower = line.to_ascii_lowercase();
            SETTLED.iter().any(|needle| lower.contains(needle))
        })
    }

    /// The handful of lines worth showing after a failure: real `ERROR:` lines
    /// first, and if there were none, the tail of whatever stderr said.
    pub fn error_lines(&self, stderr: &[String]) -> Vec<String> {
        let mut shown = Vec::new();
        for line in self.errors.iter().chain(stderr.iter()) {
            let text = line.trim();
            if text.is_empty() || !text.contains("ERROR") {
                continue;
            }
            shown.push(text.trim_start_matches("ERROR:").trim().to_owned());
            if shown.len() == 4 {
                return shown;
            }
        }
        if shown.is_empty() {
            for line in stderr.iter().rev().take(3).rev() {
                if !line.trim().is_empty() {
                    shown.push(line.trim().to_owned());
                }
            }
        }
        shown
    }
}

pub fn file_size(path: &str) -> Option<u64> {
    fs::metadata(path).ok().map(|data| data.len())
}

pub fn folder_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_else(|| path.to_owned())
}

/// yt-dlp renders absent values as `NA`.
fn numeric(field: &str) -> Option<f64> {
    let value = field.trim();
    if value.is_empty() || value == "NA" || value == "None" {
        return None;
    }
    value.parse::<f64>().ok().filter(|number| number.is_finite())
}

pub fn human_bytes(value: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value.max(0.0);
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

pub fn human_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}
