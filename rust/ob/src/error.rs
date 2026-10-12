//! One error type for the whole crate.
//!
//! A lab is easier to read when a failure can be matched on and printed without
//! a dependency, so this is a small enum rather than a boxed trait object. The
//! variant names say *which layer* refused, `context` says which call refused,
//! and the source error is kept rather than flattened into a string, so a caller
//! can still ask the operating system what happened.
//!
//! Errors here are returned, not logged. The layer that knows what the user was
//! trying to achieve is the layer that decides what to say about it.

use std::fmt;

/// Everything this crate can refuse to do.
#[derive(Debug)]
pub enum Error {
    /// An operating-system call failed. `context` names the call in the terms
    /// the caller used, for example `"open ring"` or `"mmap ring"`.
    Io {
        /// What was being attempted, in the caller's words.
        context: &'static str,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// A config file was missing, unreadable, or did not satisfy the safety
    /// policy in [`crate::config`].
    Config(String),
    /// The shared-memory bus refused: bad magic, wrong capacity, or a mapping
    /// that does not match the file.
    Bus(String),
    /// A byte span was not a whole, valid record.
    Wire(String),
    /// A fixed-capacity structure was full — a subscriber queue, the symbol
    /// table, or the pending-markout ring. The message names the structure and
    /// the capacity, because the fix is always to size it.
    Full {
        /// Which structure ran out of room.
        what: &'static str,
        /// Its configured capacity.
        capacity: usize,
    },
}

/// The crate's result alias.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Build an [`Error::Io`] from a call site.
    pub fn io(context: &'static str, source: std::io::Error) -> Self {
        Error::Io { context, source }
    }

    /// Build an [`Error::Full`] from a call site.
    pub fn full(what: &'static str, capacity: usize) -> Self {
        Error::Full { what, capacity }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::Config(m) => write!(f, "config: {m}"),
            Error::Bus(m) => write!(f, "bus: {m}"),
            Error::Wire(m) => write!(f, "wire: {m}"),
            Error::Full { what, capacity } => {
                write!(f, "{what} is full (capacity {capacity})")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    /// A bare `?` in this crate reports the operation as `"io"`. When the
    /// failure is worth naming — a ring that would not map, a WAL that would
    /// not open — the call site uses [`Error::io`] instead and says which call.
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            context: "io",
            source,
        }
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Error::Config(e.to_string())
    }
}

impl From<std::num::ParseFloatError> for Error {
    fn from(e: std::num::ParseFloatError) -> Self {
        Error::Config(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_layer() {
        let e = Error::Config("missing listen_port".to_string());
        assert_eq!(e.to_string(), "config: missing listen_port");

        let e = Error::full("symbol table", 4096);
        assert_eq!(e.to_string(), "symbol table is full (capacity 4096)");
    }

    #[test]
    fn io_error_keeps_its_source() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let e = Error::io("open ring", source);
        assert!(std::error::Error::source(&e).is_some());
        assert!(e.to_string().starts_with("open ring: "));
    }
}
