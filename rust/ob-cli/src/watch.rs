//! The set of symbols being watched, and the file that tells the adapter about it.
//!
//! # The two roles, kept apart
//!
//! There are two questions and they are not the same one:
//!
//! - **What is subscribed to?** The watch set. The adapter reads it and holds one
//!   websocket for exactly these symbols.
//! - **What is on screen?** The displayed symbol, which is one of the watch set.
//!
//! Selecting a symbol does both: it becomes the displayed one, and if it was not
//! being watched it is added. That is the behaviour that keeps the subscription to
//! what is actually being looked at, which was the point of asking.
//!
//! # The file
//!
//! `var/<env>/watch.txt`, one symbol per line. The adapter polls it and reconnects
//! when it changes, so this module's job is to write it atomically: a reader that
//! catches a half-written file would reconnect to a nonsense set. Write to a
//! temporary file beside it and rename, which on every filesystem this lab runs on
//! is atomic within a directory.
//!
//! The format is deliberately the same as `config/tickers.txt`, so a user can see
//! what is being watched by reading one file and change it with an editor.

use crate::CliError;
use ob::msg::symbol_fits;
use std::io::Write;
use std::path::Path;

/// The symbols being watched, and which of them is displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    symbols: Vec<String>,
    display: usize,
}

impl Watch {
    /// A watch set holding one symbol.
    pub fn single(symbol: &str) -> Self {
        Watch {
            symbols: vec![symbol.to_ascii_uppercase()],
            display: 0,
        }
    }

    /// Read a watch set, falling back to `default` when the file is absent or has
    /// no usable line.
    ///
    /// A symbol that does not fit the wire field is skipped with a reason rather
    /// than truncating, for the same reason the catalog refuses one: a truncated
    /// symbol is a different symbol.
    pub fn load(path: &Path, default: &str) -> Result<Self, CliError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Watch::single(default))
            }
            Err(e) => {
                return Err(CliError::failed(format!(
                    "cannot read {}: {e}",
                    path.display()
                )))
            }
        };
        let mut symbols = Vec::new();
        for line in text.lines() {
            let symbol = line.trim();
            if symbol.is_empty() || symbol.starts_with('#') {
                continue;
            }
            let symbol = symbol.to_ascii_uppercase();
            if !symbol_fits(&symbol) {
                continue;
            }
            if !symbols.contains(&symbol) {
                symbols.push(symbol);
            }
        }
        if symbols.is_empty() {
            return Ok(Watch::single(default));
        }
        Ok(Watch {
            symbols,
            display: 0,
        })
    }

    /// Write the set, atomically.
    pub fn save(&self, path: &Path) -> Result<(), CliError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CliError::failed(format!("cannot create {}: {e}", parent.display()))
                })?;
            }
        }
        // Beside the target, so the rename stays within one directory and cannot
        // cross a filesystem boundary.
        let temp = path.with_extension("tmp");
        {
            let mut file = std::fs::File::create(&temp)
                .map_err(|e| CliError::failed(format!("cannot write {}: {e}", temp.display())))?;
            writeln!(
                file,
                "# Written by the terminal. One symbol per line; the adapter \
                 subscribes to these."
            )
            .and_then(|_| {
                for symbol in &self.symbols {
                    writeln!(file, "{symbol}")?;
                }
                Ok(())
            })
            .map_err(|e| CliError::failed(format!("cannot write {}: {e}", temp.display())))?;
        }
        std::fs::rename(&temp, path)
            .map_err(|e| CliError::failed(format!("cannot replace {}: {e}", path.display())))
    }

    /// The symbols being watched, in order.
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    /// How many are watched.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether nothing is watched. Never true through the public constructors: a
    /// watch set always holds at least the displayed symbol.
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// The symbol on screen.
    pub fn current(&self) -> &str {
        self.symbols
            .get(self.display)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// Which slot is displayed.
    pub fn display_index(&self) -> usize {
        self.display
    }

    /// Whether this symbol is being watched.
    pub fn contains(&self, symbol: &str) -> bool {
        let upper = symbol.to_ascii_uppercase();
        self.symbols.contains(&upper)
    }

    /// Add a symbol without changing what is displayed.
    ///
    /// Returns whether it was new. A symbol that does not fit the field is refused
    /// rather than truncated.
    pub fn add(&mut self, symbol: &str) -> Result<bool, CliError> {
        let upper = symbol.trim().to_ascii_uppercase();
        if upper.is_empty() {
            return Err(CliError::failed("a symbol is required"));
        }
        if !symbol_fits(&upper) {
            return Err(CliError::failed(format!(
                "{upper} is {} bytes and the wire field holds at most 8",
                upper.len()
            )));
        }
        if self.contains(&upper) {
            return Ok(false);
        }
        self.symbols.push(upper);
        Ok(true)
    }

    /// Remove a symbol.
    ///
    /// Refuses to remove the last one: a watch set with nothing in it would leave
    /// the adapter with no stream and the terminal with no book, which reads as a
    /// broken program rather than as a choice.
    pub fn remove(&mut self, symbol: &str) -> Result<bool, CliError> {
        let upper = symbol.trim().to_ascii_uppercase();
        let Some(at) = self.symbols.iter().position(|s| *s == upper) else {
            return Ok(false);
        };
        if self.symbols.len() == 1 {
            return Err(CliError::failed(
                "that is the only symbol being watched; select another first",
            ));
        }
        self.symbols.remove(at);
        // Keep the display on the same symbol where possible, and in range
        // otherwise.
        if self.display >= self.symbols.len() {
            self.display = self.symbols.len() - 1;
        }
        Ok(true)
    }

    /// Select a symbol: display it, and watch it if it was not being watched.
    ///
    /// This is the behaviour that keeps the subscription to what is being looked
    /// at. Returns whether the watch set grew.
    pub fn select(&mut self, symbol: &str) -> Result<bool, CliError> {
        let upper = symbol.trim().to_ascii_uppercase();
        let added = self.add(&upper)?;
        let at = self
            .symbols
            .iter()
            .position(|s| *s == upper)
            .expect("add put it there or it was already");
        self.display = at;
        Ok(added)
    }

    /// Select by position, for a picker that has already resolved the name.
    pub fn select_index(&mut self, index: usize) -> bool {
        if index < self.symbols.len() {
            self.display = index;
            true
        } else {
            false
        }
    }

    /// Replace the whole set with one symbol.
    pub fn only(&mut self, symbol: &str) -> Result<(), CliError> {
        let upper = symbol.trim().to_ascii_uppercase();
        if !symbol_fits(&upper) {
            return Err(CliError::failed(format!(
                "{upper} is {} bytes and the wire field holds at most 8",
                upper.len()
            )));
        }
        self.symbols = vec![upper];
        self.display = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("ob-watch-{tag}-{}", std::process::id()))
            .join("watch.txt")
    }

    #[test]
    fn a_missing_file_falls_back_to_the_configured_symbol() {
        let w = Watch::load(&scratch("missing"), "BTCUSDT").expect("load");
        assert_eq!(w.symbols(), &["BTCUSDT"]);
        assert_eq!(w.current(), "BTCUSDT");
    }

    #[test]
    fn add_then_select_watches_and_displays() {
        let mut w = Watch::single("BTCUSDT");
        assert!(w.select("ethusdt").expect("select"), "it was new");
        assert_eq!(w.current(), "ETHUSDT", "selecting displays it");
        assert_eq!(w.symbols(), &["BTCUSDT", "ETHUSDT"]);
        // Selecting again is not new, and does not duplicate.
        assert!(
            !w.select("ETHUSDT").expect("select"),
            "not new the second time"
        );
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn add_does_not_move_the_display_but_select_does() {
        let mut w = Watch::single("BTCUSDT");
        w.add("ETHUSDT").expect("add");
        assert_eq!(w.current(), "BTCUSDT");
        w.select("ETHUSDT").expect("select");
        assert_eq!(w.current(), "ETHUSDT");
    }

    #[test]
    fn removing_keeps_the_display_in_range() {
        let mut w = Watch::single("BTCUSDT");
        w.select("ETHUSDT").expect("select");
        w.select("SOLUSDT").expect("select");
        assert_eq!(w.display_index(), 2);
        w.remove("SOLUSDT").expect("remove");
        assert_eq!(w.len(), 2);
        assert_eq!(w.display_index(), 1, "the display moves back into range");
        assert_eq!(w.current(), "ETHUSDT");
    }

    #[test]
    fn the_last_symbol_cannot_be_removed() {
        let mut w = Watch::single("BTCUSDT");
        let err = w.remove("BTCUSDT").expect_err("must refuse");
        assert!(err.message.contains("only symbol"), "{}", err.message);
        assert_eq!(w.len(), 1);
        // Removing something that is not there is not an error, just false.
        assert!(
            !w.remove("ETHUSDT").expect("no-op"),
            "removing nothing is a no-op"
        );
    }

    #[test]
    fn a_symbol_too_long_for_the_field_is_refused_rather_than_cut() {
        let mut w = Watch::single("BTCUSDT");
        let err = w.add("1000SHIBUSDT").expect_err("must refuse");
        assert!(err.message.contains("at most 8"), "{}", err.message);
        assert_eq!(w.len(), 1, "nothing was added");
        let err = w.only("1000SATSUSDT").expect_err("must refuse");
        assert!(err.message.contains("at most 8"), "{}", err.message);
    }

    #[test]
    fn only_replaces_the_whole_set() {
        let mut w = Watch::single("BTCUSDT");
        w.select("ETHUSDT").expect("select");
        w.select("SOLUSDT").expect("select");
        w.only("pepeusdt").expect("only");
        assert_eq!(w.symbols(), &["PEPEUSDT"]);
        assert_eq!(w.current(), "PEPEUSDT");
        assert_eq!(w.display_index(), 0);
    }

    #[test]
    fn a_saved_set_reads_back_in_order_with_the_first_displayed() {
        let path = scratch("roundtrip");
        let mut w = Watch::single("BTCUSDT");
        w.select("PEPEUSDT").expect("select");
        w.select("ETHUSDT").expect("select");
        w.save(&path).expect("save");

        let back = Watch::load(&path, "XRPUSDT").expect("load");
        assert_eq!(back.symbols(), &["BTCUSDT", "PEPEUSDT", "ETHUSDT"]);
        // The file carries the set, not which one was on screen: the display is a
        // view decision and the next session starts at the top of the list.
        assert_eq!(back.current(), "BTCUSDT");

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn saving_is_atomic_so_a_reader_never_sees_half_a_file() {
        let path = scratch("atomic");
        let mut w = Watch::single("BTCUSDT");
        w.save(&path).expect("save first");
        // There is no temporary left behind, and the file holds what was saved.
        assert!(!path.with_extension("tmp").exists());
        let first = std::fs::read_to_string(&path).expect("read");
        assert!(first.contains("BTCUSDT"));

        // A second save replaces it wholesale; nothing of the first survives.
        w.only("ETHUSDT").expect("only");
        w.save(&path).expect("save second");
        let second = std::fs::read_to_string(&path).expect("read");
        assert!(second.contains("ETHUSDT"));
        assert!(!second.contains("BTCUSDT"), "{second}");
        assert!(!path.with_extension("tmp").exists());

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn a_file_with_nothing_usable_falls_back_to_the_default() {
        let path = scratch("empty");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "# only a comment\n\n1000SHIBUSDT\n").expect("write");
        let w = Watch::load(&path, "BTCUSDT").expect("load");
        assert_eq!(
            w.symbols(),
            &["BTCUSDT"],
            "the long symbol was skipped and the default used"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn the_file_is_readable_by_a_person() {
        // The format is the one the documented workflow tells a user to edit, so
        // comments and blank lines and mixed case all have to survive.
        let path = scratch("human");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "# what I watch\nbtcusdt\n\n  ETHUSDT  \nPEPEUSDT\n").expect("write");
        let w = Watch::load(&path, "X").expect("load");
        assert_eq!(w.symbols(), &["BTCUSDT", "ETHUSDT", "PEPEUSDT"]);
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }
}
