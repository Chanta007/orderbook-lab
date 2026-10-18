//! `tui` — the terminal book, with a working input line.
//!
//! Draws the book ten times a second from the shared-memory ring. Bid prices are
//! red, ask prices are green, and the layout matches the C++ terminal.
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
//! # Why this owns the input line
//!
//! The first version read whole lines with `read_line` and cleared the screen on
//! every frame. That erases the terminal's own echo of what the user is typing,
//! ten times a second, so the commands were unusable: the input still reached
//! `read_line` — the tty's line buffer is not affected by drawing over the screen
//! — but nobody could see what they had typed.
//!
//! The fix is to stop depending on the terminal's echo and own the line:
//!
//! 1. Put the tty into non-canonical mode without echo, so bytes arrive one at a
//!    time and nothing is echoed by the kernel.
//! 2. Keep the buffer here, and draw it as the last line of every frame.
//! 3. Never clear the screen below the frame's own last row without redrawing
//!    that row, so there is no window in which the typed text is missing.
//!
//! [`TerminalMode`] restores the terminal on the way out, including from a
//! signal, so a Ctrl-C does not leave the shell in a mode where nothing echoes.
//!
//! # Input
//!
//! Read one byte at a time on a thread of its own, because a blocking read must
//! not stop the frame from drawing. `quit` and `q` leave; `levels N` changes the
//! depth, from 1 to 64. Ctrl-C and Ctrl-D also leave. Backspace works.

use ob::book::{Book, Level};
use ob::clock;
use ob::config::Config;
use ob::msg::Msg;
use ob::ring::RingReader;
use ob_cli::{load_config, CliError, CliResult};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long without a record before the header says the feed has stalled.
const STALL_AFTER: Duration = Duration::from_secs(2);

/// How often a frame is drawn.
const FRAME: Duration = Duration::from_millis(100);

/// The run flag. The signal handler is a bare `extern "C"` function and cannot
/// capture anything, so this is the one place it can write.
static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" {
    /// `void (*signal(int, void (*)(int)))(int)`.
    fn signal(sig: i32, handler: usize) -> usize;
}

/// Ask the loop to stop. Async-signal-safe: one atomic store.
extern "C" fn on_signal(_sig: i32) {
    RUNNING.store(false, Ordering::SeqCst);
}

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("tui: {e}"));
        std::process::exit(e.code);
    }
}

/// The terminal mode this program needs, restored on drop.
///
/// `stty` rather than `tcsetattr` because the `termios` structure differs between
/// Darwin and Linux and this program needs four flags from it. `stty` already
/// knows both platforms, and the two commands below are the whole of what is
/// required.
///
/// The mode is only changed when stdin is a terminal. A piped or redirected
/// stdin keeps the line-based reader and the frame says so, which is what makes
/// this program usable from a script and testable from a pseudo-terminal.
#[derive(Debug)]
struct TerminalMode {
    /// Whether this value actually changed anything, and so must restore it.
    owns_mode: bool,
}

impl TerminalMode {
    /// Switch the terminal to byte-at-a-time input with no echo.
    fn enter() -> Self {
        if !stdin_is_tty() {
            return TerminalMode { owns_mode: false };
        }
        let ok = stty(&["-icanon", "-echo", "min", "1", "time", "0"]);
        TerminalMode { owns_mode: ok }
    }
}

impl Drop for TerminalMode {
    fn drop(&mut self) {
        if self.owns_mode {
            // Explicit flags rather than a saved string. Restoring an opaque
            // `stty -g` blob means passing it back through a shell, where its
            // brackets are glob characters. These two are what was changed, and
            // they are what an interactive shell wants.
            let _ = stty(&["icanon", "echo"]);
        }
    }
}

/// Whether stdin is a terminal.
fn stdin_is_tty() -> bool {
    // SAFETY: `isatty` takes a file descriptor and has no other requirement.
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(0) == 1 }
}

/// Run `stty` with the given arguments against this process's stdin.
fn stty(args: &[&str]) -> bool {
    use std::process::{Command, Stdio};
    Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage("usage: tui <config.json>"));
    };
    let cfg = load_config(cfg_path)?;
    let levels = args.number::<usize>("--levels", cfg.levels)?;

    let ring = RingReader::open(std::path::Path::new(&cfg.bus_path))?;
    let mut book = Book::new(cfg.cache_depth.max(levels + 1));
    // Start at the live end: show what arrives from now on, not history.
    let mut cursor = ring.published();
    let level_count = Arc::new(AtomicUsize::new(levels.clamp(1, 64)));
    let typed = Arc::new(Mutex::new(String::new()));

    // Take the terminal before the input thread starts, so no byte is read in the
    // wrong mode. The guard restores it on every exit path below, including the
    // early returns, a signal, and a panic.
    let interactive = stdin_is_tty();
    let _mode = TerminalMode::enter();

    install_signal_handlers()?;
    let input = spawn_input_thread(Arc::clone(&typed), Arc::clone(&level_count));

    let mut out = [Msg::default(); 512];
    let mut last_record = Instant::now();
    let mut saw_any = false;
    let mut consumed = 0u64;
    let resyncs = 0u64;

    let stdout = std::io::stdout();
    let mut screen = stdout.lock();
    while RUNNING.load(Ordering::Relaxed) {
        let n = ring.consume_batch(&mut cursor, &mut out);
        if n > 0 {
            for m in &out[..n] {
                book.apply(m);
            }
            consumed += n as u64;
            last_record = Instant::now();
            saw_any = true;
        }
        let buffer = typed.lock().map(|b| b.clone()).unwrap_or_default();
        draw(
            &mut screen,
            &cfg,
            &book,
            &Frame {
                levels: level_count.load(Ordering::Relaxed),
                live: saw_any && last_record.elapsed() < STALL_AFTER,
                ring_capacity: ring.capacity(),
                lag: ring.lag(cursor),
                backlog_ns: backlog_ns(&ring, cursor),
                staleness_ns: staleness_ns(&ring),
                age: last_record.elapsed(),
                consumed,
                resyncs,
                input: &buffer,
                interactive,
            },
        )?;
        std::thread::sleep(FRAME);
    }

    // Leave the terminal on a clean line. The drop guard restores the mode; this
    // only moves the cursor past the frame.
    let _ = writeln!(
        screen,
        "\nclosed. The feed keeps running until `obctl stop`."
    );
    let _ = screen.flush();
    drop(screen);
    if input.is_finished() {
        let _ = input.join();
    }
    Ok(())
}

/// What one frame needs beyond the book.
#[derive(Debug)]
struct Frame<'a> {
    levels: usize,
    live: bool,
    /// The ring's own capacity, read from its header.
    ///
    /// Read from the ring rather than from the config, because the two can
    /// disagree: the file survives between runs, and a config that changed
    /// `ring_capacity` does not resize a ring that already exists.
    ring_capacity: u32,
    /// Records published and not yet taken by this reader.
    lag: u64,
    /// Feed time between the oldest record not yet taken and the newest
    /// published one. `None` when either of those slots has been reused.
    ///
    /// This is the lag in time rather than in records: a count needs the feed's
    /// rate to mean anything, and a span does not.
    backlog_ns: Option<u64>,
    /// How old the newest published record is, measured from this reader's
    /// clock. Non-zero on a quiet feed as well as on a slow reader, which is
    /// what makes it the number to watch.
    staleness_ns: Option<u64>,
    /// Time since this reader last applied a record.
    age: Duration,
    consumed: u64,
    resyncs: u64,
    /// What the user has typed and not yet submitted.
    input: &'a str,
    /// Whether the input line is being drawn at all.
    interactive: bool,
}

/// Feed time between the oldest record not yet taken and the newest published.
///
/// The two slots are read by sequence, and either can have been reused by a later
/// lap; when one has, this reports nothing rather than a span computed from the
/// wrong lap. Zero is the honest answer when there is nothing waiting: a reader
/// at the live end has a backlog of zero by definition.
fn backlog_ns(ring: &RingReader, cursor: u64) -> Option<u64> {
    let newest = ring.published().checked_sub(1)?;
    if cursor > newest {
        return Some(0);
    }
    let first_waiting = ring.timestamp_at(cursor)?;
    let latest = ring.timestamp_at(newest)?;
    Some(latest.saturating_sub(first_waiting))
}

/// How old the newest published record is, from this process's clock.
///
/// `None` when the ring is empty or the slot has been reused. The two ends are on
/// one host reading one monotonic clock, which is the same assumption every
/// latency number in this lab rests on.
fn staleness_ns(ring: &RingReader) -> Option<u64> {
    let newest = ring.published().checked_sub(1)?;
    let latest = ring.timestamp_at(newest)?;
    Some(clock::now_ns().saturating_sub(latest))
}

/// Draw one frame.
fn draw(out: &mut impl Write, cfg: &Config, book: &Book, frame: &Frame<'_>) -> CliResult {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const RESET: &str = "\x1b[0m";

    let mut bids = vec![Level::EMPTY; frame.levels];
    let mut asks = vec![Level::EMPTY; frame.levels];
    let nb = book.bids.top(frame.levels, &mut bids);
    let na = book.asks.top(frame.levels, &mut asks);
    let feed = if frame.live { "live" } else { "stalled" };

    // Home the cursor and redraw every row, erasing each to the end of its line.
    // The whole screen is not cleared, because the input line is the last row of
    // this frame and clearing the screen under it is what used to erase the
    // user's typing.
    let mut buf = String::with_capacity(2048);
    buf.push_str("\x1b[H");
    buf.push_str(&format!(
        "orderbook-lab (rust)  env={}  symbol={}  seq={}  feed={}\x1b[K\n",
        cfg.env,
        cfg.symbol,
        book.last_seq(),
        feed
    ));
    // The lag is shown twice on purpose. A count of records is what a drain loop
    // acts on; the span of feed time is what says whether that count matters, and
    // it is the one an operator can compare against a threshold without knowing
    // the feed's rate.
    let backlog = match frame.backlog_ns {
        Some(ns) => ob_cli::duration_ns(ns),
        None => "-".to_string(),
    };
    let staleness = match frame.staleness_ns {
        Some(ns) => ob_cli::duration_ns(ns),
        None => "-".to_string(),
    };
    buf.push_str(&format!(
        "ring={}  lag={} records ({backlog} of feed time)  staleness={staleness}\x1b[K\n",
        frame.ring_capacity, frame.lag
    ));
    buf.push_str(&format!(
        "consumed={} records  resyncs={}  age={}\x1b[K\n",
        frame.consumed,
        frame.resyncs,
        ob_cli::duration_ns(frame.age.as_nanos().min(u128::from(u64::MAX)) as u64)
    ));
    buf.push_str("Bid prices are red. Ask prices are green.\x1b[K\n");
    buf.push_str("  quit | q     leave this window\x1b[K\n");
    buf.push_str("  levels N     show N price levels, from 1 to 64\x1b[K\n");
    buf.push_str("  lag          records behind you, and the span of feed time that is.\x1b[K\n");
    buf.push_str(
        "  staleness    how old the newest published record is. Both are your view, not the exchange's.\x1b[K\n",
    );
    buf.push_str(&format!("Showing {} levels.\x1b[K\n", frame.levels));
    if !frame.live {
        buf.push_str(
            "Feed stalled. The adapter reconnects on its own. The book stays on the last update.\x1b[K\n",
        );
    }
    if frame.lag > u64::from(frame.ring_capacity) / 2 {
        buf.push_str(
            "Ring lag is over half the ring: records are being overwritten. Drain faster.\x1b[K\n",
        );
    }
    // A quiet feed and a slow reader both show up here, and they are the same
    // problem for a viewer: what is on screen is old.
    if frame.staleness_ns.is_some_and(|ns| ns > 1_000_000_000) {
        buf.push_str(
            "The newest record is over a second old: the feed has stopped, or the sender is slow.\x1b[K\n",
        );
    }
    buf.push_str(&format!(
        "{RED}{:>12} {:>12}{RESET}    {GREEN}{:>12} {:>12}{RESET}\x1b[K\n",
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
            "{RED}{bp:>12} {bq:>12}{RESET}    {GREEN}{ap:>12} {aq:>12}{RESET}\x1b[K\n"
        ));
    }
    // Clear whatever is left below the frame from an earlier, longer one. This
    // happens before the prompt is written, so the prompt is never erased.
    buf.push_str("\x1b[J");
    // The input line, drawn by us rather than by the tty. No newline after it, so
    // the cursor sits where the next character will be inserted.
    if frame.interactive {
        buf.push_str(&format!("> {}\x1b[K", frame.input));
    } else {
        buf.push_str("> (input disabled: stdin is not a terminal)\x1b[K");
    }
    out.write_all(buf.as_bytes())
        .map_err(|e| CliError::failed(format!("draw: {e}")))?;
    out.flush()
        .map_err(|e| CliError::failed(format!("flush: {e}")))?;
    Ok(())
}

/// Install handlers so a signal leaves the terminal usable.
///
/// Without this a Ctrl-C would kill the process with the tty still in
/// non-canonical mode, and the shell would look broken until the user ran
/// `stty sane`. The handler only sets the run flag; the loop then leaves by the
/// normal path, which drops the mode guard.
fn install_signal_handlers() -> CliResult {
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    for sig in [SIGINT, SIGTERM] {
        let handler = on_signal as extern "C" fn(i32);
        // SAFETY: the handler stores to a static `AtomicBool`, which is
        // async-signal-safe.
        let previous = unsafe { signal(sig, handler as *const () as usize) };
        if previous == usize::MAX {
            return Err(CliError::failed(format!(
                "cannot install handler for {sig}"
            )));
        }
    }
    Ok(())
}

/// Read keystrokes and maintain the input line.
///
/// Detached and not joined when it is blocked in a read: a terminal that has been
/// asked to quit must quit, and a read on a tty cannot be interrupted from here.
/// The process exits and the thread goes with it. When stdin is not a terminal
/// this still works — it reads whatever bytes arrive and treats a newline as end
/// of command — which is what makes the pseudo-terminal test possible.
fn spawn_input_thread(
    typed: Arc<Mutex<String>>,
    levels: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("input".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut handle = stdin.lock();
            let mut byte = [0u8; 1];
            loop {
                match handle.read(&mut byte) {
                    Ok(0) => {
                        // End of input.
                        RUNNING.store(false, Ordering::Relaxed);
                        return;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        // A signal. The handler has set the flag; leave so the
                        // mode guard can run.
                        return;
                    }
                    Err(_) => {
                        RUNNING.store(false, Ordering::Relaxed);
                        return;
                    }
                }
                match byte[0] {
                    // Ctrl-C, in case it arrives as a byte rather than as a
                    // signal, and Ctrl-D for end of input.
                    0x03 | 0x04 => {
                        RUNNING.store(false, Ordering::Relaxed);
                        return;
                    }
                    b'\r' | b'\n' => {
                        let line = typed
                            .lock()
                            .map(|mut b| std::mem::take(&mut *b))
                            .unwrap_or_default();
                        submit(&line, &levels);
                    }
                    // Backspace, and the delete key's other spelling.
                    0x7f | 0x08 => {
                        if let Ok(mut b) = typed.lock() {
                            b.pop();
                        }
                    }
                    // Printable ASCII. Anything else is an escape sequence from
                    // an arrow key or a terminal reply, and is ignored rather
                    // than appended to the line.
                    0x20..=0x7e => {
                        if let Ok(mut b) = typed.lock() {
                            b.push(byte[0] as char);
                        }
                    }
                    _ => {}
                }
            }
        })
        .expect("spawn the input thread")
}

/// Act on a submitted command.
///
/// Unknown commands are ignored, which is the same behaviour the C++ terminal
/// has: a typo does nothing rather than stopping the viewer.
fn submit(line: &str, levels: &AtomicUsize) {
    let cmd = line.trim();
    if cmd == "q" || cmd == "quit" {
        RUNNING.store(false, Ordering::Relaxed);
    } else if let Some(n) = parse_levels(cmd) {
        levels.store(n, Ordering::Relaxed);
    }
}

/// The depth limits a `levels` command accepts.
pub const LEVELS_MIN: usize = 1;
/// The depth limits a `levels` command accepts.
pub const LEVELS_MAX: usize = 64;

/// Parse a `levels N` command, or return `None`.
///
/// Separate from the input thread so it can be tested without a terminal, which
/// is the only part of this file that a unit test can reach.
pub fn parse_levels(line: &str) -> Option<usize> {
    let rest = line.trim().strip_prefix("levels ")?;
    let n: usize = rest.trim().parse().ok()?;
    if (LEVELS_MIN..=LEVELS_MAX).contains(&n) {
        Some(n)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parses_within_range_and_rejects_outside_it() {
        assert_eq!(parse_levels("levels 5"), Some(5));
        assert_eq!(parse_levels("  levels 1  "), Some(1));
        assert_eq!(parse_levels("levels 64"), Some(64));
        assert_eq!(parse_levels("levels 0"), None);
        assert_eq!(parse_levels("levels 65"), None);
        assert_eq!(parse_levels("levels"), None);
        assert_eq!(parse_levels("levels 5 "), Some(5));
        assert_eq!(parse_levels("levels1 5"), None);
        assert_eq!(parse_levels("levels abc"), None);
        assert_eq!(parse_levels("quit"), None);
    }
}
