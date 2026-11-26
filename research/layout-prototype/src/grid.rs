//! Block shape and row access. The only module that uses `usize`, as `base::index` is in nami.
//!
//! Every slice access in the prototype goes through `row`, `row_mut`, `sub` and `sub_mut`, which
//! use `chunks_exact().nth()` and `get()` instead of raw indexing (ADR-0003).

/// Ghost cells per side (PLM needs two).
pub const GHOST: u32 = 2;

/// Number of conserved and primitive variables of 3D Euler.
pub const NVAR: usize = 5;

/// A cubic block of `n`³ interior cells with `GHOST` ghost cells on each side.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    n: u32,
}

impl Shape {
    pub fn new(n: u32) -> Self {
        assert!(n >= 8, "block must have at least 8 cells per side");
        assert!(n % 8 == 0, "cells per side must be a multiple of 8 (hybrid lane groups)");
        assert!(n <= 1024, "block larger than 1024 cells per side");
        Self { n }
    }

    /// Interior cells per side.
    pub fn n(self) -> usize {
        to_usize(self.n)
    }

    /// Cells per side including ghosts.
    pub fn padded(self) -> usize {
        to_usize(self.n + 2 * GHOST)
    }

    pub fn ghost(self) -> usize {
        to_usize(GHOST)
    }

    /// Interior cells in the block.
    pub fn cells(self) -> usize {
        let n = self.n();
        n * n * n
    }

    /// Cells including ghosts.
    pub fn padded_cells(self) -> usize {
        let p = self.padded();
        p * p * p
    }

    /// Cell width in a unit cube.
    pub fn dx(self) -> f64 {
        assert!(self.n > 0, "shape has cells");
        1.0 / f64::from(self.n)
    }

    /// Interior cells as `f64`, for normalisation.
    pub fn cells_f64(self) -> f64 {
        let n = f64::from(self.n);
        n * n * n
    }
}

/// Block dimensions, either given at runtime (`Shape`) or known at compile time (`Fixed<N>`).
/// The SoA layout is generic over it, to measure what compile-time trip counts buy.
pub trait Dims: Copy {
    /// Variant name in reports.
    const SOA_NAME: &'static str;

    /// Builds the dimensions from a runtime shape, asserting that they agree.
    fn from_shape(shape: Shape) -> Self;

    /// Interior cells per side.
    fn n(self) -> usize;

    /// Cells per side including ghosts.
    #[inline(always)]
    fn padded(self) -> usize {
        self.n() + 2 * to_usize(GHOST)
    }

    #[inline(always)]
    fn ghost(self) -> usize {
        to_usize(GHOST)
    }

    #[inline(always)]
    fn cells(self) -> usize {
        self.n() * self.n() * self.n()
    }

    #[inline(always)]
    fn padded_cells(self) -> usize {
        self.padded() * self.padded() * self.padded()
    }

    /// Cell width in a unit cube.
    #[inline(always)]
    fn dx(self) -> f64 {
        1.0 / to_f64(self.n())
    }
}

impl Dims for Shape {
    const SOA_NAME: &'static str = "soa";

    fn from_shape(shape: Shape) -> Self {
        shape
    }

    #[inline(always)]
    fn n(self) -> usize {
        to_usize(self.n)
    }
}

/// Dimensions known at compile time: `N`³ interior cells.
#[derive(Clone, Copy, Debug)]
pub struct Fixed<const N: u32>;

impl<const N: u32> Dims for Fixed<N> {
    const SOA_NAME: &'static str = "soa-const";

    fn from_shape(shape: Shape) -> Self {
        assert_eq!(Shape::n(shape), to_usize(N), "runtime shape must match the compile-time size");
        Self
    }

    #[inline(always)]
    fn n(self) -> usize {
        // `as` is banned; `usize::try_from` on a constant folds at compile time.
        to_usize(N)
    }
}

/// Lossless widening of a `u32`.
pub fn to_usize(value: u32) -> usize {
    usize::try_from(value).expect("usize holds every u32 on supported targets")
}

/// Converts a position to `f64` for coordinates.
pub fn to_f64(value: usize) -> f64 {
    let narrow = u32::try_from(value).expect("cell positions fit in u32");
    f64::from(narrow)
}

/// Row number `index` of length `len` in `data`.
#[inline(always)]
pub fn row<T>(data: &[T], len: usize, index: usize) -> &[T] {
    assert!(len > 0, "row length must be positive");
    assert!(data.len() % len == 0, "data must hold whole rows");
    let Some(found) = data.chunks_exact(len).nth(index) else {
        panic!("row {index} out of range");
    };
    found
}

/// Mutable row number `index` of length `len` in `data`.
#[inline(always)]
pub fn row_mut<T>(data: &mut [T], len: usize, index: usize) -> &mut [T] {
    assert!(len > 0, "row length must be positive");
    assert!(data.len() % len == 0, "data must hold whole rows");
    let Some(found) = data.chunks_exact_mut(len).nth(index) else {
        panic!("row {index} out of range");
    };
    found
}

/// The `len` elements starting at `start`.
#[inline(always)]
pub fn sub<T>(data: &[T], start: usize, len: usize) -> &[T] {
    let Some(found) = data.get(start..start + len) else {
        panic!("sub-slice {start}+{len} out of range {}", data.len());
    };
    found
}

/// The `len` elements starting at `start`, mutably.
#[inline(always)]
pub fn sub_mut<T>(data: &mut [T], start: usize, len: usize) -> &mut [T] {
    let total = data.len();
    let Some(found) = data.get_mut(start..start + len) else {
        panic!("sub-slice {start}+{len} out of range {total}");
    };
    found
}

/// Periodic ghost fill of one line: `line` is `(n + 2 * GHOST) * unit` elements, where `unit` is
/// 1 for a row, a row length for a plane, a plane length for the block.
pub fn periodic_fill<T: Copy>(line: &mut [T], unit: usize, n: usize) {
    let ghost = to_usize(GHOST) * unit;
    assert!(n * unit >= ghost, "interior must be at least as wide as the ghost layer");
    assert_eq!(line.len(), n * unit + 2 * ghost, "line length must match the shape");
    let (low, rest) = line.split_at_mut(ghost);
    let (interior, high) = rest.split_at_mut(n * unit);
    let (head, _) = interior.split_at(ghost);
    let (_, tail) = interior.split_at(n * unit - ghost);
    low.copy_from_slice(tail);
    high.copy_from_slice(head);
}

/// Sweep directions. The normal velocity goes first in the per-face state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    X,
    Y,
    Z,
}

impl Dir {
    /// Reorders `[density, v_x, v_y, v_z, pressure]` so the velocity normal to the face comes
    /// first, cyclically, as Athena++ does. Its own inverse is `unpermute`.
    #[inline(always)]
    pub fn permute<T>(self, vars: [T; NVAR]) -> [T; NVAR] {
        let [d, x, y, z, e] = vars;
        match self {
            Dir::X => [d, x, y, z, e],
            Dir::Y => [d, y, z, x, e],
            Dir::Z => [d, z, x, y, e],
        }
    }

    /// Row index of the ghosted cell at (other + GHOST, s) for Y, or (s, other + GHOST) for Z:
    /// `other` is the interior index of the fixed direction, `s` the ghosted sweep index.
    #[inline(always)]
    pub fn padded_row<D: Dims>(self, other: usize, s: usize, shape: D) -> usize {
        let p = shape.padded();
        let g = shape.ghost();
        match self {
            Dir::X => panic!("padded_row is for transverse sweeps"),
            Dir::Y => (other + g) * p + s,
            Dir::Z => s * p + (other + g),
        }
    }

    /// Row index of the interior cell row at (other, s) for Y, or (s, other) for Z.
    #[inline(always)]
    pub fn interior_row<D: Dims>(self, other: usize, s: usize, shape: D) -> usize {
        let n = shape.n();
        assert!(s < n && other < n, "interior row out of range");
        match self {
            Dir::X => panic!("interior_row is for transverse sweeps"),
            Dir::Y => other * n + s,
            Dir::Z => s * n + other,
        }
    }
}
