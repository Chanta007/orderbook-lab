//! Tailing the daemon's log, so a view can show what the daemon says about itself.
//!
//! The daemon publishes its queues, its consumers and its pressure state once a
//! second on its own log lines (`docs/design/observability.md` §4.2). A view reads
//! the end of that file rather than asking the daemon for them, which is what keeps
//! this off the wire format that three implementations share.
//!
//! # What it costs
//!
//! One `stat` and one seek-and-read of the last [`TAIL_BYTES`] every
//! [`INTERVAL`] — a thousandth of a second, a few times a second, on a thread that
//! is already sleeping 100 ms per frame. The file is append-only and can grow
//! without bound, which is why the tail is read by seeking from the end rather than
//! by keeping an offset from the start: a log left running for a day is hundreds of
//! megabytes, and re-reading it is the one thing this must not do.
//!
//! A rotated or truncated file is noticed by the size going backwards, and is read
//! from the start of the new file.

use ob_view::{parse_report, DaemonReport};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How much of the end of the log to read.
///
/// One report block is a few hundred bytes; 16 KiB holds many, which covers a
/// daemon that has been quiet for a while as well as one whose block was written
/// while the reader was asleep. Reading more costs little and reading less risks
/// finding no report in a tail that starts mid-block.
pub const TAIL_BYTES: u64 = 16 * 1024;

/// How often to look.
pub const INTERVAL: Duration = Duration::from_millis(500);

/// Where the daemon's log is likely to be, given the config a window was opened
/// with.
///
/// Two candidates, in order: `var/run/<config>/feedd.log` relative to the current
/// directory, which is where `obctl` puts it and where a window started from the
/// repository root finds it; and the same path under the config's own grandparent,
/// which is what a config named by an absolute path inside the repository resolves
/// to. The first that exists wins, and when neither does the first is returned so a
/// frame can name the place it looked.
///
/// A heuristic rather than a config key, and deliberately: the run directory is
/// `obctl`'s arrangement of the config's name, not a property of the lab, and adding
/// a key to the config schema would put it in front of three implementations to
/// agree on.
pub fn locate(config_path: &Path) -> PathBuf {
    let candidates = candidates(config_path);
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// The candidate paths for a config, in the order they are tried.
pub fn candidates(config_path: &Path) -> Vec<PathBuf> {
    let stem = config_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "dev".to_string());
    let mut out = vec![PathBuf::from("var")
        .join("run")
        .join(&stem)
        .join("feedd.log")];
    if let Some(root) = config_path.parent().and_then(|p| p.parent()) {
        if !root.as_os_str().is_empty() {
            out.push(root.join("var").join("run").join(&stem).join("feedd.log"));
        }
    }
    out
}

/// The daemon's newest report, read from its log.
#[derive(Debug)]
pub struct DaemonLog {
    path: PathBuf,
    report: Option<DaemonReport>,
    last_read: Option<Instant>,
    /// The size the file had when it was last read, so a rotation is visible.
    size: u64,
    /// Whether the file was there at all, so "no daemon" is not "no report yet".
    present: bool,
}

impl DaemonLog {
    /// A tail of the log at `path`, read on the first call to [`DaemonLog::poll`].
    pub fn new(path: impl Into<PathBuf>) -> Self {
        DaemonLog {
            path: path.into(),
            report: None,
            last_read: None,
            size: 0,
            present: false,
        }
    }

    /// The path being tailed, for a frame that wants to name it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The newest report, or `None` when the log has none yet.
    pub fn report(&self) -> Option<&DaemonReport> {
        self.report.as_ref()
    }

    /// Whether the log file exists.
    pub fn present(&self) -> bool {
        self.present
    }

    /// Read the tail if the interval has passed, and return whether it changed.
    pub fn poll(&mut self) -> bool {
        if let Some(last) = self.last_read {
            if last.elapsed() < INTERVAL {
                return false;
            }
        }
        self.last_read = Some(Instant::now());
        let Ok(meta) = std::fs::metadata(&self.path) else {
            // The daemon may not have started yet, or may have exited and had its
            // log removed. Either way the last report is stale, and a view that
            // kept showing it would be showing a number nothing is producing.
            let changed = self.present || self.report.is_some();
            self.present = false;
            self.report = None;
            return changed;
        };
        self.present = true;
        let size = meta.len();
        let from = size.saturating_sub(TAIL_BYTES);
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return false;
        };
        if file.seek(SeekFrom::Start(from)).is_err() {
            return false;
        }
        let mut buf = Vec::with_capacity(TAIL_BYTES as usize);
        if file.read_to_end(&mut buf).is_err() {
            return false;
        }
        self.size = size;
        let text = String::from_utf8_lossy(&buf);
        let parsed = parse_report(&text);
        let changed = parsed != self.report;
        self.report = parsed;
        changed
    }

    /// The size last seen, which a test reads to check the seek.
    pub fn last_size(&self) -> u64 {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ob-daemon-{tag}-{}.log", std::process::id()))
    }

    /// Force the interval to have passed, without sleeping for it.
    fn expire(log: &mut DaemonLog) {
        log.last_read = Some(Instant::now() - INTERVAL - Duration::from_millis(1));
    }

    #[test]
    fn a_log_with_a_report_is_read() {
        let path = scratch("read");
        std::fs::write(
            &path,
            "t=1 op=report published=20 applied=20 dropped=0 rate=7\n\
             subscriber 0: delivered=20 dropped=0 high_water=8 state=nominal\n",
        )
        .expect("write");
        let mut log = DaemonLog::new(&path);
        assert!(log.poll());
        assert!(log.present());
        let report = log.report().expect("a report");
        assert_eq!(report.published, Some(20));
        assert_eq!(report.subscribers.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_log_is_an_absence_and_clears_an_old_report() {
        // The daemon may not be up yet. A view that kept the last report would be
        // showing numbers nothing is producing.
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        let mut log = DaemonLog::new(&path);
        assert!(!log.poll(), "nothing to report, and nothing changed");
        assert!(!log.present());
        assert!(log.report().is_none());

        std::fs::write(&path, "t=1 op=report published=5 rate=1\n").expect("write");
        expire(&mut log);
        assert!(log.poll());
        assert!(log.report().is_some());

        std::fs::remove_file(&path).expect("remove");
        expire(&mut log);
        assert!(log.poll(), "losing the log is a change");
        assert!(log.report().is_none(), "and the old report goes with it");
    }

    #[test]
    fn the_read_is_the_tail_not_the_whole_file() {
        // A log left running for a day is hundreds of megabytes; re-reading it
        // every half second is the one thing this must not do.
        let path = scratch("tail");
        let mut text = String::new();
        for i in 0..20_000 {
            text.push_str(&format!(
                "t={i} level=info component=feedd op=latency stage2=n=1 p50=1.0us\n"
            ));
        }
        text.push_str("t=99999 op=report published=42 rate=9\n");
        std::fs::write(&path, &text).expect("write");
        assert!(text.len() as u64 > TAIL_BYTES);

        let mut log = DaemonLog::new(&path);
        assert!(log.poll());
        let report = log.report().expect("a report from the tail");
        assert_eq!(report.published, Some(42));
        assert_eq!(
            log.last_size(),
            text.len() as u64,
            "the size is recorded even though only the tail was read"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_interval_is_respected_so_a_frame_loop_does_not_read_every_frame() {
        let path = scratch("interval");
        std::fs::write(&path, "t=1 op=report published=1 rate=1\n").expect("write");
        let mut log = DaemonLog::new(&path);
        assert!(log.poll(), "the first read always happens");
        assert!(!log.poll(), "and the second, moments later, does not");
        expire(&mut log);
        assert!(!log.poll(), "an unchanged log is not a change");
        std::fs::write(&path, "t=2 op=report published=2 rate=2\n").expect("rewrite");
        expire(&mut log);
        assert!(log.poll(), "a new report is");
        assert_eq!(log.report().and_then(|r| r.published), Some(2));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_log_is_looked_for_where_obctl_puts_it() {
        // The config is named relative to the repository root by hand and by an
        // absolute path by `obctl`; both have to find `var/run/<config>/feedd.log`.
        let relative = candidates(Path::new("config/dev.json"));
        assert_eq!(
            relative[0],
            PathBuf::from("var/run/dev/feedd.log"),
            "the working directory first"
        );
        assert_eq!(
            relative.len(),
            1,
            "a config named relative to the working directory has one candidate: its \
             grandparent is the working directory itself, so the second path would be \
             the first one spelled twice"
        );

        let absolute = candidates(Path::new("/repo/config/test.json"));
        assert_eq!(absolute[0], PathBuf::from("var/run/test/feedd.log"));
        assert_eq!(
            absolute[1],
            PathBuf::from("/repo/var/run/test/feedd.log"),
            "an absolute config resolves under its own root"
        );
    }

    #[test]
    fn an_absent_log_is_named_rather_than_guessed_at() {
        // A frame says where it looked; the path is the first candidate.
        let found = locate(Path::new("config/does-not-exist.json"));
        assert_eq!(found, PathBuf::from("var/run/does-not-exist/feedd.log"));
    }

    #[test]
    fn a_log_with_no_report_in_it_yields_nothing_rather_than_a_zero() {
        let path = scratch("noreport");
        std::fs::write(
            &path,
            "t=1 level=info component=feedd op=stop published=9\n",
        )
        .expect("write");
        let mut log = DaemonLog::new(&path);
        assert!(!log.poll());
        assert!(log.present(), "the file is there");
        assert!(
            log.report().is_none(),
            "and it says nothing about the queues"
        );
        let _ = std::fs::remove_file(&path);
    }
}
