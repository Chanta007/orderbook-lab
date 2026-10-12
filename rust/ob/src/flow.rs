//! A bounded queue between one producer and one consumer, with a policy for
//! what happens when it fills.
//!
//! # Why this is not a `channel`
//!
//! A feed handler's fan-out has three properties that `std::sync::mpsc` does
//! not give:
//!
//! 1. **It must never block the producer.** The producer is reading a socket.
//!    If it waits for a slow subscriber, the socket buffer fills, TCP applies
//!    back-pressure to the exchange, and the latency of every other subscriber
//!    becomes the latency of the slowest one. [`DropPolicy`] makes that choice
//!    explicit instead of hiding it behind a lock.
//! 2. **The loss has to be counted.** Dropping records silently is how a system
//!    looks healthy while feeding its readers stale prices. Every policy here
//!    increments a counter and a high-water mark that [`crate::pressure`] turns
//!    into a state.
//! 3. **The consumer must be able to see how far behind it is.** [`Consumer::lag`]
//!    answers that without touching the producer.
//!
//! # The policies
//!
//! | Policy | Behaviour when full | Use it when |
//! |--------|--------------------|-------------|
//! | [`DropPolicy::DropOldest`] | evict the oldest record, keep the newest | the newest price is the one that matters (a book display, a signal) |
//! | [`DropPolicy::DropNewest`] | refuse the new record, keep what is queued | every record must be seen in order (an audit sink, a strategy that counts) |
//! | [`DropPolicy::Spin`] | spin briefly, then refuse | the consumer is known to be microseconds behind |
//! | [`DropPolicy::Credit`] | refuse unless the consumer has granted credit | the consumer wants to control its own inflow |
//!
//! The default for a market-data fan-out is `DropOldest`, and the reasoning is
//! in `docs/design/ticker-cache.md` §5: a subscriber that is 200 ms behind does
//! not want 200 ms of history, it wants the current price.
//!
//! # Safety
//!
//! One producer, one consumer, and the two halves are produced by
//! [`ring`] as a pair. The producer writes a slot and then release-stores the
//! head index; the consumer acquire-loads the head index and only then reads the
//! slot. That pairing is what makes reading a slot that is not yet initialised
//! impossible in the normal path.
//!
//! `DropOldest` is the one policy where the consumer's own index is also written
//! by the producer, because evicting means moving the read position forward. Both
//! sides advance it with a compare-and-exchange loop, so an eviction and a read
//! cannot both claim the same slot.

use std::cell::{Cell, UnsafeCell};
use std::hint::spin_loop;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// What to do when the queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DropPolicy {
    /// Evict the oldest queued record to make room. Counted as an eviction.
    ///
    /// The default for a market-data fan-out: a subscriber that is behind wants
    /// the current price, not the history it missed.
    #[default]
    DropOldest,
    /// Refuse the new record. Counted as a drop.
    DropNewest,
    /// Spin up to `spins` times waiting for room, then refuse. Counted as a
    /// drop only when the spins run out.
    Spin {
        /// How many times to re-check before giving up.
        spins: u32,
    },
    /// Refuse unless the consumer has granted credit. Refusals are counted
    /// separately from drops, because the caller is expected to retry.
    Credit,
}

/// What one [`Producer::push`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// The record is queued.
    Queued,
    /// The queue was full and the oldest queued record was evicted.
    Evicted,
    /// The queue was full and the new record was refused.
    Dropped,
    /// The credit policy refused the record; the caller may retry.
    NoCredit,
}

impl Pushed {
    /// Whether the record is in the queue after this call.
    #[inline]
    pub fn accepted(self) -> bool {
        matches!(self, Pushed::Queued | Pushed::Evicted)
    }
}

/// The shared state. Never used directly; see [`Producer`] and [`Consumer`].
#[derive(Debug)]
struct Shared<T: Copy> {
    /// Slots, indexed by `position & mask`. A slot is only read after the head
    /// index says it has been written.
    slots: UnsafeCell<Box<[MaybeUninit<T>]>>,
    /// Next position to write. Monotonic; the slot index is this masked.
    head: AtomicU64,
    /// Next position to read. The consumer claims a slot by advancing this.
    tail: AtomicU64,
    /// Slot sequences. `seq[p & mask]` holds `p + 1` once position `p` has been
    /// written, and an older lap's value until then.
    ///
    /// The consumer checks it before and after copying a slot, which is what
    /// detects a slot the producer overwrote mid-copy. See the note on
    /// [`Consumer::pop`].
    seq: UnsafeCell<Box<[AtomicU64]>>,
    /// Records evicted to make room.
    evicted: AtomicU64,
    /// Records refused.
    dropped: AtomicU64,
    /// Refusals under the credit policy.
    refused: AtomicU64,
    /// The largest `head - tail` ever observed, in records.
    high_water: AtomicU64,
    /// Consumer-granted budget, used only by [`DropPolicy::Credit`].
    credit: AtomicU32,
    /// The mask for the slot index.
    mask: u64,
    /// Set once a push has ever had to reuse a slot. Used by the tests.
    reused: AtomicU64,
}

// SAFETY: this is the SPSC contract. `Shared` is reachable from exactly two
// threads — the one holding the `Producer` and the one holding the `Consumer` —
// and the two touch disjoint fields in the normal path: the producer writes
// `head`, `slots[head & mask]`, `evicted`, `dropped`, `refused` and
// `high_water`; the consumer writes `tail` and `credit` and reads `head`. The
// only shared mutation is `tail` under `DropOldest`, which both sides advance
// with compare-and-exchange. `T: Copy` means a slot is never dropped and there
// is no destructor to run twice.
unsafe impl<T: Copy + Send> Sync for Shared<T> {}
// SAFETY: as above; moving either half between threads moves its access rights.
unsafe impl<T: Copy + Send> Send for Shared<T> {}

/// The write half of a queue.
///
/// `Send` but not `Sync`: moving a producer to another thread moves the write
/// right with it, while sharing one between threads would give two producers and
/// break the invariant the slot writes rely on. The `PhantomData<Cell<()>>` is
/// what removes the automatic `Sync` that `Arc<Shared<T>>` would otherwise
/// confer.
#[derive(Debug)]
pub struct Producer<T: Copy> {
    shared: Arc<Shared<T>>,
    policy: DropPolicy,
    _not_sync: PhantomData<Cell<()>>,
}

/// The read half of a queue.
///
/// `Send` but not `Sync`, for the same reason on the reading side.
#[derive(Debug)]
pub struct Consumer<T: Copy> {
    shared: Arc<Shared<T>>,
    _not_sync: PhantomData<Cell<()>>,
}

/// Create a queue with `capacity` slots, rounded up to a power of two.
///
/// Returns the two halves. The producer is `Send` and the consumer is `Send`;
/// neither is `Sync`, which is what stops two threads sharing one end.
pub fn ring<T: Copy>(capacity: usize, policy: DropPolicy) -> (Producer<T>, Consumer<T>) {
    let capacity = capacity.max(2).next_power_of_two();
    let mut slots = Vec::with_capacity(capacity);
    slots.resize_with(capacity, || MaybeUninit::uninit());
    let shared = Arc::new(Shared {
        slots: UnsafeCell::new(slots.into_boxed_slice()),
        seq: UnsafeCell::new(
            (0..capacity)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        head: AtomicU64::new(0),
        tail: AtomicU64::new(0),
        evicted: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        refused: AtomicU64::new(0),
        high_water: AtomicU64::new(0),
        credit: AtomicU32::new(0),
        mask: (capacity - 1) as u64,
        reused: AtomicU64::new(0),
    });
    (
        Producer {
            shared: Arc::clone(&shared),
            policy,
            _not_sync: PhantomData,
        },
        Consumer {
            shared,
            _not_sync: PhantomData,
        },
    )
}

impl<T: Copy> Producer<T> {
    /// Slots in this queue.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.shared.mask + 1) as usize
    }

    /// The policy this producer applies when full.
    #[inline]
    pub fn policy(&self) -> DropPolicy {
        self.policy
    }

    /// How many records are queued.
    #[inline]
    pub fn len(&self) -> usize {
        let head = self.shared.head.load(Ordering::Relaxed);
        let tail = self.shared.tail.load(Ordering::Relaxed);
        head.saturating_sub(tail) as usize
    }

    /// Whether the queue is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The largest occupancy ever seen, in records.
    ///
    /// This is the number that says whether the queue was ever close to full.
    /// A queue that has never been more than a tenth full does not need to be
    /// bigger, and one that has hit its capacity is losing records.
    #[inline]
    pub fn high_water(&self) -> u64 {
        self.shared.high_water.load(Ordering::Relaxed)
    }

    /// Records evicted to make room.
    #[inline]
    pub fn evicted(&self) -> u64 {
        self.shared.evicted.load(Ordering::Relaxed)
    }

    /// Records refused.
    #[inline]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Refusals under the credit policy, which the caller may retry.
    #[inline]
    pub fn refused(&self) -> u64 {
        self.shared.refused.load(Ordering::Relaxed)
    }

    /// How many pushes had to reuse a slot. Zero unless the queue filled.
    ///
    /// The eviction path is the one the slot sequence exists for, so a test that
    /// claims to cover it has to show the path was reached.
    #[inline]
    pub fn reused(&self) -> u64 {
        self.shared.reused.load(Ordering::Relaxed)
    }

    /// Offer one record to the queue.
    ///
    /// Never blocks for longer than the policy allows, and always leaves the
    /// queue in a consistent state. See [`Pushed`] for what happened.
    pub fn push(&mut self, value: T) -> Pushed {
        if self.policy == DropPolicy::Credit && self.shared.credit.load(Ordering::Acquire) == 0 {
            self.shared.refused.fetch_add(1, Ordering::Relaxed);
            return Pushed::NoCredit;
        }

        let mut spins_left = match self.policy {
            DropPolicy::Spin { spins } => spins,
            _ => 0,
        };
        loop {
            let head = self.shared.head.load(Ordering::Relaxed);
            let tail = self.shared.tail.load(Ordering::Acquire);
            let occupancy = head.wrapping_sub(tail);
            let capacity = self.shared.mask + 1;

            if occupancy < capacity {
                self.write_slot(head, value);
                self.shared.head.store(head + 1, Ordering::Release);
                self.shared
                    .high_water
                    .fetch_max(occupancy + 1, Ordering::Relaxed);
                if self.policy == DropPolicy::Credit {
                    self.shared.credit.fetch_sub(1, Ordering::AcqRel);
                }
                return Pushed::Queued;
            }

            match self.policy {
                DropPolicy::DropNewest => {
                    self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    return Pushed::Dropped;
                }
                DropPolicy::Credit => {
                    self.shared.refused.fetch_add(1, Ordering::Relaxed);
                    return Pushed::NoCredit;
                }
                DropPolicy::Spin { .. } => {
                    if spins_left == 0 {
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                        return Pushed::Dropped;
                    }
                    spins_left -= 1;
                    spin_loop();
                    continue;
                }
                DropPolicy::DropOldest => {
                    // Claim the oldest record for eviction. The compare-and-
                    // exchange is what makes this safe against a consumer that
                    // claimed the same slot first: if it fails, the consumer won
                    // and this loop retries from the new tail. A consumer that is
                    // mid-copy is handled by the slot sequence, not by waiting:
                    // the producer must never be held up by a slow subscriber,
                    // which is the whole point of this policy.
                    self.shared.reused.fetch_add(1, Ordering::Relaxed);
                    if self
                        .shared
                        .tail
                        .compare_exchange_weak(tail, tail + 1, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok()
                    {
                        self.shared.evicted.fetch_add(1, Ordering::Relaxed);
                        // `head` is unchanged; the slot for it is the slot the
                        // evicted record occupied, because the ring is a power
                        // of two and `head - tail == capacity` means
                        // `head & mask == tail & mask`.
                        self.write_slot(head, value);
                        self.shared.head.store(head + 1, Ordering::Release);
                        return Pushed::Evicted;
                    }
                    continue;
                }
            }
        }
    }

    /// Write one slot.
    ///
    /// # Safety-adjacent note
    ///
    /// The caller has established that no consumer can be reading this slot:
    /// either it is not yet published (head not advanced), or it has just been
    /// evicted (tail advanced past it).
    #[inline]
    fn write_slot(&self, position: u64, value: T) {
        let index = (position & self.shared.mask) as usize;
        // SAFETY: `slots` is a boxed slice of `capacity` elements, `index` is
        // masked to `capacity`, and `self` is the only producer, so no other
        // thread writes this element. The slot is `MaybeUninit`, so writing
        // through the pointer does not read uninitialised memory.
        unsafe {
            let base = (*self.shared.slots.get()).as_mut_ptr();
            base.add(index).write(MaybeUninit::new(value));
        }
        // Publish the payload. A reader that copied a mix of two laps sees this
        // change and rejects the copy. No invalidating store is needed first:
        // the sequence for position `p` is `p + 1`, which no earlier write to
        // this slot can have produced, so a stale slot never looks current.
        unsafe {
            let seq = (*self.shared.seq.get()).as_ptr();
            (*seq.add(index)).store(position + 1, Ordering::Release);
        }
    }
}

impl<T: Copy> Consumer<T> {
    /// Slots in this queue.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.shared.mask + 1) as usize
    }

    /// How many records are waiting.
    #[inline]
    pub fn len(&self) -> usize {
        let head = self.shared.head.load(Ordering::Acquire);
        let tail = self.shared.tail.load(Ordering::Relaxed);
        head.saturating_sub(tail) as usize
    }

    /// Whether nothing is waiting.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take the next record, if there is one.
    ///
    /// Advances the read position with a compare-and-exchange, so an eviction
    /// racing with this read cannot cause the same slot to be claimed twice.
    pub fn pop(&mut self) -> Option<T> {
        loop {
            let tail = self.shared.tail.load(Ordering::Relaxed);
            let head = self.shared.head.load(Ordering::Acquire);
            if tail == head {
                return None;
            }
            let index = (tail & self.shared.mask) as usize;
            // The slot's own sequence is the guard. `seq == tail + 1` means this
            // slot holds the record for `tail`. If the producer overwrites the
            // slot while this copy is in flight, the sequence becomes a later
            // position's and the second read below disagrees, so the copy is
            // discarded. Without this the `DropOldest` path could hand back a mix
            // of two laps' bytes, which is a data race and was undefined
            // behaviour.
            //
            // SAFETY: `index` is masked to the capacity, and `self` is the only
            // consumer, so the load of the sequence is ordered before the payload
            // copy and the producer's release store is what makes the payload
            // visible.
            let before = unsafe {
                let seq = (*self.shared.seq.get()).as_ptr();
                (*seq.add(index)).load(Ordering::Acquire)
            };
            if before != tail + 1 {
                // Either the producer is mid-write, or this is a stale slot from
                // an earlier lap. Both mean "look again".
                std::hint::spin_loop();
                continue;
            }
            // SAFETY: the sequence above matched, so the payload for `tail` was
            // written and released before this load.
            let value = unsafe {
                let base = (*self.shared.slots.get()).as_ptr();
                (*base.add(index)).assume_init()
            };
            // SAFETY: as above.
            let after = unsafe {
                let seq = (*self.shared.seq.get()).as_ptr();
                (*seq.add(index)).load(Ordering::Acquire)
            };
            if after != before {
                // The producer overwrote the slot during the copy. The value just
                // read is a mix of two records and is dropped.
                continue;
            }
            if self
                .shared
                .tail
                .compare_exchange_weak(tail, tail + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Some(value);
            }
            // The producer evicted this slot while we were reading it. The value
            // we read is from a slot that has been reused, so it is dropped and
            // the loop retries from the new tail.
        }
    }

    /// How far behind the producer this consumer is, in records.
    #[inline]
    pub fn lag(&self) -> u64 {
        let head = self.shared.head.load(Ordering::Acquire);
        let tail = self.shared.tail.load(Ordering::Relaxed);
        head.saturating_sub(tail)
    }

    /// Records evicted before this consumer could read them.
    #[inline]
    pub fn evicted(&self) -> u64 {
        self.shared.evicted.load(Ordering::Relaxed)
    }

    /// Records the producer refused because the queue was full.
    #[inline]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// The largest occupancy ever seen.
    #[inline]
    pub fn high_water(&self) -> u64 {
        self.shared.high_water.load(Ordering::Relaxed)
    }

    /// Grant `n` records of budget to the producer, for [`DropPolicy::Credit`].
    ///
    /// Credit is the one policy where the consumer decides its own inflow: the
    /// producer cannot queue anything the consumer has not paid for. A consumer
    /// grants credit after it has finished with a batch, which turns the queue
    /// into a window rather than a buffer.
    pub fn grant(&self, n: u32) {
        self.shared.credit.fetch_add(n, Ordering::AcqRel);
    }

    /// Remaining credit.
    #[inline]
    pub fn credit(&self) -> u32 {
        self.shared.credit.load(Ordering::Acquire)
    }

    /// Drain up to `out.len()` records, returning how many were written.
    pub fn drain(&mut self, out: &mut [T]) -> usize {
        let mut n = 0;
        while n < out.len() {
            match self.pop() {
                Some(v) => {
                    out[n] = v;
                    n += 1;
                }
                None => break,
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_order_is_preserved() {
        let (mut p, mut c) = ring::<u64>(4, DropPolicy::DropOldest);
        for i in 0..3 {
            assert_eq!(p.push(i), Pushed::Queued);
        }
        assert_eq!(c.len(), 3);
        for i in 0..3 {
            assert_eq!(c.pop(), Some(i));
        }
        assert_eq!(c.pop(), None);
    }

    #[test]
    fn capacity_rounds_up_to_a_power_of_two() {
        let (p, c) = ring::<u64>(100, DropPolicy::DropOldest);
        assert_eq!(p.capacity(), 128);
        assert_eq!(c.capacity(), 128);
    }

    #[test]
    fn drop_newest_keeps_the_oldest_and_counts_the_loss() {
        let (mut p, mut c) = ring::<u64>(4, DropPolicy::DropNewest);
        for i in 0..4 {
            assert_eq!(p.push(i), Pushed::Queued);
        }
        assert_eq!(p.push(99), Pushed::Dropped);
        assert_eq!(p.dropped(), 1);
        assert_eq!(p.evicted(), 0);
        assert_eq!(c.dropped(), 1);
        // The queued records are the first four, in order.
        let mut out = [0u64; 8];
        assert_eq!(c.drain(&mut out), 4);
        assert_eq!(&out[..4], &[0, 1, 2, 3]);
    }

    #[test]
    fn drop_oldest_keeps_the_newest_and_counts_the_eviction() {
        let (mut p, mut c) = ring::<u64>(4, DropPolicy::DropOldest);
        for i in 0..4 {
            assert_eq!(p.push(i), Pushed::Queued);
        }
        assert_eq!(p.push(4), Pushed::Evicted);
        assert_eq!(p.evicted(), 1);
        assert_eq!(p.dropped(), 0);
        // The oldest record is gone; the newest four survive in order.
        let mut out = [0u64; 8];
        assert_eq!(c.drain(&mut out), 4);
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
        assert_eq!(c.evicted(), 1);
    }

    #[test]
    fn high_water_records_the_closest_call() {
        let (mut p, mut c) = ring::<u64>(4, DropPolicy::DropNewest);
        assert_eq!(p.high_water(), 0);
        for i in 0..3 {
            p.push(i);
        }
        assert_eq!(p.high_water(), 3);
        assert_eq!(c.pop(), Some(0));
        p.push(9);
        assert_eq!(
            p.high_water(),
            3,
            "the peak does not fall when the queue drains"
        );
        assert_eq!(c.high_water(), 3);
    }

    #[test]
    fn spin_refuses_only_after_its_budget() {
        let (mut p, mut c) = ring::<u64>(2, DropPolicy::Spin { spins: 3 });
        assert_eq!(p.push(1), Pushed::Queued);
        assert_eq!(p.push(2), Pushed::Queued);
        // Full, and nothing will free it: the spins run out and the record is
        // dropped rather than the producer waiting forever.
        assert_eq!(p.push(3), Pushed::Dropped);
        assert_eq!(p.dropped(), 1);
        assert_eq!(c.pop(), Some(1));
    }

    #[test]
    fn credit_is_a_window_the_consumer_controls() {
        let (mut p, mut c) = ring::<u64>(8, DropPolicy::Credit);
        // No credit granted: nothing may be queued, and the refusal is not a
        // drop, because the caller is meant to try again.
        assert_eq!(p.push(1), Pushed::NoCredit);
        assert_eq!(p.refused(), 1);
        assert_eq!(p.dropped(), 0);

        c.grant(2);
        assert_eq!(c.credit(), 2);
        assert_eq!(p.push(1), Pushed::Queued);
        assert_eq!(p.push(2), Pushed::Queued);
        assert_eq!(p.push(3), Pushed::NoCredit);
        assert_eq!(c.credit(), 0);

        // Draining does not return credit by itself; the consumer decides.
        assert_eq!(c.pop(), Some(1));
        assert_eq!(p.push(4), Pushed::NoCredit);
        c.grant(1);
        assert_eq!(p.push(4), Pushed::Queued);
    }

    #[test]
    fn lag_reports_the_gap_without_touching_the_producer() {
        let (mut p, c) = ring::<u64>(8, DropPolicy::DropNewest);
        assert_eq!(c.lag(), 0);
        for i in 0..5 {
            p.push(i);
        }
        assert_eq!(c.lag(), 5);
        assert_eq!(p.len(), 5);
    }

    #[test]
    fn an_eviction_cannot_hand_back_a_mixed_record() {
        // The regression the independent review found. A consumer copying a slot
        // while the producer evicts it used to read a mix of two records: the
        // sequence field of the old one and the payload of the new one. The
        // slot's own sequence now catches it.
        //
        // The test drives the case that makes the race reachable — a full queue
        // and a producer that keeps pushing while the consumer reads — and
        // asserts the invariant that matters: every value returned is one that
        // was pushed, and the values are non-decreasing.
        let (mut producer, mut consumer) = ring::<(u64, u64, u64, u64)>(4, DropPolicy::DropOldest);
        let writer = std::thread::spawn(move || {
            for i in 0..200_000u64 {
                // Four words so the copy spans several stores, which is what
                // makes a torn read observable.
                producer.push((i, i ^ 0xAAAA, i.wrapping_mul(3), i.wrapping_add(7)));
            }
            producer
        });

        let mut seen = 0u64;
        let mut last: Option<u64> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match consumer.pop() {
                Some((a, b, c, d)) => {
                    // Every word must belong to the same push.
                    assert_eq!(b, a ^ 0xAAAA, "torn record: first word {a}, second {b}");
                    assert_eq!(c, a.wrapping_mul(3), "torn record at word three: {a}");
                    assert_eq!(d, a.wrapping_add(7), "torn record at word four: {a}");
                    if let Some(previous) = last {
                        assert!(a > previous, "out of order: {previous} then {a}");
                    }
                    last = Some(a);
                    seen += 1;
                }
                None => {
                    if last.is_some_and(|v| v == 199_999) {
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "stalled at {seen}");
                    std::thread::yield_now();
                }
            }
        }
        let producer = writer.join().expect("writer thread");
        assert!(
            producer.reused() > 0,
            "the test never reached the eviction path"
        );
        assert!(seen > 0);
    }

    #[test]
    fn a_thread_can_consume_what_another_thread_produced() {
        const N: u64 = 200_000;
        let (mut p, mut c) = ring::<u64>(1024, DropPolicy::DropOldest);
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                p.push(i);
            }
            p
        });
        // Read everything that arrives, checking order. With DropOldest and a
        // producer that outruns the consumer, values are skipped but never
        // reordered, and the last value must be the last one pushed.
        let mut seen = 0u64;
        let mut last: Option<u64> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match c.pop() {
                Some(v) => {
                    if let Some(prev) = last {
                        assert!(v > prev, "out of order: {prev} then {v}");
                    }
                    last = Some(v);
                    seen += 1;
                }
                None => {
                    if seen > 0 && last == Some(N - 1) {
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "stalled at {seen}");
                    std::thread::yield_now();
                }
            }
        }
        let p = producer.join().expect("producer thread");
        assert!(seen <= N);
        // The producer never blocked: it either queued or evicted every record.
        assert_eq!(p.evicted() + seen, N);
        assert_eq!(p.dropped(), 0);
    }
}
