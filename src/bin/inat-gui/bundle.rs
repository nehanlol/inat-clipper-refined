//! The backends, carried inside the executable.
//!
//! INAT is one file. yt-dlp and FFmpeg ride along in it and are laid down in
//! the hidden application folder the first time the window opens, which is what
//! makes a fresh Windows machine with nothing installed on it work exactly like
//! a developer's. Nothing is ever written beside the executable.
//!
//! yt-dlp is the one piece that goes stale - sites change, and a build from
//! three months ago starts failing on them - so it can update itself in place
//! from its own releases. Once it has, this module leaves it alone: the copy on
//! disk is newer than the copy in here, and putting the old one back would undo
//! exactly what the update was for.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use inat::Tools;
use inat::settings::tools_dir;

const YTDLP: &[u8] = include_bytes!("../../../tools/yt-dlp.exe");
const FFMPEG: &[u8] = include_bytes!("../../../tools/ffmpeg.exe");

const YTDLP_NAME: &str = "yt-dlp.exe";
const FFMPEG_NAME: &str = "ffmpeg.exe";
/// Written next to yt-dlp to record which copy is on disk: the size of the
/// build that was laid down, or [`SELF_UPDATED`] once it has replaced itself.
const STAMP: &str = "yt-dlp.stamp";
const SELF_UPDATED: &str = "updated";

/// Puts the tools in place and says where they are.
///
/// Slow only once. After that it is three cheap checks that the files are still
/// the right size, so a half-written copy from an interrupted first run is
/// noticed and replaced rather than quietly failing every download afterwards.
pub fn provide() -> Result<Tools, String> {
    let dir = tools_dir();
    fs::create_dir_all(&dir).map_err(|error| {
        format!("could not create '{}': {error}", dir.display())
    })?;

    let ffmpeg = lay_down(&dir, FFMPEG_NAME, FFMPEG)?;
    let ytdlp = lay_down_tool(&dir)?;

    Ok(Tools {
        ytdlp: override_from("INAT_YTDLP").unwrap_or(ytdlp),
        ffmpeg: Some(override_from("INAT_FFMPEG").unwrap_or(ffmpeg)),
    })
}

/// yt-dlp, unless the copy already there is one it fetched for itself.
fn lay_down_tool(dir: &Path) -> Result<PathBuf, String> {
    let stamp = dir.join(STAMP);
    let target = dir.join(YTDLP_NAME);
    let recorded = fs::read_to_string(&stamp).unwrap_or_default();

    let ours = recorded.trim() == YTDLP.len().to_string();
    let theirs = recorded.trim() == SELF_UPDATED;
    if target.is_file() && (ours || theirs) {
        return Ok(target);
    }

    write_tool(&target, YTDLP)?;
    let _ = fs::write(stamp, YTDLP.len().to_string());
    Ok(target)
}

/// Records that yt-dlp has replaced itself, so the copy carried in here is
/// never put back over it.
pub fn mark_self_updated() {
    let _ = fs::write(tools_dir().join(STAMP), SELF_UPDATED);
}

fn lay_down(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let target = dir.join(name);
    let matches = fs::metadata(&target).is_ok_and(|data| data.len() == bytes.len() as u64);
    if !matches {
        write_tool(&target, bytes)?;
    }
    Ok(target)
}

/// Writes to a scratch name and moves it into place, so a run that is
/// interrupted halfway cannot leave behind a truncated tool that looks present
/// and fails at everything.
fn write_tool(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let scratch = target.with_extension("partial");
    let write = fs::write(&scratch, bytes).and_then(|()| replace(&scratch, target));
    if let Err(error) = write {
        let _ = fs::remove_file(&scratch);
        // A tool already in place that another INAT is running cannot be
        // replaced, and does not need to be.
        if target.is_file() {
            return Ok(());
        }
        return Err(format!(
            "could not unpack {}: {error}",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    Ok(())
}

fn replace(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        // Windows will not rename onto an existing file.
        Err(_) if to.exists() => {
            fs::remove_file(to)?;
            fs::rename(from, to)
        }
        Err(error) => Err(error),
    }
}

fn override_from(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

/// What yt-dlp says it is, for the line beside the update button. Its own
/// startup costs a moment, so this is never asked for on the drawing thread.
pub fn version(ytdlp: &Path) -> Option<String> {
    let text = ask(ytdlp, &["--version"])?;
    let line = text.lines().next()?.trim().to_owned();
    (!line.is_empty()).then_some(line)
}

/// Runs yt-dlp's own updater. It replaces its own file in place, which works
/// because the tools folder belongs to INAT rather than to Program Files.
pub fn update(ytdlp: &Path) -> Result<String, String> {
    let text = ask(ytdlp, &["--update"]).ok_or("yt-dlp could not be run")?;
    let said = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .unwrap_or("Updated")
        .to_owned();

    let lowered = said.to_ascii_lowercase();
    if lowered.contains("error") {
        return Err(said);
    }
    // Only once it has actually replaced itself does the copy on disk stop
    // being ours. Told it is already current, it is still the one we unpacked,
    // and a later INAT carrying a newer build should still be able to lay that
    // one down.
    if !lowered.contains("up to date") {
        mark_self_updated();
    }
    Ok(said)
}

fn ask(program: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null());
    no_console_window(&mut command);

    let output = command.output().ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Some(text)
}

#[cfg(windows)]
fn no_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_command: &mut Command) {}
