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
//! best match, the arrow keys move the highlight, `Enter` takes the highlighted
//! one, and `Escape` clears the line. Ranking lives in [`ob_cli::tickers`]: a
//! prefix beats an interior match, and the file's order breaks ties.
//!
//! A name that is not in the catalog is resolved rather than refused outright
//! ([`resolve`]): when exactly one listed symbol starts with it, that symbol is
//! used and the frame says so, because the picker was already showing it. A name
//! that matches nothing is still refused, and `force` is still the explicit way
//! past that.
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

use ob::book::Book;
use ob::ring::RingReader;
use ob_cli::actions::{act, Action, SUGGESTIONS};
use ob_cli::screen;
use ob_cli::tickers::Catalog;
use ob_cli::watch::Watch;
use ob_cli::window::{adapter_state, reattach, Attached, Prompt, Window};
use ob_cli::{load_config, CliError, CliResult, HEARTBEAT_NAME};
use ob_view::{compose, Budget, FrameCache};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often a frame is drawn.
const FRAME: Duration = Duration::from_millis(100);

/// How often the window checks that the ring it mapped is still the ring at the
/// path, and that the writer has not started a new run.
///
/// A stat is cheap enough for every frame, and the check is not: it decides
/// whether the window is attached to the feed that is running now. A quarter of
/// a second is the longest a person would call immediate.
const RING_CHECK: Duration = Duration::from_millis(250);

/// Bytes the input thread may hold, so a paste cannot grow it without bound.
const INPUT_LIMIT: usize = 64;

/// How long to wait for the rest of an escape sequence after an `Escape` byte.
///
/// A terminal writes the three bytes of an arrow key back to back, so the rest of
/// a sequence is either already queued or is not coming. This wait is the only
/// thing that separates a bare `Escape` from `Escape [ A`, and 30 ms is the
/// middle of the range every other terminal reader uses — ncurses ships 1000 ms
/// (and every application sets it to 25), mpv waits 100 ms, elvish 10 ms. Longer
/// than this and a bare `Escape` feels broken; shorter and a sequence split by a
/// slow link is misread as two keys.
const ESCAPE_WAIT_MS: i32 = 30;

/// How many bytes of an escape sequence are read before the rest is left alone.
///
/// `Escape [ A` is three bytes including the `Escape`; four covers a parameter
/// byte as well, and no sequence this program acts on is longer.
const ESCAPE_TAIL_BYTES: usize = 4;

/// Where a polled read lives, for the wait above.
const POLLIN: i16 = 0x0001;

/// One `struct pollfd`, identical on Darwin and Linux.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[cfg(target_os = "macos")]
extern "C" {
    /// `nfds_t` is `unsigned int` on Darwin and `unsigned long` on glibc, which is
    /// the one platform difference in this call. Passing a 32-bit count where a
    /// 64-bit one is read works only because the upper half happens to be zero in
    /// a register the callee ignores; the signature is spelled per platform so it
    /// is stated rather than relied on. Same class of trap as `MS_SYNC`
    /// (`docs/optimization/ledger.jsonl` OPT-023).
    fn poll(fds: *mut PollFd, nfds: u32, timeout: i32) -> i32;
}

#[cfg(not(target_os = "macos"))]
extern "C" {
    /// See the Darwin declaration above for why this is not one signature.
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}

/// Whether stdin has a byte ready, waiting up to `ms`.
///
/// `poll` rather than a blocking read, so the wait for the rest of an escape
/// sequence can be a wait rather than a demand.
fn input_waiting(ms: i32) -> bool {
    let mut fd = PollFd {
        fd: 0,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: one initialised `PollFd`, and the count says one. The call writes
    // only `revents` in that structure.
    let ready = unsafe { poll(&mut fd, 1, ms) };
    ready > 0 && fd.revents & POLLIN != 0
}

/// What a byte of `Escape` turned out to introduce.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Escape {
    /// Nothing followed within the wait: the key itself.
    Bare,
    /// An up or down arrow, which the picker uses.
    Arrow(isize),
    /// A sequence this program does not use, consumed whole.
    ///
    /// Consumed rather than typed: the bytes of a cursor-movement sequence are
    /// printable, and putting `[C` on the input line because the right arrow was
    /// pressed would be worse than ignoring the key.
    Sequence,
    /// Text that happened to arrive during the wait, so nothing is lost.
    Text(Vec<u8>),
}

/// Decide what an escape sequence is, from the bytes after the `Escape`.
///
/// Pure, so the table of cases can be read and tested without a terminal.
fn classify_escape(tail: &[u8]) -> Escape {
    match tail {
        [] => Escape::Bare,
        // CSI: `Escape [`, optional parameters, then a final byte. `A` and `B` are
        // the arrows; the final byte decides even when parameters came first, so
        // `Escape [ 1 ; 5 A` is Ctrl+Up and moves the highlight like Up. Anything
        // else is a key this program does not act on.
        [b'[', rest @ ..] => match rest.last() {
            Some(b'A') => Escape::Arrow(-1),
            Some(b'B') => Escape::Arrow(1),
            _ => Escape::Sequence,
        },
        // SS3, which is how a terminal in application cursor mode sends the same
        // keys. Treating it as text would put `OA` on the line.
        [b'O', rest @ ..] => match rest.first() {
            Some(b'A') => Escape::Arrow(-1),
            Some(b'B') => Escape::Arrow(1),
            _ => Escape::Sequence,
        },
        // Anything else is what the person typed next, arriving quickly — `Escape`
        // followed straight away by `e`. Losing it would be the defect this
        // function exists to prevent.
        tail => Escape::Text(
            tail.iter()
                .copied()
                .filter(|b| (0x20..=0x7e).contains(b))
                .collect(),
        ),
    }
}

/// A byte source for the input thread: reads, and says whether more is waiting.
///
/// The wait is the whole difficulty here — the bytes of one key arrive together
/// and the bytes of the next do not — so the source is what answers for it. A
/// terminal can be asked (`poll`); a test cannot, and a test that had to fake a
/// terminal would not exercise the loop that had the bug.
trait Keys: Read {
    /// Whether another byte is available within `ms`.
    ///
    /// The default suits a source whose bytes are all present: a `read` at the end
    /// returns zero, which ends the loop, so a wrong "yes" costs one call and can
    /// never spin.
    fn waiting(&mut self, ms: i32) -> bool {
        let _ = ms;
        true
    }
}

/// Read whatever follows an `Escape` byte, waiting a moment for the rest.
///
/// This used to be a `read_exact` of two bytes, which is the mistake that made a
/// window look dead: a terminal in non-canonical mode sends a bare `Escape` as one
/// byte, so the call blocked until the next two keystrokes arrived and then
/// consumed them whatever they were. Both were discarded, and an `Enter` among
/// them was never seen as one. A person who backed out of a refusal with `Escape`
/// and typed `ethusdt` watched `et` disappear; repeating it, `husdt` disappeared.
/// Reported as "after typing a symbol that does not exist I cannot change the
/// ticker any more" — which is what a reader that eats every other pair of
/// keystrokes looks like from the outside.
fn read_escape<H: Keys>(handle: &mut H) -> Escape {
    let mut tail = Vec::with_capacity(ESCAPE_TAIL_BYTES);
    while tail.len() < ESCAPE_TAIL_BYTES {
        if !handle.waiting(ESCAPE_WAIT_MS) {
            break;
        }
        let mut byte = [0u8; 1];
        match handle.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => tail.push(byte[0]),
        }
        // `Escape [` or `Escape O` plus one byte is a complete sequence for every
        // key this program looks at, so there is nothing more to wait for.
        if matches!(tail.as_slice(), [b'[', _] | [b'O', _]) {
            break;
        }
    }
    classify_escape(&tail)
}

/// Stdin, read one syscall at a time.
///
/// Not `std::io::stdin()`: that is buffered, and its first read takes everything
/// the terminal has queued — so the `[A` of an arrow key would be sitting in the
/// buffer, where [`input_waiting`] cannot see it, and the wait for the rest of the
/// sequence would always expire. Reading the descriptor directly keeps one queue,
/// which is the one being polled.
#[derive(Debug)]
struct RawStdin;

impl Read for RawStdin {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::os::fd::FromRawFd;
        // SAFETY: `from_raw_fd` takes ownership of the descriptor, so the file is
        // held in `ManuallyDrop`: fd 0 belongs to the process and must outlive this
        // reader. Nothing else here reads stdin.
        let mut file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
        file.read(buf)
    }
}

impl Keys for RawStdin {
    fn waiting(&mut self, ms: i32) -> bool {
        input_waiting(ms)
    }
}

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
    pending: Vec<Command>,
}

/// One key, as an event for the frame loop to apply.
///
/// The thread that reads keystrokes does not touch the book, the watch file or the
/// line: it queues an event and the loop applies it. One thread owns the state, which
/// is why none of it needs a lock beyond the small one around this struct.
///
/// The reader queues these and the loop applies them in order, rather than each
/// thread writing the line. Two writers of one string cannot preserve an order that
/// matters: `sta`, Tab and Enter in one burst arrived as a line the reader had already
/// finished editing plus two actions, so the completion was applied to the wrong text
/// or dropped entirely. As events, the sequence is exactly what was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// A character to append.
    Type(char),
    /// Remove the last character.
    Backspace,
    /// Forget the line.
    ClearLine,
    /// Submit what is in the box now.
    Submit,
    /// Replace the line with the completion of what is on it.
    Complete,
    /// Move the highlight by this much, wrapping.
    MoveHighlight(isize),
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage(
            "usage: tui <config.json> [--levels N] [--stats] [--no-diff]",
        ));
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
    let opening = match watch.save(&watch_path) {
        Ok(()) => format!("watching {}: {}", watch.len(), watch.symbols().join(" ")),
        Err(e) => format!("cannot write {}: {}", watch_path.display(), e.message),
    };

    let mut book = Book::new(cfg.cache_depth.max(levels + 1));
    let level_count = Arc::new(AtomicUsize::new(levels.clamp(1, 64)));
    let input = Arc::new(Mutex::new(Input::default()));

    let interactive = stdin_is_tty();
    let input_mode = if interactive {
        ob_view::InputMode::Local
    } else {
        // A frame with no terminal has no input line, and says so rather than
        // leaving a bare prompt.
        ob_view::InputMode::None
    };
    let _mode = TerminalMode::enter();
    install_signal_handlers()?;
    let reader = spawn_input_thread(Arc::clone(&input));

    let mut out = [ob::msg::Msg::default(); 512];
    let mut last_ring_check = Instant::now();
    let mut cache = FrameCache::new();
    let show_stats = args.has("--stats");
    // A diagnostic: rewrite every row every frame, which is what the renderer did
    // before the frame cache existed. It is here so the claim the cache makes can be
    // measured against its absence on the same terminal, rather than argued.
    let no_diff = args.has("--no-diff");
    let mut stats_at = Instant::now();
    // The window owns the counters, the tick table and the daemon log; the loop owns
    // the ring, the book and the input line.
    let mut window = Window::new(cfg_path, &watch, levels);
    window.cursor = ring.published();
    window.status = opening;

    let stdout = std::io::stdout();
    let mut screen = stdout.lock();
    while RUNNING.load(Ordering::Relaxed) {
        // Take whatever the input thread has asked for.
        let (mut line, mut highlighted, pending) = {
            let mut guard = input.lock().map_err(|_| {
                CliError::failed("the input lock was poisoned by a panic in the reader")
            })?;
            let pending = std::mem::take(&mut guard.pending);
            (guard.line.clone(), guard.highlighted, pending)
        };
        // A queue, applied in order. A burst of bytes — a paste, or Tab and Enter
        // pressed faster than a frame — used to keep only its last action, so a
        // completion was silently dropped by the submission behind it.
        //
        // `quit` is a flag rather than a `break` inside an arm: an arm is now inside
        // this `for`, so a `break` there would leave the frame loop running and the
        // window would never close.
        let mut quitting = false;
        for command in pending {
            match command {
                Command::Type(c) => {
                    if line.chars().count() < INPUT_LIMIT {
                        line.push(c);
                        highlighted = 0;
                    }
                }
                Command::Backspace => {
                    line.pop();
                    highlighted = 0;
                }
                Command::ClearLine => {
                    line.clear();
                    highlighted = 0;
                }
                Command::MoveHighlight(delta) => {
                    let suggestions = ob_view::complete(&line, catalog.symbols(), SUGGESTIONS);
                    if !suggestions.is_empty() {
                        let n = suggestions.len() as isize;
                        let next = (highlighted as isize + delta).rem_euclid(n);
                        highlighted = next as usize;
                        if let Ok(mut guard) = input.lock() {
                            guard.highlighted = highlighted;
                        }
                    }
                }
                Command::Complete => {
                    // Tab inserts the highlighted entry rather than the best guess:
                    // a prefix with several matches is only reachable if the arrows
                    // and Tab agree about which one is selected. A shell does the
                    // same, and so does the page's dropdown.
                    let suggestions = ob_view::complete(&line, catalog.symbols(), SUGGESTIONS);
                    if let Some(best) = suggestions
                        .get(highlighted)
                        .or_else(|| suggestions.first())
                        .cloned()
                    {
                        line = best.insert.clone();
                        highlighted = 0;
                        if let Ok(mut guard) = input.lock() {
                            guard.line = best.insert;
                            guard.highlighted = 0;
                        }
                    }
                }
                Command::Submit => {
                    // What is in the box now, which is after any completion above, and
                    // then the box is emptied.
                    let text = line.clone();
                    line.clear();
                    highlighted = 0;
                    if let Ok(mut guard) = input.lock() {
                        guard.line.clear();
                        guard.highlighted = 0;
                    }
                    match act(&text, &catalog, &mut watch, &watch_path) {
                        Action::Quit => {
                            quitting = true;
                            break;
                        }
                        Action::Explain(topic) => {
                            window.status = topic.render(&window.times(&ring));
                        }
                        Action::Depth(n) => {
                            window.levels = n;
                            level_count.store(n, Ordering::Relaxed);
                        }
                        Action::Switched {
                            added,
                            symbol,
                            remark,
                        } => {
                            window.displayed = symbol.clone();
                            // Start at the live end: a book built from a
                            // half-replayed history would be missing the resets
                            // that came before it, and a stale price would sit
                            // above the asks until the next picture arrived.
                            window.restart_at(ring.published());
                            book = Book::new(cfg.cache_depth.max(levels + 1));
                            window.watched = watch.len();
                            let what = if added {
                                format!("watching {} symbols now; {symbol} selected", watch.len())
                            } else {
                                format!("switched to {symbol}")
                            };
                            window.status = match remark {
                                Some(text) => format!("{what} — {text}"),
                                None => what,
                            };
                        }
                        Action::Note(text) => window.status = text,
                        Action::Panel(panel) => {
                            window.view.panel = panel;
                            window.status = match panel {
                                ob_view::Panel::Book => "the price table".to_string(),
                                ob_view::Panel::Status => {
                                    "status: the feed, the queues, the consumers and the ticks"
                                        .to_string()
                                }
                            };
                        }
                        Action::ToggleHelp => {
                            window.view.help = !window.view.help;
                            window.status = if window.view.help {
                                "the command reference is open below the prompt; `help` again hides it"
                                    .to_string()
                            } else {
                                "the command reference is hidden; `help` shows it".to_string()
                            };
                        }
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
            let restarted = match reattach(&mut ring, &ring_path, window.cursor) {
                Attached::Current => false,
                Attached::Restarted => true,
                Attached::Failed(message) => {
                    window.status = message;
                    false
                }
            };
            if restarted {
                // The whole view belongs to the run that ended, so it all goes:
                // the cursor, the book, and the counts, which would otherwise
                // describe two runs as one. Starting at zero replays whatever
                // the new run has already published, and `consume_batch`
                // resynchronises to the oldest record still in the ring.
                window.restart_at(0);
                book = Book::new(cfg.cache_depth.max(levels + 1));
                window.reattaches += 1;
                window.status = format!(
                    "the feed restarted; re-attached at seq {}",
                    ring.published()
                );
            }
        }
        if quitting {
            break;
        }

        // Drain the ring, applying only the displayed symbol's records.
        window.drain(&ring, &mut book, &mut out);

        // Write the line back: the reader queued events rather than editing it, so
        // this is the one place the box is updated, and a completion and the keys
        // around it land in the order they were typed.
        if let Ok(mut guard) = input.lock() {
            if guard.line != line {
                guard.line = line.clone();
            }
            guard.highlighted = highlighted;
        }
        window.daemon.poll();
        window.ticks.fold_if_due();
        let adapter = adapter_state(&heartbeat_path);
        // The list the frame shows, from the line as it is *now* — after any
        // completion in this frame's burst of keystrokes.
        // One frame: gather what was observed, compose it into lines, and write the
        // lines that changed. The composition is pure, so what the frame says is
        // decided here and drawn by `ob-view`, which is what lets the same frame be
        // a terminal, a page or a JSON document.
        let suggestions = ob_view::complete(&line, catalog.symbols(), SUGGESTIONS);
        let snapshot = window.snapshot(
            &ring,
            &book,
            &cfg,
            cfg_path,
            adapter,
            &Prompt {
                line: &line,
                suggestions: &suggestions,
                highlighted,
                input_mode,
            },
        );
        let (rows, cols) = screen::size();
        let frame = compose(&snapshot, &window.view, Budget { rows, cols });
        if no_diff {
            cache = FrameCache::new();
        }
        let (bytes, stats) = cache.render(&frame.lines, rows, cols, frame.cursor);
        screen
            .write_all(bytes.as_bytes())
            .map_err(|e| CliError::failed(format!("draw: {e}")))?;
        screen
            .flush()
            .map_err(|e| CliError::failed(format!("flush: {e}")))?;
        if show_stats {
            report_stats(&mut stats_at, stats, &frame, bytes.len());
        }
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

/// Print what the write path did, once a second, when `--stats` is given.
///
/// The claim the frame cache makes is that a mostly-static frame costs a few writes
/// rather than a whole screen. This is how that claim is checked on a real terminal
/// rather than asserted: `docs/design/view-layer.md` §7.
fn report_stats(
    last: &mut Instant,
    stats: ob_view::RenderStats,
    frame: &ob_view::Frame,
    bytes: usize,
) {
    if last.elapsed() < Duration::from_secs(1) {
        return;
    }
    *last = Instant::now();
    let rows = frame.lines.len();
    ob_cli::note(&format!(
        "frame: {rows} rows, {} written, {} skipped, {} erased, {bytes} bytes{}",
        stats.written,
        stats.skipped,
        stats.erased,
        if stats.cleared { ", cleared" } else { "" }
    ));
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
            let mut handle = RawStdin;
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
                    0x20..=0x7e => {
                        if guard.line.len() < INPUT_LIMIT {
                            guard.pending.push(Command::Type(byte[0] as char));
                        }
                    }
                    0x7f | 0x08 => guard.pending.push(Command::Backspace),
                    b'\r' | b'\n' => guard.pending.push(Command::Submit),
                    b'\t' => guard.pending.push(Command::Complete),
                    // The escape key reads more bytes, so the lock is released
                    // first: the draw loop must not wait on this thread. Every
                    // other control byte is ignored, which is what it was before.
                    _ => {
                        drop(guard);
                        if byte[0] != 0x1b {
                            continue;
                        }
                        match read_escape(&mut handle) {
                            // `Escape` on its own means "forget this line". The
                            // refusal for an unlisted symbol used to be the end of
                            // the road for anyone who backed out with `Escape`, and
                            // the key did nothing at all; now it is the way out of
                            // what has been typed, and the help block says so.
                            Escape::Bare => {
                                if let Ok(mut g) = input.lock() {
                                    g.pending.push(Command::ClearLine);
                                }
                            }
                            Escape::Arrow(delta) => {
                                if let Ok(mut g) = input.lock() {
                                    g.pending.push(Command::MoveHighlight(delta));
                                }
                            }
                            Escape::Sequence => {}
                            Escape::Text(bytes) => {
                                if let Ok(mut g) = input.lock() {
                                    for b in bytes {
                                        if g.line.len() + g.pending.len() < INPUT_LIMIT {
                                            g.pending.push(Command::Type(b as char));
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
                }
            }
        })
        .expect("spawn the input thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte source for the reader's tests: the bytes are all there, and the end
    /// of them is what ends the wait.
    #[derive(Debug)]
    struct FakeKeys(std::io::Cursor<Vec<u8>>);

    impl Read for FakeKeys {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Keys for FakeKeys {}

    #[test]
    fn what_follows_an_escape_byte_decides_what_the_key_was() {
        // The table the reader consults, without a terminal. The first two rows
        // are the whole point: nothing after the byte is the key itself, and
        // anything that is not a sequence is text that must not be dropped.
        assert_eq!(classify_escape(&[]), Escape::Bare);
        assert_eq!(classify_escape(b"e"), Escape::Text(vec![b'e']));
        assert_eq!(
            classify_escape(b"ethusdt"),
            Escape::Text(b"ethusdt".to_vec())
        );
        // The arrows, in both the CSI and the application-cursor form.
        assert_eq!(classify_escape(b"[A"), Escape::Arrow(-1));
        assert_eq!(classify_escape(b"[B"), Escape::Arrow(1));
        assert_eq!(classify_escape(b"OA"), Escape::Arrow(-1));
        assert_eq!(classify_escape(b"OB"), Escape::Arrow(1));
        // Sequences this program does not use are consumed, not typed: `[C` is
        // the right arrow, and its bytes are printable.
        assert_eq!(classify_escape(b"[C"), Escape::Sequence);
        // Parameters before the final byte: `Escape [ 1 ; 5 A` is Ctrl+Up, and a
        // modified arrow is still that arrow.
        assert_eq!(classify_escape(b"[1;5A"), Escape::Arrow(-1));
        assert_eq!(classify_escape(b"[1;5C"), Escape::Sequence);
        assert_eq!(classify_escape(b"["), Escape::Sequence);
        assert_eq!(classify_escape(b"O"), Escape::Sequence);
        // A control byte that arrives inside the wait is not text.
        assert_eq!(classify_escape(&[0x0d]), Escape::Text(Vec::new()));
    }

    #[test]
    fn the_reader_keeps_every_byte_it_reads() {
        // What the fixed reader does with a byte stream, and the assertion that
        // covers the reported defect: bytes after a bare `Escape` come back as
        // text instead of being discarded. The old code read two bytes with
        // `read_exact` and threw them away whatever they were, which ate `et` of
        // `ethusdt` — and, when Enter was among them, ate the submission too.
        fn keys(bytes: &[u8]) -> FakeKeys {
            FakeKeys(std::io::Cursor::new(bytes.to_vec()))
        }

        // One byte: a person typing again inside the wait. Nothing may be lost.
        assert_eq!(read_escape(&mut keys(b"e")), Escape::Text(vec![b'e']));
        assert_eq!(
            read_escape(&mut keys(b"ethusdt\r")),
            Escape::Text(b"ethu".to_vec()),
            "the wait takes at most {} bytes, and keeps every one of them",
            ESCAPE_TAIL_BYTES
        );

        // An arrow key, read whole.
        assert_eq!(read_escape(&mut keys(b"[B")), Escape::Arrow(1));
        assert_eq!(read_escape(&mut keys(b"[A")), Escape::Arrow(-1));

        // Nothing to read at all. The wait expires instead of blocking, which is
        // what makes a bare `Escape` reach the draw loop as a key.
        let mut empty = keys(b"");
        assert_eq!(read_escape(&mut empty), Escape::Bare);
    }
}
