use std::env;
use std::path::{Path, PathBuf};

use crate::error::AppError;

/// The backend binaries INAT drives. `ffmpeg` is optional at discovery time so
/// a missing copy can be reported against the job that actually needs it.
#[derive(Clone, Debug)]
pub struct Tools {
    pub ytdlp: PathBuf,
    pub ffmpeg: Option<PathBuf>,
}

impl Tools {
    pub fn discover() -> Result<Self, AppError> {
        let found = ToolDiscovery::scan();
        let ytdlp = found.ytdlp.ok_or_else(|| {
            AppError::runtime(format!(
                "yt-dlp was not found. Put yt-dlp in '{}' or on your PATH.",
                found.preferred_dir.display()
            ))
        })?;
        Ok(Self {
            ytdlp,
            ffmpeg: found.ffmpeg,
        })
    }

    pub fn require_ffmpeg(&self, purpose: &str) -> Result<(), AppError> {
        if self.ffmpeg.is_some() {
            Ok(())
        } else {
            Err(AppError::runtime(format!(
                "FFmpeg is required for {purpose}. Put ffmpeg beside yt-dlp in the tools folder."
            )))
        }
    }
}

pub struct ToolDiscovery {
    pub ytdlp: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub preferred_dir: PathBuf,
}

impl ToolDiscovery {
    pub fn scan() -> Self {
        let executable_dir = env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let preferred_dir = executable_dir.join("tools");
        // A packaged INAT keeps its tools beside the exe. A cargo build puts the
        // exe under target/release, so also look back up the tree for the
        // project's own tools folder. The window unpacks the copies it carries
        // into the application folder, so that is worth looking in first: it is
        // the one place a machine with nothing installed on it still has them.
        let mut directories = vec![
            crate::settings::tools_dir(),
            preferred_dir.clone(),
            executable_dir.clone(),
        ];
        directories.extend(tools_dirs_above(&executable_dir));
        directories.push(current_dir.join("tools"));
        directories.push(current_dir.clone());
        directories.extend(tools_dirs_above(&current_dir));

        let ytdlp = env_tool("INAT_YTDLP").or_else(|| find_tool(&directories, "yt-dlp"));
        let ffmpeg = env_tool("INAT_FFMPEG").or_else(|| find_tool(&directories, "ffmpeg"));

        Self {
            ytdlp,
            ffmpeg,
            preferred_dir,
        }
    }
}

/// `tools` folders in the directories above `start`, nearest first.
fn tools_dirs_above(start: &Path) -> Vec<PathBuf> {
    start
        .ancestors()
        .skip(1)
        .take(4)
        .map(|directory| directory.join("tools"))
        .collect()
}

fn env_tool(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

fn filenames_for(name: &str) -> &'static [&'static str] {
    if cfg!(windows) {
        match name {
            "yt-dlp" => &["yt-dlp.exe", "yt-dlp"],
            "ffmpeg" => &["ffmpeg.exe", "ffmpeg"],
            "ffprobe" => &["ffprobe.exe", "ffprobe"],
            _ => &[],
        }
    } else {
        &[]
    }
}

fn find_tool(directories: &[PathBuf], name: &str) -> Option<PathBuf> {
    let known = filenames_for(name);
    let fallback = [name];
    let filenames: &[&str] = if known.is_empty() { &fallback } else { known };

    for directory in directories {
        for filename in filenames {
            let candidate = directory.join(filename);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    find_on_path(name)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let known = filenames_for(name);
    let fallback = [name];
    let candidates: &[&str] = if known.is_empty() { &fallback } else { known };
    env::split_paths(&path)
        .flat_map(|directory| candidates.iter().map(move |name| directory.join(name)))
        .find(|candidate| candidate.is_file())
}
