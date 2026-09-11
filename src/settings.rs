use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::job::validate_quality;

pub const DEFAULT_QUALITY: &str = "best";

/// Preferences that outlive a run, kept in a small text file beside the exe so
/// a portable copy carries its own settings.
#[derive(Clone, Debug)]
pub struct Settings {
    pub quality: String,
    pub folder: PathBuf,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            quality: DEFAULT_QUALITY.to_owned(),
            folder: default_download_dir(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Ok(text) = fs::read_to_string(settings_path())
            .or_else(|_| fs::read_to_string(legacy_settings_path()))
        else {
            return settings;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            // A hand-edited file should not be able to wedge the app, so an
            // unusable value simply leaves the default in place.
            match key.trim() {
                "quality" if validate_quality(value).is_ok() => {
                    settings.quality = value.to_ascii_lowercase();
                }
                "folder" => settings.folder = PathBuf::from(value),
                _ => {}
            }
        }
        settings
    }

    pub fn save(&self) -> Result<(), AppError> {
        let text = format!(
            "# INAT settings\nquality = {}\nfolder = {}\n",
            self.quality,
            self.folder.display()
        );
        fs::write(settings_path(), text)
            .map_err(|error| AppError::runtime(format!("could not save settings: {error}")))
    }

    pub fn quality_label(&self) -> String {
        if self.quality.eq_ignore_ascii_case("best") {
            "Best available, no cap".to_owned()
        } else {
            format!("Up to {}p, steps down when it is not offered", self.quality)
        }
    }
}

/// Where the settings live now. A file left beside the executable by an older
/// copy is still read, once, so upgrading does not silently reset the download
/// folder someone chose.
pub fn settings_path() -> PathBuf {
    app_dir().join("settings.txt")
}

pub fn legacy_settings_path() -> PathBuf {
    beside_exe("inat-settings.txt")
}

pub fn beside_exe(name: &str) -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(name)
}

/// Everything INAT keeps to itself: the tools it runs, the half-finished
/// downloads, the settings, the list. One hidden folder under LOCALAPPDATA, so
/// a single executable stays a single executable - nothing is ever written
/// beside it, and none of this is anything anyone has to look at or tidy up.
pub fn app_dir() -> PathBuf {
    let dir = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(env::temp_dir)
        .join("INAT");
    if fs::create_dir_all(&dir).is_ok() {
        conceal(&dir);
    }
    dir
}

/// Where half-finished work lives: `.part` files, the fragments yt-dlp merges,
/// cover images, and the whole video a clip is cut out of.
pub fn work_dir() -> PathBuf {
    app_dir().join("work")
}

/// Where the bundled yt-dlp and FFmpeg are laid down.
pub fn tools_dir() -> PathBuf {
    app_dir().join("tools")
}

/// Clears out what a run that was killed rather than finished left behind.
///
/// A job cleans up after itself when it ends, but nothing cleans up after a
/// window that was closed mid-download, and the leavings would otherwise sit in
/// the working folder forever. Only things old enough that no running job could
/// still want them are touched: a part-downloaded file is what a paused job
/// resumes from, so those are left alone until they are clearly abandoned.
pub fn sweep_leftovers() {
    use std::time::{Duration, SystemTime};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = fs::read_dir(work_dir()) else {
        return;
    };
    let now = SystemTime::now();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let scratch = name.starts_with("thumb-") || name.starts_with("clip-");
        let abandoned = name.ends_with(".part");
        if !scratch && !abandoned {
            continue;
        }

        let keep_for = if scratch { DAY } else { 30 * DAY };
        let past_it = entry
            .metadata()
            .and_then(|data| data.modified())
            .ok()
            .and_then(|when| now.duration_since(when).ok())
            .is_some_and(|age| age > keep_for);
        if !past_it {
            continue;
        }

        let path = entry.path();
        if path.is_dir() {
            let _ = fs::remove_dir_all(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Marks a folder hidden, so the working files are out of the way rather than
/// merely out of sight. Best effort - failing to hide it is not worth failing
/// a download over.
#[cfg(windows)]
fn conceal(path: &Path) {
    use std::os::windows::ffi::OsStrExt;

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

    unsafe extern "system" {
        fn GetFileAttributesW(name: *const u16) -> u32;
        fn SetFileAttributesW(name: *const u16, attributes: u32) -> i32;
    }

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    unsafe {
        let current = GetFileAttributesW(wide.as_ptr());
        if current != INVALID_FILE_ATTRIBUTES && current & FILE_ATTRIBUTE_HIDDEN == 0 {
            SetFileAttributesW(wide.as_ptr(), current | FILE_ATTRIBUTE_HIDDEN);
        }
    }
}

#[cfg(not(windows))]
fn conceal(_path: &Path) {}

pub fn default_download_dir() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("Downloads")
        .join("INAT Downloads")
}
