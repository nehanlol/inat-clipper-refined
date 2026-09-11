use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::job::{Plan, unique_suffix};
use crate::progress::{FfmpegProgress, ProgressUpdate, RunReport, Signal, classify};
use crate::settings::work_dir;

/// Everything that happens during a run, in the order it happens. Both front
/// ends consume this same stream; neither one parses tool output itself.
#[derive(Clone, Debug)]
pub enum Event {
    /// The tool started and we are waiting on it to resolve the link.
    Started,
    /// A new title came into view. Emitted once per item, playlists included.
    Item(String),
    /// What the tool says it is doing right now, whether that is a named
    /// post-download step ("Merging video and audio") or its own narration of
    /// the extraction ("Downloading webpage").
    Stage(String),
    /// yt-dlp is pulling bytes.
    Download(Box<ProgressUpdate>),
    /// ffmpeg is writing the finished file. The fraction is resolved here so
    /// that only the runner has to know how long a clip was asked to be.
    Convert {
        progress: Box<FfmpegProgress>,
        fraction: Option<f64>,
    },
    /// A still from the finished file, as PNG bytes. Sent instead of a path so
    /// there is nothing left on disk for a front end to tidy up, and skipped
    /// entirely when the result has no picture in it.
    Poster(Vec<u8>),
    /// The attempt that just failed is being made again. Everything reported
    /// so far belongs to the previous try and should be cleared.
    Retrying { attempt: u32, of: u32 },
    /// The run is over, however it ended. Always the last event.
    Finished(Box<Outcome>),
}

/// How many times a run that failed on its own is tried before the failure is
/// reported. Most of what goes wrong mid-download - a dropped connection, a
/// host that throttles for a moment - is gone by the next try.
pub const ATTEMPTS: u32 = 3;

/// How a run ended, with everything needed to describe it to the user.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub code: u8,
    pub items: usize,
    pub report: RunReport,
    pub stderr: Vec<String>,
    pub cancelled: bool,
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        self.code == 0 && !self.cancelled
    }

    fn failure(message: String) -> Self {
        Self {
            code: 1,
            report: RunReport {
                errors: vec![format!("ERROR: {message}")],
                ..RunReport::default()
            },
            ..Self::default()
        }
    }
}

/// A running job: the events it is producing, and the switch that stops it.
pub struct Handle {
    pub events: Receiver<Event>,
    pub cancel: Cancel,
}

/// Runs `plan` on its own thread. Never fails up front - a tool that will not
/// start arrives as a `Finished` event like any other ending, so a caller only
/// has one place to handle the end of a job.
pub fn start(plan: Plan) -> Handle {
    let (sender, events) = channel();
    let cancel = Cancel::default();
    let handle_cancel = cancel.clone();

    thread::spawn(move || {
        let outcome = run(plan, &sender, &cancel);
        let _ = sender.send(Event::Finished(Box::new(outcome)));
    });

    Handle {
        events,
        cancel: handle_cancel,
    }
}

/// Runs `plan` on the calling thread, reporting as it goes, and grabs a still
/// of whatever it produced once it lands. A run that fails on its own is tried
/// again up to [`ATTEMPTS`] times before the failure is reported.
pub fn run(plan: Plan, sender: &Sender<Event>, cancel: &Cancel) -> Outcome {
    let poster = plan.poster.clone();
    let watcher = watch_thumbnail(&plan, sender);
    let found = watcher.as_ref().map(Watcher::sender);
    let mut outcome = fetch(&plan, sender, cancel, found.as_ref());

    let mut attempt = 1;
    while attempt < ATTEMPTS && worth_retrying(&outcome, cancel) {
        attempt += 1;
        // A moment between tries, so a host that is refusing everyone right
        // now is not asked three times inside a second. Waited out before the
        // attempt is announced, so a cancel during the wait leaves the failure
        // that actually happened on screen rather than a try that never ran.
        if !pause(Duration::from_secs(2 * (attempt - 1) as u64), cancel) {
            break;
        }
        let _ = sender.send(Event::Retrying {
            attempt,
            of: ATTEMPTS,
        });
        outcome = fetch(&plan, sender, cancel, found.as_ref());
    }

    // Nothing more can arrive, so the watcher can be told the run is over.
    drop(found);
    // The cover image usually wins, because it arrives while the download is
    // still going. A still out of the finished file is the fallback for the
    // handful of places that offer no picture of their own.
    let shown = watcher.map(Watcher::finish).unwrap_or(false);
    if !shown
        && outcome.succeeded()
        && let Some(ffmpeg) = poster
        && let Some(file) = outcome
            .report
            .destination
            .as_deref()
            .or(outcome.report.duplicate.as_deref())
        && let Some(png) = still_frame(&ffmpeg, Path::new(file), &FROM_MEDIA)
    {
        let _ = sender.send(Event::Poster(png));
    }
    if let Some(thumbs) = &plan.thumbs {
        clean_up(thumbs);
    }

    outcome
}

/// The thread that turns the site's own cover image into a poster, and the
/// channel that feeds it. [`Watcher::finish`] reports whether a picture was
/// ever sent.
struct Watcher {
    found: Sender<PathBuf>,
    /// Set the moment the run ends. Waiting on a file that is never going to
    /// appear must not hold up the news that the download is done.
    over: Arc<AtomicBool>,
    thread: thread::JoinHandle<bool>,
}

impl Watcher {
    fn sender(&self) -> Sender<PathBuf> {
        self.found.clone()
    }

    /// Dropping the last sender is what tells the thread the run is over.
    fn finish(self) -> bool {
        self.over.store(true, Ordering::Relaxed);
        drop(self.found);
        self.thread.join().unwrap_or(false)
    }
}

/// Turns the cover image into a poster the moment yt-dlp has one.
///
/// yt-dlp fetches the cover before it downloads a byte of video and says where
/// it put it, which is the earliest a card can be given a picture - and the
/// whole point of doing this rather than pulling a frame out of the finished
/// file. It is on its own thread because reading and scaling the image takes
/// long enough to stall the reader that is draining yt-dlp's output.
///
/// The file is later moved into the job's own thumbnail folder, so if the
/// early path is gone by the time it is looked at - a download short enough to
/// finish first - the folder is swept once at the end as a fallback.
fn watch_thumbnail(plan: &Plan, sender: &Sender<Event>) -> Option<Watcher> {
    let ffmpeg = plan.poster.clone()?;
    let folder = plan.thumbs.clone();
    let sender = sender.clone();
    let (found, arrivals) = channel::<PathBuf>();
    let over = Arc::new(AtomicBool::new(false));
    let ended = Arc::clone(&over);

    let thread = thread::spawn(move || {
        for path in &arrivals {
            if settled(&path, &ended)
                && let Some(png) = still_frame(&ffmpeg, &path, &FROM_IMAGE)
            {
                let _ = sender.send(Event::Poster(png));
                return true;
            }
        }
        // The run is over. Whatever was moved into the job's folder is the
        // last chance at a picture before falling back to a frame of the video.
        folder
            .as_deref()
            .and_then(first_file)
            .and_then(|file| still_frame(&ffmpeg, &file, &FROM_IMAGE))
            .map(|png| sender.send(Event::Poster(png)).is_ok())
            .unwrap_or(false)
    });

    Some(Watcher {
        found,
        over,
        thread,
    })
}

/// Waits for a file that was announced before it was written to appear and
/// stop growing.
///
/// Gives up when the run ends, and in any case after a few seconds. A file that
/// never turns up - because the download failed right after announcing it, or
/// because yt-dlp found one already there and reused it without saying where -
/// must not keep the job from reporting that it is finished.
fn settled(path: &Path, over: &AtomicBool) -> bool {
    let mut last = None;
    for _ in 0..40 {
        let size = fs::metadata(path).map(|data| data.len()).ok();
        let there = size.is_some_and(|size| size > 0);
        // Two readings the same means nothing is still being appended to it.
        if there && size == last {
            return true;
        }
        // Once the run is over nothing is writing anything, so whatever is
        // there is whole - and waiting longer only delays the finished job.
        if over.load(Ordering::Relaxed) {
            return there;
        }
        last = size;
        thread::sleep(Duration::from_millis(100));
    }
    false
}

fn first_file(folder: &Path) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        // A zero-length file is one that is still being written.
        .find(|path| fs::metadata(path).is_ok_and(|data| data.is_file() && data.len() > 0))
}

/// Whether the failure that just happened is worth another try. A cancel never
/// is, and neither is a video that is private, gone, or that yt-dlp says it
/// cannot read at all.
fn worth_retrying(outcome: &Outcome, cancel: &Cancel) -> bool {
    !outcome.succeeded()
        && !outcome.cancelled
        && !cancel.is_cancelled()
        && !outcome.report.permanent(&outcome.stderr)
}

/// Waits in small steps so a cancel arriving mid-wait is noticed immediately
/// rather than after the whole delay. False means the run was cancelled.
fn pause(total: Duration, cancel: &Cancel) -> bool {
    let step = Duration::from_millis(100);
    let mut waited = Duration::ZERO;
    while waited < total {
        if cancel.is_cancelled() {
            return false;
        }
        thread::sleep(step);
        waited += step;
    }
    !cancel.is_cancelled()
}

/// How to ask ffmpeg for a picture, as (seek, stream) pairs tried in order. An
/// empty seek means do not seek at all, which is the only way to reach an
/// attached picture: seeking a cover art stream skips past the one frame it has
/// and writes nothing.
///
/// A video is worth three tries - a few seconds in, which skips the black frame
/// most of them open on; the very first frame, for anything too short for that;
/// and the attached cover art, which is what gives an audio file a picture of
/// its own. A cover image that has already been downloaded is one frame and
/// needs exactly one.
const FROM_MEDIA: [(&str, &str); 3] = [("3", "0:V:0"), ("0", "0:V:0"), ("", "0:v:0")];
const FROM_IMAGE: [(&str, &str); 1] = [("", "0:v:0")];

/// A picture of a file that was downloaded some time ago, for a list being
/// rebuilt from disk rather than watched as it happens.
pub fn poster_for(ffmpeg: &Path, file: &Path) -> Option<Vec<u8>> {
    still_frame(ffmpeg, file, &FROM_MEDIA)
}

/// One picture of `file`, scaled down to something a list can hold. Anything
/// with no picture in it at all fails every pass and simply gets none.
///
/// Seeking a *local* file is instant - the whole reason a clip is cut here
/// rather than mid-download - so this costs a few hundred milliseconds.
fn still_frame(ffmpeg: &Path, file: &Path, passes: &[(&str, &str)]) -> Option<Vec<u8>> {
    let work = work_dir();
    let _ = fs::create_dir_all(&work);
    let target = work.join(format!("poster-{}.png", unique_suffix()));

    for (seek, stream) in passes.iter().copied() {
        let mut command = Command::new(ffmpeg);
        command.args(["-hide_banner", "-loglevel", "error", "-y"]);
        if !seek.is_empty() {
            command.args(["-ss", seek]);
        }
        command
            .arg("-i")
            .arg(file)
            .args([
                "-map",
                stream,
                "-frames:v",
                "1",
                "-vf",
                "scale=256:-1",
                "-f",
                "image2",
            ])
            .arg(&target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_console_window(&mut command);

        let wrote = command.status().is_ok_and(|status| status.success());
        let png = wrote.then(|| fs::read(&target).ok()).flatten();
        let _ = fs::remove_file(&target);
        if let Some(png) = png
            && !png.is_empty()
        {
            return Some(png);
        }
    }
    None
}

/// Fetches what was asked for. A clip is two passes: pull the video, then cut
/// the range out of it.
///
/// Takes the plan by reference because a failed attempt is simply run again,
/// and the working folder a clip was given is recreated here rather than when
/// the plan was built, so the second attempt still has somewhere to work.
fn fetch(
    plan: &Plan,
    sender: &Sender<Event>,
    cancel: &Cancel,
    found: Option<&Sender<PathBuf>>,
) -> Outcome {
    let _ = sender.send(Event::Started);
    if let Some(cut) = &plan.cut {
        let _ = fs::create_dir_all(&cut.scratch);
    }

    let mut outcome = step(&plan.program, &plan.args, None, sender, cancel, found);
    settle(&mut outcome.report, plan.home.as_deref());

    let Some(cut) = plan.cut.as_ref() else {
        return outcome;
    };
    if !outcome.succeeded() {
        clean_up(&cut.scratch);
        return outcome;
    }

    let Some(source) = outcome
        .report
        .destination
        .clone()
        .or_else(|| outcome.report.duplicate.clone())
    else {
        clean_up(&cut.scratch);
        outcome.code = 1;
        outcome
            .report
            .errors
            .push("ERROR: the download finished but did not say where the file went".to_owned());
        return outcome;
    };

    let source = PathBuf::from(source);
    let target = cut.target(&source);
    let _ = sender.send(Event::Stage("Cutting the clip".to_owned()));

    let cut_outcome = step(
        &cut.ffmpeg,
        &cut.args(&source, &target),
        plan.clip_seconds,
        sender,
        cancel,
        None,
    );

    // The whole video was only ever a means to the clip.
    let _ = fs::remove_file(&source);
    clean_up(&cut.scratch);

    // Handed an input it cannot read, ffmpeg has been seen to exit 0 having
    // written nothing at all, so the clip existing is the only proof that counts.
    if cut_outcome.succeeded() && !target.is_file() {
        return Outcome {
            code: 1,
            items: outcome.items,
            report: RunReport {
                errors: vec![format!(
                    "ERROR: the clip was cut but no file appeared at {}",
                    target.display()
                )],
                ..RunReport::default()
            },
            ..cut_outcome
        };
    }

    if !cut_outcome.succeeded() {
        let _ = fs::remove_file(&target);
        return Outcome {
            items: outcome.items,
            report: RunReport {
                destination: None,
                duplicate: None,
                errors: cut_outcome.report.errors,
            },
            ..cut_outcome
        };
    }

    outcome.report.destination = Some(target.display().to_string());
    outcome.report.duplicate = None;
    outcome
}

/// Turns "where yt-dlp wrote it" into "where the file is".
///
/// Everything half-finished is written to a working folder, so the path yt-dlp
/// announces is the copy it then moves into place - which is how a finished
/// download ended up pointing at a file that no longer existed. The move keeps
/// the name, so the folder it was headed for plus that name is the answer. A
/// reported path that is already a file is left alone.
fn settle(report: &mut RunReport, home: Option<&Path>) {
    let Some(home) = home else {
        return;
    };
    for slot in [&mut report.destination, &mut report.duplicate] {
        let Some(reported) = slot.as_deref() else {
            continue;
        };
        let reported = Path::new(reported);
        if reported.is_file() {
            continue;
        }
        if let Some(name) = reported.file_name() {
            let moved = home.join(name);
            if moved.is_file() {
                *slot = Some(moved.display().to_string());
            }
        }
    }
}

/// The working folder only ever holds the whole video on its way to becoming a
/// clip, so it goes as soon as the clip exists. A failure to remove it is not
/// worth failing the job over.
fn clean_up(scratch: &Path) {
    let _ = fs::remove_dir_all(scratch);
}

/// Runs one tool to completion, turning everything it says into events.
fn step(
    program: &Path,
    args: &[OsString],
    clip_seconds: Option<f64>,
    sender: &Sender<Event>,
    cancel: &Cancel,
    found: Option<&Sender<PathBuf>>,
) -> Outcome {
    let plan = Plan {
        program: program.to_path_buf(),
        args: args.to_vec(),
        clip_seconds,
        cut: None,
        home: None,
        poster: None,
        thumbs: None,
    };

    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .env("NO_COLOR", "1")
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_console_window(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Outcome::failure(format!(
                "could not start {}: {error}",
                plan.program.display()
            ));
        }
    };

    // Held for the life of the download: when INAT goes away, so does yt-dlp.
    let job = kill_on_exit::adopt(&child);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    cancel.attach(job, Arc::clone(&child));

    let stderr_reader = stderr.map(|stream| {
        let sender = sender.clone();
        let clip_seconds = plan.clip_seconds;
        thread::spawn(move || read_stderr(stream, &sender, clip_seconds))
    });

    let mut report = RunReport::default();
    let mut heading = String::new();
    let mut items = 0;

    if let Some(stdout) = stdout {
        // Read bytes, not text. yt-dlp writes in the console's own code page,
        // so a title with a curly apostrophe in it is not valid UTF-8 - and a
        // strict line reader stops dead on the first one, closes this end of
        // the pipe, and kills yt-dlp on its next progress line. That is what
        // "[Errno 22] Invalid argument" was: not a bad link, us hanging up.
        let mut reader = BufReader::new(stdout);
        let mut raw = Vec::new();
        while matches!(reader.read_until(b'\n', &mut raw), Ok(read) if read > 0) {
            let line = decode(&raw);
            raw.clear();
            for signal in classify(&line) {
                report.absorb(&signal);
                match signal {
                    Signal::Progress(update) => {
                        if let Some(next) = update.heading()
                            && next != heading
                        {
                            heading = next.clone();
                            items += 1;
                            let _ = sender.send(Event::Item(next));
                        }
                        let _ = sender.send(Event::Download(update));
                    }
                    Signal::Stage(stage) => {
                        let _ = sender.send(Event::Stage(stage));
                    }
                    // Handed straight over: the picture is worth having on
                    // screen now, and turning it into one is slow enough that
                    // it must not happen on the thread draining this output.
                    Signal::Thumbnail(path) => {
                        if let Some(found) = found {
                            let _ = found.send(PathBuf::from(path));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // stdout has closed, so the tool is on its way out. Poll rather than block
    // so a cancel arriving right now can still take the lock and kill it.
    let status = loop {
        let waited = lock(&child).try_wait();
        match waited {
            Ok(Some(status)) => break Some(status),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };

    let stderr_lines = stderr_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    let cancelled = cancel.is_cancelled();
    let code = match status {
        _ if cancelled => 1,
        Some(status) => status.code().unwrap_or(1).clamp(0, 255) as u8,
        None => 1,
    };

    Outcome {
        code,
        items,
        report,
        stderr: stderr_lines,
        cancelled,
    }
}

/// ffmpeg ends its status lines with a carriage return rather than a newline,
/// so a plain line reader sees nothing at all until it exits. Splitting on both
/// is what gives a clip a moving progress bar instead of a frozen screen.
fn read_stderr(
    stream: ChildStderr,
    sender: &Sender<Event>,
    clip_seconds: Option<f64>,
) -> Vec<String> {
    let mut collected = Vec::new();
    let mut reader = BufReader::new(stream);
    let mut chunk = Vec::new();
    let mut byte = [0u8; 1];

    while matches!(reader.read(&mut byte), Ok(1)) {
        if byte[0] == b'\r' || byte[0] == b'\n' {
            absorb_stderr(&mut chunk, sender, clip_seconds, &mut collected);
        } else {
            chunk.push(byte[0]);
        }
    }
    absorb_stderr(&mut chunk, sender, clip_seconds, &mut collected);
    collected
}

fn absorb_stderr(
    chunk: &mut Vec<u8>,
    sender: &Sender<Event>,
    clip_seconds: Option<f64>,
    collected: &mut Vec<String>,
) {
    if chunk.is_empty() {
        return;
    }
    let text = decode(chunk);
    chunk.clear();
    match FfmpegProgress::parse(&text) {
        Some(update) => {
            let fraction = update.fraction(clip_seconds);
            let _ = sender.send(Event::Convert {
                progress: Box::new(update),
                fraction,
            });
        }
        None => collected.push(text),
    }
}

/// One line of tool output as text.
///
/// UTF-8 when it is UTF-8, which covers everything on a modern setup. When it
/// is not - a Windows yt-dlp writes titles and paths in the system code page,
/// so `ain't` arrives as a byte no UTF-8 decoder will accept - the system code
/// page is asked instead, which is what turns that byte back into the right
/// character rather than a replacement mark.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => decode_ansi(bytes),
    }
}

#[cfg(windows)]
fn decode_ansi(bytes: &[u8]) -> String {
    /// The process's current ANSI code page.
    const CP_ACP: u32 = 0;

    unsafe extern "system" {
        fn MultiByteToWideChar(
            code_page: u32,
            flags: u32,
            input: *const u8,
            input_len: i32,
            output: *mut u16,
            output_len: i32,
        ) -> i32;
    }

    if bytes.is_empty() {
        return String::new();
    }
    let len = bytes.len() as i32;
    let needed =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    let written =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

#[cfg(not(windows))]
fn decode_ansi(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The switch that stops a run, and everything needed to actually stop it.
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<CancelState>>);

#[derive(Default)]
struct CancelState {
    requested: bool,
    job: Option<kill_on_exit::Job>,
    child: Option<Arc<Mutex<Child>>>,
}

impl Cancel {
    /// Ends the run. Closing the job handle takes yt-dlp *and* the ffmpeg it
    /// spawned down with it; killing the child is the fallback for when the
    /// job object could not be created.
    pub fn cancel(&self) {
        let mut state = lock(&self.0);
        state.requested = true;
        drop(state.job.take());
        if let Some(child) = state.child.take() {
            let _ = lock(&child).kill();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        lock(&self.0).requested
    }

    fn attach(&self, job: Option<kill_on_exit::Job>, child: Arc<Mutex<Child>>) {
        let mut state = lock(&self.0);
        if state.requested {
            // Cancelled between spawning and getting here.
            drop(state);
            let _ = lock(&child).kill();
            return;
        }
        state.job = job;
        state.child = Some(child);
    }
}

/// A reader thread that panics must not wedge the run, so a poisoned lock is
/// still used.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Keeps a console window from flashing open for every yt-dlp and ffmpeg the
/// GUI spawns. The CLI pipes both streams anyway, so it costs nothing there.
#[cfg(windows)]
fn no_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_command: &mut Command) {}

/// Ties a spawned tool to this process so closing the window, pressing Ctrl+C,
/// or killing INAT takes yt-dlp and its own ffmpeg children down with it,
/// instead of leaving a download running with nobody watching it.
#[cfg(windows)]
pub mod kill_on_exit {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::ptr;

    type Handle = *mut c_void;

    const KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
    const EXTENDED_LIMIT_INFORMATION: u32 = 9;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: u32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    /// Closing this handle ends every process in the job. Windows closes it for
    /// us when the process dies for any reason, which is the whole point.
    pub struct Job(Handle);

    // A job handle is process-wide, not thread-owned, so it is safe to hand to
    // whichever thread ends up cancelling the run.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// `None` simply means the job could not be created; the download still
    /// runs, it just will not be cleaned up automatically.
    pub fn adopt(child: &Child) -> Option<Job> {
        unsafe {
            let job = CreateJobObjectW(ptr::null_mut(), ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits = ExtendedLimits::default();
            limits.basic.limit_flags = KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                EXTENDED_LIMIT_INFORMATION,
                &raw const limits as *const c_void,
                size_of::<ExtendedLimits>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle() as Handle) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Job(job))
        }
    }
}

#[cfg(not(windows))]
pub mod kill_on_exit {
    use std::process::Child;

    pub struct Job;

    pub fn adopt(_child: &Child) -> Option<Job> {
        None
    }
}
