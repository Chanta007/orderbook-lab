//! One clock, one unit, one domain.
//!
//! Every time in this crate is `u64` nanoseconds from `CLOCK_MONOTONIC_RAW`,
//! taken on the host that produced it. That single choice is what makes the
//! latency numbers in [`crate::latency`] mean something:
//!
//! - **Monotonic** — the clock never steps, so a subtraction is always a real
//!   elapsed time. `SystemTime` would occasionally produce a negative latency
//!   after an NTP correction, and a negative latency that is quietly clamped is
//!   worse than no number.
//! - **Raw** — no NTP slew is applied, so two readings taken around a
//!   correction are still comparable.
//! - **Host-wide** — every process on this machine reads the same clock, so a
//!   stamp taken by the adapter and a stamp taken by `feedd` can be subtracted.
//!   This is why the lab can report one-way latency at all without a
//!   synchronisation protocol: the two ends are on one host, and the lab is
//!   honest that this stops being true the moment they are not.
//!
//! Nothing here is wall-clock time. A timestamp in this crate cannot be printed
//! as a date, and that is deliberate: adding a calendar to a hot path costs a
//! syscall, and there is no field for one in the wire record.

use crate::sys;

/// Read the monotonic clock, in nanoseconds.
///
/// # Cost
///
/// One `clock_gettime` that the kernel satisfies from a vDSO-equivalent fast
/// path on both macOS and Linux: tens of nanoseconds, no syscall trap. It is
/// still a function call the compiler cannot hoist, so the hot loops call it
/// once per batch rather than once per record, and the code says where.
///
/// # Panics
///
/// Panics if the platform does not provide `CLOCK_MONOTONIC_RAW`. Both targets
/// do; a panic here would be detected by the first test that runs, and silently
/// returning zero would poison every latency number in the program.
#[inline]
pub fn now_ns() -> u64 {
    match sys::monotonic_raw_ns() {
        Some(ns) => ns,
        None => unreachable!("CLOCK_MONOTONIC_RAW is available on macOS and Linux"),
    }
}

/// A start time, so that a duration is one subtraction at the end.
///
/// Prefer this to two calls to [`now_ns`] at the top and bottom of a function:
/// the type makes it impossible to pair the wrong two readings.
#[derive(Debug, Clone, Copy)]
pub struct Since(u64);

impl Since {
    /// Start a measurement now.
    #[inline]
    pub fn start() -> Self {
        Since(now_ns())
    }

    /// Nanoseconds elapsed since [`Since::start`].
    #[inline]
    pub fn elapsed_ns(self) -> u64 {
        now_ns().saturating_sub(self.0)
    }

    /// The stamp this measurement started at.
    #[inline]
    pub fn started_at(self) -> u64 {
        self.0
    }
}

/// Difference of two stamps, saturating at zero.
///
/// Saturating rather than wrapping is a deliberate loss: if `later` is somehow
/// earlier, the honest answer is "unknown", and a caller can detect that case
/// with [`is_ordered`] rather than receiving 18 quintillion nanoseconds.
#[inline]
pub fn delta_ns(earlier: u64, later: u64) -> u64 {
    later.saturating_sub(earlier)
}

/// Whether two stamps are in the order the caller expects.
///
/// Used by the codec and the cache to reject a record whose sender stamp is
/// ahead of the receiver's clock, which means the sender was not on this host
/// or the field was not a stamp at all.
#[inline]
pub fn is_ordered(earlier: u64, later: u64) -> bool {
    later >= earlier
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_measures_a_known_sleep() {
        let s = Since::start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let e = s.elapsed_ns();
        assert!(e >= 4_000_000, "expected at least 4 ms, got {e} ns");
        assert!(e < 500_000_000, "expected under 500 ms, got {e} ns");
    }

    #[test]
    fn delta_saturates_instead_of_wrapping() {
        assert_eq!(delta_ns(100, 40), 0);
        assert!(!is_ordered(100, 40));
        assert!(is_ordered(40, 100));
        assert!(is_ordered(40, 40));
    }

    #[test]
    fn two_reads_never_go_backwards() {
        let mut prev = now_ns();
        for _ in 0..10_000 {
            let next = now_ns();
            assert!(next >= prev, "clock went backwards: {prev} then {next}");
            prev = next;
        }
    }
}
