//! `ob` — the orderbook-lab library, second implementation, in Rust.
//!
//! # What this is
//!
//! A readable market-data lab. One process accepts normalized ticks over TCP
//! and publishes them to a shared-memory ring, an append-only log, and an
//! in-process ticker cache. Other processes read the ring through a cursor each.
//!
//! It is the sibling of the C++/Python implementation in the repository root.
//! Both are kept, both are tested, and they agree on bytes: the same 72-byte
//! record, the same ring header, the same config schema. `cargo test` asserts
//! that agreement against a fixture the Python adapter produced, and
//! `make rust-interop` feeds one implementation's ring to the other's reader.
//!
//! # Layers, and the only direction an arrow may point
//!
//! ```text
//!   BINARIES   feedd, tui, headless, obsoak, vecbench   (in ob-cli)
//!       |
//!   SERVICES   cache  book  latency  slippage  pressure
//!       |
//!   BUS        ring   wal   flow
//!       |
//!   CORE       msg  codec  fixed  symbol  clock  config  log  error
//!       |
//!   SYS        sys   (libc declarations; the single unsafe boundary)
//! ```
//!
//! A module may use the modules below it and never the ones above. `sys` is
//! used by `ring` and by `clock`, and by nothing else: everything above it works
//! on safe values. That is the whole safety argument for this crate, and it is
//! why there are few `unsafe` blocks to read.
//!
//! # Reading order
//!
//! 1. [`msg`] and [`codec`] — the 72 bytes every other module moves around.
//! 2. [`fixed`] — why prices are integers.
//! 3. [`ring`] and [`wal`] — where the bytes go, and what survives a crash.
//! 4. [`book`] — one symbol's prices.
//! 5. [`cache`] — many symbols, several readers, and the counters that say
//!    whether it is keeping up.
//! 6. [`latency`], [`slippage`], [`pressure`] — the three questions a feed
//!    handler has to answer with numbers rather than adjectives.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]

pub mod book;
pub mod cache;
pub mod clock;
pub mod codec;
pub mod config;
pub mod error;
pub mod fixed;
pub mod flow;
pub mod latency;
pub mod log;
pub mod msg;
pub mod pressure;
pub mod ring;
pub mod slippage;
pub mod symbol;
pub mod sys;
pub mod wal;

pub use error::{Error, Result};
pub use fixed::{Price, Qty, E8};
pub use msg::{Msg, MsgType, Side, MSG_SIZE};
pub use symbol::{SymbolId, SymbolTable};

/// The bytes of one wire record, as they appear on the socket and in the ring.
///
/// Kept as an alias of a fixed-size array rather than a `Vec` so that a reader
/// can prove at compile time that it has a whole record, and so that a batch of
/// records is a contiguous byte span the compiler can treat as one loop.
pub type Wire = [u8; MSG_SIZE];

/// Number of nanoseconds in one second, used by every report formatter.
pub const NS_PER_SEC: u64 = 1_000_000_000;
