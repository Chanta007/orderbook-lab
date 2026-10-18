//! The shared-memory bus: one writer, many readers, no locks.
//!
//! # The shape
//!
//! A file, mapped `MAP_SHARED`, holding a sixteen-byte header and a fixed
//! number of fixed-size slots:
//!
//! ```text
//!   offset 0    magic      u32   0x4F424C42
//!   offset 4    capacity   u32   slots in the ring, a power of two
//!   offset 8    wseq       u64   how many records have been published
//!   offset 16   slot 0     72 bytes
//!   offset 88   slot 1     72 bytes
//!   ...
//! ```
//!
//! The header is the same three fields, in the same order, as the C++
//! `RingHeader`, and a slot holds the same 72 bytes the C++ version writes. The
//! two implementations can therefore read each other's rings, and
//! `make rust-interop` proves it rather than asserting it.
//!
//! # The protocol, and why it is safe
//!
//! Exactly one process writes. It:
//!
//! 1. reads `wseq` to get the next sequence number,
//! 2. writes the record into slot `wseq & (capacity - 1)`,
//! 3. **releases** `wseq = wseq + 1`.
//!
//! Any number of processes read. A reader:
//!
//! 1. **acquires** `wseq`,
//! 2. if its cursor is behind, reads the slot for that cursor,
//! 3. checks the record's own `seq` field matches the cursor.
//!
//! The release in step 3 of the writer and the acquire in step 1 of the reader
//! are the entire synchronisation. A reader that observes the incremented
//! `wseq` is guaranteed to observe the bytes of the record that was written
//! before it. No lock is taken on either side, and neither side waits for the
//! other.
//!
//! # Why the reader checks the record's own sequence
//!
//! The ring is bounded, so a slot is reused. Two ways that matters, and the
//! second is the one that is easy to miss:
//!
//! 1. A reader that falls more than `capacity` records behind finds its slot
//!    overwritten. The `wseq` load catches this: the difference is greater than
//!    the capacity, and the reader skips forward.
//! 2. A reader whose lag was *within* the capacity when it loaded `wseq` can
//!    still have its slot overwritten before it finishes reading it, because a
//!    writer is free to publish `capacity` more records in that window. The
//!    `wseq` load cannot catch this — it was already taken.
//!
//! The record's own `seq` field catches both. It is written as part of the same
//! slot the reader is copying, so if it does not equal the sequence the reader
//! asked for, the slot belongs to a different lap. This is the same check the
//! C++ reader makes, it is reachable in normal operation, and it is the reason a
//! reader that loses a race loses a record rather than reading a wrong price.
//!
//! # Why the reader opens read-only
//!
//! The C++ reader opens the ring `O_RDWR` because one constructor serves both
//! roles. Here the roles are separate types, so [`RingReader`] maps the file
//! `PROT_READ`. A terminal that has no write access to the bus cannot corrupt it
//! even with a bug, and the type system is what enforces that.

use crate::codec;
use crate::error::{Error, Result};
use crate::msg::{Msg, MAGIC, MSG_SIZE};
use crate::sys::Mapping;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes before the first slot.
pub const HEADER_SIZE: usize = 16;

/// Offset of `wseq` within the header.
pub const OFF_WSEQ: usize = 8;

/// Default number of slots. A power of two, so the slot index is a mask.
///
/// 4096 records of 72 bytes is 288 KiB: small enough to stay in a shared
/// process's page cache, large enough that a reader polling every 10 ms does
/// not fall behind a feed publishing a few thousand records a second.
pub const DEFAULT_CAPACITY: u32 = 4096;

/// Bytes in a ring file with `capacity` slots.
#[inline]
pub const fn file_size(capacity: u32) -> usize {
    HEADER_SIZE + capacity as usize * MSG_SIZE
}

/// Read the capacity out of a header.
#[inline]
fn read_capacity(map: &Mapping) -> Result<u32> {
    // SAFETY: the mapping is at least `HEADER_SIZE` bytes, checked by the
    // caller before this is called.
    let bytes = unsafe { map.as_slice() };
    let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if magic != MAGIC {
        return Err(Error::Bus(format!(
            "ring magic {magic:#010x} is not {MAGIC:#010x}"
        )));
    }
    let capacity = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if capacity == 0 || !capacity.is_power_of_two() {
        return Err(Error::Bus(format!(
            "ring capacity {capacity} is not a power of two"
        )));
    }
    Ok(capacity)
}

/// The write side of the bus. Exactly one of these exists per ring file.
///
/// `!Sync` on purpose: two threads publishing through one ring would break the
/// single-writer protocol that every reader depends on. Moving a `Ring` between
/// threads is fine and is what the feed daemon does when it hands the ring to
/// its accept loop.
#[derive(Debug)]
pub struct Ring {
    map: Mapping,
    capacity: u32,
    /// Points at the `wseq` field inside `map`. Held as a pointer rather than a
    /// reference so that `Ring` does not become self-referential.
    wseq: *const AtomicU64,
    /// Kept alive so the mapping's file descriptor stays open for the life of
    /// the mapping.
    _file: File,
}

// SAFETY: the pointer is into the mapping `map` owns, at an eight-byte-aligned
// offset, and `Mapping` is `Send`. Moving a `Ring` moves both together.
unsafe impl Send for Ring {}

impl Ring {
    /// Create or truncate a ring at `path` with `capacity` slots.
    ///
    /// Truncating is deliberate and matches the C++ daemon: a fresh feed run
    /// starts a fresh ring. What survives a restart is the WAL, not the ring.
    pub fn create(path: &Path, capacity: u32) -> Result<Self> {
        if !capacity.is_power_of_two() {
            return Err(Error::Bus(format!(
                "capacity {capacity} must be a power of two"
            )));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| Error::io("open ring", e))?;
        file.set_len(file_size(capacity) as u64)
            .map_err(|e| Error::io("size ring", e))?;
        let map = Mapping::map(&file, file_size(capacity), true)
            .map_err(|e| Error::io("mmap ring", e))?;
        {
            // SAFETY: this process has just created the file, no other process
            // has it open, and the mapping is `file_size(capacity)` bytes.
            let bytes = unsafe { map.as_mut_slice() };
            bytes[0..4].copy_from_slice(&MAGIC.to_le_bytes());
            bytes[4..8].copy_from_slice(&capacity.to_le_bytes());
            bytes[OFF_WSEQ..OFF_WSEQ + 8].copy_from_slice(&0u64.to_le_bytes());
        }
        // No synchronous flush here, and that is a measured decision.
        //
        // The header is written through a `MAP_SHARED` mapping, so any reader
        // that maps the same file sees it through the page cache as soon as the
        // store retires — no `msync` is needed for a reader to see it. What
        // `msync(MS_SYNC)` adds is durability across a crash, for a file whose
        // contents are deliberately not durable: the ring is overwritten after
        // `capacity` records and the write-ahead log is the copy that survives.
        //
        // It cost 2.25 seconds of startup on a busy filesystem, which put the
        // daemon's ready line past the two-second ceiling the process controller
        // waits, so a start failed intermittently and the failure looked like
        // the daemon not starting at all. `docs/optimization/ledger.jsonl`
        // OPT-015 has the measurement.

        // SAFETY: the mapping is at least `HEADER_SIZE` bytes and its base is
        // page-aligned, so `OFF_WSEQ` is eight-byte aligned. `AtomicU64` has the
        // same layout and alignment as `u64`.
        let wseq = unsafe { map.as_slice().as_ptr().add(OFF_WSEQ).cast::<AtomicU64>() };
        Ok(Ring {
            map,
            capacity,
            wseq,
            _file: file,
        })
    }

    /// The `wseq` counter.
    #[inline]
    fn wseq(&self) -> &AtomicU64 {
        // SAFETY: the pointer was produced in `create` from a mapping this
        // struct owns and keeps alive, and the field it points at is never
        // moved.
        unsafe { &*self.wseq }
    }

    /// Slots in this ring.
    #[inline]
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// How many records have been published.
    #[inline]
    pub fn published(&self) -> u64 {
        self.wseq().load(Ordering::Acquire)
    }

    /// Publish one record, assigning its sequence number.
    ///
    /// The record's `magic`, `nbytes` and `seq` fields are set here: a caller
    /// that has decoded a record from the socket has already had its `magic` and
    /// `nbytes` checked, but the sequence is the ring's to assign, and letting a
    /// sender choose it would let a sender corrupt a reader's resynchronisation
    /// check.
    ///
    /// Returns the assigned sequence number.
    pub fn publish(&self, m: &mut Msg) -> u64 {
        let seq = self.wseq().load(Ordering::Relaxed);
        m.magic = MAGIC;
        m.nbytes = MSG_SIZE as u16;
        m.seq = seq;
        let wire = codec::encode(m);
        let offset = HEADER_SIZE + (seq as usize & (self.capacity as usize - 1)) * MSG_SIZE;
        {
            // SAFETY: `offset + MSG_SIZE` is inside the mapping because
            // `capacity` slots were allocated and the index is masked to
            // `capacity`. This process is the ring's only writer.
            let bytes = unsafe { self.map.as_mut_slice() };
            bytes[offset..offset + MSG_SIZE].copy_from_slice(&wire);
        }
        // Release: every byte written above is visible to any reader that
        // observes this store. This single store is the publish.
        self.wseq().store(seq + 1, Ordering::Release);
        seq
    }
}

/// The read side of the bus. Any number of these may exist.
#[derive(Debug)]
pub struct RingReader {
    map: Mapping,
    capacity: u32,
    wseq: *const AtomicU64,
    _file: File,
}

// SAFETY: as for `Ring`. Readers only load `wseq` and read slots, and the
// mapping is immutable for their lifetime.
unsafe impl Send for RingReader {}

/// What one [`RingReader::consume`] call found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consume {
    /// A record was read; the cursor advanced by one.
    Fresh,
    /// Nothing new since the last call.
    Empty,
    /// The writer had lapped this cursor. The cursor was moved to the oldest
    /// record still present and no record was returned.
    Resynchronised,
    /// The slot at this cursor belongs to an earlier lap of the ring. The
    /// cursor was moved to the write sequence and no record was returned.
    Stale,
}

impl RingReader {
    /// Open an existing ring for reading.
    ///
    /// The file must already exist and have been created by [`Ring::create`];
    /// a reader never creates one, because a reader cannot know how big the ring
    /// should be and a zero-filled file would look like a valid empty ring.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|e| Error::io("open ring for reading", e))?;
        let len = file
            .metadata()
            .map_err(|e| Error::io("stat ring", e))?
            .len() as usize;
        if len < HEADER_SIZE {
            return Err(Error::Bus(format!("ring file is {len} bytes, too short")));
        }
        let map = Mapping::map(&file, len, false).map_err(|e| Error::io("mmap ring", e))?;
        let capacity = read_capacity(&map)?;
        let expected = file_size(capacity);
        if len != expected {
            return Err(Error::Bus(format!(
                "ring file is {len} bytes, header says {expected}"
            )));
        }
        // SAFETY: as in `Ring::create`; the base is page-aligned and the header
        // is present.
        let wseq = unsafe { map.as_slice().as_ptr().add(OFF_WSEQ).cast::<AtomicU64>() };
        Ok(RingReader {
            map,
            capacity,
            wseq,
            _file: file,
        })
    }

    /// The `wseq` counter.
    #[inline]
    fn wseq(&self) -> &AtomicU64 {
        // SAFETY: produced in `open` from a mapping this struct owns.
        unsafe { &*self.wseq }
    }

    /// Slots in this ring.
    #[inline]
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// How many records the writer has published.
    #[inline]
    pub fn published(&self) -> u64 {
        self.wseq().load(Ordering::Acquire)
    }

    /// How far `cursor` is behind the writer, in records.
    ///
    /// This is the reader's lag, and it is the first half of the backpressure
    /// story in [`crate::pressure`]: a reader whose lag approaches the ring's
    /// capacity is about to start losing records.
    #[inline]
    pub fn lag(&self, cursor: u64) -> u64 {
        self.published().saturating_sub(cursor)
    }

    /// The daemon's stamp on the record at `seq`, when that slot still holds it.
    ///
    /// This is what turns a lag in *records* into a lag in *time*: the span
    /// between the stamp of the oldest record a reader has not taken and the
    /// stamp of the newest published one is how much feed time that reader is
    /// behind. Without it a reader can only say "forty records", which means
    /// nothing without knowing the rate.
    ///
    /// `None` when `seq` has not been published, or when the slot has been
    /// reused by a later lap. The sequence check is the same discipline
    /// [`RingReader::consume`] uses, and for the same reason: a stamp from the
    /// wrong lap would silently misreport how far behind a reader is.
    pub fn timestamp_at(&self, seq: u64) -> Option<u64> {
        if seq >= self.published() {
            return None;
        }
        let offset = HEADER_SIZE + (seq as usize & (self.capacity as usize - 1)) * MSG_SIZE;
        // SAFETY: `offset + MSG_SIZE` is inside the mapping, which `open`
        // checked to be `file_size(capacity)`. Readers only load from the
        // mapping; the writer's release store of `wseq` is what makes the bytes
        // visible, and the sequence check below rejects a slot from an earlier
        // lap.
        let bytes = unsafe { self.map.as_slice() };
        let (found, stamp) = codec::seq_and_stamp(&bytes[offset..offset + MSG_SIZE])?;
        if found == seq {
            Some(stamp)
        } else {
            None
        }
    }

    /// Read the record at `cursor`, advancing it on success.
    ///
    /// Never blocks, never waits for the writer, and never returns a record that
    /// the writer had not finished publishing.
    pub fn consume(&self, cursor: &mut u64, out: &mut Msg) -> Consume {
        let written = self.wseq().load(Ordering::Acquire);
        if *cursor >= written {
            return Consume::Empty;
        }
        let capacity = self.capacity as u64;
        if written - *cursor > capacity {
            // The writer lapped this reader. Skip to the oldest slot that is
            // still live; the records in between are gone and the caller finds
            // out from the gap in sequence numbers.
            *cursor = written - capacity;
            return Consume::Resynchronised;
        }
        let offset = HEADER_SIZE + (*cursor as usize & (self.capacity as usize - 1)) * MSG_SIZE;
        let mut wire = [0u8; MSG_SIZE];
        {
            // SAFETY: `offset + MSG_SIZE` is inside the mapping, which is at
            // least `file_size(capacity)` bytes because `open` checked.
            let bytes = unsafe { self.map.as_slice() };
            wire.copy_from_slice(&bytes[offset..offset + MSG_SIZE]);
        }
        match codec::decode(&wire) {
            Ok(m) if m.seq == *cursor => {
                *out = m;
                *cursor += 1;
                Consume::Fresh
            }
            // The slot was reused under this read, or the record is damaged.
            // Both mean the same thing to a reader: skip forward to the live end
            // and carry on. Reachable without any corruption — see the module
            // note on the sequence check.
            _ => {
                *cursor = written;
                Consume::Stale
            }
        }
    }

    /// Read every available record into `out`, returning how many were written.
    ///
    /// The batch form exists so a caller that drains the ring on a timer makes
    /// one acquire load and one function call rather than one per record. It
    /// stops early when `out` is full, leaving the cursor at the next unread
    /// record, so a caller with a small buffer makes progress without losing
    /// anything.
    pub fn consume_batch(&self, cursor: &mut u64, out: &mut [Msg]) -> usize {
        let mut n = 0;
        while n < out.len() {
            match self.consume(cursor, &mut out[n]) {
                Consume::Fresh => n += 1,
                // A resynchronisation consumed no record but did move the
                // cursor; loop again so the caller gets the newest records in
                // this same call rather than on the next timer tick.
                Consume::Resynchronised => continue,
                Consume::Empty | Consume::Stale => break,
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::{Price, Qty};
    use crate::msg::Side;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ob-ring-{tag}-{}-{:?}.bin",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn depth(px: i64, side: Side) -> Msg {
        Msg::depth(side, Price::from_e8(px), Qty::from_e8(1_000_000), "BTCUSDT")
    }

    #[test]
    fn header_matches_the_cpp_layout() {
        let path = scratch("header");
        let ring = Ring::create(&path, 8).expect("create");
        assert_eq!(ring.capacity(), 8);
        assert_eq!(ring.published(), 0);
        // The file is exactly a 16-byte header plus eight 72-byte slots.
        let len = std::fs::metadata(&path).expect("stat").len();
        assert_eq!(len as usize, HEADER_SIZE + 8 * MSG_SIZE);
        assert_eq!(len as usize, file_size(8));
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn publish_then_consume_round_trips_and_assigns_sequence() {
        let path = scratch("round");
        let ring = Ring::create(&path, 8).expect("create");
        let reader = RingReader::open(&path).expect("open");

        let mut m = depth(100, Side::Bid);
        assert_eq!(ring.publish(&mut m), 0);
        assert_eq!(
            m.seq, 0,
            "publish assigns the sequence onto the caller's record"
        );
        assert_eq!(ring.publish(&mut depth(101, Side::Ask)), 1);
        assert_eq!(ring.published(), 2);

        let mut cursor = 0u64;
        let mut out = Msg::default();
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Fresh);
        assert_eq!(out.px_e8, 100);
        assert_eq!(out.seq, 0);
        assert_eq!(cursor, 1);

        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Fresh);
        assert_eq!(out.px_e8, 101);
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Empty);

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn two_readers_keep_independent_cursors() {
        let path = scratch("two-readers");
        let ring = Ring::create(&path, 8).expect("create");
        let a = RingReader::open(&path).expect("open a");
        let b = RingReader::open(&path).expect("open b");

        for i in 0..3 {
            ring.publish(&mut depth(100 + i, Side::Bid));
        }

        let mut ca = 0u64;
        let mut cb = 0u64;
        let mut out = Msg::default();
        // `a` reads everything, `b` reads one and stops.
        assert_eq!(a.consume_batch(&mut ca, std::slice::from_mut(&mut out)), 1);
        assert_eq!(ca, 1);
        assert_eq!(b.consume_batch(&mut cb, std::slice::from_mut(&mut out)), 1);
        assert_eq!(cb, 1);
        assert_eq!(a.lag(ca), 2);
        assert_eq!(b.lag(cb), 2);

        let mut all = [Msg::default(); 4];
        assert_eq!(a.consume_batch(&mut ca, &mut all), 2);
        assert_eq!(ca, 3);
        assert_eq!(all[0].px_e8, 101);

        drop(a);
        drop(b);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_lapped_reader_is_told_to_resynchronise_rather_than_lied_to() {
        let path = scratch("lapped");
        let ring = Ring::create(&path, 4).expect("create");
        let reader = RingReader::open(&path).expect("open");
        // Publish two laps' worth so the first records are overwritten.
        for i in 0..10 {
            ring.publish(&mut depth(100 + i, Side::Bid));
        }
        let mut cursor = 0u64;
        let mut out = Msg::default();
        assert_eq!(
            reader.consume(&mut cursor, &mut out),
            Consume::Resynchronised
        );
        assert_eq!(cursor, 10 - 4, "cursor jumps to the oldest live slot");
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Fresh);
        assert_eq!(out.seq, 6);
        assert_eq!(out.px_e8, 106);

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_record_keeps_its_stamp_for_a_later_reader() {
        // The stamp is what makes a lag measurable in time rather than in
        // records: a reader needs the times of two records it has not decoded,
        // one it has not taken and the newest published.
        let path = scratch("stamp-at");
        let ring = Ring::create(&path, 8).expect("create");
        for i in 0..5 {
            let mut m = depth(100 + i, Side::Bid);
            m.ts_ns = 1_000_000 + i as u64 * 250;
            ring.publish(&mut m);
        }
        let reader = RingReader::open(&path).expect("open");

        // The newest published record, and the one a reader at cursor 0 is
        // waiting for. Their difference is the span of feed time not yet read.
        assert_eq!(reader.timestamp_at(4), Some(1_000_000 + 4 * 250));
        assert_eq!(reader.timestamp_at(0), Some(1_000_000));
        assert_eq!(
            reader.timestamp_at(4).unwrap() - reader.timestamp_at(0).unwrap(),
            1_000
        );

        // A sequence that has not been published has no stamp.
        assert_eq!(reader.timestamp_at(5), None);
        assert_eq!(reader.timestamp_at(u64::MAX), None);

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_reused_slot_reports_no_stamp_rather_than_the_wrong_lap() {
        let path = scratch("stamp-lap");
        let ring = Ring::create(&path, 4).expect("create");
        for i in 0..6 {
            let mut m = depth(100 + i, Side::Bid);
            m.ts_ns = 1_000 + i as u64;
            ring.publish(&mut m);
        }
        let reader = RingReader::open(&path).expect("open");
        // Sequences 0 and 1 have been overwritten by 4 and 5, so their slots
        // hold a later stamp. Reporting it would understate a lag.
        assert_eq!(reader.timestamp_at(0), None);
        assert_eq!(reader.timestamp_at(1), None);
        assert_eq!(reader.timestamp_at(2), Some(1_002));
        assert_eq!(reader.timestamp_at(5), Some(1_005));

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_damaged_slot_is_skipped_rather_than_returned() {
        // A well-behaved writer cannot produce this: the lag guard above means
        // the slot always holds the sequence the reader asked for. The record's
        // own sequence field is therefore a check against a damaged slot and
        // against a future second writer, not against the normal path. This
        // test damages a slot to prove the check is live.
        let path = scratch("damaged");
        let ring = Ring::create(&path, 4).expect("create");
        for i in 0..3 {
            ring.publish(&mut depth(100 + i, Side::Bid));
        }
        {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("open");
            let map = Mapping::map(&f, file_size(4), true).expect("map");
            // SAFETY: this test owns the file and the writer is idle.
            let bytes = unsafe { map.as_mut_slice() };
            // Zero the magic of slot 1 (sequence 1).
            let at = HEADER_SIZE + MSG_SIZE;
            bytes[at] = 0;
        }
        let reader = RingReader::open(&path).expect("open");
        let mut cursor = 0u64;
        let mut out = Msg::default();
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Fresh);
        assert_eq!(out.seq, 0);
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Stale);
        assert_eq!(cursor, 3, "the reader skips to the live end");
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Empty);

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn batch_consume_stops_when_the_output_is_full() {
        let path = scratch("batch");
        let ring = Ring::create(&path, 64).expect("create");
        let reader = RingReader::open(&path).expect("open");
        for i in 0..10 {
            ring.publish(&mut depth(100 + i, Side::Bid));
        }
        let mut cursor = 0u64;
        let mut four = [Msg::default(); 4];
        assert_eq!(reader.consume_batch(&mut cursor, &mut four), 4);
        assert_eq!(cursor, 4, "cursor sits on the next unread record");
        let mut ten = [Msg::default(); 10];
        assert_eq!(reader.consume_batch(&mut cursor, &mut ten), 6);
        assert_eq!(cursor, 10);

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_bad_capacity_or_magic_is_refused() {
        let path = scratch("badcap");
        assert!(
            Ring::create(&path, 100).is_err(),
            "100 is not a power of two"
        );

        let ring = Ring::create(&path, 8).expect("create");
        drop(ring);
        // Corrupt the magic and reopen.
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let map = Mapping::map(&f, file_size(8), true).expect("map");
        // SAFETY: this test owns the file.
        unsafe { map.as_mut_slice()[0] = 0 };
        drop(map);
        drop(f);
        assert!(RingReader::open(&path).is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_reader_refuses_a_file_whose_length_disagrees_with_its_header() {
        let path = scratch("short");
        let ring = Ring::create(&path, 8).expect("create");
        drop(ring);
        let f = OpenOptions::new().write(true).open(&path).expect("open");
        f.set_len(HEADER_SIZE as u64 + 8).expect("truncate");
        drop(f);
        let err = RingReader::open(&path).expect_err("must be refused");
        // The header is readable but describes a bigger ring than the file
        // holds; either message is a refusal, and the test pins that it is a
        // refusal rather than a silent short mapping.
        let text = err.to_string();
        assert!(
            text.contains("header says") || text.contains("too short"),
            "{text}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_writer_stamp_survives_a_ring_round_trip() {
        let path = scratch("stamp");
        let ring = Ring::create(&path, 8).expect("create");
        let reader = RingReader::open(&path).expect("open");
        let mut m = depth(100, Side::Bid);
        m.set_stamp_ns(42_000);
        m.fill_correlation(1);
        ring.publish(&mut m);

        let mut cursor = 0u64;
        let mut out = Msg::default();
        assert_eq!(reader.consume(&mut cursor, &mut out), Consume::Fresh);
        assert_eq!(out.stamp_ns(), Some(42_000));

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_thread_can_read_what_another_thread_wrote() {
        let path = scratch("threads");
        let ring = Ring::create(&path, 4096).expect("create");
        let reader = RingReader::open(&path).expect("open");
        const N: u64 = 20_000;

        let writer = std::thread::spawn(move || {
            for i in 0..N {
                ring.publish(&mut depth(100 + i as i64, Side::Bid));
            }
            ring
        });

        // Drain until the writer has finished and the cursor has reached the
        // end. A lapped reader resynchronises and carries on; what must never
        // happen is a record arriving out of order.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut cursor = 0u64;
        let mut out = Msg::default();
        let mut seen = 0u64;
        let mut last: Option<u64> = None;
        let mut resyncs = 0u64;
        loop {
            match reader.consume(&mut cursor, &mut out) {
                Consume::Fresh => {
                    if let Some(prev) = last {
                        assert!(
                            out.seq > prev,
                            "sequence went backwards: {prev} then {}",
                            out.seq
                        );
                    }
                    last = Some(out.seq);
                    seen += 1;
                }
                // Both mean "this reader lost a race with the writer"; the
                // difference is only which check noticed. Either way the reader
                // skips forward and the sequence never goes backwards.
                Consume::Resynchronised | Consume::Stale => resyncs += 1,
                Consume::Empty => {
                    if reader.lag(cursor) == 0 && cursor >= N {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reader did not reach the end: cursor={cursor} seen={seen}"
            );
        }
        let ring = writer.join().expect("writer thread");
        assert_eq!(ring.published(), N);
        assert!(seen > 0, "the reader saw nothing at all");
        // The reader may have been lapped, but it can never have seen more
        // records than were published.
        assert!(seen <= N, "seen {seen} of {N}");
        assert_eq!(cursor, N, "the cursor must end at the write sequence");
        let _ = resyncs;

        drop(reader);
        drop(ring);
        let _ = std::fs::remove_file(&path);
    }
}
