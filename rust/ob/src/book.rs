//! One symbol's prices, kept sorted, in flat arrays.
//!
//! # The layout, and why it is two arrays and not one
//!
//! A level is a price and a size. The obvious Rust is `Vec<Level>` where
//! `Level { px: i64, qty: i64 }`, and that is what the C++ implementation
//! effectively has, in a `std::map`. This module stores the two fields in two
//! arrays instead:
//!
//! ```text
//!   px:  [ 65000.10 | 64999.00 | 64990.00 | ... ]   contiguous i64
//!   qty: [     0.50 |     1.20 |     2.00 | ... ]   contiguous i64
//! ```
//!
//! Two reasons, in order of importance:
//!
//! 1. **The scans are over one field at a time.** Selecting the best `n`
//!    levels, computing a spread, and accumulating depth all read prices and
//!    sizes in separate passes. With `Vec<Level>` each pass touches 16 bytes to
//!    use 8, and the values a vector unit wants adjacent are interleaved with
//!    values it does not want. With two arrays, a pass over `px` is a
//!    unit-stride walk over `i64` — the shape LLVM can vectorize, and the shape
//!    `docs/design/vec-optimization.md` measures.
//! 2. **Insertion is a memmove of one array**, not of an array of structs, and
//!    the compiler can use the widest move it has.
//!
//! # Bounded, and honest about it
//!
//! The capacity is fixed at construction. A feed that sends more levels than
//! that has its worst levels dropped and [`Levels::overflowed`] incremented. The
//! alternative — a growable map — puts an allocation on the update path, and the
//! whole point of the cache is that an update never allocates.
//!
//! # Ordering
//!
//! Both sides are stored **best first**: bids descending, asks ascending. That
//! means the best price is always index 0 on both sides, the top `n` is always
//! `[..n]`, and the inside of the book is `(bids[0], asks[0])`. The C++ version
//! keeps `std::map`s and walks one of them backwards to get the same result;
//! storing both in display order removes that asymmetry from every consumer.

use crate::fixed::{Price, Qty};
use crate::msg::{Msg, MsgType, Side};

/// One price level, as handed to a caller.
///
/// A plain value rather than a reference into the arrays, so a caller can hold
/// a snapshot of the top of book without borrowing the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Level {
    /// The price at this level.
    pub px: Price,
    /// The size resting at it.
    pub qty: Qty,
}

impl Level {
    /// A level that is not there. Used to pad a short side in a report.
    pub const EMPTY: Level = Level {
        px: Price::from_e8(0),
        qty: Qty::from_e8(0),
    };

    /// Whether this level carries a price.
    #[inline]
    pub fn is_present(&self) -> bool {
        self.qty.is_live()
    }
}

/// What one [`Book::apply`] call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The record was not a book change (a heartbeat or a control record).
    Ignored,
    /// The record named a side this book does not have.
    WrongSide,
    /// One side was cleared.
    Reset(Side),
    /// One price was inserted, replaced, or removed.
    Changed(Side),
    /// The level would have gone past the side's capacity, so the worst level
    /// was dropped instead.
    Overflow(Side),
}

impl Applied {
    /// Whether the top of book may have moved.
    #[inline]
    pub fn touched_book(self) -> bool {
        matches!(
            self,
            Applied::Reset(_) | Applied::Changed(_) | Applied::Overflow(_)
        )
    }
}

/// One side of a book: prices and sizes, best first.
///
/// The two arrays are always the same length, and always sorted.
#[derive(Debug)]
pub struct Levels {
    px: Box<[i64]>,
    qty: Box<[i64]>,
    len: usize,
    /// Levels dropped because the side was full.
    overflowed: u64,
    /// Whether the side is a bid side, which decides the sort direction.
    is_bid: bool,
}

impl Levels {
    /// Create an empty side with room for `capacity` levels.
    ///
    /// `capacity` is rounded up to at least one.
    pub fn new(capacity: usize, is_bid: bool) -> Self {
        let capacity = capacity.max(1);
        Levels {
            px: vec![0i64; capacity].into_boxed_slice(),
            qty: vec![0i64; capacity].into_boxed_slice(),
            len: 0,
            overflowed: 0,
            is_bid,
        }
    }

    /// How many levels are present.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the side is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many levels the side can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.px.len()
    }

    /// Levels dropped because the side was full.
    #[inline]
    pub fn overflowed(&self) -> u64 {
        self.overflowed
    }

    /// The prices, best first.
    #[inline]
    pub fn prices(&self) -> &[i64] {
        &self.px[..self.len]
    }

    /// The sizes, best first.
    #[inline]
    pub fn sizes(&self) -> &[i64] {
        &self.qty[..self.len]
    }

    /// The best level, if the side is not empty.
    #[inline]
    pub fn best(&self) -> Option<Level> {
        if self.len == 0 {
            None
        } else {
            Some(Level {
                px: Price::from_e8(self.px[0]),
                qty: Qty::from_e8(self.qty[0]),
            })
        }
    }

    /// Remove every level.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Where a price belongs in this side, and whether it is already there.
    ///
    /// A linear scan, not a binary search. The sides hold a handful of levels
    /// — five on the feed this lab reads, at most a few tens for a deeper
    /// subscription — and a linear scan over fewer than 32 contiguous `i64`s is
    /// faster than a binary search's branchy tree of comparisons. The scan also
    /// stops early on the common cases: a price better than the best level, or
    /// one already present.
    #[inline]
    fn locate(&self, price: i64) -> (usize, bool) {
        let mut i = 0;
        while i < self.len {
            let at = self.px[i];
            if at == price {
                return (i, true);
            }
            let past = if self.is_bid { at < price } else { at > price };
            if past {
                return (i, false);
            }
            i += 1;
        }
        (self.len, false)
    }

    /// Insert or replace a level, keeping the side sorted.
    ///
    /// Returns `true` when the level was new, `false` when it replaced one.
    /// Returns `None` when the side was full and the incoming level was worse
    /// than every level already present, in which case nothing changed.
    pub fn upsert(&mut self, price: i64, size: i64) -> Option<bool> {
        let (at, present) = self.locate(price);
        if present {
            self.qty[at] = size;
            return Some(false);
        }
        if self.len == self.px.len() {
            // Full. A price that sorts after every existing level is worse than
            // everything here and is dropped; one that sorts before the last
            // level evicts the last level.
            if at >= self.len {
                self.overflowed += 1;
                return None;
            }
            self.overflowed += 1;
            self.shift_right(at, self.len - 1);
            self.px[at] = price;
            self.qty[at] = size;
            return Some(true);
        }
        self.shift_right(at, self.len);
        self.px[at] = price;
        self.qty[at] = size;
        self.len += 1;
        Some(true)
    }

    /// Move `[at, end)` one place to the right.
    #[inline]
    fn shift_right(&mut self, at: usize, end: usize) {
        if at < end {
            self.px.copy_within(at..end, at + 1);
            self.qty.copy_within(at..end, at + 1);
        }
    }

    /// Remove a level. Returns whether it was there.
    pub fn erase(&mut self, price: i64) -> bool {
        let (at, present) = self.locate(price);
        if !present {
            return false;
        }
        // Shift the tail left over the hole. `copy_within` is a memmove and the
        // compiler picks the widest move available.
        let rest = self.len - at - 1;
        if rest > 0 {
            self.px.copy_within(at + 1..self.len, at);
            self.qty.copy_within(at + 1..self.len, at);
        }
        self.len -= 1;
        true
    }

    /// Copy up to `n` levels into `out`, returning how many were written.
    ///
    /// This is the kernel stage 2 reshapes. In this form it is a loop over two
    /// contiguous `i64` slices writing into two more, with a bound that the
    /// compiler cannot always prove is inside `out` — see
    /// `docs/design/vec-optimization.md` §4 for what it emits and what the
    /// reshaped version costs.
    pub fn top(&self, n: usize, out: &mut [Level]) -> usize {
        let take = n.min(self.len).min(out.len());
        // Zip the three slices: the loop bound is then the shortest of them,
        // which the compiler can see, and no index arithmetic is left to
        // bounds-check.
        let prices = &self.px[..take];
        let sizes = &self.qty[..take];
        for ((slot, &px), &qty) in out.iter_mut().zip(prices).zip(sizes) {
            *slot = Level {
                px: Price::from_e8(px),
                qty: Qty::from_e8(qty),
            };
        }
        take
    }

    /// Sum the sizes of the best `n` levels into `out`, returning how many were
    /// summed.
    ///
    /// A prefix sum: `out[i]` is the total size available at the best `i + 1`
    /// levels. This is what a caller needs to answer "can I fill this size
    /// without walking past level `k`".
    ///
    /// # This is the sequential form, and that is a measured decision
    ///
    /// The obvious vectorization move is to block the loop so that the block
    /// totals and the block-offset adds become independent passes. That version
    /// was written, measured, and rejected: it is **2.5 to 3.2 times slower**
    /// than this loop at every size from 8 to 1024 levels.
    ///
    /// The reason is that this loop already runs at one `add` per element, which
    /// is the latency of a single add instruction, so there is no slack for a
    /// wider unit to win back. The blocked form pays for three passes over the
    /// data to reach the same answer.
    ///
    /// A prefix sum that is genuinely faster needs the within-block prefix
    /// computed with lane shuffles, which in Rust means `std::simd` on nightly or
    /// hand-written intrinsics. This crate takes neither, and says so rather than
    /// shipping a slower loop that looks modern. The rejected form is kept in
    /// `vecbench` as `experiment_cumulative_blocked` so the measurement stays
    /// reproducible; `docs/design/vec-optimization.md` §4.2 has the table.
    pub fn cumulative(&self, n: usize, out: &mut [i64]) -> usize {
        let take = n.min(self.len).min(out.len());
        let mut running = 0i64;
        for (slot, &qty) in out.iter_mut().zip(&self.qty[..take]) {
            running += qty;
            *slot = running;
        }
        take
    }

    /// Take `size` from the best levels, returning the volume-weighted price.
    ///
    /// Walks levels until the requested size is filled, accumulating size and
    /// notional. This is the execution model behind the slippage numbers: the
    /// difference between the best price and the price actually achieved is
    /// slippage caused by depth, and it is a real cost that a top-of-book-only
    /// calculation cannot see.
    ///
    /// Returns `None` when the side is empty.
    pub fn fill(&self, size: i64) -> Option<Fill> {
        if self.len == 0 || size <= 0 {
            return None;
        }
        let best = self.px[0];
        let mut filled = 0i64;
        let mut notional_e8 = 0i64;
        let mut levels = 0usize;
        for i in 0..self.len {
            if filled >= size {
                break;
            }
            let want = size - filled;
            let take = want.min(self.qty[i]);
            // Notional is price * size / E8, in e8-scaled currency. Done in i128
            // because price * size overflows i64 for any realistic pair.
            let product = (self.px[i] as i128) * (take as i128) / (crate::fixed::E8 as i128);
            notional_e8 = notional_e8.saturating_add(clamp_i64(product));
            filled += take;
            levels += 1;
        }
        if filled == 0 {
            return None;
        }
        Some(Fill {
            requested: size,
            filled,
            notional_e8,
            levels_used: levels,
            best_px: Price::from_e8(best),
            average_px: Price::from_e8(notional_e8.saturating_mul(crate::fixed::E8) / filled),
        })
    }
}

/// The result of taking liquidity from one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    /// The size asked for.
    pub requested: i64,
    /// The size actually available.
    pub filled: i64,
    /// Total traded value, in e8-scaled currency.
    pub notional_e8: i64,
    /// How many levels the walk consumed.
    pub levels_used: usize,
    /// The price of the first level.
    pub best_px: Price,
    /// The volume-weighted price of everything filled.
    pub average_px: Price,
}

impl Fill {
    /// Size that could not be filled because the side ran out.
    #[inline]
    pub fn unfilled(&self) -> i64 {
        (self.requested - self.filled).max(0)
    }

    /// Whether the whole request was filled.
    #[inline]
    pub fn complete(&self) -> bool {
        self.filled >= self.requested
    }

    /// How far the average price moved away from the best price, in hundredths
    /// of a basis point, signed so that a worse price is negative for a buyer
    /// and positive for a seller.
    ///
    /// The sign convention is applied by the caller, which knows which side it
    /// was taking.
    #[inline]
    pub fn depth_cost_bps_x100(&self) -> i64 {
        self.average_px.bps_x100_from(self.best_px.as_e8())
    }
}

/// Clamp an `i128` into `i64`.
#[inline]
fn clamp_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

/// One symbol's book: two sorted sides and the sequence that last changed them.
#[derive(Debug)]
pub struct Book {
    /// Bid levels, best (highest) first.
    pub bids: Levels,
    /// Ask levels, best (lowest) first.
    pub asks: Levels,
    /// The sequence number of the last record that changed a side.
    last_seq: u64,
    /// Records applied to this book.
    applied: u64,
}

impl Book {
    /// Create an empty book with room for `depth` levels on each side.
    pub fn new(depth: usize) -> Self {
        Book {
            bids: Levels::new(depth, true),
            asks: Levels::new(depth, false),
            last_seq: 0,
            applied: 0,
        }
    }

    /// Apply one record.
    ///
    /// The whole state machine:
    ///
    /// - `DepthReset` clears the named side and does **not** touch `last_seq`,
    ///   because a clear is not a new price. This matches the C++ book, and it
    ///   matters: a `depth5` picture sends a reset for each side before its
    ///   prices, and if a reset advanced the sequence a reader could not tell a
    ///   completed picture from a torn one.
    /// - `Depth` with size zero erases that price. This is how a diff feed
    ///   deletes a level.
    /// - `Depth` with a size inserts or replaces.
    /// - Anything else is ignored.
    pub fn apply(&mut self, m: &Msg) -> Applied {
        if !m.affects_book() {
            return Applied::Ignored;
        }
        let side = match m.side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
            Side::Both => return Applied::WrongSide,
        };
        self.applied += 1;
        if m.kind == MsgType::DepthReset {
            side.clear();
            return Applied::Reset(m.side);
        }
        if m.qty_e8 == 0 {
            side.erase(m.px_e8);
        } else {
            match side.upsert(m.px_e8, m.qty_e8) {
                Some(_) => {}
                None => {
                    self.last_seq = m.seq;
                    return Applied::Overflow(m.side);
                }
            }
        }
        self.last_seq = m.seq;
        Applied::Changed(m.side)
    }

    /// The sequence number of the last change.
    #[inline]
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// How many records have been applied.
    #[inline]
    pub fn applied(&self) -> u64 {
        self.applied
    }

    /// The best bid and best ask, if both sides are present.
    #[inline]
    pub fn inside(&self) -> Option<(Level, Level)> {
        match (self.bids.best(), self.asks.best()) {
            (Some(b), Some(a)) => Some((b, a)),
            _ => None,
        }
    }

    /// The midpoint of the inside, in e8-scaled units.
    ///
    /// `None` when either side is empty, rather than a made-up zero: a missing
    /// midpoint must not be mistaken for a price of zero, which would make every
    /// slippage number computed against it nonsense.
    #[inline]
    pub fn mid_e8(&self) -> Option<i64> {
        self.inside().map(|(b, a)| b.px.mid(a.px))
    }

    /// The full spread — ask minus bid — in hundredths of a basis point of the
    /// midpoint.
    ///
    /// This is the round-trip cost of crossing at the touch: a taker who buys at
    /// the ask and immediately sells at the bid pays this much. It is the
    /// reference against which [`crate::slippage`] measures what an execution
    /// actually cost.
    ///
    /// Integer division truncates, so a spread that is not a whole number of
    /// hundredths of a basis point reads low by less than one unit.
    #[inline]
    pub fn spread_bps_x100(&self) -> Option<i64> {
        let (b, a) = self.inside()?;
        let mid = b.px.mid(a.px);
        if mid == 0 {
            return None;
        }
        let spread = a.px.as_e8() - b.px.as_e8();
        Some(crate::fixed::delta_bps_x100(spread, mid))
    }

    /// Whether a bid is at or above an ask.
    ///
    /// A crossed book means a price that should have been cleared was not. It is
    /// a feed bug, not a sort bug, and the cache counts it so that a feed which
    /// starts crossing can be seen rather than argued about.
    #[inline]
    pub fn is_crossed(&self) -> bool {
        match (self.bids.best(), self.asks.best()) {
            (Some(b), Some(a)) => b.px.as_e8() >= a.px.as_e8(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::{Price, Qty};
    use crate::msg::Side;

    fn depth(side: Side, px: i64, qty: i64) -> Msg {
        Msg::depth(side, Price::from_e8(px), Qty::from_e8(qty), "BTCUSDT")
    }

    fn reset(side: Side) -> Msg {
        Msg::reset(side, "BTCUSDT")
    }

    #[test]
    fn a_bid_side_keeps_the_highest_price_first() {
        let mut l = Levels::new(8, true);
        for px in [100, 300, 200] {
            l.upsert(px, 1);
        }
        assert_eq!(l.prices(), &[300, 200, 100]);
        assert_eq!(l.best().unwrap().px.as_e8(), 300);
    }

    #[test]
    fn an_ask_side_keeps_the_lowest_price_first() {
        let mut l = Levels::new(8, false);
        for px in [300, 100, 200] {
            l.upsert(px, 1);
        }
        assert_eq!(l.prices(), &[100, 200, 300]);
        assert_eq!(l.best().unwrap().px.as_e8(), 100);
    }

    #[test]
    fn upsert_replaces_in_place() {
        let mut l = Levels::new(8, true);
        assert_eq!(l.upsert(100, 5), Some(true));
        assert_eq!(l.upsert(100, 9), Some(false));
        assert_eq!(l.len(), 1);
        assert_eq!(l.sizes(), &[9]);
    }

    #[test]
    fn erase_removes_and_reports() {
        let mut l = Levels::new(8, true);
        for px in [100, 200, 300] {
            l.upsert(px, 1);
        }
        assert!(l.erase(200));
        assert_eq!(l.prices(), &[300, 100]);
        assert!(
            !l.erase(200),
            "erasing a price that is not there is a no-op"
        );
        assert!(!l.erase(999));
    }

    #[test]
    fn a_full_side_drops_its_worst_level_and_counts_it() {
        let mut l = Levels::new(2, true);
        l.upsert(300, 1);
        l.upsert(200, 1);
        // Better than everything: it goes in and the worst is evicted.
        assert_eq!(l.upsert(400, 1), Some(true));
        assert_eq!(l.prices(), &[400, 300]);
        assert_eq!(l.overflowed(), 1);
        // Worse than everything: nothing changes, and it is still counted.
        assert_eq!(l.upsert(100, 1), None);
        assert_eq!(l.prices(), &[400, 300]);
        assert_eq!(l.overflowed(), 2);
    }

    #[test]
    fn top_copies_best_first_and_respects_the_output_length() {
        let mut l = Levels::new(8, false);
        for px in [500, 100, 300] {
            l.upsert(px, px);
        }
        let mut out = [Level::EMPTY; 4];
        assert_eq!(l.top(2, &mut out), 2);
        assert_eq!(out[0].px.as_e8(), 100);
        assert_eq!(out[1].px.as_e8(), 300);
        // A shorter output buffer is the limit, not the requested count.
        let mut two = [Level::EMPTY; 1];
        assert_eq!(l.top(10, &mut two), 1);
    }

    #[test]
    fn cumulative_depth_prefix_sums_the_sizes() {
        let mut l = Levels::new(8, false);
        for (px, qty) in [(100, 10), (200, 20), (300, 30)] {
            l.upsert(px, qty);
        }
        let mut out = [0i64; 4];
        assert_eq!(l.cumulative(3, &mut out), 3);
        assert_eq!(&out[..3], &[10, 30, 60]);
    }

    #[test]
    fn fill_walks_levels_and_reports_the_average_price() {
        let mut l = Levels::new(8, false);
        // 100.00 at 1.0, 101.00 at 1.0, 102.00 at 1.0.
        for (px, qty) in [
            (100 * crate::fixed::E8, crate::fixed::E8),
            (101 * crate::fixed::E8, crate::fixed::E8),
            (102 * crate::fixed::E8, crate::fixed::E8),
        ] {
            l.upsert(px, qty);
        }
        // Ask for 2.0: it takes both levels, average 100.50.
        let f = l.fill(2 * crate::fixed::E8).expect("fill");
        assert!(f.complete());
        assert_eq!(f.levels_used, 2);
        assert_eq!(f.best_px.as_e8(), 100 * crate::fixed::E8);
        assert_eq!(
            f.average_px.as_e8(),
            100 * crate::fixed::E8 + crate::fixed::E8 / 2
        );
        assert_eq!(f.unfilled(), 0);
        // Half a percent is fifty basis points, so 5000 in hundredths of one.
        assert_eq!(f.depth_cost_bps_x100(), 5000);
    }

    #[test]
    fn fill_reports_what_it_could_not_get() {
        let mut l = Levels::new(8, false);
        l.upsert(100 * crate::fixed::E8, crate::fixed::E8);
        let f = l.fill(3 * crate::fixed::E8).expect("fill");
        assert!(!f.complete());
        assert_eq!(f.filled, crate::fixed::E8);
        assert_eq!(f.unfilled(), 2 * crate::fixed::E8);
        assert_eq!(f.average_px.as_e8(), 100 * crate::fixed::E8);
        assert_eq!(f.depth_cost_bps_x100(), 0, "one level has no depth cost");
    }

    #[test]
    fn a_reset_clears_the_side_and_does_not_move_the_sequence() {
        let mut b = Book::new(8);
        let mut m = depth(Side::Bid, 100, 5);
        m.seq = 1;
        assert_eq!(b.apply(&m), Applied::Changed(Side::Bid));
        assert_eq!(b.last_seq(), 1);

        let mut r = reset(Side::Bid);
        r.seq = 2;
        assert_eq!(b.apply(&r), Applied::Reset(Side::Bid));
        assert!(b.bids.is_empty());
        assert_eq!(b.last_seq(), 1, "a clear is not a new price");
    }

    #[test]
    fn a_zero_size_deletes_a_level() {
        let mut b = Book::new(8);
        b.apply(&depth(Side::Ask, 100, 5));
        b.apply(&depth(Side::Ask, 101, 5));
        assert_eq!(b.asks.len(), 2);
        b.apply(&depth(Side::Ask, 100, 0));
        assert_eq!(b.asks.len(), 1);
        assert_eq!(b.asks.best().unwrap().px.as_e8(), 101);
    }

    #[test]
    fn heartbeats_and_control_records_are_ignored() {
        let mut b = Book::new(8);
        let h = Msg {
            kind: MsgType::Heartbeat,
            ..Msg::default()
        };
        assert_eq!(b.apply(&h), Applied::Ignored);
        let h = Msg {
            kind: MsgType::Cmd,
            ..Msg::default()
        };
        assert_eq!(b.apply(&h), Applied::Ignored);
        assert_eq!(b.applied(), 0);
    }

    #[test]
    fn a_side_of_both_is_refused_rather_than_guessed() {
        let mut b = Book::new(8);
        let mut m = depth(Side::Bid, 100, 5);
        m.side = Side::Both;
        assert_eq!(b.apply(&m), Applied::WrongSide);
        assert_eq!(b.bids.len(), 0);
        assert_eq!(b.asks.len(), 0);
    }

    #[test]
    fn the_inside_and_midpoint_need_both_sides() {
        let mut b = Book::new(8);
        b.apply(&depth(Side::Bid, 65000 * crate::fixed::E8, 1));
        assert_eq!(b.mid_e8(), None, "one side is not a market");
        b.apply(&depth(Side::Ask, 65001 * crate::fixed::E8, 1));
        assert_eq!(
            b.mid_e8(),
            Some(65000 * crate::fixed::E8 + crate::fixed::E8 / 2)
        );
        let (bid, ask) = b.inside().expect("inside");
        assert_eq!(bid.px.as_e8(), 65000 * crate::fixed::E8);
        assert_eq!(ask.px.as_e8(), 65001 * crate::fixed::E8);
        assert!(!b.is_crossed());
    }

    #[test]
    fn a_crossed_book_is_detected() {
        let mut b = Book::new(8);
        b.apply(&depth(Side::Bid, 65002 * crate::fixed::E8, 1));
        b.apply(&depth(Side::Ask, 65001 * crate::fixed::E8, 1));
        assert!(b.is_crossed());
    }

    #[test]
    fn a_depth5_picture_replaces_the_previous_one_exactly() {
        // The feed's actual shape: a reset for each side, then that side's
        // prices. The second picture is smaller than the first, so a stale
        // level would survive if the reset were not honoured.
        let mut b = Book::new(8);
        for (px, qty) in [(65000, 5), (64999, 12), (64990, 20)] {
            b.apply(&depth(
                Side::Bid,
                px * crate::fixed::E8,
                qty * crate::fixed::E8,
            ));
        }
        for (px, qty) in [(65001, 4), (65002, 8), (65010, 30)] {
            b.apply(&depth(
                Side::Ask,
                px * crate::fixed::E8,
                qty * crate::fixed::E8,
            ));
        }
        assert_eq!(b.bids.len(), 3);
        assert_eq!(b.asks.len(), 3);

        // Picture two: three bids become two.
        b.apply(&reset(Side::Bid));
        b.apply(&depth(
            Side::Bid,
            65000 * crate::fixed::E8,
            5 * crate::fixed::E8,
        ));
        b.apply(&depth(
            Side::Bid,
            64999 * crate::fixed::E8,
            11 * crate::fixed::E8,
        ));
        assert_eq!(b.bids.len(), 2);
        assert_eq!(
            b.bids.prices(),
            &[65000 * crate::fixed::E8, 64999 * crate::fixed::E8]
        );
        assert!(!b.is_crossed(), "65000 bid against 65001 ask");
    }

    #[test]
    fn the_spread_is_reported_in_hundredths_of_a_basis_point() {
        let mut b = Book::new(8);
        b.apply(&depth(Side::Bid, 100 * crate::fixed::E8, 1));
        b.apply(&depth(Side::Ask, 100 * crate::fixed::E8 + 1_000_000, 1)); // +0.01
                                                                           // A one-cent spread on a hundred-dollar mid is one basis point, which is
                                                                           // 100 in this unit. The midpoint is 100.005, so the exact value is
                                                                           // 99.995 and integer division reports 99.
        assert_eq!(b.spread_bps_x100(), Some(99));
    }
}
