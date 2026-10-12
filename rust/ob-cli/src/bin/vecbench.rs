//! `vecbench` — the kernels that matter, timed, with the shape they replaced.
//!
//! ```text
//!   vecbench [--iters N] [--records R] [--levels L] [--json] [--kernels a,b,c]
//! ```
//!
//! # Why a hand-rolled harness
//!
//! A benchmarking crate would be a dependency, a second result format, and a
//! statistical model this lab does not need. What it does need is three numbers
//! per kernel: how long one operation takes, how long the worst one took, and how
//! the current code compares with the shape it replaced. The harness below
//! produces exactly those, from the same monotonic clock the rest of the lab
//! uses, and prints them in a form a design document can quote.
//!
//! # The baseline kernels
//!
//! Each `baseline_*` function is a copy of a kernel **as it was written before**
//! `docs/design/vec-optimization.md` reshaped it. They are kept here, next to the
//! measurement, so the claim "this is faster" is a number in a table rather than
//! a memory of an earlier run. They are not called by anything in production.
//!
//! # What is measured
//!
//! | Kernel | What it does | Where it runs |
//! |--------|--------------|---------------|
//! | `decode` | 72-byte records to `Msg` | every byte read from a socket |
//! | `top` | copy the best `n` levels of a side | every terminal frame, every snapshot |
//! | `cumulative` | prefix sum of sizes | every depth-weighted price |
//! | `walk` | take `size` from the levels | every simulated execution |
//! | `fanout` | push one event to one subscriber queue | every published record, per subscriber |
//! | `mids` | every symbol's midpoint in one pass | every market snapshot |
//! | `torn` | how many whole valid records a buffer holds | every socket read |
//!
//! # Reading the output
//!
//! `ns/op` is the mean over the timed iterations and `p99` is the 99th
//! percentile of the per-iteration times, both from [`ob::latency`]. A mean alone
//! hides the stalls that matter, which is the same argument the latency tracker
//! makes. `vs base` is the ratio of the baseline's mean to this one, so a value
//! above 1 means the current shape is faster.

use ob::book::{Level, Levels};
use ob::cache::{self, CacheConfig};
use ob::clock;
use ob::codec;
use ob::fixed::{Price, Qty, E8};
use ob::flow::{self, DropPolicy};
use ob::latency::Histogram;
use ob::msg::{Msg, Side, MSG_SIZE};
use ob_cli::{say, CliError, CliResult};
use std::time::Duration;

/// Calls per timed sample for kernels that finish in under a hundred
/// nanoseconds, where one call is below the clock's resolution.
const FAST_BATCH: u64 = 64;

/// One kernel's result.
#[derive(Debug, Clone, Copy)]
struct Result_ {
    name: &'static str,
    mean_ns: f64,
    p50_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    baseline_mean_ns: f64,
    checksum: u64,
}

impl Result_ {
    /// Baseline mean divided by this mean. Above one means this is faster.
    fn speedup(&self) -> f64 {
        if self.mean_ns <= 0.0 {
            0.0
        } else {
            self.baseline_mean_ns / self.mean_ns
        }
    }
}

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("vecbench: {e}"));
        std::process::exit(e.code);
    }
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let iters: u64 = args.number("--iters", 20_000)?;
    let records: usize = args.number("--records", 4096)?;
    let levels: usize = args.number("--levels", 32)?;
    let json = args.has("--json");
    let only: Vec<String> = args
        .value("--kernels")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    if iters == 0 || records == 0 || levels == 0 {
        return Err(CliError::usage(
            "--iters, --records and --levels must be positive",
        ));
    }

    // Prepare the data once, outside every timed loop.
    let wire = make_wire(records);
    let mut prices: Vec<i64> = Vec::with_capacity(levels);
    let mut sizes: Vec<i64> = Vec::with_capacity(levels);
    for i in 0..levels {
        // A descending bid side, one tick apart, with varied sizes.
        prices.push((1_000_000 - i as i64) * 1_000_000);
        sizes.push(((i % 7) as i64 + 1) * (E8 / 4));
    }
    let mut side = Levels::new(levels, true);
    for i in 0..levels {
        side.upsert(prices[i], sizes[i]);
    }

    let mut results = Vec::new();
    if wanted(&only, "decode") {
        results.push(bench_decode(&wire, iters));
    }
    if wanted(&only, "top") {
        results.push(bench_top(&side, &prices, &sizes, levels, iters));
    }
    if wanted(&only, "cumulative") {
        results.push(bench_cumulative(&side, &sizes, levels, iters));
    }
    if wanted(&only, "walk") {
        results.push(bench_walk(&side, iters));
    }
    if wanted(&only, "fanout") {
        results.push(bench_fanout(iters));
    }
    if wanted(&only, "mids") {
        results.push(bench_mids(iters));
    }
    if wanted(&only, "torn") {
        results.push(bench_torn(&wire, iters));
    }

    if json {
        say(&to_json(&results));
    } else {
        print_table(&results);
    }
    Ok(())
}

/// Whether a kernel was asked for. An empty list means all of them.
fn wanted(only: &[String], name: &str) -> bool {
    only.is_empty() || only.iter().any(|k| k == name)
}

/// Build a buffer of `records` valid 72-byte records.
fn make_wire(records: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(records * MSG_SIZE);
    for i in 0..records {
        let mut m = Msg::depth(
            if i % 2 == 0 { Side::Bid } else { Side::Ask },
            Price::from_e8((65_000 - (i % 20) as i64) * E8),
            Qty::from_e8((i as i64 % 9 + 1) * (E8 / 4)),
            "BTCUSDT",
        );
        m.seq = i as u64;
        m.ts_ns = i as u64;
        out.extend_from_slice(&m.encode());
    }
    out
}

/// Fold an output buffer into one number, so the writes cannot be elided.
///
/// Reading through the whole buffer is what makes the value depend on every
/// write. The fold is cheap next to the kernel and it runs inside the timed
/// region, which is where it has to be for the guarantee to hold.
#[inline]
fn consume(values: &[i64]) -> u64 {
    values.iter().fold(0u64, |acc, &v| acc ^ (v as u64))
}

/// Fold an output slice of levels into one number.
#[inline]
fn consume_levels(values: &[Level]) -> u64 {
    values.iter().fold(0u64, |acc, l| {
        acc ^ (l.px.as_e8() as u64) ^ ((l.qty.as_e8() as u64) << 1)
    })
}

/// Time `iters` samples of `batch` calls each, reporting nanoseconds per call.
///
/// # Why this exists
///
/// `CLOCK_MONOTONIC_RAW` on this machine resolves to tens of nanoseconds, so a
/// kernel that takes 10 ns cannot be timed one call at a time: the first version
/// of this harness reported a 16 ns kernel and a 13 ns kernel as the same
/// number, and reported a 10 ns kernel as slower than a 12 ns one. Timing a
/// batch and dividing by the batch size puts the measurement above the clock's
/// resolution, which is the only way a difference between two fast kernels is
/// real rather than noise.
///
/// # Why the caller must also black-box the input
///
/// Batching introduces a failure the single-call form cannot have. A kernel
/// whose input does not change between the calls in a batch is a loop-invariant
/// computation, and an optimizer may hoist it out of the batch and reuse the
/// result — the call is still made, because the result is passed through
/// `black_box`, but it is made *once*. A profile-guided build did exactly that
/// and reported a 1000-symbol snapshot at 16 nanoseconds, which is 0.016
/// nanoseconds per symbol and impossible.
///
/// So every batched caller passes its input through
/// [`std::hint::black_box`] inside the closure, which tells the optimizer the
/// input is opaque and may differ on each call. Ledger: OPT-017.
///
/// A batch of one is exactly [`time`](time) and is what the slow kernels use.
fn time_batched<T>(iters: u64, batch: u64, mut f: impl FnMut() -> T) -> (Histogram, T) {
    let batch = batch.max(1);
    let mut last = f();
    for _ in 0..(iters / 10).max(1) {
        last = f();
    }
    let mut hist = Histogram::new();
    for _ in 0..iters {
        let start = clock::now_ns();
        for _ in 0..batch {
            last = std::hint::black_box(f());
        }
        hist.record(clock::now_ns().saturating_sub(start) / batch);
    }
    (hist, last)
}

/// Mark an input opaque, so a batched call cannot be hoisted out of its batch.
///
/// See [`time_batched`]. Named rather than written inline because the reason is
/// not obvious at the call site and this is the second thing a reader needs to
/// know about the batched timer.
#[inline]
fn opaque<T>(value: T) -> T {
    std::hint::black_box(value)
}

/// Time `iters` calls of `f`, returning the per-iteration distribution.
fn time<T>(iters: u64, mut f: impl FnMut() -> T) -> (Histogram, T) {
    // Warm up so the first iteration's page faults are not in the sample.
    let mut last = f();
    for _ in 0..(iters / 10).max(1) {
        last = f();
    }
    let mut hist = Histogram::new();
    for _ in 0..iters {
        let start = clock::now_ns();
        // `black_box` on the result is the guarantee: without it a kernel that
        // writes into a buffer nobody reads is dead code, and an optimizer is
        // entitled to remove it. The first version of this harness reported a
        // 512-element prefix sum as 33 nanoseconds for exactly that reason.
        last = std::hint::black_box(f());
        hist.record(clock::now_ns().saturating_sub(start));
    }
    (hist, last)
}

fn summarise(
    name: &'static str,
    hist: &Histogram,
    iters: u64,
    baseline: Option<(Histogram, u64)>,
    checksum: u64,
) -> Result_ {
    let mean = hist.sum() as f64 / iters.max(1) as f64;
    let baseline_mean = match baseline {
        Some((h, n)) => h.sum() as f64 / n.max(1) as f64,
        None => mean,
    };
    Result_ {
        name,
        mean_ns: mean,
        p50_ns: hist.quantile(0.50),
        p99_ns: hist.quantile(0.99),
        max_ns: hist.max(),
        baseline_mean_ns: baseline_mean,
        checksum,
    }
}

fn bench_decode(wire: &[u8], iters: u64) -> Result_ {
    let mut out = vec![Msg::default(); wire.len() / MSG_SIZE];
    let (hist, n) = time(iters, || {
        let stats = codec::decode_batch(wire, &mut out);
        (stats.decoded as u64) ^ std::hint::black_box(out[stats.decoded.saturating_sub(1)].seq)
    });
    let (base, bn) = time(iters, || {
        let written = baseline_decode(wire, &mut out);
        (written as u64) ^ std::hint::black_box(out[written.saturating_sub(1)].seq)
    });
    summarise("decode", &hist, iters, Some((base, iters)), n ^ bn)
}

fn bench_top(side: &Levels, prices: &[i64], sizes: &[i64], levels: usize, iters: u64) -> Result_ {
    let mut out = vec![Level::EMPTY; levels];
    let (hist, n) = time_batched(iters, FAST_BATCH, || {
        let written = opaque(side).top(levels, opaque(&mut out));
        (written as u64) ^ consume_levels(&out[..written])
    });
    let (base, bn) = time_batched(iters, FAST_BATCH, || {
        let written = baseline_top(prices, sizes, levels, &mut out);
        (written as u64) ^ consume_levels(&out[..written])
    });
    summarise("top", &hist, iters, Some((base, iters)), n ^ bn)
}

fn bench_cumulative(side: &Levels, sizes: &[i64], levels: usize, iters: u64) -> Result_ {
    let mut out = vec![0i64; levels];
    let (hist, n) = time_batched(iters, FAST_BATCH, || {
        let written = opaque(side).cumulative(levels, opaque(&mut out));
        (written as u64) ^ consume(&out[..written])
    });
    let (base, bn) = time_batched(iters, FAST_BATCH, || {
        let written = experiment_cumulative_blocked(sizes, levels, &mut out);
        (written as u64) ^ consume(&out[..written])
    });
    summarise("cumulative", &hist, iters, Some((base, iters)), n ^ bn)
}

fn bench_walk(side: &Levels, iters: u64) -> Result_ {
    let request = E8;
    let (hist, n) = time_batched(iters, FAST_BATCH, || {
        opaque(side)
            .fill(opaque(request))
            .map(|f| (f.filled as u64) ^ (f.notional_e8 as u64) ^ (f.levels_used as u64))
            .unwrap_or(0)
    });
    summarise("walk", &hist, iters, None, n)
}

/// Push one event through a subscriber queue.
fn bench_fanout(iters: u64) -> Result_ {
    // The queue is deep enough that a run never fills it: what is being measured
    // is the fast path of a push, not the eviction path.
    let (mut producer, mut consumer) = flow::ring::<u64>(1 << 16, DropPolicy::DropOldest);
    let mut accepted = 0u64;
    let (hist, _) = time_batched(iters, FAST_BATCH, || {
        if producer.push(7).accepted() {
            accepted += 1;
        }
    });
    // Drain after the measurement so nothing is left queued.
    let mut sink = [0u64; 1024];
    while consumer.drain(&mut sink) > 0 {}
    summarise("fanout", &hist, iters, None, accepted)
}

/// Every symbol's midpoint in one pass.
fn bench_mids(iters: u64) -> Result_ {
    // A thousand symbols with both sides priced, which is the shape a market
    // snapshot has.
    let mut cache = cache::build(CacheConfig {
        symbols: 2048,
        shards: 8,
        depth: 4,
        subscribers: 0,
        queue: 8,
        ..CacheConfig::default()
    });
    for i in 0..1000 {
        let text = format!("M{i:06}");
        let mut name = [0u8; 8];
        name[..text.len()].copy_from_slice(text.as_bytes());
        cache.publisher.intern(&name).expect("intern");
        for (side, px) in [(Side::Bid, 100 * E8), (Side::Ask, 100 * E8 + 1_000_000)] {
            let mut m = Msg::depth(side, Price::from_e8(px), Qty::from_e8(E8), "");
            m.set_symbol_bytes(&name);
            // A real timestamp, not zero: the publisher measures stage 2 from
            // this and the coordinated-omission correction scales with it. Zero
            // made the correction treat the whole monotonic clock as a stall.
            cache.publisher.publish(&mut m, ob::clock::now_ns());
        }
    }
    let mut out = vec![0i64; 2048];
    let (hist, n) = time(iters, || cache.publisher.mids_into(&mut out));
    let (base, bn) = time(iters, || baseline_mids(&cache.publisher, &mut out));
    summarise(
        "mids",
        &hist,
        iters,
        Some((base, iters)),
        n as u64 + bn as u64,
    )
}

fn bench_torn(wire: &[u8], iters: u64) -> Result_ {
    let (hist, n) = time(iters, || codec::stream_records(wire) as u64);
    let (base, bn) = time(iters, || baseline_validate(wire) as u64);
    summarise("torn", &hist, iters, Some((base, iters)), n ^ bn)
}

// ---------------------------------------------------------------------------
// The baseline kernels: the shape each kernel had before it was reshaped.
// ---------------------------------------------------------------------------

/// Every symbol's midpoint, read through the per-symbol accessor.
///
/// This is the shape `mids_into` replaced: correct, obvious, and opaque to the
/// optimizer because each iteration goes through a chain of `Option`-returning
/// method calls the inliner cannot flatten into one loop.
fn baseline_mids(publisher: &ob::cache::Publisher, out: &mut [i64]) -> usize {
    let mut n = 0;
    for id in publisher.symbols().ids() {
        if n >= out.len() {
            break;
        }
        out[n] = publisher.mid_e8(id).unwrap_or_default();
        n += 1;
    }
    n
}

/// The obvious decoder: one record at a time, through the full validating path,
/// with the bounds check the compiler cannot remove because `out` and the input
/// are unrelated slices.
fn baseline_decode(buf: &[u8], out: &mut [Msg]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + MSG_SIZE <= buf.len() && n < out.len() {
        if let Ok(m) = codec::decode(&buf[i..i + MSG_SIZE]) {
            out[n] = m;
            n += 1;
        }
        i += MSG_SIZE;
    }
    n
}

/// The obvious validator: check each record's header and stop at the first
/// failure, which is a data-dependent branch inside the loop.
fn baseline_validate(buf: &[u8]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i + MSG_SIZE <= buf.len() {
        if !codec::header_ok(&buf[i..i + MSG_SIZE]) {
            break;
        }
        count += 1;
        i += MSG_SIZE;
    }
    count
}

/// The obvious top-of-book copy: two slices, one index, two stores per level.
fn baseline_top(prices: &[i64], sizes: &[i64], n: usize, out: &mut [Level]) -> usize {
    let take = n.min(prices.len()).min(sizes.len()).min(out.len());
    let mut i = 0;
    while i < take {
        out[i] = Level {
            px: Price::from_e8(prices[i]),
            qty: Qty::from_e8(sizes[i]),
        };
        i += 1;
    }
    take
}

/// Elements per block in the rejected blocked prefix sum.
const EXPERIMENT_BLOCK: usize = 8;

/// The most blocks the experiment accumulates totals for.
const EXPERIMENT_MAX_BLOCKS: usize = 128;

/// The blocked prefix sum that was written, measured, and **rejected**.
///
/// Three passes so that two of them are independent: block totals, the prefix
/// inside each block, then a unit-stride add of each block's exclusive offset.
/// The middle pass is the one that cannot vectorize, and the other two do not pay
/// for it.
///
/// Kept here rather than deleted so that the claim in
/// `docs/design/vec-optimization.md` §4.2 — "this is 2.5 to 3.2 times slower than
/// the sequential loop, at every size" — stays reproducible. Run
/// `vecbench --kernels cumulative --levels 512` and read the `vs base` column.
fn experiment_cumulative_blocked(sizes: &[i64], n: usize, out: &mut [i64]) -> usize {
    let take = n.min(sizes.len()).min(out.len());
    if take == 0 {
        return 0;
    }
    let dst = &mut out[..take];
    let blocks = take.div_ceil(EXPERIMENT_BLOCK).min(EXPERIMENT_MAX_BLOCKS);

    let mut totals = [0i64; EXPERIMENT_MAX_BLOCKS];
    for (b, total) in totals.iter_mut().enumerate().take(blocks) {
        let start = b * EXPERIMENT_BLOCK;
        let end = (start + EXPERIMENT_BLOCK).min(take);
        let mut sum = 0i64;
        for &q in &sizes[start..end] {
            sum += q;
        }
        *total = sum;
    }
    for b in 0..blocks {
        let start = b * EXPERIMENT_BLOCK;
        let end = (start + EXPERIMENT_BLOCK).min(take);
        let mut running = 0i64;
        for i in start..end {
            running += sizes[i];
            dst[i] = running;
        }
    }
    if blocks * EXPERIMENT_BLOCK < take {
        let start = blocks * EXPERIMENT_BLOCK;
        let mut running = 0i64;
        for i in start..take {
            running += sizes[i];
            dst[i] = running;
        }
    }
    let mut offset = 0i64;
    for (b, &total) in totals.iter().enumerate().take(blocks) {
        let start = b * EXPERIMENT_BLOCK;
        let end = (start + EXPERIMENT_BLOCK).min(take);
        for value in &mut dst[start..end] {
            *value += offset;
        }
        offset += total;
    }
    if blocks * EXPERIMENT_BLOCK < take {
        for value in &mut dst[blocks * EXPERIMENT_BLOCK..take] {
            *value += offset;
        }
    }
    take
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn print_table(results: &[Result_]) {
    say("");
    say("kernel      ns/op     p50      p99      max    vs base   checksum");
    say("---------------------------------------------------------------");
    for r in results {
        let vs = if r.baseline_mean_ns > 0.0 && (r.speedup() - 1.0).abs() > 0.001 {
            format!("{:>7.2}x", r.speedup())
        } else {
            "      -".to_string()
        };
        say(&format!(
            "{:<10} {:>8.1} {:>8} {:>8} {:>8} {vs}   {:#x}",
            r.name, r.mean_ns, r.p50_ns, r.p99_ns, r.max_ns, r.checksum
        ));
    }
    say("");
    say("vs base is the baseline's mean divided by this mean: above 1.00x is faster.");
    say("A kernel with no baseline has no reshaped form to compare against.");
}

fn to_json(results: &[Result_]) -> String {
    let rows: Vec<String> = results
        .iter()
        .map(|r| {
            format!(
                "{{\"kernel\":\"{}\",\"mean_ns\":{:.1},\"p50_ns\":{},\"p99_ns\":{},\"max_ns\":{},\
                 \"baseline_mean_ns\":{:.1},\"speedup\":{:.3},\"checksum\":{}}}",
                r.name,
                r.mean_ns,
                r.p50_ns,
                r.p99_ns,
                r.max_ns,
                r.baseline_mean_ns,
                r.speedup(),
                r.checksum
            )
        })
        .collect();
    format!("{{\"kernels\":[{}]}}", rows.join(","))
}

/// The delay the soak uses between polls, for a caller that wants the same one.
pub const POLL: Duration = Duration::from_micros(100);
