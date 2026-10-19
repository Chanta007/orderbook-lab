//! Everything one frame is composed from, and nothing derived from it.
//!
//! A snapshot is what the interface observed: the ring's counters, this reader's
//! counters, the book's top levels, the tick counts, the adapter's heartbeat and
//! the daemon's own report. It is deliberately a plain struct of owned values, so a
//! test can build one by hand and check what a frame says without a ring, a daemon
//! or a terminal.
//!
//! Nothing here formats anything. Formatting is [`crate::compose`]'s job, which is
//! what keeps a duration's unit and a price's decimals in one place.

use ob::fixed::{Price, Qty};

/// What the adapter's heartbeat says about the process that should be feeding us.
///
/// The view's own vocabulary rather than the reader's: the reader's type carries a
/// path and a window, and this carries what a frame can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adapter {
    /// It wrote recently. `age_s` is how long ago and `note` is what it said.
    Live {
        /// Seconds since it last wrote.
        age_s: u64,
        /// What it said it was doing, such as `live; watching 2`.
        note: String,
    },
    /// It wrote, then stopped: the file is there and old.
    Stale {
        /// The process id it reported.
        pid: u32,
        /// Seconds since it last wrote.
        age_s: u64,
    },
    /// No file at all, so nothing has written one this run.
    Absent,
}

/// One watched symbol's tick rate, and enough history to draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// The symbol.
    pub symbol: String,
    /// Records for it in the last second.
    pub per_second: f64,
    /// The last few seconds of that rate, oldest first, for a sparkline.
    pub history: Vec<f64>,
}

/// One frame's worth of observed state.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The environment named by the config: `dev` or `prod`.
    pub env: String,
    /// The config this window was opened with, so a remedy can name it.
    pub config_path: String,
    /// The symbol on screen.
    pub symbol: String,
    /// How many symbols are watched.
    pub watched: usize,
    /// How many price levels the user asked for.
    pub levels: usize,
    /// The ring's capacity in records.
    pub ring_capacity: u32,
    /// Records the writer has published.
    pub published: u64,
    /// Records this reader has not taken.
    pub lag: u64,
    /// The span of feed time those records cover.
    pub backlog_ns: Option<u64>,
    /// How old the newest published record is.
    pub staleness_ns: Option<u64>,
    /// Records this reader has taken since it attached.
    pub consumed: u64,
    /// Times the writer lapped this reader.
    pub resyncs: u64,
    /// Times this reader attached to a restarted feed.
    pub reattaches: u64,
    /// How long since this reader applied a record for the displayed symbol.
    pub age_ns: u64,
    /// Whether a record for the displayed symbol has ever been applied.
    pub fed: bool,
    /// Records this reader takes per second, averaged over the last second.
    pub records_per_second: f64,
    /// The bid side, best first.
    pub bids: Vec<(Price, Qty)>,
    /// The ask side, best first.
    pub asks: Vec<(Price, Qty)>,
    /// Tick rates for the watched symbols, in the watch file's order.
    pub ticks: Vec<Tick>,
    /// What the adapter's heartbeat says.
    pub adapter: Adapter,
    /// The daemon's own report, when a log has one.
    pub daemon: Option<crate::report::DaemonReport>,
    /// Where the daemon's report came from, for the frame to name.
    pub daemon_log: String,
    /// The last thing that happened, in the dispatcher's words.
    pub status: String,
    /// What has been typed and not submitted.
    pub input: String,
    /// What the picker is offering for the input line.
    pub suggestions: Vec<String>,
    /// Which suggestion is highlighted.
    pub highlighted: usize,
    /// Whether stdin is a terminal, so a reader can be told its input is off.
    pub interactive: bool,
}

impl Default for Snapshot {
    /// A snapshot with nothing observed: the shape a test fills in.
    fn default() -> Self {
        Snapshot {
            env: "dev".to_string(),
            config_path: "config/dev.json".to_string(),
            symbol: "BTCUSDT".to_string(),
            watched: 1,
            levels: 5,
            ring_capacity: 4096,
            published: 0,
            lag: 0,
            backlog_ns: None,
            staleness_ns: None,
            consumed: 0,
            resyncs: 0,
            reattaches: 0,
            age_ns: 0,
            fed: false,
            records_per_second: 0.0,
            bids: Vec::new(),
            asks: Vec::new(),
            ticks: Vec::new(),
            adapter: Adapter::Absent,
            daemon: None,
            daemon_log: "var/dev/feedd.log".to_string(),
            status: String::new(),
            input: String::new(),
            suggestions: Vec::new(),
            highlighted: 0,
            interactive: true,
        }
    }
}

impl Snapshot {
    /// Whether the displayed symbol is being fed right now.
    ///
    /// Two conditions, both needed: a record has arrived for it, and the last one
    /// was recent. Without the first, a symbol selected a moment ago reads as
    /// stalled; without the second, a feed that stopped an hour ago reads as live.
    pub fn live(&self, stall_after_ns: u64) -> bool {
        self.fed && self.age_ns < stall_after_ns
    }

    /// The ring's occupancy as a fraction, for a fill bar.
    ///
    /// The ring holds the newest `capacity` records and overwrites them in place,
    /// so what is resident is `published` until the writer has lapped once, and
    /// full from then on. This is a property of the ring, not of this reader.
    pub fn ring_fill(&self) -> f64 {
        let capacity = f64::from(self.ring_capacity);
        if capacity <= 0.0 {
            return 0.0;
        }
        (self.published as f64 / capacity).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_needs_both_a_record_and_a_recent_one() {
        // Nothing has arrived for this symbol yet, however fresh this window is.
        let fresh_but_empty = Snapshot::default();
        assert!(!fresh_but_empty.live(1_000_000_000));

        let fed = Snapshot {
            fed: true,
            ..Snapshot::default()
        };
        assert!(fed.live(1_000_000_000));

        let old = Snapshot {
            fed: true,
            age_ns: 2_000_000_000,
            ..Snapshot::default()
        };
        assert!(!old.live(1_000_000_000));
    }

    #[test]
    fn ring_fill_is_full_once_the_writer_has_been_all_the_way_round() {
        let empty = Snapshot {
            ring_capacity: 100,
            ..Snapshot::default()
        };
        assert_eq!(empty.ring_fill(), 0.0);
        let half = Snapshot {
            published: 50,
            ..empty.clone()
        };
        assert!((half.ring_fill() - 0.5).abs() < f64::EPSILON);
        let lapped = Snapshot {
            published: 900,
            ..empty
        };
        assert_eq!(
            lapped.ring_fill(),
            1.0,
            "a ring past its capacity is full, not negative"
        );
    }

    #[test]
    fn a_default_snapshot_is_the_shape_a_test_fills_in() {
        let s = Snapshot::default();
        assert_eq!(s.env, "dev");
        assert_eq!(s.levels, 5);
        assert!(s.ticks.is_empty());
        assert_eq!(s.adapter, Adapter::Absent);
        assert!(s.daemon.is_none());
    }
}
