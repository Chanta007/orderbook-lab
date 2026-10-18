//! `tui` — the terminal book: several tickers, a picker, and a working input line.
//!
//! Draws one symbol's book ten times a second from the shared-memory ring. Bid
//! prices are red, ask prices are green.
//!
//! ```text
//!   tui <config.json> [--levels N]
//! ```
//!
//! # Watching several tickers
//!
//! The terminal writes `var/<env>/watch.txt` — the symbols it wants subscribed to
//! — and the adapter reads it and holds one websocket for exactly that set. Typing
//! a symbol, or picking one from the list, adds it to the set and switches the
//! book to it. So the subscription follows what is being looked at rather than
//! being a fixed list decided at startup. [`ob_cli::watch`] owns that file.
//!
//! # The picker
//!
//! While there is something on the input line, the terminal offers the catalog's
//! matching symbols below the book and above the prompt. `Tab` completes to the
//! best match, the arrow keys move the highlight, and `Enter` takes the
//! highlighted one. Ranking lives in [`ob_cli::tickers`]: a prefix beats an
//! interior match, and the file's order breaks ties.
//!
//! # Why this owns the input line
//!
//! The first version read whole lines with `read_line` and cleared the screen on
//! every frame, which erased the terminal's own echo of what the user was typing.
//! The input reached `read_line` — the tty's line buffer is not affected by drawing
//! over the screen — so commands worked and were invisible.
//!
//! The fix is to stop depending on the terminal's echo and own the line: put the
//! tty into non-canonical mode without echo, keep the buffer here, draw it as the
//! last row of every frame, and restore the mode on every exit path including a
//! signal. [`TerminalMode`].
//!
//! # The three times in the header
//!
//! - **backlog** is the span of feed time covered by the records this reader has
//!   not taken. A count of records needs the feed's rate to mean anything; a span
//!   does not.
//! - **staleness** is how old the newest published record is. It grows when the
//!   feed stops as well as when the reader falls behind.
//! - **age** is how long since this reader last applied a record, which is the one
//!   that says whether *this window* is being fed.
//!
//! All three are this reader's view, not the exchange's.
//!
//! # Losing the feed, and getting it back
//!
//! `feed=stalled` is one state with three causes, and the window says which: an
//! adapter that is alive and an exchange that has gone quiet, an adapter that has
//! exited, or no adapter at all. That comes from
//! `var/<env>/adapter.heartbeat`, which the adapter rewrites as it works
//! ([`ob_cli::read_heartbeat`]).
//!
//! A window does not stay attached to a ring by itself. The daemon truncates the
//! ring's header when it starts, so the write sequence begins again at zero while
//! this cursor is a number from the run that ended; and a mapping follows the
//! inode it was made from, so a replaced ring file leaves the window reading a
//! file nothing writes. Both look like a healthy ring with nothing new in it, so
//! the window checks ([`reattach`]) rather than infers, and rebuilds its view when
//! either has happened. `reattaches=` in the header counts it.
//!
//! The remedy the window prints for a stopped feed is `make rust-feed`, which
//! starts the feed and the adapter and no window. It used to print `make stop`
//! first, and `stop` signals this process as well.

use ob::book::{Book, Level};
use ob::clock;
use ob::config::Config;
use ob::msg::{symbol_fits, Msg};
use ob::ring::RingReader;
use ob_cli::tickers::Catalog;
use ob_cli::watch::Watch;
use ob_cli::{load_config, CliError, CliResult};
use ob_cli::{read_heartbeat, Liveness, HEARTBEAT_NAME};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long without a record before the header says the feed has stalled.
const STALL_AFTER: Duration = Duration::from_secs(2);

/// How often a frame is drawn.
const FRAME: Duration = Duration::from_millis(100);

/// How often the window checks that the ring it mapped is still the ring at the
/// path, and that the writer has not started a new run.
///
/// A stat is cheap enough for every frame, and the check is not: it decides
/// whether the window is attached to the feed that is running now. A quarter of
/// a second is the longest a person would call immediate.
const RING_CHECK: Duration = Duration::from_millis(250);

/// How many suggestions the picker offers at once.
const SUGGESTIONS: usize = 8;

/// Bytes the input thread may hold, so a paste cannot grow it without bound.
const INPUT_LIMIT: usize = 64;

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
/// Darwin and Linux and this program needs four flags from it. `stty` already knows
/// both platforms.
#[derive(Debug)]
struct TerminalMode {
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
            // brackets are glob characters.
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

/// What the input thread and the draw loop share.
#[derive(Debug, Default)]
struct Input {
    /// What has been typed and not submitted.
    line: String,
    /// Which suggestion is highlighted.
    highlighted: usize,
    /// A request from the input thread for the loop to act on.
    pending: Option<Command>,
}

/// A command the input thread parsed, to be run by the draw loop.
///
/// The thread that reads keystrokes does not touch the book or the watch file: it
/// records an intent and the loop performs it. One thread owns the state, which is
/// why none of it needs a lock beyond the small one around this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    /// A line the user submitted.
    Submit(String),
    /// Replace the line with the completion of what is on it.
    Complete,
    /// Move the highlight by this much, wrapping.
    MoveHighlight(isize),
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage("usage: tui <config.json> [--levels N]"));
    };
    let cfg = load_config(cfg_path)?;
    let levels = args.number::<usize>("--levels", cfg.levels)?;

    let ring_path = PathBuf::from(&cfg.bus_path);
    let mut ring = RingReader::open(&ring_path)?;
    let catalog = Catalog::load(&PathBuf::from(&cfg.tickers_path))?;
    let watch_path = PathBuf::from(&cfg.watch_path);
    // Beside the watch list, because that is where the adapter looks for it.
    let heartbeat_path = watch_path.with_file_name(HEARTBEAT_NAME);
    let mut watch = Watch::load(&watch_path, &cfg.symbol)?;
    // The terminal is the authority on what is watched while it runs, so write the
    // set it opened with. Without this the adapter would keep whatever was there
    // from last time, which may be a symbol this window is not showing.
    let mut status = match watch.save(&watch_path) {
        Ok(()) => format!("watching {}: {}", watch.len(), watch.symbols().join(" ")),
        Err(e) => format!("cannot write {}: {}", watch_path.display(), e.message),
    };

    let mut book = Book::new(cfg.cache_depth.max(levels + 1));
    let mut cursor = ring.published();
    let level_count = Arc::new(AtomicUsize::new(levels.clamp(1, 64)));
    let input = Arc::new(Mutex::new(Input::default()));

    let interactive = stdin_is_tty();
    let _mode = TerminalMode::enter();
    install_signal_handlers()?;
    let reader = spawn_input_thread(Arc::clone(&input));

    let mut out = [Msg::default(); 512];
    let mut last_record = Instant::now();
    let mut saw_any = false;
    let mut consumed = 0u64;
    let mut resyncs = 0u64;
    let mut reattaches = 0u64;
    let mut last_ring_check = Instant::now();
    let mut displayed = watch.current().to_string();

    let stdout = std::io::stdout();
    let mut screen = stdout.lock();
    while RUNNING.load(Ordering::Relaxed) {
        // Take whatever the input thread has asked for.
        let (line, highlighted, pending) = {
            let mut guard = input.lock().map_err(|_| {
                CliError::failed("the input lock was poisoned by a panic in the reader")
            })?;
            let pending = guard.pending.take();
            (guard.line.clone(), guard.highlighted, pending)
        };
        if let Some(command) = pending {
            match command {
                Command::MoveHighlight(delta) => {
                    let matches = catalog.suggest(&first_token(&line), SUGGESTIONS);
                    if !matches.is_empty() {
                        let n = matches.len() as isize;
                        let next = (highlighted as isize + delta).rem_euclid(n);
                        if let Ok(mut guard) = input.lock() {
                            guard.highlighted = next as usize;
                        }
                    }
                }
                Command::Complete => {
                    if let Some(best) = catalog.complete(&first_token(&line)) {
                        if let Ok(mut guard) = input.lock() {
                            guard.line = best.to_string();
                            guard.highlighted = 0;
                        }
                    }
                }
                Command::Submit(text) => {
                    match act(&text, &catalog, &mut watch, &watch_path) {
                        Action::Quit => break,
                        Action::Depth(n) => level_count.store(n, Ordering::Relaxed),
                        Action::Switched {
                            added,
                            symbol,
                            remark,
                        } => {
                            displayed = symbol.clone();
                            // Start at the live end: a book built from a
                            // half-replayed history would be missing the resets
                            // that came before it, and a stale price would sit
                            // above the asks until the next picture arrived.
                            cursor = ring.published();
                            book = Book::new(cfg.cache_depth.max(levels + 1));
                            last_record = Instant::now();
                            saw_any = false;
                            consumed = 0;
                            resyncs = 0;
                            let what = if added {
                                format!("watching {} symbols now; {symbol} selected", watch.len())
                            } else {
                                format!("switched to {symbol}")
                            };
                            status = match remark {
                                Some(text) => format!("{what} — {text}"),
                                None => what,
                            };
                        }
                        Action::Note(text) => status = text,
                    }
                }
            }
        }

        // Is this window still attached to the feed that is running?
        //
        // Two ways to stop being attached, and neither is visible in the bytes
        // the window reads. The daemon truncates the ring's header when it
        // starts a fresh run, so the sequence begins again at zero while this
        // cursor is a number from the run that ended: `consume` cannot tell an
        // old cursor from a caught-up one, reports `Empty` on every frame, and
        // the window sits on the last book of a run that is over — which is
        // what a user sees as a window that never reconnects. And a mapping
        // follows the inode it was made from, so a ring file that is removed
        // and created again leaves the window reading a file nothing writes.
        if last_ring_check.elapsed() >= RING_CHECK {
            last_ring_check = Instant::now();
            let restarted = match reattach(&mut ring, &ring_path, cursor) {
                Attached::Current => false,
                Attached::Restarted => true,
                Attached::Failed(message) => {
                    status = message;
                    false
                }
            };
            if restarted {
                // The whole view belongs to the run that ended, so it all goes:
                // the cursor, the book, and the counts, which would otherwise
                // describe two runs as one. Starting at zero replays whatever
                // the new run has already published, and `consume_batch`
                // resynchronises to the oldest record still in the ring.
                cursor = 0;
                book = Book::new(cfg.cache_depth.max(levels + 1));
                last_record = Instant::now();
                saw_any = false;
                consumed = 0;
                resyncs = 0;
                reattaches += 1;
                status = format!(
                    "the feed restarted; re-attached at seq {}",
                    ring.published()
                );
            }
        }

        // Drain the ring, applying only the displayed symbol's records.
        let n = ring.consume_batch(&mut cursor, &mut out);
        if n > 0 {
            let mut applied = 0;
            for m in &out[..n] {
                if m.symbol_bytes() == displayed.as_bytes() {
                    book.apply(m);
                    applied += 1;
                }
            }
            consumed += n as u64;
            if applied > 0 {
                last_record = Instant::now();
                saw_any = true;
            }
        }

        let suggestions = catalog.suggest(&first_token(&line), SUGGESTIONS);
        draw(
            &mut screen,
            &cfg,
            &book,
            &Frame {
                levels: level_count.load(Ordering::Relaxed),
                live: saw_any && last_record.elapsed() < STALL_AFTER,
                fed: saw_any,
                symbol: &displayed,
                config_path: cfg_path,
                ring_capacity: ring.capacity(),
                lag: ring.lag(cursor),
                backlog_ns: backlog_ns(&ring, cursor),
                staleness_ns: staleness_ns(&ring),
                age: last_record.elapsed(),
                consumed,
                resyncs,
                reattaches,
                watched: watch.len(),
                adapter: read_heartbeat(
                    &heartbeat_path,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                ),
                status: &status,
                suggestions: &suggestions,
                highlighted: highlighted.min(suggestions.len().saturating_sub(1)),
                input: &line,
                interactive,
            },
        )?;
        std::thread::sleep(FRAME);
    }

    let _ = writeln!(
        screen,
        "\nclosed. The feed keeps running until `obctl stop`."
    );
    let _ = screen.flush();
    drop(screen);
    if reader.is_finished() {
        let _ = reader.join();
    }
    Ok(())
}

/// What a submitted line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Leave.
    Quit,
    /// Show this many levels.
    Depth(usize),
    /// Display this symbol, watching it if it was not.
    Switched {
        /// Whether the watch set grew.
        added: bool,
        /// The symbol now displayed.
        symbol: String,
        /// Something worth saying about it, such as it not being in the catalog.
        ///
        /// Part of the action rather than a side channel: an earlier version
        /// attached it with a helper that discarded the text, which is the same
        /// shape of defect as a message nobody reads.
        remark: Option<String>,
    },
    /// Say something and change nothing.
    Note(String),
}

/// Act on a submitted line.
///
/// Unknown commands are ignored with a note, which is the same behaviour the C++
/// terminal has for a typo: it does nothing rather than stopping the viewer.
fn act(text: &str, catalog: &Catalog, watch: &mut Watch, watch_path: &Path) -> Action {
    let line = text.trim();
    if line.is_empty() {
        return Action::Note(String::new());
    }
    if line == "q" || line == "quit" {
        return Action::Quit;
    }
    if let Some(n) = parse_levels(line) {
        return Action::Depth(n);
    }
    if line == "watch" {
        return Action::Note(format!(
            "watching {}: {}",
            watch.len(),
            watch.symbols().join(" ")
        ));
    }
    if let Some(rest) = line.strip_prefix("unwatch ") {
        return match watch.remove(rest) {
            Ok(true) => match watch.save(watch_path) {
                Ok(()) => Action::Note(format!(
                    "unwatched {}; {} left",
                    rest.trim().to_ascii_uppercase(),
                    watch.len()
                )),
                Err(e) => Action::Note(e.message),
            },
            Ok(false) => Action::Note(format!("{} was not being watched", rest.trim())),
            Err(e) => Action::Note(e.message),
        };
    }
    if let Some(rest) = line.strip_prefix("only ") {
        return match watch.only(rest) {
            Ok(()) => match watch.save(watch_path) {
                Ok(()) => Action::Switched {
                    added: false,
                    symbol: watch.current().to_string(),
                    remark: None,
                },
                Err(e) => Action::Note(e.message),
            },
            Err(e) => Action::Note(e.message),
        };
    }
    // `force SYMBOL` is the way past the catalog check below.
    let (symbol, forced) = match line.strip_prefix("force ") {
        Some(rest) => (rest.trim(), true),
        None => (
            match line.strip_prefix("watch ") {
                Some(rest) => rest.trim(),
                None => line,
            },
            false,
        ),
    };
    if !symbol_fits(symbol) {
        return Action::Note(format!(
            "{symbol:?} is not a command and does not fit the 8-byte symbol field"
        ));
    }
    // A symbol the catalog does not list is refused rather than tried, because of
    // what it can do to everything else: all the watched symbols share one
    // websocket, and an unknown stream name in a combined URL can take the whole
    // connection down, so one typo would stop the feed for every other pair. The
    // escape is explicit and says what it risks.
    // The canonical form, so a message and the watch file agree even when the user
    // typed lower case.
    let canonical = symbol.to_ascii_uppercase();
    if !forced && !catalog.contains(&canonical) {
        return Action::Note(format!(
            "{canonical} is not in config/tickers.txt. Add it there, or type \
             `force {canonical}` to try it anyway: an unknown stream name can \
             break the subscription for every symbol."
        ));
    }
    let remark = forced.then(|| format!("{canonical} is not in the catalog; forced anyway"));

    match watch.select(symbol) {
        Ok(added) => match watch.save(watch_path) {
            Ok(()) => Action::Switched {
                added,
                symbol: watch.current().to_string(),
                remark,
            },
            Err(e) => Action::Note(e.message),
        },
        Err(e) => Action::Note(e.message),
    }
}

/// The first whitespace-separated token, which is what the picker matches on.
fn first_token(line: &str) -> String {
    line.split_whitespace().next().unwrap_or("").to_string()
}

/// What a re-attach check found.
#[derive(Debug)]
enum Attached {
    /// The ring at the path is the one this window has, and it is the same run.
    Current,
    /// A new run wrote this ring, or a new file is at the path. Either way the
    /// cursor and the book belong to a run that is over.
    Restarted,
    /// A new file is at the path and it cannot be opened as a ring.
    Failed(String),
}

/// Re-attach the window to the ring at `path`, when it is no longer the one it
/// was reading.
///
/// Split out of the loop so the decision can be read without the drawing around
/// it, and because it is the whole answer to "the feed came back and the window
/// did not": nothing else in this program can notice, since both failures look
/// like a valid ring with nothing new in it.
fn reattach(ring: &mut RingReader, path: &Path, cursor: u64) -> Attached {
    if ring.replaced(path) {
        match RingReader::open(path) {
            Ok(fresh) => *ring = fresh,
            Err(e) => {
                return Attached::Failed(format!(
                    "the ring at {} was replaced and cannot be opened: {e}",
                    path.display()
                ));
            }
        }
        return Attached::Restarted;
    }
    // The writer truncates the header it created, so the sequence going
    // backwards is a new run over the same file.
    if ring.published() < cursor {
        return Attached::Restarted;
    }
    Attached::Current
}

/// Feed time between the oldest record not yet taken and the newest published.
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
fn staleness_ns(ring: &RingReader) -> Option<u64> {
    let newest = ring.published().checked_sub(1)?;
    let latest = ring.timestamp_at(newest)?;
    Some(clock::now_ns().saturating_sub(latest))
}

/// What one frame needs beyond the book.
#[derive(Debug)]
struct Frame<'a> {
    levels: usize,
    live: bool,
    /// Whether a record for the displayed symbol has ever been applied, which is
    /// what separates "nothing yet" from "it stopped".
    fed: bool,
    /// The symbol on screen.
    symbol: &'a str,
    /// The config this window was opened with, so a remedy can name it.
    config_path: &'a str,
    ring_capacity: u32,
    lag: u64,
    backlog_ns: Option<u64>,
    staleness_ns: Option<u64>,
    age: Duration,
    consumed: u64,
    resyncs: u64,
    /// How many times this window re-attached to a feed that restarted.
    reattaches: u64,
    /// How many symbols are being watched.
    watched: usize,
    /// What the adapter's heartbeat says.
    adapter: Liveness,
    /// The last thing that happened, for the user to read.
    status: &'a str,
    /// What the picker is offering.
    suggestions: &'a [&'a str],
    /// Which suggestion is highlighted.
    highlighted: usize,
    input: &'a str,
    interactive: bool,
}

/// Draw one frame.
fn draw(out: &mut impl Write, cfg: &Config, book: &Book, frame: &Frame<'_>) -> CliResult {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";

    let mut bids = vec![Level::EMPTY; frame.levels];
    let mut asks = vec![Level::EMPTY; frame.levels];
    let nb = book.bids.top(frame.levels, &mut bids);
    let na = book.asks.top(frame.levels, &mut asks);
    let feed = if frame.live { "live" } else { "stalled" };

    // Home the cursor and redraw every row, erasing each to the end of its line.
    // The whole screen is not cleared, because the input line is the last row of
    // this frame and clearing the screen under it is what used to erase the user's
    // typing.
    let mut buf = String::with_capacity(3072);
    buf.push_str("\x1b[H");
    buf.push_str(&format!(
        "orderbook-lab (rust)  env={}  {BOLD}{}{RESET}  seq={}  feed={}  watching {}\x1b[K\n",
        cfg.env,
        frame.symbol,
        book.last_seq(),
        feed,
        frame.watched
    ));
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
        "consumed={} records  resyncs={}  reattaches={}  age={}\x1b[K\n",
        frame.consumed,
        frame.resyncs,
        frame.reattaches,
        ob_cli::duration_ns(frame.age.as_nanos().min(u128::from(u64::MAX)) as u64)
    ));
    buf.push_str("Bid prices are red. Ask prices are green.\x1b[K\n");
    buf.push_str("  q | quit        leave this window\x1b[K\n");
    buf.push_str("  levels N        show N price levels, from 1 to 64\x1b[K\n");
    buf.push_str(
        "  SYMBOL          switch to it and watch it. Tab completes; arrows pick from the list\x1b[K\n",
    );
    buf.push_str("  watch           list what is being watched\x1b[K\n");
    buf.push_str("  only SYMBOL     watch just that one\x1b[K\n");
    buf.push_str("  unwatch SYMBOL  stop watching one\x1b[K\n");
    // The three times, each on its own line and each saying what it is for. They
    // are not the same number and confusing them is easy: a backlog is not an age.
    buf.push_str("  lag             records behind you, and the span of feed time that is\x1b[K\n");
    buf.push_str("  staleness       how old the newest published record is\x1b[K\n");
    buf.push_str("  age             how long since this window applied a record\x1b[K\n");
    buf.push_str(&format!(
        "Showing {} levels, watching {} symbols.\x1b[K\n",
        frame.levels, frame.watched
    ));
    if !frame.status.is_empty() {
        buf.push_str(&format!("{}{RESET}\x1b[K\n", frame.status));
    }
    if !frame.live {
        // The adapter's own account of itself, which is what tells apart the two
        // causes that look identical from the ring: an exchange that has gone
        // quiet and a process that has exited. Before this the window had one
        // line for both, and the one it could not name is the one a user cannot
        // fix by waiting. A window stalled because the exchange went quiet and a
        // window whose daemon was stopped from another session were the same
        // frame, and the second is what happened.
        let (adapter, quiet, restartable) = match &frame.adapter {
            Liveness::Live { age_s, note, .. } => (
                format!("The adapter is alive ({note}, {age_s}s ago)"),
                ", so the exchange has gone quiet rather than this host",
                false,
            ),
            Liveness::Stale { pid, age_s, .. } => (
                format!(
                    "The adapter (pid {pid}) last wrote {age_s}s ago and has not \
                     since: it has exited, and this window cannot restart it"
                ),
                "",
                true,
            ),
            Liveness::Absent => (
                "No adapter heartbeat at all, so nothing is feeding this ring".to_string(),
                "",
                true,
            ),
        };
        if !frame.fed {
            // A symbol selected a moment ago has no book yet, and that is a
            // different situation from a feed that stopped: `age` is this
            // window's own, so it starts counting at the attach or the switch
            // and the first two seconds after a switch would otherwise read as a
            // stall. Here the selection is the suspect, not the feed.
            buf.push_str(&format!(
                "Nothing for {} yet. {adapter}. If it never arrives, check that \
                 the adapter is watching it: `watch` lists the set.\x1b[K\n",
                frame.symbol
            ));
        } else {
            let stalled =
                ob_cli::duration_ns(frame.age.as_nanos().min(u128::from(u64::MAX)) as u64);
            buf.push_str(&format!(
                "Feed stalled. {adapter}{quiet}. This window last applied a record \
                 {stalled} ago.\x1b[K\n"
            ));
        }
        if restartable {
            // The remedy has to be a command that starts the feed without
            // closing this window. It used to be `make stop` and then
            // `make rust-start`, and `stop` signals the tui as well: the advice
            // for a window that could not reach its feed was to close the
            // window. Starting the feed is now the whole of it, because the
            // window re-attaches by itself once a writer is at the ring again.
            buf.push_str(&format!(
                "Start the feed again in another window: make rust-feed CONFIG={}\x1b[K\n",
                frame.config_path
            ));
            buf.push_str("This window re-attaches by itself; `make stop` would close it.\x1b[K\n");
        }
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
    // Clear whatever is left below the frame from an earlier, longer one. Before
    // the picker and the prompt are written, so neither is ever erased.
    buf.push_str("\x1b[J");
    // The picker sits between the book and the prompt, which is where a completion
    // list belongs: the prompt stays the last line so the cursor is already where
    // the next character goes.
    if frame.interactive && !frame.suggestions.is_empty() && !frame.input.trim().is_empty() {
        for (i, symbol) in frame.suggestions.iter().enumerate() {
            if i == frame.highlighted {
                buf.push_str(&format!("  {BOLD}> {symbol}{RESET}\x1b[K\n"));
            } else {
                buf.push_str(&format!("    {symbol}\x1b[K\n"));
            }
        }
    }
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
/// non-canonical mode, and the shell would look broken until `stty sane`.
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
/// The process exits and the thread goes with it.
fn spawn_input_thread(input: Arc<Mutex<Input>>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("input".to_string())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut handle = stdin.lock();
            let mut byte = [0u8; 1];
            loop {
                match handle.read(&mut byte) {
                    Ok(0) => {
                        RUNNING.store(false, Ordering::Relaxed);
                        return;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return,
                    Err(_) => {
                        RUNNING.store(false, Ordering::Relaxed);
                        return;
                    }
                }
                let mut guard = match input.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                match byte[0] {
                    0x03 | 0x04 => {
                        RUNNING.store(false, Ordering::Relaxed);
                        return;
                    }
                    b'\r' | b'\n' => {
                        let line = std::mem::take(&mut guard.line);
                        guard.highlighted = 0;
                        guard.pending = Some(Command::Submit(line));
                    }
                    b'\t' => guard.pending = Some(Command::Complete),
                    0x7f | 0x08 => {
                        guard.line.pop();
                        guard.highlighted = 0;
                    }
                    // An escape sequence. The arrow keys are how the picker is
                    // driven, so they are read rather than ignored: ESC, then
                    // `[`, then a letter for the ones that matter.
                    0x1b => {
                        drop(guard);
                        let mut rest = [0u8; 2];
                        if handle.read_exact(&mut rest).is_err() {
                            return;
                        }
                        if rest[0] == b'[' {
                            let delta = match rest[1] {
                                b'A' => -1,
                                b'B' => 1,
                                _ => 0,
                            };
                            if delta != 0 {
                                if let Ok(mut g) = input.lock() {
                                    g.pending = Some(Command::MoveHighlight(delta));
                                }
                            }
                        }
                        continue;
                    }
                    0x20..=0x7e => {
                        if guard.line.len() < INPUT_LIMIT {
                            guard.line.push(byte[0] as char);
                        }
                        guard.highlighted = 0;
                    }
                    _ => {}
                }
            }
        })
        .expect("spawn the input thread")
}

/// Parse a `levels N` command, or return `None`.
///
/// Separate from the input thread so it can be tested without a terminal.
pub fn parse_levels(line: &str) -> Option<usize> {
    let rest = line.trim().strip_prefix("levels ")?;
    let n: usize = rest.trim().parse().ok()?;
    if (LEVELS_MIN..=LEVELS_MAX).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// The depth limits a `levels` command accepts.
pub const LEVELS_MIN: usize = 1;
/// The depth limits a `levels` command accepts.
pub const LEVELS_MAX: usize = 64;

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
        assert_eq!(parse_levels("levels abc"), None);
        assert_eq!(parse_levels("quit"), None);
    }

    #[test]
    fn the_picker_matches_on_the_first_token_only() {
        // `watch ETH` should suggest ETHUSDT, not treat "ETH" as part of a longer
        // query that matches nothing.
        assert_eq!(first_token("eth"), "eth");
        assert_eq!(first_token("watch eth"), "watch");
        assert_eq!(first_token("   et   "), "et");
        assert_eq!(first_token(""), "");
    }

    /// The repository's catalog, found the way any test must find a repo file:
    /// from the crate's own directory, not from the working directory, which is
    /// the crate directory when `cargo test` runs. A relative path here loaded
    /// nothing, so every symbol looked unlisted and three tests failed for a
    /// reason that had nothing to do with the behaviour under test.
    fn repo_catalog() -> Catalog {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf();
        Catalog::load(&root.join("config/tickers.txt")).expect("the shipped catalog must load")
    }

    fn watch_at(path: &std::path::Path) -> (Watch, PathBuf, Catalog) {
        (
            Watch::load(path, "BTCUSDT").expect("watch"),
            path.to_path_buf(),
            repo_catalog(),
        )
    }

    #[test]
    fn a_bare_symbol_switches_and_watches_it() {
        let dir = std::env::temp_dir().join(format!("ob-act-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        let action = act("pepeusdt", &catalog, &mut w, &path);
        match action {
            Action::Switched { added, symbol, .. } => {
                assert!(added, "it was not being watched");
                assert_eq!(symbol, "PEPEUSDT");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT", "PEPEUSDT"]);
        assert_eq!(w.current(), "PEPEUSDT");
        // And the file on disk says so, which is what the adapter reads.
        let written = std::fs::read_to_string(&path).expect("written");
        assert!(written.contains("PEPEUSDT"), "{written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watch_add_subcommand_and_only_both_work() {
        let dir = std::env::temp_dir().join(format!("ob-act2-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        act("watch ethusdt", &catalog, &mut w, &path);
        assert_eq!(w.symbols(), &["BTCUSDT", "ETHUSDT"]);
        assert_eq!(w.current(), "ETHUSDT", "watch switches to what it adds");
        act("only pepeusdt", &catalog, &mut w, &path);
        assert_eq!(w.symbols(), &["PEPEUSDT"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unwatch_removes_and_the_plain_command_lists() {
        let dir = std::env::temp_dir().join(format!("ob-act3-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        act("only pepeusdt", &catalog, &mut w, &path);
        act("watch ethusdt", &catalog, &mut w, &path);
        match act("unwatch ethusdt", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("unwatched ETHUSDT"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["PEPEUSDT"]);
        match act("watch", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("PEPEUSDT"), "{text}"),
            other => panic!("{other:?}"),
        }
        // Removing the last one is refused with a reason.
        match act("unwatch pepeusdt", &catalog, &mut w, &path) {
            Action::Note(text) => assert!(text.contains("only symbol"), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(w.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symbol_that_does_not_fit_is_refused_with_a_reason() {
        let dir = std::env::temp_dir().join(format!("ob-act4-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        match act("1000shibusdt", &catalog, &mut w, &path) {
            Action::Note(text) => {
                assert!(text.contains("does not fit"), "{text}");
                assert!(text.contains("8-byte"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"], "nothing changed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quit_and_levels_are_still_commands() {
        let dir = std::env::temp_dir().join(format!("ob-act5-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert_eq!(act("q", &catalog, &mut w, &path), Action::Quit);
        assert_eq!(act("quit", &catalog, &mut w, &path), Action::Quit);
        assert_eq!(act("levels 7", &catalog, &mut w, &path), Action::Depth(7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symbol_the_catalog_does_not_list_is_refused_with_the_way_past_it() {
        // Refused, not tried: every watched symbol shares one websocket, and an
        // unknown stream name in a combined URL can take the whole connection
        // down. One typo would stop the feed for every other pair.
        let dir = std::env::temp_dir().join(format!("ob-act6-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(
            !catalog.contains("ZZZTUSDT"),
            "the test needs an unlisted symbol"
        );
        match act("zzztusdt", &catalog, &mut w, &path) {
            Action::Note(text) => {
                assert!(text.contains("not in config/tickers.txt"), "{text}");
                assert!(text.contains("force ZZZTUSDT"), "{text}");
                assert!(text.contains("break the subscription"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.symbols(), &["BTCUSDT"], "nothing changed");

        // And the escape hatch works, with a remark saying what was done.
        match act("force zzztusdt", &catalog, &mut w, &path) {
            Action::Switched { symbol, remark, .. } => {
                assert_eq!(symbol, "ZZZTUSDT");
                assert!(
                    remark.as_deref().is_some_and(|r| r.contains("forced")),
                    "{remark:?}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(w.current(), "ZZZTUSDT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_listed_symbol_needs_no_force() {
        let dir = std::env::temp_dir().join(format!("ob-act7-{}", std::process::id()));
        let (mut w, path, catalog) = watch_at(&dir.join("watch.txt"));
        assert!(catalog.contains("ETHUSDT"));
        match act("ethusdt", &catalog, &mut w, &path) {
            Action::Switched { remark, .. } => assert!(remark.is_none(), "{remark:?}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
