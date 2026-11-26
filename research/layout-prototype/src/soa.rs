//! Variant 1, the hypothesis: structure of arrays `[var][k][j][i]`, with row stencils. A y- or
//! z-sweep reads three rows (j-1, j, j+1 or k-1, k, k+1), each contiguous in i; nothing is
//! transposed or copied into pencils.

use crate::Block;
use crate::grid::{self, Dims, Dir, Shape, row, row_mut, sub, sub_mut};
use crate::kernels::{self, RowFailure, Rows, RowsMut};
use crate::timed;
use crate::timers::Phase;

type Vars = [Box<[f64]>; 5];

/// Allocates NaN-filled arrays: the pages are touched at init, and a read of a cell that was
/// never written trips the backstop.
fn poisoned(len: usize) -> Vars {
    assert!(len > 0, "arrays must be non-empty");
    core::array::from_fn(|_| vec![f64::NAN; len].into_boxed_slice())
}

fn refs(vars: &Vars) -> Rows<'_> {
    vars.each_ref().map(|v| &**v)
}

fn muts(vars: &mut Vars) -> RowsMut<'_> {
    vars.each_mut().map(|v| &mut **v)
}

/// Row scratch, allocated once at init.
struct Scratch {
    minus_x: Vars,
    plus_x: Vars,
    flux_x: Vars,
    minus: Vars,
    plus: Vars,
    plus_prev: Vars,
    flux: Vars,
    flux_prev: Vars,
}

pub struct Soa<D: Dims = Shape> {
    shape: D,
    state: Vars,
    stage: Vars,
    div: Vars,
    prim: Vars,
    scratch: Scratch,
}

impl<D: Dims> Block for Soa<D> {
    const NAME: &'static str = D::SOA_NAME;

    fn new(shape: Shape, canonical: &[f64]) -> Self {
        let shape = D::from_shape(shape);
        assert_eq!(canonical.len(), 5 * shape.cells(), "canonical state has five variables");
        let n = shape.n();
        let mut state = poisoned(shape.cells());
        for (dst, src) in state.iter_mut().zip(canonical.chunks_exact(shape.cells())) {
            dst.copy_from_slice(src);
        }
        let scratch = Scratch {
            minus_x: poisoned(n + 2),
            plus_x: poisoned(n + 2),
            flux_x: poisoned(n + 1),
            minus: poisoned(n),
            plus: poisoned(n),
            plus_prev: poisoned(n),
            flux: poisoned(n),
            flux_prev: poisoned(n),
        };
        let block = Self {
            shape,
            state,
            stage: poisoned(shape.cells()),
            div: poisoned(shape.cells()),
            prim: poisoned(shape.padded_cells()),
            scratch,
        };
        assert_eq!(block.prim[0].len(), shape.padded_cells(), "primitives include ghosts");
        block
    }

    fn prepare(&mut self) -> f64 {
        timed!(Phase::Convert, { convert::<true, D>(&self.state, &mut self.prim, self.shape) })
    }

    fn advance(&mut self, dt: f64) {
        assert!(dt > 0.0 && dt.is_finite(), "dt must be positive and finite");
        self.divergence();
        timed!(Phase::Update, {
            for ((u, d), s) in self.state.iter().zip(&self.div).zip(self.stage.iter_mut()) {
                kernels::heun_first(u, d, s, dt);
            }
        });
        let _speed =
            timed!(Phase::Convert, { convert::<false, D>(&self.stage, &mut self.prim, self.shape) });
        self.divergence();
        timed!(Phase::Update, {
            for ((u, s), d) in self.state.iter_mut().zip(&self.stage).zip(&self.div) {
                kernels::heun_second(u, s, d, dt);
            }
        });
    }

    fn canonical(&self) -> Box<[f64]> {
        let all: Vec<f64> = self.state.iter().flat_map(|v| v.iter().copied()).collect();
        assert_eq!(all.len(), 5 * self.shape.cells(), "canonical state has five variables");
        all.into_boxed_slice()
    }
}

impl<D: Dims> Soa<D> {
    /// Ghost fill, then the flux divergence of the primitives into `div`. A row that failed its
    /// deferred face checks ends the step with an assertion here (E2) or in the sweep (E).
    fn divergence(&mut self) {
        let result = self.sweeps();
        if let Err(failure) = result {
            panic!("{failure:?}: a face failed its sqrt checks (deferred, returned as Err)");
        }
    }

    fn sweeps(&mut self) -> Result<(), RowFailure> {
        timed!(Phase::Ghost, { fill_ghosts(&mut self.prim, self.shape) });
        sweep_x(&self.prim, &mut self.div, &mut self.scratch, self.shape)?;
        sweep_transverse(Dir::Y, &self.prim, &mut self.div, &mut self.scratch, self.shape)?;
        sweep_transverse(Dir::Z, &self.prim, &mut self.div, &mut self.scratch, self.shape)
    }
}

/// Conserved to primitive over the interior, with the per-block backstop.
fn convert<const SPEED: bool, D: Dims>(state: &Vars, prim: &mut Vars, shape: D) -> f64 {
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let mut speed = 0.0_f64;
    let mut physical = true;
    for k in 0..n {
        for j in 0..n {
            let u = state.each_ref().map(|v| row(v, n, k * n + j));
            let w = prim.each_mut().map(|v| sub_mut(row_mut(v, p, (k + g) * p + j + g), g, n));
            let (row_speed, row_physical) = kernels::convert_row::<SPEED>(u, w);
            speed = speed.max(row_speed);
            physical &= row_physical;
        }
    }
    assert!(physical, "backstop: non-physical or non-finite state in the block");
    speed
}

fn fill_ghosts<D: Dims>(prim: &mut Vars, shape: D) {
    let (n, p) = (shape.n(), shape.padded());
    for v in prim.iter_mut() {
        assert_eq!(v.len(), p * p * p, "primitives include ghosts");
        for r in v.chunks_exact_mut(p) {
            grid::periodic_fill(r, 1, n);
        }
        for plane in v.chunks_exact_mut(p * p) {
            grid::periodic_fill(plane, p, n);
        }
        grid::periodic_fill(v, p * p, n);
    }
}

#[inline(never)]
fn sweep_x<D: Dims>(
    prim: &Vars,
    div: &mut Vars,
    s: &mut Scratch,
    shape: D,
) -> Result<(), RowFailure> {
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let inv_dx = 1.0 / shape.dx();
    for k in 0..n {
        for j in 0..n {
            let w = prim.each_ref().map(|v| row(v, p, (k + g) * p + j + g));
            timed!(Phase::Reconstruct, {
                for ((src, m), pl) in w.into_iter().zip(s.minus_x.iter_mut()).zip(&mut s.plus_x) {
                    kernels::plm_along(src, m, pl);
                }
            });
            let left = s.plus_x.each_ref().map(|r| sub(r, 0, n + 1));
            let right = s.minus_x.each_ref().map(|r| sub(r, 1, n + 1));
            let ok = timed!(Phase::Riemann, { kernels::riemann_row(left, right, muts(&mut s.flux_x)) });
            kernels::check_row(ok, Dir::X, (j, k))?;
            let d = div.each_mut().map(|v| row_mut(v, n, k * n + j));
            timed!(Phase::FluxDiff, {
                for (f, dv) in s.flux_x.iter().zip(d) {
                    kernels::diff_set(f, dv, inv_dx);
                }
            });
        }
    }
    Ok(())
}

/// A y- or z-sweep over row stencils. For each line `other` it walks the sweep index, keeps the
/// previous cell's plus-face values and the previous face's flux, and so touches each row once.
#[inline(never)]
fn sweep_transverse<D: Dims>(
    dir: Dir,
    prim: &Vars,
    div: &mut Vars,
    s: &mut Scratch,
    shape: D,
) -> Result<(), RowFailure> {
    assert!(dir != Dir::X, "x is not a transverse sweep");
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let inv_dx = 1.0 / shape.dx();
    for other in 0..n {
        let stencil = |at: usize| {
            dir.permute(
                prim.each_ref().map(|v| sub(row(v, p, dir.padded_row(other, at, shape)), g, n)),
            )
        };
        for centre in 1..=n + 2 {
            let (l, c, r) = (stencil(centre - 1), stencil(centre), stencil(centre + 1));
            timed!(Phase::Reconstruct, {
                let out = s.minus.iter_mut().zip(s.plus.iter_mut());
                for (((l, c), r), (m, pl)) in l.into_iter().zip(c).zip(r).zip(out) {
                    kernels::plm_across(l, c, r, m, pl);
                }
            });
            if centre >= 2 {
                let ok = timed!(Phase::Riemann, {
                    kernels::riemann_row(refs(&s.plus_prev), refs(&s.minus), muts(&mut s.flux))
                });
                kernels::check_row(ok, dir, (other, centre - 2))?;
                if centre >= 3 {
                    let row_index = dir.interior_row(other, centre - 3, shape);
                    let d = dir.permute(div.each_mut().map(|v| row_mut(v, n, row_index)));
                    timed!(Phase::FluxDiff, {
                        for ((m, pl), dv) in s.flux_prev.iter().zip(&s.flux).zip(d) {
                            kernels::diff_add(m, pl, dv, inv_dx);
                        }
                    });
                }
                core::mem::swap(&mut s.flux, &mut s.flux_prev);
            }
            core::mem::swap(&mut s.plus, &mut s.plus_prev);
        }
    }
    Ok(())
}
