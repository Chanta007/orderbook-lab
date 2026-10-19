//! The web frontend's server: one frame streamed to a browser, and commands back.
//!
//! The terminal draws a composed frame and `view` prints one; this serves the same
//! frame to a page over server-sent events, and takes commands over ordinary POSTs.
//! Nothing about what a frame says lives here — the frame arrives as
//! [`ob_view::to_json`] output and the page renders it — which is what makes this a
//! frontend rather than a second implementation.
//!
//! # Why SSE and not a websocket
//!
//! The traffic is one-way (frames out) with occasional commands back, and a command
//! is one HTTP request. SSE reconnects on its own, is plain HTTP through proxies, and
//! costs about 10% more framing bytes on small messages than a websocket — against a
//! handshake, a heartbeat protocol and reconnect logic written by hand.
//! `docs/research/2026-10-19-view-layer-and-web-frontend.md` §4 has the sources.
//!
//! # A slow viewer cannot slow the feed
//!
//! Each connection has a queue of **one** frame: the newest replaces whatever was
//! waiting, and the client counts what it missed. That is the ring's own `DropOldest`
//! policy, for the ring's own reason — for a live view the current frame beats a
//! delayed history — and it is why a background tab or a throttled connection cannot
//! make the frame loop wait. The counter is in the page's status line, so a viewer
//! that is behind says so instead of pretending.
//!
//! The writer thread never blocks the publisher: the publisher stores the frame and
//! notifies; a connection writes whatever is newest when it gets there. A connection
//! that stops reading entirely blocks only its own thread, which gives up after a
//! write timeout and closes.
//!
//! # What is deliberately absent
//!
//! No keep-alive, no chunked encoding, no TLS, no authentication, and no compression.
//! It binds `127.0.0.1` and serves one person's browser on the same host, which is the
//! same posture as the rest of the lab (`docs/design/security.md` §2). Each request is
//! one connection, which costs a handshake per command and keeps the parser small
//! enough to read in one sitting.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The page, served at `/`.
///
/// Inline, with no fetched asset: the lab's Rust side has no dependencies and the page
/// keeps that true in the browser as well. The stylesheet names the same styles the
/// composition uses ([`ob_view::Style::name`]), so a style is decided once and the
/// terminal and the page agree about what a bid price looks like.
pub const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>orderbook-lab</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; background: #101214; color: #d6d9dd;
         font: 13px/1.35 ui-monospace, SFMono-Regular, Menlo, monospace; }
  #screen { margin: 0; padding: 12px 14px 0; white-space: pre; overflow-x: auto; }
  .row { min-height: 1.35em; }
  .plain { color: #d6d9dd; }
  .dim { color: #8b939c; }
  .bold { color: #ffffff; font-weight: 600; }
  .bid { color: #ff6b6b; }
  .ask { color: #51cf66; }
  .good { color: #51cf66; }
  .warn { color: #ffd43b; }
  .bad { color: #ff6b6b; }
  .accent { color: #4dabf7; }
  #bar { display: flex; gap: 12px; align-items: center; padding: 8px 14px;
         border-top: 1px solid #22262b; background: #16191c; position: sticky; bottom: 0; }
  #cmd { flex: 1; background: #101214; color: #d6d9dd; border: 1px solid #2b3036;
         border-radius: 4px; padding: 6px 8px; font: inherit; }
  #cmd:focus { outline: 1px solid #4dabf7; }
  #stats { color: #8b939c; white-space: nowrap; }
  #stats.behind { color: #ffd43b; }
</style>
</head>
<body>
<pre id="screen">connecting…</pre>
<div id="bar">
  <span>&gt;</span>
  <input id="cmd" autocomplete="off" spellcheck="false"
         placeholder="levels 7 · status · book · ETHUSDT · help">
  <span id="stats">…</span>
</div>
<script>
"use strict";
const screen = document.getElementById("screen");
const stats = document.getElementById("stats");
const cmd = document.getElementById("cmd");
let frames = 0, dropped = 0, bytes = 0, first = null;

function row(node) {
  const div = document.createElement("div");
  div.className = "row";
  for (const span of node.spans) {
    const el = document.createElement("span");
    el.className = span.style;
    // textContent, not innerHTML: a symbol arrives from a config file or a
    // websocket, and it must not be able to write markup into this page.
    el.textContent = span.text;
    div.appendChild(el);
  }
  if (node.spans.length === 0) div.appendChild(document.createTextNode(" "));
  return div;
}

function draw(frame) {
  const frag = document.createDocumentFragment();
  for (const line of frame.lines) frag.appendChild(row(line));
  screen.replaceChildren(frag);
}

function show(seq) {
  // The first frame a page sees is not a frame it missed: the server has been
  // publishing since before this browser connected, and counting that gap as a drop
  // would make every viewer report a lag on opening.
  if (first === null) first = seq;
  else if (seq > frames + 1) dropped += seq - frames - 1;
  frames = seq;
  stats.textContent = frames + " frames · " + dropped + " dropped · " +
                      (bytes / 1024).toFixed(1) + " KB";
  stats.className = dropped > 0 ? "behind" : "";
}

const stream = new EventSource("/events");
stream.onmessage = (event) => {
  const payload = JSON.parse(event.data);
  bytes += event.data.length;
  draw(payload.frame);
  show(payload.seq);
};
stream.onerror = () => { stats.textContent = "reconnecting…"; };

async function send(text) {
  const body = "cmd=" + encodeURIComponent(text);
  try {
    await fetch("/act", { method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" }, body });
  } catch (e) { stats.textContent = "command failed: " + e; }
}

cmd.addEventListener("keydown", (event) => {
  if (event.key !== "Enter") return;
  const text = cmd.value.trim();
  cmd.value = "";
  if (text) send(text);
});
cmd.focus();

// `?cmd=status` runs one command as soon as the first frame is on screen, so a view
// can be linked to — and so a browser can be pointed at a command without a keyboard.
const initial = new URLSearchParams(location.search).get("cmd");
if (initial) {
  const once = stream.addEventListener("message", () => {
    stream.removeEventListener("message", once);
    send(initial);
  });
}
</script>
</body>
</html>
"#;

/// How many frames one connection may be behind before it is counted as dropping.
///
/// One: the queue holds the newest frame and nothing else, so a connection that is
/// still writing when the next frame arrives has already missed one.
pub const QUEUE: usize = 1;

/// How long a connection may take to accept one write before it is dropped.
///
/// Two seconds: a viewer on the same host writes in microseconds, and a connection
/// that cannot take a 4 KB frame in two seconds is not going to catch up. Its thread
/// closes the socket and the page reconnects on its own.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// The most connections served at once.
///
/// Bounded so that a misbehaving client cannot make the server allocate threads
/// without limit; past it a connection is refused with 503 rather than queued. The
/// page uses one connection and the commands use short-lived ones, so this is far
/// more than a person needs.
pub const MAX_CLIENTS: usize = 32;

/// How long a connection waits for a new frame before sending a comment.
///
/// Fifteen seconds, which is the interval the research record gives for keeping a
/// proxy or a browser from deciding an idle stream is dead. The comment is not an
/// event, so the page's `onmessage` never sees it.
pub const HEARTBEAT: Duration = Duration::from_secs(15);

/// What every connection and the frame loop share.
#[derive(Debug)]
pub struct App {
    /// The newest composed frame, and the sequence number that says which it is.
    frames: Mutex<Option<(u64, String)>>,
    /// Signalled when a frame arrives.
    arrived: Condvar,
    /// Commands waiting for the frame loop to run them.
    commands: Mutex<Vec<String>>,
    /// Frames published since the server started.
    published: AtomicU64,
    /// Frames a connection was too slow to take, summed over connections.
    dropped: AtomicU64,
    /// Connections being served right now.
    clients: AtomicUsize,
    /// Connections refused because [`MAX_CLIENTS`] were already being served.
    refused: AtomicU64,
    /// When the server started, for the page's status line.
    started: Instant,
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

impl App {
    /// A server with no frame published yet.
    pub fn new() -> Self {
        App {
            frames: Mutex::new(None),
            arrived: Condvar::new(),
            commands: Mutex::new(Vec::new()),
            published: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            clients: AtomicUsize::new(0),
            refused: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    /// Publish a frame, replacing whatever the last one was.
    ///
    /// The replacement is the policy: a connection that has not taken the previous
    /// frame loses it, and counts it. Nothing here can block, which is what keeps a
    /// slow viewer from being a slow feed.
    pub fn publish(&self, json: String) {
        let seq = self.published.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut slot) = self.frames.lock() {
            *slot = Some((seq, json));
        }
        self.arrived.notify_all();
    }

    /// The newest frame, when it is newer than `after`.
    ///
    /// Waits up to `timeout` for one. `None` means nothing new arrived in that time,
    /// which is what a connection turns into a heartbeat comment.
    pub fn wait_for_frame(&self, after: u64, timeout: Duration) -> Option<(u64, String)> {
        let deadline = Instant::now() + timeout;
        let mut slot = self.frames.lock().ok()?;
        loop {
            if let Some((seq, json)) = slot.as_ref() {
                if *seq > after {
                    return Some((*seq, json.clone()));
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let (next, _) = self.arrived.wait_timeout(slot, left).ok()?;
            slot = next;
        }
    }

    /// Queue a command for the frame loop to run.
    pub fn push_command(&self, command: String) {
        if let Ok(mut queue) = self.commands.lock() {
            queue.push(command);
        }
    }

    /// Take the commands queued since the last call.
    pub fn take_commands(&self) -> Vec<String> {
        match self.commands.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => Vec::new(),
        }
    }

    /// Count frames a connection was too slow to take.
    pub fn count_dropped(&self, frames: u64) {
        self.dropped.fetch_add(frames, Ordering::Relaxed);
    }

    /// A line for `/health`: what the server has done since it started.
    pub fn health(&self) -> String {
        format!(
            "ok frames={} dropped={} clients={} refused={} uptime_s={}",
            self.published.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            self.clients.load(Ordering::Relaxed),
            self.refused.load(Ordering::Relaxed),
            self.started.elapsed().as_secs()
        )
    }

    /// Frames published, for a test or a caller that wants the number.
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    /// Frames a slow connection missed, summed.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// A bound listener, ready to serve.
#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    app: Arc<App>,
}

impl Server {
    /// Bind `addr` and serve frames from `app`.
    ///
    /// The address is a `SocketAddr` rather than a host and a port so the caller
    /// decides what it listens on: the binary binds loopback and refuses anything
    /// else, and a test binds port zero and reads back what it got.
    pub fn bind(addr: SocketAddr, app: Arc<App>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        Ok(Server { listener, app })
    }

    /// The address actually bound, which is how a test learns its port.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve until the process ends.
    ///
    /// One thread per connection, capped. A thread that panics takes its own
    /// connection down and nothing else, which is why the handler is a plain function
    /// rather than anything clever.
    pub fn serve(self) -> std::io::Result<()> {
        let app = Arc::clone(&self.app);
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            if app.clients.load(Ordering::Relaxed) >= MAX_CLIENTS {
                app.refused.fetch_add(1, Ordering::Relaxed);
                let _ = refuse(stream);
                continue;
            }
            app.clients.fetch_add(1, Ordering::Relaxed);
            let app = Arc::clone(&app);
            std::thread::spawn(move || {
                handle(stream, &app);
                app.clients.fetch_sub(1, Ordering::Relaxed);
            });
        }
        Ok(())
    }
}

/// Answer one connection, then close it.
fn handle(mut stream: TcpStream, app: &App) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return,
    };
    let route = Route::of(&request);
    match route {
        Route::Page => {
            let _ = respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                PAGE.as_bytes(),
            );
        }
        Route::Health => {
            let _ = respond(
                &mut stream,
                200,
                "text/plain; charset=utf-8",
                app.health().as_bytes(),
            );
        }
        Route::Act => {
            let command = form_field(&request.body, "cmd");
            let body = match command {
                Some(command) if !command.trim().is_empty() => {
                    let before = app.published();
                    app.push_command(command);
                    // Wait for the frame that the command produced, so the reply is
                    // the answer rather than an acknowledgement. One frame at 10 Hz,
                    // with room for a slow one.
                    match app.wait_for_frame(before, Duration::from_millis(500)) {
                        Some((_, json)) => json,
                        None => String::from("{\"lines\":[]}"),
                    }
                }
                _ => String::from("{\"lines\":[]}"),
            };
            let _ = respond(&mut stream, 200, "application/json", body.as_bytes());
        }
        Route::Events => stream_events(stream, app),
        Route::NotFound => {
            let _ = respond(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"not found\n",
            );
        }
    }
}

/// Serve one connection the frame stream, until it goes away.
fn stream_events(mut stream: TcpStream, app: &App) {
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\n\
                Connection: keep-alive\r\n\
                X-Accel-Buffering: no\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();
    let mut sent = app.published();
    loop {
        match app.wait_for_frame(sent, HEARTBEAT) {
            Some((seq, json)) => {
                // What arrived while this connection was writing is what it missed.
                // `sent` is the last sequence it took, so anything between them is a
                // frame it never saw.
                let missed = seq.saturating_sub(sent).saturating_sub(QUEUE as u64);
                if missed > 0 {
                    app.count_dropped(missed);
                }
                if write_event(&mut stream, seq, &json).is_err() {
                    return;
                }
                sent = seq;
            }
            // Nothing for fifteen seconds: say so without an event, which is what
            // keeps a proxy or a browser from closing an idle stream.
            None => {
                if stream.write_all(b": keepalive\n\n").is_err() {
                    return;
                }
            }
        }
        if stream.flush().is_err() {
            return;
        }
    }
}

/// One server-sent event: an id, and the frame as JSON.
fn write_event(stream: &mut TcpStream, seq: u64, json: &str) -> std::io::Result<()> {
    // The frame JSON has no newlines — `to_json` writes one object on one line — so
    // one `data:` line is the whole event. If that ever stops being true, the page
    // would see a truncated frame rather than a parse error, so the assertion is
    // here in the code path that depends on it.
    debug_assert!(!json.contains('\n'), "a frame must be one line for SSE");
    write!(
        stream,
        "id: {seq}\ndata: {{\"seq\":{seq},\"frame\":{json}}}\n\n"
    )
}

/// What a request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `GET /` — the page.
    Page,
    /// `GET /events` — the frame stream.
    Events,
    /// `POST /act` — run a command.
    Act,
    /// `GET /health` — the counters, as text.
    Health,
    /// Anything else.
    NotFound,
}

impl Route {
    /// Route a parsed request.
    pub fn of(request: &Request) -> Route {
        let path = request.path.split('?').next().unwrap_or("");
        match (request.method.as_str(), path) {
            ("GET", "/") | ("GET", "/index.html") => Route::Page,
            ("GET", "/events") => Route::Events,
            ("POST", "/act") => Route::Act,
            ("GET", "/health") => Route::Health,
            _ => Route::NotFound,
        }
    }
}

/// One parsed request: what was asked, and what it carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// `GET`, `POST`, and whatever else a client sends.
    pub method: String,
    /// The path, with any query string still on it.
    pub path: String,
    /// The body, when there was one.
    pub body: String,
}

/// The largest request this server will read.
///
/// Eight kilobytes: a command line is at most 64 bytes and the page asks for nothing
/// but itself. A larger request is refused rather than buffered, because there is no
/// reason for this server to hold an arbitrary amount of a client's bytes.
pub const MAX_REQUEST: usize = 8 * 1024;

/// Read and parse one request.
pub fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = String::new();
    if length > 0 && length <= MAX_REQUEST {
        let mut buf = vec![0u8; length];
        reader.read_exact(&mut buf)?;
        body = String::from_utf8_lossy(&buf).into_owned();
    }
    Ok(Request { method, path, body })
}

/// One field out of a form-encoded body, percent-decoded.
///
/// `cmd=levels+7` and `cmd=levels%207` both mean `levels 7`. Written out rather than
/// pulled in, because this server has no dependencies and the format is four rules.
pub fn form_field(body: &str, key: &str) -> Option<String> {
    for pair in body.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name != key {
            continue;
        }
        let mut out = String::with_capacity(value.len());
        let bytes = value.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => {
                    out.push(' ');
                    i += 1;
                }
                b'%' if i + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                    let byte = u8::from_str_radix(hex, 16).ok()?;
                    out.push(byte as char);
                    i += 3;
                }
                _ => {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
        }
        return Some(out);
    }
    None
}

/// Write a response with a body and close the connection.
fn respond(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// Refuse a connection politely, saying why.
///
/// The request is read first, and thrown away. A socket closed with unread bytes in
/// its receive buffer sends a reset, so a client that was refused would see a
/// connection error instead of the sentence that explains it — and a refusal nobody
/// can read is indistinguishable from a crash.
fn refuse(mut stream: TcpStream) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut scratch = [0u8; 1024];
    let _ = stream.read(&mut scratch);
    respond(
        &mut stream,
        503,
        "text/plain; charset=utf-8",
        b"too many viewers: this server serves 32 at once\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, body: &str) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            body: body.to_string(),
        }
    }

    #[test]
    fn the_routes_are_the_four_the_page_needs_and_a_refusal() {
        assert_eq!(Route::of(&request("GET", "/", "")), Route::Page);
        assert_eq!(Route::of(&request("GET", "/index.html", "")), Route::Page);
        assert_eq!(Route::of(&request("GET", "/events", "")), Route::Events);
        assert_eq!(Route::of(&request("POST", "/act", "cmd=q")), Route::Act);
        assert_eq!(Route::of(&request("GET", "/health", "")), Route::Health);
        // A GET on /act is not a command: a command changes state, and a browser (or
        // a crawler following a link) may issue a GET for anything.
        assert_eq!(Route::of(&request("GET", "/act", "")), Route::NotFound);
        assert_eq!(Route::of(&request("POST", "/events", "")), Route::NotFound);
        assert_eq!(
            Route::of(&request("GET", "/../etc/passwd", "")),
            Route::NotFound
        );
        assert_eq!(
            Route::of(&request("GET", "/events?since=3", "")),
            Route::Events
        );
    }

    #[test]
    fn a_form_field_is_percent_and_plus_decoded() {
        assert_eq!(
            form_field("cmd=levels+7", "cmd").as_deref(),
            Some("levels 7")
        );
        assert_eq!(
            form_field("cmd=levels%207", "cmd").as_deref(),
            Some("levels 7")
        );
        assert_eq!(form_field("cmd=ethusdt", "cmd").as_deref(), Some("ethusdt"));
        assert_eq!(
            form_field("a=1&cmd=status&b=2", "cmd").as_deref(),
            Some("status")
        );
        assert_eq!(form_field("a=1", "cmd"), None);
        assert_eq!(form_field("", "cmd"), None);
    }

    #[test]
    fn a_frame_is_published_replaced_and_counted() {
        let app = App::new();
        assert_eq!(app.published(), 0);
        app.publish("{\"lines\":[]}".to_string());
        app.publish("{\"lines\":[]}".to_string());
        assert_eq!(app.published(), 2);
        // The slot holds the newest frame and nothing else: a second publish before
        // anyone reads is a frame that was replaced.
        let (seq, _) = app
            .wait_for_frame(0, Duration::from_millis(10))
            .expect("a frame");
        assert_eq!(seq, 2, "the newest frame, not the first");
    }

    #[test]
    fn a_wait_with_nothing_new_times_out_rather_than_blocking_for_ever() {
        let app = App::new();
        app.publish("{}".to_string());
        assert!(app.wait_for_frame(0, Duration::from_millis(10)).is_some());
        let started = Instant::now();
        assert!(app.wait_for_frame(1, Duration::from_millis(50)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn a_frame_published_while_a_reader_waits_wakes_it() {
        let app = Arc::new(App::new());
        let waiter = Arc::clone(&app);
        let handle = std::thread::spawn(move || waiter.wait_for_frame(0, Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(20));
        app.publish("{\"lines\":[1]}".to_string());
        let got = handle.join().expect("thread").expect("a frame");
        assert_eq!(got.0, 1);
        assert!(got.1.contains("[1]"));
    }

    #[test]
    fn commands_are_queued_and_taken_once() {
        let app = App::new();
        app.push_command("levels 7".to_string());
        app.push_command("status".to_string());
        let taken = app.take_commands();
        assert_eq!(taken, vec!["levels 7".to_string(), "status".to_string()]);
        assert!(
            app.take_commands().is_empty(),
            "taking them empties the queue"
        );
    }

    #[test]
    fn the_health_line_names_what_the_server_has_done() {
        let app = App::new();
        app.publish("{}".to_string());
        app.count_dropped(3);
        let health = app.health();
        assert!(health.starts_with("ok "), "{health}");
        assert!(health.contains("frames=1"), "{health}");
        assert!(health.contains("dropped=3"), "{health}");
        assert!(health.contains("clients=0"), "{health}");
    }

    #[test]
    fn the_page_carries_the_styles_the_composition_uses() {
        // A style that the page does not know renders as plain text, so a new style
        // would be invisible in the browser and correct in the terminal. This is the
        // check that keeps the two palettes one palette.
        for style in [
            ob_view::Style::Plain,
            ob_view::Style::Dim,
            ob_view::Style::Bold,
            ob_view::Style::Bid,
            ob_view::Style::Ask,
            ob_view::Style::Good,
            ob_view::Style::Warn,
            ob_view::Style::Bad,
            ob_view::Style::Accent,
        ] {
            assert!(
                PAGE.contains(&format!(".{} {{", style.name())),
                "the page has no rule for .{}",
                style.name()
            );
        }
    }

    #[test]
    fn the_page_never_writes_markup_from_a_frame() {
        // A symbol arrives from a config file or a websocket. The page builds nodes
        // and sets textContent, so it cannot be made to write markup.
        assert!(PAGE.contains("textContent"), "the page sets text, not HTML");
        // The word appears once, in the comment that explains why it is not used, so
        // the assertion is about the calls rather than the letters.
        assert!(!PAGE.contains(".innerHTML"), "and never assigns markup");
        assert!(!PAGE.contains("innerHTML ="), "and never assigns markup");
    }

    #[test]
    fn a_frame_that_is_not_one_line_is_refused_rather_than_split() {
        // The event format is one `data:` line per event. A frame with a newline in it
        // would be delivered as two events and the page would see a truncated frame,
        // so the invariant is asserted where it matters.
        let json = ob_view::to_json(&[ob_view::Line::plain("a\nb")]);
        assert!(
            !json.contains('\n'),
            "the renderer escapes newlines: {json}"
        );
    }
}
