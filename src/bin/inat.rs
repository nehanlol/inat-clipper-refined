//! The terminal front end. Everything it knows about a download arrives as a
//! [`Event`]; all it does is draw.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use inat::job::{Plan, is_open_end, looks_like_url, validate_quality};
use inat::progress::{file_size, folder_of};
use inat::{AppError, Event, JobSpec, MediaMode, Outcome, Settings, Tools, VERSION, runner};

const BAR_WIDTH: usize = 24;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) if error.quiet => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("inat: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, AppError> {
    let mut args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.is_empty() {
        return interactive_session();
    }

    let raw_command = args.remove(0);
    let command = raw_command.to_string_lossy().to_ascii_lowercase();
    match command.as_str() {
        "download" | "dl" | "d" => command_for(args, MediaMode::Download),
        "clip" | "c" => command_for(args, MediaMode::Clip),
        "audio" | "a" => command_for(args, MediaMode::Audio),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(0)
        }
        "version" | "--version" | "-v" => {
            println!("inat {VERSION}");
            Ok(0)
        }
        _ if looks_like_url(&command) => {
            args.insert(0, raw_command);
            command_for(args, MediaMode::Download)
        }
        _ => Err(AppError::usage(format!(
            "unknown command '{command}'. Run 'inat help' for usage."
        ))),
    }
}

fn command_for(args: Vec<OsString>, mode: MediaMode) -> Result<u8, AppError> {
    let Some(spec) = JobSpec::from_args(args, mode)? else {
        print_command_help(mode);
        return Err(AppError::quiet());
    };
    let tools = Tools::discover()?;
    execute(spec.plan(&tools)?)
}

/// Runs a plan with its output captured, so the terminal shows a progress bar
/// and a one-line result instead of the raw download log.
fn execute(plan: Plan) -> Result<u8, AppError> {
    let handle = runner::start(plan);
    let mut view = ProgressView::new();
    let mut outcome = Outcome::default();

    for event in handle.events {
        match event {
            Event::Started => view.stage("Reading the link"),
            Event::Item(heading) => view.announce(&heading),
            Event::Stage(stage) => view.stage(&stage),
            Event::Download(update) => view.draw_bar(update.fraction(), &update.detail()),
            Event::Convert { progress, fraction } => view.draw_bar(fraction, &progress.detail()),
            // A terminal has nowhere to put a picture.
            Event::Poster(_) => {}
            Event::Retrying { attempt, of } => view.retrying(attempt, of),
            Event::Finished(finished) => outcome = *finished,
        }
    }

    view.clear();
    render_outcome(&outcome);
    Ok(outcome.code)
}

fn render_outcome(outcome: &Outcome) {
    if outcome.cancelled {
        println!("  {}  {}", styled("x", YELLOW), styled("Stopped", BOLD));
        return;
    }
    if outcome.succeeded() {
        render_success(outcome);
    } else {
        render_failure(outcome);
    }
}

fn render_success(outcome: &Outcome) {
    if let Some(path) = &outcome.report.duplicate {
        println!(
            "  {}  {}",
            styled("=", YELLOW),
            styled("Already downloaded, nothing to do.", BOLD)
        );
        println!("     {}", styled(path, DIM));
        println!(
            "     {}",
            styled("Pass --overwrite to download it again.", DIM)
        );
        return;
    }

    match &outcome.report.destination {
        Some(path) if outcome.items <= 1 => {
            println!("  {}  {}", styled("+", GREEN), styled("Saved", BOLD));
            println!("     {}{}", path, styled(&size_note(path), DIM));
        }
        Some(path) => {
            println!(
                "  {}  {}",
                styled("+", GREEN),
                styled(&format!("Saved {} files", outcome.items), BOLD)
            );
            println!("     {}", styled(&folder_of(path), DIM));
        }
        None => println!("  {}  {}", styled("+", GREEN), styled("Done", BOLD)),
    }
}

fn render_failure(outcome: &Outcome) {
    println!("  {}  {}", styled("x", RED), styled("Download failed", BOLD));
    for line in outcome.report.error_lines(&outcome.stderr) {
        println!("     {}", styled(&line, DIM));
    }
}

fn size_note(path: &str) -> String {
    match file_size(path) {
        Some(bytes) => format!("   ({})", inat::human_bytes(bytes as f64)),
        None => String::new(),
    }
}

/// Draws the single self-updating status line.
struct ProgressView {
    tty: bool,
    line_width: usize,
    heading: String,
    milestone: i64,
}

impl ProgressView {
    fn new() -> Self {
        Self {
            tty: io::stdout().is_terminal(),
            line_width: 0,
            heading: String::new(),
            milestone: -1,
        }
    }

    /// Replaces the status line in place. `plain` is the same text without
    /// color codes, and is what the width is measured from.
    fn transient_styled(&mut self, plain: &str, shown: &str) {
        if !self.tty {
            return;
        }
        let width = plain.chars().count();
        let padding = self.line_width.saturating_sub(width);
        let mut out = io::stdout().lock();
        let _ = write!(out, "\r{shown}{}", " ".repeat(padding));
        let _ = out.flush();
        self.line_width = width;
    }

    fn transient(&mut self, text: &str) {
        let plain = strip_style(text);
        self.transient_styled(&plain, text);
    }

    fn clear(&mut self) {
        if self.tty && self.line_width > 0 {
            let mut out = io::stdout().lock();
            let _ = write!(out, "\r{}\r", " ".repeat(self.line_width));
            let _ = out.flush();
        }
        self.line_width = 0;
    }

    fn stage(&mut self, label: &str) {
        self.transient(&format!("   {}  {label}...", styled("*", CYAN)));
    }

    /// A failed attempt is being made again. Worth a permanent line rather
    /// than a transient one: a download that took three tries should say so
    /// after it finishes, not only while it is happening.
    fn retrying(&mut self, attempt: u32, of: u32) {
        self.clear();
        println!(
            "  {}  {}",
            styled("~", YELLOW),
            styled(&format!("That try failed. Attempt {attempt} of {of}."), DIM)
        );
        self.heading = String::new();
        self.milestone = -1;
    }

    /// Prints the video title once, the first time it is seen.
    fn announce(&mut self, heading: &str) {
        if heading == self.heading {
            return;
        }
        self.clear();
        println!("  {}  {}", styled(">", CYAN), styled(heading, BOLD));
        self.heading = heading.to_owned();
        self.milestone = -1;
    }

    fn draw_bar(&mut self, fraction: Option<f64>, detail: &str) {
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!("   {detail}")
        };

        if !self.tty {
            self.log_milestone(fraction, &detail);
            return;
        }

        let filled = fraction.map_or(0, |value| (value * BAR_WIDTH as f64).round() as usize);
        let filled = filled.min(BAR_WIDTH);
        let done = "#".repeat(filled);
        let left = ".".repeat(BAR_WIDTH - filled);
        let percent = match fraction {
            Some(value) => format!("{:>3}%", (value * 100.0).round() as i64),
            None => "  ?%".to_owned(),
        };

        let plain = format!("   [{done}{left}] {percent}{detail}");
        let shown = format!(
            "   [{}{}] {percent}{detail}",
            styled(&done, GREEN),
            styled(&left, DIM)
        );
        self.transient_styled(&plain, &shown);
    }

    /// Redirected output has no cursor to move, so report every 25% instead.
    fn log_milestone(&mut self, fraction: Option<f64>, detail: &str) {
        let Some(fraction) = fraction else { return };
        let step = ((fraction * 100.0) as i64 / 25) * 25;
        if step > self.milestone {
            self.milestone = step;
            println!("     {step}%{detail}");
        }
    }
}

/// The no-arguments flow: pick a job, hand it a link.
fn interactive_session() -> Result<u8, AppError> {
    loop {
        let settings = Settings::load();
        clear_screen();
        render_banner(&settings);

        let Some(choice) = prompt_choice(&settings)? else {
            clear_screen();
            return Ok(0);
        };
        let mode = match choice {
            Choice::Settings => {
                settings_screen()?;
                continue;
            }
            Choice::Job(mode) => mode,
        };

        println!();
        let url = prompt("Link")?;
        if is_quit(&url) {
            clear_screen();
            return Ok(0);
        }

        println!();
        let result = run_interactive(mode, url, &settings);

        println!();
        if let Err(error) = result {
            println!("  {}  {error}", styled("x", RED));
        }
        wait_for_enter()?;
    }
}

fn run_interactive(mode: MediaMode, url: String, settings: &Settings) -> Result<u8, AppError> {
    let mut spec = JobSpec::new(mode, url, settings);
    if mode == MediaMode::Clip {
        let start = prompt("Start  (Enter to start from the beginning)")?;
        let end = prompt("End    (Enter to run to the end)")?;
        println!();
        // An empty start is the top of the video, so the pair alone can describe
        // "the first two minutes" without making the user type a zero.
        spec.start = Some(if start.is_empty() { "0".to_owned() } else { start });
        spec.end = (!is_open_end(&end)).then_some(end);
    }
    let tools = Tools::discover()?;
    execute(spec.plan(&tools)?)
}

enum Choice {
    Job(MediaMode),
    Settings,
}

/// Asks which job to run. `None` means the user chose to leave.
fn prompt_choice(settings: &Settings) -> Result<Option<Choice>, AppError> {
    menu_row("1", "VIDEO", &settings.quality_label());
    menu_row("2", "AUDIO", "Highest quality available, original format");
    menu_row("3", "CLIP", "A time range cut out of a video");
    menu_row("4", "SETTINGS", "Quality and download folder");
    println!();
    loop {
        let choice = prompt("Type")?;
        match choice.to_ascii_lowercase().as_str() {
            "1" | "video" | "v" => return Ok(Some(Choice::Job(MediaMode::Download))),
            "2" | "audio" | "a" => return Ok(Some(Choice::Job(MediaMode::Audio))),
            "3" | "clip" | "c" => return Ok(Some(Choice::Job(MediaMode::Clip))),
            "4" | "settings" | "s" => return Ok(Some(Choice::Settings)),
            "q" | "quit" | "exit" => return Ok(None),
            // Anything else, Enter included, just asks again in place.
            _ => {}
        }
    }
}

fn menu_row(number: &str, title: &str, detail: &str) {
    println!(
        "  {}  {:<9} {}",
        styled(&format!("[{number}]"), INAT),
        styled(title, BOLD),
        styled(detail, DIM)
    );
}

fn settings_screen() -> Result<(), AppError> {
    loop {
        let settings = Settings::load();
        clear_screen();
        render_banner(&settings);
        println!("  {}", styled("SETTINGS", BOLD));
        println!();
        menu_row("1", "QUALITY", &settings.quality_label());
        menu_row("2", "FOLDER", &settings.folder.display().to_string());
        menu_row("3", "BACK", "Return to the menu");
        println!();

        let choice = prompt("Type")?;
        let outcome = match choice.to_ascii_lowercase().as_str() {
            "1" | "quality" => edit_quality(settings),
            "2" | "folder" => edit_folder(settings),
            "3" | "b" | "back" | "q" | "quit" | "exit" => return Ok(()),
            _ => continue,
        };
        if let Err(error) = outcome {
            println!();
            println!("  {}  {error}", styled("x", RED));
            wait_for_enter()?;
        }
    }
}

fn edit_quality(mut settings: Settings) -> Result<(), AppError> {
    println!();
    println!(
        "     {}",
        styled("2160, 1440, 1080, 720, 480, 360, or best", DIM)
    );
    let value = prompt("Max quality")?;
    if value.is_empty() {
        return Ok(());
    }
    validate_quality(&value)?;
    settings.quality = value.to_ascii_lowercase();
    settings.save()
}

fn edit_folder(mut settings: Settings) -> Result<(), AppError> {
    println!();
    println!("     {}", styled("Enter to keep the current folder", DIM));
    let value = prompt("Download folder")?;
    if value.is_empty() {
        return Ok(());
    }
    // Creating it now surfaces a bad path here rather than mid-download.
    let folder = PathBuf::from(value.trim_matches(|c| c == '"' || c == '\''));
    fs::create_dir_all(&folder).map_err(|error| {
        AppError::runtime(format!("could not use '{}': {error}", folder.display()))
    })?;
    settings.folder = folder;
    settings.save()
}

fn is_quit(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "q" | "quit" | "exit"
    )
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[96m";
const GREEN: &str = "\x1b[92m";
const YELLOW: &str = "\x1b[93m";
const RED: &str = "\x1b[91m";
/// The INAT green, #2cdb92.
const INAT: &str = "\x1b[38;2;44;219;146m";
const INAT_BOLD: &str = "\x1b[1;38;2;44;219;146m";

fn render_banner(settings: &Settings) {
    println!();
    println!("  {}", styled("╭───────────────────────────────╮", INAT));
    println!(
        "  {}{}{}{}{}",
        styled("│   ", INAT),
        styled("INAT", INAT_BOLD),
        styled("  |  ", DIM),
        styled("Video Downloader", INAT),
        styled("   │", INAT)
    );
    println!("  {}", styled("╰───────────────────────────────╯", INAT));
    println!();
    println!(
        "  {} {}",
        styled("Saves to", DIM),
        styled(&settings.folder.display().to_string(), INAT)
    );
    println!();
}

fn clear_screen() {
    if io::stdout().is_terminal() {
        print!("\x1b]0;INAT\x07\x1b[2J\x1b[H");
        let _ = io::stdout().flush();
    }
}

fn prompt(label: &str) -> Result<String, AppError> {
    print!("  {} {}: ", styled("›", CYAN), styled(label, BOLD));
    io::stdout()
        .flush()
        .map_err(|error| AppError::runtime(error.to_string()))?;
    let mut value = String::new();
    let read = io::stdin()
        .read_line(&mut value)
        .map_err(|error| AppError::runtime(error.to_string()))?;
    // A closed stdin never fills in, so treat it as "leave" rather than
    // re-asking a question nobody is there to answer.
    if read == 0 {
        return Err(AppError::quiet());
    }
    Ok(value.trim().to_owned())
}

fn wait_for_enter() -> Result<(), AppError> {
    println!();
    let _ = prompt("Press Enter for another link")?;
    Ok(())
}

fn styled(text: &str, code: &str) -> String {
    if io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

fn strip_style(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for escape in chars.by_ref() {
                if escape == 'm' {
                    break;
                }
            }
        } else {
            plain.push(ch);
        }
    }
    plain
}

fn print_help() {
    println!(
        r#"inat {VERSION} - convenient yt-dlp + FFmpeg CLI

USAGE:
  inat <command> [options]
  inat <url> [options]
  inat                 Open the interactive prompt

COMMANDS:
  download, dl <url>   Download a full video
  clip, c <url>        Download only a time range
  audio, a <url>       Download audio only
  help                 Show this help

EXAMPLES:
  inat dl "https://example.com/video"
  inat dl "https://example.com/video" -q best
  inat clip "https://example.com/video" -s 01:20 -e 03:45
  inat clip "https://example.com/video" -s 01:20
  inat audio "https://example.com/video"

NOTES:
  Menu option 4 sets the quality cap and the download folder. Both live in
  inat-settings.txt beside the exe; --quality and --output still override them.
  Video steps down a level whenever the cap is not offered.
  Audio keeps the original stream; pass --format mp3 to convert instead.
  A file that is already downloaded is skipped; pass --overwrite to redo it.
  Profile, channel, and playlist links are refused unless you pass --playlist.
  inat-gui.exe is the same downloader in a window.

Run 'inat <command> --help' for command options."#
    );
}

fn print_command_help(mode: MediaMode) {
    match mode {
        MediaMode::Download => println!(
            "USAGE: inat download <url> [--quality best|HEIGHT] [--format mp4|mkv|webm]
       [--output FOLDER] [--filename TEMPLATE] [--playlist] [--overwrite]"
        ),
        MediaMode::Clip => println!(
            "USAGE: inat clip <url> --from TIME [--to TIME] [--quality best|HEIGHT]
       [--format mp4|mkv|webm] [--accurate] [--output FOLDER] [--overwrite]

Omit --to to clip through to the end of the video. Clips are saved as
'Title [01.20-03.45].mp4' unless you pass your own --filename."
        ),
        MediaMode::Audio => println!(
            "USAGE: inat audio <url> [--format best|mp3|m4a|opus|flac|wav]
       [--audio-quality 0-10] [--output FOLDER] [--filename TEMPLATE] [--overwrite]"
        ),
    }
}
