//! The 72-byte record that crosses every boundary in this lab.
//!
//! # One record, two implementations, three languages
//!
//! The Python adapter packs this record. The C++ `feedd` reads it, writes it to
//! the ring and the log. The Rust `feedd` does the same. Either implementation's
//! reader can read either implementation's ring, because the layout is fixed
//! here and asserted below.
//!
//! ```text
//!   offset  size  field       meaning
//!   ------  ----  ----------  ----------------------------------------------
//!        0     4  magic       0x4F424C42, "OBLB". A reader rejects anything else
//!        4     2  nbytes      always 72. Guards against a short read
//!        6     1  kind        Depth, Heartbeat, Cmd, DepthReset
//!        7     1  side        Bid, Ask, Both
//!        8     8  seq         filled by the ring at publish time, not by the sender
//!       16     8  ts_ns       filled by the feed daemon when the record is read
//!       24    16  trace_id    W3C width. First 8 bytes double as the sender stamp
//!       40     8  span_id     W3C width. Locally generated counter
//!       48     8  px_e8       price, scaled by 100 million, as a signed integer
//!       56     8  qty_e8      size, same scale. Zero on a Depth means delete
//!       64     8  symbol      eight ASCII bytes, zero padded. Not a C string
//! ```
//!
//! # `trace_id[0..8]`: the sender's stamp, and why the daemon leaves it alone
//!
//! The C++ daemon **overwrites** `trace_id[0..8]` with its own clock reading, so
//! a record arriving from the Python adapter always carries the daemon's time and
//! never the sender's. That loses the one measurement worth having: how long the
//! record spent between the sender writing it and the daemon reading it.
//!
//! This daemon does not overwrite it. The field means:
//!
//! - **non-zero** — the sender's monotonic stamp, written by whoever produced
//!   the record. [`Msg::stamp_ns`] returns it, and the daemon reports the
//!   difference between it and its own read time as stage 1.
//! - **zero** — the sender did not stamp it. Stage 1 is not measurable for this
//!   record and is reported as unmeasured rather than as a zero-latency sample.
//!
//! The daemon writes its own correlation counter into `trace_id[8..16]` and
//! `span_id` instead, so a record still carries a locally generated correlation
//! id. Nothing in either implementation reads `trace_id` as anything but an
//! opaque id, so a record written by one daemon is valid to the other's reader.
//!
//! An earlier version of this file filled `trace_id[0..8]` with the daemon's
//! clock when it was empty, which made `stamp_ns` unable to distinguish "the
//! sender stamped this" from "the daemon did". The end-to-end run reported every
//! record as stamped with a stage-1 latency of zero, which is a measurement that
//! looks like data and is not. See [`Msg::fill_correlation`].
//!
//! # Why this is not `#[repr(packed)]`
//!
//! It could be, and then a slice of records could be cast straight to bytes.
//! It would also mean every read of a `u64` field is an unaligned load that
//! Rust refuses to reference, so the struct would be unusable in ordinary code.
//! Instead the layout lives in [`crate::codec`], as explicit little-endian
//! `from_le_bytes` and `to_le_bytes` calls, and the offsets above are asserted
//! against it by tests. That keeps the byte work in one readable place, and it
//! gives stage 2's batch decoder something to vectorize.

use crate::codec;
use crate::fixed::{Price, Qty};

/// Size of one record on the wire, in bytes.
pub const MSG_SIZE: usize = 72;

/// The first four bytes of every valid record: ASCII `OBLB`.
pub const MAGIC: u32 = 0x4F42_4C42;

/// Bytes in the W3C trace-id field.
pub const TRACE_ID_LEN: usize = 16;

/// Bytes in the W3C span-id field.
pub const SPAN_ID_LEN: usize = 8;

/// Bytes reserved for the symbol, including the terminator.
pub const SYMBOL_LEN: usize = 8;

/// Whether a symbol can be carried in the record's eight bytes.
///
/// The check a caller makes before setting a symbol, because the field cannot
/// hold more and the alternative is losing the tail: a truncated symbol is a
/// different symbol, and a reader would show one pair's prices under another's
/// name.
pub fn symbol_fits(name: &str) -> bool {
    !name.is_empty() && name.len() <= SYMBOL_LEN && name.is_ascii()
}

/// What a record means.
///
/// `Heartbeat` and `Cmd` are reserved: the book ignores both. They exist so a
/// later feature can put control traffic on the same bus without a second
/// framing protocol, and so a reader that does not know a code can still
/// recognise that it is not a book update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MsgType {
    /// Update one price on one side. A zero size deletes that price.
    Depth = 1,
    /// Liveness only. Carries no book change.
    Heartbeat = 2,
    /// Reserved for control traffic. Ignored by the book.
    Cmd = 3,
    /// Clear one whole side before the prices that follow it.
    DepthReset = 4,
}

impl MsgType {
    /// Decode the wire byte. Unknown codes are rejected rather than guessed.
    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(MsgType::Depth),
            2 => Some(MsgType::Heartbeat),
            3 => Some(MsgType::Cmd),
            4 => Some(MsgType::DepthReset),
            _ => None,
        }
    }

    /// The wire byte.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Which side of the book a record applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Side {
    /// Bids, ordered best (highest) first.
    Bid = 0,
    /// Asks, ordered best (lowest) first.
    Ask = 1,
    /// Both sides. Used by a control record, never by a depth update.
    Both = 2,
}

impl Side {
    /// Decode the wire byte. Unknown codes are rejected.
    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Side::Bid),
            1 => Some(Side::Ask),
            2 => Some(Side::Both),
            _ => None,
        }
    }

    /// The wire byte.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// One record, decoded.
///
/// Plainly laid out in memory — see the module note on `#[repr(packed)]`. Use
/// [`crate::codec`] to move it to and from bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Msg {
    /// Always [`MAGIC`] for a valid record.
    pub magic: u32,
    /// Always [`MSG_SIZE`] for a valid record.
    pub nbytes: u16,
    /// What the record means.
    pub kind: MsgType,
    /// Which side it applies to.
    pub side: Side,
    /// Ring-assigned publish sequence. Zero until the ring publishes it.
    pub seq: u64,
    /// Feed-daemon stamp, taken when the bytes were read from the socket.
    pub ts_ns: u64,
    /// W3C-width correlation id. The first eight bytes may carry a sender stamp.
    pub trace_id: [u8; TRACE_ID_LEN],
    /// W3C-width span id. Locally generated.
    pub span_id: [u8; SPAN_ID_LEN],
    /// Price, scaled by 100 million.
    pub px_e8: i64,
    /// Size, scaled by 100 million. Zero deletes the price on a `Depth`.
    pub qty_e8: i64,
    /// Eight ASCII bytes, zero padded.
    pub symbol: [u8; SYMBOL_LEN],
}

impl Default for Msg {
    /// A zeroed record with the magic and length already correct, so that a
    /// caller who forgets to set them still produces something a reader can
    /// recognise as this lab's.
    fn default() -> Self {
        Msg {
            magic: MAGIC,
            nbytes: MSG_SIZE as u16,
            kind: MsgType::Heartbeat,
            side: Side::Both,
            seq: 0,
            ts_ns: 0,
            trace_id: [0; TRACE_ID_LEN],
            span_id: [0; SPAN_ID_LEN],
            px_e8: 0,
            qty_e8: 0,
            symbol: [0; SYMBOL_LEN],
        }
    }
}

impl Msg {
    /// A depth update for one price on one side.
    pub fn depth(side: Side, px: Price, qty: Qty, symbol: &str) -> Self {
        let mut m = Msg {
            kind: MsgType::Depth,
            side,
            ..Msg::default()
        };
        m.px_e8 = px.as_e8();
        m.qty_e8 = qty.as_e8();
        m.set_symbol(symbol);
        m
    }

    /// A whole-side clear.
    pub fn reset(side: Side, symbol: &str) -> Self {
        let mut m = Msg {
            kind: MsgType::DepthReset,
            side,
            ..Msg::default()
        };
        m.set_symbol(symbol);
        m
    }

    /// Price as a typed value.
    #[inline]
    pub const fn price(&self) -> Price {
        Price::from_e8(self.px_e8)
    }

    /// Size as a typed value.
    #[inline]
    pub const fn qty(&self) -> Qty {
        Qty::from_e8(self.qty_e8)
    }

    /// Whether this record satisfies the two field checks every reader makes.
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.magic == MAGIC && self.nbytes as usize == MSG_SIZE
    }

    /// Whether this record changes the book.
    ///
    /// `Heartbeat` and `Cmd` travel on the same bus and are ignored by every
    /// consumer; naming the test keeps that decision in one place.
    #[inline]
    pub const fn affects_book(&self) -> bool {
        matches!(self.kind, MsgType::Depth | MsgType::DepthReset)
    }

    /// Copy `name` into the symbol field, using all eight bytes.
    ///
    /// # Why not seven plus a terminator
    ///
    /// The field is fixed-width eight bytes and is **not** a C string. An earlier
    /// version truncated to seven to keep a terminator, which mangled every
    /// eight-character symbol: `PEPEUSDT` became `PEPEUSD`, `SHIBUSDT` became
    /// `SHIBUSD`, and a reader could not tell it apart from a genuinely
    /// seven-character name. A large share of the market's pairs are eight
    /// characters, so the terminator was costing real symbols to buy nothing:
    /// [`Msg::symbol_bytes`] stops at the first zero byte, which is the right rule
    /// for a zero-padded field and needs no terminator to be present.
    ///
    /// A name longer than eight characters cannot be carried. Use
    /// [`symbol_fits`] to check rather than losing the tail silently.
    pub fn set_symbol(&mut self, name: &str) {
        self.symbol = [0; SYMBOL_LEN];
        let bytes = name.as_bytes();
        let take = bytes.len().min(SYMBOL_LEN);
        self.symbol[..take].copy_from_slice(&bytes[..take]);
    }

    /// The symbol as a byte slice, without the zero padding.
    #[inline]
    pub fn symbol_bytes(&self) -> &[u8] {
        let end = self
            .symbol
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(SYMBOL_LEN);
        &self.symbol[..end]
    }

    /// Set the symbol from eight wire bytes, exactly as given.
    ///
    /// For a caller that already holds the wire form — the soak generator, the
    /// ring's own echo path — and would otherwise have to convert to `&str` and
    /// back. No truncation and no padding happens here: the caller is
    /// responsible for a name that fits, which is the same contract the wire
    /// format has.
    pub fn set_symbol_bytes(&mut self, bytes: &[u8; SYMBOL_LEN]) {
        self.symbol = *bytes;
    }

    /// The symbol as text, lossily. Symbols are ASCII on every feed this lab
    /// reads; a replacement character here means the sender sent something that
    /// is not, which is worth seeing rather than hiding.
    pub fn symbol_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.symbol_bytes())
    }

    /// The sender's stamp, if the sender wrote one.
    ///
    /// The field is the first eight bytes of `trace_id`, little-endian, the same
    /// order as the wire. Zero means the sender did not stamp the record, which
    /// is the case for every sender that predates this extension.
    #[inline]
    pub fn stamp_ns(&self) -> Option<u64> {
        let raw = u64::from_le_bytes([
            self.trace_id[0],
            self.trace_id[1],
            self.trace_id[2],
            self.trace_id[3],
            self.trace_id[4],
            self.trace_id[5],
            self.trace_id[6],
            self.trace_id[7],
        ]);
        if raw == 0 {
            None
        } else {
            Some(raw)
        }
    }

    /// Write a sender stamp into `trace_id[0..8]`.
    ///
    /// The adapter calls this; the daemon calls [`Msg::fill_correlation`].
    pub fn set_stamp_ns(&mut self, ns: u64) {
        self.trace_id[..8].copy_from_slice(&ns.to_le_bytes());
    }

    /// Fill the daemon's correlation counter, and leave the sender's stamp
    /// alone.
    ///
    /// `trace_id[8..16]` and `span_id` are the daemon's; `trace_id[0..8]` is the
    /// sender's and is never written here. See the module note for why the two
    /// are separated.
    pub fn fill_correlation(&mut self, counter: u64) {
        if self.trace_id[8..] == [0; TRACE_ID_LEN - 8] {
            self.trace_id[8..].copy_from_slice(&counter.to_le_bytes());
        }
        if self.span_id == [0; SPAN_ID_LEN] {
            self.span_id.copy_from_slice(&counter.to_le_bytes());
        }
    }

    /// Decode from a 72-byte span.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Wire`] when the span is the wrong length, the
    /// magic is wrong, the declared length is wrong, or a type or side code is
    /// not one this implementation knows.
    pub fn decode(bytes: &[u8]) -> crate::Result<Self> {
        codec::decode(bytes)
    }

    /// Encode into a 72-byte span.
    pub fn encode(&self) -> crate::Wire {
        codec::encode(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_uses_all_eight_bytes_and_is_zero_padded() {
        let mut m = Msg::default();
        m.set_symbol("BTCUSDT");
        assert_eq!(&m.symbol, b"BTCUSDT\0");
        assert_eq!(m.symbol_str(), "BTCUSDT");

        // The eight-character case, which truncating to seven used to mangle.
        m.set_symbol("PEPEUSDT");
        assert_eq!(&m.symbol, b"PEPEUSDT");
        assert_eq!(m.symbol_str(), "PEPEUSDT");

        // Longer than the field cannot be carried, and the tail is lost.
        m.set_symbol("AVERYLONGSYMBOL");
        assert_eq!(&m.symbol, b"AVERYLON");
        assert!(!symbol_fits("AVERYLONGSYMBOL"));
    }

    #[test]
    fn the_fit_check_matches_what_the_field_holds() {
        assert!(symbol_fits("BTCUSDT"));
        assert!(symbol_fits("PEPEUSDT"));
        assert!(symbol_fits("DOGEUSDT"));
        assert!(!symbol_fits("1000SHIBUSDT"));
        assert!(!symbol_fits(""));
        assert!(!symbol_fits("BTC€"));
    }

    #[test]
    fn an_unstamped_record_reads_as_unstamped() {
        let m = Msg::default();
        assert_eq!(m.stamp_ns(), None);
    }

    #[test]
    fn fill_correlation_keeps_a_sender_stamp() {
        let mut sender = Msg::default();
        sender.set_stamp_ns(1_234_567);
        let sender_half = sender.trace_id[..8].to_vec();

        sender.fill_correlation(99);
        // The sender's half is untouched; the daemon's half is now the counter.
        assert_eq!(sender.trace_id[..8], sender_half[..]);
        assert_eq!(sender.stamp_ns(), Some(1_234_567));
        assert_eq!(&sender.trace_id[8..], &99u64.to_le_bytes());
    }

    #[test]
    fn fill_correlation_leaves_an_unstamped_record_unstamped() {
        // This is the regression the end-to-end run found: a daemon that filled
        // the field with its own clock made every record look stamped, and stage
        // 1 report a zero latency that was not a measurement.
        let mut m = Msg::default();
        m.fill_correlation(99);
        assert_eq!(m.stamp_ns(), None, "the sender did not stamp this record");
        assert_eq!(&m.trace_id[8..], &99u64.to_le_bytes());
        assert_eq!(&m.span_id, &99u64.to_le_bytes());
    }

    #[test]
    fn type_and_side_codes_round_trip_and_reject_unknown() {
        for t in [
            MsgType::Depth,
            MsgType::Heartbeat,
            MsgType::Cmd,
            MsgType::DepthReset,
        ] {
            assert_eq!(MsgType::from_u8(t.as_u8()), Some(t));
        }
        assert_eq!(MsgType::from_u8(0), None);
        assert_eq!(MsgType::from_u8(5), None);

        for s in [Side::Bid, Side::Ask, Side::Both] {
            assert_eq!(Side::from_u8(s.as_u8()), Some(s));
        }
        assert_eq!(Side::from_u8(3), None);
    }

    #[test]
    fn only_depth_records_affect_the_book() {
        assert!(Msg::depth(Side::Bid, Price::from_e8(1), Qty::from_e8(1), "X").affects_book());
        assert!(Msg::reset(Side::Bid, "X").affects_book());
        assert!(!Msg {
            kind: MsgType::Heartbeat,
            ..Msg::default()
        }
        .affects_book());
        assert!(!Msg {
            kind: MsgType::Cmd,
            ..Msg::default()
        }
        .affects_book());
    }
}
