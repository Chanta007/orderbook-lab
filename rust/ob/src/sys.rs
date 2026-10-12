//! The crate's one boundary with the operating system.
//!
//! # Why this file exists at all
//!
//! The standard library already covers almost everything the lab needs: `File`
//! reads and writes, `TcpListener` accepts, `thread` spawns, `Instant` reads a
//! clock. Two things it does not cover:
//!
//! 1. **Mapping a file into shared memory.** That is the bus. [`crate::ring`]
//!    needs `mmap` with `MAP_SHARED` so that a second process sees the same
//!    bytes, and `std` has no equivalent.
//! 2. **A monotonic clock that can be read fast and compared across processes
//!    on one host.** `Instant` is opaque and not portable to a number;
//!    [`crate::clock`] needs raw nanoseconds.
//!
//! Rather than take a dependency for those two things, this module declares the
//! seven libc functions it uses. The declarations are the entire `unsafe`
//! surface of the crate, and each one is wrapped by a safe function that states
//! its contract, so no `unsafe` block appears outside this module except in
//! [`crate::ring`], where the mapping is turned into slices.
//!
//! # What is deliberately not here
//!
//! - Sockets: `std::net` is a complete, safe TCP client and server.
//! - Files: `std::fs` plus `File::sync_data` covers open, read, write and flush.
//! - Signals: `std::process::id` and `std::thread::yield_now` cover the rest.
//!
//! The rule for adding to this file: only when `std` genuinely cannot express
//! the call, and only with a safe wrapper that documents the contract.

use std::ffi::{c_int, c_void};
use std::io;

// ---------------------------------------------------------------------------
// libc declarations
//
// Signatures are transcribed from the POSIX and Darwin headers. They are the
// same on macOS and Linux for everything used here, with two exceptions noted
// at the definitions below (`MAP_SHARED` and `MS_SYNC` agree; `madvise` advice
// values agree only for the ones used).
// ---------------------------------------------------------------------------

extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn msync(addr: *mut c_void, len: usize, flags: c_int) -> c_int;
    fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;
    fn clock_gettime(clock_id: c_int, tp: *mut Timespec) -> c_int;
}

#[cfg(target_os = "macos")]
extern "C" {
    /// Variadic in C; only the two-argument form is used here.
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
}

/// `F_FULLFSYNC` on Darwin: flush the drive's own cache, not just the kernel's.
#[cfg(target_os = "macos")]
const F_FULLFSYNC: c_int = 51;

/// `struct timespec`, identical on Darwin and glibc for a 64-bit target.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// Read-only mapping.
pub const PROT_READ: c_int = 0x1;
/// Writable mapping.
pub const PROT_WRITE: c_int = 0x2;
/// Changes are shared with other processes mapping the same file.
pub const MAP_SHARED: c_int = 0x0001;
/// Flush changes to the backing file before returning.
///
/// The values differ and the difference is silent: passing Darwin's 0x10 to
/// Linux's `msync` is `EINVAL`, so the flush does not happen and the call reports
/// a failure that a caller ignoring the result would never see. They are **not**
/// the same constant, which an earlier version of this file claimed.
#[cfg(target_os = "macos")]
pub const MS_SYNC: c_int = 0x0010;
/// `MS_SYNC` on Linux. See the note on the macOS definition.
#[cfg(not(target_os = "macos"))]
pub const MS_SYNC: c_int = 4;
/// Expect sequential access. Used for WAL replay, where it earns its keep.
pub const MADV_SEQUENTIAL: c_int = 0x2;

/// `CLOCK_MONOTONIC_RAW` is 4 on both Darwin and Linux.
///
/// It is the right clock for this lab because it never steps: no NTP
/// correction, no leap-second smear, so a difference of two readings is a real
/// elapsed time. It does count time spent asleep, which is what a feed handler
/// wants when it asks "how stale is this book", and it is the same clock
/// domain in every process on the host, which is what makes the cross-process
/// latency numbers in [`crate::latency`] comparable.
pub const CLOCK_MONOTONIC_RAW: c_int = 4;

/// A read-only, writable, or read-write mapping of a file.
///
/// The mapping is held for the life of the value and unmapped on drop. It is
/// **not** `Sync`: two threads writing through one mapping would race with no
/// protection, and the bus is explicitly a single-writer structure.
#[derive(Debug)]
pub struct Mapping {
    base: *mut u8,
    len: usize,
}

// SAFETY: `Mapping` owns an exclusive mapping of a file. Moving it between
// threads moves the only handle to that address range; no thread is left with a
// pointer into it. Sharing it (`&Mapping`) is what is forbidden, and it is
// forbidden by the absence of `Sync` below.
unsafe impl Send for Mapping {}

impl Mapping {
    /// Map `len` bytes of `file` starting at byte 0.
    ///
    /// `writable` selects `PROT_READ|PROT_WRITE` against `PROT_READ`. The file
    /// must already be at least `len` bytes long; the caller sets the length
    /// (see [`crate::ring::Ring::create`]) so that a short file cannot be
    /// mapped and then read past its end.
    ///
    /// The descriptor's own access mode has to permit what is being requested:
    /// a writable mapping of a file opened write-only fails with `EACCES`,
    /// which is the operating system's way of saying that the mapping's
    /// permissions are the intersection of the two. [`crate::ring`] opens with
    /// read and write for exactly this reason.
    pub fn map(file: &std::fs::File, len: usize, writable: bool) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        let prot = if writable {
            PROT_READ | PROT_WRITE
        } else {
            PROT_READ
        };
        // SAFETY: `len` is non-zero and the fd is open and owned by `file` for
        // the duration of the call. `mmap` returns `MAP_FAILED`, never null, on
        // failure, and that is checked below.
        let base = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                prot,
                MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if base as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping {
            base: base.cast::<u8>(),
            len,
        })
    }

    /// The mapping as a byte slice.
    ///
    /// Marked `unsafe` because the caller must guarantee that no other process
    /// or thread writes to the mapping concurrently in a way this program has
    /// not accounted for. [`crate::ring`] is the only caller, and it publishes
    /// and consumes whole records with release and acquire stores around them,
    /// so a slice handed out here is only ever read after the ordering check
    /// that says the bytes are complete.
    ///
    /// # Safety
    ///
    /// The caller must uphold the single-writer, ordered-publish protocol
    /// documented on [`crate::ring`].
    pub unsafe fn as_slice(&self) -> &[u8] {
        // SAFETY: the pointer is the base of a live mapping of `len` bytes
        // owned by `self`, and it outlives the returned slice because the slice
        // borrows `self`.
        unsafe { std::slice::from_raw_parts(self.base, self.len) }
    }

    /// The mapping as a mutable byte slice.
    ///
    /// # Safety
    ///
    /// In addition to the requirements of [`Mapping::as_slice`], the caller
    /// must be the single writer described by the ring protocol.
    #[allow(clippy::mut_from_ref)] // the mutability is a property of the protocol, not of `self`
    pub unsafe fn as_mut_slice(&self) -> &mut [u8] {
        // SAFETY: as `as_slice`, plus the single-writer guarantee above.
        unsafe { std::slice::from_raw_parts_mut(self.base, self.len) }
    }

    /// Length of the mapping in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always `false` for a mapping: a zero-length mapping is rejected by
    /// `mmap`. Present so the type does not trip `clippy::len_without_is_empty`.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Flush dirty pages to the backing file and wait for the write.
    ///
    /// Used once, when a ring is created: the header must be on disk before any
    /// reader can be pointed at the file.
    pub fn sync(&self) -> io::Result<()> {
        // SAFETY: the whole mapping is owned by `self` and `MS_SYNC` is a valid
        // flag for `msync` on both platforms.
        let rc = unsafe { msync(self.base.cast::<c_void>(), self.len, MS_SYNC) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Tell the kernel how this mapping will be touched.
    ///
    /// A failure is reported and not fatal: it is advice, and the read path is
    /// correct without it.
    pub fn advise_sequential(&self) -> io::Result<()> {
        // SAFETY: page-aligned addresses are what `madvise` wants; the base of
        // an `mmap` is page-aligned by construction.
        let rc = unsafe { madvise(self.base.cast::<c_void>(), self.len, MADV_SEQUENTIAL) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `base` came from a successful `mmap` of exactly `len` bytes
        // and has not been unmapped. `munmap`'s return value is deliberately
        // ignored: there is nothing a destructor can do about it, and the
        // address space is released on process exit either way.
        unsafe {
            munmap(self.base.cast::<c_void>(), self.len);
        }
    }
}

/// Nanoseconds from [`CLOCK_MONOTONIC_RAW`].
///
/// Returns `None` rather than panicking if the kernel refuses the clock, which
/// in practice cannot happen on the platforms this lab targets.
pub fn monotonic_raw_ns() -> Option<u64> {
    let mut ts = Timespec::default();
    // SAFETY: `ts` is a live, correctly typed, writable `struct timespec`, and
    // `CLOCK_MONOTONIC_RAW` is a valid clock id.
    let rc = unsafe { clock_gettime(CLOCK_MONOTONIC_RAW, &mut ts) };
    if rc != 0 {
        return None;
    }
    // Both fields are non-negative for this clock. A negative value would mean
    // the platform's clock started before the epoch, which MONOTONIC_RAW cannot
    // do; clamp to zero rather than wrapping into a huge number.
    let secs = u64::try_from(ts.tv_sec).ok()?;
    let nanos = u64::try_from(ts.tv_nsec).ok()?;
    Some(secs.saturating_mul(crate::NS_PER_SEC).saturating_add(nanos))
}

/// Flush a file with the strongest call the platform offers.
///
/// On macOS this is `fcntl(F_FULLFSYNC)`. `fsync` — which is what
/// `File::sync_data` and `File::sync_all` call — returns once the data has
/// reached the drive's own cache, so a power loss can still lose it. `F_FULLFSYNC`
/// asks the drive to flush that cache, and it costs milliseconds rather than
/// microseconds, which is why [`crate::wal::SyncPolicy`] makes it a choice
/// rather than the default.
///
/// On Linux there is no stronger portable call than `fdatasync` for a single
/// file, so this is `sync_data` there, and the difference is documented rather
/// than papered over.
pub fn full_fsync(file: &std::fs::File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the descriptor is open for the life of the call, and
        // `F_FULLFSYNC` takes no argument.
        let rc = unsafe { fcntl(file.as_raw_fd(), F_FULLFSYNC) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    /// Open a scratch file the way the ring does: read and write, so that a
    /// writable mapping is permitted.
    fn scratch(tag: &str, len: u64) -> (std::path::PathBuf, std::fs::File) {
        let path = std::env::temp_dir().join(format!(
            "ob-sys-{tag}-{}-{:?}.bin",
            std::process::id(),
            std::thread::current().id()
        ));
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("open scratch");
        f.set_len(len).expect("set_len");
        (path, f)
    }

    #[test]
    fn monotonic_clock_advances_and_is_sane() {
        let a = monotonic_raw_ns().expect("MONOTONIC_RAW is available");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = monotonic_raw_ns().expect("MONOTONIC_RAW is available");
        assert!(b > a, "clock must advance: a={a} b={b}");
        // A machine that reports more than ten seconds of elapsed time across a
        // two-millisecond sleep is suspending or the clock is wrong.
        assert!(b - a < 10 * crate::NS_PER_SEC, "elapsed {} ns", b - a);
    }

    #[test]
    fn mapping_round_trips_through_a_file() {
        let (path, f) = scratch("rw", 4096);
        let m = Mapping::map(&f, 4096, true).expect("map");
        assert_eq!(m.len(), 4096);
        // SAFETY: this test is the only writer of the file, and no other
        // process has it open.
        unsafe {
            m.as_mut_slice()[0] = 0xAB;
            m.as_mut_slice()[4095] = 0xCD;
            assert_eq!(m.as_slice()[0], 0xAB);
            assert_eq!(m.as_slice()[4095], 0xCD);
        }
        m.sync().expect("sync");
        drop(m);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_writable_mapping_needs_a_writable_descriptor() {
        // The contract on `Mapping::map`, pinned: a file opened write-only
        // cannot back a writable mapping, and the failure is `EACCES` rather
        // than something subtle later.
        let path = std::env::temp_dir().join(format!("ob-sys-wo-{}.bin", std::process::id()));
        let f = std::fs::File::create(&path).expect("create write-only");
        f.set_len(64).expect("set_len");
        let err = Mapping::map(&f, 64, true).expect_err("must be refused");
        assert_eq!(err.raw_os_error(), Some(13), "expected EACCES, got {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mapping_exactly_the_file_length_succeeds() {
        let (path, f) = scratch("exact", 64);
        let m = Mapping::map(&f, 64, false).expect("map exactly the file length");
        assert_eq!(m.len(), 64);
        assert!(!m.is_empty());
        drop(m);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sequential_advice_is_accepted_on_a_read_only_mapping() {
        let (path, f) = scratch("advise", 4096);
        let m = Mapping::map(&f, 4096, false).expect("map");
        m.advise_sequential().expect("madvise");
        drop(m);
        let _ = std::fs::remove_file(&path);
    }
}
