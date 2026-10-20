//! What a frame says: a snapshot and a little view state, in, lines out.
//!
//! This is the function every frontend calls. It is pure: no file, no socket, no
//! terminal, no clock of its own. That is what makes a panel testable by building a
//! [`Snapshot`] by hand and asserting the lines, and it is what lets one frame be a
//! terminal, a page and a JSON document at the same time.
//!
//! # The budget
//!
//! A frame drawn with a home-and-redraw writer has one hard rule: **never write
//! more rows than the terminal has.** One row too many scrolls the screen, and a
//! scrolled screen makes every later frame's row arithmetic wrong — the bug shows
//! up as corruption several frames later, which is why the rule is enforced here
//! *and* in [`crate::line::FrameCache`].
//!
//! Rows are spent in a fixed order, and each block is told what it may use:
//!
//! ```text
//!   1  prompt (always)
//!   3  header and the two time rows (always)
//!   n  the diagnostic, when the feed is not live (up to four rows)
//!   n  the picker, while something is typed (up to eight rows)
//!   n  the help panel, when it is open (below the prompt)
//!   →  the body takes what is left: the price table or the dashboard
//! ```
//!
//! The body is the block that can give up rows without losing meaning, so it gives
//! them up first, and it never takes less than [`MIN_BODY`] or it is not drawn at
//! all.

use crate::bars;
use crate::line::{Line, Span, Style};
use crate::snapshot::{Adapter, InputMode, Snapshot};

/// How long without a record before the frame says the feed has stalled.
///
/// Two seconds: far longer than the 100 ms the exchange sends at, and far shorter
/// than a person's patience.
pub const STALL_AFTER_NS: u64 = 2_000_000_000;

/// The most rows a status message may take.
///
/// Two: enough for a refusal that names the file to edit and the way past it, and
/// not enough for an essay to push the book off the screen.
pub const STATUS_ROWS: usize = 2;

/// The most rows the reason may take, before the remedy.
///
/// Three: two sentences of cause, wrapped, and the line that says what this window
/// did or did not apply. A block that competes with the book for rows has to be
/// bounded by something, and this is the longest a useful reason has needed to be.
pub const DIAGNOSTIC_ROWS: usize = 3;

/// The most rows the remedy may take: the command, and the config it needs.
pub const REMEDY_ROWS: usize = 2;

/// The fewest rows the body may have before it is dropped instead of squeezed.
///
/// Three is a column header and two levels: below that the table stops being a
/// table, and a dashboard cut to two rows says less than nothing.
pub const MIN_BODY: usize = 3;

/// Which of the two bodies is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    /// The price table.
    #[default]
    Book,
    /// The feed, the queues, the consumers and the ticks.
    Status,
}

/// The state that is not data: what the user is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct View {
    /// Which body is on screen.
    pub panel: Panel,
    /// Whether the command reference is open below the prompt.
    pub help: bool,
}

/// The terminal's size, and the rows a block may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Rows the terminal has.
    pub rows: usize,
    /// Columns the terminal has.
    pub cols: usize,
}

impl Default for Budget {
    /// The size assumed when the terminal will not say: the traditional 80×24.
    fn default() -> Self {
        Budget { rows: 24, cols: 80 }
    }
}

/// One frame: what to draw, and where the cursor belongs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    /// The rows, top to bottom.
    pub lines: Vec<Line>,
    /// Where a person types, as a one-based `(row, column)`, when there is one.
    pub cursor: Option<(usize, usize)>,
    /// Whether the body was shortened, so a caller can say so.
    pub body_truncated: bool,
}

impl Frame {
    /// The frame as plain text, one row per line.
    pub fn text(&self) -> String {
        crate::line::to_text(&self.lines)
    }
}

/// How many rows the reference may take.
///
/// Its length is the vocabulary's, not a constant: a command added to
/// [`crate::command::COMMANDS`] lengthens the reference, and a window with no room
/// shows the head of it. Fourteen today.
pub fn reference_len() -> usize {
    crate::command::REFERENCE.len()
}

/// Compose one frame.
pub fn compose(snapshot: &Snapshot, view: &View, budget: Budget) -> Frame {
    let cols = budget.cols.max(20);
    let rows = budget.rows.max(4);
    let live = snapshot.live(STALL_AFTER_NS);

    let mut head: Vec<Line> = Vec::with_capacity(6);
    head.push(header(snapshot, live));
    head.extend(times(snapshot));

    // The prompt and the header are reserved before anything else competes for
    // rows: a window that cannot say what it is and cannot show what is being typed
    // has nothing left to be useful for.
    // The status line is words about what just happened, so it is wrapped and
    // reserved like the diagnostic rather than truncated: a refusal cut in the
    // middle of its reason is the least useful thing a frame can show.
    let status: Vec<Line> = if snapshot.status.is_empty() {
        Vec::new()
    } else {
        wrap(&snapshot.status, cols, Style::Plain)
            .into_iter()
            .take(STATUS_ROWS)
            .collect()
    };
    let prompt_rows = usize::from(snapshot.input_mode != InputMode::Remote);
    let reserved = head.len() + prompt_rows + status.len();
    let mut remaining = rows.saturating_sub(reserved);

    // Each block below may take at most what is left after the ones under it have a
    // row: the body, which is the block that can give up rows without losing
    // meaning, is the one that gets them back.
    let mut diagnostic = if live {
        Vec::new()
    } else {
        diagnostic(snapshot, cols)
    };
    diagnostic.truncate(
        remaining
            .saturating_sub(1)
            .min(DIAGNOSTIC_ROWS + REMEDY_ROWS),
    );
    remaining = remaining.saturating_sub(diagnostic.len());

    let mut picker = picker_lines(snapshot, remaining, cols);
    picker.truncate(remaining.saturating_sub(1));
    remaining = remaining.saturating_sub(picker.len());

    let mut help: Vec<Line> = if view.help {
        crate::command::help_lines()
            .into_iter()
            .take(remaining.saturating_sub(1))
            .collect()
    } else {
        Vec::new()
    };
    help.truncate(remaining.saturating_sub(1));
    remaining = remaining.saturating_sub(help.len());

    let (body, truncated) = match view.panel {
        Panel::Book => table(snapshot, remaining, cols),
        Panel::Status => dashboard(snapshot, remaining, cols),
    };

    let mut lines = head;
    lines.extend(diagnostic);
    lines.extend(body);
    lines.extend(status);
    lines.extend(picker);
    // Where the prompt lands once everything above it is in place. Counted rather
    // than predicted: the body's height depends on the level count and the window,
    // and a cursor row that does not match the prompt's is a cursor in the wrong
    // place.
    // A remote frontend has its own input, so this frame has no prompt row: the page's
    // bar is the prompt, and a second one in the text would be a lie about where the
    // typing goes.
    let prompt_row = lines.len() + 1;
    if snapshot.input_mode != InputMode::Remote {
        lines.push(prompt(snapshot));
    }
    lines.extend(help);

    // The rule this whole function is arranged around, applied once at the end so
    // no block can break it by being added later: never more rows than the terminal
    // has, and never a row wider than it. A row that wraps takes a row the budget
    // did not give it, which is the same failure as writing one row too many.
    lines.truncate(rows);
    for line in lines.iter_mut() {
        if line.width() > cols {
            *line = line.truncated(cols);
        }
    }

    // Only when the prompt survived the truncation: a cursor row that does not
    // exist would place the terminal's cursor somewhere unrelated.
    let cursor = if snapshot.input_mode == InputMode::Local && prompt_row <= lines.len() {
        let typed = snapshot.input.chars().count();
        // Two for `> `, one because ANSI columns are one-based.
        Some((prompt_row, 3 + typed.min(cols.saturating_sub(3))))
    } else {
        None
    };

    Frame {
        lines,
        cursor,
        body_truncated: truncated,
    }
}

fn header(snapshot: &Snapshot, live: bool) -> Line {
    let feed = if live {
        Span::new("live", Style::Good)
    } else {
        Span::new("stalled", Style::Warn)
    };
    Line::new()
        .with(Span::new("orderbook-lab (rust)", Style::Dim))
        .with(Span::plain("  env="))
        .with(Span::new(snapshot.env.clone(), Style::Plain))
        .with(Span::plain("  "))
        .with(Span::new(snapshot.symbol.clone(), Style::Bold))
        .with(Span::plain("  seq="))
        .with(Span::plain(snapshot.published.to_string()))
        .with(Span::plain("  feed="))
        .with(feed)
        .with(Span::plain("  watching "))
        .with(Span::plain(snapshot.watched.to_string()))
}

fn times(snapshot: &Snapshot) -> Vec<Line> {
    let backlog = match snapshot.backlog_ns {
        Some(ns) => ob_cli_duration(ns),
        None => "-".to_string(),
    };
    let staleness = match snapshot.staleness_ns {
        Some(ns) => ob_cli_duration(ns),
        None => "-".to_string(),
    };
    vec![
        Line::new()
            .with(Span::plain(format!(
                "ring={}  lag={} records",
                snapshot.ring_capacity, snapshot.lag
            )))
            .with(Span::new(format!(" ({backlog} of feed time)"), Style::Dim))
            .with(Span::plain("  staleness="))
            .with(Span::plain(staleness)),
        Line::new().with(Span::plain(format!(
            "consumed={} records  resyncs={}  reattaches={}  age={}",
            snapshot.consumed,
            snapshot.resyncs,
            snapshot.reattaches,
            ob_cli_duration(snapshot.age_ns)
        ))),
    ]
}

/// Why the feed is not live, from the adapter's own heartbeat.
///
/// Wrapped to the window rather than truncated, because this is the one block a
/// person acts on: a sentence cut in the middle of its cause is worse than a
/// sentence on two rows. It is at most four rows, which is what the budget reserves
/// for it, and a four-row diagnostic on a very short window is still the right thing
/// to spend the rows on.
fn diagnostic(snapshot: &Snapshot, cols: usize) -> Vec<Line> {
    let (adapter, quiet, restartable) = match &snapshot.adapter {
        Adapter::Live { age_s, note } => (
            format!("The adapter is alive ({note}, {age_s}s ago)"),
            ", so the exchange has gone quiet rather than this host",
            false,
        ),
        Adapter::Stale { pid, age_s } => (
            format!(
                "The adapter (pid {pid}) last wrote {age_s}s ago and has not since: \
                 it has exited, and this window cannot restart it"
            ),
            "",
            true,
        ),
        Adapter::Absent => (
            "No adapter heartbeat at all, so nothing is feeding this ring".to_string(),
            "",
            true,
        ),
    };
    let sentence = if !snapshot.fed {
        // A symbol selected a moment ago has no book yet, which is not a feed that
        // stopped: `age` is this window's own, so it starts at the attach or the
        // switch. Here the selection and the watch set are the suspects.
        format!(
            "Nothing for {} yet. {adapter}. If it never arrives, check that the \
             adapter is watching it: `watch` lists the set.",
            snapshot.symbol
        )
    } else {
        format!(
            "Feed stalled. {adapter}{quiet}. This window last applied a record {} ago.",
            ob_cli_duration(snapshot.age_ns)
        )
    };
    let style = Style::Warn;
    let mut out: Vec<Line> = wrap(&sentence, cols, style)
        .into_iter()
        .take(DIAGNOSTIC_ROWS)
        .collect();
    if restartable {
        // Two rows on purpose: the command stays readable, and the config's path
        // gets a row of its own, because a path is the part that does not fit.
        out.extend(wrap(
            "Start the feed again in another window: make rust-feed",
            cols,
            Style::Plain,
        ));
        out.extend(wrap(
            &format!("  CONFIG={}", snapshot.config_path),
            cols,
            Style::Dim,
        ));
        out.extend(wrap(
            "This window re-attaches by itself; `make stop` would close it.",
            cols,
            Style::Dim,
        ));
    }
    out.truncate(DIAGNOSTIC_ROWS + REMEDY_ROWS);
    out
}

/// Word-wrap text into rows of at most `cols` characters.
///
/// Words longer than a row are cut rather than allowed to overflow: a long path or
/// a symbol with no spaces in it would otherwise push the row past the terminal and
/// wrap it, which costs a row the budget did not give it.
fn wrap(text: &str, cols: usize, style: Style) -> Vec<Line> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if !current.is_empty() && current.chars().count() + 1 + word_len > cols {
            out.push(Line::new().with(Span::new(current.clone(), style)));
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        if word_len > cols {
            current.push_str(&truncate(word, cols));
        } else {
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(Line::new().with(Span::new(current, style)));
    }
    out
}

/// The price table, and whether it had to show fewer levels than asked for.
fn table(snapshot: &Snapshot, rows: usize, cols: usize) -> (Vec<Line>, bool) {
    if rows == 0 {
        return (Vec::new(), true);
    }
    if rows < MIN_BODY {
        return (
            vec![Line::new().with(Span::new(
                "not enough rows for the book: widen the window, or run `book` after `levels 3`",
                Style::Warn,
            ))],
            true,
        );
    }
    // One row for the column headers, one for the note when levels were cut.
    let asked = snapshot.levels;
    let room = rows - 1;
    let shown = asked.min(room);
    let truncated = shown < asked;
    let room = if truncated {
        room.saturating_sub(1)
    } else {
        room
    };
    let shown = asked.min(room.max(1));

    let mut out = Vec::with_capacity(rows);
    out.push(
        Line::new()
            .with(Span::new(
                format!("{:>12} {:>12}", "BID PX", "BID QTY"),
                Style::Bid,
            ))
            .with(Span::plain("    "))
            .with(Span::new(
                format!("{:>12} {:>12}", "ASK PX", "ASK QTY"),
                Style::Ask,
            )),
    );
    for i in 0..shown {
        let (bp, bq) = level(snapshot.bids.get(i));
        let (ap, aq) = level(snapshot.asks.get(i));
        out.push(
            Line::new()
                .with(Span::new(format!("{bp:>12} {bq:>12}"), Style::Bid))
                .with(Span::plain("    "))
                .with(Span::new(format!("{ap:>12} {aq:>12}"), Style::Ask)),
        );
    }
    if truncated {
        out.push(Line::new().with(Span::new(
            truncate(
                &format!(
                    "showing {shown} of {asked} levels: the window has {rows} rows for the book",
                ),
                cols,
            ),
            Style::Warn,
        )));
    }
    (out, truncated)
}

fn level(level: Option<&(ob::fixed::Price, ob::fixed::Qty)>) -> (String, String) {
    match level {
        Some((px, qty)) => (px.format(2), qty.to_string()),
        None => (String::new(), String::new()),
    }
}

/// The dashboard: the ring, this reader, the ticks, the daemon and the adapter.
///
/// Every number here is either this reader's own measurement or the daemon's
/// published one, and the two are labelled apart: `reader` is what this window saw,
/// `daemon` is what the process feeding the ring said about itself. They are not
/// the same number and confusing them is easy — a reader with no lag on a stalled
/// feed is the case this whole view exists to show.
fn dashboard(snapshot: &Snapshot, rows: usize, cols: usize) -> (Vec<Line>, bool) {
    let mut body: Vec<Line> = Vec::with_capacity(rows);
    let mut truncated = false;

    // The ring, as a fill bar. The bar's units are stated, because a bar with no
    // scale is decoration.
    let fill = snapshot.ring_fill();
    let bar_width = cols.saturating_sub(34).clamp(8, 28);
    let resident = snapshot.published.min(u64::from(snapshot.ring_capacity));
    body.push(
        Line::new()
            .with(Span::new("ring      ", Style::Dim))
            .with(Span::new(bars::bar(fill, bar_width), Style::Accent))
            .with(Span::new(
                format!(
                    "  {} slots, {resident} resident, {:.0}% full",
                    snapshot.ring_capacity,
                    fill * 100.0
                ),
                Style::Plain,
            )),
    );
    body.push(
        Line::new()
            .with(Span::new("reader    ", Style::Dim))
            .with(Span::plain(format!(
                "lag {} records ({})  consumed {}  resyncs {}  reattaches {}",
                snapshot.lag,
                match snapshot.backlog_ns {
                    Some(ns) => ob_cli_duration(ns),
                    None => "-".to_string(),
                },
                snapshot.consumed,
                snapshot.resyncs,
                snapshot.reattaches
            ))),
    );
    body.push(
        Line::new()
            .with(Span::new("rate      ", Style::Dim))
            .with(Span::plain(format!(
                "{}/s  age {}  staleness {}",
                rate(snapshot.records_per_second),
                ob_cli_duration(snapshot.age_ns),
                match snapshot.staleness_ns {
                    Some(ns) => ob_cli_duration(ns),
                    None => "-".to_string(),
                }
            ))),
    );

    // The ticks, one row each, newest rate and a sparkline of the last seconds.
    if snapshot.ticks.is_empty() {
        body.push(
            Line::new()
                .with(Span::new("ticks     ", Style::Dim))
                .with(Span::new(
                    "none yet: no record has been counted",
                    Style::Warn,
                )),
        );
    } else {
        let max = snapshot
            .ticks
            .iter()
            .map(|t| t.per_second)
            .fold(0.0_f64, f64::max);
        let mut sorted: Vec<&crate::snapshot::Tick> = snapshot.ticks.iter().collect();
        // Busiest first: a status view is read to find what is moving and what is
        // not, and the watch set is longer than the panel can show.
        sorted.sort_by(|a, b| b.per_second.total_cmp(&a.per_second));
        for (i, tick) in sorted.iter().enumerate() {
            let label = if i == 0 { "ticks     " } else { "          " };
            let spark_width = cols.saturating_sub(34).clamp(8, 24);
            let spark = bars::sparkline(&tick.history, spark_width);
            let share = if max > 0.0 {
                tick.per_second / max
            } else {
                0.0
            };
            body.push(
                Line::new()
                    .with(Span::new(label, Style::Dim))
                    .with(Span::new(format!("{:<9} ", tick.symbol), Style::Bold))
                    .with(Span::new(spark, Style::Ask))
                    .with(Span::plain(format!(
                        " {:>9}/s  {:.0}% of the busiest",
                        rate(tick.per_second),
                        share * 100.0
                    ))),
            );
        }
    }

    // The adapter, as its heartbeat describes it.
    let (adapter_text, style) = match &snapshot.adapter {
        Adapter::Live { age_s, note } => (format!("{note} ({age_s}s ago)"), Style::Good),
        Adapter::Stale { pid, age_s } => (
            format!("exited: pid {pid} last wrote {age_s}s ago"),
            Style::Bad,
        ),
        Adapter::Absent => (
            "no heartbeat: nothing is feeding this ring".to_string(),
            Style::Bad,
        ),
    };
    body.push(
        Line::new()
            .with(Span::new("adapter   ", Style::Dim))
            .with(Span::new(adapter_text, style)),
    );

    // The daemon's own account, or an honest absence.
    match &snapshot.daemon {
        None => body.push(
            Line::new()
                .with(Span::new("daemon    ", Style::Dim))
                .with(Span::new(
                    truncate(
                        &format!(
                            "no report in {} yet: the daemon writes one a second",
                            snapshot.daemon_log
                        ),
                        cols,
                    ),
                    Style::Warn,
                )),
        ),
        Some(report) => {
            body.push(
                Line::new()
                    .with(Span::new("daemon    ", Style::Dim))
                    .with(Span::plain(format!(
                        "rate={} published={} applied={} symbols={} connections={} invalid={}",
                        field(report.rate),
                        field(report.published),
                        field(report.applied),
                        field(report.symbols),
                        field(report.connections),
                        field(report.invalid),
                    ))),
            );
            let lost = report.lost();
            body.push(
                Line::new()
                    .with(Span::new("loss      ", Style::Dim))
                    .with(Span::new(
                        format!(
                            "dropped={} evicted={} crossed={} ignored={}",
                            field(report.dropped),
                            field(report.evicted),
                            field(report.crossed),
                            field(report.ignored)
                        ),
                        if lost > 0 { Style::Bad } else { Style::Good },
                    ))
                    .with(Span::new(
                        if lost > 0 {
                            "  ← a consumer is losing records"
                        } else {
                            ""
                        },
                        Style::Bad,
                    )),
            );
            for subscriber in &report.subscribers {
                let state = subscriber.state.clone().unwrap_or_else(|| "-".to_string());
                let style = match state.as_str() {
                    "nominal" => Style::Good,
                    "elevated" => Style::Warn,
                    "saturated" => Style::Bad,
                    _ => Style::Plain,
                };
                body.push(
                    Line::new()
                        .with(Span::new(
                            format!("consumer{} ", subscriber.index),
                            Style::Dim,
                        ))
                        .with(Span::plain(format!(
                            "delivered={} dropped={} evicted={} high_water={} state=",
                            field(subscriber.delivered),
                            field(subscriber.dropped),
                            field(subscriber.evicted),
                            field(subscriber.high_water),
                        )))
                        .with(Span::new(state, style))
                        .with(Span::new(
                            match (&subscriber.stage3_p50, &subscriber.stage3_p99) {
                                (Some(p50), Some(p99)) => {
                                    format!("  stage3 p50={p50} p99={p99}")
                                }
                                _ => String::new(),
                            },
                            Style::Dim,
                        )),
                );
            }
            body.push(
                Line::new()
                    .with(Span::new("stage2    ", Style::Dim))
                    .with(Span::plain(format!(
                        "p50={} p99={}  stamped={} unstamped={} implausible={}",
                        report.stage2_p50.clone().unwrap_or_else(|| "-".to_string()),
                        report.stage2_p99.clone().unwrap_or_else(|| "-".to_string()),
                        field(report.stamped),
                        field(report.unstamped),
                        field(report.implausible),
                    ))),
            );
        }
    }

    if body.len() > rows {
        truncated = true;
        body.truncate(rows);
        if let Some(last) = body.last_mut() {
            *last = Line::new().with(Span::new(
                "…more: the window is short, widen it or close the help panel",
                Style::Warn,
            ));
        }
    }
    (body, truncated)
}

/// An absent counter renders as `-`, never as zero: the two are different claims.
fn field(value: Option<u64>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}

fn picker_lines(snapshot: &Snapshot, remaining: usize, cols: usize) -> Vec<Line> {
    if snapshot.input_mode != InputMode::Local
        || snapshot.suggestions.is_empty()
        || snapshot.input.trim().is_empty()
    {
        return Vec::new();
    }
    let room = remaining.saturating_sub(MIN_BODY);
    let shown = snapshot.suggestions.len().min(8).min(room);
    let selected = snapshot.highlighted.min(shown.saturating_sub(1));
    let mut out = Vec::with_capacity(shown);
    for (i, suggestion) in snapshot.suggestions.iter().take(shown).enumerate() {
        let chosen = i == selected;
        let label = format!("  {}{}", if chosen { "> " } else { "  " }, suggestion.label);
        // The detail is what the entry means, so a list of symbols and a list of
        // commands are told apart without a second column of type names.
        let detail = format!("  {}", suggestion.detail);
        let mut line = Line::new()
            .with(Span::new(
                label,
                if chosen { Style::Bold } else { Style::Dim },
            ))
            .with(Span::new(detail, Style::Dim));
        if line.width() > cols {
            line = line.truncated(cols);
        }
        out.push(line);
    }
    out
}

fn prompt(snapshot: &Snapshot) -> Line {
    match snapshot.input_mode {
        InputMode::Local => Line::new()
            .with(Span::new("> ", Style::Accent))
            .with(Span::plain(snapshot.input.clone())),
        // Unreachable through `compose`, which does not draw a prompt for a remote
        // frontend; written out so the match is total and a future caller cannot
        // silently get the wrong prompt.
        InputMode::Remote => Line::new(),
        InputMode::None => Line::new()
            .with(Span::new("> ", Style::Accent))
            .with(Span::new(
                "(input disabled: stdin is not a terminal)",
                Style::Dim,
            )),
    }
}

/// A rate, in the units a rate is read in.
fn rate(per_second: f64) -> String {
    if per_second >= 1_000_000.0 {
        format!("{:.2}M", per_second / 1_000_000.0)
    } else if per_second >= 1_000.0 {
        format!("{:.2}k", per_second / 1_000.0)
    } else {
        format!("{per_second:.0}")
    }
}

/// Truncate to a column budget, keeping the text.
fn truncate(text: &str, cols: usize) -> String {
    if text.chars().count() <= cols {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cols.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Durations are formatted by the crate that owns the clock's units.
///
/// A thin alias so the composition has one call site to read: the formatter moved
/// into `ob::clock` when a second frontend needed it, and this keeps that move from
/// being visible in twenty format strings.
fn ob_cli_duration(ns: u64) -> String {
    ob::clock::duration_ns(ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Tick;
    use ob::fixed::{Price, Qty};

    fn snapshot() -> Snapshot {
        Snapshot {
            fed: true,
            age_ns: 1_000_000,
            levels: 2,
            bids: vec![
                (Price::from_e8(8_170_602_000_000), Qty::from_e8(577_385_000)),
                (Price::from_e8(8_170_593_000_000), Qty::from_e8(139_990)),
            ],
            asks: vec![
                (Price::from_e8(8_170_603_000_000), Qty::from_e8(43_366_000)),
                (Price::from_e8(8_170_604_000_000), Qty::from_e8(35_000)),
            ],
            ..Snapshot::default()
        }
    }

    fn text_of(frame: &Frame) -> Vec<String> {
        frame.lines.iter().map(Line::text).collect()
    }

    #[test]
    fn a_live_frame_says_so_and_draws_the_table() {
        let frame = compose(&snapshot(), &View::default(), Budget::default());
        let text = text_of(&frame);
        assert!(text[0].contains("env=dev"), "{:?}", text[0]);
        assert!(text[0].contains("feed=live"));
        assert!(text[0].contains("watching 1"));
        assert!(text[1].contains("ring=4096"));
        assert!(text[2].contains("consumed=0 records"));
        assert!(
            text.iter().any(|l| l.contains("81706.02")),
            "the book's price is on screen: {text:?}"
        );
        assert!(
            !text.iter().any(|l| l.contains("Feed stalled")),
            "a live feed has no diagnostic"
        );
    }

    #[test]
    fn the_frame_never_exceeds_the_rows_it_was_given() {
        for rows in [4, 5, 8, 12, 24, 60] {
            for help in [false, true] {
                let mut s = snapshot();
                s.suggestions = crate::command::complete(
                    "et",
                    &["ETHUSDT".to_string(), "ETCUSDT".to_string()],
                    8,
                );
                s.input = "ET".into();
                s.levels = 40;
                let view = View {
                    panel: Panel::Book,
                    help,
                };
                let frame = compose(&s, &view, Budget { rows, cols: 80 });
                assert!(
                    frame.lines.len() <= rows,
                    "rows={rows} help={help} produced {} lines",
                    frame.lines.len()
                );
            }
        }
    }

    #[test]
    fn the_command_reference_is_below_the_prompt_and_off_by_default() {
        let s = snapshot();
        let closed = compose(&s, &View::default(), Budget::default());
        assert!(
            !closed
                .lines
                .iter()
                .any(|l| l.text().contains("leave this window")),
            "the reference is not in a default frame"
        );

        let open = compose(
            &s,
            &View {
                panel: Panel::Book,
                help: true,
            },
            Budget::default(),
        );
        let text = text_of(&open);
        let prompt_at = text
            .iter()
            .position(|l| l.starts_with("> "))
            .expect("a prompt");
        let help_at = text
            .iter()
            .position(|l| l.contains("leave this window"))
            .expect("the reference");
        assert!(
            help_at > prompt_at,
            "the reference is below the prompt: prompt {prompt_at}, reference {help_at}"
        );
        assert!(text[help_at].contains("quit | q"), "{}", text[help_at]);
    }

    #[test]
    fn the_cursor_sits_just_past_what_is_typed() {
        let mut s = snapshot();
        s.input = "levels 7".to_string();
        let frame = compose(&s, &View::default(), Budget::default());
        let prompt_at = frame
            .lines
            .iter()
            .position(|l| l.text().starts_with("> "))
            .expect("a prompt");
        assert_eq!(frame.cursor, Some((prompt_at + 1, 3 + 8)));
    }

    #[test]
    fn a_stalled_feed_says_which_of_the_three_causes_it_is() {
        let mut s = snapshot();
        s.fed = true;
        s.age_ns = 40_000_000_000;
        s.adapter = Adapter::Stale {
            pid: 4718,
            age_s: 22,
        };
        let frame = compose(&s, &View::default(), Budget::default());
        // The diagnostic wraps to the window, so the cause is read across its
        // rows rather than looked for in one of them.
        let text: Vec<String> = frame.lines.iter().map(Line::text).collect();
        let all = text.join("\n");
        assert!(all.contains("Feed stalled."), "{all}");
        assert!(all.contains("pid 4718"), "{all}");
        assert!(all.contains("has exited"), "{all}");
        assert!(all.contains("make rust-feed"), "{all}");
        assert!(all.contains("config/dev.json"), "{all}");
        for line in &text {
            assert!(
                line.len() <= 80,
                "a wrapped row is at most 80 wide: {line:?}"
            );
        }
    }

    #[test]
    fn a_symbol_that_has_never_arrived_is_not_reported_as_a_dead_feed() {
        let mut s = snapshot();
        s.fed = false;
        s.adapter = Adapter::Live {
            age_s: 0,
            note: "live; watching 2".into(),
        };
        let frame = compose(&s, &View::default(), Budget::default());
        let text: Vec<String> = frame.lines.iter().map(Line::text).collect();
        assert!(text.iter().any(|l| l.contains("Nothing for BTCUSDT yet")));
        assert!(!text.iter().any(|l| l.contains("Feed stalled")));
        assert!(
            text.iter().any(|l| l.contains("live; watching 2")),
            "the adapter is named"
        );
    }

    #[test]
    fn the_status_panel_shows_the_ring_the_reader_the_ticks_and_the_daemon() {
        let mut s = snapshot();
        s.published = 2048;
        s.consumed = 2048;
        s.records_per_second = 751_200.0;
        s.ticks = vec![
            Tick {
                symbol: "BTCUSDT".into(),
                per_second: 12_400.0,
                history: vec![1.0, 2.0, 3.0, 4.0],
            },
            Tick {
                symbol: "ETHUSDT".into(),
                per_second: 3_100.0,
                history: vec![1.0, 1.0, 1.0, 1.0],
            },
        ];
        s.adapter = Adapter::Live {
            age_s: 0,
            note: "live; watching 2".into(),
        };
        s.daemon = crate::report::parse_report(
            "t=1 op=report published=2048 applied=2048 crossed=0 evicted=1 dropped=2 symbols=2 connections=1 invalid=0 rate=751\n\
             subscriber 0: delivered=2048 evicted=1 dropped=2 high_water=64 state=saturated | stage3: n=1 p50=165.5us p99=205.4us\n",
        );
        let frame = compose(
            &s,
            &View {
                panel: Panel::Status,
                help: false,
            },
            Budget {
                rows: 30,
                cols: 120,
            },
        );
        let text: Vec<String> = frame.lines.iter().map(Line::text).collect();
        let joined = text.join("\n");
        assert!(joined.contains("ring      "), "{joined}");
        assert!(
            joined.contains("4096 slots, 2048 resident, 50% full"),
            "{joined}"
        );
        assert!(joined.contains("751.20k/s"), "{joined}");
        assert!(joined.contains("BTCUSDT"), "{joined}");
        assert!(joined.contains("12.40k/s"), "{joined}");
        assert!(joined.contains("rate=751"), "{joined}");
        assert!(joined.contains("dropped=2"), "{joined}");
        assert!(joined.contains("saturated"), "{joined}");
        assert!(joined.contains("stage3 p50=165.5us"), "{joined}");
        assert!(joined.contains("live; watching 2"), "{joined}");
        assert!(
            !joined.contains("81706.02"),
            "the table is not drawn in the status panel"
        );
    }

    #[test]
    fn a_daemon_with_no_report_says_so_rather_than_showing_zeros() {
        let s = Snapshot::default();
        let frame = compose(
            &s,
            &View {
                panel: Panel::Status,
                help: false,
            },
            Budget::default(),
        );
        let joined = frame
            .lines
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("no report"), "{joined}");
        assert!(
            !joined.contains("rate=0"),
            "an absent report is not a zero rate"
        );
        assert!(joined.contains("none yet"), "and no ticks says so");
    }

    #[test]
    fn a_short_window_shortens_the_book_and_says_how_many_levels_it_cut() {
        let mut s = snapshot();
        s.levels = 30;
        let frame = compose(&s, &View::default(), Budget { rows: 12, cols: 80 });
        let joined = frame
            .lines
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(frame.body_truncated);
        assert!(joined.contains("showing"), "{joined}");
        assert!(frame.lines.len() <= 12);
    }

    #[test]
    fn a_body_with_no_room_says_so_instead_of_drawing_half_a_table() {
        let mut s = snapshot();
        s.fed = false;
        s.adapter = Adapter::Stale { pid: 1, age_s: 9 };
        let frame = compose(&s, &View::default(), Budget { rows: 7, cols: 40 });
        let joined = frame
            .lines
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("not enough rows") || joined.contains("showing"),
            "{joined}"
        );
        assert!(frame.lines.len() <= 7);
    }

    #[test]
    fn long_lines_are_truncated_to_the_window_rather_than_wrapped() {
        // A wrapped row would take a row the budget did not give it, and the frame
        // would then be longer than the terminal.
        let mut s = snapshot();
        s.status = "x".repeat(500);
        s.config_path = "c".repeat(200);
        s.adapter = Adapter::Stale { pid: 1, age_s: 9 };
        let frame = compose(&s, &View::default(), Budget { rows: 24, cols: 40 });
        for line in &frame.lines {
            assert!(
                line.width() <= 40,
                "a row is {} wide: {:?}",
                line.width(),
                line.text()
            );
        }
    }

    #[test]
    fn the_picker_never_takes_the_body_s_room() {
        let mut s = snapshot();
        s.input = "e".into();
        s.suggestions = (0..8)
            .map(|i| crate::command::Suggestion {
                label: format!("E{i}USDT"),
                insert: format!("E{i}USDT"),
                kind: crate::command::Kind::Symbol,
                detail: "switch to it and watch it".to_string(),
            })
            .collect();
        let frame = compose(&s, &View::default(), Budget { rows: 10, cols: 60 });
        let joined = frame
            .lines
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(frame.lines.len() <= 10);
        assert!(joined.contains("BID PX"), "the table survived the picker");
        assert!(joined.contains("> e"), "and the prompt did");
    }
}
