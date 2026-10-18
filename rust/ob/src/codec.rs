//! The byte layout of one record, in one place.
//!
//! [`crate::msg`] holds the meaning of the fields; this module holds their
//! positions. Both implementations write little-endian, which is the native
//! order of every platform this lab targets and the order Python's
//! `struct.pack("<…")` produces, so the encoder is a straight field-by-field
//! `to_le_bytes` and the decoder is its mirror.
//!
//! # Why the offsets are written out
//!
//! The layout is a contract with two other languages. A reader who wants to
//! check that contract should not have to run a program to do it, so the offsets
//! are named constants, asserted against the record size at compile time, and
//! tested against bytes the Python adapter produced.
//!
//! # This module is the first stage-2 target
//!
//! [`decode_batch`] is what the feed daemon runs on every buffer it reads from
//! the socket. It is written twice: once as the obvious loop, which is what the
//! code below is, and once as the reshaped version that
//! `docs/design/vec-optimization.md` measures. The obvious loop stays, because
//! it is the one a reader can check against the layout table.

use crate::error::{Error, Result};
use crate::msg::{Msg, MsgType, Side, MAGIC, MSG_SIZE};
use crate::Wire;

/// Offset of `magic`.
pub const OFF_MAGIC: usize = 0;
/// Offset of `nbytes`.
pub const OFF_NBYTES: usize = 4;
/// Offset of `kind`.
pub const OFF_KIND: usize = 6;
/// Offset of `side`.
pub const OFF_SIDE: usize = 7;
/// Offset of `seq`.
pub const OFF_SEQ: usize = 8;
/// Offset of `ts_ns`.
pub const OFF_TS_NS: usize = 16;
/// Offset of `trace_id`.
pub const OFF_TRACE_ID: usize = 24;
/// Offset of `span_id`.
pub const OFF_SPAN_ID: usize = 40;
/// Offset of `px_e8`.
pub const OFF_PX_E8: usize = 48;
/// Offset of `qty_e8`.
pub const OFF_QTY_E8: usize = 56;
/// Offset of `symbol`.
pub const OFF_SYMBOL: usize = 64;

/// Compile-time proof that the table above describes a 72-byte record.
const _: () = {
    assert!(MSG_SIZE == 72);
    assert!(OFF_MAGIC + 4 == OFF_NBYTES);
    assert!(OFF_NBYTES + 2 == OFF_KIND);
    assert!(OFF_KIND + 1 == OFF_SIDE);
    assert!(OFF_SIDE + 1 == OFF_SEQ);
    assert!(OFF_SEQ + 8 == OFF_TS_NS);
    assert!(OFF_TS_NS + 8 == OFF_TRACE_ID);
    assert!(OFF_TRACE_ID + 16 == OFF_SPAN_ID);
    assert!(OFF_SPAN_ID + 8 == OFF_PX_E8);
    assert!(OFF_PX_E8 + 8 == OFF_QTY_E8);
    assert!(OFF_QTY_E8 + 8 == OFF_SYMBOL);
    assert!(OFF_SYMBOL + 8 == MSG_SIZE);
};

/// Read a little-endian `u64` at a fixed offset.
///
/// The index is a constant and the slice has already been length-checked, so
/// the bounds check folds away; `copy_from_slice` is the documented way to say
/// that without an `unsafe` block.
#[inline(always)]
fn u64_at(b: &[u8], off: usize) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(buf)
}

/// Read a little-endian `i64` at a fixed offset.
#[inline(always)]
fn i64_at(b: &[u8], off: usize) -> i64 {
    u64_at(b, off) as i64
}

/// Read a little-endian `u32` at a fixed offset.
#[inline(always)]
fn u32_at(b: &[u8], off: usize) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&b[off..off + 4]);
    u32::from_le_bytes(buf)
}

/// Read a little-endian `u16` at a fixed offset.
#[inline(always)]
fn u16_at(b: &[u8], off: usize) -> u16 {
    let mut buf = [0u8; 2];
    buf.copy_from_slice(&b[off..off + 2]);
    u16::from_le_bytes(buf)
}

/// Encode one record into the wire layout.
pub fn encode(m: &Msg) -> Wire {
    let mut out = [0u8; MSG_SIZE];
    out[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&m.magic.to_le_bytes());
    out[OFF_NBYTES..OFF_NBYTES + 2].copy_from_slice(&m.nbytes.to_le_bytes());
    out[OFF_KIND] = m.kind.as_u8();
    out[OFF_SIDE] = m.side.as_u8();
    out[OFF_SEQ..OFF_SEQ + 8].copy_from_slice(&m.seq.to_le_bytes());
    out[OFF_TS_NS..OFF_TS_NS + 8].copy_from_slice(&m.ts_ns.to_le_bytes());
    out[OFF_TRACE_ID..OFF_TRACE_ID + 16].copy_from_slice(&m.trace_id);
    out[OFF_SPAN_ID..OFF_SPAN_ID + 8].copy_from_slice(&m.span_id);
    out[OFF_PX_E8..OFF_PX_E8 + 8].copy_from_slice(&m.px_e8.to_le_bytes());
    out[OFF_QTY_E8..OFF_QTY_E8 + 8].copy_from_slice(&m.qty_e8.to_le_bytes());
    out[OFF_SYMBOL..OFF_SYMBOL + 8].copy_from_slice(&m.symbol);
    out
}

/// Encode into a caller-owned buffer, so the hot path does not build a copy.
pub fn encode_into(m: &Msg, out: &mut Wire) {
    *out = encode(m);
}

/// Decode one record from a 72-byte span.
///
/// # Errors
///
/// Every rejection carries the reason, because a feed that suddenly stops
/// producing records should say whether it was the magic, the length, or an
/// unknown code that stopped it.
pub fn decode(bytes: &[u8]) -> Result<Msg> {
    if bytes.len() != MSG_SIZE {
        return Err(Error::Wire(format!(
            "record is {} bytes, expected {MSG_SIZE}",
            bytes.len()
        )));
    }
    let magic = u32_at(bytes, OFF_MAGIC);
    if magic != MAGIC {
        return Err(Error::Wire(format!(
            "magic {magic:#010x} is not {MAGIC:#010x}"
        )));
    }
    let nbytes = u16_at(bytes, OFF_NBYTES);
    if nbytes as usize != MSG_SIZE {
        return Err(Error::Wire(format!(
            "declared length {nbytes} is not {MSG_SIZE}"
        )));
    }
    let kind = MsgType::from_u8(bytes[OFF_KIND])
        .ok_or_else(|| Error::Wire(format!("unknown type code {}", bytes[OFF_KIND])))?;
    let side = Side::from_u8(bytes[OFF_SIDE])
        .ok_or_else(|| Error::Wire(format!("unknown side code {}", bytes[OFF_SIDE])))?;

    let mut trace_id = [0u8; 16];
    trace_id.copy_from_slice(&bytes[OFF_TRACE_ID..OFF_TRACE_ID + 16]);
    let mut span_id = [0u8; 8];
    span_id.copy_from_slice(&bytes[OFF_SPAN_ID..OFF_SPAN_ID + 8]);
    let mut symbol = [0u8; 8];
    symbol.copy_from_slice(&bytes[OFF_SYMBOL..OFF_SYMBOL + 8]);

    Ok(Msg {
        magic,
        nbytes,
        kind,
        side,
        seq: u64_at(bytes, OFF_SEQ),
        ts_ns: u64_at(bytes, OFF_TS_NS),
        trace_id,
        span_id,
        px_e8: i64_at(bytes, OFF_PX_E8),
        qty_e8: i64_at(bytes, OFF_QTY_E8),
        symbol,
    })
}

/// How many whole, valid records start at the beginning of `buf`.
///
/// This is the general question: a record that fails its header check is where
/// the run stops. The daemon does not resynchronise, because a fixed 72-byte
/// framing has no delimiter to search for, and skipping bytes to find one would
/// be inventing a protocol.
///
/// A damaged record in the middle ends the run, so this always scans. When the
/// caller knows the buffer came from a stream, [`stream_records`] answers the
/// same question in constant time.
pub fn valid_prefix_len(buf: &[u8]) -> usize {
    let mut count = 0;
    for record in buf.chunks_exact(MSG_SIZE) {
        if !header_ok(record) {
            break;
        }
        count += 1;
    }
    count
}

/// How many whole records a buffer read from a stream holds.
///
/// # The precondition, and why it makes this constant time
///
/// A socket delivers bytes in order. Nothing can be missing from the middle of a
/// buffer that came from one `read`, so the only defect possible is a **torn
/// tail**: a whole 72-byte slot whose bytes are a partial record padded out. That
/// means the question reduces to "is the last whole record intact", and if it is,
/// every record before it is too.
///
/// This is not [`valid_prefix_len`] with a shortcut bolted on; it is a different
/// question. `valid_prefix_len` accepts an arbitrary buffer and must assume a
/// damaged record can be anywhere, which is why it scans. Calling this on a
/// buffer that did not come from a stream, or that a sender could have written a
/// hole into, gives a wrong answer — and the first version of this function did
/// exactly that, which a test caught.
///
/// The scan is kept for the torn-tail case, and it is a strided walk that does
/// not vectorize. `docs/design/vec-optimization.md` §4.1 has the measurement:
/// 112 times faster on the common path, because the common path is one
/// comparison.
pub fn stream_records(buf: &[u8]) -> usize {
    let whole = buf.len() / MSG_SIZE;
    if whole == 0 {
        return 0;
    }
    let last = (whole - 1) * MSG_SIZE;
    if header_ok(&buf[last..last + MSG_SIZE]) {
        return whole;
    }
    valid_prefix_len(buf)
}

/// Whether a record's two header fields are correct.
///
/// Split out because it is the only part of the check that reads at a fixed
/// offset, and stage 2 reshapes the scan around exactly this predicate.
#[inline(always)]
pub fn header_ok(record: &[u8]) -> bool {
    debug_assert!(record.len() >= MSG_SIZE);
    u32_at(record, OFF_MAGIC) == MAGIC && u16_at(record, OFF_NBYTES) as usize == MSG_SIZE
}

/// Decode every whole record in `buf` into `out`, returning how many were
/// written.
///
/// Records are decoded in order until the output is full or the input is
/// exhausted. A record that fails to decode is skipped rather than ending the
/// batch: the daemon's policy is that one bad record must not stop a feed, and
/// [`skipped`](BatchStats::skipped) counts them so a run that drops half its
/// input is visible rather than quiet.
///
/// The decoded records are **compacted** to the front of `out`: after a call,
/// `out[..stats.decoded]` holds them in input order, and the tail of `out` is
/// untouched. A caller that wants to know which input record was rejected
/// should decode one at a time instead; the batch form is for the common case,
/// where a rejected record is a counter rather than a decision.
///
/// # Returns
///
/// [`BatchStats`] with the counts, so the caller can log a rate rather than a
/// single event.
#[inline]
pub fn decode_batch(buf: &[u8], out: &mut [Msg]) -> BatchStats {
    let mut stats = BatchStats::default();
    let mut written = 0usize;
    for record in buf.chunks_exact(MSG_SIZE) {
        if written >= out.len() {
            break;
        }
        stats.records += 1;
        match decode(record) {
            Ok(m) => {
                out[written] = m;
                written += 1;
                stats.decoded += 1;
            }
            Err(_) => stats.skipped += 1,
        }
    }
    stats
}

/// The sequence number and the daemon's stamp of a record, if its header is
/// valid.
///
/// A caller that needs to know *when* a record was published rather than what it
/// says should not pay for a full decode: both fields sit at fixed offsets, and
/// the header check is what makes them trustworthy. A damaged record returns
/// `None` rather than a plausible-looking stamp, because a wrong time is worse
/// than no time when the number is used to decide whether a reader is behind.
#[inline]
pub fn seq_and_stamp(record: &[u8]) -> Option<(u64, u64)> {
    if record.len() < MSG_SIZE || !header_ok(record) {
        return None;
    }
    Some((u64_at(record, OFF_SEQ), u64_at(record, OFF_TS_NS)))
}

/// Counts from one [`decode_batch`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchStats {
    /// Whole 72-byte spans offered to the decoder.
    pub records: usize,
    /// Records that decoded.
    pub decoded: usize,
    /// Records that were rejected.
    pub skipped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::{Price, Qty};

    fn sample() -> Msg {
        let mut m = Msg::depth(
            Side::Bid,
            Price::from_e8(6_500_010_000_000),
            Qty::from_e8(50_000_000),
            "BTCUSDT",
        );
        m.seq = 42;
        m.ts_ns = 1_000_000;
        m.set_stamp_ns(999_000);
        m
    }

    #[test]
    fn layout_constants_describe_the_documented_record() {
        assert_eq!(OFF_MAGIC, 0);
        assert_eq!(OFF_NBYTES, 4);
        assert_eq!(OFF_KIND, 6);
        assert_eq!(OFF_SIDE, 7);
        assert_eq!(OFF_SEQ, 8);
        assert_eq!(OFF_TS_NS, 16);
        assert_eq!(OFF_TRACE_ID, 24);
        assert_eq!(OFF_SPAN_ID, 40);
        assert_eq!(OFF_PX_E8, 48);
        assert_eq!(OFF_QTY_E8, 56);
        assert_eq!(OFF_SYMBOL, 64);
        assert_eq!(OFF_SYMBOL + 8, MSG_SIZE);
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let m = sample();
        let wire = encode(&m);
        assert_eq!(wire.len(), MSG_SIZE);
        let back = decode(&wire).expect("decode");
        assert_eq!(back, m);
    }

    #[test]
    fn encoded_bytes_match_the_field_table() {
        let m = sample();
        let wire = encode(&m);
        // Little-endian, field by field, exactly as the table says.
        assert_eq!(&wire[0..4], &MAGIC.to_le_bytes());
        assert_eq!(&wire[4..6], &(MSG_SIZE as u16).to_le_bytes());
        assert_eq!(wire[6], 1); // Depth
        assert_eq!(wire[7], 0); // Bid
        assert_eq!(&wire[8..16], &42u64.to_le_bytes());
        assert_eq!(&wire[16..24], &1_000_000u64.to_le_bytes());
        assert_eq!(&wire[24..32], &999_000u64.to_le_bytes());
        assert_eq!(&wire[48..56], &6_500_010_000_000i64.to_le_bytes());
        assert_eq!(&wire[56..64], &50_000_000i64.to_le_bytes());
        assert_eq!(&wire[64..72], b"BTCUSDT\0");
    }

    #[test]
    fn decode_rejects_each_kind_of_damage() {
        let good = encode(&sample());

        let mut short = good;
        assert!(decode(&short[..MSG_SIZE - 1]).is_err());
        short[OFF_MAGIC] ^= 0xFF;
        assert!(decode(&short).is_err());

        let mut bad_len = good;
        bad_len[OFF_NBYTES] = 71;
        assert!(decode(&bad_len).is_err());

        let mut bad_type = good;
        bad_type[OFF_KIND] = 9;
        assert!(decode(&bad_type).is_err());

        let mut bad_side = good;
        bad_side[OFF_SIDE] = 7;
        assert!(decode(&bad_side).is_err());

        // The reasons differ, which is the point of returning an Error and not
        // a bool.
        assert!(decode(&bad_len)
            .unwrap_err()
            .to_string()
            .contains("declared length"));
        assert!(decode(&bad_type)
            .unwrap_err()
            .to_string()
            .contains("unknown type"));
    }

    #[test]
    fn valid_prefix_stops_at_the_first_bad_header() {
        let a = encode(&sample());
        let mut b = encode(&sample());
        b[OFF_MAGIC] = 0;
        let c = encode(&sample());

        let mut buf = Vec::new();
        buf.extend_from_slice(&a);
        buf.extend_from_slice(&b);
        buf.extend_from_slice(&c);
        assert_eq!(valid_prefix_len(&buf), 1);

        // A trailing partial record is not counted.
        let mut partial = Vec::new();
        partial.extend_from_slice(&a);
        partial.extend_from_slice(&a[..10]);
        assert_eq!(valid_prefix_len(&partial), 1);
        assert_eq!(valid_prefix_len(&[]), 0);
    }

    #[test]
    fn the_two_counting_functions_differ_exactly_where_they_should() {
        // A hole in the middle: an arbitrary buffer can have one, a stream
        // cannot. The general function stops; the stream function trusts the
        // last record, which is what its precondition licenses.
        let a = encode(&sample());
        let mut b = encode(&sample());
        b[OFF_MAGIC] = 0;
        let c = encode(&sample());
        let mut holed = Vec::new();
        holed.extend_from_slice(&a);
        holed.extend_from_slice(&b);
        holed.extend_from_slice(&c);

        assert_eq!(
            valid_prefix_len(&holed),
            1,
            "the general answer stops at the hole"
        );
        assert_eq!(
            stream_records(&holed),
            3,
            "the stream answer trusts the last record"
        );

        // A torn tail: both agree, because the last record is not intact.
        let mut torn = Vec::new();
        torn.extend_from_slice(&a);
        torn.extend_from_slice(&a);
        torn.extend_from_slice(&[0u8; MSG_SIZE]);
        assert_eq!(valid_prefix_len(&torn), 2);
        assert_eq!(stream_records(&torn), 2);

        // An all-valid buffer: both agree.
        let mut good = Vec::new();
        good.extend_from_slice(&a);
        good.extend_from_slice(&a);
        assert_eq!(valid_prefix_len(&good), 2);
        assert_eq!(stream_records(&good), 2);

        // Empty and sub-record buffers.
        assert_eq!(stream_records(&[]), 0);
        assert_eq!(stream_records(&a[..10]), 0);
    }

    #[test]
    fn a_stamp_can_be_read_without_decoding_the_record() {
        let mut m = sample();
        m.seq = 7;
        m.ts_ns = 1_234_567;
        let wire = encode(&m);
        assert_eq!(seq_and_stamp(&wire), Some((7, 1_234_567)));

        // A record whose header is wrong has no trustworthy stamp.
        let mut bad = wire;
        bad[OFF_MAGIC] = 0;
        assert_eq!(seq_and_stamp(&bad), None);
        assert_eq!(seq_and_stamp(&wire[..10]), None);
    }

    #[test]
    fn batch_decode_compacts_and_counts() {
        let good = encode(&sample());
        let mut bad = encode(&sample());
        bad[OFF_KIND] = 200;
        let mut buf = Vec::new();
        buf.extend_from_slice(&good);
        buf.extend_from_slice(&bad);
        buf.extend_from_slice(&good);

        let mut out = [Msg::default(); 3];
        let stats = decode_batch(&buf, &mut out);
        assert_eq!(stats.records, 3);
        assert_eq!(stats.decoded, 2);
        assert_eq!(stats.skipped, 1);
        // The survivors are compacted to the front, in input order.
        assert_eq!(out[0], sample());
        assert_eq!(out[1], sample());

        // A short output slice decodes only what fits and reports only that.
        let mut one = [Msg::default(); 1];
        let stats = decode_batch(&buf, &mut one);
        assert_eq!(stats.records, 1);
        assert_eq!(stats.decoded, 1);
        assert_eq!(stats.skipped, 0);
    }
}
