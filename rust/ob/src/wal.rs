//! The append-only log that survives the ring being overwritten.
//!
//! # What it is for
//!
//! The ring holds the last few thousand records. When a reader falls behind it
//! loses them, by design: a feed handler must not be slowed down by a slow
//! consumer. The log is the other half of that bargain — every record that was
//! ever published is also appended here, in order, so that a process starting
//! later can replay what it missed and a crash can be recovered to a known
//! point.
//!
//! This is the write-ahead-log shape, not a database: records are fixed size, in
//! publish order, with no index and no commit protocol. Recovery is "read the
//! file until a record is short or damaged, and keep everything before that".
//!
//! # Durability, and what the sync policy actually buys
//!
//! A write to a file reaches the operating system's cache, not the disk. The
//! question is how much of the tail a machine crash may lose.
//!
//! | Policy | Call | What a crash loses | Cost |
//! |--------|------|--------------------|------|
//! | [`SyncPolicy::None`] | nothing | everything not yet flushed by the OS | zero |
//! | [`SyncPolicy::Data`] | `fdatasync` / `fsync` | at most the last [`SYNC_EVERY`] records | tens of microseconds |
//! | [`SyncPolicy::Full`] | `F_FULLFSYNC` on macOS, `fdatasync` elsewhere | nothing the call returned for | milliseconds |
//!
//! `Data` is the default and is what the C++ implementation does: `fdatasync`
//! on Linux, `fsync` on macOS, every [`SYNC_EVERY`] records. `Full` exists
//! because on macOS `fsync` returns once the data has reached the *drive's*
//! cache, not the platter, and a lab that claims point-in-time recovery should
//! be able to say so honestly. The measured cost of each policy is in
//! `docs/design/vec-optimization.md`.

use crate::codec;
use crate::error::{Error, Result};
use crate::msg::{Msg, MSG_SIZE};
use crate::sys::Mapping;
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

/// How many appends between automatic syncs under [`SyncPolicy::Data`].
pub const SYNC_EVERY: u32 = 32;

/// How much of the tail a crash may lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncPolicy {
    /// Never sync explicitly. The operating system flushes on its own schedule.
    None,
    /// Sync every [`SYNC_EVERY`] records, and on close.
    #[default]
    Data,
    /// Sync every [`SYNC_EVERY`] records with the strongest call the platform
    /// offers, and on close.
    Full,
}

/// An append-only record log.
#[derive(Debug)]
pub struct Wal {
    file: File,
    path: PathBuf,
    policy: SyncPolicy,
    since_sync: u32,
    appended: u64,
}

impl Wal {
    /// Open a log for appending, creating it if it does not exist.
    ///
    /// An existing file is appended to, never truncated: the whole point is that
    /// records outlive the process. Pass `truncate = true` to start a fresh log,
    /// which is what `obctl setup` does for a clean dev run.
    ///
    /// The file is opened read-write and then seeked to the end rather than
    /// opened with `O_APPEND`, because `O_APPEND` and `O_TRUNC` are mutually
    /// exclusive in the standard library's `OpenOptions` and the two modes this
    /// function needs are "append to what is there" and "start again". The
    /// daemon is the log's only writer, so positioning writes explicitly is
    /// equivalent to append mode.
    pub fn open(path: &Path, truncate: bool, policy: SyncPolicy) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        if truncate {
            options.truncate(true);
        }
        let mut file = options.open(path).map_err(|e| Error::io("open wal", e))?;
        let len = file.metadata().map_err(|e| Error::io("stat wal", e))?.len();
        if len % MSG_SIZE as u64 != 0 {
            // A log whose length is not a whole number of records already has a
            // torn tail. Appending after it would put every later record at an
            // offset a reader cannot find, so the tail is trimmed first.
            file.set_len(len - (len % MSG_SIZE as u64))
                .map_err(|e| Error::io("trim wal tail", e))?;
        }
        // Position at the end so the next write follows the existing records.
        file.seek(std::io::SeekFrom::End(0))
            .map_err(|e| Error::io("seek wal", e))?;
        Ok(Wal {
            file,
            path: path.to_path_buf(),
            policy,
            since_sync: 0,
            appended: 0,
        })
    }

    /// The log's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records appended by this handle.
    pub fn appended(&self) -> u64 {
        self.appended
    }

    /// Append one record.
    ///
    /// A short write is an error, not something to retry: a log with a partial
    /// record in it is a log a reader will stop at, and retrying a partial write
    /// would put the next record at a non-record offset.
    pub fn append(&mut self, m: &Msg) -> Result<()> {
        let wire = codec::encode(m);
        self.file
            .write_all(&wire)
            .map_err(|e| Error::io("wal write", e))?;
        self.appended += 1;
        self.since_sync += 1;
        if self.since_sync >= SYNC_EVERY && self.policy != SyncPolicy::None {
            self.sync()?;
        }
        Ok(())
    }

    /// Flush to the durability level the policy selects, and reset the counter.
    pub fn sync(&mut self) -> Result<()> {
        match self.policy {
            SyncPolicy::None => {}
            SyncPolicy::Data => self
                .file
                .sync_data()
                .map_err(|e| Error::io("wal sync_data", e))?,
            SyncPolicy::Full => {
                crate::sys::full_fsync(&self.file).map_err(|e| Error::io("wal full fsync", e))?
            }
        }
        self.since_sync = 0;
        Ok(())
    }

    /// Bytes on disk, as the log's own length.
    pub fn bytes(&self) -> u64 {
        self.file.metadata().map(|m| m.len()).unwrap_or(0)
    }
}

impl Drop for Wal {
    fn drop(&mut self) {
        // Best effort: a failing sync on the way out has nowhere to be reported,
        // and the records are still in the operating system's cache either way.
        let _ = self.sync();
    }
}

/// What a replay found.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    /// Records that decoded, in file order.
    pub records: Vec<Msg>,
    /// Whole records offered to the decoder.
    pub records_seen: usize,
    /// Records that failed to decode and were skipped.
    pub rejected: usize,
    /// Trailing bytes that were not a whole record: the torn tail of a crash.
    pub torn_tail_bytes: usize,
    /// The file's length in bytes.
    pub bytes: u64,
}

impl Replay {
    /// The highest sequence number in the replay, if any.
    ///
    /// This is the point the log recovers to: a feed daemon that replays a log
    /// knows where it was and can ask the exchange to resume from there.
    pub fn last_seq(&self) -> Option<u64> {
        self.records.last().map(|m| m.seq)
    }

    /// Whether the replay ended on a clean record boundary.
    pub fn clean_tail(&self) -> bool {
        self.torn_tail_bytes == 0 && self.rejected == 0
    }
}

/// Read every whole record from a log file.
///
/// A missing file replays as empty rather than failing, which is what the
/// headless checker wants: a first run with no log yet is not an error.
///
/// The file is mapped rather than read, and advised as sequential, because
/// replay is the one place in this crate that walks a large file front to back
/// exactly once. Records are decoded in batches through
/// [`codec::decode_batch`], which is the same decoder the socket path uses.
pub fn replay(path: &Path) -> Result<Replay> {
    let file = match OpenOptions::new().read(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replay::default()),
        Err(e) => return Err(Error::io("open wal for replay", e)),
    };
    let len = file.metadata().map_err(|e| Error::io("stat wal", e))?.len();
    if len == 0 {
        return Ok(Replay::default());
    }
    let map = Mapping::map(&file, len as usize, false).map_err(|e| Error::io("mmap wal", e))?;
    // Advice, not a requirement: a failure here changes nothing about what is
    // read, only how the kernel reads it.
    let _ = map.advise_sequential();

    let whole = (len / MSG_SIZE as u64) as usize;
    let torn = (len % MSG_SIZE as u64) as usize;
    // SAFETY: the mapping is `len` bytes and lives for the whole loop, and this
    // process has the file open read-only, so nothing writes through it.
    let bytes = unsafe { map.as_slice() };
    let body = &bytes[..whole * MSG_SIZE];

    // Decode in chunks so that the batch decoder sees a buffer that fits in L1
    // rather than the whole file at once.
    const BATCH: usize = 256;
    let mut out = [Msg::default(); BATCH];
    let mut replay = Replay {
        records: Vec::with_capacity(whole),
        torn_tail_bytes: torn,
        bytes: len,
        ..Replay::default()
    };
    for chunk in body.chunks(BATCH * MSG_SIZE) {
        let stats = codec::decode_batch(chunk, &mut out);
        replay.records_seen += stats.records;
        replay.rejected += stats.skipped;
        // `decode_batch` writes only the records that decoded, compacted to the
        // front of `out`; take exactly those.
        replay.records.extend_from_slice(&out[..stats.decoded]);
    }
    Ok(replay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::{Price, Qty};
    use crate::msg::Side;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ob-wal-{tag}-{}-{:?}.log",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn rec(i: i64) -> Msg {
        let mut m = Msg::depth(Side::Bid, Price::from_e8(i), Qty::from_e8(i * 2), "BTCUSDT");
        m.seq = i as u64;
        m
    }

    #[test]
    fn append_then_replay_round_trips() {
        let path = scratch("round");
        let mut wal = Wal::open(&path, true, SyncPolicy::Data).expect("open");
        for i in 0..100 {
            wal.append(&rec(i)).expect("append");
        }
        wal.sync().expect("sync");
        drop(wal);

        let r = replay(&path).expect("replay");
        assert_eq!(r.records.len(), 100);
        assert_eq!(r.records_seen, 100);
        assert_eq!(r.rejected, 0);
        assert_eq!(r.torn_tail_bytes, 0);
        assert!(r.clean_tail());
        assert_eq!(r.last_seq(), Some(99));
        assert_eq!(r.records[7].px_e8, 7);
        assert_eq!(r.records[7].seq, 7);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_log_replays_as_empty() {
        let path = scratch("missing");
        let _ = std::fs::remove_file(&path);
        let r = replay(&path).expect("replay");
        assert!(r.records.is_empty());
        assert_eq!(r.last_seq(), None);
        assert!(r.clean_tail());
    }

    #[test]
    fn a_torn_tail_is_reported_and_everything_before_it_is_kept() {
        let path = scratch("torn");
        let mut wal = Wal::open(&path, true, SyncPolicy::Data).expect("open");
        for i in 0..10 {
            wal.append(&rec(i)).expect("append");
        }
        wal.sync().expect("sync");
        let good = wal.bytes();
        drop(wal);

        // Simulate a crash mid-append by extending the file with half a record.
        let f = OpenOptions::new().append(true).open(&path).expect("open");
        let mut f = f;
        f.write_all(&[0u8; MSG_SIZE / 2]).expect("write partial");
        drop(f);

        let r = replay(&path).expect("replay");
        assert_eq!(r.records.len(), 10);
        assert_eq!(r.torn_tail_bytes, MSG_SIZE / 2);
        assert!(!r.clean_tail(), "a torn tail is not clean");
        assert_eq!(r.bytes, good + MSG_SIZE as u64 / 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn opening_over_a_torn_tail_trims_it_before_appending() {
        let path = scratch("trim");
        let mut wal = Wal::open(&path, true, SyncPolicy::Data).expect("open");
        wal.append(&rec(1)).expect("append");
        let good = wal.bytes();
        drop(wal);
        let mut f = OpenOptions::new().append(true).open(&path).expect("open");
        f.write_all(&[0u8; 7]).expect("write partial");
        drop(f);

        // Reopening trims the partial tail so the next record lands on a
        // boundary a reader can find.
        let mut wal = Wal::open(&path, false, SyncPolicy::Data).expect("reopen");
        assert_eq!(wal.bytes(), good);
        wal.append(&rec(2)).expect("append");
        wal.sync().expect("sync");
        drop(wal);

        let r = replay(&path).expect("replay");
        assert_eq!(r.records.len(), 2);
        assert_eq!(r.last_seq(), Some(2));
        assert!(r.clean_tail());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_damaged_record_is_counted_and_skipped() {
        let path = scratch("damaged");
        let mut wal = Wal::open(&path, true, SyncPolicy::Data).expect("open");
        for i in 0..5 {
            wal.append(&rec(i)).expect("append");
        }
        wal.sync().expect("sync");
        drop(wal);

        // Corrupt the type byte of the third record.
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let map = Mapping::map(&f, 5 * MSG_SIZE, true).expect("map");
        // SAFETY: this test owns the file.
        unsafe { map.as_mut_slice()[2 * MSG_SIZE + crate::codec::OFF_KIND] = 99 };
        drop(map);
        drop(f);

        let r = replay(&path).expect("replay");
        assert_eq!(r.records_seen, 5);
        assert_eq!(r.rejected, 1);
        assert_eq!(r.records.len(), 4);
        assert!(!r.clean_tail());
        // Everything after the damaged record is still read: the log does not
        // stop at the first surprise, it reports it.
        assert_eq!(r.records.last().map(|m| m.seq), Some(4));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn appending_to_an_existing_log_keeps_the_old_records() {
        let path = scratch("append");
        let mut wal = Wal::open(&path, true, SyncPolicy::Data).expect("open");
        wal.append(&rec(1)).expect("append");
        wal.sync().expect("sync");
        drop(wal);

        let mut wal = Wal::open(&path, false, SyncPolicy::Data).expect("reopen");
        wal.append(&rec(2)).expect("append");
        wal.sync().expect("sync");
        drop(wal);

        let r = replay(&path).expect("replay");
        assert_eq!(r.records.len(), 2);
        assert_eq!(r.records[0].seq, 1);
        assert_eq!(r.records[1].seq, 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_sync_policy_produces_the_same_bytes() {
        // The policy changes when data reaches the disk, never what is written.
        let mut contents = Vec::new();
        for policy in [SyncPolicy::None, SyncPolicy::Data, SyncPolicy::Full] {
            let path = scratch(&format!("policy-{policy:?}"));
            let mut wal = Wal::open(&path, true, policy).expect("open");
            for i in 0..70 {
                // More than SYNC_EVERY, so each policy actually syncs.
                wal.append(&rec(i)).expect("append");
            }
            wal.sync().expect("sync");
            drop(wal);
            contents.push(std::fs::read(&path).expect("read back"));
            let _ = std::fs::remove_file(&path);
        }
        assert_eq!(contents[0], contents[1]);
        assert_eq!(contents[1], contents[2]);
        assert_eq!(contents[0].len(), 70 * MSG_SIZE);
    }
}
