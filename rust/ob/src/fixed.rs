//! Prices and sizes as scaled integers, and the formatting rules that go with
//! them.
//!
//! # Why not `f64`
//!
//! A price is a label, not a measurement. `65000.10` and `65000.09999999999`
//! are different keys, and a binary floating-point value cannot represent either
//! of them exactly. In a book keyed by price, that difference is not a rounding
//! nuisance: two ticks that should be the same level become two levels, and an
//! erase that arrives as `65000.1` misses the row stored as `65000.10000001`.
//!
//! So the lab stores prices and sizes as `i64` scaled by [`E8`] — 100 million —
//! which is Binance's own convention for these two fields. `65000.10` is
//! `6_500_010_000_000`. Comparison is integer comparison, equality is exact, and
//! the wire record needs no float.
//!
//! # Where floats are allowed
//!
//! Formatting, and nothing else. [`Price::to_f64`] and [`Qty::to_f64`] exist so
//! that a report can print a percentage; the display helpers below do not even
//! use them, because integer formatting avoids a rounding mode that differs
//! between platforms.

use std::fmt;

/// The scale: one unit of a price or a size is one hundred-millionth.
pub const E8: i64 = 100_000_000;

/// Digits in the scale, used by the formatters.
const E8_DIGITS: usize = 8;

/// A price, scaled by [`E8`].
///
/// `Ord` is derived, and it is the ordering the book depends on: the best bid is
/// the largest price, the best ask is the smallest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Price(i64);

/// A size, scaled by [`E8`].
///
/// Negative sizes do not occur on the wire; the type does not forbid them
/// because a delta feed would use a negative size to mean a decrease, and the
/// lab would rather keep that option open than add a second type for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Qty(i64);

impl Price {
    /// Wrap a raw scaled integer.
    #[inline]
    pub const fn from_e8(raw: i64) -> Self {
        Price(raw)
    }

    /// The raw scaled integer, as it appears in the wire record.
    #[inline]
    pub const fn as_e8(self) -> i64 {
        self.0
    }

    /// Build from a decimal string such as `"65000.10"`, exactly.
    ///
    /// Used by tests and by the fixture reader; never on the ingest path, which
    /// receives already-scaled integers.
    ///
    /// # Errors
    ///
    /// Returns `None` for a malformed string or more than eight fractional
    /// digits, rather than rounding silently.
    pub fn parse(text: &str) -> Option<Self> {
        parse_scaled(text).map(Price)
    }

    /// The midpoint of two prices, rounded towards zero.
    ///
    /// Computed as `a + (b - a) / 2` rather than `(a + b) / 2` so that two large
    /// prices near `i64::MAX` cannot overflow. With `b >= a` the subtraction is
    /// the smaller number and the addition stays inside the range.
    #[inline]
    pub const fn mid(self, other: Self) -> i64 {
        let (lo, hi) = if self.0 <= other.0 {
            (self.0, other.0)
        } else {
            (other.0, self.0)
        };
        lo + (hi - lo) / 2
    }

    /// How far this price is from `reference`, in hundredths of a basis point.
    ///
    /// One basis point is 0.01%, so the result is in units of 0.0001%. This is
    /// the unit every slippage number in [`crate::slippage`] is reported in: it
    /// is small enough to show a sub-tick cost on a liquid symbol and large
    /// enough that integer rounding does not dominate.
    #[inline]
    pub fn bps_x100_from(self, reference: i64) -> i64 {
        delta_bps_x100(self.0.wrapping_sub(reference), reference)
    }

    /// Lossy conversion, for display only.
    ///
    /// Never use this to make a decision: it is here so a report can print a
    /// percentage, and every comparison in this crate is done on the integer.
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / E8 as f64
    }

    /// Format with `decimals` places, rounded half away from zero.
    pub fn format(self, decimals: usize) -> String {
        format_scaled(self.0, decimals)
    }
}

impl Qty {
    /// Wrap a raw scaled integer.
    #[inline]
    pub const fn from_e8(raw: i64) -> Self {
        Qty(raw)
    }

    /// The raw scaled integer, as it appears in the wire record.
    #[inline]
    pub const fn as_e8(self) -> i64 {
        self.0
    }

    /// Build from a decimal string such as `"0.50000000"`, exactly.
    pub fn parse(text: &str) -> Option<Self> {
        parse_scaled(text).map(Qty)
    }

    /// Whether this size means "remove the level".
    ///
    /// A zero size is how a diff feed deletes a price, so this predicate has a
    /// name rather than appearing as `== 0` at each call site.
    #[inline]
    pub const fn is_delete(self) -> bool {
        self.0 == 0
    }

    /// Whether this size is a real quantity.
    #[inline]
    pub const fn is_live(self) -> bool {
        self.0 > 0
    }

    /// Multiply by a price and divide by the scale, keeping the result in
    /// e8-scaled units: `price * qty / E8`, which is a notional value in
    /// currency scaled by e8.
    ///
    /// Done in `i128` because `price * qty` overflows `i64` for any realistic
    /// pair: `65000.10 * 0.50` is `6.5e12 * 5e7 = 3.25e20`.
    #[inline]
    pub const fn notional_e8(self, price: Price) -> i64 {
        let product = (self.0 as i128) * (price.0 as i128) / (E8 as i128);
        clamp_i64(product)
    }

    /// Lossy conversion, for display only.
    #[inline]
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / E8 as f64
    }

    /// Format with `decimals` places, rounded half away from zero.
    pub fn format(self, decimals: usize) -> String {
        format_scaled(self.0, decimals)
    }
}

impl fmt::Display for Price {
    /// Two decimals, matching the terminal book in both implementations.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_scaled(self.0, 2))
    }
}

impl fmt::Display for Qty {
    /// Up to eight decimals with trailing zeros removed, so `0.5` prints as
    /// `0.5` and not `0.50000000`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let full = format_scaled(self.0, E8_DIGITS);
        f.write_str(&trim_trailing_zeros(&full))
    }
}

/// A change of `delta`, measured against `reference`, in hundredths of a basis
/// point.
///
/// Separate from [`Price::bps_x100_from`] because the two take different kinds
/// of argument: that one compares two prices, this one scales a difference. The
/// arithmetic is the same either way — the e8 scale cancels because both sides
/// carry it — so only the 1e6 factor is left.
///
/// A zero reference has no percentage to report, and returns zero rather than
/// dividing by it.
#[inline]
pub fn delta_bps_x100(delta: i64, reference: i64) -> i64 {
    if reference == 0 {
        return 0;
    }
    let ratio = i128::from(delta) * 1_000_000 / i128::from(reference);
    clamp_i64(ratio)
}

/// Clamp an `i128` into `i64` rather than wrapping.
#[inline]
const fn clamp_i64(v: i128) -> i64 {
    if v > i64::MAX as i128 {
        i64::MAX
    } else if v < i64::MIN as i128 {
        i64::MIN
    } else {
        v as i64
    }
}

/// Exact decimal parse: sign, integer digits, optional fraction of at most eight
/// digits. No exponent, no thousands separators, no locale.
///
/// At least one digit is required somewhere. `"."`, `"-"`, and `"+"` are all
/// refused rather than read as zero, because a config or a fixture that produced
/// one of those has a bug and a silent zero would hide it.
fn parse_scaled(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (negative, rest) = match bytes[0] {
        b'-' => (true, &bytes[1..]),
        b'+' => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    let mut int_part: i64 = 0;
    let mut frac: i64 = 0;
    let mut frac_digits = 0usize;
    let mut digits = 0usize;
    let mut seen_dot = false;
    for &b in rest {
        match b {
            b'.' if !seen_dot => seen_dot = true,
            b'0'..=b'9' => {
                let digit = i64::from(b - b'0');
                digits += 1;
                if seen_dot {
                    if frac_digits == E8_DIGITS {
                        return None; // more precision than the scale can hold
                    }
                    frac = frac * 10 + digit;
                    frac_digits += 1;
                } else {
                    int_part = int_part.checked_mul(10)?.checked_add(digit)?;
                }
            }
            _ => return None,
        }
    }
    if digits == 0 {
        return None;
    }
    // Scale the fraction up to eight digits: "0.5" is 50_000_000, not 5.
    for _ in frac_digits..E8_DIGITS {
        frac *= 10;
    }
    let magnitude = int_part.checked_mul(E8)?.checked_add(frac)?;
    Some(if negative { -magnitude } else { magnitude })
}

/// Format a scaled integer with `decimals` places, rounded half away from zero.
///
/// Integer-only on purpose: `format!("{:.2}", x as f64 / 1e8)` would give a
/// different answer for the same input on a platform whose `printf` rounds
/// differently, and the two implementations of this lab are compared on their
/// printed output.
fn format_scaled(raw: i64, decimals: usize) -> String {
    let decimals = decimals.min(E8_DIGITS);
    let negative = raw < 0;
    // `unsigned_abs` handles i64::MIN, where `-raw` would overflow.
    let magnitude = raw.unsigned_abs();
    let scale = 10u64.pow(E8_DIGITS as u32);
    let divisor = 10u64.pow((E8_DIGITS - decimals) as u32);
    let int_part = magnitude / scale;
    let frac_all = magnitude % scale;
    // Round half away from zero, carrying into the integer part when the
    // fraction rounds up to a whole unit.
    let rounded = frac_all / divisor + u64::from((frac_all % divisor) * 2 >= divisor);
    let (int_part, frac) = if rounded >= 10u64.pow(decimals as u32) {
        (int_part + 1, 0)
    } else {
        (int_part, rounded)
    };
    let mut out = String::with_capacity(24);
    if negative && (int_part != 0 || frac != 0) {
        out.push('-');
    }
    out.push_str(&int_part.to_string());
    if decimals > 0 {
        out.push('.');
        let digits = frac.to_string();
        for _ in digits.len()..decimals {
            out.push('0');
        }
        out.push_str(&digits);
    }
    out
}

/// Remove trailing zeros, and a trailing point if the fraction was all zeros.
fn trim_trailing_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    let trimmed = text.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_exact_and_refuses_to_round() {
        assert_eq!(Price::parse("65000.10").unwrap().as_e8(), 6_500_010_000_000);
        assert_eq!(Price::parse("0").unwrap().as_e8(), 0);
        assert_eq!(Price::parse("-1.5").unwrap().as_e8(), -150_000_000);
        assert_eq!(Qty::parse("0.50000000").unwrap().as_e8(), 50_000_000);
        assert_eq!(Qty::parse("1").unwrap().as_e8(), E8);
        // Nine fractional digits cannot be represented; refuse rather than round.
        assert!(Price::parse("1.123456789").is_none());
        assert!(Price::parse("abc").is_none());
        assert!(Price::parse("").is_none());
        assert!(Price::parse(".").is_none());
        assert!(Price::parse("1e8").is_none());
    }

    #[test]
    fn formatting_matches_the_cpp_terminal() {
        // The C++ TUI prints prices with %.2f and sizes with %.8f trimmed.
        assert_eq!(Price::from_e8(6_500_010_000_000).to_string(), "65000.10");
        assert_eq!(Price::from_e8(6_500_000_000_000).to_string(), "65000.00");
        assert_eq!(Price::from_e8(1).to_string(), "0.00");
        assert_eq!(Price::from_e8(5_000_000).to_string(), "0.05");
        assert_eq!(Qty::from_e8(50_000_000).to_string(), "0.5");
        assert_eq!(Qty::from_e8(150_000_000).to_string(), "1.5");
        assert_eq!(Qty::from_e8(0).to_string(), "0");
        assert_eq!(Qty::from_e8(E8).to_string(), "1");
        assert_eq!(Qty::from_e8(1).to_string(), "0.00000001");
    }

    #[test]
    fn formatting_rounds_half_away_from_zero() {
        // 0.005 rounds to 0.01, and the carry out of the fraction is handled.
        assert_eq!(Price::from_e8(500_000).to_string(), "0.01");
        assert_eq!(Price::from_e8(499_999).to_string(), "0.00");
        assert_eq!(Price::from_e8(99_999_999).to_string(), "1.00");
        assert_eq!(Price::from_e8(-500_000).to_string(), "-0.01");
    }

    #[test]
    fn mid_does_not_overflow_near_the_top_of_the_range() {
        let a = Price::from_e8(i64::MAX - 1);
        let b = Price::from_e8(i64::MAX);
        // The naive (a + b) / 2 overflows; the difference form does not.
        assert!(a.mid(b) > 0);
        assert_eq!(Price::from_e8(0).mid(Price::from_e8(4)), 2);
        assert_eq!(Price::from_e8(4).mid(Price::from_e8(0)), 2);
        assert_eq!(Price::from_e8(-4).mid(Price::from_e8(4)), 0);
    }

    #[test]
    fn basis_points_are_hundredths_of_a_bp() {
        // A move from 100.00 to 100.01 is one basis point: 100 in this unit.
        let reference = 100 * E8;
        let moved = 100 * E8 + 1_000_000; // +0.01
        assert_eq!(Price::from_e8(moved).bps_x100_from(reference), 100);
        // Half a basis point.
        let half = 100 * E8 + 500_000;
        assert_eq!(Price::from_e8(half).bps_x100_from(reference), 50);
        // Downwards is negative, and symmetric.
        let down = 100 * E8 - 1_000_000;
        assert_eq!(Price::from_e8(down).bps_x100_from(reference), -100);
        // A zero reference has no percentage to report.
        assert_eq!(Price::from_e8(moved).bps_x100_from(0), 0);
    }

    #[test]
    fn a_delta_is_scaled_against_the_reference_not_subtracted_from_it() {
        // One cent on a hundred dollars is one basis point: 100 in this unit.
        assert_eq!(delta_bps_x100(1_000_000, 100 * E8), 100);
        // The same absolute move on a ten-dollar reference is ten times larger.
        assert_eq!(delta_bps_x100(1_000_000, 10 * E8), 1000);
        // Downwards is negative.
        assert_eq!(delta_bps_x100(-1_000_000, 100 * E8), -100);
        assert_eq!(delta_bps_x100(1_000_000, 0), 0);
        // A price compared to a reference is the same function of the
        // difference, which is what makes the two entry points consistent.
        let px = Price::from_e8(100 * E8 + 1_000_000);
        assert_eq!(
            px.bps_x100_from(100 * E8),
            delta_bps_x100(1_000_000, 100 * E8)
        );
    }

    #[test]
    fn notional_uses_wide_arithmetic() {
        // 65000.10 * 0.5 = 32500.05, which overflows i64 in the raw product.
        let px = Price::from_e8(6_500_010_000_000);
        let qty = Qty::from_e8(50_000_000);
        assert_eq!(qty.notional_e8(px), 3_250_005_000_000);
    }
}
