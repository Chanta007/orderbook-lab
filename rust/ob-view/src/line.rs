//! Lines, spans and styles, and the four ways to render them.
//!
//! A frame is a `Vec<Line>`; a line is one or more [`Span`]s; a span is text with
//! one [`Style`]. That is the whole model, and it is deliberately small: it is
//! enough for a terminal (ANSI), a web page (DOM nodes), a CLI (`--json`) and a
//! file (`--text`), and it costs one small allocation per line per frame.
//!
//! # Styles are semantic
//!
//! [`Style::Bid`] means "this is a bid price". It does not mean red, and this
//! crate never says red: the ANSI renderer maps it to the terminal's red, the HTML
//! renderer maps it to a class, and a JSON consumer reads `"bid"`. The composition
//! therefore has no opinion about where it is drawn, which is what lets one frame
//! be a terminal and a page at the same time.
//!
//! # The terminal renderer writes only what changed
//!
//! A frame is a few kilobytes and mostly static: prices move, the header does not.
//! [`FrameCache::render`] hashes each rendered line, skips the ones whose hash it
//! has seen, and moves the cursor to the ones it has not. That is the documented
//! win — ratatui's own guidance is that the terminal write, not the composition,
//! is the bottleneck (`docs/research/2026-10-19-view-layer-and-web-frontend.md`
//! §2) — and it counts what it skipped so the claim can be measured.

use std::fmt::Write as _;

/// What a span is, in the words of the thing it shows.
///
/// Semantic rather than visual on purpose; see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    /// Ordinary text.
    Plain,
    /// Present but secondary: a unit, a label, a note.
    Dim,
    /// A heading or the selected item.
    Bold,
    /// A bid price.
    Bid,
    /// An ask price.
    Ask,
    /// A state that is healthy: live, nominal, clean.
    Good,
    /// A state that needs attention but is working: elevated, stalled, truncated.
    Warn,
    /// A state that is wrong: saturated, dropped, exited.
    Bad,
    /// A structure, not a value: a rule, a key in a legend.
    Accent,
}

impl Style {
    /// The ANSI sequence that starts this style.
    ///
    /// The palette is the C++ terminal's, so a window of either implementation
    /// looks the same: red bids, green asks, and the standard bright colours for
    /// state.
    pub fn ansi(self) -> &'static str {
        match self {
            Style::Plain => "\x1b[0m",
            Style::Dim => "\x1b[2m",
            Style::Bold => "\x1b[1m",
            Style::Bid => "\x1b[31m",
            Style::Ask => "\x1b[32m",
            Style::Good => "\x1b[32m",
            Style::Warn => "\x1b[33m",
            Style::Bad => "\x1b[31m",
            Style::Accent => "\x1b[36m",
        }
    }

    /// The name this style carries on the wire and in the JSON form.
    ///
    /// A name rather than a colour, because the consumer decides the colour: a
    /// page can honour a dark theme, and a test can assert that a price is a price
    /// without knowing the palette.
    pub fn name(self) -> &'static str {
        match self {
            Style::Plain => "plain",
            Style::Dim => "dim",
            Style::Bold => "bold",
            Style::Bid => "bid",
            Style::Ask => "ask",
            Style::Good => "good",
            Style::Warn => "warn",
            Style::Bad => "bad",
            Style::Accent => "accent",
        }
    }
}

/// Text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// What it says.
    pub text: String,
    /// How it is meant.
    pub style: Style,
}

impl Span {
    /// A span in the given style.
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Span {
            text: text.into(),
            style,
        }
    }

    /// A span of ordinary text.
    pub fn plain(text: impl Into<String>) -> Self {
        Span::new(text, Style::Plain)
    }
}

/// One row of a frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    /// The pieces of this row, left to right.
    pub spans: Vec<Span>,
}

impl Line {
    /// An empty row.
    pub fn new() -> Self {
        Line { spans: Vec::new() }
    }

    /// A row of ordinary text.
    pub fn plain(text: impl Into<String>) -> Self {
        Line {
            spans: vec![Span::plain(text)],
        }
    }

    /// Add a span and return the line, so a row can be built in one expression.
    #[must_use]
    pub fn with(mut self, span: Span) -> Self {
        self.spans.push(span);
        self
    }

    /// Add a span in place.
    pub fn push(&mut self, span: Span) {
        self.spans.push(span);
    }

    /// The row's text, without any styling.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.width());
        for span in &self.spans {
            out.push_str(&span.text);
        }
        out
    }

    /// Whether the row draws nothing.
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }

    /// The row's width in characters.
    ///
    /// Characters rather than bytes or display columns: everything this interface
    /// draws is ASCII apart from the block characters in [`crate::bars`], which are
    /// one column wide. A comment rather than a grapheme walker, because the one
    /// place a column matters is the prompt, whose text is the user's line.
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }

    /// Cut the row to `cols` characters, keeping the styles of what remains.
    ///
    /// Used by the budget when a panel's row is longer than the terminal. A line
    /// that cannot be cut without losing meaning is the composer's problem, not
    /// this one: it truncates text and this keeps the last span's style.
    #[must_use]
    pub fn truncated(&self, cols: usize) -> Line {
        let mut out = Line::new();
        let mut left = cols;
        for span in &self.spans {
            if left == 0 {
                break;
            }
            let chars: Vec<char> = span.text.chars().collect();
            let take = chars.len().min(left);
            out.spans.push(Span::new(
                chars[..take].iter().collect::<String>(),
                span.style,
            ));
            left -= take;
        }
        out
    }
}

/// What a render did, so a claim about the write path can be measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Rows written this frame.
    pub written: usize,
    /// Rows the terminal already had, so they were not written again.
    pub skipped: usize,
    /// Rows erased because the previous frame was longer.
    pub erased: usize,
    /// Bytes handed to the write.
    pub bytes: usize,
    /// The screen was cleared first, because the terminal was resized or this is
    /// the first frame.
    pub cleared: bool,
}

/// The previous frame, so the next one can write only what changed.
///
/// Keyed by a hash of each rendered row, which includes the styles: a price that
/// moved from red to green is a different row and is written again.
#[derive(Debug, Default)]
pub struct FrameCache {
    hashes: Vec<u64>,
    rows: usize,
    cols: usize,
}

impl FrameCache {
    /// A cache with nothing in it. The first render writes every row.
    pub fn new() -> Self {
        FrameCache::default()
    }

    /// How many rows the last frame had.
    pub fn rows(&self) -> usize {
        self.hashes.len()
    }

    /// Render a frame for a terminal of `rows` by `cols`, placing the cursor.
    ///
    /// `cursor` is a one-based `(row, column)`, which is where a person types. A
    /// frame is never longer than `rows`: the composer is told the size and keeps
    /// inside it, because one row too many scrolls the screen and every later
    /// frame's row arithmetic is then wrong.
    pub fn render(
        &mut self,
        frame: &[Line],
        rows: usize,
        cols: usize,
        cursor: Option<(usize, usize)>,
    ) -> (String, RenderStats) {
        let mut out = String::with_capacity(frame.len() * 64 + 32);
        let mut stats = RenderStats::default();
        let resized = rows != self.rows || cols != self.cols;
        if resized {
            // A resize moves every row, and the old screen is the wrong shape to
            // patch. Clear and write the whole frame.
            out.push_str("\x1b[2J");
            self.hashes.clear();
            self.rows = rows;
            self.cols = cols;
            stats.cleared = true;
        }

        let visible = frame.len().min(rows);
        for (row, line) in frame.iter().take(visible).enumerate() {
            let text = ansi_row(line);
            let hash = fnv1a(text.as_bytes());
            if self.hashes.get(row) == Some(&hash) {
                stats.skipped += 1;
                continue;
            }
            // Position, write, erase to the end of the row. Positioning rather
            // than assuming the cursor is where the previous row ended is what
            // makes a partial write possible at all.
            let _ = write!(out, "\x1b[{};1H{text}\x1b[K", row + 1);
            if self.hashes.len() <= row {
                self.hashes.push(hash);
            } else {
                self.hashes[row] = hash;
            }
            stats.written += 1;
        }
        // A frame that got shorter must not leave the old rows behind.
        for row in visible..self.hashes.len() {
            let _ = write!(out, "\x1b[{};1H\x1b[K", row + 1);
            stats.erased += 1;
        }
        self.hashes.truncate(visible);

        if let Some((row, col)) = cursor {
            let _ = write!(out, "\x1b[{row};{col}H");
        }
        stats.bytes = out.len();
        (out, stats)
    }
}

/// One row as a terminal draws it.
fn ansi_row(line: &Line) -> String {
    let mut out = String::with_capacity(line.width() + line.spans.len() * 6);
    for span in &line.spans {
        if span.text.is_empty() {
            continue;
        }
        out.push_str(span.style.ansi());
        out.push_str(&span.text);
    }
    out.push_str("\x1b[0m");
    out
}

/// The frame with no styling at all, one line each.
pub fn to_text(frame: &[Line]) -> String {
    let mut out = String::with_capacity(frame.len() * 48);
    for line in frame {
        out.push_str(&line.text());
        out.push('\n');
    }
    out
}

/// The frame as JSON: an array of rows, each an array of spans.
///
/// Hand-written rather than derived, because this crate has no dependencies and a
/// frame's shape is four fields. Escaping follows RFC 8259 for the characters that
/// occur in this lab's text: quote, backslash and the control characters.
pub fn to_json(frame: &[Line]) -> String {
    let mut out = String::with_capacity(frame.len() * 64);
    out.push_str("{\"lines\":[");
    for (i, line) in frame.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"spans\":[");
        for (j, span) in line.spans.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str("{\"text\":\"");
            escape_json(&span.text, &mut out);
            out.push_str("\",\"style\":\"");
            out.push_str(span.style.name());
            out.push_str("\"}");
        }
        out.push_str("]}");
    }
    out.push_str("]}");
    out
}

/// The frame as HTML: rows as `<div>`s of styled `<span>`s.
///
/// The page's first paint, and the same markup the stream updates. The stylesheet
/// that gives the classes their colours lives with the page, not here: this crate
/// has no opinion about a palette.
pub fn to_html(frame: &[Line]) -> String {
    let mut out = String::with_capacity(frame.len() * 80);
    for line in frame {
        out.push_str("<div class=\"row\">");
        for span in &line.spans {
            if span.text.is_empty() {
                continue;
            }
            out.push_str("<span class=\"");
            out.push_str(span.style.name());
            out.push_str("\">");
            escape_html(&span.text, &mut out);
            out.push_str("</span>");
        }
        // A non-breaking space keeps an empty row's height, so the page does not
        // jump as panels open and close.
        if line.is_empty() {
            out.push_str("&nbsp;");
        }
        out.push_str("</div>");
    }
    out.push('\n');
    out
}

/// FNV-1a, 64-bit: the same cheap hash the symbol intern table uses.
///
/// Not for a hash map that faces an adversary — this compares a row with the row it
/// was a moment ago.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn escape_json(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

fn escape_html(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Line> {
        vec![
            Line::new()
                .with(Span::new("orderbook-lab", Style::Bold))
                .with(Span::plain("  env=dev")),
            Line::new()
                .with(Span::new("81706.02", Style::Bid))
                .with(Span::plain("  "))
                .with(Span::new("81706.03", Style::Ask)),
        ]
    }

    #[test]
    fn a_line_is_its_spans_in_order() {
        let line = Line::new()
            .with(Span::new("a", Style::Bold))
            .with(Span::plain("b"));
        assert_eq!(line.text(), "ab");
        assert_eq!(line.width(), 2);
        assert!(!line.is_empty());
    }

    #[test]
    fn a_style_carries_a_name_and_a_colour() {
        assert_eq!(Style::Bid.ansi(), "\x1b[31m");
        assert_eq!(Style::Ask.ansi(), "\x1b[32m");
        assert_eq!(Style::Bid.name(), "bid");
        assert_eq!(Style::Plain.name(), "plain");
        // Every style is named and coloured, so a new one cannot be added and
        // silently render as nothing.
        for style in [
            Style::Plain,
            Style::Dim,
            Style::Bold,
            Style::Bid,
            Style::Ask,
            Style::Good,
            Style::Warn,
            Style::Bad,
            Style::Accent,
        ] {
            assert!(!style.name().is_empty());
            assert!(style.ansi().starts_with("\x1b["));
        }
    }

    #[test]
    fn json_escapes_what_it_must_and_keeps_the_styles() {
        let frame = vec![Line::new().with(Span::new("a\"b\\c", Style::Bid))];
        let json = to_json(&frame);
        assert_eq!(
            json,
            "{\"lines\":[{\"spans\":[{\"text\":\"a\\\"b\\\\c\",\"style\":\"bid\"}]}]}"
        );
    }

    #[test]
    fn a_control_character_does_not_break_the_json() {
        let frame = vec![Line::plain("a\u{1}b")];
        assert!(to_json(&frame).contains("\\u0001"));
    }

    #[test]
    fn html_escapes_markup_so_a_symbol_cannot_inject_a_tag() {
        // A symbol arrives from a config or a websocket. It must not be able to
        // close a span and write its own markup.
        let frame = vec![Line::plain("<script>x & y</script>")];
        let html = to_html(&frame);
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn an_empty_row_keeps_its_height_in_html() {
        let html = to_html(&[Line::new()]);
        assert!(html.contains("&nbsp;"));
    }

    #[test]
    fn text_rendering_has_no_escapes() {
        let text = to_text(&sample());
        assert!(text.contains("orderbook-lab  env=dev"));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn the_first_frame_writes_every_row_and_the_next_one_writes_nothing() {
        let frame = sample();
        let mut cache = FrameCache::new();
        let (out, stats) = cache.render(&frame, 24, 80, Some((3, 3)));
        assert_eq!(stats.written, 2);
        assert_eq!(stats.skipped, 0);
        assert!(stats.cleared, "the first frame clears");
        assert!(out.contains("\x1b[1;1H"), "each row is positioned");
        assert!(out.ends_with("\x1b[3;3H"), "the cursor is placed last");

        let (out, stats) = cache.render(&frame, 24, 80, Some((3, 3)));
        assert_eq!(stats.written, 0);
        assert_eq!(stats.skipped, 2);
        assert_eq!(out, "\x1b[3;3H", "an unchanged frame is the cursor move");
    }

    #[test]
    fn only_the_row_that_changed_is_written_again() {
        let mut frame = sample();
        let mut cache = FrameCache::new();
        cache.render(&frame, 24, 80, None);
        frame[1] = Line::new().with(Span::new("81707.00", Style::Bid));
        let (out, stats) = cache.render(&frame, 24, 80, None);
        assert_eq!(stats.written, 1);
        assert_eq!(stats.skipped, 1);
        assert!(out.contains("\x1b[2;1H"), "the second row was written");
        assert!(!out.contains("\x1b[1;1H"), "the first was not");
    }

    #[test]
    fn a_change_of_style_alone_is_a_change_of_row() {
        // The hash covers the escapes, so a price that turned from a bid into an
        // ask is written again even though its text is the same.
        let mut frame = vec![Line::new().with(Span::new("100", Style::Bid))];
        let mut cache = FrameCache::new();
        cache.render(&frame, 24, 80, None);
        frame[0] = Line::new().with(Span::new("100", Style::Ask));
        let (_, stats) = cache.render(&frame, 24, 80, None);
        assert_eq!(stats.written, 1);
    }

    #[test]
    fn a_shorter_frame_erases_the_rows_it_no_longer_has() {
        let mut frame = sample();
        let mut cache = FrameCache::new();
        cache.render(&frame, 24, 80, None);
        frame.truncate(1);
        let (out, stats) = cache.render(&frame, 24, 80, None);
        assert_eq!(stats.erased, 1);
        assert!(out.contains("\x1b[2;1H\x1b[K"));
        assert_eq!(cache.rows(), 1);
    }

    #[test]
    fn a_resize_clears_and_rewrites_everything() {
        let frame = sample();
        let mut cache = FrameCache::new();
        cache.render(&frame, 24, 80, None);
        let (out, stats) = cache.render(&frame, 40, 100, None);
        assert!(stats.cleared);
        assert_eq!(stats.written, 2);
        assert!(out.starts_with("\x1b[2J"));
    }

    #[test]
    fn more_rows_than_the_terminal_has_are_never_written() {
        // The rule the whole budget exists for: writing one row past the bottom
        // scrolls, and a scrolled screen breaks every later frame.
        let frame: Vec<Line> = (0..40).map(|i| Line::plain(format!("row {i}"))).collect();
        let mut cache = FrameCache::new();
        let (_, stats) = cache.render(&frame, 24, 80, None);
        assert_eq!(stats.written, 24, "at most one row per terminal row");
    }

    #[test]
    fn truncating_a_line_keeps_the_styles_of_what_survives() {
        let line = Line::new()
            .with(Span::new("abc", Style::Bold))
            .with(Span::new("def", Style::Bid));
        let cut = line.truncated(4);
        assert_eq!(cut.text(), "abcd");
        assert_eq!(cut.spans[0].style, Style::Bold);
        assert_eq!(cut.spans[1].style, Style::Bid);
        assert_eq!(cut.spans[1].text, "d");
        assert_eq!(line.truncated(0).text(), "");
        assert_eq!(line.truncated(99).text(), "abcdef");
    }
}
