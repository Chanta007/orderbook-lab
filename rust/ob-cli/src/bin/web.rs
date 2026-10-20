//! `web` — the same frame, served to a browser.
//!
//! ```text
//!   web <config.json> [--port 9002] [--panel book|status] [--levels N]
//!                      [--interval ms] [--act "command"]
//! ```
//!
//! The third frontend over one composition: the terminal draws the frame, `view`
//! prints it, and this serves it to a page over server-sent events and takes commands
//! back over POSTs. `docs/design/view-layer.md` §8.3 is the design;
//! `rust/ob-cli/src/web.rs` is the server, and its module note argues the two
//! decisions that matter — SSE rather than a websocket, and a queue of one frame per
//! viewer so that a slow browser cannot slow the feed.
//!
//! # Bound to loopback, with no authentication
//!
//! The address is `127.0.0.1` and cannot be changed, for the same reason the rest of
//! the lab is loopback-only: this serves a person's own browser on their own machine.
//! `docs/design/security.md` §2.
//!
//! # Ready line
//!
//! It prints `web listening on http://127.0.0.1:<port>` to stderr when the socket is
//! bound, which is what a test — or `obctl`, or a person — waits for. The line comes
//! after the bind, so a printed line means the port answers.

use ob::book::Book;
use ob::msg::Msg;
use ob::ring::RingReader;
use ob_cli::actions::{act, Action};
use ob_cli::tickers::Catalog;
use ob_cli::watch::Watch;
use ob_cli::web::{App, Server};
use ob_cli::window::{adapter_state, Attached, Prompt, Window};
use ob_cli::{load_config, CliError, CliResult, HEARTBEAT_NAME};
use ob_view::{compose, to_json, Budget, Panel, View};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The port served when `--port` is not given.
///
/// 9002, which is `config/dev.json`'s 9001 plus one and not derived from it: a derived
/// default is a second place for a number to be decided, and two ports that move
/// together are two ports that collide together. `--port` is how a second window, or a
/// test, gets its own.
pub const DEFAULT_PORT: u16 = 9002;

/// How often a frame is composed and published.
pub const FRAME: Duration = Duration::from_millis(100);

fn main() {
    if let Err(e) = run() {
        ob_cli::note(&format!("web: {e}"));
        std::process::exit(e.code);
    }
}

fn run() -> CliResult {
    let args = ob_cli::Args::from_env();
    let Some(cfg_path) = args.positional(0) else {
        return Err(CliError::usage(
            "usage: web <config.json> [--port 9002] [--panel book|status] [--levels N] \
             [--interval ms] [--act \"command\"]",
        ));
    };
    let port = args.number::<u16>("--port", DEFAULT_PORT)?;
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

    let cfg = load_config(cfg_path)?;
    let ring_path = PathBuf::from(&cfg.bus_path);
    let mut ring = RingReader::open(&ring_path)?;
    let catalog = Catalog::load(&PathBuf::from(&cfg.tickers_path))?;
    let watch_path = PathBuf::from(&cfg.watch_path);
    let heartbeat_path = watch_path.with_file_name(HEARTBEAT_NAME);
    let mut watch = Watch::load(&watch_path, &cfg.symbol)?;
    let levels = args.number::<usize>("--levels", cfg.levels)?;
    // The book's depth, kept here rather than read back out of the book: a switch
    // rebuilds it, and a structure sized from a number should be sized from the number.
    let depth = cfg.cache_depth.max(levels + 1);
    let mut window = Window::new(cfg_path, &watch, levels);
    window.view = View { panel, help: false };
    // A page opened at an arbitrary moment should show the book as it is, so the
    // server reads what the ring already holds before it follows. `--follow` starts
    // at the live end instead, for a viewer that only cares about what happens next.
    let follow = args.has("--follow");
    window.cursor = if follow {
        ring.published()
    } else {
        ring.published().saturating_sub(u64::from(ring.capacity()))
    };
    window.watched = watch.len();
    let mut book = Book::new(depth);
    let mut out = [Msg::default(); 512];

    // A command on the command line, run before the first frame: `web --act status`
    // opens the page on the dashboard.
    for command in args
        .flags
        .iter()
        .filter(|(k, _)| k == "act")
        .map(|(_, v)| v.clone())
    {
        apply(
            &command,
            &catalog,
            &mut watch,
            &watch_path,
            &mut window,
            &ring,
            &mut book,
            depth,
        );
    }

    // The symbols the completion may offer come from the catalog the daemon watches,
    // read once: a completion that re-read the file per keystroke would be a
    // completion with a file read in it.
    let app = Arc::new(App::new().with_symbols(catalog.symbols().to_vec()));
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let server = Server::bind(addr, Arc::clone(&app)).map_err(|e| {
        CliError::failed(format!(
            "cannot bind {addr}: {e}. Another web frontend may be running; \
             pass --port to serve a second one."
        ))
    })?;
    let bound = server
        .local_addr()
        .map_err(|e| CliError::failed(format!("cannot read the bound address: {e}")))?;
    std::thread::spawn(move || {
        if let Err(e) = server.serve() {
            ob_cli::note(&format!("web: the listener stopped: {e}"));
        }
    });
    // The ready line comes after the bind, so a caller that waits for it is waiting
    // for something that answers.
    ob_cli::note(&format!("web listening on http://{bound}"));

    let mut last_ring_check = Instant::now();
    let mut tick = Instant::now();
    loop {
        for command in app.take_commands() {
            apply(
                &command,
                &catalog,
                &mut watch,
                &watch_path,
                &mut window,
                &ring,
                &mut book,
                depth,
            );
        }
        if last_ring_check.elapsed() >= Duration::from_millis(250) {
            last_ring_check = Instant::now();
            if let Attached::Restarted =
                ob_cli::window::reattach(&mut ring, &ring_path, window.cursor)
            {
                window.restart_at(0);
                window.reattaches += 1;
                window.status = format!(
                    "the feed restarted; re-attached at seq {}",
                    ring.published()
                );
            }
        }
        window.daemon.poll();
        window.ticks.fold_if_due();
        window.drain(&ring, &mut book, &mut out);
        let adapter = adapter_state(&heartbeat_path);
        let snapshot = window.snapshot(
            &ring,
            &book,
            &cfg,
            cfg_path,
            adapter,
            &Prompt {
                line: "",
                suggestions: &[],
                highlighted: 0,
                // The page has an input bar of its own; the frame needs no prompt.
                input_mode: ob_view::InputMode::Remote,
            },
        );
        // The page is not a terminal: it gets the whole frame, at the height a page
        // can scroll, rather than the rows a terminal happens to have.
        let frame = compose(
            &snapshot,
            &window.view,
            Budget {
                rows: 48,
                cols: 110,
            },
        );
        app.publish(to_json(&frame.lines));

        let elapsed = tick.elapsed();
        if elapsed < interval {
            std::thread::sleep(interval - elapsed);
        }
        tick = Instant::now();
    }
}

/// Run one command from a viewer, through the same dispatcher a keystroke uses.
///
/// A command that switches symbol or depth has to reach the structures sized from it,
/// which is why this is the library's dispatcher and not a second implementation of
/// "what does `levels 7` mean".
#[allow(clippy::too_many_arguments)]
fn apply(
    command: &str,
    catalog: &Catalog,
    watch: &mut Watch,
    watch_path: &std::path::Path,
    window: &mut Window,
    ring: &RingReader,
    book: &mut Book,
    depth: usize,
) {
    match act(command, catalog, watch, watch_path) {
        Action::Quit => {
            window.status =
                "`quit` closes a terminal, not a server: stop this process to end it".to_string();
        }
        Action::Depth(n) => {
            window.levels = n;
            window.status = format!("showing {n} levels");
        }
        Action::Panel(panel) => {
            window.view.panel = panel;
            window.status = match panel {
                Panel::Book => "the price table".to_string(),
                Panel::Status => {
                    "status: the feed, the queues, the consumers and the ticks".to_string()
                }
            };
        }
        Action::Explain(topic) => {
            window.status = topic.render(&window.times(ring));
        }
        Action::ToggleHelp => {
            window.view.help = !window.view.help;
            window.status = if window.view.help {
                "the command reference is open below the prompt".to_string()
            } else {
                "the command reference is hidden".to_string()
            };
        }
        Action::Switched {
            added,
            symbol,
            remark,
        } => {
            window.displayed = symbol.clone();
            window.restart_at(ring.published());
            window.watched = watch.len();
            *book = Book::new(depth.max(window.levels + 1));
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
    }
}
