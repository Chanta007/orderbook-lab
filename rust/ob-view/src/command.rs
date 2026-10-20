//! What can be typed, and what it completes to.
//!
//! One vocabulary, three readers: the command reference the frame draws, the picker
//! the terminal shows under the book, and the dropdown a browser shows under its
//! input bar. They used to be three lists — a formatted array of strings for the
//! reference, a symbol lookup for the picker, and nothing at all for the page — and
//! three lists is three answers to "what can I type here".
//!
//! # Why the completion lives here and not in the page
//!
//! The page's dropdown asks the server what to offer, and the server asks this. The
//! alternative — sending the catalog to the browser and completing in JavaScript —
//! would be a second implementation of the rules that decide what a prefix means, in
//! a second language, with the terminal's behaviour as the thing it must not drift
//! from. `docs/optimization/ledger.jsonl` has four entries about that class of
//! mistake; the round trip on loopback is about a millisecond.
//!
//! # The rules
//!
//! - The first token is matched against command names and their aliases, and against
//!   the symbols.
//! - A command that takes an argument offers its arguments after a space: `only eth`
//!   offers `only ETHUSDT`.
//! - A symbol is offered on a prefix first and inside the name second, so `ET` offers
//!   `ETCUSDT` before `BETACOIN` and `usdt` still finds the pairs — the same order the
//!   catalog's own search uses, because it *is* the catalog's own search
//!   ([`match_symbols`]).
//! - Nothing is offered for an empty line. Both frontends show nothing until
//!   something is typed, which keeps the dropdown from covering the book on every
//!   fresh window.

use crate::line::{Line, Span, Style};

/// What an argument is, if a command takes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    /// The command stands alone.
    None,
    /// A symbol, checked against the catalog.
    Symbol,
    /// A number, with its own bounds.
    Number,
}

/// A command the dispatcher accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// The name to type.
    pub name: &'static str,
    /// Other spellings the dispatcher accepts, without the name.
    pub aliases: &'static [&'static str],
    /// What, if anything, follows it.
    pub takes: Takes,
    /// One line for the reference and for the dropdown.
    pub summary: &'static str,
}

impl Command {
    /// The usage as it is written in the reference: `levels N`, `quit | q`.
    pub fn usage(&self) -> String {
        let mut out = self.name.to_string();
        match self.takes {
            Takes::Symbol => out.push_str(" SYMBOL"),
            Takes::Number => out.push_str(" N"),
            Takes::None => {}
        }
        if !self.aliases.is_empty() {
            out.push_str(" | ");
            out.push_str(&self.aliases.join(" | "));
        }
        out
    }

    /// Whether this command's first token is `token`, by name or by alias.
    pub fn answers_to(&self, token: &str) -> bool {
        let token = token.trim();
        self.name == token || self.aliases.contains(&token)
    }
}

/// One of the three times in the header, as a thing that can be asked about.
///
/// They were in the reference from the start and the dispatcher never knew them, so
/// typing `age` was answered with "AGE is not in config/tickers.txt" — the reference
/// promised three commands that did not exist. The cross-check test in
/// `ob-cli/src/actions.rs` is what found it: every command the reference offers is
/// one the dispatcher accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Explain {
    /// Records behind this reader, and the feed time they cover.
    Lag,
    /// How old the newest published record is.
    Staleness,
    /// How long since this window applied a record.
    Age,
}

impl Explain {
    /// All three, in the order the reference lists them.
    pub const ALL: [Explain; 3] = [Explain::Lag, Explain::Staleness, Explain::Age];

    /// The word that asks for it.
    pub fn name(self) -> &'static str {
        match self {
            Explain::Lag => "lag",
            Explain::Staleness => "staleness",
            Explain::Age => "age",
        }
    }

    /// The one-line summary the reference shows.
    pub fn summary(self) -> &'static str {
        match self {
            Explain::Lag => "records behind you, and the span of feed time that is",
            Explain::Staleness => "how old the newest published record is",
            Explain::Age => "how long since this window applied a record",
        }
    }

    /// Which of the three, from what was typed.
    pub fn of(token: &str) -> Option<Explain> {
        Explain::ALL.into_iter().find(|e| e.name() == token.trim())
    }

    /// The explanation, with the current value in it.
    ///
    /// Written once, here, because three frontends ask the same question and the
    /// answer to "what does age mean" must not depend on where it was asked.
    pub fn render(self, times: &Times) -> String {
        let backlog = match times.backlog_ns {
            Some(ns) => ob::clock::duration_ns(ns),
            None => "nothing to cover: this reader has taken everything".to_string(),
        };
        let staleness = match times.staleness_ns {
            Some(ns) => ob::clock::duration_ns(ns),
            None => "nothing has been published yet".to_string(),
        };
        match self {
            Explain::Lag => format!(
                "lag: {} records behind the writer, covering {backlog} of feed time. \
                 The count is how much is queued for you; the span is what that costs \
                 in latency, and it is the one to watch.",
                times.lag
            ),
            Explain::Staleness => format!(
                "staleness: {staleness}. The age of the newest record the writer has \
                 published, from the stamp the sender put on it. It is the exchange's \
                 age, not this window's: a large staleness with a small age means this \
                 host is fine and the feed is not.",
            ),
            Explain::Age => format!(
                "age: {}. How long since this window applied a record for the symbol on \
                 screen. Growing while staleness stays small means the records arriving \
                 are for other symbols; growing together with staleness means nothing \
                 is arriving at all.",
                ob::clock::duration_ns(times.age_ns)
            ),
        }
    }
}

/// The three times, as the numbers a frontend can hand to [`Explain::render`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Times {
    /// Records behind the writer.
    pub lag: u64,
    /// The feed time those records cover.
    pub backlog_ns: Option<u64>,
    /// The age of the newest published record.
    pub staleness_ns: Option<u64>,
    /// How long since this window applied a record.
    pub age_ns: u64,
}

/// One row of the reference: a command, or a key that is not a command.
///
/// The two keys are in the reference because a person needs to know they exist, and
/// out of the completion because neither is something you type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A command the dispatcher accepts.
    Command(Command),
    /// One of the three times, which the dispatcher also accepts.
    Explain(Explain),
    /// A key, which the reference documents and the completion does not offer.
    Key {
        /// How the reference writes it.
        name: &'static str,
        /// What it does.
        summary: &'static str,
    },
}

/// Every command, in the order the reference reads best.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "quit",
        aliases: &["q"],
        takes: Takes::None,
        summary: "leave this window",
    },
    Command {
        name: "help",
        aliases: &["?"],
        takes: Takes::None,
        summary: "show or hide this reference",
    },
    Command {
        name: "status",
        aliases: &[],
        takes: Takes::None,
        summary: "the feed, the queues and the ticks, as a dashboard",
    },
    Command {
        name: "book",
        aliases: &[],
        takes: Takes::None,
        summary: "back to the price table",
    },
    Command {
        name: "levels",
        aliases: &[],
        takes: Takes::Number,
        summary: "show N price levels, from 1 to 64",
    },
    Command {
        name: "watch",
        aliases: &[],
        takes: Takes::None,
        summary: "list what is being watched",
    },
    Command {
        name: "only",
        aliases: &[],
        takes: Takes::Symbol,
        summary: "watch just that one",
    },
    Command {
        name: "unwatch",
        aliases: &[],
        takes: Takes::Symbol,
        summary: "stop watching one",
    },
    Command {
        name: "force",
        aliases: &[],
        takes: Takes::Symbol,
        summary: "switch to a symbol the catalog does not list",
    },
];

/// The reference, in reading order, including the two keys.
pub const REFERENCE: &[Row] = &[
    Row::Command(COMMANDS[0]), // quit
    Row::Command(COMMANDS[1]), // help
    Row::Command(COMMANDS[2]), // status
    Row::Command(COMMANDS[3]), // book
    Row::Command(COMMANDS[4]), // levels
    Row::Key {
        name: "SYMBOL",
        summary: "switch and watch; a unique prefix works; Tab, then arrows",
    },
    Row::Key {
        name: "Escape",
        summary: "clear what you have typed",
    },
    Row::Command(COMMANDS[5]), // watch
    Row::Command(COMMANDS[6]), // only
    Row::Command(COMMANDS[7]), // unwatch
    Row::Command(COMMANDS[8]), // force
    Row::Explain(Explain::Lag),
    Row::Explain(Explain::Staleness),
    Row::Explain(Explain::Age),
];

/// The column the summaries start in, so the reference is scannable.
const SUMMARY_COLUMN: usize = 17;

/// The command reference, as the frame draws it.
///
/// Built from [`REFERENCE`] rather than written out, so a command cannot be added to
/// the dispatcher and left out of the reference — or listed in the reference and
/// refused by the dispatcher. `python/test_web.py` and the dispatcher's own tests
/// check both directions.
pub fn help_lines() -> Vec<Line> {
    REFERENCE
        .iter()
        .map(|row| {
            let (usage, summary) = match row {
                Row::Command(command) => (command.usage(), command.summary),
                Row::Explain(explain) => (explain.name().to_string(), explain.summary()),
                Row::Key { name, summary } => ((*name).to_string(), *summary),
            };
            let pad = SUMMARY_COLUMN.saturating_sub(usage.chars().count()).max(1);
            Line::new()
                .with(Span::plain("  "))
                .with(Span::new(usage, Style::Accent))
                .with(Span::new(" ".repeat(pad), Style::Plain))
                .with(Span::new(summary, Style::Dim))
        })
        .collect()
}

/// What kind of thing a suggestion is, for a UI that shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A command.
    Command,
    /// A symbol from the catalog.
    Symbol,
}

impl Kind {
    /// The name a frontend puts on it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Symbol => "symbol",
        }
    }
}

/// One thing the line could become.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What to show in the list: `ETHUSDT`, or `only ETHUSDT`.
    pub label: String,
    /// The whole line after completing to it, which is what Tab inserts.
    pub insert: String,
    /// Which kind it is.
    pub kind: Kind,
    /// One line of explanation, for a dropdown that has room for it.
    pub detail: String,
}

impl Suggestion {
    /// The name without its argument, for a frontend that wants the short form.
    pub fn first_token(&self) -> &str {
        self.insert.split_whitespace().next().unwrap_or("")
    }
}

/// Suggestions as JSON, for a frontend that completes over the wire.
///
/// Hand-written like the frame's renderer, for the same reason: this crate has no
/// dependencies and the shape is four fields.
pub fn suggestions_json(suggestions: &[Suggestion]) -> String {
    let mut out = String::with_capacity(suggestions.len() * 96 + 24);
    out.push_str("{\"suggestions\":[");
    for (i, suggestion) in suggestions.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"label\":\"");
        crate::line::escape_json(&suggestion.label, &mut out);
        out.push_str("\",\"insert\":\"");
        crate::line::escape_json(&suggestion.insert, &mut out);
        out.push_str("\",\"kind\":\"");
        out.push_str(suggestion.kind.name());
        out.push_str("\",\"detail\":\"");
        crate::line::escape_json(&suggestion.detail, &mut out);
        out.push_str("\"}");
    }
    out.push_str("]}");
    out
}

/// The symbols matching `query`, prefix first and interior second, at most `limit`.
///
/// The order is the catalog's: a symbol that starts with what was typed comes before
/// one that merely contains it, and ties keep the file's order. `Catalog::suggest`
/// delegates here, so the terminal's picker, the browser's dropdown and the
/// dispatcher's prefix resolution cannot disagree about which symbol is the better
/// match for `ET`.
pub fn match_symbols(symbols: &[String], query: &str, limit: usize) -> Vec<String> {
    let query = query.trim().to_ascii_uppercase();
    if query.is_empty() {
        return symbols.iter().take(limit).cloned().collect();
    }
    let mut prefix = Vec::new();
    let mut interior = Vec::new();
    for symbol in symbols {
        if symbol.starts_with(&query) {
            prefix.push(symbol.clone());
            if prefix.len() >= limit {
                break;
            }
        } else if symbol.contains(&query) {
            interior.push(symbol.clone());
        }
    }
    prefix.extend(interior);
    prefix.truncate(limit);
    prefix
}

/// What the line could become, best first, at most `limit`.
///
/// An empty line completes to nothing: both frontends show their list only once
/// something is typed.
pub fn complete(line: &str, symbols: &[String], limit: usize) -> Vec<Suggestion> {
    let typed = line.trim_start();
    if typed.is_empty() || limit == 0 {
        return Vec::new();
    }
    let (first, rest) = match typed.split_once(' ') {
        Some((first, rest)) => (first, Some(rest)),
        None => (typed, None),
    };

    // An argument position: a command that takes one, with a space after it.
    if let Some(rest) = rest {
        let Some(command) = COMMANDS.iter().find(|c| c.answers_to(first)) else {
            return Vec::new();
        };
        return match command.takes {
            Takes::Symbol => match_symbols(symbols, rest, limit)
                .into_iter()
                .map(|symbol| Suggestion {
                    label: format!("{first} {symbol}"),
                    insert: format!("{first} {symbol}"),
                    kind: Kind::Symbol,
                    detail: format!("{}: {}", command.name, command.summary),
                })
                .collect(),
            // A number is typed, not chosen: there is no list of numbers to offer.
            Takes::None | Takes::Number => Vec::new(),
        };
    }

    // The dropdown lists what the reference lists, in the order the reference lists
    // it. Two orderings — one for the list a person reads and one for the list a
    // prefix matches — would answer `sta` with `staleness` in the completion and
    // `status` in the reference, and neither would be wrong.
    let mut out = Vec::with_capacity(limit);
    for row in REFERENCE {
        if out.len() == limit {
            break;
        }
        let suggestion = match row {
            Row::Command(command) => {
                let matches = command.name.starts_with(first)
                    || command.aliases.iter().any(|alias| alias.starts_with(first));
                matches.then(|| Suggestion {
                    label: command.name.to_string(),
                    // A command that takes an argument completes with the space, so
                    // the next keystroke starts the argument rather than the command.
                    insert: match command.takes {
                        Takes::None => command.name.to_string(),
                        Takes::Symbol | Takes::Number => format!("{} ", command.name),
                    },
                    kind: Kind::Command,
                    detail: command.summary.to_string(),
                })
            }
            Row::Explain(explain) => explain.name().starts_with(first).then(|| Suggestion {
                label: explain.name().to_string(),
                insert: explain.name().to_string(),
                kind: Kind::Command,
                detail: explain.summary().to_string(),
            }),
            // A key is not something you type, so it is not offered.
            Row::Key { .. } => None,
        };
        if let Some(suggestion) = suggestion {
            out.push(suggestion);
        }
    }
    for symbol in match_symbols(symbols, first, limit.saturating_sub(out.len())) {
        out.push(Suggestion {
            label: symbol.clone(),
            insert: symbol,
            kind: Kind::Symbol,
            detail: "switch to it and watch it".to_string(),
        });
    }
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols() -> Vec<String> {
        ["BTCUSDT", "ETHUSDT", "ETCUSDT", "SOLUSDT", "BETACOIN"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    fn labels(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.label.as_str()).collect()
    }

    #[test]
    fn a_command_prefix_offers_the_command() {
        let got = complete("bo", &symbols(), 8);
        assert_eq!(
            labels(&got),
            vec!["book"],
            "and nothing else starts with bo"
        );
        assert_eq!(got[0].kind, Kind::Command);
        assert_eq!(got[0].insert, "book");
    }

    #[test]
    fn a_command_that_takes_an_argument_completes_with_a_space() {
        let got = complete("lev", &symbols(), 8);
        assert_eq!(got[0].label, "levels");
        assert_eq!(
            got[0].insert, "levels ",
            "so the next keystroke starts the argument"
        );
    }

    #[test]
    fn an_alias_is_offered_and_completes_to_the_name() {
        // `q` is an alias for `quit`; completing it to the name it means is what a
        // shell does, and it leaves the line in the form the reference documents.
        let got = complete("q", &symbols(), 8);
        assert_eq!(labels(&got), vec!["quit"]);
        assert_eq!(got[0].insert, "quit");
    }

    #[test]
    fn the_dropdown_lists_what_the_reference_lists_in_its_order() {
        // `sta` matches `status` and `staleness`. The reference lists the commands
        // first, so the completion does too: a prefix answered with the twelfth entry
        // of the list would make Tab and the reference disagree.
        let got = complete("sta", &symbols(), 8);
        assert_eq!(labels(&got), vec!["status", "staleness"]);
        // And the whole order follows the reference.
        let offered: Vec<&str> = REFERENCE
            .iter()
            .filter_map(|row| match row {
                Row::Command(c) => Some(c.name),
                Row::Explain(e) => Some(e.name()),
                Row::Key { .. } => None,
            })
            .collect();
        let every = complete("", &symbols(), 0);
        assert!(every.is_empty(), "an empty line offers nothing");
        for (i, name) in offered.iter().enumerate() {
            let got = complete(name, &symbols(), 1);
            if !got.is_empty() {
                assert_eq!(got[0].label, *name, "entry {i} of the reference");
            }
        }
    }

    #[test]
    fn a_symbol_prefix_comes_after_the_commands_that_match() {
        let got = complete("b", &symbols(), 8);
        let names: Vec<&str> = labels(&got);
        assert_eq!(names[0], "book", "commands first: {names:?}");
        assert!(names.contains(&"BTCUSDT"), "{names:?}");
        assert!(names.contains(&"BETACOIN"), "{names:?}");
    }

    #[test]
    fn a_symbol_argument_is_offered_after_the_command() {
        let got = complete("only et", &symbols(), 8);
        // Prefix matches first, then the interior ones: `BETACOIN` contains `ET` and
        // is a symbol you could name here, so it is offered below the two that start
        // with it rather than hidden.
        assert_eq!(
            labels(&got),
            vec!["only ETHUSDT", "only ETCUSDT", "only BETACOIN"]
        );
        assert_eq!(got[0].insert, "only ETHUSDT");
        assert_eq!(got[0].first_token(), "only");
    }

    #[test]
    fn an_alias_takes_its_argument_too() {
        // `force` has no alias, but `q`/`quit` show that aliases are resolved; a
        // command's argument position follows the alias to the same command.
        let got = complete("force zzz", &symbols(), 8);
        assert!(
            got.is_empty(),
            "force takes a symbol the catalog does not list"
        );
    }

    #[test]
    fn a_number_argument_offers_nothing_because_there_is_no_list_to_offer() {
        assert!(complete("levels ", &symbols(), 8).is_empty());
        assert!(complete("levels 7", &symbols(), 8).is_empty());
    }

    #[test]
    fn an_empty_line_offers_nothing() {
        // Both frontends show their list only once something is typed.
        assert!(complete("", &symbols(), 8).is_empty());
        assert!(complete("   ", &symbols(), 8).is_empty());
        assert!(complete("bo", &symbols(), 0).is_empty(), "a limit of zero");
    }

    #[test]
    fn an_interior_match_is_offered_after_the_prefixes() {
        // `usdt` is inside every pair here: a search-like list is useful in a
        // dropdown, and the prefix matches still come first.
        let got = complete("usdt", &symbols(), 8);
        assert_eq!(got.len(), 4, "{:?}", labels(&got));
        assert_eq!(labels(&got)[0], "BTCUSDT", "file order among equals");
    }

    #[test]
    fn the_limit_is_respected_across_both_kinds() {
        let got = complete("s", &symbols(), 3);
        assert_eq!(got.len(), 3, "{:?}", labels(&got));
    }

    #[test]
    fn nothing_typed_matches_nothing_rather_than_everything() {
        let got = complete("zzzz", &symbols(), 8);
        assert!(got.is_empty(), "{:?}", labels(&got));
    }

    #[test]
    fn the_reference_lists_every_command_and_nothing_that_is_not_one() {
        let rows: Vec<String> = help_lines().iter().map(Line::text).collect();
        let text = rows.join("\n");
        for command in COMMANDS {
            assert!(
                text.contains(command.name),
                "the reference omits {}: {text}",
                command.name
            );
            assert!(
                text.contains(command.summary),
                "the reference omits the summary of {}",
                command.name
            );
        }
        for explain in Explain::ALL {
            assert!(
                text.contains(explain.name()),
                "the reference omits {}: {text}",
                explain.name()
            );
            assert!(text.contains(explain.summary()));
        }
        assert!(text.contains("SYMBOL"), "the key rows are in it");
        assert!(text.contains("Escape"));
        assert_eq!(rows.len(), REFERENCE.len());
    }

    #[test]
    fn the_reference_is_aligned_so_it_can_be_scanned() {
        let rows: Vec<String> = help_lines().iter().map(Line::text).collect();
        // Every summary starts in the same column, which is what makes a list of
        // fourteen rows readable rather than a paragraph. The usage is taken from the
        // reference rather than guessed from the row: `quit | q` has spaces inside it,
        // so "the first gap" is not where the usage ends.
        for (row, reference) in rows.iter().zip(REFERENCE) {
            let usage = match reference {
                Row::Command(command) => command.usage(),
                Row::Explain(explain) => explain.name().to_string(),
                Row::Key { name, .. } => (*name).to_string(),
            };
            assert!(row.starts_with(&format!("  {usage}")), "row: {row:?}");
            let rest = &row[2 + usage.len()..];
            let gap = rest.len() - rest.trim_start().len();
            assert_eq!(
                2 + usage.len() + gap,
                2 + SUMMARY_COLUMN,
                "misaligned row: {row:?}"
            );
        }
    }

    #[test]
    fn a_usage_reads_the_way_it_is_typed() {
        let levels = COMMANDS
            .iter()
            .find(|c| c.name == "levels")
            .expect("levels");
        assert_eq!(levels.usage(), "levels N");
        let only = COMMANDS.iter().find(|c| c.name == "only").expect("only");
        assert_eq!(only.usage(), "only SYMBOL");
        let quit = COMMANDS.iter().find(|c| c.name == "quit").expect("quit");
        assert_eq!(quit.usage(), "quit | q");
        assert!(quit.answers_to("q"));
        assert!(quit.answers_to("quit"));
        assert!(!quit.answers_to("quitter"));
    }

    #[test]
    fn suggestions_render_as_json_for_a_frontend_over_the_wire() {
        let got = complete("only et", &symbols(), 1);
        let json = suggestions_json(&got);
        assert_eq!(
            json,
            "{\"suggestions\":[{\"label\":\"only ETHUSDT\",\"insert\":\"only ETHUSDT\",\
             \"kind\":\"symbol\",\"detail\":\"only: watch just that one\"}]}"
        );
        assert_eq!(suggestions_json(&[]), "{\"suggestions\":[]}");
    }

    #[test]
    fn a_suggestion_cannot_break_the_json_it_is_written_into() {
        // The catalog is a file a person edits; a symbol with a quote in it must not
        // be able to close the string it is written into.
        let odd = vec!["A\"B".to_string()];
        let got = complete("a", &odd, 4);
        let json = suggestions_json(&got);
        assert!(json.contains("A\\\"B"), "{json}");
        assert!(!json.contains("A\"B"), "{json}");
    }

    #[test]
    fn the_matcher_is_the_one_the_catalog_uses() {
        // `Catalog::suggest` delegates here, so this ordering is the ordering both
        // frontends and the dispatcher see.
        let got = match_symbols(&symbols(), "et", 8);
        assert_eq!(got, vec!["ETHUSDT", "ETCUSDT", "BETACOIN"]);
        assert_eq!(match_symbols(&symbols(), "", 2), vec!["BTCUSDT", "ETHUSDT"]);
        assert!(match_symbols(&symbols(), "zz", 8).is_empty());
    }
}
