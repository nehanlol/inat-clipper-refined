//! INAT's core: link handling, argument building, tool discovery, and the
//! runner that drives yt-dlp and FFmpeg.
//!
//! Nothing in here prints. A run is a stream of [`runner::Event`]s, which is
//! what lets the terminal front end and the window front end show the same
//! download without either one knowing about the other.

pub mod error;
pub mod job;
pub mod progress;
pub mod queue;
pub mod runner;
pub mod settings;
pub mod tools;

pub use error::AppError;
pub use job::{ClipRange, JobSpec, MediaMode, Plan};
pub use progress::{FfmpegProgress, ProgressUpdate, RunReport, human_bytes, human_duration};
pub use queue::Pending;
pub use runner::{Cancel, Event, Handle, Outcome};
pub use settings::Settings;
pub use tools::Tools;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::MediaMode;
    use super::job::*;
    use super::progress::*;

    #[test]
    fn accepts_supported_timestamps() {
        assert_eq!(parse_timestamp("45"), Some(45.0));
        assert_eq!(parse_timestamp("01:20"), Some(80.0));
        assert_eq!(parse_timestamp("1:02:03.5"), Some(3723.5));
    }

    #[test]
    fn rejects_invalid_timestamps() {
        assert_eq!(parse_timestamp("1:60"), None);
        assert_eq!(parse_timestamp("-1"), None);
        assert_eq!(parse_timestamp("1:2:3:4"), None);
        assert_eq!(parse_timestamp("hello"), None);
    }

    #[test]
    fn rejects_backtracking_filename_template() {
        assert!(validate_filename("../outside.%(ext)s").is_err());
        assert!(validate_filename("video.%(ext)s").is_ok());
    }

    #[test]
    fn refuses_feed_links_that_would_download_a_whole_account() {
        assert!(bulk_url_reason("https://www.tiktok.com/@someone").is_some());
        assert!(bulk_url_reason("https://www.tiktok.com/@someone/").is_some());
        assert!(bulk_url_reason("https://www.tiktok.com/tag/fyp").is_some());
        assert!(bulk_url_reason("https://www.youtube.com/@channel/videos").is_some());
        assert!(bulk_url_reason("https://www.youtube.com/playlist?list=PL1").is_some());
    }

    #[test]
    fn allows_single_video_links() {
        assert!(bulk_url_reason("https://www.tiktok.com/@someone/video/7653521104").is_none());
        assert!(bulk_url_reason("https://www.tiktok.com/@someone/photo/7653521104").is_none());
        assert!(bulk_url_reason("https://vm.tiktok.com/ZMabcdef").is_none());
        assert!(bulk_url_reason("https://www.youtube.com/watch?v=abc123").is_none());
        assert!(bulk_url_reason("https://youtu.be/abc123").is_none());
    }

    #[test]
    fn drops_share_tracking_but_keeps_routing_queries() {
        assert_eq!(
            normalize_url("  https://www.tiktok.com/@a/video/77?is_from_webapp=1&sender=pc "),
            "https://www.tiktok.com/@a/video/77"
        );
        assert_eq!(
            normalize_url("\"https://www.youtube.com/watch?v=abc123\""),
            "https://www.youtube.com/watch?v=abc123"
        );
    }

    #[test]
    fn names_clips_after_their_time_range() {
        assert_eq!(
            ClipRange::new("01:20", Some("03:45")).unwrap().label(),
            "01.20-03.45"
        );
        assert_eq!(
            ClipRange::new("1:02:03", Some("1:02:30")).unwrap().label(),
            "1.02.03-1.02.30"
        );
        assert_eq!(ClipRange::new("01:20", None).unwrap().label(), "01.20-end");
    }

    #[test]
    fn clips_without_an_end_run_to_the_video_end() {
        assert_eq!(ClipRange::new("90", None).unwrap().section(), "*90-inf");
        assert_eq!(ClipRange::new("90", Some("")).unwrap().section(), "*90-inf");
        assert_eq!(ClipRange::new("30", Some("45")).unwrap().section(), "*30-45");
        assert!(ClipRange::new("45", Some("30")).is_err());
    }

    #[test]
    fn a_clip_knows_how_long_it_should_be() {
        assert_eq!(
            ClipRange::new("30", Some("45")).unwrap().duration(),
            Some(15.0)
        );
        assert_eq!(
            ClipRange::new("0", Some("02:00")).unwrap().duration(),
            Some(120.0)
        );
        assert_eq!(ClipRange::new("30", None).unwrap().duration(), None);
    }

    #[test]
    fn names_each_kind_of_download_differently() {
        assert_eq!(MediaMode::Download.default_filename(), "%(title)s.%(ext)s");
        assert_eq!(
            MediaMode::Audio.default_filename(),
            "%(title)s [AUDIO].%(ext)s"
        );
        // A clip starts from the plain title; `plan` appends its time range.
        assert_eq!(MediaMode::Clip.default_filename(), "%(title)s.%(ext)s");
    }

    #[test]
    fn parses_progress_lines_and_ignores_malformed_ones() {
        let line = "downloading|1048576|10485760|NA|524288.0|18|NA|NA|2|5|My Video | Part 1";
        let update = ProgressUpdate::parse(line).expect("well-formed progress line");
        assert_eq!(update.fraction(), Some(0.1));
        assert_eq!(update.speed, Some(524288.0));
        assert_eq!(update.playlist_index, Some(2));
        assert_eq!(update.title, "My Video | Part 1");
        assert!(ProgressUpdate::parse("downloading|1|2").is_none());
    }

    #[test]
    fn falls_back_to_fragment_counts_when_size_is_unknown() {
        let line = "downloading|NA|NA|NA|NA|NA|3|12|NA|NA|Stream";
        let update = ProgressUpdate::parse(line).expect("well-formed progress line");
        assert_eq!(update.fraction(), Some(0.25));
    }

    #[test]
    fn numbers_a_playlist_item_in_its_heading() {
        let line = "downloading|1|2|NA|NA|NA|NA|NA|2|5|Second Video";
        let update = ProgressUpdate::parse(line).expect("well-formed progress line");
        assert_eq!(update.heading().as_deref(), Some("[2/5] Second Video"));

        let single = "downloading|1|2|NA|NA|NA|NA|NA|1|1|Only Video";
        let update = ProgressUpdate::parse(single).expect("well-formed progress line");
        assert_eq!(update.heading().as_deref(), Some("Only Video"));

        let untitled = "downloading|1|2|NA|NA|NA|NA|NA|NA|NA|NA";
        let update = ProgressUpdate::parse(untitled).expect("well-formed progress line");
        assert_eq!(update.heading(), None);
    }

    #[test]
    fn recognizes_duplicate_and_destination_lines() {
        assert_eq!(
            already_downloaded_path("[download] C:\\Videos\\clip.mp4 has already been downloaded"),
            Some("C:\\Videos\\clip.mp4".to_owned())
        );
        assert_eq!(
            destination_path("[download] Destination: C:\\Videos\\clip.f137.mp4"),
            Some("C:\\Videos\\clip.f137.mp4".to_owned())
        );
        assert_eq!(
            destination_path("[Merger] Merging formats into \"C:\\Videos\\clip.mp4\""),
            Some("C:\\Videos\\clip.mp4".to_owned())
        );
        assert_eq!(destination_path("[youtube] abc: Downloading webpage"), None);
    }

    /// The merge line names the finished file. Treating it as *only* a stage
    /// change leaves the last fragment (`...f251-15.webm`) on screen as the
    /// result, and "Show file" then points at something already deleted.
    #[test]
    fn the_merge_line_reports_both_the_stage_and_the_finished_file() {
        let signals = classify("[Merger] Merging formats into \"C:\\Videos\\clip.mp4\"");
        assert!(
            signals
                .iter()
                .any(|signal| matches!(signal, Signal::Stage(_)))
        );
        let destination = signals.iter().find_map(|signal| match signal {
            Signal::Destination(path) => Some(path.as_str()),
            _ => None,
        });
        assert_eq!(destination, Some("C:\\Videos\\clip.mp4"));
    }

    #[test]
    fn surfaces_what_the_tool_says_it_is_doing() {
        let stage = |line: &str| {
            classify(line).into_iter().find_map(|signal| match signal {
                Signal::Stage(text) => Some(text),
                _ => None,
            })
        };
        assert_eq!(
            stage("[youtube] ba6-_MMxF4E: Downloading webpage").as_deref(),
            Some("Downloading webpage")
        );
        assert_eq!(
            stage("[info] ba6-_MMxF4E: Downloading 1 format(s): 399+251-15").as_deref(),
            Some("Downloading 1 format(s): 399+251-15")
        );
        // The bare URL echo is noise, and progress lines are not stages.
        assert_eq!(stage("[youtube] Extracting URL: https://example.com/x"), None);
        assert_eq!(stage("[download] Destination: C:\\Videos\\clip.mp4"), None);
    }

    #[test]
    fn reads_ffmpeg_status_lines() {
        let line = "frame= 1234 fps= 30 q=-1.0 size=   10240kB time=00:00:41.00 bitrate=2045.6kbits/s speed=1.02x";
        let update = FfmpegProgress::parse(line).expect("well-formed ffmpeg status");
        assert_eq!(update.seconds, 41.0);
        assert_eq!(update.bytes, Some(10_485_760.0));
        assert_eq!(update.speed.as_deref(), Some("1.02x"));
        assert_eq!(update.fraction(Some(82.0)), Some(0.5));
        assert_eq!(update.fraction(None), None);
    }

    #[test]
    fn ignores_ffmpeg_lines_that_carry_no_time() {
        assert!(FfmpegProgress::parse("  Duration: 00:10:34.60, start: 0.000000").is_none());
        assert!(FfmpegProgress::parse("size=N/A time=N/A bitrate=N/A").is_none());
        assert!(FfmpegProgress::parse("[mp4 @ 0x1] track 1: codec frame size is not set").is_none());
    }

    #[test]
    fn reports_the_error_lines_worth_showing() {
        let report = RunReport {
            errors: vec!["ERROR: video unavailable".to_owned()],
            ..RunReport::default()
        };
        assert_eq!(report.error_lines(&[]), vec!["video unavailable"]);

        // Nothing marked ERROR falls back to the tail of stderr.
        let quiet = RunReport::default();
        let stderr = vec!["first".to_owned(), "last".to_owned()];
        assert_eq!(quiet.error_lines(&stderr), vec!["first", "last"]);
    }

    /// The cover image is announced before a byte of video moves, which is the
    /// whole reason this line is worth reading: it is the earliest a card can
    /// be given a picture.
    #[test]
    fn finds_the_cover_image_yt_dlp_just_wrote() {
        let thumbnail = |line: &str| {
            classify(line).into_iter().find_map(|signal| match signal {
                Signal::Thumbnail(path) => Some(path),
                _ => None,
            })
        };
        assert_eq!(
            thumbnail("[info] Writing video thumbnail originCover to: C:\\work\\clip.image")
                .as_deref(),
            Some("C:\\work\\clip.image")
        );
        assert_eq!(
            thumbnail("[info] Writing video thumbnail to: C:\\work\\a b.webp").as_deref(),
            Some("C:\\work\\a b.webp")
        );
        // Downloading one is not the same as having written it.
        assert_eq!(thumbnail("[info] Downloading video thumbnail originCover ..."), None);
        assert_eq!(thumbnail("[info] Writing video subtitles to: C:\\work\\a.srt"), None);
        // And it must not be mistaken for the file that was asked for.
        assert_eq!(
            destination_path("[info] Writing video thumbnail originCover to: C:\\work\\clip.image"),
            None
        );
    }

    /// A run is tried three times before it gives up, so it matters that the
    /// failures nothing can fix are not among the three.
    #[test]
    fn knows_which_failures_another_try_cannot_fix() {
        let settled = |message: &str| {
            RunReport {
                errors: vec![format!("ERROR: {message}")],
                ..RunReport::default()
            }
            .permanent(&[])
        };
        assert!(settled("Video unavailable. This video is private"));
        assert!(settled("Unsupported URL: https://example.com/x"));
        assert!(settled("Requested format is not available"));
        // A connection that dropped mid-download is exactly what retrying is for.
        assert!(!settled("unable to download video data: <urlopen error timed out>"));
        assert!(!settled("HTTP Error 500: Internal Server Error"));
        // stderr counts too, since not every failure announces itself as ERROR.
        let quiet = RunReport::default();
        assert!(quiet.permanent(&["WARNING: Sign in to confirm you are not a bot".to_owned()]));
        assert!(!quiet.permanent(&["giving up after 10 fragment retries".to_owned()]));
    }

    #[test]
    fn formats_sizes_and_durations() {
        assert_eq!(human_bytes(1536.0), "1.5 KB");
        assert_eq!(human_duration(75.0), "01:15");
        assert_eq!(human_duration(3725.0), "1:02:05");
    }
}
