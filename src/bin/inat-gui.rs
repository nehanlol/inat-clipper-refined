//! The window front end: the same downloader as the CLI, built like a desktop
//! app rather than a terminal.
//!
//! Two columns. Everything you set lives on the left and stays put; everything
//! that happens lands on the right. Several downloads can run at once, which
//! is the one thing the terminal cannot do.
#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;
use egui::{
    Align, Color32, CornerRadius, Layout, Rect, RichText, Stroke, TextEdit, TextureHandle, Vec2,
    pos2, vec2,
};

use inat::job::{QUALITY_CHOICES, is_open_end, looks_like_url};
use inat::progress::{file_size, folder_of};
use inat::{
    Event, Handle, JobSpec, MediaMode, Outcome, Pending, RunReport, Settings, Tools, human_bytes,
    queue, runner,
};

// The INAT teal, top-lit to bottom-shaded, the same run of tones the mark is
// built from. `INAT` is the one that carries the brand - every filled button,
// every bar, every live status light. The rest are the light above it and the
// depth below it, and they are what keep a flat dark window from reading flat.
/// Top highlight, #a8f0e2. Rims and hover edges, where light would catch.
const INAT_LIT: Color32 = Color32::from_rgb(168, 240, 226);
/// Upper body, #6ee7d2. Text that has something good to say.
const INAT_SOFT: Color32 = Color32::from_rgb(110, 231, 210);
/// Mid body, #3ed6be. The brand tone.
const INAT: Color32 = Color32::from_rgb(62, 214, 190);
/// Lower body, #14b8a6.
const INAT_DEEP: Color32 = Color32::from_rgb(20, 184, 166);
/// Deep shadow at the base, #0d9488. Under an edge, never on top of one.
const INAT_DARK: Color32 = Color32::from_rgb(13, 148, 136);
/// What is legible on top of the brand tone.
const INK: Color32 = Color32::from_rgb(4, 35, 31);

// The greys are not neutral: every one of them carries a little of the teal,
// which is what makes the accent look like it grew out of the window rather
// than having been dropped on it.
const BG: Color32 = Color32::from_rgb(12, 18, 17);
const RAIL: Color32 = Color32::from_rgb(15, 23, 22);
const CARD: Color32 = Color32::from_rgb(21, 32, 30);
const RAISED: Color32 = Color32::from_rgb(29, 42, 40);
const FIELD: Color32 = Color32::from_rgb(13, 21, 20);
const LINE: Color32 = Color32::from_rgb(37, 52, 49);
const TEXT: Color32 = Color32::from_rgb(230, 239, 236);
const DIM: Color32 = Color32::from_rgb(138, 160, 155);
const AMBER: Color32 = Color32::from_rgb(230, 184, 96);
const RED: Color32 = Color32::from_rgb(233, 105, 105);

const CARD_RADIUS: u8 = 14;
const CONTROL_RADIUS: u8 = 10;
const FIELD_HEIGHT: f32 = 42.0;
const CONTROL_HEIGHT: f32 = 34.0;
const RAIL_WIDTH: f32 = 430.0;
/// Width kept clear at the end of a card's title row for the mode tag.
const TAG_ROOM: f32 = 84.0;

const LOGO_PNG: &[u8] = include_bytes!("../../assets/inat.png");

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("INAT")
        .with_inner_size([1120.0, 780.0])
        .with_min_inner_size([900.0, 600.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    eframe::run_native(
        "INAT",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(Inat::new(cc)))),
    )
}

// Kept out of `src/bin` proper so Cargo does not mistake it for another
// program to build.
#[path = "inat-gui/bundle.rs"]
mod bundle;

/// The backends, once they are in place. `None` while the first run is still
/// laying them down, which is the only time it takes long enough to notice.
type Backends = Option<Result<Tools, String>>;

/// The yt-dlp line in the rail: which build is on disk, and whatever the last
/// update attempt had to say for itself.
#[derive(Default)]
struct Backend {
    version: Option<String>,
    note: Option<String>,
    working: bool,
    replies: Option<Receiver<Reply>>,
}

enum Reply {
    Version(String),
    Updated(Result<String, String>),
}

struct Inat {
    settings: Settings,
    /// Unpacked once at startup. A tool that will not unpack is a banner, not
    /// a crash.
    tools: Backends,
    preparing: Option<Receiver<Result<Tools, String>>>,
    backend: Backend,
    logo: Option<Logo>,
    mode: MediaMode,
    url: String,
    format: String,
    start: String,
    end: String,
    accurate: bool,
    playlist: bool,
    overwrite: bool,
    notice: Option<String>,
    /// Whether the link field has been handed the caret yet.
    greeted: bool,
    /// Pictures being rebuilt for the jobs restored from the last session.
    /// Cannot start until FFmpeg is in place, so it waits on the unpack.
    posters: Option<Receiver<(u64, Vec<u8>)>>,
    jobs: Vec<Job>,
    next_id: u64,
}

/// Puts the pictures back on the downloads restored from the last session.
///
/// A poster is never written to disk - it is a few hundred kilobytes that the
/// finished file can always produce again - so a rebuilt list would otherwise
/// come back as a column of empty frames. Done on one background thread,
/// oldest work first, because a long history would otherwise hold the window
/// shut for as long as it took to read every file.
fn revive_posters(jobs: &[Job], tools: &Tools) -> Receiver<(u64, Vec<u8>)> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let Some(ffmpeg) = tools.ffmpeg.clone() else {
        return receiver;
    };
    let files = jobs
        .iter()
        .filter_map(|job| Some((job.id, PathBuf::from(job.saved_path()?))))
        .filter(|(_, file)| file.is_file())
        .collect::<Vec<_>>();
    if files.is_empty() {
        return receiver;
    }

    std::thread::spawn(move || {
        for (id, file) in files {
            // A send that fails means the window has gone.
            if let Some(png) = runner::poster_for(&ffmpeg, &file)
                && sender.send((id, png)).is_err()
            {
                return;
            }
        }
    });
    receiver
}

impl Inat {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_theme(&cc.egui_ctx);
        // The list from last time: what already landed, and whatever was still
        // going, which comes back held rather than quietly disappearing.
        let restored = queue::load();
        let mut next_id = 1;
        let jobs = restored
            .into_iter()
            .map(|pending| {
                let id = next_id;
                next_id += 1;
                Job::restored(id, pending)
            })
            .collect::<Vec<_>>();

        // Unpacking the backends touches a couple of hundred megabytes the
        // first time, so it happens behind the window rather than in front of
        // it. Every run after that it is three checks and a few milliseconds.
        let (sender, preparing) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            inat::settings::sweep_leftovers();
            let _ = sender.send(bundle::provide());
        });

        Self {
            tools: None,
            preparing: Some(preparing),
            backend: Backend::default(),
            posters: None,
            mode: MediaMode::Download,
            url: String::new(),
            format: MediaMode::Download.default_format().to_owned(),
            start: String::new(),
            end: String::new(),
            accurate: false,
            playlist: false,
            overwrite: false,
            notice: None,
            greeted: false,
            logo: Logo::load(&cc.egui_ctx),
            settings: Settings::load(),
            jobs,
            next_id,
        }
    }

    /// Picks up the unpacked backends, and everything that was waiting on them.
    fn settle_backends(&mut self) {
        if let Some(preparing) = &self.preparing
            && let Ok(ready) = preparing.try_recv()
        {
            self.preparing = None;
            if let Ok(tools) = &ready {
                self.posters = Some(revive_posters(&self.jobs, tools));
                self.ask_backend(tools.clone(), None);
            }
            self.tools = Some(ready);
        }

        let Some(replies) = &self.backend.replies else {
            return;
        };
        while let Ok(reply) = replies.try_recv() {
            match reply {
                Reply::Version(version) => self.backend.version = Some(version),
                Reply::Updated(result) => {
                    self.backend.working = false;
                    self.backend.note = Some(match result {
                        Ok(said) => said,
                        Err(said) => said,
                    });
                }
            }
        }
    }

    /// Asks yt-dlp about itself, and updates it first when told to. Both cost a
    /// whole process start, so neither happens on the drawing thread.
    fn ask_backend(&mut self, tools: Tools, update: Option<()>) {
        let (sender, replies) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if update.is_some() {
                let _ = sender.send(Reply::Updated(bundle::update(&tools.ytdlp)));
            }
            if let Some(version) = bundle::version(&tools.ytdlp) {
                let _ = sender.send(Reply::Version(version));
            }
        });
        self.backend.replies = Some(replies);
    }

    fn update_backend(&mut self) {
        let Some(Ok(tools)) = self.tools.clone() else {
            return;
        };
        self.backend.working = true;
        self.backend.note = Some("Checking with yt-dlp...".to_owned());
        self.ask_backend(tools, Some(()));
    }

    /// Builds a spec from the form and hands it to the runner.
    fn start_job(&mut self) {
        self.notice = None;
        let url = self.url.trim().to_owned();
        if url.is_empty() {
            self.notice = Some("Paste a link first.".to_owned());
            return;
        }
        let Some(tools) = self.backends() else {
            return;
        };

        let mut spec = JobSpec::new(self.mode, url, &self.settings);
        if !looks_like_url(&spec.url) {
            self.notice = Some(format!(
                "'{}' does not look like a link. Paste the full https:// address.",
                spec.url
            ));
            return;
        }
        spec.format = self.format.clone();
        spec.playlist = self.playlist;
        spec.overwrite = self.overwrite;
        if self.mode == MediaMode::Clip {
            let start = self.start.trim();
            spec.start = Some(if start.is_empty() {
                "0".to_owned()
            } else {
                start.to_owned()
            });
            let end = self.end.trim();
            spec.end = (!is_open_end(end)).then(|| end.to_owned());
            spec.accurate = self.accurate;
        }

        match spec.clone().plan(&tools) {
            Ok(plan) => {
                let id = self.next_id;
                self.next_id += 1;
                // Newest at the top: the one you just started is the one you
                // are watching.
                self.jobs.insert(0, Job::new(id, spec, runner::start(plan)));
                self.url.clear();
                self.remember();
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    /// The backends, if they are ready to be used. Says why not when they are
    /// not, since the answer is always worth showing rather than swallowing.
    fn backends(&mut self) -> Option<Tools> {
        match &self.tools {
            Some(Ok(tools)) => Some(tools.clone()),
            Some(Err(error)) => {
                self.notice = Some(error.clone());
                None
            }
            None => {
                self.notice =
                    Some("Still getting ready - this happens once. Try again in a moment.".to_owned());
                None
            }
        }
    }

    /// The quality picker and the folder picker are the settings now, so a
    /// change to either is written straight to disk.
    fn persist(&mut self) {
        if let Err(error) = self.settings.save() {
            self.notice = Some(error.to_string());
        }
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) -> bool {
        self.settle_backends();
        while let Some(Ok((id, png))) = self.posters.as_ref().map(Receiver::try_recv) {
            if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
                job.poster = decode_poster(ctx, id, &png);
            }
        }

        let mut settled = false;
        for job in &mut self.jobs {
            settled |= job.poll(ctx);
        }
        // A job that just ended is a job that no longer needs remembering.
        if settled {
            self.remember();
        }
        self.jobs.iter().any(Job::is_running)
    }
}

impl eframe::App for Inat {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.poll_jobs(ui.ctx()) {
            // Fast enough that a progress bar looks live, slow enough that an
            // idle window costs nothing.
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }

        egui::Panel::left("controls")
            .exact_size(RAIL_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::default()
                    .fill(RAIL)
                    .inner_margin(egui::Margin::symmetric(24, 24)),
            )
            .show(ui, |ui| self.rail(ui));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(24, 24)),
            )
            .show(ui, |ui| self.history(ui));
    }
}

impl Inat {
    /// The left column: identity, then every control, top to bottom.
    fn rail(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 16.0;
                self.brand(ui);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 7.0;
                    for mode in [MediaMode::Download, MediaMode::Audio, MediaMode::Clip] {
                        if pill(ui, mode_title(mode), self.mode == mode).clicked() {
                            self.mode = mode;
                            self.format = mode.default_format().to_owned();
                        }
                    }
                });

                self.link_box(ui);

                if self.mode == MediaMode::Clip {
                    self.clip_controls(ui);
                }

                self.options(ui);
                self.destination(ui);
                self.backend_row(ui);

                match &self.tools {
                    Some(Err(error)) => banner(ui, RED, &error.clone()),
                    None => banner(
                        ui,
                        INAT,
                        "Getting ready - unpacking the tools INAT downloads with. \
                         This happens once.",
                    ),
                    Some(Ok(_)) => {}
                }
                if let Some(notice) = self.notice.clone() {
                    banner(ui, AMBER, &notice);
                }
                ui.add_space(8.0);
            });
    }

    fn brand(&self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            match &self.logo {
                Some(logo) => logo.show(ui, 34.0),
                None => {
                    ui.label(RichText::new("inat").size(32.0).color(TEXT).strong());
                }
            }
            ui.label(RichText::new("Video downloader").size(13.0).color(DIM));
        });
        ui.add_space(4.0);
    }

    fn link_box(&mut self, ui: &mut egui::Ui) {
        let mut submit = false;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(RichText::new("Link").size(13.0).color(DIM));

            let field = ui.add_sized(
                vec2(ui.available_width(), FIELD_HEIGHT),
                TextEdit::singleline(&mut self.url)
                    .hint_text("Paste a video link")
                    .vertical_align(Align::Center)
                    .margin(egui::Margin::symmetric(14, 10))
                    .font(egui::FontId::proportional(15.0))
                    .text_color(TEXT),
            );
            // The window opens for one reason: to be given a link. Putting the
            // caret there means a fresh start is paste, Enter, done.
            if !self.greeted {
                self.greeted = true;
                field.request_focus();
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            }

            // The one button the window is really for, lit along its top edge
            // the way the mark is.
            if ui
                .add_sized(
                    vec2(ui.available_width(), FIELD_HEIGHT),
                    egui::Button::new(
                        RichText::new(action_label(self.mode))
                            .size(15.0)
                            .color(INK)
                            .strong(),
                    )
                    .fill(INAT)
                    .stroke(Stroke::new(1.0, INAT_LIT.gamma_multiply(0.55)))
                    .corner_radius(CornerRadius::same(CONTROL_RADIUS)),
                )
                .clicked()
            {
                submit = true;
            }
        });
        if submit {
            self.start_job();
        }
    }

    fn clip_controls(&mut self, ui: &mut egui::Ui) {
        section(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new("From").size(13.0).color(DIM));
                stamp_field(ui, &mut self.start, "00:00");
                ui.add_space(4.0);
                ui.label(RichText::new("To").size(13.0).color(DIM));
                stamp_field(ui, &mut self.end, "end");
            });
            ui.add_space(10.0);
            ui.checkbox(&mut self.accurate, RichText::new("Exact cut").color(TEXT))
                .on_hover_text(
                    "Cuts land on the nearest keyframe unless this is on, which re-encodes at the \
                     cut points so the range starts and ends exactly where you asked. Slower.",
                );
        });
    }

    fn options(&mut self, ui: &mut egui::Ui) {
        section(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;

            if self.mode != MediaMode::Audio {
                labelled(ui, "Quality", |ui| {
                    let mut quality = self.settings.quality.clone();
                    if dropdown(ui, "quality", &mut quality, QUALITY_CHOICES) {
                        self.settings.quality = quality;
                        self.persist();
                    }
                });
            }

            labelled(ui, "Format", |ui| {
                let mut format = self.format.clone();
                if dropdown(ui, "format", &mut format, self.mode.formats()) {
                    self.format = format;
                }
            });

            ui.checkbox(
                &mut self.playlist,
                RichText::new("Whole playlist").color(TEXT),
            )
            .on_hover_text("Allow a channel or playlist link to download every item.");

            ui.checkbox(&mut self.overwrite, RichText::new("Overwrite").color(TEXT))
                .on_hover_text(
                    "A file that is already in the folder is normally skipped. Turn this on to \
                     download it again and replace it.",
                );
        });
    }

    fn destination(&mut self, ui: &mut egui::Ui) {
        section(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(RichText::new("Save to").size(13.0).color(DIM));
            ui.label(
                RichText::new(self.settings.folder.display().to_string())
                    .size(13.0)
                    .color(TEXT),
            );
            if ui
                .add_sized(
                    vec2(ui.available_width(), CONTROL_HEIGHT),
                    egui::Button::new(RichText::new("Choose folder").color(TEXT))
                        .fill(RAISED)
                        .corner_radius(CornerRadius::same(CONTROL_RADIUS)),
                )
                .clicked()
                && let Some(picked) = rfd::FileDialog::new()
                    .set_directory(&self.settings.folder)
                    .pick_folder()
            {
                self.settings.folder = picked;
                self.persist();
            }
        });
    }

    /// yt-dlp is the half of INAT that goes stale: sites change, and a build a
    /// few months old starts failing on them. This is the one button that fixes
    /// that, and it fetches straight from yt-dlp's own releases.
    fn backend_row(&mut self, ui: &mut egui::Ui) {
        let version = self
            .backend
            .version
            .clone()
            .unwrap_or_else(|| "checking...".to_owned());
        let note = self.backend.note.clone();
        let busy = self.backend.working || self.tools.is_none();
        let mut asked = false;

        section(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Downloader").size(13.0).color(DIM));
                ui.label(RichText::new(version).size(13.0).color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if busy { "Working" } else { "Update" };
                    let response = action(ui, label, 92.0, if busy { DIM } else { TEXT })
                        .on_hover_text("Fetches the newest yt-dlp from its own releases.");
                    if response.clicked() && !busy {
                        asked = true;
                    }
                });
            });
            if let Some(note) = note {
                ui.label(RichText::new(note).size(12.0).color(DIM));
            }
        });

        if asked {
            self.update_backend();
        }
    }

    /// The right column: what has happened and what is happening.
    fn history(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Downloads").size(19.0).color(TEXT).strong());
            let active = self.jobs.iter().filter(|job| job.is_running()).count();
            if active > 0 {
                ui.label(
                    RichText::new(format!("{active} running"))
                        .size(13.0)
                        .color(INAT),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if action(ui, "Open folder", 112.0, TEXT)
                    .on_hover_text(self.settings.folder.display().to_string())
                    .clicked()
                {
                    open_folder(&self.settings.folder);
                }
                if self.jobs.iter().any(Job::is_done)
                    && ui
                        .add(
                            egui::Button::new(
                                RichText::new("Clear finished").size(13.0).color(DIM),
                            )
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE),
                        )
                        .clicked()
                {
                    self.jobs.retain(|job| !job.is_done());
                    self.remember();
                }
            });
        });
        ui.add_space(14.0);

        if self.jobs.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label(RichText::new("Nothing here yet").size(15.0).color(TEXT));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Downloads you start will appear on this side.")
                        .size(13.0)
                        .color(DIM),
                );
            });
            return;
        }

        let mut asked = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                for job in &self.jobs {
                    let act = job.card(ui);
                    if act != Act::Nothing {
                        asked = Some((job.id, act));
                    }
                }
            });

        match asked {
            Some((id, Act::End)) => self.drop_job(id),
            Some((id, Act::Hold)) => self.hold(id),
            Some((id, Act::Resume)) => self.resume(id),
            _ => {}
        }
    }

    /// Stops a job for good and takes it off the list. Cancelling one that has
    /// already finished is a no-op, so one button can mean both "stop this"
    /// and "clear this".
    fn drop_job(&mut self, id: u64) {
        if let Some(job) = self.jobs.iter().find(|job| job.id == id)
            && let Some(handle) = &job.handle
        {
            handle.cancel.cancel();
        }
        self.jobs.retain(|job| job.id != id);
        self.remember();
    }

    /// Holds a job where it is. yt-dlp is stopped, but the part-downloaded file
    /// it leaves in the working folder is what resuming picks back up.
    fn hold(&mut self, id: u64) {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
            if let Some(handle) = &job.handle {
                handle.cancel.cancel();
            }
            job.paused = true;
            job.stage = "Paused".to_owned();
        }
        self.remember();
    }

    fn resume(&mut self, id: u64) {
        self.notice = None;
        let Some(tools) = self.backends() else {
            return;
        };
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };

        match job.spec.clone().plan(&tools) {
            Ok(plan) => {
                job.paused = false;
                job.outcome = None;
                job.attempt = 1;
                job.fraction = None;
                job.parts.clear();
                job.stage = "Starting".to_owned();
                job.handle = Some(runner::start(plan));
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        self.remember();
    }

    /// Writes the list down, so closing the window neither loses a job that is
    /// still going nor forgets the ones that already landed.
    ///
    /// A run that failed is the one thing not worth keeping: what made it worth
    /// looking at was the reason it gave, and that is gone once the window is.
    fn remember(&mut self) {
        let pending = self
            .jobs
            .iter()
            .filter(|job| !job.is_done() || job.saved_path().is_some())
            .map(Job::pending)
            .collect::<Vec<_>>();
        if let Err(error) = queue::save(&pending) {
            self.notice = Some(error.to_string());
        }
    }
}

/// What a card's buttons asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Nothing,
    /// Stop while running, dismiss once finished.
    End,
    Hold,
    Resume,
}

/// One download, and everything the window knows about how it is going.
struct Job {
    id: u64,
    mode: MediaMode,
    /// What was asked for, kept so the job can be started again - after a
    /// pause, or after the window has been closed and reopened.
    spec: JobSpec,
    /// Held rather than stopped: the part-downloaded file stays where it is
    /// and the job can pick up from it.
    paused: bool,
    /// The link, shown until yt-dlp resolves a real title.
    label: String,
    heading: String,
    stage: String,
    fraction: Option<f64>,
    parts: Vec<String>,
    /// Which try this is. Stays above one after the fact, so a download that
    /// only worked on the third go still says so.
    attempt: u32,
    /// A frame of the finished file, once there is one to show.
    poster: Option<TextureHandle>,
    outcome: Option<Outcome>,
    handle: Option<Handle>,
}

impl Job {
    fn new(id: u64, spec: JobSpec, handle: Handle) -> Self {
        Self {
            id,
            mode: spec.mode,
            label: spec.url.clone(),
            spec,
            paused: false,
            heading: String::new(),
            stage: "Starting".to_owned(),
            fraction: None,
            parts: Vec::new(),
            attempt: 1,
            poster: None,
            outcome: None,
            handle: Some(handle),
        }
    }

    /// A job read back from the last session: either one that already landed,
    /// which stays on the list as history, or one held exactly where the window
    /// closed on it, waiting to be picked up.
    fn restored(id: u64, pending: Pending) -> Self {
        let label = if pending.title.is_empty() {
            pending.spec.url.clone()
        } else {
            pending.title
        };
        // A finished job is described entirely by where its file ended up.
        let outcome = pending.done.then(|| Outcome {
            items: 1,
            report: RunReport {
                destination: pending.saved,
                ..RunReport::default()
            },
            ..Outcome::default()
        });
        Self {
            id,
            mode: pending.spec.mode,
            spec: pending.spec,
            paused: outcome.is_none(),
            label,
            heading: String::new(),
            stage: String::new(),
            fraction: None,
            parts: Vec::new(),
            attempt: 1,
            poster: None,
            outcome,
            handle: None,
        }
    }

    fn is_running(&self) -> bool {
        self.outcome.is_none() && !self.paused
    }

    /// Held, and still here to be resumed. True from the moment pause is
    /// pressed, before the tool has finished winding down.
    fn is_held(&self) -> bool {
        self.paused
    }

    /// Over, one way or another. The only jobs "Clear finished" may remove.
    fn is_done(&self) -> bool {
        self.outcome.is_some() && !self.paused
    }

    /// What to write down so this job survives the window closing.
    fn pending(&self) -> Pending {
        Pending {
            spec: self.spec.clone(),
            title: self.title().to_owned(),
            saved: self.saved_path().map(str::to_owned),
            done: self.is_done(),
        }
    }

    /// The finished file, when the job produced one.
    fn saved_path(&self) -> Option<&str> {
        let outcome = self.outcome.as_ref()?;
        if !outcome.succeeded() {
            return None;
        }
        outcome
            .report
            .destination
            .as_deref()
            .or(outcome.report.duplicate.as_deref())
    }

    fn title(&self) -> &str {
        if self.heading.is_empty() {
            &self.label
        } else {
            &self.heading
        }
    }

    /// Returns true when this poll is the one that ended the job.
    fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut drained = Vec::new();
        if let Some(handle) = &self.handle {
            while let Ok(event) = handle.events.try_recv() {
                drained.push(event);
            }
        }
        let mut settled = false;
        for event in drained {
            settled |= matches!(event, Event::Finished(_));
            self.apply(event, ctx);
        }
        settled
    }

    fn apply(&mut self, event: Event, ctx: &egui::Context) {
        match event {
            Event::Started => self.stage = "Reading the link".to_owned(),
            Event::Item(heading) => {
                self.heading = heading;
                self.fraction = None;
                self.parts.clear();
            }
            Event::Stage(stage) => {
                self.stage = stage;
                self.fraction = None;
                self.parts.clear();
            }
            Event::Download(update) => {
                self.stage = "Downloading".to_owned();
                self.fraction = update.fraction();
                self.parts = update.parts();
            }
            Event::Convert { progress, fraction } => {
                self.fraction = fraction;
                self.parts = progress.parts();
            }
            Event::Poster(png) => self.poster = decode_poster(ctx, self.id, &png),
            Event::Retrying { attempt, .. } => {
                self.attempt = attempt;
                self.stage = "Trying again".to_owned();
                self.fraction = None;
                self.parts.clear();
            }
            Event::Finished(outcome) => {
                self.outcome = Some(*outcome);
                self.handle = None;
            }
        }
    }

    /// The picture leads, the way it does anywhere else a list of videos is
    /// shown, and everything written about the job sits beside it in one
    /// column: what it is, how it is going, and what can be done about it.
    fn card(&self, ui: &mut egui::Ui) -> Act {
        card(ui, |ui| {
            let mut act = Act::Nothing;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                thumbnail(ui, self.poster.as_ref());

                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 9.0;
                    self.headline(ui);
                    if self.paused {
                        self.held_rows(ui);
                    } else {
                        match &self.outcome {
                            None => self.running_rows(ui),
                            Some(outcome) => self.finished_rows(ui, outcome),
                        }
                    }
                    act = self.actions(ui);
                });
            });
            act
        })
    }

    /// The title, with the kind of job tagged at the far right. The title is
    /// cut rather than wrapped: a TikTok caption is several lines long and
    /// would otherwise push everything else down the card.
    fn headline(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 9.0;
            status_dot(ui, self.accent());
            let room = (ui.available_width() - TAG_ROOM).max(90.0);
            ui.scope(|ui| {
                ui.set_max_width(room);
                ui.add(
                    egui::Label::new(RichText::new(self.title()).size(15.0).color(TEXT).strong())
                        .truncate(),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                tag(ui, self.mode);
            });
        });
    }

    fn actions(&self, ui: &mut egui::Ui) -> Act {
        let mut act = Act::Nothing;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let end = if self.is_running() { "Stop" } else { "Clear" };
                if action(ui, end, 78.0, DIM).clicked() {
                    act = Act::End;
                }
                if self.is_held() {
                    if action(ui, "Resume", 92.0, TEXT)
                        .on_hover_text(
                            "Carries on from where it stopped rather than starting over.",
                        )
                        .clicked()
                    {
                        act = Act::Resume;
                    }
                } else if self.is_running()
                    && action(ui, "Pause", 88.0, TEXT)
                        .on_hover_text(
                            "Keeps what has downloaded so far. Safe to close the window \
                             afterwards - the job is still here when you open it again.",
                        )
                        .clicked()
                {
                    act = Act::Hold;
                }
            });
        });
        act
    }

    /// A held job. The bar keeps whatever it reached, because that is exactly
    /// what resuming starts from.
    fn held_rows(&self, ui: &mut egui::Ui) {
        progress_bar(ui, self.fraction.or(Some(0.0)));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(RichText::new("Paused").size(13.0).color(AMBER));
            if let Some(fraction) = self.fraction {
                ui.label(
                    RichText::new(format!("{}%", (fraction * 100.0).round() as i64))
                        .size(13.0)
                        .color(AMBER)
                        .strong(),
                );
            }
            ui.label(
                RichText::new("·  Resume picks up where it left off")
                    .size(13.0)
                    .color(DIM),
            );
        });
    }

    fn accent(&self) -> Color32 {
        match &self.outcome {
            None => INAT,
            Some(outcome) if outcome.cancelled => AMBER,
            Some(outcome) if outcome.report.duplicate.is_some() => AMBER,
            Some(outcome) if outcome.succeeded() => INAT,
            Some(_) => RED,
        }
    }

    fn running_rows(&self, ui: &mut egui::Ui) {
        progress_bar(ui, self.fraction);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let stage = if self.stage.is_empty() {
                "Working"
            } else {
                &self.stage
            };
            ui.label(RichText::new(stage).size(13.0).color(TEXT));
            if let Some(fraction) = self.fraction {
                ui.label(
                    RichText::new(format!("{}%", (fraction * 100.0).round() as i64))
                        .size(13.0)
                        .color(INAT)
                        .strong(),
                );
            }
            if !self.parts.is_empty() {
                ui.label(
                    RichText::new(format!("·  {}", self.parts.join("  ·  ")))
                        .size(13.0)
                        .color(DIM),
                );
            }
            if self.attempt > 1 {
                ui.label(
                    RichText::new(format!("·  try {} of {}", self.attempt, runner::ATTEMPTS))
                        .size(13.0)
                        .color(AMBER),
                );
            }
        });
    }

    fn finished_rows(&self, ui: &mut egui::Ui, outcome: &Outcome) {
        if outcome.cancelled {
            ui.label(RichText::new("Stopped").size(13.0).color(DIM));
            return;
        }

        if !outcome.succeeded() {
            let headline = if self.attempt > 1 {
                format!("Download failed after {} tries", self.attempt)
            } else {
                "Download failed".to_owned()
            };
            ui.label(RichText::new(headline).size(13.0).color(RED));
            for line in outcome.report.error_lines(&outcome.stderr) {
                ui.label(RichText::new(line).size(13.0).color(DIM));
            }
            return;
        }

        if let Some(path) = &outcome.report.duplicate {
            ui.label(
                RichText::new("Already downloaded, nothing to do")
                    .size(13.0)
                    .color(AMBER),
            );
            ui.label(RichText::new(path).size(13.0).color(DIM));
            ui.label(
                RichText::new("Turn on Overwrite to download it again.")
                    .size(13.0)
                    .color(DIM),
            );
            return;
        }

        match &outcome.report.destination {
            Some(path) if outcome.items <= 1 => {
                ui.label(
                    RichText::new(format!("Saved{}", size_note(path)))
                        .size(13.0)
                        .color(INAT_SOFT),
                );
                ui.label(RichText::new(path).size(13.0).color(DIM));
            }
            Some(path) => {
                ui.label(
                    RichText::new(format!("Saved {} files", outcome.items))
                        .size(13.0)
                        .color(INAT_SOFT),
                );
                ui.label(RichText::new(folder_of(path)).size(13.0).color(DIM));
            }
            None => {
                ui.label(RichText::new("Done").size(13.0).color(INAT_SOFT));
            }
        }
    }
}

fn mode_title(mode: MediaMode) -> &'static str {
    match mode {
        MediaMode::Download => "Video",
        MediaMode::Audio => "Audio",
        MediaMode::Clip => "Clip",
    }
}

fn action_label(mode: MediaMode) -> &'static str {
    match mode {
        MediaMode::Download => "Download",
        MediaMode::Audio => "Get audio",
        MediaMode::Clip => "Cut clip",
    }
}

fn size_note(path: &str) -> String {
    match file_size(path) {
        Some(bytes) => format!("  ·  {}", human_bytes(bytes as f64)),
        None => String::new(),
    }
}

/// Opens the download folder in the file manager, creating it first if nothing
/// has been saved there yet - an explorer window that opens on nothing at all
/// is worse than one that opens on an empty folder.
fn open_folder(folder: &std::path::Path) {
    let _ = std::fs::create_dir_all(folder);
    #[cfg(windows)]
    let _ = Command::new("explorer.exe").arg(folder).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(folder).spawn();
}

// ------------------------------------------------------------------ posters

/// The still the runner grabbed, turned into something the window can draw.
/// eframe already carries a PNG decoder for the window icon, which is exactly
/// the format FFmpeg was asked to write.
fn decode_poster(ctx: &egui::Context, id: u64, png: &[u8]) -> Option<TextureHandle> {
    let decoded = eframe::icon_data::from_png_bytes(png).ok()?;
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [decoded.width as usize, decoded.height as usize],
        &decoded.rgba,
    );
    Some(ctx.load_texture(
        format!("poster-{id}"),
        image,
        egui::TextureOptions::LINEAR,
    ))
}

/// The picture at the head of a card, always the same size whether or not
/// there is one yet: an empty frame that fills in is far steadier to watch
/// than a card that jumps sideways the moment a still arrives.
///
/// The image is fitted rather than cropped, so a portrait video keeps its
/// whole frame and sits in the same column as a landscape one.
fn thumbnail(ui: &mut egui::Ui, poster: Option<&TextureHandle>) {
    const BOX: Vec2 = Vec2 { x: 120.0, y: 92.0 };
    let radius = CornerRadius::same(10);

    let (rect, _) = ui.allocate_exact_size(BOX, egui::Sense::hover());
    ui.painter().rect_filled(rect, radius, FIELD);

    match poster {
        Some(poster) => {
            let source = poster.size_vec2();
            let scale = (BOX.x / source.x.max(1.0)).min(BOX.y / source.y.max(1.0));
            let size = source * scale;
            egui::Image::new(egui::load::SizedTexture::new(poster.id(), size))
                .corner_radius(radius)
                .paint_at(ui, Rect::from_center_size(rect.center(), size));
        }
        None => {
            // A play mark, so an empty frame still reads as "a video goes here"
            // rather than as a hole in the card.
            let centre = rect.center();
            let (wide, tall) = (7.0, 9.0);
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    pos2(centre.x - wide * 0.5, centre.y - tall),
                    pos2(centre.x - wide * 0.5, centre.y + tall),
                    pos2(centre.x + wide * 1.2, centre.y),
                ],
                LINE,
                Stroke::NONE,
            ));
        }
    }

    ui.painter()
        .rect_stroke(rect, radius, Stroke::new(1.0, LINE), egui::StrokeKind::Inside);
}

// --------------------------------------------------------------------- brand

/// The wordmark, decoded once and trimmed of the transparent margin the source
/// PNG carries so it can sit flush against the text below it.
struct Logo {
    texture: TextureHandle,
    aspect: f32,
}

impl Logo {
    fn load(ctx: &egui::Context) -> Option<Self> {
        let icon = eframe::icon_data::from_png_bytes(LOGO_PNG).ok()?;
        let (rgba, width, height) =
            trim_transparent(&icon.rgba, icon.width as usize, icon.height as usize)?;
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba);
        Some(Self {
            texture: ctx.load_texture("inat-logo", image, egui::TextureOptions::LINEAR),
            aspect: width as f32 / height as f32,
        })
    }

    fn show(&self, ui: &mut egui::Ui, height: f32) {
        let size = vec2(height * self.aspect, height);
        ui.add(egui::Image::new(egui::load::SizedTexture::new(
            self.texture.id(),
            size,
        )));
    }
}

/// The taskbar icon, made as large as a square can hold.
///
/// The source PNG is a 1254 square with the wordmark sitting in a 1032x506
/// band in the middle of it, so more than half the picture is empty air. Handed
/// over as-is, Windows fits that whole square into the taskbar and the mark
/// lands at under half the height it could have had. Trimming the empty air and
/// squaring up what is left spends the icon on the logo instead.
fn window_icon() -> Option<egui::IconData> {
    let source = eframe::icon_data::from_png_bytes(LOGO_PNG).ok()?;
    let (rgba, width, height) =
        trim_transparent(&source.rgba, source.width as usize, source.height as usize)?;

    // A hair of margin, so the mark does not sit hard against the edge.
    let side = width.max(height) * 26 / 25;
    let (left, top) = ((side - width) / 2, (side - height) / 2);
    let mut square = vec![0u8; side * side * 4];
    for row in 0..height {
        let from = row * width * 4;
        let to = ((top + row) * side + left) * 4;
        square[to..to + width * 4].copy_from_slice(&rgba[from..from + width * 4]);
    }

    Some(egui::IconData {
        rgba: square,
        width: side as u32,
        height: side as u32,
    })
}

/// The bounding box of everything that is not fully transparent.
fn trim_transparent(rgba: &[u8], width: usize, height: usize) -> Option<(Vec<u8>, usize, usize)> {
    if width == 0 || height == 0 || rgba.len() < width * height * 4 {
        return None;
    }
    let (mut left, mut top) = (width, height);
    let (mut right, mut bottom) = (0usize, 0usize);
    for y in 0..height {
        for x in 0..width {
            if rgba[(y * width + x) * 4 + 3] > 8 {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    if right < left || bottom < top {
        return None;
    }

    let (cropped_width, cropped_height) = (right - left + 1, bottom - top + 1);
    let mut out = Vec::with_capacity(cropped_width * cropped_height * 4);
    for y in top..=bottom {
        let start = (y * width + left) * 4;
        out.extend_from_slice(&rgba[start..start + cropped_width * 4]);
    }
    Some((out, cropped_width, cropped_height))
}

// ---------------------------------------------------------------- components

/// A card in the history column.
fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(egui::Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui)
        })
        .inner
}

/// A grouped block of controls in the left rail.
fn section<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui)
        })
        .inner
}

/// A dim caption on the left, its control pinned to the right.
fn labelled(ui: &mut egui::Ui, label: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}

/// What kind of job this is, as a small label at the top right of its card.
/// Deliberately quiet: it says which of three things you asked for, which is
/// worth knowing at a glance and worth nothing more than that.
fn tag(ui: &mut egui::Ui, mode: MediaMode) {
    egui::Frame::default()
        .fill(RAISED)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(mode.label()).size(11.0).color(DIM).strong());
        });
}

/// A button on a card.
fn action(ui: &mut egui::Ui, label: &str, width: f32, colour: Color32) -> egui::Response {
    ui.add_sized(
        vec2(width, CONTROL_HEIGHT),
        egui::Button::new(RichText::new(label).size(13.0).color(colour))
            .fill(RAISED)
            .corner_radius(CornerRadius::same(CONTROL_RADIUS)),
    )
}

fn pill(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let text = RichText::new(label)
        .size(14.0)
        .color(if active { INK } else { DIM })
        .strong();
    ui.add_sized(
        vec2(122.0, CONTROL_HEIGHT + 2.0),
        egui::Button::new(text)
            .fill(if active { INAT } else { RAISED })
            .stroke(Stroke::new(1.0, if active { INAT } else { LINE }))
            .corner_radius(CornerRadius::same(18)),
    )
}

fn stamp_field(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.add_sized(
        vec2(92.0, CONTROL_HEIGHT),
        TextEdit::singleline(value)
            .hint_text(hint)
            .vertical_align(Align::Center)
            .margin(egui::Margin::symmetric(10, 7))
            .text_color(TEXT),
    );
}

/// Returns true when the selection changed. The menu is sized to hold every
/// choice outright, so it never grows a scrollbar.
fn dropdown(ui: &mut egui::Ui, id: &str, value: &mut String, choices: &[&str]) -> bool {
    let mut changed = false;
    let row = CONTROL_HEIGHT + 4.0;
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(value.as_str()).color(TEXT))
        .width(128.0)
        .height(row * choices.len() as f32 + 16.0)
        .show_ui(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for choice in choices {
                if ui
                    .selectable_label(value == choice, RichText::new(*choice).color(TEXT))
                    .clicked()
                {
                    *value = (*choice).to_owned();
                    changed = true;
                }
            }
        });
    changed
}

/// A rounded track with a green fill. With no known percentage it shows a
/// segment sliding along instead, so a stalled-looking bar still reads as work.
fn progress_bar(ui: &mut egui::Ui, fraction: Option<f64>) {
    let height = 8.0;
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
    let radius = CornerRadius::same(4);
    ui.painter().rect_filled(rect, radius, FIELD);

    match fraction {
        Some(fraction) => {
            let filled = rect.width() * fraction.clamp(0.0, 1.0) as f32;
            if filled >= 1.0 {
                let bar = Rect::from_min_size(rect.min, vec2(filled, rect.height()));
                ui.painter().rect_filled(bar, radius, INAT_DEEP);
                // A lit top edge over a deeper body, so the bar has a surface
                // rather than being a flat block of colour.
                let lit = Rect::from_min_size(bar.min, vec2(filled, bar.height() * 0.55));
                ui.painter().rect_filled(lit, radius, INAT);
            }
        }
        None => {
            let time = ui.input(|input| input.time) as f32;
            let phase = (time * 0.55).rem_euclid(1.0);
            let segment = rect.width() * 0.3;
            let start = (rect.left() + (rect.width() + segment) * phase - segment).max(rect.left());
            let end = (start + segment).min(rect.right());
            if end > start {
                let bar = Rect::from_min_max(pos2(start, rect.top()), pos2(end, rect.bottom()));
                ui.painter().rect_filled(bar, radius, INAT_DEEP);
            }
            ui.ctx().request_repaint();
        }
    }
}

fn status_dot(ui: &mut egui::Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, colour);
}

/// A tinted strip for something the user needs to read: a missing tool, a
/// rejected link.
fn banner(ui: &mut egui::Ui, colour: Color32, message: &str) {
    egui::Frame::default()
        .fill(colour.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, colour.gamma_multiply(0.5)))
        .corner_radius(CornerRadius::same(CONTROL_RADIUS))
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(message).size(13.0).color(colour));
        });
}

/// One dark theme, applied to both slots so the OS light/dark setting never
/// changes the look.
fn install_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(restyle);
}

fn restyle(style: &mut egui::Style) {
    use egui::{FontFamily::Proportional, FontId, TextStyle};

    style.text_styles = [
        (TextStyle::Heading, FontId::new(24.0, Proportional)),
        (TextStyle::Body, FontId::new(14.5, Proportional)),
        (TextStyle::Button, FontId::new(14.5, Proportional)),
        (TextStyle::Small, FontId::new(12.5, Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(13.5, egui::FontFamily::Monospace),
        ),
    ]
    .into();

    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(14.0, 8.0);
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.spacing.icon_width = 18.0;
    style.spacing.icon_width_inner = 10.0;
    style.spacing.scroll.bar_width = 9.0;

    let visuals = &mut style.visuals;
    visuals.dark_mode = true;
    visuals.panel_fill = BG;
    visuals.window_fill = CARD;
    visuals.extreme_bg_color = FIELD;
    visuals.faint_bg_color = RAISED;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.window_corner_radius = CornerRadius::same(CONTROL_RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(CONTROL_RADIUS);
    visuals.selection.bg_fill = INAT_DARK.gamma_multiply(0.55);
    visuals.selection.stroke = Stroke::new(1.0, INAT_SOFT);
    visuals.hyperlink_color = INAT;

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(CONTROL_RADIUS);
        widget.bg_fill = RAISED;
        widget.weak_bg_fill = RAISED;
        widget.bg_stroke = Stroke::new(1.0, LINE);
        widget.fg_stroke = Stroke::new(1.0, TEXT);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = CARD;
    visuals.widgets.noninteractive.weak_bg_fill = CARD;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, DIM);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, INAT_LIT.gamma_multiply(0.55));
    visuals.widgets.hovered.expansion = 1.0;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, INAT);
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, LINE);
}
