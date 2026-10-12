//! Structured log lines, formatted on the calling thread into a stack buffer.
//!
//! # What this is, and what it is not
//!
//! It is a formatter, not a logger: it turns a level, a component, an operation
//! and a handful of typed fields into one line and writes it. There is no queue,
//! no background thread, and no allocation for the common case.
//!
//! The C++ implementation's `AsyncLog` does the opposite: a queue and a side
//! thread, because its publish path formats a 32-character hex trace id and a
//! `std::ofstream` write would otherwise stall the feed. This implementation has
//! no such call on the hot path. The feed daemon's publish loop writes nothing;
//! the periodic report and the startup lines are the only users of this module,
//! and they run once a second at most. Adding a queue to move work that does not
//! happen would be complexity without a measurement behind it.
//!
//! # The line format
//!
//! ```text
//!   t=1234567890 level=info component=feedd op=listen addr=127.0.0.1:9001
//! ```
//!
//! `t` is nanoseconds from [`crate::clock`], which is monotonic and not a
//! calendar time. That is deliberate: a hot-path stamp is cheap and a calendar
//! conversion is not, and a run's lines are read together, where differences
//! matter and dates do not.
//!
//! Fields are typed, so a value is formatted by the type that produced it rather
//! than by a `Display` impl that might allocate. The whole line is built in a
//! fixed 512-byte buffer; a line that would overflow is truncated with a marker
//! rather than allocating, because a log call must never be the thing that fails
//! under pressure.

use crate::clock;
use std::io::Write;

/// How severe a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Detail for a developer, off unless asked for.
    Debug,
    /// Something happened that an operator wants in the record.
    Info,
    /// Something is wrong but the process is still working.
    Warn,
    /// Something failed.
    Error,
}

impl Level {
    /// The label written into the line.
    pub fn label(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// A typed field value.
///
/// Typed rather than `Display` so that the formatter decides the representation
/// and nothing here can allocate or panic.
#[derive(Debug, Clone, Copy)]
pub enum Field<'a> {
    /// A borrowed string.
    Str(&'a str),
    /// An unsigned integer.
    U64(u64),
    /// A signed integer.
    I64(i64),
    /// A boolean, written as `true` or `false`.
    Bool(bool),
    /// A duration in nanoseconds, written in microseconds with three decimals.
    Micros(u64),
    /// A count of bytes, written as a plain number.
    Bytes(u64),
}

/// The size of the stack buffer one line is built in.
pub const LINE_BUFFER: usize = 512;

/// One line under construction.
///
/// Borrows a fixed buffer so that a caller can build a line without allocating,
/// then either write it or inspect it in a test.
#[derive(Debug)]
pub struct Line<'a> {
    buf: &'a mut [u8],
    len: usize,
    truncated: bool,
}

impl<'a> Line<'a> {
    /// Start a line in `buf`.
    pub fn new(buf: &'a mut [u8], level: Level, component: &str, op: &str) -> Self {
        let mut line = Line {
            buf,
            len: 0,
            truncated: false,
        };
        line.push_str("t=");
        line.push_u64(clock::now_ns());
        line.push_str(" level=");
        line.push_str(level.label());
        line.push_str(" component=");
        line.push_str(component);
        line.push_str(" op=");
        line.push_str(op);
        line
    }

    /// Add one field.
    pub fn field(mut self, name: &str, value: Field<'_>) -> Self {
        self.push_str(" ");
        self.push_str(name);
        self.push_str("=");
        match value {
            Field::Str(s) => self.push_str(s),
            Field::U64(v) => self.push_u64(v),
            Field::I64(v) => {
                if v < 0 {
                    self.push_str("-");
                    self.push_u64(v.unsigned_abs());
                } else {
                    self.push_u64(v as u64);
                }
            }
            Field::Bool(v) => self.push_str(if v { "true" } else { "false" }),
            Field::Micros(ns) => {
                self.push_u64(ns / 1000);
                self.push_str(".");
                let frac = (ns % 1000) / 100;
                self.push_u64(frac);
            }
            Field::Bytes(v) => {
                self.push_u64(v);
            }
        }
        self
    }

    /// Finish, appending a newline.
    pub fn finish(mut self) -> &'a str {
        if self.truncated {
            // The marker replaces the last four bytes so that a reader can tell
            // a complete line from a clipped one.
            let at = self.len.saturating_sub(4);
            for (i, b) in b"...!".iter().enumerate() {
                if at + i < self.buf.len() {
                    self.buf[at + i] = *b;
                }
            }
        }
        if self.len < self.buf.len() {
            self.buf[self.len] = b'\n';
            self.len += 1;
        }
        // The buffer is filled with ASCII only, so this cannot fail; a caller
        // that wrote non-ASCII would get a replacement character rather than a
        // panic, which is the right trade for a log line.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    fn push_str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            if self.len >= self.buf.len() {
                self.truncated = true;
                return;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
    }

    fn push_u64(&mut self, mut v: u64) {
        let mut digits = [0u8; 20];
        let mut n = 0;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        for i in (0..n).rev() {
            if self.len >= self.buf.len() {
                self.truncated = true;
                return;
            }
            self.buf[self.len] = digits[i];
            self.len += 1;
        }
    }
}

/// Where lines go, and which of them are written.
#[derive(Debug)]
pub struct Logger {
    level: Level,
    to_stderr: bool,
    written: u64,
    suppressed: u64,
}

impl Default for Logger {
    fn default() -> Self {
        Logger {
            level: Level::Info,
            to_stderr: true,
            written: 0,
            suppressed: 0,
        }
    }
}

impl Logger {
    /// A logger that writes `level` and above to stderr.
    pub fn new(level: Level) -> Self {
        Logger {
            level,
            ..Logger::default()
        }
    }

    /// A logger that counts lines without writing them. For tests.
    pub fn silent(level: Level) -> Self {
        Logger {
            level,
            to_stderr: false,
            written: 0,
            suppressed: 0,
        }
    }

    /// The level this logger writes.
    pub fn level(&self) -> Level {
        self.level
    }

    /// How many lines were written.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// How many were below the level and dropped.
    pub fn suppressed(&self) -> u64 {
        self.suppressed
    }

    /// Format and write one line.
    pub fn log(&mut self, level: Level, component: &str, op: &str, fields: &[(&str, Field<'_>)]) {
        if level < self.level {
            self.suppressed += 1;
            return;
        }
        let mut buf = [0u8; LINE_BUFFER];
        let mut line = Line::new(&mut buf, level, component, op);
        for (name, value) in fields {
            line = line.field(name, *value);
        }
        let text = line.finish();
        self.written += 1;
        if self.to_stderr {
            // A failed write to stderr is not something a log call can report,
            // and a log call that panicked would be worse than a lost line.
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
    }

    /// Format one line without writing it. Used by the tests and by a caller
    /// that wants the line as a string.
    pub fn format(level: Level, component: &str, op: &str, fields: &[(&str, Field<'_>)]) -> String {
        let mut buf = [0u8; LINE_BUFFER];
        let mut line = Line::new(&mut buf, level, component, op);
        for (name, value) in fields {
            line = line.field(name, *value);
        }
        line.finish().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_carries_the_level_component_op_and_fields() {
        let text = Logger::format(
            Level::Info,
            "feedd",
            "listen",
            &[
                ("addr", Field::Str("127.0.0.1:9001")),
                ("env", Field::Str("dev")),
            ],
        );
        assert!(text.starts_with("t="), "{text}");
        assert!(text.contains(" level=info "), "{text}");
        assert!(text.contains(" component=feedd "), "{text}");
        assert!(text.contains(" op=listen "), "{text}");
        assert!(text.contains(" addr=127.0.0.1:9001"), "{text}");
        assert!(text.contains(" env=dev"), "{text}");
        assert!(text.ends_with('\n'), "{text}");
    }

    #[test]
    fn every_field_type_has_a_representation() {
        let text = Logger::format(
            Level::Warn,
            "c",
            "o",
            &[
                ("u", Field::U64(42)),
                ("i", Field::I64(-7)),
                ("b", Field::Bool(true)),
                ("us", Field::Micros(1_234_567)),
                ("bytes", Field::Bytes(294_928)),
            ],
        );
        assert!(text.contains(" u=42"), "{text}");
        assert!(text.contains(" i=-7"), "{text}");
        assert!(text.contains(" b=true"), "{text}");
        assert!(text.contains(" us=1234.5"), "{text}");
        assert!(text.contains(" bytes=294928"), "{text}");
    }

    #[test]
    fn a_negative_integer_is_written_with_its_sign() {
        assert!(Logger::format(Level::Info, "c", "o", &[("d", Field::I64(-1))]).contains(" d=-1"));
        assert!(Logger::format(Level::Info, "c", "o", &[("d", Field::I64(0))]).contains(" d=0"));
        assert!(
            Logger::format(Level::Info, "c", "o", &[("d", Field::I64(i64::MIN))])
                .contains(" d=-9223372036854775808")
        );
    }

    #[test]
    fn a_level_below_the_threshold_is_counted_and_not_written() {
        let mut log = Logger::silent(Level::Warn);
        log.log(Level::Info, "c", "o", &[]);
        log.log(Level::Debug, "c", "o", &[]);
        assert_eq!(log.written(), 0);
        assert_eq!(log.suppressed(), 2);
        log.log(Level::Error, "c", "o", &[]);
        assert_eq!(log.written(), 1);
    }

    #[test]
    fn a_line_too_long_for_the_buffer_is_marked_rather_than_allocated() {
        let long = "x".repeat(LINE_BUFFER * 2);
        let text = Logger::format(Level::Info, "c", "o", &[("v", Field::Str(&long))]);
        assert!(text.len() <= LINE_BUFFER, "len={}", text.len());
        assert!(
            text.contains("...!"),
            "the truncation marker is missing: {text}"
        );
    }

    #[test]
    fn the_timestamp_is_the_monotonic_clock() {
        let before = clock::now_ns();
        let text = Logger::format(Level::Info, "c", "o", &[]);
        let after = clock::now_ns();
        let value: u64 = text
            .trim_start_matches("t=")
            .split(' ')
            .next()
            .expect("timestamp")
            .parse()
            .expect("a number");
        assert!(
            (before..=after).contains(&value),
            "{value} not in {before}..={after}"
        );
    }

    #[test]
    fn two_lines_in_a_row_do_not_share_state() {
        let a = Logger::format(Level::Info, "one", "a", &[("x", Field::U64(1))]);
        let b = Logger::format(Level::Info, "two", "b", &[("y", Field::U64(2))]);
        assert!(a.contains("component=one"));
        assert!(!a.contains("component=two"));
        assert!(b.contains("component=two"));
        assert!(b.contains(" y=2"));
        assert!(!b.contains(" x=1"));
    }
}
