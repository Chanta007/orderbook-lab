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

pub mod ingest;

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
    flags: Vec<(String, String)>,
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
