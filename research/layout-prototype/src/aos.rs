//! Variant 2: array of structures, one `[f64; 5]` per cell, laid out `[k][j][i][var]`. The loop
//! structure is the same as in `soa`; only the layout differs.

use crate::Block;
use crate::grid::{self, Dir, Shape, row, row_mut, sub, sub_mut};
use crate::kernels::{self, zip5};
use crate::physics::{self, Conserved, Primitive};
use crate::timed;
use crate::timers::Phase;

type Cell = [f64; 5];

/// Allocates NaN-filled arrays: the pages are touched at init, and a read of a cell that was
/// never written trips the backstop.
fn poisoned(len: usize) -> Box<[Cell]> {
    assert!(len > 0, "arrays must be non-empty");
    vec![[f64::NAN; 5]; len].into_boxed_slice()
}

struct Scratch {
    minus_x: Box<[Cell]>,
    plus_x: Box<[Cell]>,
    flux_x: Box<[Cell]>,
    minus: Box<[Cell]>,
    plus: Box<[Cell]>,
    plus_prev: Box<[Cell]>,
    flux: Box<[Cell]>,
    flux_prev: Box<[Cell]>,
}

pub struct Aos {
    shape: Shape,
    state: Box<[Cell]>,
    stage: Box<[Cell]>,
    div: Box<[Cell]>,
    prim: Box<[Cell]>,
    scratch: Scratch,
}

impl Block for Aos {
    const NAME: &'static str = "aos";

    fn new(shape: Shape, canonical: &[f64]) -> Self {
        let cells = shape.cells();
        assert_eq!(canonical.len(), 5 * cells, "canonical state has five variables");
        let mut state = poisoned(cells);
        let [a, b, c, d, e] = core::array::from_fn(|v| sub(canonical, v * cells, cells));
        for (cell, values) in state.iter_mut().zip(zip5([a, b, c, d, e])) {
            *cell = values;
        }
        let n = shape.n();
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
            stage: poisoned(cells),
            div: poisoned(cells),
            prim: poisoned(shape.padded_cells()),
            scratch,
        };
        assert_eq!(block.prim.len(), shape.padded_cells(), "primitives include ghosts");
        block
    }

    fn prepare(&mut self) -> f64 {
        timed!(Phase::Convert, { convert::<true>(&self.state, &mut self.prim, self.shape) })
    }

    fn advance(&mut self, dt: f64) {
        assert!(dt > 0.0 && dt.is_finite(), "dt must be positive and finite");
        self.divergence();
        timed!(Phase::Update, {
            kernels::heun_first(
                self.state.as_flattened(),
                self.div.as_flattened(),
                self.stage.as_flattened_mut(),
                dt,
            );
        });
        let _speed =
            timed!(Phase::Convert, { convert::<false>(&self.stage, &mut self.prim, self.shape) });
        self.divergence();
        timed!(Phase::Update, {
            kernels::heun_second(
                self.state.as_flattened_mut(),
                self.stage.as_flattened(),
                self.div.as_flattened(),
                dt,
            );
        });
    }

    fn canonical(&self) -> Box<[f64]> {
        let mut all = vec![0.0; 5 * self.shape.cells()].into_boxed_slice();
        for (v, out) in all.chunks_exact_mut(self.shape.cells()).enumerate() {
            for (o, cell) in out.iter_mut().zip(self.state.iter()) {
                let Some(value) = cell.get(v) else { panic!("variable {v} out of range") };
                *o = *value;
            }
        }
        all
    }
}

impl Aos {
    fn divergence(&mut self) {
        timed!(Phase::Ghost, { fill_ghosts(&mut self.prim, self.shape) });
        sweep_x(&self.prim, &mut self.div, &mut self.scratch, self.shape);
        sweep_transverse(Dir::Y, &self.prim, &mut self.div, &mut self.scratch, self.shape);
        sweep_transverse(Dir::Z, &self.prim, &mut self.div, &mut self.scratch, self.shape);
    }
}

fn convert<const SPEED: bool>(state: &[Cell], prim: &mut [Cell], shape: Shape) -> f64 {
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let mut speed = 0.0_f64;
    let mut physical = true;
    for k in 0..n {
        for j in 0..n {
            let u = row(state, n, k * n + j);
            let w = sub_mut(row_mut(prim, p, (k + g) * p + j + g), g, n);
            assert_eq!(u.len(), w.len(), "rows must match");
            for (cons, out) in u.iter().zip(w.iter_mut()) {
                let state = physics::primitive_from_conserved(Conserved::from_vars(*cons));
                physical &= state.is_physical();
                if SPEED {
                    speed = speed.max(physics::max_signal_speed(state));
                }
                *out = state.to_vars();
            }
        }
    }
    assert!(physical, "backstop: non-physical or non-finite state in the block");
    speed
}

fn fill_ghosts(prim: &mut [Cell], shape: Shape) {
    let (n, p) = (shape.n(), shape.padded());
    assert_eq!(prim.len(), p * p * p, "primitives include ghosts");
    for r in prim.chunks_exact_mut(p) {
        grid::periodic_fill(r, 1, n);
    }
    for plane in prim.chunks_exact_mut(p * p) {
        grid::periodic_fill(plane, p, n);
    }
    grid::periodic_fill(prim, p * p, n);
}

#[inline(always)]
fn plm_cell(l: Cell, c: Cell, r: Cell) -> (Cell, Cell) {
    let mut minus = [0.0; 5];
    let mut plus = [0.0; 5];
    let out = minus.iter_mut().zip(plus.iter_mut());
    for ((((m, p), l), c), r) in out.zip(l).zip(c).zip(r) {
        [*m, *p] = physics::plm(l, c, r);
    }
    (minus, plus)
}

#[inline(always)]
fn riemann_cells(dir: Dir, left: &[Cell], right: &[Cell], flux: &mut [Cell]) {
    assert!(left.len() == flux.len() && right.len() == flux.len(), "face rows must match");
    for ((wl, wr), out) in left.iter().zip(right).zip(flux.iter_mut()) {
        let l = Primitive::from_vars(dir.permute(*wl));
        let r = Primitive::from_vars(dir.permute(*wr));
        let f = physics::hllc(l, r);
        debug_assert!(f.density.is_finite(), "flux must be finite");
        kernels::store5(dir.permute(out.each_mut()), f.to_vars());
    }
}

#[inline(never)]
fn sweep_x(prim: &[Cell], div: &mut [Cell], s: &mut Scratch, shape: Shape) {
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let inv_dx = 1.0 / shape.dx();
    for k in 0..n {
        for j in 0..n {
            let w = row(prim, p, (k + g) * p + j + g);
            timed!(Phase::Reconstruct, {
                assert_eq!(s.minus_x.len(), w.len() - 2, "one face pair per inner cell");
                let out = s.minus_x.iter_mut().zip(s.plus_x.iter_mut());
                for ([l, c, r], (m, pl)) in w.array_windows::<3>().zip(out) {
                    (*m, *pl) = plm_cell(*l, *c, *r);
                }
            });
            let left = sub(&s.plus_x, 0, n + 1);
            let right = sub(&s.minus_x, 1, n + 1);
            timed!(Phase::Riemann, { riemann_cells(Dir::X, left, right, &mut s.flux_x) });
            let d = row_mut(div, n, k * n + j);
            timed!(Phase::FluxDiff, {
                assert_eq!(s.flux_x.len(), d.len() + 1, "one more face than cells");
                for (dv, [fm, fp]) in d.iter_mut().zip(s.flux_x.array_windows::<2>()) {
                    for ((dv, m), p) in dv.iter_mut().zip(fm).zip(fp) {
                        *dv = (p - m) * inv_dx;
                    }
                }
            });
        }
    }
}

#[inline(never)]
fn sweep_transverse(dir: Dir, prim: &[Cell], div: &mut [Cell], s: &mut Scratch, shape: Shape) {
    assert!(dir != Dir::X, "x is not a transverse sweep");
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let inv_dx = 1.0 / shape.dx();
    for other in 0..n {
        let stencil = |at: usize| sub(row(prim, p, dir.padded_row(other, at, shape)), g, n);
        for centre in 1..=n + 2 {
            let (l, c, r) = (stencil(centre - 1), stencil(centre), stencil(centre + 1));
            timed!(Phase::Reconstruct, {
                assert!(l.len() == n && c.len() == n && r.len() == n, "stencil rows match");
                let out = s.minus.iter_mut().zip(s.plus.iter_mut());
                for (((l, c), r), (m, pl)) in l.iter().zip(c).zip(r).zip(out) {
                    (*m, *pl) = plm_cell(*l, *c, *r);
                }
            });
            if centre >= 2 {
                timed!(Phase::Riemann, { riemann_cells(dir, &s.plus_prev, &s.minus, &mut s.flux) });
                if centre >= 3 {
                    let d = row_mut(div, n, dir.interior_row(other, centre - 3, shape));
                    timed!(Phase::FluxDiff, {
                        assert_eq!(d.len(), s.flux.len(), "face rows match the cell row");
                        for ((dv, fm), fp) in d.iter_mut().zip(&s.flux_prev).zip(&s.flux) {
                            for ((dv, m), p) in dv.iter_mut().zip(fm).zip(fp) {
                                *dv += (p - m) * inv_dx;
                            }
                        }
                    });
                }
                core::mem::swap(&mut s.flux, &mut s.flux_prev);
            }
            core::mem::swap(&mut s.plus, &mut s.plus_prev);
        }
    }
}
