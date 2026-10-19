//! What a typed line means, and what the window should do about it.
//!
//! This is the dispatcher: one function from a line of text to an [`Action`]. It
//! used to live inside the terminal binary, which is why nothing else could run a
//! command — the web frontend and the `view` binary take the same path through
//! here, so a command cannot mean one thing in the terminal and another in a
//! browser.
//!
//! It is deliberately free of the terminal, the ring and the clock: everything it
//! needs is a [`Catalog`] to check a symbol against, a [`Watch`] to change, and the
//! path of the watch file to write. That is what makes it a unit test rather than a
//! pty test.

use crate::tickers::Catalog;
use crate::watch::Watch;
use ob::msg::symbol_fits;
use ob_view::Panel;
use std::path::Path;

/// How many matches a prefix may have before it is ambiguous.
///
/// Eight, which is also how many the picker offers: a prefix that matches more than
/// a screenful is not a prefix, and the refusal lists at most this many before
/// saying there are more.
pub const SUGGESTIONS: usize = 8;

/// What a submitted line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Leave.
    Quit,
    /// Show this many levels.
    Depth(usize),
    /// Display this symbol, watching it if it was not.
    Switched {
        /// Whether the watch set grew.
        added: bool,
        /// The symbol now displayed.
        symbol: String,
        /// Something worth saying about it, such as it not being in the catalog.
        ///
        /// Part of the action rather than a side channel: an earlier version
        /// attached it with a helper that discarded the text, which is the same
        /// shape of defect as a message nobody reads.
        remark: Option<String>,
    },
    /// Say something and change nothing.
    Note(String),
    /// Switch the main area between the price table and the dashboard.
    Panel(Panel),
    /// Show or hide the command reference.
    ToggleHelp,
}

/// Act on a submitted line.
///
/// Unknown commands are ignored with a note, which is the same behaviour the C++
/// terminal has for a typo: it does nothing rather than stopping the viewer.
pub fn act(text: &str, catalog: &Catalog, watch: &mut Watch, watch_path: &Path) -> Action {
    let line = text.trim();
    if line.is_empty() {
        return Action::Note(String::new());
    }
    if line == "q" || line == "quit" {
        return Action::Quit;
    }
    // The reference and the dashboard are commands rather than keys, so the same
    // words work from a shell (`view --act "status"`) and from the web page.
    if line == "help" || line == "?" {
        return Action::ToggleHelp;
    }
    if line == "status" {
        return Action::Panel(Panel::Status);
    }
    if line == "book" {
        return Action::Panel(Panel::Book);
    }
    if let Some(n) = parse_levels(line) {
        return Action::Depth(n);
    }
    if line == "watch" {
        return Action::Note(format!(
            "watching {}: {}",
            watch.len(),
            watch.symbols().join(" ")
        ));
    }
    if let Some(rest) = line.strip_prefix("unwatch ") {
        return match watch.remove(rest) {
            Ok(true) => match watch.save(watch_path) {
                Ok(()) => Action::Note(format!(
                    "unwatched {}; {} left",
                    rest.trim().to_ascii_uppercase(),
                    watch.len()
                )),
                Err(e) => Action::Note(e.message),
            },
            Ok(false) => Action::Note(format!("{} was not being watched", rest.trim())),
            Err(e) => Action::Note(e.message),
        };
    }
    if let Some(rest) = line.strip_prefix("only ") {
        // Through the same resolution as a bare symbol, because this writes the
        // watch file too: `only eth` used to put the literal string `ETH` in it,
        // and an unlisted stream name in the combined URL can take the whole
        // subscription down. That check was on the bare-symbol path only, so the
        // form that replaces the entire subscription was the one without it.
        let (canonical, remark) = match resolve(rest, false, catalog) {
            Resolution::Symbol { canonical, remark } => (canonical, remark),
            Resolution::Refused(text) => return Action::Note(text),
        };
        return match watch.only(&canonical) {
            Ok(()) => match watch.save(watch_path) {
                Ok(()) => Action::Switched {
                    added: false,
                    symbol: watch.current().to_string(),
                    remark,
                },
                Err(e) => Action::Note(e.message),
            },
            Err(e) => Action::Note(e.message),
        };
    }
    // `force SYMBOL` is the way past the catalog check below.
    let (symbol, forced) = match line.strip_prefix("force ") {
        Some(rest) => (rest.trim(), true),
        None => (
            match line.strip_prefix("watch ") {
                Some(rest) => rest.trim(),
                None => line,
            },
            false,
        ),
    };
    if !symbol_fits(symbol) {
        return Action::Note(format!(
            "{symbol:?} is not a command and does not fit the 8-byte symbol field"
        ));
    }
    let (canonical, remark) = match resolve(symbol, forced, catalog) {
        Resolution::Symbol { canonical, remark } => (canonical, remark),
        Resolution::Refused(text) => return Action::Note(text),
    };

    match watch.select(&canonical) {
        Ok(added) => match watch.save(watch_path) {
            Ok(()) => Action::Switched {
                added,
                symbol: watch.current().to_string(),
                remark,
            },
            Err(e) => Action::Note(e.message),
        },
        Err(e) => Action::Note(e.message),
    }
}

/// What a typed name resolved to.
#[derive(Debug)]
pub enum Resolution {
    /// A symbol to act on, and anything worth saying about how it was arrived at.
    Symbol {
        /// The canonical form: upper case, and from the catalog when it was
        /// completed from one.
        canonical: String,
        /// Empty unless the name was completed or forced.
        remark: Option<String>,
    },
    /// Nothing to act on, with the reason to print.
    Refused(String),
}

/// Turn what was typed into a symbol the watch file may hold.
///
/// A name the catalog does not list is **refused rather than tried**, because all
/// the watched symbols share one websocket and an unknown stream name in a
/// combined URL can take the whole connection down: one typo would stop the feed
/// for every other pair. That has not changed. What changed is what happens to a
/// name that is *most of* a listed one:
///
/// - exactly one listed symbol starts with it: use that symbol, and say so. The
///   picker was already showing the name the person meant, so refusing `eth` told
///   them their own shortcut was wrong;
/// - more than one: refuse and name them, because guessing between `ETHUSDT` and
///   `ETCUSDT` is not a choice to make for somebody;
/// - none: refuse as before, with `force` and its risk named;
/// - `force` skips the whole question, including the completion: it means "this
///   exact string", which is the point of it.
///
/// Only a *prefix* is completed, never an interior match: `usdt` appears inside
/// `BTCUSDT`, and substituting that would answer a request for one symbol with a
/// different one. [`Catalog::prefixed`] is that rule, and it is tested.
pub fn resolve(symbol: &str, forced: bool, catalog: &Catalog) -> Resolution {
    let typed = symbol.trim().to_ascii_uppercase();
    if forced {
        return Resolution::Symbol {
            remark: Some(format!("{typed} is not in the catalog; forced anyway")),
            canonical: typed,
        };
    }
    if catalog.contains(&typed) {
        return Resolution::Symbol {
            canonical: typed,
            remark: None,
        };
    }
    match catalog.prefixed(&typed, SUGGESTIONS + 1).as_slice() {
        [only] => Resolution::Symbol {
            remark: Some(format!(
                "{typed} is not listed; {only} is the only symbol starting with it"
            )),
            canonical: (*only).to_string(),
        },
        [] => Resolution::Refused(format!(
            "{typed} is not in config/tickers.txt. Add it there, or type \
             `force {typed}` to try it anyway: an unknown stream name can \
             break the subscription for every symbol."
        )),
        several => {
            let more = if several.len() > SUGGESTIONS {
                " (and more)"
            } else {
                ""
            };
            let listed: Vec<&str> = several.iter().take(SUGGESTIONS).copied().collect();
            Resolution::Refused(format!(
                "{typed} is not in config/tickers.txt, and {} symbols start with \
                 it: {}{more}. Type more of the name, or `force {typed}` to try it \
                 anyway.",
                several.len(),
                listed.join(", ")
            ))
        }
    }
}

/// The first whitespace-separated token, which is what the picker matches on.
pub fn first_token(line: &str) -> String {
    line.split_whitespace().next().unwrap_or("").to_string()
}

/// The level count a `levels N` line asks for, when it is in range.
///
/// `None` for anything else, including a count outside the bounds: a command that
/// silently clamps is a command that lies about what it did.
pub fn parse_levels(line: &str) -> Option<usize> {
    let rest = line.trim().strip_prefix("levels ")?;
    let n: usize = rest.trim().parse().ok()?;
    if (LEVELS_MIN..=LEVELS_MAX).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// The depth limits a `levels` command accepts.
pub const LEVELS_MIN: usize = 1;
/// The depth limits a `levels` command accepts.
pub const LEVELS_MAX: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The repository's catalog, found the way any test must find a repo file:
    /// from the crate's own directory, not from the working directory, which is
    /// the crate directory when `cargo test` runs. A relative path here loaded
    /// nothing, so every symbol looked unlisted and three tests failed for a
    /// reason that had nothing to do with the behaviour under test.
    fn repo_catalog() -> Catalog {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf();
        Catalog::load(&root.join("config/tickers.txt")).expect("the shipped catalog must load")
    }
    fn watch_at(path: &std::path::Path) -> (Watch, PathBuf, Catalog) {
        (
            Watch::load(path, "BTCUSDT").expect("watch"),
            path.to_path_buf(),
            repo_catalog(),
        )
    }

    #[test]
    fn levels_parses_within_range_and_rejects_outside_it() {
        assert_eq!(parse_levels("levels 5"), Some(5));
        assert_eq!(parse_levels("  levels 1  "), Some(1));
        assert_eq!(parse_levels("levels 64"), Some(64));
        assert_eq!(parse_levels("levels 0"), None);
        assert_eq!(parse_levels("levels 65"), None);
        assert_eq!(parse_levels("levels"), None);
        assert_eq!(parse_levels("levels abc"), None);
        assert_eq!(parse_levels("quit"), None);
    }
    #[test]
    fn the_picker_matches_on_the_first_token_only() {
        // `watch ETH` should suggest ETHUSDT, not treat "ETH" as part of a longer
        // query that matches nothing.
        assert_eq!(first_token("eth"), "eth");
        assert_eq!(first_token("watch eth"), "watch");
        assert_eq!(first_token("   et   "), "et");
        assert_eq!(first_token(""), "");
    }
    #[test]
    fn a_bare_symbol_switches_and_watches_it() {
        let dir = std::env::temp_dir().join(format!("ob-act-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        let action = act("pepeusdt", &catalog, &mut w, &path);
        match action {
            Action::Switched { added, symbol, .. } => {
                assert!(added, "it was not being watched");
                assert_eq!(symbol, "PEPEUSDT");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT", "PEPEUSDT"]);
        assert_eq!(w.current(), "PEPEUSDT");
        // And the file on disk says so, which is what the adapter reads.
        let written = std::fs::read_to_string(&path).expect("written");
        assert!(written.contains("PEPEUSDT"), "{written}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn watch_add_subcommand_and_only_both_work() {
        let dir = std::env::temp_dir().join(format!("ob-act2-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        act("watch ethusdt", &catalog, &mut w, &path);
        assert_eq!(w.symbols(), &["BTCUSDT", "ETHUSDT"]);
        assert_eq!(w.current(), "ETHUSDT", "watch switches to what it adds");
        act("only pepeusdt", &catalog, &mut w, &path);
        assert_eq!(w.symbols(), &["PEPEUSDT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn unwatch_removes_and_the_plain_command_lists() {
        let dir = std::env::temp_dir().join(format!("ob-act3-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        act("only pepeusdt", &catalog, &mut w, &path);
        act("watch ethusdt", &catalog, &mut w, &path);
        match act("unwatch ethusdt", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("unwatched ETHUSDT"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["PEPEUSDT"]);
        match act("watch", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("PEPEUSDT"), "{text}"),
            other => panic!("{other:?}"),
        }
        // Removing the last one is refused with a reason.
        match act("unwatch pepeusdt", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("only symbol"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(w.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_symbol_that_does_not_fit_is_refused_with_a_reason() {
        let dir = std::env::temp_dir().join(format!("ob-act4-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        match act("1000shibusdt", &catalog, &mut w, &path) {
            Action::Note(text) => {
                assert!(text.contains("does not fit"), "{text}");
                assert!(text.contains("8-byte"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"], "nothing changed");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn quit_and_levels_are_still_commands() {
        let dir = std::env::temp_dir().join(format!("ob-act5-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert_eq!(act("q", &catalog, &mut w, &path), Action::Quit);
        assert_eq!(act("quit", &catalog, &mut w, &path), Action::Quit);
        assert_eq!(act("levels 7", &catalog, &mut w, &path), Action::Depth(7));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_symbol_the_catalog_does_not_list_is_refused_with_the_way_past_it() {
        // Refused, not tried: every watched symbol shares one websocket, and an
        // unknown stream name in a combined URL can take the whole connection
        // down. One typo would stop the feed for every other pair.
        let dir = std::env::temp_dir().join(format!("ob-act6-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(
            !catalog.contains("ZZZTUSDT"),
            "the test needs an unlisted symbol"
        );
        match act("zzztusdt", &catalog, &mut w, &path) {
            Action::Note(text) => {
                assert!(text.contains("not in config/tickers.txt"), "{text}");
                assert!(text.contains("force ZZZTUSDT"), "{text}");
                assert!(text.contains("break the subscription"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"], "nothing changed");

        // And the escape hatch works, with a remark saying what was done.
        match act("force zzztusdt", &catalog, &mut w, &path) {
            Action::Switched { symbol, remark, .. } => {
                assert_eq!(symbol, "ZZZTUSDT");
                assert!(
                    remark.as_deref().is_some_and(|r| r.contains("forced")),
                    "{remark:?}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.current(), "ZZZTUSDT");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_listed_symbol_needs_no_force() {
        let dir = std::env::temp_dir().join(format!("ob-act7-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(catalog.contains("ETHUSDT"));
        match act("ethusdt", &catalog, &mut w, &path) {
            Action::Switched { remark, .. } => assert!(remark.is_none(), "{remark:?}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_prefix_matching_one_symbol_switches_to_it_and_says_so() {
        let dir = std::env::temp_dir().join(format!("ob-act8-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(catalog.contains("ETHUSDT"));
        match act("eth", &catalog, &mut w, &path) {
            Action::Switched {
                added,
                symbol,
                remark,
            } => {
                assert!(added);
                assert_eq!(symbol, "ETHUSDT", "the completed symbol is displayed");
                let text = remark.expect("the completion is worth saying");
                assert!(text.contains("ETHUSDT"), "{text}");
                assert!(text.contains("only symbol starting with it"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // And the watch file holds the symbol, never the prefix.
        let written = std::fs::read_to_string(&path).expect("written");
        assert!(written.contains("ETHUSDT"), "{written}");
        assert!(!written.contains("\nETH\n"), "{written}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_prefix_matching_several_symbols_is_refused_with_them_named() {
        let dir = std::env::temp_dir().join(format!("ob-act9-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        // `ET` starts both ETHUSDT and ETCUSDT in the shipped catalog. Counted
        // rather than assumed, so the test fails on a catalog edit that made the
        // case disappear rather than passing for the wrong reason.
        let starts: Vec<&str> = catalog.prefixed("ET", 8);
        assert!(starts.len() >= 2, "{starts:?}");
        match act("et", &catalog, &mut w, &path) {
            Action::Note(text) => {
                assert!(text.contains("not in config/tickers.txt"), "{text}");
                for symbol in &starts {
                    assert!(text.contains(symbol), "{symbol} missing from {text}");
                }
                assert!(text.contains("force ET"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"], "nothing changed");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn an_interior_match_is_never_substituted() {
        // `usdt` is inside almost every symbol in the catalog and starts none of
        // them. Substituting the first would answer a request for one thing with
        // another, so this is refused.
        let dir = std::env::temp_dir().join(format!("ob-act10-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(
            !catalog.suggest("USDT", 1).is_empty(),
            "the catalog changed"
        );
        assert!(
            catalog.prefixed("USDT", 1).is_empty(),
            "the catalog changed"
        );
        match act("usdt", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("not in config/tickers.txt"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn only_goes_through_the_same_catalog_check_as_a_bare_symbol() {
        // `only SYMBOL` replaces the whole subscription, so it is the form where an
        // unlisted name does the most damage: one bad stream name in the combined
        // URL can take the connection down for every symbol. It used to skip the
        // check entirely and write whatever was typed into the watch file.
        let dir = std::env::temp_dir().join(format!("ob-act11-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(!catalog.contains("ZZZTUSDT"));
        match act("only zzztusdt", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("not in config/tickers.txt"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"], "the watch set is untouched");

        // A prefix of a listed symbol is completed here too.
        match act("only eth", &catalog, &mut w, &path) {
            Action::Switched { added, symbol, .. } => {
                assert!(!added, "only replaces the set rather than growing it");
                assert_eq!(symbol, "ETHUSDT");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["ETHUSDT"]);
        let written = std::fs::read_to_string(&path).expect("written");
        assert!(written.contains("ETHUSDT"), "{written}");
        assert!(!written.contains("\nETH\n"), "{written}");

        // And the explicit way past the check still works.
        match act("force zzztusdt", &catalog, &mut w, &path) {
            Action::Switched { symbol, .. } => assert_eq!(symbol, "ZZZTUSDT"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
