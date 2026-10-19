//! The ticker catalog: what the picker offers, and how it ranks what you type.
//!
//! # Why a file
//!
//! `config/tickers.txt` holds one symbol per line. Names change — an exchange
//! renames a pair, a project rebrands, a pair is delisted — so the list belongs
//! in a file that can be edited rather than in a constant that has to be
//! recompiled.
//!
//! # The eight-byte limit, and why this refuses rather than truncates
//!
//! The wire record carries a symbol in a fixed eight-byte field, so a longer name
//! cannot be carried at all. Several liquid pairs have twelve-character names
//! because they carry a multiplier prefix — `1000SHIBUSDT`, `1000SATSUSDT` — and
//! they cannot appear here.
//!
//! A loader that truncated them would be worse than one that refused them:
//! `1000SHIBUSDT` and `1000SATSUSDT` both truncate to `1000SHIB`, so a reader
//! would show one pair's prices under the other's name. [`Catalog::load`] returns
//! an error naming the line and the length, so the mistake is a message rather
//! than a wrong price.
//!
//! # Ranking
//!
//! [`Catalog::suggest`] puts prefix matches before interior matches, because
//! typing `ET` should offer `ETCUSDT` before `BETACOIN`, and the file's own order
//! breaks ties so the pairs most people watch come first.

use crate::CliError;
use ob::msg::symbol_fits;
use std::path::Path;

/// The symbols the picker offers, in file order.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    symbols: Vec<String>,
}

impl Catalog {
    /// Read a catalog from a file, one symbol per line.
    ///
    /// Blank lines and lines starting with `#` are skipped. A symbol that does not
    /// fit the wire field is an error, not a truncation.
    pub fn load(path: &Path) -> Result<Self, CliError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // No file is not a failure: the picker simply has nothing to
                // offer, and typing a symbol by hand still works.
                return Ok(Catalog::default());
            }
            Err(e) => {
                return Err(CliError::failed(format!(
                    "cannot read {}: {e}",
                    path.display()
                )))
            }
        };
        let mut symbols = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let symbol = line.trim();
            if symbol.is_empty() || symbol.starts_with('#') {
                continue;
            }
            let symbol = symbol.to_ascii_uppercase();
            if !symbol_fits(&symbol) {
                return Err(CliError::failed(format!(
                    "{} line {}: {symbol:?} is {} bytes and the wire field holds \
                     at most 8. A truncated symbol is a different symbol, so this \
                     is refused rather than shortened. See the note at the top of \
                     the file.",
                    path.display(),
                    number + 1,
                    symbol.len()
                )));
            }
            if !symbols.contains(&symbol) {
                symbols.push(symbol);
            }
        }
        Ok(Catalog { symbols })
    }

    /// How many symbols the catalog holds.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Every symbol, in file order.
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    /// Whether the catalog lists this symbol, case-insensitively.
    pub fn contains(&self, symbol: &str) -> bool {
        let upper = symbol.to_ascii_uppercase();
        self.symbols.contains(&upper)
    }

    /// The symbols to offer for what has been typed, best first, at most `limit`.
    ///
    /// An empty query offers the head of the file, which is what makes the picker
    /// useful before a letter is typed. A non-empty query matches on prefix first
    /// and substring second, so `ET` offers `ETCUSDT` before `BETACOIN`, and ties
    /// keep the file's order.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<&str> {
        let query = query.trim().to_ascii_uppercase();
        if query.is_empty() {
            return self
                .symbols
                .iter()
                .take(limit)
                .map(String::as_str)
                .collect();
        }
        let mut prefix = Vec::new();
        let mut interior = Vec::new();
        for symbol in &self.symbols {
            if symbol.starts_with(&query) {
                prefix.push(symbol.as_str());
            } else if symbol.contains(&query) {
                interior.push(symbol.as_str());
            }
            if prefix.len() >= limit {
                break;
            }
        }
        prefix.extend(interior);
        prefix.truncate(limit);
        prefix
    }

    /// The single best completion for what has been typed, for the Tab key.
    ///
    /// `None` when nothing matches, which is the signal to leave the line alone
    /// rather than replace it with something unrelated.
    pub fn complete(&self, query: &str) -> Option<&str> {
        self.suggest(query, 1).first().copied()
    }

    /// The listed symbols that *start with* `query`, in file order, at most
    /// `limit`.
    ///
    /// Prefixes only, unlike [`Catalog::suggest`], because this answers a
    /// different question: which symbol did somebody mean by typing part of a
    /// name. An interior match is not what they typed — `usdt` may be inside
    /// `BTCUSDT`, and substituting it there would answer a request for a symbol
    /// with a different one — so this deliberately finds nothing for it.
    pub fn prefixed(&self, query: &str, limit: usize) -> Vec<&str> {
        let query = query.trim().to_ascii_uppercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.symbols
            .iter()
            .filter(|s| s.starts_with(&query))
            .take(limit)
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(names: &[&str]) -> Catalog {
        Catalog {
            symbols: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn an_empty_query_offers_the_head_of_the_file() {
        let c = catalog(&["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
        assert_eq!(c.suggest("", 2), vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(c.suggest("   ", 1), vec!["BTCUSDT"]);
    }

    #[test]
    fn a_prefix_beats_an_interior_match() {
        // The point of the ranking: typing ET should offer ETCUSDT, not a pair
        // that merely contains those letters.
        let c = catalog(&["BETACOIN", "ETCUSDT", "ETHUSDT", "PETUSDT"]);
        assert_eq!(
            c.suggest("ET", 4),
            vec!["ETCUSDT", "ETHUSDT", "BETACOIN", "PETUSDT"]
        );
    }

    #[test]
    fn ties_keep_the_file_order_so_the_liquid_pairs_come_first() {
        let c = catalog(&["ETHUSDT", "ETCUSDT", "ETHDOWN"]);
        assert_eq!(c.suggest("ET", 3), vec!["ETHUSDT", "ETCUSDT", "ETHDOWN"]);
    }

    #[test]
    fn the_query_is_case_insensitive_and_padded() {
        let c = catalog(&["BTCUSDT", "ETHUSDT"]);
        assert_eq!(c.suggest("btc", 5), vec!["BTCUSDT"]);
        assert_eq!(c.suggest("  eth  ", 5), vec!["ETHUSDT"]);
    }

    #[test]
    fn the_limit_holds_even_when_interior_matches_also_exist() {
        let c = catalog(&["AAAA", "AAAB", "BAAA", "CAAA"]);
        assert_eq!(c.suggest("A", 2), vec!["AAAA", "AAAB"]);
        // A single prefix match still leaves room for interior ones.
        assert_eq!(c.suggest("B", 3), vec!["BAAA", "AAAB"]);
    }

    #[test]
    fn nothing_matching_is_an_empty_list_and_no_completion() {
        let c = catalog(&["BTCUSDT"]);
        assert!(c.suggest("ZZZ", 5).is_empty());
        assert_eq!(c.complete("ZZZ"), None);
    }

    #[test]
    fn tab_completes_to_the_best_match() {
        let c = catalog(&["BTCUSDT", "ETHUSDT", "ETCUSDT"]);
        assert_eq!(c.complete("eth"), Some("ETHUSDT"));
        assert_eq!(c.complete("ET"), Some("ETHUSDT"));
        assert_eq!(c.complete(""), Some("BTCUSDT"));
    }

    #[test]
    fn a_prefix_query_finds_only_what_starts_with_it() {
        let c = catalog(&["BETACOIN", "ETHUSDT", "ETCUSDT", "BTCUSDT"]);
        assert_eq!(c.prefixed("ET", 8), vec!["ETHUSDT", "ETCUSDT"]);
        assert_eq!(c.prefixed("eth", 8), vec!["ETHUSDT"]);
        // The whole point of the difference from `suggest`: a name that merely
        // contains the query is not what was typed, so nothing is returned.
        assert!(c.prefixed("usdt", 8).is_empty());
        assert!(c.prefixed("", 8).is_empty());
        assert_eq!(c.prefixed("ET", 1), vec!["ETHUSDT"]);
    }

    #[test]
    fn membership_is_case_insensitive() {
        let c = catalog(&["BTCUSDT"]);
        assert!(c.contains("btcusdt"));
        assert!(c.contains("BTCUSDT"));
        assert!(!c.contains("ETHUSDT"));
    }

    #[test]
    fn a_missing_file_is_an_empty_catalog_rather_than_an_error() {
        let c = Catalog::load(Path::new("/nonexistent/tickers.txt")).expect("empty");
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        assert!(c.suggest("BTC", 5).is_empty());
    }

    #[test]
    fn a_symbol_too_long_for_the_wire_field_is_refused_with_its_line() {
        let dir = std::env::temp_dir().join(format!("ob-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tickers.txt");
        std::fs::write(&path, "# a comment\nBTCUSDT\n\n1000SHIBUSDT\n").expect("write");
        let err = Catalog::load(&path).expect_err("must be refused");
        let message = err.message.clone();
        assert!(message.contains("line 4"), "{message}");
        assert!(message.contains("1000SHIBUSDT"), "{message}");
        assert!(message.contains("12 bytes"), "{message}");

        // And the same file without the long line loads, skipping comments and
        // blanks, and uppercasing.
        std::fs::write(&path, "# a comment\nbtcusdt\n\nPEPEUSDT\n").expect("write");
        let c = Catalog::load(&path).expect("loads");
        assert_eq!(c.symbols(), &["BTCUSDT", "PEPEUSDT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_in_the_file_is_offered_once() {
        let dir = std::env::temp_dir().join(format!("ob-cat-dup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tickers.txt");
        std::fs::write(&path, "BTCUSDT\nbtcusdt\nETHUSDT\n").expect("write");
        let c = Catalog::load(&path).expect("loads");
        assert_eq!(c.symbols(), &["BTCUSDT", "ETHUSDT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_shipped_catalog_loads_and_is_a_sensible_size() {
        // The catalog the repository ships, checked by the loader that will read
        // it: a long line anywhere in it would fail here.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root");
        let path = root.join("config/tickers.txt");
        if !path.is_file() {
            return;
        }
        let c = Catalog::load(&path).expect("the shipped catalog must load");
        assert!(c.len() >= 40, "only {} symbols shipped", c.len());
        assert!(c.contains("BTCUSDT"));
        assert!(c.contains("PEPEUSDT"));
        // Nothing longer than the field, which the loader already enforces.
        assert!(c.symbols().iter().all(|s| s.len() <= 8));
    }
}
