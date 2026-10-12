//! Backpressure, as a state you can alert on rather than a number you have to
//! interpret.
//!
//! # The problem with reporting queue depth
//!
//! A queue that is 60% full is fine in a system whose consumer runs every
//! microsecond and alarming in one whose consumer runs every second. Depth alone
//! says nothing, and a threshold on depth produces alerts that are either
//! constant or useless.
//!
//! What actually matters is **lag**: how far behind the producer the consumer is,
//! measured in time. A consumer that is 200 µs behind is keeping up with a
//! 100 µs feed; one that is 20 ms behind has already lost data or is about to.
//! Lag is comparable across symbols, across queue sizes, and across machines,
//! which is why every state in this module is derived from it.
//!
//! # Three states, with hysteresis
//!
//! ```text
//!   Nominal   lag < elevated_ns
//!   Elevated  elevated_ns <= lag < saturated_ns
//!   Saturated lag >= saturated_ns
//! ```
//!
//! Leaving a state needs the lag to fall to **half** the threshold that was
//! crossed. Without that, a lag hovering exactly on a threshold flips the state
//! on every sample, and a counter of state changes becomes a measure of jitter
//! rather than of incidents.
//!
//! # What a state change is for
//!
//! Each transition is counted, and the time spent in each state is accumulated.
//! A run that reports "Saturated for 1.2 s across 3 events" tells an operator
//! something actionable. A run that reports "maximum queue depth 4096" does not.

use crate::latency::Histogram;

/// How hard the pipeline is being pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureState {
    /// Keeping up comfortably.
    Nominal,
    /// Falling behind. Data may still be flowing in order, but the margin is
    /// gone.
    Elevated,
    /// Falling behind faster than the consumer can recover. Records are being
    /// evicted or dropped, and every latency number for this period is a
    /// measurement of a system in trouble.
    Saturated,
}

impl PressureState {
    /// A short label for a report line.
    pub fn label(self) -> &'static str {
        match self {
            PressureState::Nominal => "nominal",
            PressureState::Elevated => "elevated",
            PressureState::Saturated => "saturated",
        }
    }
}

/// The thresholds that separate the states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressureConfig {
    /// Lag at which the state becomes [`PressureState::Elevated`].
    pub elevated_ns: u64,
    /// Lag at which the state becomes [`PressureState::Saturated`].
    pub saturated_ns: u64,
    /// Whether to collect the lag distribution.
    pub enabled: bool,
}

impl Default for PressureConfig {
    /// One millisecond to elevated, ten to saturated.
    ///
    /// The numbers come from the shape of this lab rather than from a standard:
    /// the feed publishes a picture every 100 ms per symbol, a consumer polling
    /// on a 10 ms timer should never be more than a few milliseconds behind, and
    /// a lag of 10 ms means a consumer has missed at least one poll. A different
    /// deployment should change them; the point is that they are stated in time.
    fn default() -> Self {
        PressureConfig {
            elevated_ns: 1_000_000,
            saturated_ns: 10_000_000,
            enabled: true,
        }
    }
}

/// What one sample saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// The lag measured, in nanoseconds.
    pub lag_ns: u64,
    /// Records waiting in the queue.
    pub queue_len: usize,
    /// The queue's capacity.
    pub queue_capacity: usize,
}

/// Backpressure history for one consumer.
#[derive(Debug)]
pub struct PressureTracker {
    cfg: PressureConfig,
    state: PressureState,
    /// When the current state was entered.
    state_since_ns: u64,
    /// Nanoseconds accumulated in each state, indexed by state order.
    spent_ns: [u64; 3],
    /// How many times the state changed.
    transitions: u64,
    /// How many times the state entered `Saturated`.
    saturation_events: u64,
    /// The largest lag ever sampled, with its time.
    peak_lag_ns: u64,
    /// The largest queue occupancy ever sampled.
    peak_queue: u64,
    /// Samples taken.
    samples: u64,
    /// The lag distribution.
    lag: Histogram,
}

impl Default for PressureTracker {
    fn default() -> Self {
        PressureTracker::new(PressureConfig::default())
    }
}

impl PressureTracker {
    /// A tracker in [`PressureState::Nominal`].
    pub fn new(cfg: PressureConfig) -> Self {
        PressureTracker {
            cfg,
            state: PressureState::Nominal,
            state_since_ns: 0,
            spent_ns: [0; 3],
            transitions: 0,
            saturation_events: 0,
            peak_lag_ns: 0,
            peak_queue: 0,
            samples: 0,
            lag: Histogram::new(),
        }
    }

    /// The configuration.
    pub fn config(&self) -> PressureConfig {
        self.cfg
    }

    /// The state as of the last sample.
    #[inline]
    pub fn state(&self) -> PressureState {
        self.state
    }

    /// How many times the state changed.
    #[inline]
    pub fn transitions(&self) -> u64 {
        self.transitions
    }

    /// How many times the pipeline entered `Saturated`.
    #[inline]
    pub fn saturation_events(&self) -> u64 {
        self.saturation_events
    }

    /// The largest lag ever sampled.
    #[inline]
    pub fn peak_lag_ns(&self) -> u64 {
        self.peak_lag_ns
    }

    /// The lag distribution.
    #[inline]
    pub fn lag(&self) -> &Histogram {
        &self.lag
    }

    /// Take one sample and return the state it implies.
    ///
    /// Call this once per drain, not once per record: the state is a property of
    /// the pipeline over time, and sampling it per record would make the
    /// transition counter measure record volume.
    pub fn sample(&mut self, now_ns: u64, s: Sample) -> PressureState {
        if !self.cfg.enabled {
            return self.state;
        }
        self.samples += 1;
        self.lag.record(s.lag_ns);
        if s.lag_ns > self.peak_lag_ns {
            self.peak_lag_ns = s.lag_ns;
        }
        if s.queue_len as u64 > self.peak_queue {
            self.peak_queue = s.queue_len as u64;
        }

        let next = self.classify(s.lag_ns);
        if next != self.state {
            // Charge the elapsed time to the state being left.
            let elapsed = now_ns.saturating_sub(self.state_since_ns);
            self.spent_ns[self.state as usize] += elapsed;
            if next == PressureState::Saturated {
                self.saturation_events += 1;
            }
            self.transitions += 1;
            self.state = next;
            self.state_since_ns = now_ns;
        }
        self.state
    }

    /// Which state a lag implies, applying the hysteresis rule.
    fn classify(&self, lag_ns: u64) -> PressureState {
        let half = |t: u64| t / 2;
        match self.state {
            PressureState::Nominal => {
                if lag_ns >= self.cfg.saturated_ns {
                    PressureState::Saturated
                } else if lag_ns >= self.cfg.elevated_ns {
                    PressureState::Elevated
                } else {
                    PressureState::Nominal
                }
            }
            PressureState::Elevated => {
                if lag_ns >= self.cfg.saturated_ns {
                    PressureState::Saturated
                } else if lag_ns < half(self.cfg.elevated_ns) {
                    PressureState::Nominal
                } else {
                    PressureState::Elevated
                }
            }
            PressureState::Saturated => {
                if lag_ns < half(self.cfg.saturated_ns) {
                    if lag_ns < half(self.cfg.elevated_ns) {
                        PressureState::Nominal
                    } else {
                        PressureState::Elevated
                    }
                } else {
                    PressureState::Saturated
                }
            }
        }
    }

    /// Charge the time since the last sample to the current state.
    ///
    /// Called once at the end of a run so that the final state's duration is
    /// counted. Without it, a run that ends saturated reports zero time
    /// saturated, which is the most misleading answer available.
    pub fn close(&mut self, now_ns: u64) {
        if !self.cfg.enabled {
            return;
        }
        let elapsed = now_ns.saturating_sub(self.state_since_ns);
        self.spent_ns[self.state as usize] += elapsed;
        self.state_since_ns = now_ns;
    }

    /// The accumulated report.
    pub fn report(&self) -> PressureReport {
        PressureReport {
            state: self.state,
            samples: self.samples,
            transitions: self.transitions,
            saturation_events: self.saturation_events,
            peak_lag_ns: self.peak_lag_ns,
            peak_queue: self.peak_queue,
            nominal_ns: self.spent_ns[PressureState::Nominal as usize],
            elevated_ns: self.spent_ns[PressureState::Elevated as usize],
            saturated_ns: self.spent_ns[PressureState::Saturated as usize],
            lag: self.lag.summary(),
        }
    }
}

/// A snapshot of the pressure history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressureReport {
    /// The state at the end of the run.
    pub state: PressureState,
    /// Samples taken.
    pub samples: u64,
    /// State changes.
    pub transitions: u64,
    /// Times the pipeline entered `Saturated`.
    pub saturation_events: u64,
    /// The largest lag sampled.
    pub peak_lag_ns: u64,
    /// The largest queue occupancy sampled.
    pub peak_queue: u64,
    /// Time spent in `Nominal`.
    pub nominal_ns: u64,
    /// Time spent in `Elevated`.
    pub elevated_ns: u64,
    /// Time spent in `Saturated`.
    pub saturated_ns: u64,
    /// The lag distribution.
    pub lag: crate::latency::Summary,
}

impl PressureReport {
    /// Total time accounted for.
    pub fn total_ns(&self) -> u64 {
        self.nominal_ns
            .saturating_add(self.elevated_ns)
            .saturating_add(self.saturated_ns)
    }

    /// The fraction of accounted time spent saturated, in hundredths of a
    /// percent.
    ///
    /// This is the number worth alerting on: it is dimensionless, comparable
    /// between runs, and non-zero only when records were actually at risk.
    pub fn saturated_pct_x100(&self) -> u64 {
        let total = self.total_ns();
        if total == 0 {
            return 0;
        }
        (self.saturated_ns.saturating_mul(10_000)) / total
    }
}

impl std::fmt::Display for PressureReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pressure={} samples={} transitions={} saturation_events={} \
             saturated={:.3}% peak_lag={:.3}ms peak_queue={} | lag: {}",
            self.state.label(),
            self.samples,
            self.transitions,
            self.saturation_events,
            self.saturated_pct_x100() as f64 / 100.0,
            self.peak_lag_ns as f64 / 1_000_000.0,
            self.peak_queue,
            self.lag,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_at(lag_ns: u64) -> Sample {
        Sample {
            lag_ns,
            queue_len: 0,
            queue_capacity: 1024,
        }
    }

    fn cfg() -> PressureConfig {
        PressureConfig {
            elevated_ns: 1_000,
            saturated_ns: 10_000,
            enabled: true,
        }
    }

    #[test]
    fn a_comfortable_run_stays_nominal() {
        let mut t = PressureTracker::new(cfg());
        for i in 0..100 {
            assert_eq!(t.sample(i * 100, sample_at(50)), PressureState::Nominal);
        }
        assert_eq!(t.transitions(), 0);
        let r = t.report();
        assert_eq!(r.saturation_events, 0);
        assert_eq!(r.saturated_pct_x100(), 0);
        assert_eq!(r.peak_lag_ns, 50);
    }

    #[test]
    fn crossing_a_threshold_changes_the_state_once() {
        let mut t = PressureTracker::new(cfg());
        assert_eq!(t.sample(0, sample_at(0)), PressureState::Nominal);
        assert_eq!(t.sample(1, sample_at(2_000)), PressureState::Elevated);
        assert_eq!(t.transitions(), 1);
        // Staying above the threshold does not count again.
        assert_eq!(t.sample(2, sample_at(3_000)), PressureState::Elevated);
        assert_eq!(t.transitions(), 1);
        assert_eq!(t.sample(3, sample_at(20_000)), PressureState::Saturated);
        assert_eq!(t.transitions(), 2);
        assert_eq!(t.saturation_events(), 1);
    }

    #[test]
    fn hysteresis_stops_a_lag_on_the_threshold_from_flapping() {
        let mut t = PressureTracker::new(cfg());
        t.sample(0, sample_at(0));
        // Enter Elevated at exactly the threshold.
        assert_eq!(t.sample(1, sample_at(1_000)), PressureState::Elevated);
        // A lag just under the threshold does NOT return to Nominal: it has to
        // fall to half.
        assert_eq!(t.sample(2, sample_at(999)), PressureState::Elevated);
        assert_eq!(t.sample(3, sample_at(600)), PressureState::Elevated);
        assert_eq!(t.sample(4, sample_at(499)), PressureState::Nominal);
        assert_eq!(t.transitions(), 2, "one up, one down, and no flapping");
    }

    #[test]
    fn a_saturated_pipeline_returns_through_elevated_not_straight_to_nominal() {
        let mut t = PressureTracker::new(cfg());
        t.sample(0, sample_at(0));
        t.sample(1, sample_at(20_000));
        assert_eq!(t.state(), PressureState::Saturated);
        // Half of 10_000 is 5_000: 4_999 is below it, but it is still above half
        // of the elevated threshold, so the state is Elevated rather than
        // Nominal.
        assert_eq!(t.sample(2, sample_at(4_999)), PressureState::Elevated);
        assert_eq!(t.sample(3, sample_at(100)), PressureState::Nominal);
        assert_eq!(
            t.saturation_events(),
            1,
            "the return trip is not a new event"
        );
    }

    #[test]
    fn time_is_charged_to_the_state_that_was_current() {
        let mut t = PressureTracker::new(cfg());
        t.sample(0, sample_at(0)); // nominal from 0
        t.sample(1_000, sample_at(2_000)); // elevated from 1000
        t.sample(5_000, sample_at(20_000)); // saturated from 5000
        t.close(10_000);
        let r = t.report();
        assert_eq!(r.nominal_ns, 1_000);
        assert_eq!(r.elevated_ns, 4_000);
        assert_eq!(r.saturated_ns, 5_000);
        assert_eq!(r.total_ns(), 10_000);
        assert_eq!(r.saturated_pct_x100(), 5_000);
    }

    #[test]
    fn closing_a_run_that_ends_saturated_counts_that_time() {
        let mut t = PressureTracker::new(cfg());
        t.sample(0, sample_at(0));
        t.sample(1_000, sample_at(50_000));
        // No further samples: without `close`, the saturated time would be zero.
        t.close(9_000);
        let r = t.report();
        assert_eq!(r.saturated_ns, 8_000);
        assert_eq!(r.state, PressureState::Saturated);
    }

    #[test]
    fn the_peak_lag_is_kept_with_the_lag_distribution() {
        let mut t = PressureTracker::new(cfg());
        for lag in [10u64, 20, 5_000, 30] {
            t.sample(0, sample_at(lag));
        }
        assert_eq!(t.peak_lag_ns(), 5_000);
        assert_eq!(t.report().peak_lag_ns, 5_000);
        assert_eq!(t.lag().count(), 4);
        assert_eq!(t.lag().max(), 5_000);
    }

    #[test]
    fn peak_queue_is_recorded_from_the_sample() {
        let mut t = PressureTracker::new(cfg());
        t.sample(
            0,
            Sample {
                lag_ns: 0,
                queue_len: 10,
                queue_capacity: 100,
            },
        );
        t.sample(
            1,
            Sample {
                lag_ns: 0,
                queue_len: 99,
                queue_capacity: 100,
            },
        );
        t.sample(
            2,
            Sample {
                lag_ns: 0,
                queue_len: 3,
                queue_capacity: 100,
            },
        );
        assert_eq!(t.report().peak_queue, 99);
    }

    #[test]
    fn a_disabled_tracker_reports_nominal_and_nothing_else() {
        let mut t = PressureTracker::new(PressureConfig {
            enabled: false,
            ..cfg()
        });
        assert_eq!(t.sample(0, sample_at(1_000_000)), PressureState::Nominal);
        assert_eq!(t.report().samples, 0);
        assert_eq!(t.report().peak_lag_ns, 0);
    }

    #[test]
    fn the_report_line_carries_the_state_and_the_tail() {
        let mut t = PressureTracker::new(cfg());
        t.sample(0, sample_at(0));
        t.sample(1_000, sample_at(20_000));
        t.close(2_000);
        let line = t.report().to_string();
        assert!(line.contains("pressure=saturated"), "{line}");
        assert!(line.contains("saturation_events=1"), "{line}");
        assert!(line.contains("peak_lag=0.020ms"), "{line}");
    }
}
