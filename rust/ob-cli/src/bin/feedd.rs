//! `feedd` — read normalized records from a socket and publish them.
//!
//! # The thread model, and why it differs from the C++ daemon
//!
//! The C++ `feedd` accepts a connection and starts a thread per client, and each
//! of those threads calls `ring.publish` directly. With one adapter that is one
//! writer, which is what the ring's protocol requires. With two adapters it is
//! two writers publishing through one `wseq` counter, which is a race the ring
//! cannot detect: both would read the same sequence number and one record would
//! overwrite the other.
//!
//! This daemon has **one publishing thread**. Connection threads decode and hand
//! records to it over a bounded channel:
//!
//! ```text
//!   accept ──▶ connection thread ──decode──▶ bounded channel ──▶ publisher
//!                                                                    │
//!                                             ring ◀────────────────┤
//!                                             WAL  ◀────────────────┤
//!                                             cache ◀───────────────┘
//! ```
//!
//! That gives the single-writer rule for free, and it puts the durability policy
//! in one place: the ring, the log and the cache are all written by the same
//! thread, in the same order, for every record.
//!
//! # Backpressure, and why this queue blocks
//!
//! The channel between a connection and the publisher is bounded, and a full
//! channel **blocks the reader**. That is the correct behaviour here and the
//! opposite of the cache's fan-out policy. The reason: this is the durable path.
//! A record that is dropped here is a record that is not in the log, so it
//! cannot be recovered. Blocking the reader closes the TCP window, the adapter
//! slows down, and the exchange's own flow control takes over — which is what
//! "backpressure" means on an ingest path. The cache's subscribers are not
//! durable, so there the right answer is to drop the oldest and count it.
//!
//! # Usage
//!
//! ```text
//!   feedd <config.json>
//! ```
//!
//! The ready line `feedd listen <host>:<port>` goes to stderr, because
//! `python/obctl.py` waits for it before starting the adapter.

use ob::cache;
use ob::clock;
use ob::log::{Field, Level, Logger};
use ob::msg::Msg;
use ob::ring::Ring;
use ob::wal::{SyncPolicy, Wal};
use ob_cli::ingest::{accept_loop, Counters};
use ob_cli::{ensure_parent, load_config, note, rate, CliError, CliResult};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::Duration;

/// Records a connection may hand to the publisher before it blocks.
///
/// 64 Ki records is 4.6 MB of `Msg` and about a second of headroom at this lab's
/// rate. Large enough that a scheduling hiccup does not stall a reader, small
/// enough that the queue cannot absorb a sustained overload silently.
const INGEST_QUEUE: usize = 65_536;

/// How often the publisher wakes to run its timer.
const TICK: Duration = Duration::from_millis(50);

/// The run flag. The signal handler is a bare `extern "C"` function and cannot
/// capture anything, so this is the one place it can write.
static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn main() {
    if let Err(e) = run() {
        note(&format!("feedd: {e}"));
        std::process::exit(e.code);
    }
}

/// Set by the signal handler. Only a store, which is all a handler may do.
extern "C" fn on_signal(_sig: i32) {
    RUNNING.store(false, Ordering::SeqCst);
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage("usage: feedd <config.json>"));
    };
    let cfg = load_config(cfg_path)?;

    install_signal_handlers()?;

    // Every path a config names gets its directory, so a first run works.
    for path in [&cfg.bus_path, &cfg.wal_path, &cfg.log_path] {
        ensure_parent(path)?;
    }

    let mut log = Logger::new(Level::Info);
    let ring = Ring::create(std::path::Path::new(&cfg.bus_path), cfg.ring_capacity)?;
    let mut wal = Wal::open(std::path::Path::new(&cfg.wal_path), true, cfg.sync_policy)?;

    let listener = TcpListener::bind((cfg.listen_host.as_str(), cfg.listen_port)).map_err(|e| {
        CliError::failed(format!("bind {}:{}: {e}", cfg.listen_host, cfg.listen_port))
    })?;
    // The listener is polled rather than blocked on, so the loop notices the
    // run flag going false within one tick.
    listener
        .set_nonblocking(true)
        .map_err(|e| CliError::failed(format!("set_nonblocking: {e}")))?;

    let cache_cfg = cfg.cache_config();
    log.log(
        Level::Info,
        "feedd",
        "start",
        &[
            ("env", Field::Str(&cfg.env)),
            ("symbols", Field::U64(cache_cfg.symbols as u64)),
            ("shards", Field::U64(cache_cfg.shards as u64)),
            ("subscribers", Field::U64(cache_cfg.subscribers as u64)),
            ("ring", Field::U64(cfg.ring_capacity as u64)),
            ("sync", Field::Str(sync_label(cfg.sync_policy))),
        ],
    );

    // The ready line obctl waits for. It must be the last thing printed before
    // the listener is usable, and it is printed after `bind` has succeeded.
    note(&format!(
        "feedd listen {}:{} env={}",
        cfg.listen_host, cfg.listen_port, cfg.env
    ));

    let (tx, rx) = sync_channel::<Msg>(INGEST_QUEUE);
    let counters = Arc::new(Counters::default());

    // Connection threads. Detached: they end when their socket closes or the
    // process exits, and the publisher owns everything they write to.
    let acceptor = {
        let counters = Arc::clone(&counters);
        std::thread::Builder::new()
            .name("accept".to_string())
            .spawn(move || accept_loop(listener, tx, counters, &RUNNING))
            .map_err(|e| CliError::failed(format!("spawn accept thread: {e}")))?
    };

    // Subscribers. Each owns the read half of one queue and measures what it
    // sees; without them the fan-out would have nowhere to go and every queue
    // would fill, which the counters would report as loss.
    let mut handles = Vec::with_capacity(cache_cfg.subscribers);
    let mut consumers = Vec::with_capacity(cache_cfg.subscribers);
    let cache = cache::build(cache_cfg);
    let mut publisher = cache.publisher;
    for subscriber in cache.subscribers {
        consumers.push(subscriber);
    }
    for mut subscriber in consumers {
        let flag = Arc::new(AtomicBool::new(true));
        let stop = Arc::clone(&flag);
        let handle = std::thread::Builder::new()
            .name(format!("subscriber-{}", subscriber.id()))
            .spawn(move || {
                let mut out = vec![ob::cache::Event::placeholder(); 256];
                while stop.load(Ordering::Relaxed) {
                    let n = subscriber.drain(&mut out);
                    if n == 0 {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
                subscriber.close(clock::now_ns());
                subscriber
            })
            .map_err(|e| CliError::failed(format!("spawn subscriber: {e}")))?;
        handles.push((flag, handle));
    }

    // The publishing loop: drain the channel, publish, and run the timer.
    let mut published: u64 = 0;
    let started = clock::now_ns();
    let mut last_report = started;
    while RUNNING.load(Ordering::Relaxed) {
        match rx.recv_timeout(TICK) {
            Ok(mut m) => {
                publish_one(&mut m, &ring, &mut wal, &mut publisher, &counters)?;
                published += 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let now = clock::now_ns();
        if now.saturating_sub(last_report) >= 1_000_000_000 {
            report(&mut log, &publisher, &counters, published, started, now);
            last_report = now;
        }
    }

    // Shut down: stop the subscribers, then report once more.
    for (flag, _) in &handles {
        flag.store(false, Ordering::Relaxed);
    }
    let mut consumers = Vec::new();
    for (_, handle) in handles {
        if let Ok(subscriber) = handle.join() {
            consumers.push(subscriber);
        }
    }
    let now = clock::now_ns();
    wal.sync()?;
    log.log(
        Level::Info,
        "feedd",
        "stop",
        &[
            ("published", Field::U64(published)),
            ("elapsed_ms", Field::U64((now - started) / 1_000_000)),
        ],
    );
    report(&mut log, &publisher, &counters, published, started, now);
    for subscriber in &consumers {
        note(&ob::cache::subscriber_report(subscriber).to_string());
    }
    let _ = acceptor;
    Ok(())
}

/// Publish one record to the ring, the log and the cache.
///
/// The order matters and is the same in both implementations: the ring first so
/// that a live reader sees it with the least delay, then the log, then the cache.
/// The record's daemon stamp is taken before any of it, so the latency this
/// daemon reports covers its own work and not the client's.
fn publish_one(
    m: &mut Msg,
    ring: &Ring,
    wal: &mut Wal,
    publisher: &mut ob::cache::Publisher,
    counters: &Counters,
) -> CliResult {
    let t_recv = clock::now_ns();
    m.ts_ns = t_recv;
    // Fill the daemon's correlation counter. The sender's stamp in
    // `trace_id[0..8]` is left exactly as it arrived, so a record that carries
    // one keeps a measurable stage-1 latency and a record that does not is
    // reported as unmeasured. See `ob::msg`.
    let counter = counters.accepted.load(Ordering::Relaxed) + 1;
    m.fill_correlation(counter);

    ring.publish(m);
    wal.append(m)?;
    publisher.publish(m, t_recv);
    Ok(())
}

/// Print the periodic report line.
fn report(
    log: &mut Logger,
    publisher: &ob::cache::Publisher,
    counters: &Counters,
    published: u64,
    started: u64,
    now: u64,
) {
    let stats = publisher.stats();
    let latency = publisher.latency();
    let elapsed = now.saturating_sub(started);
    log.log(
        Level::Info,
        "feedd",
        "report",
        &[
            ("published", Field::U64(published)),
            ("applied", Field::U64(stats.applied)),
            ("ignored", Field::U64(stats.ignored)),
            ("crossed", Field::U64(stats.crossed)),
            ("evicted", Field::U64(stats.evicted)),
            ("dropped", Field::U64(stats.dropped)),
            ("symbols", Field::U64(publisher.symbol_count() as u64)),
            (
                "connections",
                Field::U64(counters.connections.load(Ordering::Relaxed)),
            ),
            (
                "invalid",
                Field::U64(counters.invalid.load(Ordering::Relaxed)),
            ),
            (
                "bytes",
                Field::Bytes(counters.bytes.load(Ordering::Relaxed)),
            ),
            ("rate", Field::Str(&rate(published, elapsed))),
        ],
    );
    log.log(
        Level::Info,
        "feedd",
        "latency",
        &[
            (
                "stage1",
                Field::Str(&latency.send_to_recv().summary().to_string()),
            ),
            (
                "stage2",
                Field::Str(&latency.recv_to_publish().summary().to_string()),
            ),
            (
                "ingress",
                Field::Str(&latency.total().summary().to_string()),
            ),
            ("stamped", Field::U64(latency.stamped())),
            ("unstamped", Field::U64(latency.unstamped())),
            ("implausible", Field::U64(latency.implausible())),
            (
                "peak_stage2_us",
                Field::Micros(latency.recv_to_publish().max()),
            ),
        ],
    );
}

/// A short label for a sync policy.
fn sync_label(p: SyncPolicy) -> &'static str {
    match p {
        SyncPolicy::None => "none",
        SyncPolicy::Data => "data",
        SyncPolicy::Full => "full",
    }
}

/// Install the two signal handlers.
///
/// `signal(2)` rather than `sigaction`: the signature is the same on macOS and
/// Linux, and the handler does one atomic store, which is all a handler may
/// safely do. The C++ daemon uses the same call for the same reason.
fn install_signal_handlers() -> CliResult {
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    for sig in [SIGINT, SIGTERM] {
        // SAFETY: `on_signal` only stores to a static `AtomicBool`, which is
        // async-signal-safe. The previous handler is discarded, which is what a
        // single-purpose daemon wants.
        let handler = on_signal as extern "C" fn(i32);
        let previous = unsafe { signal(sig, handler as *const () as usize) };
        if previous == usize::MAX {
            return Err(CliError::failed(format!(
                "cannot install handler for {sig}"
            )));
        }
    }
    Ok(())
}

extern "C" {
    /// `void (*signal(int, void (*)(int)))(int)`, as a function returning the
    /// previous handler as a bare address.
    fn signal(sig: i32, handler: usize) -> usize;
}
