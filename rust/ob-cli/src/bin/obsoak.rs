//! `obsoak` — the multi-symbol cache under load, with the numbers that matter.
//!
//! This is the demonstration the cache exists for: hundreds to thousands of
//! tickers, one publishing thread, several subscribers, and a report that says
//! what the pipeline cost in latency, in slippage and in backpressure.
//!
//! ```text
//!   obsoak [--symbols N] [--ticks T] [--subscribers K] [--depth D]
//!          [--queue Q] [--shards S] [--size S] [--seed X] [--json]
//! ```
//!
//! # The shape of the run
//!
//! The main thread is the **feed and the strategy**. It walks a deterministic
//! price process for every symbol, publishes the same shape of record the real
//! adapter sends — a reset for each side, then that side's levels — and, after
//! each symbol's picture, takes liquidity from that symbol's book.
//!
//! That the feed and the strategy are the same thread is not a shortcut. It is
//! the reason the cache exists: a strategy co-located with the feed reads the
//! book without a round trip, and the price of that is that the strategy's work
//! is on the feed's thread. `docs/design/ticker-cache.md` §8 measures what that
//! costs.
//!
//! The other threads are **subscribers**: pure consumers that drain their queue
//! and measure how long each event waited. They model downstream consumers and
//! they are where backpressure appears.
//!
//! # What the slippage numbers mean here
//!
//! The strategy decides on the **previous** picture's midpoint and acts on this
//! one, which is what a one-picture decision delay looks like. So:
//!
//! - the **latency component** is one picture of price drift,
//! - the **depth component** is the cost of walking the levels for the order
//!   size,
//! - and the two add up to the implementation shortfall against the price the
//!   decision was made at.
//!
//! A synthetic run cannot tell you what a real symbol's drift will be. It can
//! tell you what this pipeline charges for a given drift, which is the number
//! that decides whether the pipeline is fast enough.
//!
//! # Determinism
//!
//! The price process is a seeded xorshift, so the same arguments produce the
//! same records and the same report. A benchmark whose input changes between runs
//! cannot be compared with itself.

use ob::cache::{self, CacheConfig, Event, Publisher};
use ob::clock;
use ob::fixed::{Price, Qty, E8};
use ob::flow::DropPolicy;
use ob::msg::{Msg, Side};
use ob::slippage::{Execution, Taker, DEFAULT_HORIZONS_NS, HORIZONS};
use ob_cli::{ms, rate, say, CliError, CliResult};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A deterministic random source. xorshift64*, seeded per symbol.
#[derive(Debug, Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // A zero state makes xorshift produce zeros forever.
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `[0, n)`.
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }

    /// A value in `[-n, n]`.
    fn signed(&mut self, n: i64) -> i64 {
        if n <= 0 {
            return 0;
        }
        self.below((2 * n + 1) as u64) as i64 - n
    }
}

/// Per-symbol state the generator carries.
#[derive(Debug, Clone, Copy)]
struct Ticker {
    /// The wire name, so publishing never has to reconstruct it.
    symbol: [u8; 8],
    /// Midpoint at the previous picture, which is what the strategy decided on.
    previous_mid_e8: i64,
    /// Midpoint now.
    mid_e8: i64,
    /// The price tick, in e8 units.
    tick_e8: i64,
    rng: Rng,
}

/// Everything the run needs.
#[derive(Debug, Clone)]
struct Settings {
    symbols: usize,
    ticks: usize,
    subscribers: usize,
    depth: usize,
    queue: usize,
    shards: usize,
    order_size_e8: i64,
    seed: u64,
    /// Half the spread, in hundredths of a basis point of the midpoint.
    half_spread_bps_x100: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            symbols: 1000,
            ticks: 200,
            subscribers: 2,
            depth: 5,
            queue: 8192,
            shards: 8,
            order_size_e8: E8, // one unit
            seed: 0x5EED_1234,
            half_spread_bps_x100: 50, // half a basis point each way
        }
    }
}

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("obsoak: {e}"));
        std::process::exit(e.code);
    }
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let d = Settings::default();
    let s = Settings {
        symbols: args.number("--symbols", d.symbols)?,
        ticks: args.number("--ticks", d.ticks)?,
        subscribers: args.number("--subscribers", d.subscribers)?,
        depth: args.number("--depth", d.depth)?,
        queue: args.number("--queue", d.queue)?,
        shards: args.number("--shards", d.shards)?,
        order_size_e8: args.number("--size", d.order_size_e8)?,
        seed: args.number("--seed", d.seed)?,
        half_spread_bps_x100: args.number("--half-spread-bps-x100", d.half_spread_bps_x100)?,
    };
    if s.symbols == 0 || s.ticks == 0 || s.depth == 0 {
        return Err(CliError::usage(
            "--symbols, --ticks and --depth must be positive",
        ));
    }

    let cfg = CacheConfig {
        symbols: s.symbols * 2,
        shards: s.shards,
        depth: s.depth + 4,
        subscribers: s.subscribers,
        queue: s.queue,
        policy: DropPolicy::DropOldest,
        latency: true,
        slippage: true,
        pressure: ob::pressure::PressureConfig::default(),
        expected_interval_ns: 100_000,
        horizons: DEFAULT_HORIZONS_NS,
    };

    let mut cache = cache::build(cfg);
    let tickers = seed_tickers(&s, &mut cache.publisher)?;
    say(&format!(
        "obsoak: symbols={} ticks={} depth={} subscribers={} queue={} shards={} size={} seed={:#x}",
        s.symbols, s.ticks, s.depth, s.subscribers, s.queue, s.shards, s.order_size_e8, s.seed
    ));

    let running = Arc::new(AtomicBool::new(true));
    let mut handles = Vec::new();
    for mut subscriber in cache.subscribers.drain(..) {
        let stop = Arc::clone(&running);
        let handle = std::thread::Builder::new()
            .name(format!("subscriber-{}", subscriber.id()))
            .spawn(move || {
                let mut out = vec![Event::placeholder(); 512];
                while stop.load(Ordering::Relaxed) {
                    if subscriber.drain(&mut out) == 0 {
                        std::thread::sleep(Duration::from_micros(100));
                    }
                }
                subscriber.close(clock::now_ns());
                subscriber
            })
            .map_err(|e| CliError::failed(format!("spawn subscriber: {e}")))?;
        handles.push(handle);
    }

    let started = clock::now_ns();
    let published = drive(&s, &mut cache.publisher, &tickers);
    let elapsed = clock::now_ns().saturating_sub(started);
    running.store(false, Ordering::Relaxed);

    let mut subscribers = Vec::new();
    for handle in handles {
        if let Ok(sub) = handle.join() {
            subscribers.push(sub);
        }
    }

    let report = Report {
        settings: &s,
        publisher: &cache.publisher,
        subscribers: &subscribers,
        published,
        elapsed_ns: elapsed,
    };
    if args.has("--json") {
        say(&report.to_json());
    } else {
        say(&report.to_text());
    }
    Ok(())
}

/// Build one ticker per symbol and intern their names.
fn seed_tickers(s: &Settings, publisher: &mut Publisher) -> CliResult<Vec<Ticker>> {
    let mut tickers = Vec::with_capacity(s.symbols);
    let mut rng = Rng::new(s.seed);
    for i in 0..s.symbols {
        // Seven characters, because the wire field is eight bytes with a
        // terminator. A longer name would be truncated and two symbols would
        // collide.
        let name = format!("S{:06}", i);
        let mut wire = [0u8; 8];
        wire[..name.len()].copy_from_slice(name.as_bytes());
        publisher
            .intern(&wire)
            .map_err(|e| CliError::failed(format!("intern {name}: {e}")))?;
        // A mid near 100, a one-cent tick, and a starting walk.
        let mid = 100 * E8 + rng.signed(100) * 1_000_000;
        tickers.push(Ticker {
            symbol: wire,
            previous_mid_e8: mid,
            mid_e8: mid,
            tick_e8: 1_000_000,
            rng: Rng::new(s.seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        });
    }
    Ok(tickers)
}

/// Publish every tick and take liquidity after each picture.
///
/// Returns how many records were published.
fn drive(s: &Settings, publisher: &mut Publisher, tickers: &[Ticker]) -> u64 {
    let mut tickers = tickers.to_vec();
    let mut published = 0u64;
    let mut batch: Vec<Msg> = Vec::with_capacity(s.depth * 2 + 2);

    for _tick in 0..s.ticks {
        for ticker in tickers.iter_mut() {
            advance(ticker);

            batch.clear();
            for side in [Side::Bid, Side::Ask] {
                let mut reset = Msg::reset(side, "");
                reset.set_symbol_bytes(&ticker.symbol);
                batch.push(reset);
                for level in 0..s.depth {
                    let (px, qty) = level_of(ticker, s, side, level);
                    let mut m = Msg::depth(side, Price::from_e8(px), Qty::from_e8(qty), "");
                    m.set_symbol_bytes(&ticker.symbol);
                    batch.push(m);
                }
            }

            for m in batch.iter_mut() {
                let t_recv = clock::now_ns();
                publisher.publish(m, t_recv);
                published += 1;
            }

            // The strategy: decide on the previous mid, act on this book.
            take(publisher, ticker, s);
        }
        // Settle markouts once per tick rather than once per record: a markout is
        // about a time that has passed, so checking it per record would be work
        // for nothing.
        publisher.resolve_markouts(clock::now_ns());
    }
    publisher.resolve_markouts(clock::now_ns() + 2 * DEFAULT_HORIZONS_NS[2]);
    published
}

/// A symbol's name, for the report.
fn ticker_name(publisher: &Publisher, ticker: &Ticker) -> String {
    match publisher.symbols().lookup(&ticker.symbol) {
        Some(id) => publisher.symbols().symbol_str(id).to_string(),
        None => String::from_utf8_lossy(&ticker.symbol)
            .trim_end_matches('\0')
            .to_string(),
    }
}

/// Move one ticker's midpoint by a random number of ticks.
fn advance(t: &mut Ticker) {
    t.previous_mid_e8 = t.mid_e8;
    // A random walk of up to three ticks per picture, which is a few basis
    // points on a mid of 100.
    let steps = t.rng.signed(3);
    t.mid_e8 = (t.mid_e8 + steps * t.tick_e8).max(10 * E8);
}

/// One level's price and size, on the given side.
fn level_of(t: &mut Ticker, s: &Settings, side: Side, level: usize) -> (i64, i64) {
    // The touch is the mid plus or minus half the spread; each further level is
    // one tick worse. Sizes vary a little so the depth walk has something to
    // walk.
    let half = ob::slippage::price_from_bps(t.mid_e8, s.half_spread_bps_x100);
    let offset = (level as i64) * t.tick_e8;
    let px = match side {
        Side::Bid => half - offset,
        _ => half + offset,
    };
    let size = E8 / 2 + (t.rng.below(4) as i64) * (E8 / 4);
    (px, size)
}

/// Take liquidity from a symbol's book and record the cost.
fn take(publisher: &mut Publisher, ticker: &Ticker, s: &Settings) {
    let Some(symbol) = publisher.symbols().lookup(&ticker.symbol) else {
        return;
    };
    // Buy, so the relevant side is the asks.
    let Some(fill) = publisher.fill(symbol, Side::Ask, s.order_size_e8) else {
        return;
    };
    let Some(top) = publisher.top(symbol) else {
        return;
    };
    let Some(action_mid) = top.mid_e8() else {
        return;
    };
    let decision_mid = ticker.previous_mid_e8;
    if decision_mid == 0 {
        return;
    }
    publisher.record_execution(
        symbol,
        clock::now_ns(),
        Execution {
            taker: Taker::Buy,
            decision_mid_e8: decision_mid,
            action_mid_e8: action_mid,
            action_px_e8: fill.average_px.as_e8(),
            filled: Qty::from_e8(fill.filled),
            unfilled: Qty::from_e8(fill.unfilled()),
        },
    );
}

/// The final report.
#[derive(Debug)]
struct Report<'a> {
    settings: &'a Settings,
    publisher: &'a Publisher,
    subscribers: &'a [cache::Subscriber],
    published: u64,
    elapsed_ns: u64,
}

impl Report<'_> {
    fn to_text(&self) -> String {
        let s = self.settings;
        let stats = self.publisher.stats();
        let latency = self.publisher.latency();
        let mut out = String::new();
        out.push_str("\n=== cache soak ===\n");
        out.push_str(&format!(
            "symbols={} ticks={} records={} elapsed={}ms rate={}/s\n",
            self.publisher.symbol_count(),
            s.ticks,
            self.published,
            ms(self.elapsed_ns),
            rate(self.published, self.elapsed_ns),
        ));
        out.push_str(&format!(
            "applied={} ignored={} overflow={} crossed={} unknown_symbol={} wrong_side={}\n",
            stats.applied,
            stats.ignored,
            stats.overflow,
            stats.crossed,
            stats.unknown_symbol,
            stats.wrong_side,
        ));
        out.push_str(&format!(
            "fan-out: evicted={} dropped={} refused={}\n",
            stats.evicted, stats.dropped, stats.refused,
        ));
        if stats.crossed > 0 {
            // A reader seeing a seven-figure crossed count must not conclude the
            // cache is broken. This generator publishes each side of a picture in
            // turn, so between a side's reset and its prices the book holds the
            // new bids against the previous asks, which is crossed. A real
            // depth5 picture arrives the same way; the count is a property of
            // publishing a picture, not of the cache.
            out.push_str(&format!(
                "note: {} of {} applied records left the book crossed, because this\n\
                 generator publishes each side of a picture in turn. That is a property\n\
                 of the generator, not a fault: see docs/design/ticker-cache.md section 4.\n",
                stats.crossed, stats.applied,
            ));
        }

        out.push_str("\n-- latency (ingest, stages 1 and 2) --\n");
        out.push_str(&format!(
            "  stage1 send->recv : {}\n",
            latency.send_to_recv().summary()
        ));
        out.push_str(&format!(
            "  stage2 recv->pub  : {}\n",
            latency.recv_to_publish().summary()
        ));
        out.push_str(&format!(
            "  ingress 1+2       : {}\n",
            latency.total().summary()
        ));
        out.push_str(&format!(
            "  stamped={} unstamped={} implausible={}\n",
            latency.stamped(),
            latency.unstamped(),
            latency.implausible(),
        ));

        out.push_str("\n-- latency (per subscriber, stage 3 and end to end) --\n");
        for sub in self.subscribers {
            let r = cache::subscriber_report(sub);
            out.push_str(&format!("  {r}\n"));
            out.push_str(&format!(
                "    end-to-end: {}\n",
                sub.latency().total().summary()
            ));
            out.push_str(&format!("    {}\n", sub.pressure_report()));
        }

        out.push_str("\n-- slippage (simulated taker, buys) --\n");
        let mut executions = 0u64;
        let mut latency_sum = 0i128;
        let mut depth_sum = 0i128;
        let mut shortfall_sum = 0i128;
        let mut spread_sum = 0i128;
        let mut filled = 0i128;
        let mut unfilled = 0i128;
        let mut markouts = [0i128; HORIZONS];
        let mut markout_counts = [0u64; HORIZONS];
        let mut worst: Vec<(String, i64, u64)> = Vec::new();
        for id in self.publisher.symbols().ids() {
            let Some(ledger) = self.publisher.symbol_ledger(id) else {
                continue;
            };
            if ledger.executions == 0 {
                continue;
            }
            executions += ledger.executions;
            latency_sum += ledger.latency_sum;
            depth_sum += ledger.depth_sum;
            shortfall_sum += ledger.shortfall_sum;
            spread_sum += ledger.spread_sum;
            filled += ledger.filled;
            unfilled += ledger.unfilled;
            for h in 0..HORIZONS {
                markouts[h] += ledger.markout[h].sum_bps_x100;
                markout_counts[h] += ledger.markout[h].count;
            }
            worst.push((
                self.publisher.symbols().symbol_str(id).to_string(),
                ledger.worst_shortfall(),
                ledger.executions,
            ));
        }
        let mean = |sum: i128| -> i64 {
            if executions == 0 {
                0
            } else {
                (sum / i128::from(executions)) as i64
            }
        };
        out.push_str(&format!(
            "  executions={} mean_size={}\n",
            executions,
            filled / executions.max(1) as i128
        ));
        out.push_str(&format!(
            "  latency component : {} bps\n",
            bps(mean(latency_sum))
        ));
        out.push_str(&format!(
            "  depth component   : {} bps\n",
            bps(mean(depth_sum))
        ));
        out.push_str(&format!(
            "  shortfall         : {} bps\n",
            bps(mean(shortfall_sum))
        ));
        out.push_str(&format!(
            "  effective spread  : {} bps\n",
            bps(mean(spread_sum))
        ));
        out.push_str(&format!(
            "  unfilled          : {} of {} units\n",
            unfilled / i128::from(E8),
            (filled + unfilled) / i128::from(E8),
        ));
        for h in 0..HORIZONS {
            let label = match h {
                0 => "10ms ",
                1 => "100ms",
                _ => "1s   ",
            };
            let mean_bps = if markout_counts[h] == 0 {
                0
            } else {
                (markouts[h] / i128::from(markout_counts[h])) as i64
            };
            out.push_str(&format!(
                "  markout {label}: {} bps over {} observations\n",
                bps(mean_bps),
                markout_counts[h],
            ));
        }
        worst.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        worst.truncate(5);
        if !worst.is_empty() {
            out.push_str("  worst shortfall:\n");
            for (name, bps_x100, n) in &worst {
                out.push_str(&format!(
                    "    {name}: {} bps over {n} executions\n",
                    bps(*bps_x100)
                ));
            }
        }

        let _ = ticker_name;
        out.push_str("\n-- footprint --\n");
        out.push_str(&format!(
            "  books: {} symbols x {} levels x 2 sides x 16 bytes = {} KiB\n",
            self.publisher.symbol_count(),
            s.depth + 4,
            self.publisher.symbol_count() * (s.depth + 4) * 2 * 16 / 1024,
        ));
        out.push_str(&format!(
            "  queues: {} subscribers x {} records x {} bytes = {} KiB\n",
            s.subscribers,
            s.queue.next_power_of_two(),
            std::mem::size_of::<Event>(),
            s.subscribers * s.queue.next_power_of_two() * std::mem::size_of::<Event>() / 1024,
        ));
        out
    }

    fn to_json(&self) -> String {
        let stats = self.publisher.stats();
        let latency = self.publisher.latency();
        let mut sub_lines = Vec::new();
        for sub in self.subscribers {
            let r = cache::subscriber_report(sub);
            let p = sub.pressure_report();
            sub_lines.push(format!(
                "{{\"id\":{},\"delivered\":{},\"evicted\":{},\"dropped\":{},\"high_water\":{},\"state\":\"{}\",\"saturated_pct_x100\":{},\"stage3_p50_ns\":{},\"stage3_p99_ns\":{},\"stage3_max_ns\":{},\"end_to_end_p99_ns\":{}}}",
                r.id,
                r.delivered,
                r.evicted,
                r.dropped,
                r.high_water,
                r.pressure.label(),
                p.saturated_pct_x100(),
                r.stage3.p50,
                r.stage3.p99,
                r.stage3.max,
                sub.latency().total().quantile(0.99),
            ));
        }
        format!(
            "{{\"symbols\":{},\"ticks\":{},\"records\":{},\"elapsed_ns\":{},\"rate_per_s\":{:.1},\
             \"applied\":{},\"ignored\":{},\"overflow\":{},\"crossed\":{},\"unknown_symbol\":{},\
             \"evicted\":{},\"dropped\":{},\"refused\":{},\
             \"stage1_p50_ns\":{},\"stage1_p99_ns\":{},\"stage2_p50_ns\":{},\"stage2_p99_ns\":{},\
             \"ingress_p99_ns\":{},\"stage2_max_ns\":{},\
             \"stamped\":{},\"unstamped\":{},\"implausible\":{},\
             \"subscribers\":[{}]}}",
            self.publisher.symbol_count(),
            self.settings.ticks,
            self.published,
            self.elapsed_ns,
            self.published as f64 * 1e9 / self.elapsed_ns.max(1) as f64,
            stats.applied,
            stats.ignored,
            stats.overflow,
            stats.crossed,
            stats.unknown_symbol,
            stats.evicted,
            stats.dropped,
            stats.refused,
            latency.send_to_recv().quantile(0.50),
            latency.send_to_recv().quantile(0.99),
            latency.recv_to_publish().quantile(0.50),
            latency.recv_to_publish().quantile(0.99),
            latency.total().quantile(0.99),
            latency.recv_to_publish().max(),
            latency.stamped(),
            latency.unstamped(),
            latency.implausible(),
            sub_lines.join(","),
        )
    }
}

/// Format hundredths of a basis point as basis points with two decimals.
fn bps(bps_x100: i64) -> String {
    format!("{:.2}", bps_x100 as f64 / 100.0)
}
