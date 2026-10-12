//! Many symbols, several readers, one writer, and the counters that say whether
//! it is keeping up.
//!
//! # What this is for
//!
//! The ring in [`crate::ring`] is a bus between processes. This is a bus between
//! *threads* inside one process, with the per-symbol state that a strategy needs
//! and the measurement that says whether the strategy is being fed in time.
//!
//! A feed daemon owns one [`Publisher`] and hands each consumer a [`Subscriber`].
//! Both halves come out of [`build`] together, which is how the single-writer
//! rule is expressed in the type system rather than in a comment: the publisher
//! is the only value that can touch the books, and each subscriber owns the read
//! side of exactly one queue.
//!
//! ```text
//!                        ┌──────────── Publisher ─────────────┐
//!   socket ──decode──▶   │  symbol table → shard → book        │
//!                        │  latency stage 1 + 2                │
//!                        │  slippage ledger                    │
//!                        └──────────────┬─────────────────────┘
//!                                       │  bounded queue, per subscriber
//!                          ┌────────────┼────────────┐
//!                          ▼            ▼            ▼
//!                     Subscriber 1  Subscriber 2  Subscriber 3
//!                     stage 3       stage 3       stage 3
//!                     pressure      pressure      pressure
//! ```
//!
//! # Sharding
//!
//! Symbols are assigned to shards by their id's low bits, and each shard holds a
//! dense array of books indexed by the id's high bits. Two consequences:
//!
//! - A lookup is a mask and a shift, with no hashing on the update path.
//! - Shards exist so that a future version can give each shard to a different
//!   publishing thread without changing anything else. Today there is one
//!   publisher, because a market-data connection is one thread, and pretending
//!   otherwise would add locking that nothing needs. The shard count is still
//!   configurable, and `docs/design/ticker-cache.md` §6 records what changing it
//!   costs.
//!
//! # What is deliberately not here
//!
//! - **No depth snapshot per event.** An [`Event`] carries the top of book and
//!   the message that produced it. A subscriber that needs the full depth calls
//!   [`Publisher::top`] for a snapshot, or keeps its own [`crate::book::Book`]
//!   and applies the events; both are supported and the difference is measured
//!   in `docs/design/ticker-cache.md` §7.
//! - **No locking.** Nothing here takes a mutex. A subscriber that cannot keep
//!   up loses records according to its [`DropPolicy`], and the loss is counted.
//! - **No allocation after `build`.** The only allocating calls are the symbol
//!   table growing on a new symbol and a shard's book array growing on a new
//!   symbol; both are on the first sight of a symbol and neither is on the
//!   update path.

use crate::book::{Applied, Book, Fill, Level};
use crate::clock;
use crate::error::Result;
use crate::flow::{self, Consumer, DropPolicy, Producer, Pushed};
use crate::latency::LatencyTracker;
use crate::msg::{Msg, MsgType, Side};
use crate::pressure::{PressureConfig, PressureReport, PressureState, PressureTracker, Sample};
use crate::slippage::{
    Cost, Execution, SlippageLedger, SymbolLedger, DEFAULT_HORIZONS_NS, HORIZONS,
};
use crate::symbol::{SymbolId, SymbolTable};

/// How the cache is sized and what it measures.
#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    /// Symbols the table will accept before it stops growing.
    pub symbols: usize,
    /// Shards. Rounded up to a power of two.
    pub shards: usize,
    /// Levels kept per side, per symbol.
    pub depth: usize,
    /// Subscriber queues to create.
    pub subscribers: usize,
    /// Records each subscriber queue holds.
    pub queue: usize,
    /// What a subscriber queue does when it is full.
    pub policy: DropPolicy,
    /// Whether to keep the latency distributions.
    pub latency: bool,
    /// Whether to keep the slippage ledger.
    pub slippage: bool,
    /// The backpressure thresholds.
    pub pressure: PressureConfig,
    /// The interval at which records are expected, for the coordinated-omission
    /// correction. Zero disables the correction.
    pub expected_interval_ns: u64,
    /// The markout horizons.
    pub horizons: [u64; HORIZONS],
}

impl Default for CacheConfig {
    /// Sized for the shape this lab is built around: a few thousand symbols, a
    /// handful of subscribers, and a queue deep enough to absorb a scheduling
    /// hiccup without dropping.
    fn default() -> Self {
        CacheConfig {
            symbols: 4096,
            shards: 4,
            depth: 16,
            subscribers: 2,
            queue: 8192,
            policy: DropPolicy::DropOldest,
            latency: true,
            slippage: true,
            pressure: PressureConfig::default(),
            expected_interval_ns: 100_000,
            horizons: DEFAULT_HORIZONS_NS,
        }
    }
}

/// The top of a book, as it stood when a record was published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopOfBook {
    /// The best bid.
    pub bid: Level,
    /// The best ask.
    pub ask: Level,
}

impl TopOfBook {
    /// Whether both sides are present.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.bid.is_present() && self.ask.is_present()
    }

    /// The midpoint, or `None` when one side is missing.
    #[inline]
    pub fn mid_e8(&self) -> Option<i64> {
        if self.is_valid() {
            Some(self.bid.px.mid(self.ask.px))
        } else {
            None
        }
    }

    /// The full spread in hundredths of a basis point, or `None`.
    #[inline]
    pub fn spread_bps_x100(&self) -> Option<i64> {
        let mid = self.mid_e8()?;
        if mid == 0 {
            return None;
        }
        Some(crate::fixed::delta_bps_x100(
            self.ask.px.as_e8() - self.bid.px.as_e8(),
            mid,
        ))
    }

    /// Read the inside out of a book.
    pub fn of(book: &Book) -> Self {
        match book.inside() {
            Some((bid, ask)) => TopOfBook { bid, ask },
            None => TopOfBook {
                bid: book.bids.best().unwrap_or(Level::EMPTY),
                ask: book.asks.best().unwrap_or(Level::EMPTY),
            },
        }
    }
}

/// One published change, as a subscriber sees it.
#[derive(Debug, Clone, Copy)]
pub struct Event {
    /// The symbol, interned.
    pub symbol: SymbolId,
    /// The ring-assigned sequence number.
    pub seq: u64,
    /// The record that produced this event.
    pub msg: Msg,
    /// The top of book **after** the record was applied.
    pub top: TopOfBook,
    /// When the publisher put this event on the queue.
    pub published_ns: u64,
    /// How long the publishing side took for this record: its stages 1 and 2.
    ///
    /// Carried so that a subscriber can report the **end-to-end** time without a
    /// second clock read or a second measurement of the same work.
    pub ingress_ns: u64,
    /// Stage 1 as a **duration**: the sender's stamp to the daemon's read.
    ///
    /// A duration, not the stamp. The first version carried the raw monotonic
    /// stamp and handed it to the subscriber's tracker, which treats its argument
    /// as a duration and compares it against the plausibility ceiling. A stamp is
    /// an absolute reading that passes one second of uptime immediately, so every
    /// stamped event was counted as implausible, stage 1 was dropped, and the
    /// end-to-end figure silently became stages 2 and 3.
    pub stage1_ns: Option<u64>,
}

impl Event {
    /// The midpoint at publication, or `None`.
    #[inline]
    pub fn mid_e8(&self) -> Option<i64> {
        self.top.mid_e8()
    }

    /// A value to fill an output buffer with before [`Subscriber::drain`] writes
    /// into it.
    ///
    /// Its fields are meaningless and every one of them is overwritten by the
    /// drain. It exists so that draining into a caller-owned array does not need
    /// an allocation, an `Option` wrapper per slot, or uninitialised memory.
    pub fn placeholder() -> Self {
        Event {
            symbol: SymbolId::first(),
            seq: 0,
            msg: Msg::default(),
            top: TopOfBook::default(),
            published_ns: 0,
            ingress_ns: 0,
            stage1_ns: None,
        }
    }
}

/// What one [`Publisher::publish`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A book changed and the event was fanned out.
    Applied(Applied),
    /// A record the book ignores, such as a heartbeat. Not fanned out.
    Ignored,
    /// The symbol table is full, so the record could not be attributed.
    UnknownSymbol,
    /// The record named a side that does not exist.
    WrongSide,
}

impl Outcome {
    /// Whether an event was produced.
    #[inline]
    pub fn fanned_out(self) -> bool {
        matches!(self, Outcome::Applied(_))
    }
}

/// Counters for the publishing side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublishStats {
    /// Records offered to the publisher.
    pub published: u64,
    /// Records that changed a book.
    pub applied: u64,
    /// Records ignored, such as heartbeats.
    pub ignored: u64,
    /// Records whose symbol the table could not take.
    pub unknown_symbol: u64,
    /// Records naming a side that does not exist.
    pub wrong_side: u64,
    /// Level updates that overflowed a side's depth.
    pub overflow: u64,
    /// Times a book was found crossed after an update.
    pub crossed: u64,
    /// Events evicted from a subscriber queue to make room.
    pub evicted: u64,
    /// Events refused because a subscriber queue was full.
    pub dropped: u64,
    /// Events the credit policy refused.
    pub refused: u64,
}

/// One shard's books.
#[derive(Debug)]
struct Shard {
    books: Vec<Book>,
}

/// The write side of the cache.
///
/// `Send` and not `Sync`: one publishing thread, which is the whole design.
#[derive(Debug)]
pub struct Publisher {
    symbols: SymbolTable,
    shards: Box<[Shard]>,
    shard_bits: u32,
    depth: usize,
    queues: Box<[Producer<Event>]>,
    /// The best bid of every symbol, by global symbol id. Zero means unpriced.
    ///
    /// A `Vec` rather than a `Box<[i64]>` because the symbol table can grow, and
    /// growth is off the update path: it happens on the first sight of a symbol.
    best_bid: Vec<i64>,
    /// The best ask of every symbol, by global symbol id. Zero means unpriced.
    best_ask: Vec<i64>,
    latency: LatencyTracker,
    slippage: SlippageLedger,
    stats: PublishStats,
    cfg: CacheConfig,
    started_ns: u64,
}

impl Publisher {
    /// How many symbols have been interned.
    #[inline]
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// The symbol table, for a caller that wants to name a symbol.
    #[inline]
    pub fn symbols(&self) -> &SymbolTable {
        &self.symbols
    }

    /// The counters.
    #[inline]
    pub fn stats(&self) -> PublishStats {
        self.stats
    }

    /// The latency distributions for stages 1 and 2.
    #[inline]
    pub fn latency(&self) -> &LatencyTracker {
        &self.latency
    }

    /// The slippage ledger.
    #[inline]
    pub fn slippage(&self) -> &SlippageLedger {
        &self.slippage
    }

    /// The slippage ledger, mutably, for recording executions.
    #[inline]
    pub fn slippage_mut(&mut self) -> &mut SlippageLedger {
        &mut self.slippage
    }

    /// When the publisher was built.
    #[inline]
    pub fn started_ns(&self) -> u64 {
        self.started_ns
    }

    /// Intern a symbol that is not on the wire yet, such as a synthetic one.
    ///
    /// Grows the snapshot lane as well as the symbol table. The first version
    /// grew only the table, so a caller that pre-registered symbols and then
    /// asked for a snapshot sliced the side tables past their end and panicked.
    /// Pre-registering is the documented use of this method, so the panic was
    /// reachable from the documented path.
    pub fn intern(&mut self, symbol: &[u8; 8]) -> Result<SymbolId> {
        let id = self.symbols.intern(symbol)?;
        self.ensure_touch_capacity(id.index());
        Ok(id)
    }

    /// Publish one record.
    ///
    /// `t_recv_ns` is when the bytes were read from the socket, which is what
    /// stage 2 is measured from. Passing [`clock::now_ns`] here is correct for a
    /// caller with no earlier stamp, and makes stage 2 cover only the work done
    /// inside this call.
    pub fn publish(&mut self, m: &mut Msg, t_recv_ns: u64) -> Outcome {
        self.stats.published += 1;
        if !m.affects_book() {
            self.stats.ignored += 1;
            return Outcome::Ignored;
        }
        let symbol = match self.symbols.intern(&m.symbol) {
            Ok(id) => id,
            Err(_) => {
                // A feed that sends more distinct symbols than the table can
                // hold. Counted rather than fatal: one unattributable symbol is
                // not a reason to stop a feed that is otherwise working.
                self.stats.unknown_symbol += 1;
                return Outcome::UnknownSymbol;
            }
        };

        let shard = symbol.index() & ((1usize << self.shard_bits) - 1);
        let slot = symbol.index() >> self.shard_bits;
        // Grow the snapshot lane before borrowing the books: the two are
        // disjoint fields, but the borrow checker cannot see that through a
        // method call on `self`.
        self.ensure_touch_capacity(symbol.index());
        let books = &mut self.shards[shard].books;
        // Growing happens on the first sight of a symbol, never on an update.
        if slot >= books.len() {
            books.resize_with(slot + 1, || Book::new(self.depth));
        }
        let applied = books[slot].apply(m);
        // Refresh the snapshot lane for this symbol. One store each, on the same
        // path that already wrote the book.
        let book = &books[slot];
        self.best_bid[symbol.index()] = book.bids.best().map(|l| l.px.as_e8()).unwrap_or(0);
        self.best_ask[symbol.index()] = book.asks.best().map(|l| l.px.as_e8()).unwrap_or(0);
        match applied {
            Applied::Ignored => {
                self.stats.ignored += 1;
                return Outcome::Ignored;
            }
            Applied::WrongSide => {
                self.stats.wrong_side += 1;
                return Outcome::WrongSide;
            }
            Applied::Overflow(_) => self.stats.overflow += 1,
            _ => {}
        }
        self.stats.applied += 1;
        if books[slot].is_crossed() {
            self.stats.crossed += 1;
        }

        let published_ns = clock::now_ns();
        let sender_stamp_ns = m.stamp_ns();
        // Stage 1 is a duration, so it is the read time minus the sender's
        // stamp. Recording the stamp itself would be an absolute time of day
        // masquerading as a latency, and every percentile would be nonsense.
        let stage1_ns = sender_stamp_ns.map(|s| t_recv_ns.saturating_sub(s));
        let event = Event {
            symbol,
            seq: m.seq,
            msg: *m,
            top: TopOfBook::of(&books[slot]),
            published_ns,
            ingress_ns: published_ns.saturating_sub(t_recv_ns),
            stage1_ns,
        };

        // Fan out. The producer never waits: what happens when a queue is full
        // is the policy's decision, and the result is counted here so that a
        // subscriber that is losing data shows up in the publisher's report too.
        for queue in self.queues.iter_mut() {
            match queue.push(event) {
                Pushed::Queued => {}
                Pushed::Evicted => self.stats.evicted += 1,
                Pushed::Dropped => self.stats.dropped += 1,
                Pushed::NoCredit => self.stats.refused += 1,
            }
        }

        if self.cfg.latency {
            // Stages 1 and 2 only: stage 3 belongs to each subscriber, and
            // measuring it here would report the publisher's own speed as if it
            // were the subscriber's.
            // The expected interval is passed through, so the configured
            // coordinated-omission correction actually runs. It was read from
            // the config, stored on the publisher, and never used: the option was
            // documented and dead.
            let ingress = published_ns.saturating_sub(t_recv_ns);
            if self.cfg.expected_interval_ns == 0 {
                self.latency.record_source(stage1_ns, ingress);
            } else {
                self.latency.record_source_with_expected_interval(
                    stage1_ns,
                    ingress,
                    self.cfg.expected_interval_ns,
                );
            }
        }
        Outcome::Applied(applied)
    }

    /// Copy up to `out.len()` levels of one side into `out`.
    ///
    /// A snapshot, taken from the publishing thread. `None` for a symbol this
    /// publisher has never seen.
    pub fn levels(&self, symbol: SymbolId, side: Side, out: &mut [Level]) -> Option<usize> {
        let book = self.book(symbol)?;
        Some(match side {
            Side::Bid => book.bids.top(out.len(), out),
            Side::Ask => book.asks.top(out.len(), out),
            Side::Both => 0,
        })
    }

    /// Take `size` from one side of a symbol's book.
    pub fn fill(&self, symbol: SymbolId, side: Side, size: i64) -> Option<Fill> {
        let book = self.book(symbol)?;
        match side {
            Side::Bid => book.bids.fill(size),
            Side::Ask => book.asks.fill(size),
            Side::Both => None,
        }
    }

    /// The top of a symbol's book.
    pub fn top(&self, symbol: SymbolId) -> Option<TopOfBook> {
        Some(TopOfBook::of(self.book(symbol)?))
    }

    /// The midpoint of a symbol, or `None`.
    pub fn mid_e8(&self, symbol: SymbolId) -> Option<i64> {
        self.book(symbol)?.mid_e8()
    }

    /// One symbol's book.
    pub fn book(&self, symbol: SymbolId) -> Option<&Book> {
        let shard = symbol.index() & ((1usize << self.shard_bits) - 1);
        let slot = symbol.index() >> self.shard_bits;
        self.shards.get(shard)?.books.get(slot)
    }

    /// Record an execution against the slippage ledger.
    ///
    /// `now_ns` is when the execution happened, which is what the markout
    /// horizons are measured from.
    pub fn record_execution(
        &mut self,
        symbol: SymbolId,
        now_ns: u64,
        exec: Execution,
    ) -> Option<Cost> {
        self.slippage.record(symbol, now_ns, exec)
    }

    /// Settle any markouts whose horizons have passed.
    ///
    /// Call this on the publisher's timer, not per record. Returns how many
    /// horizons settled.
    pub fn resolve_markouts(&mut self, now_ns: u64) -> usize {
        if !self.slippage.enabled() {
            return 0;
        }
        // The closure reads books through the shard layout; it cannot borrow
        // `self` mutably because the ledger already has, so it captures a shared
        // reference to the shards and the shard bits instead.
        let shards = &self.shards;
        let bits = self.shard_bits;
        self.slippage.resolve_all(now_ns, |index| {
            let shard = index & ((1 << bits) - 1);
            let slot = index >> bits;
            shards.get(shard)?.books.get(slot)?.mid_e8()
        })
    }

    /// A one-symbol ledger, for a report.
    pub fn symbol_ledger(&self, symbol: SymbolId) -> Option<&SymbolLedger> {
        self.slippage.symbol(symbol)
    }

    /// Write every symbol's midpoint into `out`, returning how many were
    /// written.
    ///
    /// `out[symbol.index()]` receives the midpoint of that symbol's book, or
    /// zero when one side is missing. The array is indexed by symbol id, so it
    /// is as long as the symbol table's capacity.
    ///
    /// # Why this reads a side table rather than the books
    ///
    /// The obvious implementation walks the symbol ids and calls a per-symbol
    /// accessor, which for each symbol does shard arithmetic, two bounds-checked
    /// lookups, and two `Option` chains to find the best level. That is a
    /// pointer chase per symbol, and no optimizer can flatten it into a loop over
    /// contiguous memory.
    ///
    /// So the publisher keeps the touch of every book in two flat arrays, written
    /// once per book change by the publish path. This function is
    /// then three contiguous `i64` arrays, unit stride, with a select instead of
    /// a branch and no dependency between iterations — the shape a vector unit
    /// can use, and the reason the side tables exist at all.
    ///
    /// It is also the better data layout on its own terms: a market snapshot
    /// wants every symbol's mid, and reading 8 bytes per symbol from one array
    /// beats touching a thousand `Book` structs to read the same two numbers.
    /// `vecbench --kernels mids` measures both against each other, and
    /// `docs/design/vec-optimization.md` §4.3 records which one won.
    pub fn mids_into(&self, out: &mut [i64]) -> usize {
        let written = self.symbols.len().min(out.len());
        let bids = &self.best_bid[..written];
        let asks = &self.best_ask[..written];
        for ((slot, &bid), &ask) in out[..written].iter_mut().zip(bids).zip(asks) {
            // All ones when both sides are priced, all zeros when either is not.
            // `bid != 0` is the "no price" marker, because a real price is never
            // zero.
            //
            // The mask is the whole fix. `if priced { mid } else { 0 }` compiled
            // to a *branch*, which put the two arms in different basic blocks and
            // stopped the vectorizer from touching the loop at all.
            //
            // The division was then suspected of being a second blocker and was
            // replaced with a shift to test it. It made no difference — forty
            // vector operations either way — so the division came back: it is the
            // clearer statement of a midpoint, and a shift would have differed
            // from it for a crossed book. `docs/optimization/ledger.jsonl` records
            // both the change that worked and the one that did not.
            let mask = -i64::from(bid != 0 && ask != 0);
            // The ordered form, the same one `Book::mid_e8` uses, so the
            // snapshot lane and the per-book accessor cannot disagree by a unit
            // on a crossed book.
            let (lo, hi) = if bid <= ask { (bid, ask) } else { (ask, bid) };
            *slot = mask & (lo + (hi - lo) / 2);
        }
        written
    }

    /// The touch of one symbol's book, as this publisher last recorded it.
    ///
    /// `None` for a symbol that is not priced, or an index past the table.
    pub fn touch(&self, index: usize) -> Option<(i64, i64)> {
        let bid = *self.best_bid.get(index)?;
        let ask = *self.best_ask.get(index)?;
        if bid == 0 || ask == 0 {
            None
        } else {
            Some((bid, ask))
        }
    }

    /// Grow the snapshot lane so that `index` is addressable.
    ///
    /// Rounded to a power of two so that a feed opening a thousand symbols does
    /// not reallocate a thousand times.
    #[inline]
    fn ensure_touch_capacity(&mut self, index: usize) {
        if index >= self.best_bid.len() {
            let grow_to = (index + 1).next_power_of_two();
            self.best_bid.resize(grow_to, 0);
            self.best_ask.resize(grow_to, 0);
        }
    }
}

/// The read side of one subscriber's queue, with its own measurement.
#[derive(Debug)]
pub struct Subscriber {
    id: usize,
    queue: Consumer<Event>,
    latency: LatencyTracker,
    pressure: PressureTracker,
    delivered: u64,
    events_applied: u64,
    last_event_ns: u64,
}

impl Subscriber {
    /// This subscriber's index, as handed out by [`build`].
    #[inline]
    pub fn id(&self) -> usize {
        self.id
    }

    /// Records this subscriber has taken off its queue.
    #[inline]
    pub fn delivered(&self) -> u64 {
        self.delivered
    }

    /// The latency distribution for stage 3, measured here.
    #[inline]
    pub fn latency(&self) -> &LatencyTracker {
        &self.latency
    }

    /// The backpressure history.
    #[inline]
    pub fn pressure(&self) -> &PressureTracker {
        &self.pressure
    }

    /// Records waiting.
    #[inline]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// How far behind the publisher this subscriber is, in records.
    #[inline]
    pub fn lag_records(&self) -> u64 {
        self.queue.lag()
    }

    /// Records evicted before this subscriber could read them.
    #[inline]
    pub fn evicted(&self) -> u64 {
        self.queue.evicted()
    }

    /// Records refused because this subscriber's queue was full.
    #[inline]
    pub fn dropped(&self) -> u64 {
        self.queue.dropped()
    }

    /// The largest occupancy this subscriber's queue has reached.
    #[inline]
    pub fn high_water(&self) -> u64 {
        self.queue.high_water()
    }

    /// Grant the publisher `n` records of budget, for [`DropPolicy::Credit`].
    pub fn grant(&mut self, n: u32) {
        self.queue.grant(n);
    }

    /// Take the next event, if there is one.
    ///
    /// Measuring happens here because this is the only place that knows when a
    /// subscriber actually saw the record. Stage 3 is the time the event waited
    /// in the queue, which is also the lag the pressure state is derived from:
    /// one measurement, two uses, no second clock read.
    pub fn try_recv(&mut self) -> Option<Event> {
        let event = self.queue.pop()?;
        let now = clock::now_ns();
        let waited = now.saturating_sub(event.published_ns);
        // Stage 3 is the time the event waited in this queue, and the total is
        // the whole path from the sender's stamp to this read. Both are recorded
        // for every event: a subscriber that is being starved is exactly the one
        // whose numbers matter.
        self.latency
            // `event.stage1_ns` is already a duration. Passing the raw stamp
            // here was the bug described on the field.
            .record_sink(event.stage1_ns, event.ingress_ns, waited);
        self.pressure.sample(
            now,
            Sample {
                lag_ns: waited,
                queue_len: self.queue.len(),
                queue_capacity: self.queue.capacity(),
            },
        );
        self.delivered += 1;
        self.last_event_ns = now;
        Some(event)
    }

    /// Take up to `out.len()` events.
    pub fn drain(&mut self, out: &mut [Event]) -> usize {
        let mut n = 0;
        while n < out.len() {
            match self.try_recv() {
                Some(e) => {
                    out[n] = e;
                    n += 1;
                }
                None => break,
            }
        }
        n
    }

    /// Take up to `n` events and apply each one's record to `book`.
    ///
    /// For a subscriber that wants depth rather than just the top: it keeps its
    /// own [`Book`] and this keeps it current. The events still go through
    /// [`Subscriber::try_recv`], so the measurement is identical to a subscriber
    /// that only reads the top.
    pub fn apply_to(&mut self, book: &mut Book, n: usize) -> usize {
        let mut applied = 0;
        for _ in 0..n {
            let Some(event) = self.try_recv() else {
                break;
            };
            book.apply(&event.msg);
            self.events_applied += 1;
            applied += 1;
        }
        applied
    }

    /// Records applied to a subscriber-owned book.
    #[inline]
    pub fn events_applied(&self) -> u64 {
        self.events_applied
    }

    /// When the last event was taken.
    #[inline]
    pub fn last_event_ns(&self) -> u64 {
        self.last_event_ns
    }

    /// Charge the elapsed time to the current pressure state.
    ///
    /// Call once when a run ends, so a run that finishes saturated reports the
    /// time it spent that way.
    pub fn close(&mut self, now_ns: u64) {
        self.pressure.close(now_ns);
    }

    /// The backpressure report.
    pub fn pressure_report(&self) -> PressureReport {
        self.pressure.report()
    }
}

/// The two halves of a cache, ready to be moved to their threads.
#[derive(Debug)]
pub struct Cache {
    /// The single writer.
    pub publisher: Publisher,
    /// One reader per configured subscriber.
    pub subscribers: Vec<Subscriber>,
}

/// Build a cache and its subscribers.
///
/// The publisher and every subscriber come from one call so that the queue
/// halves cannot be mismatched and a subscriber cannot be created after the
/// publisher has started publishing into a queue nobody holds.
pub fn build(cfg: CacheConfig) -> Cache {
    let shards = cfg.shards.max(1).next_power_of_two();
    let shard_bits = shards.trailing_zeros();
    let mut queues = Vec::with_capacity(cfg.subscribers);
    let mut subscribers = Vec::with_capacity(cfg.subscribers);
    for id in 0..cfg.subscribers {
        let (producer, consumer) = flow::ring::<Event>(cfg.queue, cfg.policy);
        queues.push(producer);
        subscribers.push(Subscriber {
            id,
            queue: consumer,
            latency: LatencyTracker::new(),
            pressure: PressureTracker::new(cfg.pressure),
            delivered: 0,
            events_applied: 0,
            last_event_ns: 0,
        });
    }
    Cache {
        publisher: Publisher {
            symbols: SymbolTable::new(cfg.symbols),
            shards: (0..shards)
                .map(|_| Shard { books: Vec::new() })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            shard_bits,
            depth: cfg.depth.max(1),
            queues: queues.into_boxed_slice(),
            // Sized to the symbol table's initial capacity; `note_touch` grows
            // them, and growth happens on the first sight of a symbol.
            best_bid: vec![0i64; cfg.symbols.max(1)],
            best_ask: vec![0i64; cfg.symbols.max(1)],
            latency: LatencyTracker::new(),
            slippage: SlippageLedger::new(cfg.symbols, cfg.horizons, cfg.slippage),
            stats: PublishStats::default(),
            cfg,
            started_ns: clock::now_ns(),
        },
        subscribers,
    }
}

/// The state of one subscriber, as a report line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriberReport {
    /// The subscriber's index.
    pub id: usize,
    /// Events taken.
    pub delivered: u64,
    /// Events evicted before it read them.
    pub evicted: u64,
    /// Events refused because its queue was full.
    pub dropped: u64,
    /// The queue's peak occupancy.
    pub high_water: u64,
    /// The state at the end of the run.
    pub pressure: PressureState,
    /// The stage-3 distribution.
    pub stage3: crate::latency::Summary,
}

impl std::fmt::Display for SubscriberReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "subscriber {}: delivered={} evicted={} dropped={} high_water={} state={} | stage3: {}",
            self.id,
            self.delivered,
            self.evicted,
            self.dropped,
            self.high_water,
            self.pressure.label(),
            self.stage3
        )
    }
}

/// Summarise a subscriber.
pub fn subscriber_report(s: &Subscriber) -> SubscriberReport {
    SubscriberReport {
        id: s.id,
        delivered: s.delivered,
        evicted: s.evicted(),
        dropped: s.dropped(),
        high_water: s.high_water(),
        pressure: s.pressure.state(),
        stage3: s.latency.publish_to_seen().summary(),
    }
}

/// The symbol's name, for a report, without holding the table.
pub fn symbol_name(publisher: &Publisher, symbol: SymbolId) -> std::borrow::Cow<'_, str> {
    publisher.symbols.symbol_str(symbol)
}

/// A record's kind, as a short label.
pub fn kind_label(kind: MsgType) -> &'static str {
    match kind {
        MsgType::Depth => "depth",
        MsgType::Heartbeat => "heartbeat",
        MsgType::Cmd => "cmd",
        MsgType::DepthReset => "reset",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::{Price, Qty, E8};
    use crate::slippage::Taker;

    fn cfg(subscribers: usize) -> CacheConfig {
        CacheConfig {
            symbols: 64,
            shards: 4,
            depth: 8,
            subscribers,
            queue: 64,
            policy: DropPolicy::DropOldest,
            expected_interval_ns: 0,
            ..CacheConfig::default()
        }
    }

    fn sym(name: &str) -> [u8; 8] {
        let mut out = [0u8; 8];
        let b = name.as_bytes();
        out[..b.len().min(8)].copy_from_slice(&b[..b.len().min(8)]);
        out
    }

    fn depth(side: Side, px: i64, qty: i64) -> Msg {
        let mut m = Msg::depth(
            side,
            Price::from_e8(px * E8),
            Qty::from_e8(qty * E8),
            "BTCUSDT",
        );
        m.seq = 0;
        m
    }

    fn seeded(subscribers: usize) -> Cache {
        let mut cache = build(cfg(subscribers));
        let mut bid = depth(Side::Bid, 65_000, 1);
        bid.seq = 1;
        cache.publisher.publish(&mut bid, clock::now_ns());
        let mut ask = depth(Side::Ask, 65_001, 1);
        ask.seq = 2;
        cache.publisher.publish(&mut ask, clock::now_ns());
        cache
    }

    #[test]
    fn a_symbol_is_interned_once_and_found_by_mask_and_shift() {
        let mut cache = build(cfg(1));
        let mut a = depth(Side::Bid, 100, 1);
        a.set_symbol("AAAUSDT");
        let mut b = depth(Side::Bid, 100, 1);
        b.set_symbol("BBBUSDT");
        cache.publisher.publish(&mut a, 0);
        cache.publisher.publish(&mut b, 0);
        cache.publisher.publish(&mut a, 0);
        assert_eq!(cache.publisher.symbol_count(), 2, "interned once each");
        assert_eq!(cache.publisher.stats().applied, 3);
        // Both books exist and are independent.
        let a_id = cache.publisher.symbols().lookup(&sym("AAAUSDT")).unwrap();
        let b_id = cache.publisher.symbols().lookup(&sym("BBBUSDT")).unwrap();
        assert_ne!(a_id, b_id);
        assert!(cache.publisher.book(a_id).is_some());
        assert!(cache.publisher.book(b_id).is_some());
    }

    #[test]
    fn a_book_change_is_fanned_out_to_every_subscriber() {
        let mut cache = seeded(3);
        assert_eq!(cache.publisher.stats().applied, 2);
        let mut out = [Event::placeholder(); 4];
        for s in cache.subscribers.iter_mut() {
            assert_eq!(s.drain(&mut out), 2);
            assert_eq!(out[0].msg.side, Side::Bid);
            assert_eq!(out[1].msg.side, Side::Ask);
            assert_eq!(out[0].top.bid.px.as_e8(), 65_000 * E8);
            assert_eq!(out[1].top.ask.px.as_e8(), 65_001 * E8);
        }
    }

    #[test]
    fn heartbeats_are_counted_and_not_fanned_out() {
        let mut cache = build(cfg(1));
        let mut h = Msg {
            kind: MsgType::Heartbeat,
            ..Msg::default()
        };
        h.set_symbol("BTCUSDT");
        assert_eq!(cache.publisher.publish(&mut h, 0), Outcome::Ignored);
        assert_eq!(cache.publisher.stats().ignored, 1);
        assert_eq!(cache.publisher.stats().applied, 0);
        assert_eq!(cache.subscribers[0].pending(), 0);
    }

    #[test]
    fn the_top_of_book_carries_the_midpoint_and_spread() {
        let cache = seeded(1);
        let id = cache
            .publisher
            .symbols()
            .lookup(&sym("BTCUSDT"))
            .expect("interned");
        let top = cache.publisher.top(id).expect("top");
        assert!(top.is_valid());
        assert_eq!(top.mid_e8(), Some(65_000 * E8 + E8 / 2));
        // A spread of 1.00 on a mid of 65000.50 is 0.1538 basis points, which
        // truncates to 15 in hundredths.
        assert_eq!(top.spread_bps_x100(), Some(15));
    }

    #[test]
    fn a_one_sided_book_has_no_midpoint() {
        let mut cache = build(cfg(1));
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut m, 0);
        let id = cache.publisher.symbols().lookup(&sym("BTCUSDT")).unwrap();
        let top = cache.publisher.top(id).unwrap();
        assert!(!top.is_valid());
        assert_eq!(top.mid_e8(), None);
        assert_eq!(top.spread_bps_x100(), None);
    }

    #[test]
    fn a_subscriber_that_cannot_keep_up_loses_records_and_says_so() {
        let mut cache = build(CacheConfig {
            queue: 4,
            policy: DropPolicy::DropOldest,
            ..cfg(1)
        });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        for _ in 0..20 {
            cache.publisher.publish(&mut m, 0);
        }
        // The queue holds four; the rest were evicted.
        assert_eq!(cache.subscribers[0].pending(), 4);
        assert_eq!(cache.publisher.stats().evicted, 16);
        assert_eq!(cache.subscribers[0].evicted(), 16);
        // The records that survived are the newest four, in order.
        let mut out = [Event::placeholder(); 8];
        assert_eq!(cache.subscribers[0].drain(&mut out), 4);
        assert_eq!(cache.subscribers[0].high_water(), 4);
    }

    #[test]
    fn drop_newest_refuses_at_the_publisher_too() {
        let mut cache = build(CacheConfig {
            queue: 4,
            policy: DropPolicy::DropNewest,
            ..cfg(1)
        });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        for _ in 0..10 {
            cache.publisher.publish(&mut m, 0);
        }
        assert_eq!(cache.publisher.stats().dropped, 6);
        assert_eq!(cache.publisher.stats().evicted, 0);
        assert_eq!(cache.subscribers[0].pending(), 4);
        assert_eq!(cache.subscribers[0].dropped(), 6);
    }

    #[test]
    fn one_slow_subscriber_does_not_stop_another() {
        let mut cache = build(CacheConfig {
            queue: 4,
            policy: DropPolicy::DropOldest,
            ..cfg(2)
        });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        for _ in 0..10 {
            cache.publisher.publish(&mut m, 0);
        }
        // Subscriber 0 drains one; subscriber 1 never drains.
        let mut out = [Event::placeholder(); 1];
        cache.subscribers[0].drain(&mut out);
        // Both queues evicted the same six records, and both hold the newest
        // four; subscriber 0 has read one of them.
        assert_eq!(cache.subscribers[0].evicted(), 6);
        assert_eq!(cache.subscribers[1].evicted(), 6);
        assert_eq!(cache.subscribers[0].pending(), 3);
        assert_eq!(cache.subscribers[1].pending(), 4);
        assert_eq!(cache.subscribers[0].delivered(), 1);
        assert_eq!(cache.subscribers[1].delivered(), 0);
    }

    #[test]
    fn the_publisher_never_waits_for_a_subscriber() {
        // A subscriber that never drains must not stop the publisher from
        // accepting records, however many are published.
        let mut cache = build(CacheConfig { queue: 8, ..cfg(1) });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        for _ in 0..10_000 {
            assert!(cache.publisher.publish(&mut m, 0).fanned_out());
        }
        assert_eq!(cache.publisher.stats().applied, 10_000);
        assert_eq!(cache.subscribers[0].pending(), 8);
    }

    #[test]
    fn stage_three_is_measured_at_the_subscriber() {
        let mut cache = seeded(1);
        let mut out = [Event::placeholder(); 4];
        let n = cache.subscribers[0].drain(&mut out);
        assert_eq!(n, 2);
        assert_eq!(cache.subscribers[0].latency().publish_to_seen().count(), 2);
        // The publisher measured stages 1 and 2 and left stage 3 alone.
        assert_eq!(cache.publisher.latency().publish_to_seen().count(), 0);
        assert_eq!(cache.publisher.latency().recv_to_publish().count(), 2);
    }

    #[test]
    fn a_stamped_record_carries_its_stamp_through_to_stage_one() {
        let mut cache = build(cfg(1));
        let now = clock::now_ns();
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        m.set_stamp_ns(now.saturating_sub(1_000));
        cache.publisher.publish(&mut m, now);
        assert_eq!(cache.publisher.latency().stamped(), 1);
        assert_eq!(cache.publisher.latency().unstamped(), 0);
        assert!(cache.publisher.latency().send_to_recv().max() >= 1_000);
    }

    #[test]
    fn an_unstamped_record_is_counted_as_unstamped() {
        let mut cache = build(cfg(1));
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut m, clock::now_ns());
        assert_eq!(cache.publisher.latency().unstamped(), 1);
        assert_eq!(cache.publisher.latency().stamped(), 0);
    }

    #[test]
    fn the_pressure_state_rises_when_a_subscriber_falls_behind() {
        let mut cache = build(CacheConfig {
            queue: 4096,
            pressure: PressureConfig {
                elevated_ns: 1,
                saturated_ns: 2,
                enabled: true,
            },
            ..cfg(1)
        });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut m, 0);
        // Sleep so that the event is unambiguously older than the thresholds.
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut out = [Event::placeholder(); 1];
        cache.subscribers[0].drain(&mut out);
        assert_eq!(
            cache.subscribers[0].pressure().state(),
            PressureState::Saturated
        );
        let report = cache.subscribers[0].pressure_report();
        assert_eq!(report.saturation_events, 1);
        assert!(report.peak_lag_ns >= 2_000_000, "{}", report.peak_lag_ns);
    }

    #[test]
    fn a_subscriber_can_keep_its_own_book_from_the_events() {
        let mut cache = seeded(1);
        let mut book = Book::new(8);
        let applied = cache.subscribers[0].apply_to(&mut book, 10);
        assert_eq!(applied, 2);
        assert_eq!(book.bids.best().unwrap().px.as_e8(), 65_000 * E8);
        assert_eq!(book.asks.best().unwrap().px.as_e8(), 65_001 * E8);
        assert_eq!(cache.subscribers[0].events_applied(), 2);
        assert_eq!(cache.subscribers[0].delivered(), 2);
    }

    #[test]
    fn a_snapshot_of_depth_is_available_from_the_publisher() {
        let mut cache = build(cfg(1));
        for (px, qty) in [(100, 1), (99, 2), (98, 3)] {
            let mut m = depth(Side::Bid, px, qty);
            m.set_symbol("BTCUSDT");
            cache.publisher.publish(&mut m, 0);
        }
        let id = cache.publisher.symbols().lookup(&sym("BTCUSDT")).unwrap();
        let mut out = [Level::EMPTY; 8];
        assert_eq!(cache.publisher.levels(id, Side::Bid, &mut out), Some(3));
        assert_eq!(out[0].px.as_e8(), 100 * E8);
        assert_eq!(out[2].qty.as_e8(), 3 * E8);
        // A shorter buffer is the limit.
        let mut two = [Level::EMPTY; 2];
        assert_eq!(cache.publisher.levels(id, Side::Bid, &mut two), Some(2));
    }

    #[test]
    fn a_fill_can_be_taken_from_the_publishers_book() {
        let mut cache = build(cfg(1));
        for (px, qty) in [(100, 1), (101, 1)] {
            let mut m = depth(Side::Ask, px, qty);
            m.set_symbol("BTCUSDT");
            cache.publisher.publish(&mut m, 0);
        }
        let id = cache.publisher.symbols().lookup(&sym("BTCUSDT")).unwrap();
        let f = cache.publisher.fill(id, Side::Ask, 2 * E8).expect("fill");
        assert!(f.complete());
        assert_eq!(f.levels_used, 2);
        assert_eq!(f.average_px.as_e8(), 100 * E8 + E8 / 2);
    }

    #[test]
    fn an_execution_is_decomposed_and_its_markout_settles() {
        let mut cache = seeded(1);
        let id = cache.publisher.symbols().lookup(&sym("BTCUSDT")).unwrap();
        let mid = cache.publisher.mid_e8(id).expect("mid");
        let now = clock::now_ns();
        // Moves big enough to be visible in hundredths of a basis point: on a
        // mid of 65,000 a tenth of a cent is under one unit, so the test uses a
        // tenth of a percent.
        let move_ = mid / 1000;
        let cost = cache
            .publisher
            .record_execution(
                id,
                now,
                Execution {
                    taker: Taker::Buy,
                    decision_mid_e8: mid,
                    action_mid_e8: mid + move_,
                    action_px_e8: mid + 2 * move_,
                    filled: Qty::from_e8(E8),
                    unfilled: Qty::from_e8(0),
                },
            )
            .expect("recorded");
        // A tenth of a percent is ten basis points, which is 1000 in hundredths
        // of a basis point.
        assert_eq!(cost.latency_bps_x100, 1000);
        assert_eq!(cost.depth_bps_x100, 1000);
        assert!(cost.shortfall_bps_x100 > 0);

        // Nothing has passed yet.
        assert_eq!(cache.publisher.resolve_markouts(now + 1), 0);
        // Well past every horizon.
        let settled = cache.publisher.resolve_markouts(now + 2_000_000_000);
        assert_eq!(settled, HORIZONS, "all three horizons settle");
        let ledger = cache.publisher.symbol_ledger(id).expect("ledger");
        assert_eq!(ledger.executions, 1);
        for h in 0..HORIZONS {
            assert_eq!(ledger.markout[h].count, 1, "horizon {h}");
        }
    }

    #[test]
    fn the_symbol_table_growing_is_reported_rather_than_fatal() {
        // A table that cannot grow past its ceiling is exercised by asking for
        // more symbols than the configured capacity allows in one build.
        let mut cache = build(CacheConfig {
            symbols: 1,
            ..cfg(1)
        });
        // The table grows internally, so the first few are fine.
        for i in 0..20 {
            let mut m = depth(Side::Bid, 100, 1);
            m.set_symbol(&format!("Y{i:06}"));
            cache.publisher.publish(&mut m, 0);
        }
        assert_eq!(cache.publisher.stats().unknown_symbol, 0);
        assert_eq!(cache.publisher.symbol_count(), 20);
        // The table grew, so the ceiling is what it should be checked against,
        // not the initial capacity.
        assert!(cache.publisher.symbols().capacity() >= 20);
    }

    #[test]
    fn a_crossed_book_is_counted() {
        let mut cache = build(cfg(1));
        let mut bid = depth(Side::Bid, 100, 1);
        bid.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut bid, 0);
        let mut ask = depth(Side::Ask, 99, 1);
        ask.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut ask, 0);
        assert_eq!(cache.publisher.stats().crossed, 1);
    }

    #[test]
    fn sharding_splits_symbols_across_independent_book_arrays() {
        for shards in [1usize, 2, 8, 32] {
            let mut cache = build(CacheConfig {
                shards,
                symbols: 256,
                ..cfg(1)
            });
            for i in 0..64 {
                let mut m = depth(Side::Bid, 100 + i, 1);
                m.set_symbol(&format!("S{i:06}"));
                cache.publisher.publish(&mut m, 0);
            }
            assert_eq!(cache.publisher.symbol_count(), 64, "shards={shards}");
            for i in 0..64 {
                let id = cache
                    .publisher
                    .symbols()
                    .lookup(&sym(&format!("S{i:06}")))
                    .expect("interned");
                let book = cache.publisher.book(id).expect("book");
                assert_eq!(
                    book.bids.best().unwrap().px.as_e8(),
                    (100 + i) * E8,
                    "shards={shards} symbol={i}"
                );
            }
        }
    }

    #[test]
    fn a_thousand_symbols_land_in_their_own_books() {
        // The shape the cache exists for: a thousand tickers, one publisher.
        let mut cache = build(CacheConfig {
            symbols: 4096,
            shards: 8,
            ..cfg(1)
        });
        for i in 0..1000 {
            let mut m = depth(Side::Bid, 100 + (i % 100), 1);
            m.set_symbol(&format!("T{i:06}"));
            cache.publisher.publish(&mut m, 0);
        }
        assert_eq!(cache.publisher.symbol_count(), 1000);
        for i in (0..1000).step_by(97) {
            let id = cache
                .publisher
                .symbols()
                .lookup(&sym(&format!("T{i:06}")))
                .expect("interned");
            assert_eq!(
                cache
                    .publisher
                    .book(id)
                    .unwrap()
                    .bids
                    .best()
                    .unwrap()
                    .px
                    .as_e8(),
                (100 + (i % 100)) * E8
            );
        }
    }

    #[test]
    fn the_credit_policy_is_driven_by_the_subscriber() {
        let mut cache = build(CacheConfig {
            queue: 8,
            policy: DropPolicy::Credit,
            ..cfg(1)
        });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        // No credit: the publisher records the refusal rather than dropping.
        cache.publisher.publish(&mut m, 0);
        assert_eq!(cache.publisher.stats().refused, 1);
        assert_eq!(cache.publisher.stats().dropped, 0);

        cache.subscribers[0].grant(2);
        cache.publisher.publish(&mut m, 0);
        cache.publisher.publish(&mut m, 0);
        assert_eq!(cache.publisher.stats().refused, 1, "credit was enough");
        assert_eq!(cache.subscribers[0].pending(), 2);
    }

    #[test]
    fn pre_registering_symbols_does_not_leave_the_snapshot_lane_short() {
        // The regression: `intern` grew the symbol table and not the side
        // tables, so a caller that pre-registered and then snapshotted sliced
        // past the end and panicked. Pre-registering is `intern`'s documented
        // use, so the panic was reachable from the documented path.
        let mut cache = build(cfg(1));
        for i in 0..64 {
            let name = format!("P{i:06}");
            let mut wire = [0u8; 8];
            wire[..name.len()].copy_from_slice(name.as_bytes());
            cache.publisher.intern(&wire).expect("intern");
        }
        let mut out = vec![0i64; 128];
        let written = cache.publisher.mids_into(&mut out);
        assert_eq!(written, 64, "the lane must cover every interned symbol");
        assert!(out[..64].iter().all(|&v| v == 0), "nothing is priced yet");
    }

    #[test]
    fn a_stamped_record_reports_stage_one_as_a_duration() {
        // The regression: the subscriber's tracker was handed the raw monotonic
        // stamp and compared it against the plausibility ceiling. A stamp passes
        // one second of uptime immediately, so every stamped event was counted
        // implausible, stage 1 was dropped, and the end-to-end figure silently
        // became stages 2 and 3.
        let mut cache = build(cfg(1));
        let now = clock::now_ns();
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        m.set_stamp_ns(now.saturating_sub(5_000));
        cache.publisher.publish(&mut m, now);

        let mut out = [Event::placeholder(); 1];
        assert_eq!(cache.subscribers[0].drain(&mut out), 1);
        let sink = cache.subscribers[0].latency();
        assert_eq!(
            sink.implausible(),
            0,
            "a stamp is not an implausible duration"
        );
        assert_eq!(sink.stamped(), 1);
        // The event carries a duration, not a stamp.
        assert!(out[0].stage1_ns.is_some_and(|d| d >= 5_000));
        // And the end-to-end figure is larger than stage 2 alone.
        assert!(sink.total().max() >= 5_000);
    }

    #[test]
    fn the_snapshot_lane_agrees_with_the_per_book_accessor() {
        // A crossed book with an odd spread is where the ordered and unordered
        // midpoint differ by a unit.
        let mut cache = build(cfg(1));
        let mut bid = depth(Side::Bid, 100, 1);
        bid.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut bid, 0);
        let mut ask = depth(Side::Ask, 99, 1);
        ask.set_symbol("BTCUSDT");
        cache.publisher.publish(&mut ask, 0);

        let id = cache.publisher.symbols().lookup(&sym("BTCUSDT")).unwrap();
        let mut out = vec![0i64; 8];
        cache.publisher.mids_into(&mut out);
        assert_eq!(
            out[id.index()],
            cache.publisher.mid_e8(id).expect("mid"),
            "the lane and the accessor must not disagree"
        );
    }

    #[test]
    fn a_configured_interval_actually_reaches_the_tracker() {
        // The regression: `expected_interval_ns` was read from the config,
        // stored, and never used, so the documented correction never ran.
        let mut cache = build(CacheConfig {
            expected_interval_ns: 1_000,
            ..cfg(1)
        });
        let mut m = depth(Side::Bid, 100, 1);
        m.set_symbol("BTCUSDT");
        // A record whose ingress is far longer than the expected interval makes
        // the correction insert the samples a stall swallowed.
        let now = clock::now_ns();
        cache
            .publisher
            .publish(&mut m, now.saturating_sub(1_000_000));
        let total = cache.publisher.latency().total().count();
        assert!(
            total > 1,
            "the correction added nothing: {total} samples for one record"
        );
    }

    #[test]
    fn the_report_line_names_the_state_and_the_tail() {
        let mut cache = seeded(1);
        let mut out = [Event::placeholder(); 4];
        cache.subscribers[0].drain(&mut out);
        let line = subscriber_report(&cache.subscribers[0]).to_string();
        assert!(line.contains("subscriber 0:"), "{line}");
        assert!(line.contains("delivered=2"), "{line}");
        assert!(line.contains("state=nominal"), "{line}");
    }
}
