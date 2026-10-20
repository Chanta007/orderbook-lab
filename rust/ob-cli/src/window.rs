//! The window's state: what it is showing, where its reader is, and what it counted.
//!
//! One window on one ring. It owns the cursor, the counters, the tick table, the
//! daemon's log tail and the level buffers, and it answers one question: given what
//! the ring and the files say, what is on screen? The terminal binary draws its
//! answer, the `view` binary prints it, and the web binary streams it — none of them
//! keeps state of its own, which is why they cannot disagree about what the feed is
//! doing.
//!
//! The counters live here rather than in a caller because they are one thing — this
//! reader's view of one run — and because a reset (a switch, a re-attach) has to
//! clear all of them together. Leaving one behind is how a frame ends up describing
//! two runs as though they were one.

use crate::daemon::DaemonLog;
use crate::stats::Ticks;
use crate::watch::Watch;
use crate::{read_heartbeat, Liveness};
use ob::book::{Book, Level};
use ob::config::Config;
use ob::msg::Msg;
use ob::ring::RingReader;
use ob_view::{Adapter, InputMode, Snapshot, View};
use std::path::Path;
use std::time::Instant;

/// What a re-attach check found.
#[derive(Debug, PartialEq, Eq)]
pub enum Attached {
    /// The ring at the path is the one this window has, and it is the same run.
    Current,
    /// A new run wrote this ring, or a new file is at the path. Either way the
    /// cursor and the book belong to a run that is over.
    Restarted,
    /// A new file is at the path and it cannot be opened as a ring.
    Failed(String),
}

/// Re-attach the window to the ring at `path`, when it is no longer the one it
/// was reading.
///
/// Split out of the loop so the decision can be read without the drawing around
/// it, and because it is the whole answer to "the feed came back and the window
/// did not": nothing else in this program can notice, since both failures look
/// like a valid ring with nothing new in it.
pub fn reattach(ring: &mut RingReader, path: &Path, cursor: u64) -> Attached {
    if ring.replaced(path) {
        match RingReader::open(path) {
            Ok(fresh) => *ring = fresh,
            Err(e) => {
                return Attached::Failed(format!(
                    "the ring at {} was replaced and cannot be opened: {e}",
                    path.display()
                ));
            }
        }
        return Attached::Restarted;
    }
    // The writer truncates the header it created, so the sequence going
    // backwards is a new run over the same file.
    if ring.published() < cursor {
        return Attached::Restarted;
    }
    Attached::Current
}

/// Feed time between the oldest record not yet taken and the newest published.
pub fn backlog_ns(ring: &RingReader, cursor: u64) -> Option<u64> {
    let newest = ring.published().checked_sub(1)?;
    if cursor > newest {
        return Some(0);
    }
    let first_waiting = ring.timestamp_at(cursor)?;
    let latest = ring.timestamp_at(newest)?;
    Some(latest.saturating_sub(first_waiting))
}

/// How old the newest published record is, from this process's clock.
pub fn staleness_ns(ring: &RingReader) -> Option<u64> {
    let newest = ring.published().checked_sub(1)?;
    let latest = ring.timestamp_at(newest)?;
    Some(ob::clock::now_ns().saturating_sub(latest))
}

/// The adapter's heartbeat, in the view's vocabulary.
///
/// Two types for one file: the reader's (`Liveness`, with the path and the window it
/// applies) and the view's (`Adapter`, with what a frame can say). The conversion is
/// here because this is the layer that knows both.
pub fn adapter_state(path: &Path) -> Adapter {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match read_heartbeat(path, now) {
        Liveness::Live { age_s, note, .. } => Adapter::Live { age_s, note },
        Liveness::Stale { pid, age_s, .. } => Adapter::Stale { pid, age_s },
        Liveness::Absent => Adapter::Absent,
    }
}

/// The window's own state: what it is showing, where its reader is, and what it has
/// counted.
///
/// The counters live here rather than in the loop because they are one thing — this
/// window's view of one run — and because a reset (a switch, a re-attach) has to
/// clear all of them together. Leaving one behind is how a frame ends up describing
/// two runs as though they were one.
#[derive(Debug)]
pub struct Window {
    /// Which body is on screen, and whether the reference is open.
    pub view: View,
    /// The symbol on screen.
    pub displayed: String,
    /// The last thing that happened, in words.
    pub status: String,
    /// How many levels the user asked for.
    pub levels: usize,
    /// Where this reader has got to in the ring.
    pub cursor: u64,
    /// Records taken since the attach.
    pub consumed: u64,
    /// Times the writer lapped this reader.
    pub resyncs: u64,
    /// Times this reader attached to a restarted feed.
    pub reattaches: u64,
    /// When a record for the displayed symbol was last applied.
    pub last_record: Instant,
    /// Whether one ever was.
    pub fed: bool,
    /// How many symbols are watched.
    pub watched: usize,
    /// Per-symbol tick counts and their history.
    pub ticks: Ticks,
    /// The daemon's log, tailed for its own report.
    pub daemon: DaemonLog,
    /// A level buffer for the bid side, reused every frame.
    bids: Vec<Level>,
    /// A level buffer for the ask side, reused every frame.
    asks: Vec<Level>,
}

/// What the input line is showing, for a frame.
#[derive(Debug)]
pub struct Prompt<'a> {
    /// What has been typed and not submitted.
    pub line: &'a str,
    /// What the picker is offering, from the shared completion.
    pub suggestions: &'a [ob_view::Suggestion],
    /// Which suggestion is highlighted.
    pub highlighted: usize,
    /// Where the person reading this frame types.
    pub input_mode: InputMode,
}

impl Window {
    /// A window on the config at `cfg_path`, watching what `watch` says.
    pub fn new(cfg_path: &str, watch: &Watch, levels: usize) -> Self {
        Window {
            view: View::default(),
            displayed: watch.current().to_string(),
            status: String::new(),
            levels,
            cursor: 0,
            consumed: 0,
            resyncs: 0,
            reattaches: 0,
            last_record: Instant::now(),
            fed: false,
            watched: watch.len(),
            ticks: Ticks::new(),
            daemon: DaemonLog::new(crate::daemon::locate(Path::new(cfg_path))),
            bids: vec![Level::EMPTY; levels + 1],
            asks: vec![Level::EMPTY; levels + 1],
        }
    }

    /// Start again at the live end of the ring.
    ///
    /// A book built from a half-replayed history would be missing the resets that
    /// came before it, and a stale price would sit above the asks until the next
    /// picture arrived, so a switch and a re-attach both start here.
    pub fn restart_at(&mut self, cursor: u64) {
        self.cursor = cursor;
        self.last_record = Instant::now();
        self.fed = false;
        self.consumed = 0;
        self.resyncs = 0;
    }

    /// Take everything the ring has, and apply it.
    ///
    /// Returns how many records were read. Every record is counted in the tick table
    /// — the status view asks what each watched symbol is doing, not only what is on
    /// screen — and only the displayed symbol's records reach the book.
    pub fn drain(&mut self, ring: &RingReader, book: &mut Book, out: &mut [Msg]) -> usize {
        let n = ring.consume_batch(&mut self.cursor, out);
        if n == 0 {
            return 0;
        }
        let mut applied = 0;
        for m in &out[..n] {
            self.ticks.count(m.symbol_bytes());
            if m.symbol_bytes() == self.displayed.as_bytes() {
                book.apply(m);
                applied += 1;
            }
        }
        self.consumed += n as u64;
        if applied > 0 {
            self.last_record = Instant::now();
            self.fed = true;
        }
        n
    }

    /// The three times, as the explanation needs them.
    pub fn times(&self, ring: &RingReader) -> ob_view::command::Times {
        ob_view::command::Times {
            lag: ring.lag(self.cursor),
            backlog_ns: backlog_ns(ring, self.cursor),
            staleness_ns: staleness_ns(ring),
            age_ns: self
                .last_record
                .elapsed()
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        }
    }

    /// Compose everything this frame needs into a snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot(
        &mut self,
        ring: &RingReader,
        book: &Book,
        cfg: &Config,
        cfg_path: &str,
        adapter: Adapter,
        prompt: &Prompt<'_>,
    ) -> Snapshot {
        self.levels = self.levels.clamp(1, 64);
        // `levels N` can ask for more than the window opened with, so the buffers
        // grow with the request. Sizing them once from the config left a deeper book
        // showing only the rows the buffers happened to have, which is a frame that
        // silently ignores the command it was given.
        if self.bids.len() < self.levels + 1 {
            self.bids.resize(self.levels + 1, Level::EMPTY);
            self.asks.resize(self.levels + 1, Level::EMPTY);
        }
        let want = self.levels.min(self.bids.len());
        let nb = book.bids.top(want, &mut self.bids);
        let na = book.asks.top(want, &mut self.asks);
        let mut bids = Vec::with_capacity(nb);
        let mut asks = Vec::with_capacity(na);
        for level in &self.bids[..nb] {
            bids.push((level.px, level.qty));
        }
        for level in &self.asks[..na] {
            asks.push((level.px, level.qty));
        }
        let age_ns = self
            .last_record
            .elapsed()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        Snapshot {
            env: cfg.env.clone(),
            config_path: cfg_path.to_string(),
            symbol: self.displayed.clone(),
            watched: self.watched,
            levels: self.levels,
            ring_capacity: ring.capacity(),
            published: ring.published(),
            lag: ring.lag(self.cursor),
            backlog_ns: backlog_ns(ring, self.cursor),
            staleness_ns: staleness_ns(ring),
            consumed: self.consumed,
            resyncs: self.resyncs,
            reattaches: self.reattaches,
            age_ns,
            fed: self.fed,
            records_per_second: self.ticks.rate(),
            bids,
            asks,
            ticks: self.ticks.snapshot(),
            adapter,
            daemon: self.daemon.report().cloned(),
            daemon_log: self.daemon.path().display().to_string(),
            status: self.status.clone(),
            input: prompt.line.to_string(),
            suggestions: prompt.suggestions.to_vec(),
            highlighted: prompt.highlighted,
            input_mode: prompt.input_mode,
        }
    }
}
