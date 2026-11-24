//! Typed indices, counts and field views (ADR-0002).
//!
//! This is the only module in nami that may use `usize`. Every conversion between nami's `u32`
//! index types and `usize` happens here, so the rest of the code cannot mix up index spaces or
//! truncate an index by accident.

use core::ops::{Index, IndexMut};

// Widening u32 to usize must be lossless on every target nami supports.
const _: () = assert!(usize::BITS >= 32, "usize must hold every u32 index");

/// Position of a cell in a field or pencil, counted from zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellIndex(u32);

/// Number of cells in a field or pencil.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellCount(u32);

impl CellIndex {
    /// Creates an index from its raw `u32` value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw `u32` value.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }

    /// Converts to `usize` for slice access. Always lossless.
    #[must_use]
    pub fn to_usize(self) -> usize {
        usize::try_from(self.0).expect("usize holds every u32 (checked at compile time)")
    }

    /// Converts from a `usize` position, asserting that it fits in `u32`.
    #[must_use]
    pub fn from_usize(position: usize) -> Self {
        let value = u32::try_from(position).expect("cell index must fit in u32 (ADR-0002)");
        Self(value)
    }
}

impl CellCount {
    /// Creates a count from its raw `u32` value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw `u32` value.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }

    /// Converts to `usize` for slice lengths. Always lossless.
    #[must_use]
    pub fn to_usize(self) -> usize {
        usize::try_from(self.0).expect("usize holds every u32 (checked at compile time)")
    }

    /// Converts from a slice length, asserting that it fits in `u32`.
    #[must_use]
    pub fn from_usize(length: usize) -> Self {
        let value = u32::try_from(length).expect("cell count must fit in u32 (ADR-0002)");
        Self(value)
    }

    /// Returns true if `index` addresses one of these cells.
    #[must_use]
    pub const fn contains(self, index: CellIndex) -> bool {
        index.0 < self.0
    }

    /// Iterates over every index from zero up to, not including, the count.
    pub fn indices(self) -> impl Iterator<Item = CellIndex> {
        (0..self.0).map(CellIndex)
    }
}

/// Read-only view of per-cell values, indexed by [`CellIndex`].
#[derive(Clone, Copy, Debug)]
pub struct CellSlice<'a, T> {
    values: &'a [T],
}

/// Mutable view of per-cell values, indexed by [`CellIndex`].
#[derive(Debug)]
pub struct CellSliceMut<'a, T> {
    values: &'a mut [T],
}

impl<'a, T> CellSlice<'a, T> {
    /// Wraps a slice, asserting that its length fits in a [`CellCount`].
    #[must_use]
    pub fn new(values: &'a [T]) -> Self {
        let count = CellCount::from_usize(values.len());
        assert_eq!(count.to_usize(), values.len(), "length must round-trip through u32");
        Self { values }
    }

    /// Returns the number of cells.
    #[must_use]
    pub fn count(&self) -> CellCount {
        CellCount::from_usize(self.values.len())
    }

    /// Returns the underlying contiguous slice, for pencil sweeps (ADR-0003).
    #[must_use]
    pub fn as_slice(&self) -> &'a [T] {
        self.values
    }
}

impl<'a, T> CellSliceMut<'a, T> {
    /// Wraps a mutable slice, asserting that its length fits in a [`CellCount`].
    #[must_use]
    pub fn new(values: &'a mut [T]) -> Self {
        let count = CellCount::from_usize(values.len());
        assert_eq!(count.to_usize(), values.len(), "length must round-trip through u32");
        Self { values }
    }

    /// Returns the number of cells.
    #[must_use]
    pub fn count(&self) -> CellCount {
        CellCount::from_usize(self.values.len())
    }

    /// Returns the underlying contiguous slice, for pencil sweeps (ADR-0003).
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.values
    }
}

// The single audited indexing site for typed gathers (ADR-0002, ADR-0003).
impl<T> Index<CellIndex> for CellSlice<'_, T> {
    type Output = T;

    fn index(&self, index: CellIndex) -> &T {
        assert!(self.count().contains(index), "cell index must be within the field");
        self.values.get(index.to_usize()).expect("bounds asserted above")
    }
}

impl<T> Index<CellIndex> for CellSliceMut<'_, T> {
    type Output = T;

    fn index(&self, index: CellIndex) -> &T {
        assert!(self.count().contains(index), "cell index must be within the field");
        self.values.get(index.to_usize()).expect("bounds asserted above")
    }
}

impl<T> IndexMut<CellIndex> for CellSliceMut<'_, T> {
    fn index_mut(&mut self, index: CellIndex) -> &mut T {
        assert!(self.count().contains(index), "cell index must be within the field");
        self.values.get_mut(index.to_usize()).expect("bounds asserted above")
    }
}

#[cfg(test)]
mod tests {
    use super::{CellCount, CellIndex, CellSlice, CellSliceMut};

    // Goal: conversions round-trip and `contains` matches the half-open range [0, count).
    #[test]
    fn index_and_count_conversions() {
        let count = CellCount::new(4);
        assert_eq!(count.to_usize(), 4);
        assert_eq!(CellIndex::from_usize(3).value(), 3);
        assert!(count.contains(CellIndex::new(3)));
        assert!(!count.contains(CellIndex::new(4)));
        let indices: Vec<u32> = count.indices().map(CellIndex::value).collect();
        assert_eq!(indices, [0, 1, 2, 3]);
    }

    // Goal: positions beyond u32 are rejected instead of silently truncated.
    #[test]
    #[should_panic(expected = "cell index must fit in u32")]
    fn from_usize_rejects_values_beyond_u32() {
        let position = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        let _ = CellIndex::from_usize(position);
    }

    // Goal: typed gathers read and write the addressed cell.
    #[test]
    fn typed_gather_reads_and_writes() {
        let mut values = [1.0_f64, 2.0_f64, 3.0_f64];
        let mut field = CellSliceMut::new(&mut values);
        field[CellIndex::new(1)] = 5.0_f64;
        assert_eq!(field[CellIndex::new(1)].to_bits(), 5.0_f64.to_bits());
        let view = CellSlice::new(&values);
        assert_eq!(view.count(), CellCount::new(3));
        assert_eq!(view[CellIndex::new(2)].to_bits(), 3.0_f64.to_bits());
    }

    // Goal: a gather outside the field fails with the assertion message, not a generic panic.
    #[test]
    #[should_panic(expected = "cell index must be within the field")]
    fn typed_gather_rejects_out_of_range() {
        let values = [1.0_f64, 2.0_f64];
        let view = CellSlice::new(&values);
        let _ = view[CellIndex::new(2)];
    }
}
