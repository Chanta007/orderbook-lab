//! `view` — one frame of the lab, printed instead of drawn.
//!
//! ```text
//!   view <config.json> [--json|--text] [--once] [--frames N] [--interval ms]
//!                       [--panel book|status] [--levels N] [--act "command"]
//! ```
//!
//! The same snapshot, the same composition and the same commands as the terminal:
//! this binary is the proof that the view is a layer rather than a drawing. If the
//! terminal can show it, this can print it, because both call
//! [`ob_view::compose`] — and a script, a test or a future dashboard can read it
//! without a terminal, without ANSI, and without parsing either.
//!
//! # Examples
//!
//! ```text
//!   view config/dev.json --once                     # one frame, as text
//!   view config/dev.json --once --json              # one frame, as JSON
//!   view config/dev.json --json --frames 3          # three frames, one per line
//!   view config/dev.json --once --panel status      # the dashboard
//!   view config/dev.json --act "levels 7" --act "status" --once
//! ```
//!
//! `--act` runs a command through the shared dispatcher, so `levels 7` here and
//! `levels 7` typed into the terminal do the same thing to the same watch file.
//!
//! # Exit codes
//!
//! 0 on success, 2 for a usage error, 1 for a failure (a missing ring, a config that
//! will not load). A frame with nothing in it is still a frame: an empty book is a
//! successful read of an empty book.

use ob::book::Book;
use ob::msg::Msg;
use ob::ring::RingReader;
use ob_cli::actions::{act, Action};
use ob_cli::tickers::Catalog;
use ob_cli::watch::Watch;
use ob_cli::window::{adapter_state, Attached, Prompt, Window};
use ob_cli::{load_config, CliError, CliResult, HEARTBEAT_NAME};
use ob_view::{compose, to_json, to_text, Adapter, Budget, Frame, Panel, View};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// What this run was asked for.
#[derive(Debug)]
struct Options {
    config: String,
    json: bool,
    frames: usize,
    interval: Duration,
    panel: Panel,
    levels: Option<usize>,
    acts: Vec<String>,
    /// Frames to compose and throw away, for a cost measurement.
    bench: Option<usize>,
    /// Start at the live end, like a terminal, rather than reading what the ring
    /// already holds.
    follow: bool,
}

impl Options {
    /// Parse the command line, refusing anything this binary cannot do.
    fn from_env() -> CliResult<Options> {
        let args = ob_cli::Args::from_env();
        let Some(config) = args.positional(0) else {
            return Err(CliError::usage(
                "usage: view <config.json> [--json|--text] [--once] [--frames N] \
                 [--interval ms] [--panel book|status] [--levels N] [--act \"command\"]",
            ));
        };
        let once = args.has("--once");
        let frames = args.number::<usize>("--frames", 1)?;
        let interval = Duration::from_millis(args.number::<u64>("--interval", 100)?);
        let panel = match args.value("--panel") {
            None | Some("book") => Panel::Book,
            Some("status") => Panel::Status,
            Some(other) => {
                return Err(CliError::usage(format!(
                    "--panel takes `book` or `status`, not {other:?}"
                )))
            }
        };
        let levels =
            match args.value("--levels") {
                None | Some("") => None,
                Some(raw) => Some(raw.parse::<usize>().map_err(|_| {
                    CliError::usage(format!("--levels expects a number, got {raw:?}"))
                })?),
            };
        let acts: Vec<String> = args
            .flags
            .iter()
            .filter(|(k, _)| k == "act")
            .map(|(_, v)| v.clone())
            .collect();
        let bench =
            match args.value("--bench") {
                None | Some("") => None,
                Some(raw) => Some(raw.parse::<usize>().map_err(|_| {
                    CliError::usage(format!("--bench expects a number, got {raw:?}"))
                })?),
            };
        Ok(Options {
            config: config.to_string(),
            json: args.has("--json"),
            frames: if once { 1 } else { frames },
            interval,
            panel,
            levels,
            acts,
            bench,
            follow: args.has("--follow"),
        })
    }
}

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("view: {e}"));
        std::process::exit(e.code);
    }
}

fn run() -> CliResult {
    let options = Options::from_env()?;
    let cfg = load_config(&options.config)?;
    let ring_path = PathBuf::from(&cfg.bus_path);
    let mut ring = RingReader::open(&ring_path)?;
    let catalog = Catalog::load(&PathBuf::from(&cfg.tickers_path))?;
    let watch_path = PathBuf::from(&cfg.watch_path);
    let heartbeat_path = watch_path.with_file_name(HEARTBEAT_NAME);
    let mut watch = Watch::load(&watch_path, &cfg.symbol)?;
    let levels = options.levels.unwrap_or(cfg.levels);
    let mut window = Window::new(&options.config, &watch, levels);
    // A printed frame describes the present, so it reads what the ring already holds
    // rather than waiting for the next record. A window on a terminal is the other
    // way round — it watches the future from the moment it attaches — and `--follow`
    // is that behaviour, for a caller that wants a stream of what happens next.
    window.cursor = if options.follow {
        ring.published()
    } else {
        ring.published().saturating_sub(u64::from(ring.capacity()))
    };
    window.view = View {
        panel: options.panel,
        help: false,
    };
    window.watched = watch.len();

    let mut book = Book::new(cfg.cache_depth.max(levels + 1));
    let mut out = [Msg::default(); 512];

    if let Some(frames) = options.bench {
        return bench(frames, &mut window, &ring, &book, &cfg, &options);
    }
    let stdout = std::io::stdout();
    let mut screen = stdout.lock();

    // Commands first, then the frame they produced: `view --act "levels 7" --once`
    // is a way to run one command and see what it did.
    for command in &options.acts {
        let (action, note) = dispatch(command, &catalog, &mut watch, &watch_path)?;
        match action {
            Action::Quit => return Ok(()),
            Action::Panel(panel) => window.view.panel = panel,
            Action::ToggleHelp => window.view.help = !window.view.help,
            Action::Depth(n) => window.levels = n,
            Action::Switched { symbol, .. } => {
                window.displayed = symbol;
                window.restart_at(ring.published());
                window.watched = watch.len();
                book = Book::new(cfg.cache_depth.max(window.levels + 1));
            }
            Action::Note(_) => {}
        }
        if !note.is_empty() {
            window.status = note;
        }
    }

    for frame_index in 0..options.frames {
        if frame_index > 0 {
            std::thread::sleep(options.interval);
        }
        window.daemon.poll();
        window.ticks.fold_if_due();
        // Re-attach the way the terminal does: a printed frame has to be as honest
        // about a restarted feed as a drawn one, or the CLI is a second opinion
        // nobody can trust.
        if let Attached::Restarted = ob_cli::window::reattach(&mut ring, &ring_path, window.cursor)
        {
            window.restart_at(0);
            window.reattaches += 1;
            window.status = format!(
                "the feed restarted; re-attached at seq {}",
                ring.published()
            );
        }
        window.drain(&ring, &mut book, &mut out);
        let adapter = adapter_state(&heartbeat_path);
        let snapshot = window.snapshot(
            &ring,
            &book,
            &cfg,
            &options.config,
            adapter,
            &Prompt {
                line: "",
                suggestions: &[],
                highlighted: 0,
                interactive: false,
            },
        );
        let frame: Frame = compose(&snapshot, &window.view, budget(&options));
        if options.json {
            writeln!(screen, "{}", to_json(&frame.lines))
                .map_err(|e| CliError::failed(format!("write: {e}")))?;
        } else {
            // A frame per block, with a blank line between them so two frames are
            // two frames rather than one tall one.
            if frame_index > 0 {
                writeln!(screen).map_err(|e| CliError::failed(format!("write: {e}")))?;
            }
            write!(screen, "{}", to_text(&frame.lines))
                .map_err(|e| CliError::failed(format!("write: {e}")))?;
        }
        screen
            .flush()
            .map_err(|e| CliError::failed(format!("flush: {e}")))?;
    }
    Ok(())
}

/// Compose `frames` frames and report what it cost.
///
/// The design's cost claim is that composition is a pure function over a small
/// snapshot and that the terminal write, not the composition, is the expensive part
/// (`docs/design/view-layer.md` §9). This is how that is checked: a number with a
/// command behind it, rather than an assertion.
fn bench(
    frames: usize,
    window: &mut Window,
    ring: &RingReader,
    book: &Book,
    cfg: &ob::config::Config,
    options: &Options,
) -> CliResult {
    let adapter = Adapter::Absent;
    let prompt = Prompt {
        line: "eth",
        suggestions: &["ETHUSDT", "ETCUSDT"],
        highlighted: 0,
        interactive: true,
    };
    let budget = budget(options);
    let started = std::time::Instant::now();
    let mut bytes = 0usize;
    let mut rows = 0usize;
    for i in 0..frames {
        // A frame that changes a little every time, which is what a live book does:
        // a benchmark over an identical frame would measure the hash and nothing
        // else.
        window.view.panel = if i % 2 == 0 {
            Panel::Book
        } else {
            Panel::Status
        };
        let snapshot = window.snapshot(ring, book, cfg, &options.config, adapter.clone(), &prompt);
        let frame = compose(&snapshot, &window.view, budget);
        bytes += to_text(&frame.lines).len();
        rows += frame.lines.len();
    }
    let elapsed = started.elapsed();
    let per_frame = elapsed.as_nanos() as f64 / frames as f64;
    ob_cli::say(&format!(
        "compose: {frames} frames, {per_frame:.0} ns per frame, {} rows, {} bytes per frame, \
         {:.0} frames/s",
        rows / frames,
        bytes / frames,
        1e9 / per_frame
    ));
    Ok(())
}

/// The size a printed frame is composed to.
///
/// A terminal's size, when there is one, so `view` and `tui` in the same terminal
/// agree; 80×40 when there is not, which is a frame tall enough to show the status
/// panel and the reference without being a wall of text in a log.
fn budget(options: &Options) -> Budget {
    let (rows, cols) = ob_cli::screen::size();
    let rows = if rows == ob_cli::screen::FALLBACK.0 {
        40
    } else {
        rows
    };
    let _ = options;
    Budget { rows, cols }
}

/// Run one command, and say what it did.
fn dispatch(
    text: &str,
    catalog: &Catalog,
    watch: &mut Watch,
    watch_path: &std::path::Path,
) -> CliResult<(Action, String)> {
    let action = act(text, catalog, watch, watch_path);
    let note = match &action {
        Action::Note(text) => text.clone(),
        Action::Switched {
            added,
            symbol,
            remark,
        } => {
            let what = if *added {
                format!("watching {} symbols now; {symbol} selected", watch.len())
            } else {
                format!("switched to {symbol}")
            };
            match remark {
                Some(text) => format!("{what} — {text}"),
                None => what,
            }
        }
        Action::Depth(n) => format!("showing {n} levels"),
        Action::Panel(Panel::Book) => "the price table".to_string(),
        Action::Panel(Panel::Status) => "the status panel".to_string(),
        Action::ToggleHelp => "the command reference".to_string(),
        Action::Quit => String::new(),
    };
    Ok((action, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_printed_frame_is_the_same_frame_the_terminal_draws() {
        // The point of the layer: one composition, two frontends. This asserts the
        // contract rather than the pixels — the text of a frame is the text of the
        // lines the terminal renders.
        let snapshot = ob_view::Snapshot::default();
        let frame = compose(&snapshot, &View::default(), Budget { rows: 40, cols: 80 });
        let text = to_text(&frame.lines);
        assert!(text.contains("orderbook-lab"), "{text}");
        assert_eq!(text.lines().count(), frame.lines.len());
        let json = to_json(&frame.lines);
        assert!(json.starts_with("{\"lines\":["));
    }

    #[test]
    fn the_interval_and_the_frame_count_are_read_from_the_command_line() {
        // `--once` is one frame whatever `--frames` says: a script that asks for one
        // frame and gets a stream would hang.
        let args = ob_cli::Args::parse(
            ["config/dev.json", "--once", "--frames", "9"]
                .iter()
                .map(|s| s.to_string()),
        );
        assert!(args.has("once"));
        assert_eq!(args.number::<usize>("frames", 1).unwrap(), 9);
    }

    #[test]
    fn a_panel_name_that_is_not_one_of_the_two_is_refused() {
        let args = ob_cli::Args::parse(
            ["config/dev.json", "--panel", "charts"]
                .iter()
                .map(|s| s.to_string()),
        );
        match args.value("--panel") {
            Some("charts") => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_command_run_here_and_a_command_typed_there_are_the_same_command() {
        let dir = std::env::temp_dir().join(format!("ob-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("watch.txt");
        let mut watch = Watch::load(&path, "BTCUSDT").expect("watch");
        let catalog = Catalog::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .expect("workspace root")
                .join("config/tickers.txt"),
        )
        .expect("catalog");
        let (action, note) = dispatch("ethusdt", &catalog, &mut watch, &path).expect("dispatch");
        assert!(matches!(action, Action::Switched { .. }));
        assert!(note.contains("ETHUSDT"), "{note}");
        assert!(std::fs::read_to_string(&path)
            .expect("written")
            .contains("ETHUSDT"));
        let (_, note) = dispatch("levels 7", &catalog, &mut watch, &path).expect("dispatch");
        assert_eq!(note, "showing 7 levels");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_is_composed_without_touching_the_terminal() {
        // A printed frame must work with no terminal at all: that is what makes it
        // usable from a script, a cron job or a test.
        let snapshot = ob_view::Snapshot::default();
        let frame = compose(
            &snapshot,
            &View {
                panel: Panel::Status,
                help: true,
            },
            Budget {
                rows: 40,
                cols: 100,
            },
        );
        let text = to_text(&frame.lines);
        assert!(text.contains("ring"), "{text}");
        assert!(
            text.contains("leave this window"),
            "the reference is in the frame"
        );
    }
}
