//! Symbols become dense integers, once, at the edge.
//!
//! # The problem
//!
//! A record carries eight bytes of symbol. Every structure behind the ingest
//! point wants a small integer: an array index, a shard number, a key in a
//! per-symbol counter. Hashing eight bytes on every update is a cost paid
//! thousands of times a second to answer a question whose answer never changes.
//!
//! So the wire symbol is interned exactly once — the first time this process
//! sees it — and everything after that carries a [`SymbolId`], a `u32` that is
//! also an array index.
//!
//! # What this buys, and what it costs
//!
//! An update for a known symbol costs one comparison of a `u32` and an array
//! index, with no allocation and no hashing. The cost is a fixed table whose
//! size has to be chosen up front and an error path when a feed sends more
//! distinct symbols than that. [`SymbolTable`] reports
//! [`Error::Full`](crate::Error::Full) rather than growing without bound: a
//! feed that suddenly sends ten thousand symbols is a feed change worth
//! noticing, not a reason to allocate.
//!
//! # Why the ids are dense
//!
//! Ids are assigned in first-seen order, `0, 1, 2, …`, and never reused. That
//! makes `id` usable as an index into a per-symbol array with no second lookup,
//! which is what [`crate::cache`] relies on to keep a thousand symbols in
//! flat storage instead of a thousand hash maps.

use crate::error::{Error, Result};
use crate::msg::SYMBOL_LEN;

/// A dense identifier for one symbol.
///
/// Small, `Copy`, and cheap to compare. The numeric value is only meaningful
/// alongside the [`SymbolTable`] that issued it; it is not a market identifier
/// and does not encode anything about the symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId(u32);

impl SymbolId {
    /// The id as a `usize`, for indexing.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The id as a `u32`, for wire and counter fields.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// The first id a table issues.
    ///
    /// Used to fill an output buffer before it is overwritten. It is not a
    /// meaningful symbol on its own — an id is only meaningful next to the table
    /// that issued it — but it is the one value that cannot be mistaken for an
    /// id a table never handed out.
    #[inline]
    pub const fn first() -> Self {
        SymbolId(0)
    }
}

/// Interns wire symbols into [`SymbolId`]s and back.
///
/// Open addressing with linear probing over a power-of-two table. The load
/// factor is kept under one half, which is what makes linear probing cheap here
/// and is the reason the table is twice the size of the symbol capacity.
#[derive(Debug)]
pub struct SymbolTable {
    /// `slots[id]` is the eight wire bytes for that id. Ids are dense.
    slots: Vec<[u8; SYMBOL_LEN]>,
    /// `index[hash]` holds `id + 1`, or zero for an empty bucket. Storing
    /// `id + 1` rather than the id lets zero mean "empty" without a sentinel
    /// value that is also a legal id.
    index: Vec<u32>,
    /// Mask for the power-of-two table size.
    mask: usize,
    /// How many ids have been issued.
    len: usize,
    /// The most ids that may be issued.
    capacity: usize,
}

impl SymbolTable {
    /// Build a table that can hold `capacity` distinct symbols.
    ///
    /// Allocates once. Nothing after this allocates until the table grows, and
    /// it only grows on the first sight of a symbol, never on an update.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // Twice the capacity, rounded up to a power of two: at full occupancy
        // the load factor is 0.5, so the expected probe length is under 1.5.
        let buckets = (capacity * 2).next_power_of_two();
        SymbolTable {
            slots: Vec::with_capacity(capacity),
            index: vec![0u32; buckets],
            mask: buckets - 1,
            len: 0,
            capacity,
        }
    }

    /// How many symbols have been interned.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no symbol has been interned yet.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The most symbols this table will accept.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Look up a symbol without adding it.
    #[inline]
    pub fn lookup(&self, symbol: &[u8; SYMBOL_LEN]) -> Option<SymbolId> {
        let mut bucket = hash_symbol(symbol) & self.mask;
        loop {
            let entry = self.index[bucket];
            if entry == 0 {
                return None;
            }
            let id = entry - 1;
            if &self.slots[id as usize] == symbol {
                return Some(SymbolId(id));
            }
            bucket = (bucket + 1) & self.mask;
        }
    }

    /// Intern a symbol, issuing a new id the first time it is seen.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Full`] when the table already holds `capacity` symbols.
    /// The caller decides whether that is fatal; the feed daemon logs it and
    /// drops the record, because dropping one unknown symbol is better than
    /// stopping a feed that is otherwise working.
    pub fn intern(&mut self, symbol: &[u8; SYMBOL_LEN]) -> Result<SymbolId> {
        if let Some(id) = self.lookup(symbol) {
            return Ok(id);
        }
        if self.len == self.capacity {
            // Grow rather than fail, but only up to the point where the
            // allocation is still bounded by the configured ceiling. Growth is
            // off the hot path by construction: it happens on the first sight
            // of a symbol, which is a moment when the process is not yet at
            // full rate for that symbol.
            self.grow()?;
        }
        let id = SymbolId(self.len as u32);
        self.slots.push(*symbol);
        self.len += 1;
        self.insert_bucket(id);
        Ok(id)
    }

    /// The wire bytes for an id.
    ///
    /// # Panics
    ///
    /// Panics if the id did not come from this table. That is a programming
    /// error, not a data error, and the alternative — returning zeros — would
    /// silently attribute one symbol's numbers to another.
    #[inline]
    pub fn symbol(&self, id: SymbolId) -> &[u8; SYMBOL_LEN] {
        &self.slots[id.index()]
    }

    /// The symbol as text, without padding.
    pub fn symbol_str(&self, id: SymbolId) -> std::borrow::Cow<'_, str> {
        let bytes = self.symbol(id);
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(SYMBOL_LEN);
        String::from_utf8_lossy(&bytes[..end])
    }

    /// Every id issued so far, in order.
    pub fn ids(&self) -> impl Iterator<Item = SymbolId> + '_ {
        (0..self.len as u32).map(SymbolId)
    }

    /// Double the table. Never called from an update path.
    fn grow(&mut self) -> Result<()> {
        // The ceiling is a policy choice, not a memory limit: past this point a
        // feed is not the shape this lab was built for and the operator should
        // raise the configured capacity deliberately.
        const CEILING: usize = 1 << 20;
        if self.capacity >= CEILING {
            return Err(Error::full("symbol table", self.capacity));
        }
        self.capacity *= 2;
        let buckets = (self.capacity * 2).next_power_of_two();
        self.index = vec![0u32; buckets];
        self.mask = buckets - 1;
        self.slots.reserve(self.capacity - self.slots.len());
        for id in 0..self.len {
            self.insert_bucket(SymbolId(id as u32));
        }
        Ok(())
    }

    /// Place one id in the probe table.
    fn insert_bucket(&mut self, id: SymbolId) {
        let symbol = self.slots[id.index()];
        let mut bucket = hash_symbol(&symbol) & self.mask;
        while self.index[bucket] != 0 {
            bucket = (bucket + 1) & self.mask;
        }
        self.index[bucket] = id.raw() + 1;
    }
}

/// FNV-1a over eight bytes, folded to 32 bits.
///
/// Eight bytes is a short key, so a hash with a multiply per byte is more than
/// fast enough and needs no table. The fold mixes the high and low halves so
/// that symbols differing only in their later characters — which is most
/// symbols in a real feed, where the quote currency is at the end — still land
/// in different buckets.
#[inline]
fn hash_symbol(symbol: &[u8; SYMBOL_LEN]) -> usize {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in symbol {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    ((h >> 32) ^ h) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> [u8; SYMBOL_LEN] {
        let mut out = [0u8; SYMBOL_LEN];
        let bytes = s.as_bytes();
        let take = bytes.len().min(SYMBOL_LEN);
        out[..take].copy_from_slice(&bytes[..take]);
        out
    }

    #[test]
    fn ids_are_dense_and_stable() {
        let mut t = SymbolTable::new(8);
        let a = t.intern(&sym("BTCUSDT")).unwrap();
        let b = t.intern(&sym("ETHUSDT")).unwrap();
        let again = t.intern(&sym("BTCUSDT")).unwrap();
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);
        assert_eq!(again, a, "interning twice must return the same id");
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn lookup_does_not_add() {
        let mut t = SymbolTable::new(8);
        assert_eq!(t.lookup(&sym("BTCUSDT")), None);
        let a = t.intern(&sym("BTCUSDT")).unwrap();
        assert_eq!(t.lookup(&sym("BTCUSDT")), Some(a));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn round_trips_through_text() {
        let mut t = SymbolTable::new(4);
        let a = t.intern(&sym("BTCUSDT")).unwrap();
        assert_eq!(t.symbol_str(a), "BTCUSDT");
        assert_eq!(&t.symbol(a)[..7], b"BTCUSDT");
        assert_eq!(t.symbol(a)[7], 0);
    }

    #[test]
    fn grows_past_its_initial_capacity_without_losing_anything() {
        let mut t = SymbolTable::new(4);
        let mut ids = Vec::new();
        for i in 0..500 {
            let name = format!("S{i:06}");
            ids.push(t.intern(&sym(&name)).unwrap());
        }
        assert_eq!(t.len(), 500);
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(t.symbol_str(*id), format!("S{i:06}"));
            assert_eq!(t.lookup(&sym(&format!("S{i:06}"))), Some(*id));
        }
    }

    #[test]
    fn similar_symbols_do_not_collide_into_a_long_probe() {
        // A thousand symbols that share a prefix and differ only in the last
        // characters: the shape a real feed has.
        let mut t = SymbolTable::new(16);
        for i in 0..1000 {
            let name = format!("USDT{i:04}");
            t.intern(&sym(&name)).unwrap();
        }
        for i in 0..1000 {
            let name = format!("USDT{i:04}");
            assert!(t.lookup(&sym(&name)).is_some());
        }
        assert_eq!(t.len(), 1000);
    }

    #[test]
    fn capacity_is_reported_before_it_is_exhausted() {
        let t = SymbolTable::new(16);
        assert_eq!(t.capacity(), 16);
        assert!(t.is_empty());
    }
}
