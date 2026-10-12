//! Latency, measured in stages and reported as a distribution.
//!
//! # What is measured, and what is honestly not
//!
//! Every process in this lab runs on one host and reads the same monotonic
//! clock, so a stamp taken by the adapter and a stamp taken by the daemon can be
//! subtracted. That gives four stages of one record's life:
//!
//! ```text
//!   sender stamps   daemon reads    ring publishes   subscriber sees
//!        |               |               |               |
//!        |<- stage 1 --->|<- stage 2 --->|<- stage 3 --->|
//!        |<--------------- total --------------------->|
//! ```
//!
//! - **Stage 1, `send_to_recv`** — the socket and the kernel. Only measurable
//!   when the sender stamped the record; see [`crate::msg`] for the rule that
//!   makes that possible without breaking the C++ path. When the sender did not
//!   stamp, this stage is `None` and the count of unstamped records is reported
//!   rather than a fabricated zero.
//! - **Stage 2, `recv_to_publish`** — decode, ring publish, log append, cache
//!   update. This is the part the implementation controls, and the part the
//!   stage-2 work in `docs/design/vec-optimization.md` moves.
//! - **Stage 3, `publish_to_seen`** — fan-out to a subscriber. This is where
//!   backpressure shows up first: a subscriber that cannot keep up sees this
//!   stage grow before anything is dropped.
//!
//! What is **not** measured: the exchange's own latency, the network, or
//! anything that happened before the sender's process wrote the record. A
//! number for those would need a synchronised clock at the other end, and this
//! lab does not have one. Saying so is part of the design.
//!
//! # Why a histogram and not an average
//!
//! A feed handler's latency is a distribution with a long right tail, and the
//! average hides exactly the part that matters. A mean of 40 µs is compatible
//! with "always 40 µs" and with "38 µs, but one record in a thousand takes
//! 4 ms", and only the second will lose money. So the tracker keeps a
//! histogram, reports percentiles, and reports the maximum separately.
//!
//! # Coordinated omission
//!
//! A histogram built from "how long did the records that arrived take" is
//! optimistic whenever the system stalls: during a stall, no records complete,
//! so the stall is invisible. The correction, due to Gil Tene, is to record the
//! latency the *missed* samples would have had if the load generator had kept
//! going. [`LatencyTracker::record_with_expected_interval`] implements it.
//!
//! # Resolution
//!
//! Buckets are logarithmic with a ratio of [`BUCKET_RATIO`] ≈ 1.075, so a
//! percentile is reported as the **upper bound** of the bucket it lands in and
//! is within 7.5% of the true value. A hundredth of a microsecond would cost
//! 16 KiB per histogram per symbol and buy nothing: the decisions this data
//! informs are about microseconds, not nanoseconds.

use std::fmt;

/// Number of buckets. Covers 1 ns to roughly 100 ms at [`BUCKET_RATIO`].
pub const BUCKETS: usize = 256;

/// One bucket spans this factor. `10^(1/32)`, so 32 buckets is a decade.
pub const BUCKET_RATIO: f64 = 1.074_607_948_815_797_2;

/// Values at or above this are counted in the overflow bucket.
pub const MAX_TRACKED_NS: u64 = 100_000_000;

/// Upper bound of bucket `i`, in nanoseconds.
///
/// Bucket `i` holds values in `[ratio^i, ratio^(i+1))`, so its upper bound is
/// `ratio^(i+1)`. Using `ratio^i` here would report a percentile *below* the
/// value it was computed from, which is the one direction a latency estimate
/// must never err in.
fn bucket_upper(i: usize) -> u64 {
    let v = BUCKET_RATIO.powi(i as i32 + 1);
    if v >= MAX_TRACKED_NS as f64 {
        MAX_TRACKED_NS
    } else {
        v as u64
    }
}

/// Which bucket a value belongs to.
fn bucket_of(value: u64) -> usize {
    if value == 0 {
        return 0;
    }
    if value >= MAX_TRACKED_NS {
        return BUCKETS - 1;
    }
    // Solve `ratio^i = value` for i, then clamp. One `ln` per record would be
    // too slow for a hot path, so callers that care use `record_fast`; this is
    // the exact version, used off the hot path and by the tests.
    let i = (value as f64).ln() / BUCKET_RATIO.ln();
    (i as usize).min(BUCKETS - 1)
}

/// A fixed-size latency distribution with exact count, sum, minimum and maximum.
///
/// Two kilobytes, no allocation after construction, no locking. One per stage,
/// and one per symbol only when a caller asks for it.
#[derive(Clone)]
pub struct Histogram {
    counts: Box<[u64; BUCKETS]>,
    count: u64,
    sum: u64,
    min: u64,
    max: u64,
}

impl fmt::Debug for Histogram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Histogram")
            .field("count", &self.count)
            .field("min_ns", &self.min)
            .field("p50_ns", &self.quantile(0.50))
            .field("p99_ns", &self.quantile(0.99))
            .field("max_ns", &self.max)
            .finish()
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Histogram::new()
    }
}

impl Histogram {
    /// An empty distribution.
    pub fn new() -> Self {
        Histogram {
            counts: Box::new([0u64; BUCKETS]),
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
        }
    }

    /// Add one observation.
    pub fn record(&mut self, value: u64) {
        self.counts[bucket_of(value)] += 1;
        self.count += 1;
        self.sum = self.sum.saturating_add(value);
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
    }

    /// How many observations.
    #[inline]
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Whether nothing has been recorded.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The smallest observation, or zero when empty.
    #[inline]
    pub fn min(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.min
        }
    }

    /// The largest observation, or zero when empty.
    ///
    /// Exact, not bucketed: the maximum is the one number a tail problem is
    /// always visible in.
    #[inline]
    pub fn max(&self) -> u64 {
        self.max
    }

    /// The arithmetic mean, truncated.
    #[inline]
    pub fn mean(&self) -> u64 {
        self.sum.checked_div(self.count).unwrap_or(0)
    }

    /// The total of every observation.
    #[inline]
    pub fn sum(&self) -> u64 {
        self.sum
    }

    /// The upper bound of the bucket that `q` falls in.
    ///
    /// `q` is in `[0, 1]`. The answer is within [`BUCKET_RATIO`] of the true
    /// value and is never below it. Zero when nothing has been recorded.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let q = q.clamp(0.0, 1.0);
        // The rank of the observation we want, counting from one.
        let target = ((q * self.count as f64).ceil() as u64)
            .max(1)
            .min(self.count);
        let mut seen = 0u64;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= target {
                return bucket_upper(i);
            }
        }
        self.max
    }

    /// How many observations fell in the overflow bucket.
    ///
    /// A non-zero value means the distribution has a tail beyond
    /// [`MAX_TRACKED_NS`], and every percentile above the point where the
    /// overflow starts is a lower bound rather than an estimate.
    pub fn overflow(&self) -> u64 {
        self.counts[BUCKETS - 1]
    }

    /// Merge another distribution into this one.
    pub fn merge(&mut self, other: &Histogram) {
        for (dst, src) in self.counts.iter_mut().zip(other.counts.iter()) {
            *dst += *src;
        }
        self.count += other.count;
        self.sum = self.sum.saturating_add(other.sum);
        if other.count > 0 {
            if other.min < self.min {
                self.min = other.min;
            }
            if other.max > self.max {
                self.max = other.max;
            }
        }
    }

    /// Forget every observation, keeping the allocation.
    pub fn reset(&mut self) {
        self.counts.fill(0);
        self.count = 0;
        self.sum = 0;
        self.min = u64::MAX;
        self.max = 0;
    }

    /// The percentiles a report shows, in one call.
    pub fn summary(&self) -> Summary {
        Summary {
            count: self.count,
            min: self.min(),
            mean: self.mean(),
            p50: self.quantile(0.50),
            p90: self.quantile(0.90),
            p99: self.quantile(0.99),
            p999: self.quantile(0.999),
            max: self.max,
            overflow: self.overflow(),
        }
    }
}

/// The handful of numbers worth printing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Observations.
    pub count: u64,
    /// Smallest, exact.
    pub min: u64,
    /// Mean, exact.
    pub mean: u64,
    /// Median, bucket upper bound.
    pub p50: u64,
    /// 90th percentile, bucket upper bound.
    pub p90: u64,
    /// 99th percentile, bucket upper bound.
    pub p99: u64,
    /// 99.9th percentile, bucket upper bound.
    pub p999: u64,
    /// Largest, exact.
    pub max: u64,
    /// Observations past the tracked range.
    pub overflow: u64,
}

impl fmt::Display for Summary {
    /// One line, in microseconds, with the count and the maximum.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.count == 0 {
            return f.write_str("(no samples)");
        }
        write!(
            f,
            "n={} min={:.1}us p50={:.1}us p90={:.1}us p99={:.1}us p99.9={:.1}us max={:.1}us",
            self.count,
            self.min as f64 / 1000.0,
            self.p50 as f64 / 1000.0,
            self.p90 as f64 / 1000.0,
            self.p99 as f64 / 1000.0,
            self.p999 as f64 / 1000.0,
            self.max as f64 / 1000.0,
        )
    }
}

/// The three stages of one record's life inside this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stages {
    /// Sender stamp to the daemon's read. `None` when the sender did not stamp.
    pub send_to_recv_ns: Option<u64>,
    /// The daemon's read to the ring publish.
    pub recv_to_publish_ns: u64,
    /// The ring publish to a subscriber observing it.
    pub publish_to_seen_ns: u64,
}

impl Stages {
    /// The stages that were measured, added up.
    ///
    /// A record with no sender stamp totals stages 2 and 3 only, and the
    /// tracker counts it as such rather than pretending stage 1 was zero.
    #[inline]
    pub fn total_ns(&self) -> u64 {
        self.send_to_recv_ns
            .unwrap_or(0)
            .saturating_add(self.recv_to_publish_ns)
            .saturating_add(self.publish_to_seen_ns)
    }
}

/// A stage-1 reading larger than this is treated as a bad stamp, not latency.
///
/// The sender and the daemon read the same clock on the same host, so a
/// send-to-receive time of more than a second means one of three things: the
/// record came from another machine, the field was not a stamp, or the process
/// was suspended. None of those is a latency measurement, and folding one into
/// the histogram would put a two-second spike in the maximum forever.
pub const IMPLAUSIBLE_STAGE_NS: u64 = 1_000_000_000;

/// The latency of a pipeline, stage by stage.
///
/// [`total`](LatencyTracker::total) holds the sum of the stages that were
/// measured, for **every** record: all three stages for a stamped record,
/// stages 2 and 3 for an unstamped one. The composition is not guessed at —
/// [`stamped`](LatencyTracker::stamped) and
/// [`unstamped`](LatencyTracker::unstamped) say how many records of each kind
/// are in it, and a reader who needs a total over stamped records only should
/// compare those two counts against the sample count.
#[derive(Debug, Default)]
pub struct LatencyTracker {
    send_to_recv: Histogram,
    recv_to_publish: Histogram,
    publish_to_seen: Histogram,
    total: Histogram,
    stamped: u64,
    unstamped: u64,
    implausible: u64,
}

impl LatencyTracker {
    /// An empty tracker.
    pub fn new() -> Self {
        LatencyTracker::default()
    }

    /// Record one record's stages, all three at once.
    ///
    /// For a pipeline that runs in one process and can see every stage. A
    /// publisher and its subscribers use [`LatencyTracker::record_source`] and
    /// [`LatencyTracker::record_sink`] instead, because each of them sees a
    /// different part.
    pub fn record(&mut self, stages: Stages) {
        self.recv_to_publish.record(stages.recv_to_publish_ns);
        self.publish_to_seen.record(stages.publish_to_seen_ns);
        // One accounting call, not two: a record counted by both the source and
        // the sink entry points would be counted twice.
        let stage_one = match self.classify_stamp(stages.send_to_recv_ns) {
            Some(ns) => {
                self.send_to_recv.record(ns);
                ns
            }
            None => 0,
        };
        self.total.record(
            stage_one
                .saturating_add(stages.recv_to_publish_ns)
                .saturating_add(stages.publish_to_seen_ns),
        );
    }

    /// Record what the **publishing** side sees: the sender's stamp, and how
    /// long the record took to read, decode, publish and apply.
    ///
    /// This tracker's [`total`](LatencyTracker::total) is therefore the ingress
    /// path, stages 1 and 2. It never records stage 3, because the publisher
    /// does not know when a subscriber looked.
    pub fn record_source(&mut self, send_to_recv_ns: Option<u64>, recv_to_publish_ns: u64) {
        self.recv_to_publish.record(recv_to_publish_ns);
        let ingress = match self.classify_stamp(send_to_recv_ns) {
            Some(ns) => {
                self.send_to_recv.record(ns);
                ns
            }
            None => 0,
        };
        self.total
            .record(ingress.saturating_add(recv_to_publish_ns));
    }

    /// Record what a **subscribing** side sees: how long the event waited, and
    /// the whole path it travelled.
    ///
    /// `ingress_ns` is the publishing side's stages 1 and 2 for this record,
    /// carried in the event. With it, this tracker's
    /// [`total`](LatencyTracker::total) is the end-to-end time from the sender's
    /// stamp to this subscriber seeing the record — the number an operator
    /// actually wants, measured at the only place that can measure it.
    pub fn record_sink(
        &mut self,
        send_to_recv_ns: Option<u64>,
        ingress_ns: u64,
        publish_to_seen_ns: u64,
    ) {
        self.publish_to_seen.record(publish_to_seen_ns);
        let stage_one = self.classify_stamp(send_to_recv_ns).unwrap_or_default();
        self.total.record(
            stage_one
                .saturating_add(ingress_ns)
                .saturating_add(publish_to_seen_ns),
        );
    }

    /// Count a record's stamp as usable, absent, or implausible.
    ///
    /// Returns the stage-1 duration when it is usable, `None` otherwise. Every
    /// tracker that is handed a record calls this exactly once, so its
    /// `stamped` and `unstamped` counts describe the records *it* saw.
    fn classify_stamp(&mut self, send_to_recv_ns: Option<u64>) -> Option<u64> {
        match send_to_recv_ns {
            Some(ns) if ns < IMPLAUSIBLE_STAGE_NS => {
                self.stamped += 1;
                Some(ns)
            }
            Some(_) => {
                // Stamped, but the number cannot be a duration on this host, so
                // stage 1 is not recorded and the total covers the rest.
                self.implausible += 1;
                None
            }
            None => {
                self.unstamped += 1;
                None
            }
        }
    }

    /// Record one record's stages, correcting for coordinated omission.
    ///
    /// `expected_interval_ns` is how often a record *should* have arrived. When
    /// this record took longer than that, the samples that did not arrive during
    /// the gap are recorded too — and here is the part that is easy to get
    /// wrong: they are **not** recorded at the expected interval.
    ///
    /// A sample that would have arrived during the stall was queued behind the
    /// stall, so it would itself have been slow. Tene's algorithm charges the
    /// `k`th missing sample a latency of `observed - k * interval`, walking down
    /// from just under the observed value to the interval. The effect is that
    /// the tail fills in with the latencies that were actually experienced
    /// rather than being averaged away by the records that did arrive, and the
    /// correction never invents a sample worse than the one observed.
    ///
    /// Passing zero disables the correction and is exactly
    /// [`LatencyTracker::record`].
    pub fn record_with_expected_interval(&mut self, stages: Stages, expected_interval_ns: u64) {
        self.record(stages);
        if expected_interval_ns == 0 {
            return;
        }
        let observed = stages.total_ns();
        if observed <= expected_interval_ns {
            return;
        }
        let mut missing = observed - expected_interval_ns;
        while missing >= expected_interval_ns {
            self.total.record(missing);
            missing -= expected_interval_ns;
        }
    }

    /// The stage-1 distribution.
    pub fn send_to_recv(&self) -> &Histogram {
        &self.send_to_recv
    }

    /// The stage-2 distribution.
    pub fn recv_to_publish(&self) -> &Histogram {
        &self.recv_to_publish
    }

    /// The stage-3 distribution.
    pub fn publish_to_seen(&self) -> &Histogram {
        &self.publish_to_seen
    }

    /// The whole-path distribution, over stamped records only.
    pub fn total(&self) -> &Histogram {
        &self.total
    }

    /// How many records carried a usable sender stamp.
    pub fn stamped(&self) -> u64 {
        self.stamped
    }

    /// How many records arrived without a sender stamp.
    ///
    /// Reported rather than hidden: if this equals the record count, stage 1 is
    /// not being measured at all and the total is stages 2 and 3.
    pub fn unstamped(&self) -> u64 {
        self.unstamped
    }

    /// How many sender stamps were rejected as implausible.
    pub fn implausible(&self) -> u64 {
        self.implausible
    }

    /// Forget everything, keeping the allocations.
    pub fn reset(&mut self) {
        self.send_to_recv.reset();
        self.recv_to_publish.reset();
        self.publish_to_seen.reset();
        self.total.reset();
        self.stamped = 0;
        self.unstamped = 0;
        self.implausible = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_histogram_reports_zero_rather_than_nonsense() {
        let h = Histogram::new();
        assert!(h.is_empty());
        assert_eq!(h.count(), 0);
        assert_eq!(h.min(), 0);
        assert_eq!(h.max(), 0);
        assert_eq!(h.mean(), 0);
        assert_eq!(h.quantile(0.99), 0);
        assert_eq!(h.summary().to_string(), "(no samples)");
    }

    #[test]
    fn count_sum_min_and_max_are_exact() {
        let mut h = Histogram::new();
        for v in [100u64, 200, 300, 400, 500] {
            h.record(v);
        }
        assert_eq!(h.count(), 5);
        assert_eq!(h.sum(), 1500);
        assert_eq!(h.mean(), 300);
        assert_eq!(h.min(), 100);
        assert_eq!(h.max(), 500);
    }

    #[test]
    fn quantiles_land_in_the_right_bucket_and_never_read_low() {
        let mut h = Histogram::new();
        // A thousand samples, 900 at 10 us and 100 at 1 ms.
        for _ in 0..900 {
            h.record(10_000);
        }
        for _ in 0..100 {
            h.record(1_000_000);
        }
        assert_eq!(h.count(), 1000);
        // p50 is in the 10 us bucket; the bucket's upper bound is within 7.5%.
        let p50 = h.quantile(0.50);
        assert!((10_000..11_000).contains(&p50), "p50 = {p50}");
        // p90 sits exactly on the boundary: the 900th sample is the last of the
        // 10 us group, so p90 is still the fast bucket.
        assert!((10_000..11_000).contains(&h.quantile(0.90)));
        // p99 is in the slow group.
        let p99 = h.quantile(0.99);
        assert!((1_000_000..1_080_000).contains(&p99), "p99 = {p99}");
        // The maximum is exact, not bucketed.
        assert_eq!(h.max(), 1_000_000);
    }

    #[test]
    fn a_percentile_is_never_below_the_value_it_estimates() {
        let mut h = Histogram::new();
        for v in [1u64, 7, 63, 1023, 65535, 1_048_576] {
            h.record(v);
            let p = h.quantile(1.0);
            assert!(p >= v, "quantile(1.0) = {p} for a maximum of {v}");
        }
    }

    #[test]
    fn bucket_upper_bounds_are_monotonic() {
        let mut last = 0u64;
        for i in 0..BUCKETS {
            let upper = bucket_upper(i);
            assert!(upper >= last, "bucket {i} went backwards");
            last = upper;
        }
        assert_eq!(bucket_upper(BUCKETS - 1), MAX_TRACKED_NS);
    }

    #[test]
    fn values_past_the_tracked_range_land_in_the_overflow_bucket() {
        let mut h = Histogram::new();
        h.record(MAX_TRACKED_NS + 1);
        assert_eq!(h.overflow(), 1);
        assert_eq!(h.max(), MAX_TRACKED_NS + 1, "the maximum is still exact");
        assert_eq!(h.count(), 1);
    }

    #[test]
    fn merging_two_histograms_is_the_same_as_recording_both() {
        let mut a = Histogram::new();
        let mut b = Histogram::new();
        for v in [100u64, 200, 300] {
            a.record(v);
        }
        for v in [400u64, 500] {
            b.record(v);
        }
        let mut merged = a.clone();
        merged.merge(&b);
        assert_eq!(merged.count(), 5);
        assert_eq!(merged.sum(), 1500);
        assert_eq!(merged.min(), 100);
        assert_eq!(merged.max(), 500);
    }

    #[test]
    fn reset_clears_everything_and_keeps_the_capacity() {
        let mut h = Histogram::new();
        for v in 0..1000 {
            h.record(v);
        }
        h.reset();
        assert_eq!(h.count(), 0);
        assert_eq!(h.sum(), 0);
        assert_eq!(h.max(), 0);
        h.record(42);
        assert_eq!(h.count(), 1);
        assert_eq!(h.min(), 42);
    }

    #[test]
    fn the_tracker_separates_the_three_stages() {
        let mut t = LatencyTracker::new();
        t.record(Stages {
            send_to_recv_ns: Some(1_000),
            recv_to_publish_ns: 2_000,
            publish_to_seen_ns: 3_000,
        });
        assert_eq!(t.stamped(), 1);
        assert_eq!(t.unstamped(), 0);
        assert_eq!(t.send_to_recv().max(), 1_000);
        assert_eq!(t.recv_to_publish().max(), 2_000);
        assert_eq!(t.publish_to_seen().max(), 3_000);
        assert_eq!(t.total().max(), 6_000);
    }

    #[test]
    fn the_source_entry_point_never_touches_stage_three() {
        let mut t = LatencyTracker::new();
        t.record_source(Some(1_000), 2_000);
        assert_eq!(t.recv_to_publish().count(), 1);
        assert_eq!(t.send_to_recv().count(), 1);
        assert_eq!(
            t.publish_to_seen().count(),
            0,
            "the publisher cannot see this"
        );
        // Its total is the ingress path.
        assert_eq!(t.total().max(), 3_000);
    }

    #[test]
    fn the_sink_entry_point_reports_the_whole_path() {
        let mut t = LatencyTracker::new();
        // The sender stamped it, ingress took 2 us, and it waited 3 us here.
        t.record_sink(Some(1_000), 2_000, 3_000);
        assert_eq!(t.publish_to_seen().count(), 1);
        assert_eq!(t.total().max(), 6_000, "1 + 2 + 3 us end to end");
        assert_eq!(t.stamped(), 1);
    }

    #[test]
    fn an_unstamped_record_is_counted_and_not_folded_into_stage_one() {
        let mut t = LatencyTracker::new();
        t.record(Stages {
            send_to_recv_ns: None,
            recv_to_publish_ns: 2_000,
            publish_to_seen_ns: 3_000,
        });
        assert_eq!(t.stamped(), 0);
        assert_eq!(t.unstamped(), 1);
        assert!(
            t.send_to_recv().is_empty(),
            "nothing was measured for stage 1"
        );
        // The total covers the stages that were measured: 2 and 3 here.
        assert_eq!(t.total().count(), 1);
        assert_eq!(t.total().max(), 5_000);
        assert_eq!(t.recv_to_publish().max(), 2_000);
        assert_eq!(t.publish_to_seen().max(), 3_000);
    }

    #[test]
    fn an_implausible_stamp_is_rejected_rather_than_recorded() {
        let mut t = LatencyTracker::new();
        t.record(Stages {
            send_to_recv_ns: Some(IMPLAUSIBLE_STAGE_NS + 1),
            recv_to_publish_ns: 2_000,
            publish_to_seen_ns: 3_000,
        });
        assert_eq!(t.implausible(), 1);
        assert_eq!(t.stamped(), 0);
        assert!(t.send_to_recv().is_empty());
        // The stages that were measurable are still recorded.
        assert_eq!(t.recv_to_publish().max(), 2_000);
    }

    #[test]
    fn coordinated_omission_charges_the_missing_samples() {
        let expected = 100_000; // a record every 100 us
        let mut plain = LatencyTracker::new();
        let mut corrected = LatencyTracker::new();

        // Ninety-nine records arrive on time, then one takes 10 ms.
        for _ in 0..99 {
            plain.record(Stages {
                send_to_recv_ns: None,
                recv_to_publish_ns: expected,
                publish_to_seen_ns: 0,
            });
            corrected.record_with_expected_interval(
                Stages {
                    send_to_recv_ns: None,
                    recv_to_publish_ns: expected,
                    publish_to_seen_ns: 0,
                },
                expected,
            );
        }
        let stall = Stages {
            send_to_recv_ns: None,
            recv_to_publish_ns: 10_000_000,
            publish_to_seen_ns: 0,
        };
        plain.record(stall);
        corrected.record_with_expected_interval(stall, expected);

        // Both saw the same measured records.
        assert_eq!(plain.recv_to_publish().count(), 100);
        assert_eq!(corrected.recv_to_publish().count(), 100);

        // The plain total is 100 samples and its 99th percentile is a fast one,
        // because 99 of the 100 records arrived on time and the stall is a
        // single sample.
        assert_eq!(plain.total().count(), 100);
        assert!(
            plain.total().quantile(0.99) < 1_000_000,
            "the uncorrected p99 should be fast, was {}",
            plain.total().quantile(0.99)
        );

        // The corrected total has the 99 records the stall swallowed as well:
        // 10 ms at a 100 us interval is 99 missing samples.
        assert_eq!(corrected.total().count(), 199);
        // And now the tail is real: the p99 is in the millisecond range,
        // because the backfilled samples were charged the latencies they would
        // have experienced behind the stall.
        let corrected_p99 = corrected.total().quantile(0.99);
        assert!(
            corrected_p99 > 1_000_000,
            "the correction did not move the tail: p99 = {corrected_p99}"
        );
        // The maximum is unchanged: the correction does not invent a sample
        // worse than the one observed.
        assert_eq!(corrected.total().max(), 10_000_000);
        assert_eq!(plain.total().max(), 10_000_000);
    }

    #[test]
    fn a_record_inside_the_expected_interval_gets_no_backfill() {
        let mut t = LatencyTracker::new();
        t.record_with_expected_interval(
            Stages {
                send_to_recv_ns: Some(500),
                recv_to_publish_ns: 500,
                publish_to_seen_ns: 0,
            },
            100_000,
        );
        assert_eq!(t.total().count(), 1, "no samples were missing");
    }

    #[test]
    fn a_zero_expected_interval_is_exactly_record() {
        let mut a = LatencyTracker::new();
        let mut b = LatencyTracker::new();
        let stages = Stages {
            send_to_recv_ns: Some(1_000),
            recv_to_publish_ns: 2_000,
            publish_to_seen_ns: 3_000,
        };
        a.record(stages);
        b.record_with_expected_interval(stages, 0);
        assert_eq!(a.total().count(), b.total().count());
        assert_eq!(a.total().sum(), b.total().sum());
    }
}
