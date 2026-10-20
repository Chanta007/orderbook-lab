//! What one frame of the lab's interface is, as pure data.
//!
//! The terminal book used to be one function that read the ring, decided what a
//! frame said, and wrote ANSI escapes, in that order. Every new panel grew that
//! function, and nothing about it could be shown anywhere but a terminal. This
//! crate is the split:
//!
//! ```text
//!   ring ─┐
//!   files ├─→ Snapshot ──→ compose(snapshot, view) ──→ Vec<Line> ─┬─→ ANSI   (tui)
//!   input ┘                       (pure)                          ├─→ HTML   (web)
//!                                                                 ├─→ JSON   (view --json)
//!                                                                 └─→ text   (view --text)
//! ```
//!
//! Nothing here opens a file, a socket or a terminal, and nothing here knows what
//! a terminal is. The dependency graph enforces it: this crate depends on `ob` and
//! on nothing else, so a terminal type cannot appear in a composed line.
//!
//! # The pieces
//!
//! - [`line`] — [`Line`], [`Span`], [`Style`], and the four renderers. Styles are
//!   semantic (`Bid`, not `red`), so the terminal, a web page and a JSON consumer
//!   each decide what a bid price looks like.
//! - [`snapshot`] — everything observed for one frame, and nothing derived.
//! - [`compose`] — [`compose`], the pure function from a snapshot and a view to a
//!   frame, plus the terminal-size budget that keeps a frame inside the screen.
//! - [`report`] — the parser for the lines the daemon already writes about its own
//!   queues and consumers, so the status view can show them without changing the
//!   wire format that three implementations share.
//! - [`bars`] — the two ways a terminal draws a quantity: a horizontal bar and a
//!   sparkline, both from the Unicode block elements.
//!
//! # Why lines and not widgets
//!
//! A frame here is one column of text in a fixed order: a header, an optional
//! diagnostic, a table or a dashboard, an optional panel, a prompt. A layout
//! system that divides a rectangle by constraints — the design ratatui uses — is
//! machinery with nothing to do in one column. `docs/design/view-layer.md` §3 and
//! the research record behind it argue this at length.

#![forbid(unsafe_code)]

pub mod bars;
pub mod command;
pub mod compose;
pub mod line;
pub mod report;
pub mod snapshot;

pub use command::{complete, help_lines, Kind, Suggestion, COMMANDS, REFERENCE};
pub use compose::{
    compose, Budget, Frame, Panel, View, DIAGNOSTIC_ROWS, MIN_BODY, REMEDY_ROWS, STALL_AFTER_NS,
    STATUS_ROWS,
};
pub use line::{to_html, to_json, to_text, FrameCache, Line, RenderStats, Span, Style};
pub use report::{parse_report, DaemonReport, Subscriber};
pub use snapshot::{Adapter, InputMode, Snapshot, Tick};
