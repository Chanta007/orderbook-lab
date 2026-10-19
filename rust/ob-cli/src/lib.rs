//! Shared plumbing for the five binaries.
//!
//! Argument parsing, config loading, directory creation, and the small
//! formatting helpers a report line needs. Nothing here is on a hot path.
//!
//! # Why there is no argument-parsing crate
//!
//! The whole CLI is positional arguments and a handful of `--flags`, which is
//! forty lines of code below. A derive-macro crate would add a build dependency,
//! a version to keep current, and a second place to look when a flag does not
//! work, in exchange for saving those forty lines.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

use ob::config::Config;
use std::path::Path;

pub mod actions;
pub mod daemon;
pub mod ingest;
pub mod screen;
pub mod stats;
pub mod tickers;
pub mod watch;
pub mod window;

/// The result type the binaries use: an error message and an exit code.
pub type CliResult<T = ()> = std::result::Result<T, CliError>;

/// A failure that should be printed and turned into an exit code.
#[derive(Debug)]
pub struct CliError {
    /// What to print.
    pub message: String,
    /// What to exit with.
    pub code: i32,
}

impl CliError {
    /// A usage error, exit code 2, matching the C++ binaries.
    pub fn usage(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            code: 2,
        }
    }

    /// A runtime error, exit code 1.
    pub fn failed(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            code: 1,
        }
    }
}

impl From<ob::Error> for CliError {
    fn from(e: ob::Error) -> Self {
        CliError::failed(e.to_string())
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A parsed command line.
#[derive(Debug, Default)]
pub struct Args {
    positional: Vec<String>,
    /// Every flag, in the order it was given, so a flag that may repeat can be read
    /// as a list. `--act` is one: two commands are two flags.
    pub flags: Vec<(String, String)>,
}

impl Args {
    /// Parse `std::env::args().skip(1)`.
    ///
    /// `--name value` becomes a flag with a value; `--name` alone becomes a flag
    /// with an empty value; anything else is positional. A `--name=value` form
    /// is also accepted.
    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1))
    }

    /// Parse an iterator of arguments.
    pub fn parse<I: IntoIterator<Item = String>>(items: I) -> Self {
        let mut out = Args::default();
        let mut it = items.into_iter().peekable();
        while let Some(arg) = it.next() {
            if let Some(rest) = arg.strip_prefix("--") {
                match rest.split_once('=') {
                    Some((name, value)) => out.flags.push((name.to_string(), value.to_string())),
                    None => {
                        // Look ahead: a value is taken only when the next
                        // argument is not itself a flag.
                        let value = match it.peek() {
                            Some(next) if !next.starts_with("--") => it.next().unwrap_or_default(),
                            _ => String::new(),
                        };
                        out.flags.push((rest.to_string(), value));
                    }
                }
            } else {
                out.positional.push(arg);
            }
        }
        out
    }

    /// The `n`th positional argument, if present.
    pub fn positional(&self, n: usize) -> Option<&str> {
        self.positional.get(n).map(String::as_str)
    }

    /// Every positional argument.
    pub fn positionals(&self) -> &[String] {
        &self.positional
    }

    /// A flag's value, or `None` when the flag was not given.
    ///
    /// The lookup accepts the name with or without its leading dashes, because
    /// both spellings are natural to write at a call site and a mismatch is
    /// silent: the first version of this type stored names without dashes and
    /// every caller looked them up with dashes, so `--json`, `--levels` and
    /// `--symbols` were all ignored and every binary ran on its defaults.
    pub fn value(&self, name: &str) -> Option<&str> {
        let bare = name.trim_start_matches('-');
        self.flags
            .iter()
            .rev()
            .find(|(k, _)| k == bare)
            .map(|(_, v)| v.as_str())
    }

    /// Whether a flag was given, with or without a value.
    pub fn has(&self, name: &str) -> bool {
        let bare = name.trim_start_matches('-');
        self.flags.iter().any(|(k, _)| k == bare)
    }

    /// A flag's value parsed as a number, falling back to `default`.
    ///
    /// A flag that was given but whose value will not parse is a usage error, so
    /// this returns a `Result` rather than quietly using the default.
    pub fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> CliResult<T> {
        match self.value(name) {
            None | Some("") => Ok(default),
            Some(raw) => raw
                .parse()
                .map_err(|_| CliError::usage(format!("--{name} expects a number, got {raw:?}"))),
        }
    }
}

/// Load a config, reporting a usage error when the path is missing.
pub fn load_config(path: &str) -> CliResult<Config> {
    if path.is_empty() {
        return Err(CliError::usage("a config path is required"));
    }
    Config::load(Path::new(path)).map_err(|e| CliError::failed(e.to_string()))
}

/// Create the directory that holds a file, if it does not exist.
///
/// Called for every path a config names, so a first run does not fail on a
/// missing `var/dev`. The C++ `obctl setup` does the same thing from Python.
pub fn ensure_parent(path: &str) -> CliResult<()> {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::failed(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
    }
    Ok(())
}

/// Format a duration in nanoseconds for a report, in milliseconds with three
/// decimals.
pub fn ms(ns: u64) -> String {
    format!("{:.3}", ns as f64 / 1_000_000.0)
}

/// The file the adapter writes to say it is alive.
///
/// The name is shared with `python/feed_adapter.py`'s `HEARTBEAT_NAME`, and the
/// two have to agree: if they do not, a reader finds no file, reports `Absent`,
/// and the window loses the distinction this whole mechanism exists for. It is
/// the same shape of defect as the watch file's path, which was derived twice and
/// disagreed once (`docs/optimization/ledger.jsonl` OPT-034). The name lives here
/// so there is one place per side rather than one literal per call site, and
/// `python/test_obctl.py` asserts the two strings match.
pub const HEARTBEAT_NAME: &str = "adapter.heartbeat";

/// What a heartbeat file says about a process that should be running.
///
/// Two causes of a stalled book look identical from the ring: an exchange that
/// has gone quiet, and no adapter at all. The second is what a stopped session
/// leaves behind, and it is the one a window cannot recover from on its own, so it
/// has to be told apart from the first and said out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// The file is there and recent, with the note the process wrote.
    Live {
        /// The process id it reported.
        pid: u32,
        /// Seconds since it last wrote.
        age_s: u64,
        /// What it said it was doing.
        note: String,
    },
    /// The file is there and stale: the process wrote, then stopped.
    Stale {
        /// The process id it reported.
        pid: u32,
        /// Seconds since it last wrote.
        age_s: u64,
        /// The last note it left.
        note: String,
    },
    /// No file at all, so nothing has written one this run.
    Absent,
}

/// How stale a heartbeat may be before it stops counting as live.
///
/// The adapter writes one about five times a second while frames arrive, so a
/// couple of seconds is far longer than its own interval and far shorter than a
/// person's patience.
pub const HEARTBEAT_STALE_S: u64 = 3;

/// Read a heartbeat file and classify it.
pub fn read_heartbeat(path: &std::path::Path, now_unix_s: u64) -> Liveness {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Liveness::Absent;
    };
    // `pid` unix_seconds note...; anything else is a file this program does not
    // understand, and saying nothing is better than saying something wrong.
    let mut parts = text.splitn(3, ' ');
    let pid = parts.next().and_then(|p| p.trim().parse::<u32>().ok());
    let stamped = parts.next().and_then(|p| p.trim().parse::<u64>().ok());
    let note = parts.next().unwrap_or("").trim().to_string();
    let (Some(pid), Some(stamped)) = (pid, stamped) else {
        return Liveness::Absent;
    };
    let age_s = now_unix_s.saturating_sub(stamped);
    if age_s <= HEARTBEAT_STALE_S {
        Liveness::Live { pid, age_s, note }
    } else {
        Liveness::Stale { pid, age_s, note }
    }
}

/// Format a duration for a report, choosing the unit by magnitude.
///
/// Moved to [`ob::clock`] when a second frontend needed it: the terminal, the web
/// page and the headless report show the same durations, and two formatters for one
/// unit is two answers to "is 400 microseconds slow". Re-exported so the call sites
/// in the binaries did not have to move with it.
pub use ob::clock::duration_ns;

/// Format a count of records per second.
pub fn rate(count: u64, elapsed_ns: u64) -> String {
    if elapsed_ns == 0 {
        return "0".to_string();
    }
    let per_sec = count as f64 * 1e9 / elapsed_ns as f64;
    if per_sec >= 1e6 {
        format!("{:.2}M", per_sec / 1e6)
    } else if per_sec >= 1e3 {
        format!("{:.2}k", per_sec / 1e3)
    } else {
        format!("{per_sec:.0}")
    }
}

/// Print a line to stderr, flushing.
///
/// The ready lines `obctl` waits for go to stderr, because stdout belongs to the
/// terminal UI and to the headless checker's report.
pub fn note(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{text}");
}

/// Print a line to stdout, flushing.
pub fn say(text: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        Args::parse(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn flags_and_positionals_are_separated() {
        let a = args(&["config/dev.json", "--levels", "5", "--tui"]);
        assert_eq!(a.positional(0), Some("config/dev.json"));
        assert_eq!(a.value("levels"), Some("5"));
        assert!(a.has("tui"));
        assert_eq!(a.value("tui"), Some(""));
        assert!(!a.has("nope"));
    }

    #[test]
    fn a_flag_can_be_looked_up_with_or_without_its_dashes() {
        // The regression: names are stored without dashes, so a lookup written
        // with them silently found nothing and every binary ran on its defaults.
        let a = args(&["--json", "--levels", "7"]);
        assert!(a.has("json"));
        assert!(a.has("--json"));
        assert_eq!(a.value("levels"), Some("7"));
        assert_eq!(a.value("--levels"), Some("7"));
        assert_eq!(a.number::<usize>("--levels", 1).unwrap(), 7);
        assert!(!a.has("--missing"));
        assert_eq!(a.value("--missing"), None);
    }

    #[test]
    fn an_equals_form_is_accepted() {
        let a = args(&["--config=config/prod.json"]);
        assert_eq!(a.value("config"), Some("config/prod.json"));
    }

    #[test]
    fn a_flag_followed_by_another_flag_takes_no_value() {
        let a = args(&["--tui", "--levels", "3"]);
        assert_eq!(a.value("tui"), Some(""));
        assert_eq!(a.value("levels"), Some("3"));
    }

    #[test]
    fn the_last_repeated_flag_wins() {
        let a = args(&["--levels", "3", "--levels", "9"]);
        assert_eq!(a.value("levels"), Some("9"));
    }

    #[test]
    fn a_number_flag_parses_and_reports_a_bad_value() {
        let a = args(&["--symbols", "1000"]);
        assert_eq!(a.number::<usize>("symbols", 10).unwrap(), 1000);
        assert_eq!(a.number::<usize>("missing", 10).unwrap(), 10);
        let bad = args(&["--symbols", "many"]);
        let err = bad.number::<usize>("symbols", 10).expect_err("must fail");
        assert_eq!(err.code, 2);
        assert!(err.message.contains("expects a number"), "{}", err.message);
    }

    #[test]
    fn a_heartbeat_reads_back_with_its_note_and_age() {
        let dir = std::env::temp_dir().join(format!("ob-beat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("adapter.heartbeat");

        // Nothing written yet.
        assert_eq!(read_heartbeat(&path, 1000), Liveness::Absent);

        // Two seconds old is inside the window.
        std::fs::write(&path, "4242 998 live; watching 3\n").expect("write");
        match read_heartbeat(&path, 1000) {
            Liveness::Live { pid, age_s, note } => {
                assert_eq!(pid, 4242);
                assert_eq!(age_s, 2);
                assert_eq!(note, "live; watching 3");
            }
            other => panic!("{other:?}"),
        }

        // Five seconds old is outside it: the process wrote and then stopped.
        std::fs::write(&path, "4242 995 live; watching 3\n").expect("write");
        match read_heartbeat(&path, 1000) {
            Liveness::Stale { pid, age_s, .. } => {
                assert_eq!(pid, 4242);
                assert_eq!(age_s, 5);
            }
            other => panic!("{other:?}"),
        }
        // Exactly at the threshold still counts as live, so the boundary is a
        // decision rather than an accident.
        std::fs::write(&path, format!("4242 {} live\n", 1000 - HEARTBEAT_STALE_S)).expect("write");
        assert!(matches!(read_heartbeat(&path, 1000), Liveness::Live { .. }));

        // A file this program does not understand says nothing rather than
        // something wrong.
        std::fs::write(&path, "not a heartbeat").expect("write");
        assert_eq!(read_heartbeat(&path, 1000), Liveness::Absent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_heartbeat_from_the_future_is_treated_as_live_not_as_negative() {
        // A clock that stepped backwards must not produce a huge age.
        let dir = std::env::temp_dir().join(format!("ob-beat2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("adapter.heartbeat");
        std::fs::write(&path, "1 2000 live\n").expect("write");
        match read_heartbeat(&path, 1000) {
            Liveness::Live { age_s, .. } => assert_eq!(age_s, 0, "saturated rather than wrapping"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_duration_formatter_is_the_library_s_one() {
        // One implementation, in `ob::clock`, where the tests for its units live.
        assert_eq!(duration_ns(1_500_000), ob::clock::duration_ns(1_500_000));
    }

    #[test]
    fn formatting_helpers_read_the_way_a_report_needs() {
        assert_eq!(ms(1_500_000), "1.500");
        assert_eq!(rate(1_000, 1_000_000_000), "1.00k");
        assert_eq!(rate(2_000_000, 1_000_000_000), "2.00M");
        assert_eq!(rate(999, 1_000_000_000), "999");
        assert_eq!(rate(10, 1_000_000_000), "10");
        assert_eq!(rate(10, 0), "0");
    }

    #[test]
    fn ensure_parent_creates_the_directory() {
        let dir = std::env::temp_dir().join(format!("ob-cli-test-{}", std::process::id()));
        let file = dir.join("nested").join("file.bin");
        ensure_parent(&file.to_string_lossy()).expect("create");
        assert!(file.parent().unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
