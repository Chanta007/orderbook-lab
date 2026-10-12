//! `tui` — the terminal book.
//!
//! Draws the book ten times a second from the shared-memory ring. Bid prices are
//! red, ask prices are green, and the layout matches the C++ terminal so the two
//! implementations look the same side by side.
//!
//! ```text
//!   tui <config.json>
//! ```
//!
//! # The cursor starts at the live end
//!
//! The C++ terminal starts at the ring's write sequence, so it shows what arrives
//! after it opens rather than replaying history. This does the same, and the
//! reason is the same: a book built from a partial replay would be missing the
//! resets that came before it, and a stale price would sit above the asks until
//! the next picture arrived.
//!
//! # What this adds over the C++ terminal
//!
//! Two numbers that the ring can supply and the C++ version does not show:
//! **lag**, how many records behind the writer this reader is, and **age**, how
//! long since a record was applied. They are the cross-process form of the
//! backpressure state that the in-process cache reports through
//! [`ob::pressure`], and they are the first thing to look at when the book stops
//! moving.
//!
//! # Input
//!
//! A second thread reads lines, because `read_line` blocks and the draw loop must
//! keep running while nobody is typing. The refresh erases the screen, so the
//! characters being typed are not visible; they still reach the reader when Enter
//! is pressed. The C++ terminal has the same behaviour and says so on screen.

use ob::book::{Book, Level};
use ob::clock;
use ob::config::Config;
use ob::msg::{Msg, Side};
use ob::ring::{Consume, RingReader};
use ob_cli::{load_config, CliError, CliResult};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long without a record before the header says the feed has stalled.
const STALL_AFTER: Duration = Duration::from_secs(2);

/// How often a frame is drawn.
const FRAME: Duration = Duration::from_millis(100);

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("tui: {e}"));
        std::process::exit(e.code);
    }
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage("usage: tui <config.json>"));
    };
    let cfg = load_config(cfg_path)?;
    let levels = args.number::<usize>("levels", cfg.levels)?;

    let ring = RingReader::open(std::path::Path::new(&cfg.bus_path))?;
    let mut book = Book::new(cfg.cache_depth.max(levels + 1));
    // Start at the live end: show what arrives from now on, not history.
    let mut cursor = ring.published();
    let running = Arc::new(AtomicBool::new(true));
    let level_count = Arc::new(AtomicUsize::new(levels.clamp(1, 64)));

    let input = spawn_input_thread(Arc::clone(&running), Arc::clone(&level_count));

    let mut out = [Msg::default(); 512];
    let mut last_record = Instant::now();
    let mut saw_any = false;
    let mut consumed = 0u64;
    let resyncs = 0u64;

    let stdout = std::io::stdout();
    let mut screen = stdout.lock();
    while running.load(Ordering::Relaxed) {
        let n = ring.consume_batch(&mut cursor, &mut out);
        if n > 0 {
            for m in &out[..n] {
                book.apply(m);
            }
            consumed += n as u64;
            last_record = Instant::now();
            saw_any = true;
        }
        draw(
            &mut screen,
            &cfg,
            &book,
            &Frame {
                levels: level_count.load(Ordering::Relaxed),
                live: saw_any && last_record.elapsed() < STALL_AFTER,
                lag: ring.lag(cursor),
                age: last_record.elapsed(),
                consumed,
                resyncs,
            },
        )?;
        std::thread::sleep(FRAME);
    }

    // Leave the terminal on a clean line.
    let _ = writeln!(
        screen,
        "\nclosed. The feed keeps running until `obctl stop`."
    );
    let _ = screen.flush();
    if input.is_finished() {
        let _ = input.join();
    }
    Ok(())
}

/// What one frame needs beyond the book.
#[derive(Debug, Clone, Copy)]
struct Frame {
    levels: usize,
    live: bool,
    lag: u64,
    age: Duration,
    consumed: u64,
    resyncs: u64,
}

/// Draw one frame.
fn draw(out: &mut impl Write, cfg: &Config, book: &Book, frame: &Frame) -> CliResult {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const RESET: &str = "\x1b[0m";

    let mut bids = vec![Level::EMPTY; frame.levels];
    let mut asks = vec![Level::EMPTY; frame.levels];
    let nb = book.bids.top(frame.levels, &mut bids);
    let na = book.asks.top(frame.levels, &mut asks);
    let feed = if frame.live { "live" } else { "stalled" };

    // Home the cursor and erase, then draw the whole frame: the command list is
    // part of the frame rather than printed once, which is why it survives.
    let mut buf = String::with_capacity(2048);
    buf.push_str("\x1b[H\x1b[2J");
    buf.push_str(&format!(
        "orderbook-lab (rust)  env={}  symbol={}  seq={}  feed={}\n",
        cfg.env,
        cfg.symbol,
        book.last_seq(),
        feed
    ));
    buf.push_str(&format!(
        "ring={} lag={} records  age={:.1}s  consumed={} resyncs={}\n",
        cfg.ring_capacity,
        frame.lag,
        frame.age.as_secs_f64(),
        frame.consumed,
        frame.resyncs
    ));
    buf.push_str("Bid prices are red. Ask prices are green.\n");
    buf.push_str("Type a command and press Enter. Refresh clears the echo.\n");
    buf.push_str("  quit | q     leave this window\n");
    buf.push_str("  levels N     show N price levels, from 1 to 64\n");
    buf.push_str("  help         this list is already on screen\n");
    if !frame.live {
        buf.push_str(
            "Feed stalled. The adapter reconnects on its own. The book stays on the last update.\n",
        );
    }
    if frame.lag > cfg.ring_capacity as u64 / 2 {
        buf.push_str(
            "Ring lag is over half the ring: records are being overwritten. Drain faster.\n",
        );
    }
    buf.push_str(&format!(
        "{RED}{:>12} {:>12}{RESET}    {GREEN}{:>12} {:>12}{RESET}\n",
        "BID PX", "BID QTY", "ASK PX", "ASK QTY"
    ));
    let rows = nb.max(na);
    for i in 0..rows {
        let (bp, bq) = match bids.get(i) {
            Some(l) if l.is_present() => (l.px.format(2), l.qty.to_string()),
            _ => (String::new(), String::new()),
        };
        let (ap, aq) = match asks.get(i) {
            Some(l) if l.is_present() => (l.px.format(2), l.qty.to_string()),
            _ => (String::new(), String::new()),
        };
        buf.push_str(&format!(
            "{RED}{bp:>12} {bq:>12}{RESET}    {GREEN}{ap:>12} {aq:>12}{RESET}\n"
        ));
    }
    out.write_all(buf.as_bytes())
        .map_err(|e| CliError::failed(format!("draw: {e}")))?;
    out.flush()
        .map_err(|e| CliError::failed(format!("flush: {e}")))?;
    Ok(())
}

/// Read commands on a thread of their own.
///
/// Detached and not joined when it is still blocked in `read_line`: a terminal
/// that has been asked to quit must quit, and a thread blocked on stdin cannot
/// be interrupted. The process exits and the thread goes with it.
fn spawn_input_thread(
    running: Arc<AtomicBool>,
    levels: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("input".to_string())
        .spawn(move || {
            let mut line = String::new();
            while running.load(Ordering::Relaxed) {
                line.clear();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) => {
                        running.store(false, Ordering::Relaxed);
                        return;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        running.store(false, Ordering::Relaxed);
                        return;
                    }
                }
                let cmd = line.trim();
                if cmd == "q" || cmd == "quit" {
                    running.store(false, Ordering::Relaxed);
                    return;
                }
                if let Some(rest) = cmd.strip_prefix("levels ") {
                    if let Ok(n) = rest.trim().parse::<usize>() {
                        levels.store(n.clamp(1, 64), Ordering::Relaxed);
                    }
                }
            }
        })
        .expect("spawn the input thread")
}

/// A short description of the ring a terminal is attached to, for a caller that
/// wants to print one before drawing.
pub fn describe(ring: &RingReader) -> String {
    format!(
        "ring capacity={} published={}",
        ring.capacity(),
        ring.published()
    )
}

/// Read one record without drawing, for a caller that wants the raw stream.
pub fn peek(ring: &RingReader, cursor: &mut u64, out: &mut Msg) -> Consume {
    let _ = clock::now_ns();
    ring.consume(cursor, out)
}

/// The side a record names, for a caller that wants to label it.
pub fn side_of(m: &Msg) -> Side {
    m.side
}
