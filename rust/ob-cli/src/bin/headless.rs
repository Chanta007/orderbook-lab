//! `headless` — read the ring and the log, and say whether the feed worked.
//!
//! This is the end-to-end checker. It is the same program as the C++ `headless`
//! in everything a caller can see: the same arguments, the same output line, and
//! the same exit code. `make e2e` runs one and `make rust-e2e` runs the other,
//! and `make rust-interop` runs one against the other's ring.
//!
//! ```text
//!   headless <config.json> <wait_ms>
//! ```
//!
//! It attaches to the ring the daemon created, drains it from cursor zero, and
//! applies every record to a book. Then it replays the write-ahead log. It exits
//! 0 only when both sides of the book have a level and the log has at least one
//! record, which is exactly the C++ checker's rule.
//!
//! It deliberately does not check prices. A price check would be a check of the
//! feed's content, and the fixture's content is the adapter's business; what this
//! checker is for is proving that the bytes made it from a socket to a ring, from
//! a ring to a book, and from a socket to a log.

use ob::book::Book;
use ob::clock;
use ob::log::{Field, Level, Logger};
use ob::msg::Msg;
use ob::ring::{Consume, RingReader};
use ob::wal;
use ob_cli::{load_config, say, CliError, CliResult};
use std::time::{Duration, Instant};

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("headless: {e}"));
        std::process::exit(e.code);
    }
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage("usage: headless <config.json> <wait_ms>"));
    };
    let Some(wait_ms) = args.positional(1) else {
        return Err(CliError::usage("usage: headless <config.json> <wait_ms>"));
    };
    let wait_ms: u64 = wait_ms
        .parse()
        .map_err(|_| CliError::usage(format!("wait_ms must be a number, got {wait_ms:?}")))?;
    let cfg = load_config(cfg_path)?;

    // A read-only view of a ring the daemon created. Attaching must not create
    // one: a zero-filled file would look like a valid empty ring and this
    // checker would report a failure that looks like the daemon's fault.
    let ring = RingReader::open(std::path::Path::new(&cfg.bus_path))?;
    let mut book = Book::new(cfg.cache_depth);
    let mut cursor = 0u64;
    let mut out = [Msg::default(); 512];
    let mut consumed = 0u64;
    let resyncs = 0u64;

    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    while Instant::now() < deadline {
        let n = ring.consume_batch(&mut cursor, &mut out);
        for m in &out[..n] {
            book.apply(m);
            consumed += 1;
        }
        if n == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // The log holds everything that was ever published, including records the
    // ring has already overwritten. Replaying it is the point-in-time-recovery
    // path, and it is checked here so a broken log cannot pass unnoticed.
    let replayed = wal::replay(std::path::Path::new(&cfg.wal_path))?;

    say(&format!(
        "bids={} asks={} seq={} wal={}",
        book.bids.len(),
        book.asks.len(),
        book.last_seq(),
        replayed.records.len()
    ));

    let mut log = Logger::new(Level::Info);
    log.log(
        Level::Info,
        "headless",
        "summary",
        &[
            ("consumed", Field::U64(consumed)),
            ("resyncs", Field::U64(resyncs)),
            ("ring_capacity", Field::U64(ring.capacity() as u64)),
            ("ring_published", Field::U64(ring.published())),
            ("wal_records", Field::U64(replayed.records.len() as u64)),
            ("wal_rejected", Field::U64(replayed.rejected as u64)),
            (
                "wal_torn_bytes",
                Field::U64(replayed.torn_tail_bytes as u64),
            ),
            ("crossed", Field::Bool(book.is_crossed())),
            ("bid_levels", Field::U64(book.bids.len() as u64)),
            ("ask_levels", Field::U64(book.asks.len() as u64)),
            ("last_seq", Field::U64(book.last_seq())),
        ],
    );

    // A torn tail is worth reporting and not worth failing on: it is what a
    // crash mid-append looks like, and the log is designed to survive it.
    if !replayed.clean_tail() {
        log.log(
            Level::Warn,
            "headless",
            "wal-tail",
            &[
                ("rejected", Field::U64(replayed.rejected as u64)),
                ("torn_bytes", Field::U64(replayed.torn_tail_bytes as u64)),
            ],
        );
    }

    // A crossed book means a price that should have been cleared was not. It is
    // reported rather than failed on, because it is a statement about the feed's
    // content and this checker is about the path.
    if book.is_crossed() {
        log.log(
            Level::Warn,
            "headless",
            "crossed",
            &[("seq", Field::U64(book.last_seq()))],
        );
    }

    if book.bids.is_empty() || book.asks.is_empty() || replayed.records.is_empty() {
        return Err(CliError::failed(format!(
            "incomplete: bids={} asks={} wal={} (consumed {consumed} records by {} ms)",
            book.bids.len(),
            book.asks.len(),
            replayed.records.len(),
            wait_ms
        )));
    }
    let _ = clock::now_ns();
    let _ = Consume::Empty;
    Ok(())
}
