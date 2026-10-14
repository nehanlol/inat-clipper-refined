//! The download list, written beside the settings so that closing the window
//! does not throw it away: what is still waiting to be finished, and what has
//! already landed.
//!
//! Nothing about a half-downloaded file itself is stored here. yt-dlp leaves
//! its `.part` in the working folder when it is stopped, so a job restored from
//! this list and started again picks up from the byte it had reached rather
//! than from zero - the file on disk is the progress, and this is only the note
//! saying what it was for.

use std::fs;
use std::path::PathBuf;

use crate::error::AppError;
use crate::job::{JobSpec, MediaMode};
use crate::settings::{app_dir, beside_exe};

/// How many finished downloads are kept. Enough to be a history worth looking
/// back through, few enough that the file never becomes something to manage.
pub const HISTORY: usize = 60;

/// One job on the list: everything needed to run it again, what to call it on
/// screen, and - once it has landed - where it landed.
#[derive(Clone, Debug)]
pub struct Pending {
    pub spec: JobSpec,
    pub title: String,
    /// Where the finished file is. `None` while the job is still unfinished.
    pub saved: Option<String>,
    /// Whether this one is over. A job can be done with no path to show for it,
    /// so this is not simply `saved.is_some()`.
    pub done: bool,
}

pub fn path() -> PathBuf {
    app_dir().join("queue.txt")
}

/// Where an older copy kept the list, read once so that upgrading does not
/// throw away whatever was still unfinished at the time.
fn legacy_path() -> PathBuf {
    beside_exe("inat-queue.txt")
}

/// Replaces the list on disk. An empty list removes the file rather than
/// leaving an empty one behind.
///
/// Everything unfinished is kept, however much of it there is - losing one of
/// those loses work. Finished downloads are history, so only the most recent
/// [`HISTORY`] of them are written; `pending` is expected newest first, the
/// order the window shows.
pub fn save(pending: &[Pending]) -> Result<(), AppError> {
    let file = path();
    let keeping = worth_keeping(pending);

    if keeping.is_empty() {
        let _ = fs::remove_file(&file);
        return Ok(());
    }

    fs::write(&file, render(&keeping)).map_err(|error| {
        AppError::runtime(format!("could not save the download list: {error}"))
    })
}

/// The file's text. Separate from writing it so that what goes out and what
/// [`parse`] reads back can be checked against each other.
fn render(keeping: &[&Pending]) -> String {
    let mut text = String::from("# INAT downloads\n");
    for job in keeping {
        let spec = &job.spec;
        text.push_str("\n[job]\n");
        write_field(&mut text, "mode", spec.mode.key());
        write_field(&mut text, "state", if job.done { "done" } else { "held" });
        if let Some(saved) = &job.saved {
            write_field(&mut text, "saved", saved);
        }
        write_field(&mut text, "url", &spec.url);
        write_field(&mut text, "quality", &spec.quality);
        write_field(&mut text, "format", &spec.format);
        write_field(&mut text, "audio-quality", &spec.audio_quality);
        write_field(&mut text, "filename", &spec.filename);
        write_field(&mut text, "title", &job.title);
        if let Some(folder) = &spec.output {
            write_field(&mut text, "folder", &folder.display().to_string());
        }
        if let Some(start) = &spec.start {
            write_field(&mut text, "start", start);
        }
        if let Some(end) = &spec.end {
            write_field(&mut text, "end", end);
        }
        write_field(&mut text, "accurate", flag(spec.accurate));
        write_field(&mut text, "playlist", flag(spec.playlist));
        write_field(&mut text, "overwrite", flag(spec.overwrite));
    }
    text
}

/// Everything unfinished, plus the most recent [`HISTORY`] that are done.
fn worth_keeping(pending: &[Pending]) -> Vec<&Pending> {
    let mut history = 0;
    pending
        .iter()
        .filter(|job| {
            !job.done || {
                history += 1;
                history <= HISTORY
            }
        })
        .collect()
}

/// Reads the list back. A file that has been hand-edited into nonsense costs a
/// job, never a crash: anything that does not parse is dropped.
pub fn load() -> Vec<Pending> {
    fs::read_to_string(path())
        .or_else(|_| fs::read_to_string(legacy_path()))
        .map(|text| parse(&text))
        .unwrap_or_default()
}

fn parse(text: &str) -> Vec<Pending> {
    let mut jobs = Vec::new();
    let mut current: Option<Draft> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.eq_ignore_ascii_case("[job]") {
            jobs.extend(current.take().and_then(Draft::build));
            current = Some(Draft::default());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(draft) = current.as_mut() {
            draft.set(key.trim(), value.trim());
        }
    }
    jobs.extend(current.and_then(Draft::build));
    jobs
}

/// A job being read back, before it is known to be complete enough to run.
#[derive(Default)]
struct Draft {
    mode: Option<MediaMode>,
    url: String,
    quality: String,
    format: String,
    audio_quality: String,
    filename: String,
    title: String,
    folder: Option<PathBuf>,
    start: Option<String>,
    end: Option<String>,
    accurate: bool,
    playlist: bool,
    overwrite: bool,
    saved: Option<String>,
    done: bool,
}

impl Draft {
    fn set(&mut self, key: &str, value: &str) {
        if value.is_empty() {
            return;
        }
        match key {
            "mode" => self.mode = MediaMode::from_key(value),
            "state" => self.done = value.eq_ignore_ascii_case("done"),
            "saved" => self.saved = Some(value.to_owned()),
            "url" => self.url = value.to_owned(),
            "quality" => self.quality = value.to_owned(),
            "format" => self.format = value.to_owned(),
            "audio-quality" => self.audio_quality = value.to_owned(),
            "filename" => self.filename = value.to_owned(),
            "title" => self.title = value.to_owned(),
            "folder" => self.folder = Some(PathBuf::from(value)),
            "start" => self.start = Some(value.to_owned()),
            "end" => self.end = Some(value.to_owned()),
            "accurate" => self.accurate = truthy(value),
            "playlist" => self.playlist = truthy(value),
            "overwrite" => self.overwrite = truthy(value),
            _ => {}
        }
    }

    /// A job is only worth restoring if it still describes something runnable -
    /// a finished one included, since its card offers to run it again.
    fn build(self) -> Option<Pending> {
        let mode = self.mode?;
        if self.url.is_empty() {
            return None;
        }
        let (saved, done) = (self.saved, self.done);
        let spec = JobSpec {
            mode,
            url: self.url,
            quality: pick(self.quality, "best"),
            format: pick(self.format, mode.default_format()),
            audio_quality: pick(self.audio_quality, "0"),
            output: self.folder,
            filename: pick(self.filename, mode.default_filename()),
            start: self.start,
            end: self.end,
            accurate: self.accurate,
            playlist: self.playlist,
            overwrite: self.overwrite,
        };
        spec.validate().ok()?;
        Some(Pending {
            title: self.title,
            spec,
            saved,
            done,
        })
    }
}

/// A saved value can contain anything except a newline, which is the one thing
/// that would break the file apart.
fn write_field(text: &mut String, key: &str, value: &str) {
    let value = value.replace(['\r', '\n'], " ");
    text.push_str(key);
    text.push_str(" = ");
    text.push_str(value.trim());
    text.push('\n');
}

fn flag(on: bool) -> &'static str {
    if on { "yes" } else { "no" }
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "yes" | "true" | "on" | "1"
    )
}

fn pick(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What is written and what is read back have to agree exactly, because
    /// what comes out of this file is what runs when Resume is pressed - and
    /// what the window shows for everything it has already downloaded.
    #[test]
    fn the_list_survives_a_trip_through_the_file() {
        let held = Pending {
            spec: JobSpec {
                mode: MediaMode::Clip,
                url: "https://www.tiktok.com/@a/video/77".to_owned(),
                quality: "1080".to_owned(),
                format: "mkv".to_owned(),
                audio_quality: "5".to_owned(),
                output: Some(PathBuf::from(r"C:\Users\me\Downloads\INAT Downloads")),
                filename: "%(title)s.%(ext)s".to_owned(),
                start: Some("01:20".to_owned()),
                end: Some("03:45".to_owned()),
                accurate: true,
                playlist: false,
                overwrite: true,
            },
            title: "A Clip".to_owned(),
            saved: None,
            done: false,
        };
        let done = Pending {
            spec: JobSpec {
                mode: MediaMode::Audio,
                url: "https://example.com/song".to_owned(),
                quality: "best".to_owned(),
                format: "mp3".to_owned(),
                audio_quality: "0".to_owned(),
                output: None,
                filename: "%(title)s [AUDIO].%(ext)s".to_owned(),
                start: None,
                end: None,
                accurate: false,
                playlist: false,
                overwrite: false,
            },
            title: "A Song".to_owned(),
            saved: Some(r"C:\Downloads\A Song [AUDIO].mp3".to_owned()),
            done: true,
        };

        let read = parse(&render(&[&held, &done]));
        assert_eq!(read.len(), 2);

        let back = &read[0];
        assert_eq!(back.spec.mode, MediaMode::Clip);
        assert_eq!(back.spec.url, held.spec.url);
        assert_eq!(back.spec.quality, "1080");
        assert_eq!(back.spec.format, "mkv");
        assert_eq!(back.spec.audio_quality, "5");
        assert_eq!(back.spec.output, held.spec.output);
        assert_eq!(back.spec.filename, held.spec.filename);
        assert_eq!(back.spec.start, held.spec.start);
        assert_eq!(back.spec.end, held.spec.end);
        assert!(back.spec.accurate);
        assert!(!back.spec.playlist);
        assert!(back.spec.overwrite);
        assert_eq!(back.title, "A Clip");
        assert!(!back.done);
        assert_eq!(back.saved, None);

        let back = &read[1];
        assert_eq!(back.spec.mode, MediaMode::Audio);
        assert_eq!(back.spec.format, "mp3");
        assert_eq!(back.title, "A Song");
        assert!(back.done);
        assert_eq!(back.saved, done.saved);
    }

    /// A hand-edited or half-written file costs the jobs it broke, never the
    /// ones it did not, and never a crash.
    #[test]
    fn skips_jobs_it_cannot_rebuild() {
        let text = "\
# INAT unfinished downloads
[job]
url = https://example.com/one
[job]
mode = video
[job]
mode = audio
url = https://example.com/two
nonsense
also = unknown key
";
        let read = parse(text);
        // The first has no mode, the second no link; only the third can run.
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].spec.mode, MediaMode::Audio);
        assert_eq!(read[0].spec.url, "https://example.com/two");
        // Absent fields fall back to what that mode would have used anyway.
        assert_eq!(read[0].spec.format, MediaMode::Audio.default_format());
    }

    /// A link that would pull down a whole account is refused when it is typed,
    /// so it must not sneak back in through the saved list either.
    #[test]
    fn refuses_to_restore_a_job_that_would_be_refused_now() {
        let text = "[job]\nmode = video\nurl = https://www.tiktok.com/@someone\n";
        assert!(parse(text).is_empty());
    }

    /// A download that has already landed stays on the list, and comes back
    /// knowing where its file went - otherwise the window forgets everything
    /// it has ever downloaded the moment it closes.
    #[test]
    fn a_finished_download_stays_on_the_list() {
        let one = |done: bool, saved: Option<&str>| Pending {
            spec: JobSpec {
                mode: MediaMode::Download,
                url: "https://example.com/one".to_owned(),
                quality: "best".to_owned(),
                format: "mp4".to_owned(),
                audio_quality: "0".to_owned(),
                output: None,
                filename: "%(title)s.%(ext)s".to_owned(),
                start: None,
                end: None,
                accurate: false,
                playlist: false,
                overwrite: false,
            },
            title: "Something".to_owned(),
            saved: saved.map(str::to_owned),
            done,
        };

        let mut text = String::from("[job]\n");
        let job = one(true, Some(r"C:\Downloads\Something.mp4"));
        write_field(&mut text, "mode", job.spec.mode.key());
        write_field(&mut text, "state", "done");
        write_field(&mut text, "saved", job.saved.as_deref().unwrap());
        write_field(&mut text, "url", &job.spec.url);
        write_field(&mut text, "title", &job.title);

        let read = parse(&text);
        assert_eq!(read.len(), 1);
        assert!(read[0].done);
        assert_eq!(read[0].saved.as_deref(), Some(r"C:\Downloads\Something.mp4"));

        // A job with no state written is one that never finished.
        let held = parse("[job]\nmode = video\nurl = https://example.com/two\n");
        assert!(!held[0].done);
        assert_eq!(held[0].saved, None);
    }

    /// History is capped so the file cannot grow without end - but only the
    /// history. Dropping an unfinished job would drop actual work.
    #[test]
    fn keeps_every_unfinished_job_but_only_so_much_history() {
        let job = |done: bool| Pending {
            spec: JobSpec {
                mode: MediaMode::Download,
                url: "https://example.com/x".to_owned(),
                quality: "best".to_owned(),
                format: "mp4".to_owned(),
                audio_quality: "0".to_owned(),
                output: None,
                filename: "%(title)s.%(ext)s".to_owned(),
                start: None,
                end: None,
                accurate: false,
                playlist: false,
                overwrite: false,
            },
            title: String::new(),
            saved: done.then(|| r"C:\Downloads\x.mp4".to_owned()),
            done,
        };

        let mut list = vec![job(false); HISTORY + 5];
        list.extend(vec![job(true); HISTORY + 20]);

        let kept = worth_keeping(&list);
        assert_eq!(kept.len(), HISTORY + 5 + HISTORY);
        assert_eq!(kept.iter().filter(|entry| !entry.done).count(), HISTORY + 5);
        assert_eq!(kept.iter().filter(|entry| entry.done).count(), HISTORY);
    }
}
