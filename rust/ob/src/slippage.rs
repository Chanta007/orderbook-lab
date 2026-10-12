//! What an execution actually cost, split into the part that was latency and
//! the part that was depth.
//!
//! # The decomposition
//!
//! A taker decides at one price and trades at another. The gap has two causes,
//! and lumping them together makes the number useless because the fixes are
//! different:
//!
//! ```text
//!   decision mid          mid when the order         price actually
//!   (t0)                  reached the book (t1)      paid (t1)
//!       |                        |                        |
//!       |<--- latency move ----->|<---- depth cost ------>|
//!       |<------------------- implementation shortfall -->|
//! ```
//!
//! - **Latency move** — the market moved while the decision was in flight. Its
//!   size is a property of how fast this pipeline is and how volatile the symbol
//!   is. A faster pipeline reduces it; nothing else does.
//! - **Depth cost** — the order walked past the best price, because the size at
//!   the touch was not enough. Its size is a property of the order relative to
//!   the book. A smaller order reduces it; speed does not.
//!
//! Both are reported in **hundredths of a basis point** of the reference mid, and
//! they add up to the implementation shortfall. A system that reports only one
//! number cannot tell an operator whether to optimise the feed or change the
//! order size, which is the whole reason for the split.
//!
//! # Sign convention
//!
//! Every figure is a **cost when positive**. For a buy, paying above the
//! reference is a cost; for a sell, receiving below it is. The caller passes the
//! side and this module applies the sign, so a report can add buys and sells
//! together without knowing which was which.
//!
//! # Markout
//!
//! Markout answers a different question: after the trade, did the price move in
//! the position's favour? It is measured at fixed horizons after the execution,
//! against the mid at that later time. A negative markout on a buy means the
//! price fell after buying, which is the signature of having been picked off by
//! faster participants.
//!
//! Markouts need the *future*, so an observation is queued when the trade
//! happens and settled later by [`SlippageLedger::resolve`], which the publisher
//! calls on its timer. A queue that overflows loses the oldest observation and
//! counts it, because an unsettled markout is better dropped than mis-settled
//! against a stale mid.

use crate::fixed::{delta_bps_x100, Price, Qty};
use crate::symbol::SymbolId;

/// How many markout horizons a ledger tracks.
pub const HORIZONS: usize = 3;

/// The horizons, in nanoseconds: 10 ms, 100 ms, 1 s.
pub const DEFAULT_HORIZONS_NS: [u64; HORIZONS] = [10_000_000, 100_000_000, 1_000_000_000];

/// Pending markout observations held per symbol before the oldest is dropped.
pub const PENDING_PER_SYMBOL: usize = 64;

/// Which way the order went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Taker {
    /// Bought at the ask.
    Buy,
    /// Sold at the bid.
    Sell,
}

impl Taker {
    /// `+1` for a buy, `-1` for a sell: the multiplier that turns a raw price
    /// difference into a cost.
    #[inline]
    pub const fn sign(self) -> i64 {
        match self {
            Taker::Buy => 1,
            Taker::Sell => -1,
        }
    }
}

/// One execution, as the caller observed it.
#[derive(Debug, Clone, Copy)]
pub struct Execution {
    /// Which way the order went.
    pub taker: Taker,
    /// The mid when the decision was taken.
    pub decision_mid_e8: i64,
    /// The mid when the order reached the book.
    pub action_mid_e8: i64,
    /// The volume-weighted price actually achieved, from [`crate::book::Fill`].
    pub action_px_e8: i64,
    /// How much was filled.
    pub filled: Qty,
    /// How much could not be filled.
    pub unfilled: Qty,
}

/// The cost of one execution, decomposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cost {
    /// The adverse move between decision and action. Positive is a cost.
    pub latency_bps_x100: i64,
    /// The cost of walking past the touch. Positive is a cost.
    pub depth_bps_x100: i64,
    /// The two added together: what the execution cost against the decision mid.
    pub shortfall_bps_x100: i64,
    /// Twice the distance from the action price to the action mid, which is the
    /// round-trip cost of crossing at that moment.
    pub effective_spread_bps_x100: i64,
}

/// Accumulated markouts at one horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MarkoutAcc {
    /// Observations settled.
    pub count: u64,
    /// Sum of the signed markouts, in hundredths of a basis point.
    pub sum_bps_x100: i128,
    /// The worst signed markout seen, in hundredths of a basis point.
    pub worst_bps_x100: i64,
}

impl MarkoutAcc {
    /// The mean signed markout, or zero when nothing has settled.
    #[inline]
    pub fn mean_bps_x100(&self) -> i64 {
        if self.count == 0 {
            0
        } else {
            (self.sum_bps_x100 / i128::from(self.count)) as i64
        }
    }
}

/// A trade waiting for its markout horizons to pass.
#[derive(Debug, Clone, Copy)]
struct Pending {
    at_ns: u64,
    taker: Taker,
    action_px_e8: i64,
    /// Which horizons have already been settled, as a bit per horizon.
    settled: u8,
    /// Set once every horizon has been settled, so the slot can be reused.
    done: bool,
}

impl Default for Pending {
    fn default() -> Self {
        Pending {
            at_ns: 0,
            taker: Taker::Buy,
            action_px_e8: 0,
            settled: 0,
            done: true,
        }
    }
}

/// What one symbol's executions cost.
#[derive(Debug, Clone)]
pub struct SymbolLedger {
    /// Executions recorded.
    pub executions: u64,
    /// Sum of the latency component.
    pub latency_sum: i128,
    /// Sum of the depth component.
    pub depth_sum: i128,
    /// Sum of the implementation shortfall.
    pub shortfall_sum: i128,
    /// Sum of the effective spread.
    pub spread_sum: i128,
    /// The worst implementation shortfall seen.
    pub worst_shortfall: i64,
    /// The best (most negative) implementation shortfall seen.
    pub best_shortfall: i64,
    /// Quantity filled, summed.
    pub filled: i128,
    /// Quantity that could not be filled, summed.
    pub unfilled: i128,
    /// Markout accumulators, one per horizon.
    pub markout: [MarkoutAcc; HORIZONS],
    /// Observations dropped because the pending queue was full.
    pub dropped_pending: u64,
    pending: [Pending; PENDING_PER_SYMBOL],
    next_pending: usize,
}

impl Default for SymbolLedger {
    fn default() -> Self {
        SymbolLedger {
            executions: 0,
            latency_sum: 0,
            depth_sum: 0,
            shortfall_sum: 0,
            spread_sum: 0,
            worst_shortfall: i64::MIN,
            best_shortfall: i64::MAX,
            filled: 0,
            unfilled: 0,
            markout: [MarkoutAcc::default(); HORIZONS],
            dropped_pending: 0,
            pending: [Pending::default(); PENDING_PER_SYMBOL],
            next_pending: 0,
        }
    }
}

impl SymbolLedger {
    /// Mean latency component, or zero.
    pub fn mean_latency(&self) -> i64 {
        mean(self.latency_sum, self.executions)
    }

    /// Mean depth component, or zero.
    pub fn mean_depth(&self) -> i64 {
        mean(self.depth_sum, self.executions)
    }

    /// Mean implementation shortfall, or zero.
    pub fn mean_shortfall(&self) -> i64 {
        mean(self.shortfall_sum, self.executions)
    }

    /// Mean effective spread, or zero.
    pub fn mean_spread(&self) -> i64 {
        mean(self.spread_sum, self.executions)
    }

    /// The worst shortfall, or zero when nothing has been recorded.
    pub fn worst_shortfall(&self) -> i64 {
        if self.executions == 0 {
            0
        } else {
            self.worst_shortfall
        }
    }

    /// The best shortfall, or zero when nothing has been recorded.
    pub fn best_shortfall(&self) -> i64 {
        if self.executions == 0 {
            0
        } else {
            self.best_shortfall
        }
    }
}

/// Mean of a sum over a count, or zero.
fn mean(sum: i128, count: u64) -> i64 {
    if count == 0 {
        0
    } else {
        (sum / i128::from(count)) as i64
    }
}

/// The cost of executions, per symbol.
///
/// Allocated once, sized by the symbol table's capacity, and indexed by
/// [`SymbolId`]. Nothing here allocates on the path an execution takes.
#[derive(Debug)]
pub struct SlippageLedger {
    ledgers: Box<[SymbolLedger]>,
    horizons: [u64; HORIZONS],
    enabled: bool,
}

impl SlippageLedger {
    /// Build a ledger with room for `symbols` symbols.
    pub fn new(symbols: usize, horizons: [u64; HORIZONS], enabled: bool) -> Self {
        SlippageLedger {
            ledgers: (0..symbols.max(1))
                .map(|_| SymbolLedger::default())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            horizons,
            enabled,
        }
    }

    /// Whether anything is being recorded.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The horizons, in nanoseconds.
    #[inline]
    pub fn horizons(&self) -> &[u64; HORIZONS] {
        &self.horizons
    }

    /// How many symbols this ledger has room for.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.ledgers.len()
    }

    /// One symbol's ledger.
    ///
    /// Returns `None` for a symbol the ledger has no room for, rather than
    /// panicking: a feed that grew past the configured symbol count should
    /// report the loss, not stop.
    #[inline]
    pub fn symbol(&self, id: SymbolId) -> Option<&SymbolLedger> {
        self.ledgers.get(id.index())
    }

    /// Record one execution and queue its markouts.
    ///
    /// Returns the cost decomposition, so a caller can log the individual trade
    /// rather than only the aggregate.
    pub fn record(&mut self, id: SymbolId, now_ns: u64, exec: Execution) -> Option<Cost> {
        let cost = decompose(&exec);
        if !self.enabled {
            return Some(cost);
        }
        let slot = self.ledgers.get_mut(id.index())?;
        slot.executions += 1;
        slot.latency_sum += i128::from(cost.latency_bps_x100);
        slot.depth_sum += i128::from(cost.depth_bps_x100);
        slot.shortfall_sum += i128::from(cost.shortfall_bps_x100);
        slot.spread_sum += i128::from(cost.effective_spread_bps_x100);
        slot.filled += i128::from(exec.filled.as_e8());
        slot.unfilled += i128::from(exec.unfilled.as_e8());
        slot.worst_shortfall = slot.worst_shortfall.max(cost.shortfall_bps_x100);
        slot.best_shortfall = slot.best_shortfall.min(cost.shortfall_bps_x100);

        // Queue the markout observation. The slot chosen is the one after the
        // last, wrapping; if it is still waiting on a horizon, the oldest
        // unsettled observation is dropped and counted.
        let index = slot.next_pending;
        if !slot.pending[index].done {
            slot.dropped_pending += 1;
        }
        slot.pending[index] = Pending {
            at_ns: now_ns,
            taker: exec.taker,
            action_px_e8: exec.action_px_e8,
            settled: 0,
            done: false,
        };
        slot.next_pending = (index + 1) % PENDING_PER_SYMBOL;
        Some(cost)
    }

    /// Settle every pending observation whose horizons have passed.
    ///
    /// `mid_e8` is the symbol's current mid. Returns how many horizons were
    /// settled.
    ///
    /// Called from the publisher's timer, not from the update path: a markout is
    /// by definition about a time that has already passed, so there is nothing
    /// to gain by checking it on every record.
    pub fn resolve(&mut self, id: SymbolId, now_ns: u64, mid_e8: i64) -> usize {
        if !self.enabled || mid_e8 == 0 {
            return 0;
        }
        let horizons = self.horizons;
        match self.ledgers.get_mut(id.index()) {
            Some(slot) => resolve_in(slot, &horizons, now_ns, mid_e8),
            None => 0,
        }
    }

    /// Settle pending observations for every symbol.
    ///
    /// The publisher calls this on its timer with a closure that returns each
    /// symbol's current mid, given its ledger slot. The slot is a plain `usize`
    /// rather than a [`SymbolId`] on purpose: a ledger slot is not an interned
    /// symbol, and handing out an id that never came from the symbol table would
    /// let a caller index the table with it.
    ///
    /// Returning `None` leaves that symbol's observations pending, which is what
    /// happens while its book is one-sided.
    pub fn resolve_all<F>(&mut self, now_ns: u64, mut mid_of: F) -> usize
    where
        F: FnMut(usize) -> Option<i64>,
    {
        if !self.enabled {
            return 0;
        }
        let mut settled = 0;
        for index in 0..self.ledgers.len() {
            let Some(mid) = mid_of(index) else {
                continue;
            };
            let horizons = self.horizons;
            let slot = &mut self.ledgers[index];
            settled += resolve_in(slot, &horizons, now_ns, mid);
        }
        settled
    }
}

/// Settle one symbol's pending observations against a current mid.
///
/// Shared by [`SlippageLedger::resolve`] and [`SlippageLedger::resolve_all`] so
/// that a single-symbol settle and a sweep cannot drift apart.
fn resolve_in(
    slot: &mut SymbolLedger,
    horizons: &[u64; HORIZONS],
    now_ns: u64,
    mid_e8: i64,
) -> usize {
    let mut settled = 0;
    for p in slot.pending.iter_mut() {
        if p.done {
            continue;
        }
        let elapsed = now_ns.saturating_sub(p.at_ns);
        for (h, &horizon) in horizons.iter().enumerate() {
            let bit = 1u8 << h;
            if p.settled & bit != 0 || elapsed < horizon {
                continue;
            }
            // Signed so that a favourable move is positive: a buy wants the mid
            // to rise, a sell wants it to fall.
            let signed = p
                .taker
                .sign()
                .saturating_mul(mid_e8.saturating_sub(p.action_px_e8));
            let bps = delta_bps_x100(signed, p.action_px_e8);
            let acc = &mut slot.markout[h];
            acc.count += 1;
            acc.sum_bps_x100 += i128::from(bps);
            acc.worst_bps_x100 = acc.worst_bps_x100.min(bps);
            p.settled |= bit;
            settled += 1;
        }
        if p.settled == (1u8 << HORIZONS as u8) - 1 {
            p.done = true;
        }
    }
    settled
}

/// Split one execution into its components.
fn decompose(exec: &Execution) -> Cost {
    let sign = exec.taker.sign();
    let reference = exec.decision_mid_e8;
    // A zero reference has no percentage to report, and every figure here is a
    // percentage of it.
    if reference == 0 {
        return Cost::default();
    }
    let latency = delta_bps_x100(
        sign.saturating_mul(exec.action_mid_e8.saturating_sub(reference)),
        reference,
    );
    let depth = delta_bps_x100(
        sign.saturating_mul(exec.action_px_e8.saturating_sub(exec.action_mid_e8)),
        reference,
    );
    let shortfall = delta_bps_x100(
        sign.saturating_mul(exec.action_px_e8.saturating_sub(reference)),
        reference,
    );
    // The effective spread is unsigned: it is the cost of crossing, and it is
    // the same size whether the order was a buy or a sell.
    let half = exec.action_px_e8.saturating_sub(exec.action_mid_e8).abs();
    let effective_spread = delta_bps_x100(half.saturating_mul(2), reference);
    Cost {
        latency_bps_x100: latency,
        depth_bps_x100: depth,
        shortfall_bps_x100: shortfall,
        effective_spread_bps_x100: effective_spread,
    }
}

/// A price scaled by [`crate::fixed::E8`] from a mid and a spread in hundredths
/// of a basis point.
///
/// Used by the soak to build a synthetic book whose spread is a stated number
/// rather than a round price, which is what makes a synthetic run's slippage
/// numbers comparable with a real one's.
pub fn price_from_bps(mid_e8: i64, bps_x100: i64) -> i64 {
    if mid_e8 == 0 {
        return 0;
    }
    let delta = (i128::from(mid_e8) * i128::from(bps_x100) / 1_000_000) as i64;
    mid_e8.saturating_add(delta)
}

/// A price as a typed value, for callers that want one.
#[inline]
pub fn price(mid_e8: i64, bps_x100: i64) -> Price {
    Price::from_e8(price_from_bps(mid_e8, bps_x100))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::E8;
    use crate::symbol::SymbolTable;

    /// The id for slot `i`, interned through a real table so that a test never
    /// constructs an id the table did not issue.
    fn sid(i: usize) -> SymbolId {
        let mut table = SymbolTable::new(8);
        let mut last = None;
        for k in 0..=i {
            let mut name = [0u8; 8];
            name[0] = b'A' + k as u8;
            last = Some(table.intern(&name).expect("intern"));
        }
        last.expect("at least one")
    }

    fn exec(taker: Taker, decision: i64, action_mid: i64, action_px: i64) -> Execution {
        Execution {
            taker,
            decision_mid_e8: decision,
            action_mid_e8: action_mid,
            action_px_e8: action_px,
            filled: Qty::from_e8(E8),
            unfilled: Qty::from_e8(0),
        }
    }

    #[test]
    fn a_buy_that_pays_above_the_decision_mid_costs_money() {
        // Decision mid 100.00, action mid 100.01 (market moved up 1 bp), paid
        // 100.02 (one bp of depth cost).
        let c = decompose(&exec(
            Taker::Buy,
            100 * E8,
            100 * E8 + 1_000_000,
            100 * E8 + 2_000_000,
        ));
        assert_eq!(c.latency_bps_x100, 100);
        assert_eq!(c.depth_bps_x100, 100);
        assert_eq!(c.shortfall_bps_x100, 200);
        assert_eq!(c.effective_spread_bps_x100, 200);
    }

    #[test]
    fn a_sell_that_receives_below_the_decision_mid_costs_money() {
        // The mirror image: the market moved down 1 bp and the sell got one more
        // basis point below the action mid.
        let c = decompose(&exec(
            Taker::Sell,
            100 * E8,
            100 * E8 - 1_000_000,
            100 * E8 - 2_000_000,
        ));
        assert_eq!(c.latency_bps_x100, 100, "a fall is a cost to a seller");
        assert_eq!(c.depth_bps_x100, 100);
        assert_eq!(c.shortfall_bps_x100, 200);
    }

    #[test]
    fn a_favourable_move_is_negative() {
        let c = decompose(&exec(
            Taker::Buy,
            100 * E8,
            100 * E8 - 1_000_000,
            100 * E8 - 1_000_000,
        ));
        assert_eq!(
            c.latency_bps_x100, -100,
            "the market fell, which helps a buyer"
        );
        assert_eq!(c.depth_bps_x100, 0);
        assert_eq!(c.shortfall_bps_x100, -100);
    }

    #[test]
    fn the_components_add_up_to_the_shortfall() {
        for (taker, decision, action_mid, action_px) in [
            (
                Taker::Buy,
                100 * E8,
                100 * E8 + 3_000_000,
                100 * E8 + 7_000_000,
            ),
            (
                Taker::Sell,
                100 * E8,
                100 * E8 - 3_000_000,
                100 * E8 - 7_000_000,
            ),
            (Taker::Buy, 65000 * E8, 65000 * E8 + 1, 65000 * E8 + 1),
        ] {
            let c = decompose(&exec(taker, decision, action_mid, action_px));
            // The two components are computed from different references, so they
            // agree with the shortfall up to integer truncation in each step.
            let sum = c.latency_bps_x100 + c.depth_bps_x100;
            assert!(
                (sum - c.shortfall_bps_x100).abs() <= 2,
                "{taker:?}: {sum} vs {}",
                c.shortfall_bps_x100
            );
        }
    }

    #[test]
    fn a_zero_reference_reports_no_percentage() {
        let c = decompose(&exec(Taker::Buy, 0, 100 * E8, 100 * E8));
        assert_eq!(c, Cost::default());
    }

    #[test]
    fn the_ledger_accumulates_and_averages() {
        let mut ledger = SlippageLedger::new(4, DEFAULT_HORIZONS_NS, true);
        let id = sid(0);
        let one = ledger
            .record(
                id,
                1_000,
                exec(
                    Taker::Buy,
                    100 * E8,
                    100 * E8 + 1_000_000,
                    100 * E8 + 2_000_000,
                ),
            )
            .expect("recorded");
        assert_eq!(one.shortfall_bps_x100, 200);
        // A second execution with no cost at all.
        ledger.record(id, 2_000, exec(Taker::Buy, 100 * E8, 100 * E8, 100 * E8));

        let s = ledger.symbol(id).expect("ledger");
        assert_eq!(s.executions, 2);
        assert_eq!(s.mean_shortfall(), 100, "(200 + 0) / 2");
        assert_eq!(s.worst_shortfall(), 200);
        assert_eq!(s.best_shortfall(), 0);
        assert_eq!(s.mean_latency(), 50);
        assert_eq!(s.mean_depth(), 50);
    }

    #[test]
    fn an_unknown_symbol_reports_rather_than_panics() {
        let mut ledger = SlippageLedger::new(2, DEFAULT_HORIZONS_NS, true);
        let far = sid(9);
        assert!(ledger.record(far, 0, exec(Taker::Buy, 1, 1, 1)).is_none());
        assert!(ledger.symbol(far).is_none());
        assert_eq!(ledger.resolve(far, 0, 1), 0);
    }

    #[test]
    fn markouts_settle_at_their_horizons_and_only_once() {
        let horizons = [10, 100, 1000];
        let mut ledger = SlippageLedger::new(1, horizons, true);
        let id = sid(0);
        let action = 100 * E8;
        ledger.record(id, 1_000, exec(Taker::Buy, action, action, action));

        // Nothing has passed yet.
        assert_eq!(ledger.resolve(id, 1_005, action), 0);
        // The first horizon passes.
        assert_eq!(ledger.resolve(id, 1_010, action), 1);
        // Resolving again does not double-count.
        assert_eq!(ledger.resolve(id, 1_011, action), 0);
        // The second and third.
        assert_eq!(ledger.resolve(id, 1_100, action), 1);
        assert_eq!(ledger.resolve(id, 2_000, action), 1);
        assert_eq!(ledger.resolve(id, 3_000, action), 0, "all horizons settled");

        let s = ledger.symbol(id).expect("ledger");
        for h in 0..HORIZONS {
            assert_eq!(s.markout[h].count, 1, "horizon {h}");
        }
    }

    #[test]
    fn a_markout_is_signed_for_the_side_that_took() {
        let horizons = [10, 100, 1000];
        let action = 100 * E8;
        // A buy whose price rose afterwards: favourable, so positive.
        let mut up = SlippageLedger::new(1, horizons, true);
        up.record(sid(0), 0, exec(Taker::Buy, action, action, action));
        up.resolve(sid(0), 10, action + 1_000_000);
        assert!(up.symbol(sid(0)).unwrap().markout[0].mean_bps_x100() > 0);

        // The same move for a sell is unfavourable.
        let mut down = SlippageLedger::new(1, horizons, true);
        down.record(sid(0), 0, exec(Taker::Sell, action, action, action));
        down.resolve(sid(0), 10, action + 1_000_000);
        assert!(down.symbol(sid(0)).unwrap().markout[0].mean_bps_x100() < 0);
    }

    #[test]
    fn a_one_sided_book_leaves_markouts_pending_rather_than_settling_them_wrong() {
        let horizons = [10, 100, 1000];
        let mut ledger = SlippageLedger::new(1, horizons, true);
        let id = sid(0);
        ledger.record(id, 0, exec(Taker::Buy, 100, 100, 100));
        // A mid of zero means the book is one-sided; nothing settles.
        assert_eq!(ledger.resolve(id, 5_000, 0), 0);
        assert_eq!(ledger.symbol(id).unwrap().markout[0].count, 0);
        // Once there is a mid, everything that has passed settles at once.
        assert_eq!(ledger.resolve(id, 5_000, 100), HORIZONS);
    }

    #[test]
    fn the_pending_queue_overflows_visibly() {
        let horizons = [u64::MAX / 2, u64::MAX / 2, u64::MAX / 2];
        let mut ledger = SlippageLedger::new(1, horizons, true);
        let id = sid(0);
        for _ in 0..(PENDING_PER_SYMBOL + 10) {
            ledger.record(id, 0, exec(Taker::Buy, 100, 100, 100));
        }
        assert_eq!(ledger.symbol(id).unwrap().dropped_pending, 10);
    }

    #[test]
    fn a_disabled_ledger_costs_nothing_and_records_nothing() {
        let mut ledger = SlippageLedger::new(2, DEFAULT_HORIZONS_NS, false);
        let id = sid(0);
        let c = ledger.record(id, 0, exec(Taker::Buy, 100 * E8, 100 * E8, 100 * E8));
        assert!(c.is_some(), "the caller still gets the decomposition");
        assert_eq!(ledger.symbol(id).unwrap().executions, 0);
        assert_eq!(ledger.resolve(id, u64::MAX, 1), 0);
    }

    #[test]
    fn a_price_can_be_built_from_a_spread_in_basis_points() {
        // One basis point above 100.00 is 100.01.
        assert_eq!(price_from_bps(100 * E8, 100), 100 * E8 + 1_000_000);
        assert_eq!(price_from_bps(100 * E8, -100), 100 * E8 - 1_000_000);
        assert_eq!(price_from_bps(0, 100), 0);
        // And it round-trips through the delta helper.
        let px = price_from_bps(65000 * E8, 250);
        assert_eq!(delta_bps_x100(px - 65000 * E8, 65000 * E8), 250);
    }

    #[test]
    fn the_fill_side_of_an_execution_is_accumulated() {
        let mut ledger = SlippageLedger::new(1, DEFAULT_HORIZONS_NS, true);
        let id = sid(0);
        let mut e = exec(Taker::Buy, 100 * E8, 100 * E8, 100 * E8);
        e.filled = Qty::from_e8(2 * E8);
        e.unfilled = Qty::from_e8(E8);
        ledger.record(id, 0, e);
        let s = ledger.symbol(id).unwrap();
        assert_eq!(s.filled, i128::from(2 * E8));
        assert_eq!(s.unfilled, i128::from(E8));
    }

    #[test]
    fn price_type_wrapper_matches_the_raw_helper() {
        assert_eq!(
            price(100 * E8, 100),
            Price::from_e8(price_from_bps(100 * E8, 100))
        );
    }
}
