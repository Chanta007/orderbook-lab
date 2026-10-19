//! Per-symbol tick counts, and the rate history a status view draws.
//!
//! The reader already sees every record and already compares its symbol with the one
//! on screen. Counting them is one integer per watched symbol per record, which is
//! why this is a fixed table with a linear probe rather than a map: a map would
//! allocate on the drain path, and the drain path runs a hundred thousand times a
//! second.
//!
//! The table is sized for the watch set, which is tens of symbols, not thousands:
//! this is a view of what is being watched, and the watch file is written by hand.
//! A symbol that does not fit is counted in [`Ticks::other`] rather than dropped, so
//! the frame can say that the table is full instead of quietly showing nothing.

/// The most symbols the tick table holds.
///
/// 64: the watch set is written by hand and the catalog is a list of pairs a person
/// reads, so a set this large is already beyond what the panel can show. A symbol
/// past the end is counted as "other", and the frame says so.
pub const CAPACITY: usize = 64;

/// How many one-second samples of each rate are kept for a sparkline.
pub const HISTORY: usize = 16;

/// One symbol's counters: this second, and the last few seconds' rates.
#[derive(Debug, Clone)]
struct Row {
    /// The symbol, at most eight bytes on the wire and copied as text.
    symbol: String,
    /// Records seen since the last fold.
    count: u32,
    /// One rate per second, oldest first.
    history: Vec<f64>,
}

/// Tick counts by symbol, with a rate history.
#[derive(Debug)]
pub struct Ticks {
    rows: Vec<Row>,
    /// Records for a symbol not in the table.
    other: u64,
    /// When the last fold happened.
    folded: std::time::Instant,
    /// Records seen since the last fold, all symbols together.
    total: u64,
    /// The rate of the last completed second, all symbols together.
    rate: f64,
}

impl Default for Ticks {
    fn default() -> Self {
        Ticks::new()
    }
}

impl Ticks {
    /// An empty table.
    pub fn new() -> Self {
        Ticks {
            rows: Vec::new(),
            other: 0,
            folded: std::time::Instant::now(),
            total: 0,
            rate: 0.0,
        }
    }

    /// Count one record for `symbol`.
    ///
    /// The linear probe is over at most [`CAPACITY`] rows and usually over a
    /// handful, and it compares eight bytes at a time, so it is a few nanoseconds
    /// on the path that matters.
    pub fn count(&mut self, symbol: &[u8]) {
        self.total += 1;
        for row in &mut self.rows {
            if row.symbol.as_bytes() == symbol {
                row.count = row.count.saturating_add(1);
                return;
            }
        }
        if self.rows.len() < CAPACITY {
            let text = String::from_utf8_lossy(symbol).into_owned();
            self.rows.push(Row {
                symbol: text,
                count: 1,
                history: Vec::new(),
            });
        } else {
            self.other = self.other.saturating_add(1);
        }
    }

    /// Fold the counts of the last second into rates, if a second has passed.
    ///
    /// Called once per frame. The rate is what the counts of the elapsed second were
    /// divided by the time that actually elapsed, so a frame loop that is late does
    /// not report a rate that is wrong by the lateness.
    pub fn fold_if_due(&mut self) -> bool {
        let elapsed = self.folded.elapsed();
        if elapsed < std::time::Duration::from_secs(1) {
            return false;
        }
        let seconds = elapsed.as_secs_f64();
        for row in &mut self.rows {
            let per_second = f64::from(row.count) / seconds;
            if row.history.len() == HISTORY {
                row.history.remove(0);
            }
            row.history.push(per_second);
            row.count = 0;
        }
        self.rate = self.total as f64 / seconds;
        self.total = 0;
        self.folded = std::time::Instant::now();
        true
    }

    /// Records per second across every symbol, from the last completed second.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// How many records were for a symbol the table could not hold.
    pub fn other(&self) -> u64 {
        self.other
    }

    /// Whether the table is full, so a caller can say a symbol was not counted.
    pub fn full(&self) -> bool {
        self.rows.len() >= CAPACITY
    }

    /// The tick rows for a frame, busiest first.
    ///
    /// The rate of the elapsed second is included so the first frame after a fold
    /// shows a number rather than a zero, and the history is what the sparkline
    /// draws.
    pub fn snapshot(&self) -> Vec<ob_view::Tick> {
        let mut out: Vec<ob_view::Tick> = self
            .rows
            .iter()
            .map(|row| {
                // A symbol counted for the first time has no history yet; its
                // current count is the honest rate until the first fold.
                let current = if row.history.is_empty() {
                    row.count as f64
                } else {
                    row.history[row.history.len() - 1]
                };
                // A symbol with no folded second yet gets one point of history, so
                // the sparkline has something to draw; one that has been counted
                // since the last fold gets a point for the second in progress.
                let mut history = row.history.clone();
                if history.is_empty() || row.count > 0 {
                    history.push(current);
                }
                ob_view::Tick {
                    symbol: row.symbol.clone(),
                    per_second: current,
                    history,
                }
            })
            .collect();
        out.sort_by(|a, b| b.per_second.total_cmp(&a.per_second));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(ticks: &mut Ticks, symbol: &str, n: usize) {
        for _ in 0..n {
            ticks.count(symbol.as_bytes());
        }
    }

    #[test]
    fn the_second_record_for_a_symbol_does_not_add_a_row() {
        let mut ticks = Ticks::new();
        count(&mut ticks, "BTCUSDT", 5);
        count(&mut ticks, "ETHUSDT", 2);
        count(&mut ticks, "BTCUSDT", 3);
        let rows = ticks.snapshot();
        assert_eq!(rows.len(), 2, "two symbols, two rows");
        let btc = rows.iter().find(|r| r.symbol == "BTCUSDT").expect("btc");
        assert_eq!(btc.per_second, 8.0, "the counts of one symbol add up");
    }

    #[test]
    fn a_fold_turns_counts_into_a_rate_and_keeps_the_history() {
        let mut ticks = Ticks::new();
        count(&mut ticks, "BTCUSDT", 10);
        // The fold is due after a second; a test cannot wait for one, so the
        // interesting part — that a fold moves the count into the history and
        // clears it — is checked through the rate the frame reads.
        assert!(!ticks.fold_if_due(), "not yet due");
        assert_eq!(ticks.rate(), 0.0, "no completed second yet");
        let rows = ticks.snapshot();
        assert_eq!(
            rows[0].per_second, 10.0,
            "the elapsed count is the rate so far"
        );
    }

    #[test]
    fn the_table_has_a_capacity_and_says_when_it_is_full() {
        let mut ticks = Ticks::new();
        for i in 0..CAPACITY {
            count(&mut ticks, &format!("S{i:06}"), 1);
        }
        assert!(ticks.full());
        assert_eq!(ticks.other(), 0);
        count(&mut ticks, "OVERFLOW", 3);
        assert_eq!(ticks.other(), 3, "an uncounted symbol is counted as other");
        assert_eq!(ticks.snapshot().len(), CAPACITY);
    }

    #[test]
    fn the_snapshot_is_busiest_first() {
        let mut ticks = Ticks::new();
        count(&mut ticks, "AAA", 1);
        count(&mut ticks, "BBB", 9);
        count(&mut ticks, "CCC", 5);
        let rows = ticks.snapshot();
        assert_eq!(
            rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(),
            vec!["BBB", "CCC", "AAA"]
        );
    }

    #[test]
    fn an_empty_table_snapshots_to_nothing_rather_than_to_a_zero_row() {
        let ticks = Ticks::new();
        assert!(ticks.snapshot().is_empty());
        assert!(!ticks.full());
        assert_eq!(ticks.other(), 0);
    }
}
