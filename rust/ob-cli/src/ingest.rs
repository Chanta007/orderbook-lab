//! Reading whole records from TCP connections, and the one platform difference
//! that made this module a separate file.
//!
//! # The bug this module exists to prevent
//!
//! The accept loop polls, so the listening socket is non-blocking. On Linux, a
//! socket returned by `accept` is **always blocking** regardless of the
//! listener's flag. On macOS and the other BSD-derived systems, the accepted
//! socket **inherits** the listener's flag.
//!
//! A reader that assumes either behaviour is wrong on the other platform. Worse,
//! the wrong assumption is silent: the first `read` returns whatever had already
//! arrived, and the second returns `WouldBlock`, which a loop that treats every
//! error as "the peer hung up" turns into a closed connection. The symptom is a
//! feed that delivers exactly one buffer of data and then disconnects, which
//! looks like the *sender's* problem.
//!
//! So: the accepted stream's mode is set explicitly, and `WouldBlock` is handled
//! as "nothing yet" rather than as a closed socket. Both are cheap and both are
//! needed.
//!
//! # Why the framing is not a scan
//!
//! Records are a fixed 72 bytes with no delimiter, so there is nothing to search
//! for. The reader accumulates bytes and decodes when it has a whole record,
//! carrying a partial record over to the next read. A stream that sends a
//! partial record and stops simply blocks, which is the correct behaviour: the
//! alternative is to invent a resynchronisation rule for a protocol that has no
//! framing to resynchronise to.

use ob::codec;
use ob::msg::{Msg, MSG_SIZE};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

/// Counters shared between the connection threads and the report.
#[derive(Debug, Default)]
pub struct Counters {
    /// Records handed to the publisher.
    pub accepted: AtomicU64,
    /// Bytes read from sockets.
    pub bytes: AtomicU64,
    /// Records whose header or body failed validation.
    pub invalid: AtomicU64,
    /// Connections accepted.
    pub connections: AtomicU64,
}

/// How many records one decode pass handles before the buffer is reused.
const DECODE_BATCH: usize = 64;

/// How long to wait before retrying a read that found nothing.
const RETRY: Duration = Duration::from_micros(200);

/// Accept connections until `running` goes false.
///
/// The listener must already be bound and non-blocking.
///
/// `running` is a reference to the caller's own static rather than a
/// module-level one. A binary's signal handler cannot capture anything, so it
/// has to write to a `static` it owns; taking it as a parameter is what lets the
/// tests below use one flag each. A shared static would make one test's shutdown
/// stop another test mid-connection, which is exactly how these tests failed the
/// first time they ran.
pub fn accept_loop(
    listener: TcpListener,
    tx: SyncSender<Msg>,
    counters: Arc<Counters>,
    running: &'static AtomicBool,
) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                counters.connections.fetch_add(1, Ordering::Relaxed);
                let tx = tx.clone();
                let counters = Arc::clone(&counters);
                if std::thread::Builder::new()
                    .name("connection".to_string())
                    .spawn(move || serve(stream, tx, counters, running))
                    .is_err()
                {
                    // Out of threads. Dropping the connection makes the sender
                    // reconnect, which is what it already does on any error.
                    eprintln!("feedd: cannot spawn a connection thread");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => {
                eprintln!("feedd: accept: {e}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Read whole records from one connection and hand them to the publisher.
///
/// Returns when the peer closes, when the publisher is gone, or when the run
/// flag goes false. A send on a full channel blocks, which is the ingest path's
/// backpressure: the kernel's TCP window closes and the sender slows down.
pub fn serve(
    mut stream: TcpStream,
    tx: SyncSender<Msg>,
    counters: Arc<Counters>,
    running: &'static AtomicBool,
) {
    // The platform difference described in the module note, handled explicitly.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_nodelay(true);

    let mut buf = vec![0u8; 64 * 1024];
    let mut pending: Vec<u8> = Vec::with_capacity(MSG_SIZE * 2);
    let mut out = [Msg::default(); DECODE_BATCH];

    while running.load(Ordering::Relaxed) {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // Nothing to read yet. Not a closed socket, and treating it as one
            // is the bug this module is named for.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(RETRY);
                continue;
            }
            Err(_) => break,
        };
        counters.bytes.fetch_add(n as u64, Ordering::Relaxed);
        pending.extend_from_slice(&buf[..n]);

        // `stream_records` rather than `valid_prefix_len`: this buffer came
        // from one socket read, so a hole in the middle is impossible and the
        // count is constant time. The decoder still validates every record.
        let whole = codec::stream_records(&pending);
        if whole > 0 {
            let body = &pending[..whole * MSG_SIZE];
            for chunk in body.chunks(MSG_SIZE * DECODE_BATCH) {
                let stats = codec::decode_batch(chunk, &mut out);
                counters
                    .invalid
                    .fetch_add(stats.skipped as u64, Ordering::Relaxed);
                for m in &out[..stats.decoded] {
                    if tx.send(*m).is_err() {
                        return;
                    }
                    counters.accepted.fetch_add(1, Ordering::Relaxed);
                }
            }
            pending.drain(..whole * MSG_SIZE);
        }
        // A record is 72 bytes and the buffer is 64 KiB, so this can only hold a
        // partial record. The guard is for a future change to the framing.
        if pending.len() > 1024 * 1024 {
            eprintln!("feedd: dropping an oversized partial record");
            pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ob::fixed::{Price, Qty, E8};
    use ob::msg::Side;
    use std::io::Write;
    use std::net::SocketAddr;
    use std::sync::mpsc::sync_channel;

    /// Bind a listener on an ephemeral port and return it with its address.
    fn listener() -> (TcpListener, SocketAddr) {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        (l, addr)
    }

    fn record(i: i64) -> Msg {
        let mut m = Msg::depth(
            Side::Bid,
            Price::from_e8(65_000 * E8 + i),
            Qty::from_e8(E8),
            "BTCUSDT",
        );
        m.seq = i as u64;
        m
    }

    #[test]
    fn a_connection_survives_the_first_read() {
        // The regression this module exists for: a listener that polls (so it is
        // non-blocking) hands out a connection on which a second read must still
        // block and wait rather than look like a hang-up.
        let (l, addr) = listener();
        l.set_nonblocking(true).expect("non-blocking listener");
        let (tx, rx) = sync_channel::<Msg>(1024);
        let counters = Arc::new(Counters::default());
        static RUNNING: AtomicBool = AtomicBool::new(true);
        RUNNING.store(true, Ordering::Relaxed);

        let acceptor = std::thread::spawn(move || accept_loop(l, tx, counters, &RUNNING));

        let mut client = TcpStream::connect(addr).expect("connect");
        // First burst: eight records.
        for i in 0..8 {
            client.write_all(&record(i).encode()).expect("write 1");
        }
        // Give the reader time to consume the first burst and go back to
        // blocking, which is where the inherited-non-blocking bug showed up.
        std::thread::sleep(Duration::from_millis(50));
        // Second burst, after the first read has already returned.
        for i in 8..16 {
            client
                .write_all(&record(i).encode())
                .expect("write 2 must not fail: the reader must still be there");
        }
        drop(client);

        let mut got = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while got.len() < 16 && std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(m) => got.push(m),
                Err(_) => break,
            }
        }
        RUNNING.store(false, Ordering::Relaxed);
        let _ = acceptor.join();

        assert_eq!(got.len(), 16, "both bursts must arrive on one connection");
        for (i, m) in got.iter().enumerate() {
            assert_eq!(m.seq, i as u64);
        }
    }

    #[test]
    fn a_partial_record_is_held_until_the_rest_arrives() {
        let (l, addr) = listener();
        l.set_nonblocking(true).expect("non-blocking listener");
        let (tx, rx) = sync_channel::<Msg>(1024);
        let counters = Arc::new(Counters::default());
        static RUNNING: AtomicBool = AtomicBool::new(true);
        RUNNING.store(true, Ordering::Relaxed);
        let acceptor = std::thread::spawn(move || accept_loop(l, tx, counters, &RUNNING));

        let mut client = TcpStream::connect(addr).expect("connect");
        let wire = record(1).encode();
        // Half a record, then the other half. TCP has no reason to keep these
        // together, and neither does the reader.
        client.write_all(&wire[..30]).expect("write head");
        std::thread::sleep(Duration::from_millis(20));
        assert!(rx.try_recv().is_err(), "a partial record is not a record");
        client.write_all(&wire[30..]).expect("write tail");
        drop(client);

        let m = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the completed record arrives");
        assert_eq!(m.seq, 1);
        RUNNING.store(false, Ordering::Relaxed);
        let _ = acceptor.join();
    }

    #[test]
    fn two_connections_are_served_independently() {
        let (l, addr) = listener();
        l.set_nonblocking(true).expect("non-blocking listener");
        let (tx, rx) = sync_channel::<Msg>(1024);
        let counters = Arc::new(Counters::default());
        static RUNNING: AtomicBool = AtomicBool::new(true);
        RUNNING.store(true, Ordering::Relaxed);
        let acceptor = std::thread::spawn(move || accept_loop(l, tx, counters, &RUNNING));

        let mut a = TcpStream::connect(addr).expect("connect a");
        let mut b = TcpStream::connect(addr).expect("connect b");
        a.write_all(&record(100).encode()).expect("write a");
        b.write_all(&record(200).encode()).expect("write b");
        std::thread::sleep(Duration::from_millis(50));
        // Both connections are still open, so both can write again.
        a.write_all(&record(101).encode()).expect("write a2");
        b.write_all(&record(201).encode()).expect("write b2");
        drop(a);
        drop(b);

        let mut seqs = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seqs.len() < 4 && std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(m) => seqs.push(m.seq),
                Err(_) => break,
            }
        }
        RUNNING.store(false, Ordering::Relaxed);
        let _ = acceptor.join();

        seqs.sort_unstable();
        assert_eq!(seqs, vec![100, 101, 200, 201]);
    }

    #[test]
    fn a_record_with_a_bad_header_is_counted_and_skipped() {
        let (l, addr) = listener();
        l.set_nonblocking(true).expect("non-blocking listener");
        let (tx, rx) = sync_channel::<Msg>(1024);
        let counters = Arc::new(Counters::default());
        let seen = Arc::clone(&counters);
        static RUNNING: AtomicBool = AtomicBool::new(true);
        RUNNING.store(true, Ordering::Relaxed);
        let acceptor = std::thread::spawn(move || accept_loop(l, tx, counters, &RUNNING));

        let mut client = TcpStream::connect(addr).expect("connect");
        let mut bad = record(7).encode();
        bad[ob::codec::OFF_KIND] = 99;
        client.write_all(&bad).expect("write bad");
        client.write_all(&record(8).encode()).expect("write good");
        drop(client);

        let m = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the good record arrives");
        assert_eq!(m.seq, 8);
        RUNNING.store(false, Ordering::Relaxed);
        let _ = acceptor.join();
        assert_eq!(seen.invalid.load(Ordering::Relaxed), 1);
    }
}
