//! Variant 3: a hybrid (AoSoA). Cells are grouped by `W` along i, and each group stores its five
//! variables as `W`-lane arrays: `[k][j][i / W][var][i % W]`. Transverse sweeps work group by
//! group on the lanes. The x-sweep needs neighbours across group boundaries, so it unpacks one
//! padded row into plain rows, runs the SoA row kernels, and packs the result back.
//!
//! The ghosted primitives pad i by a whole group on each side, so interior groups stay aligned;
//! only the last two lanes of the low pad and the first two of the high pad are used.

use crate::Block;
use crate::grid::{self, Dir, Shape, row, row_mut, sub, sub_mut};
use crate::kernels::{self, Rows, RowsMut};
use crate::timed;
use crate::timers::Phase;

type Group<const W: usize> = [[f64; W]; 5];
type Vars = [Box<[f64]>; 5];

fn groups<const W: usize>(len: usize) -> Box<[Group<W>]> {
    assert!(len > 0, "arrays must be non-empty");
    vec![[[f64::NAN; W]; 5]; len].into_boxed_slice()
}

fn rows(len: usize) -> Vars {
    assert!(len > 0, "arrays must be non-empty");
    core::array::from_fn(|_| vec![f64::NAN; len].into_boxed_slice())
}

#[inline(always)]
fn lanes<const W: usize>(g: &Group<W>) -> Rows<'_> {
    g.each_ref().map(|lane| lane.as_slice())
}

#[inline(always)]
fn lanes_mut<const W: usize>(g: &mut Group<W>) -> RowsMut<'_> {
    g.each_mut().map(|lane| lane.as_mut_slice())
}

struct Scratch<const W: usize> {
    unpacked: Vars,
    minus_x: Vars,
    plus_x: Vars,
    flux_x: Vars,
    div_x: Vars,
    minus: Box<[Group<W>]>,
    plus: Box<[Group<W>]>,
    plus_prev: Box<[Group<W>]>,
    flux: Box<[Group<W>]>,
    flux_prev: Box<[Group<W>]>,
}

pub struct Hybrid<const W: usize> {
    shape: Shape,
    state: Box<[Group<W>]>,
    stage: Box<[Group<W>]>,
    div: Box<[Group<W>]>,
    prim: Box<[Group<W>]>,
    scratch: Scratch<W>,
}

impl<const W: usize> Hybrid<W> {
    const LANES_FIT: () = assert!(W >= 2 && 8 % W == 0, "W divides 8 and covers the ghosts");

    /// Groups per interior row, and per padded row.
    fn widths(shape: Shape) -> (usize, usize) {
        let () = Self::LANES_FIT;
        assert!(shape.n() % W == 0, "rows hold whole groups");
        (shape.n() / W, shape.n() / W + 2)
    }

    fn divergence(&mut self) {
        timed!(Phase::Ghost, { fill_ghosts(&mut self.prim, self.shape) });
        sweep_x(&self.prim, &mut self.div, &mut self.scratch, self.shape);
        sweep_transverse(Dir::Y, &self.prim, &mut self.div, &mut self.scratch, self.shape);
        sweep_transverse(Dir::Z, &self.prim, &mut self.div, &mut self.scratch, self.shape);
    }
}

impl<const W: usize> Block for Hybrid<W> {
    const NAME: &'static str = if W == 4 { "hybrid4" } else { "hybrid8" };

    fn new(shape: Shape, canonical: &[f64]) -> Self {
        let cells = shape.cells();
        assert_eq!(canonical.len(), 5 * cells, "canonical state has five variables");
        let (gn, gp) = Self::widths(shape);
        let (n, p) = (shape.n(), shape.padded());
        let mut state = groups::<W>(cells / W);
        for (v, src) in canonical.chunks_exact(cells).enumerate() {
            for (g, chunk) in state.iter_mut().zip(src.chunks_exact(W)) {
                let Some(lane) = g.get_mut(v) else { panic!("variable {v} out of range") };
                lane.copy_from_slice(chunk);
            }
        }
        let scratch = Scratch {
            unpacked: rows(gp * W),
            minus_x: rows(n + 2),
            plus_x: rows(n + 2),
            flux_x: rows(n + 1),
            div_x: rows(n),
            minus: groups(gn),
            plus: groups(gn),
            plus_prev: groups(gn),
            flux: groups(gn),
            flux_prev: groups(gn),
        };
        Self {
            shape,
            state,
            stage: groups(cells / W),
            div: groups(cells / W),
            prim: groups(gp * p * p),
            scratch,
        }
    }

    fn prepare(&mut self) -> f64 {
        timed!(Phase::Convert, { convert::<W, true>(&self.state, &mut self.prim, self.shape) })
    }

    fn advance(&mut self, dt: f64) {
        assert!(dt > 0.0 && dt.is_finite(), "dt must be positive and finite");
        self.divergence();
        timed!(Phase::Update, {
            let (u, d) = (self.state.as_flattened().as_flattened(), self.div.as_flattened());
            kernels::heun_first(
                u,
                d.as_flattened(),
                self.stage.as_flattened_mut().as_flattened_mut(),
                dt,
            );
        });
        let _speed = timed!(Phase::Convert, {
            convert::<W, false>(&self.stage, &mut self.prim, self.shape)
        });
        self.divergence();
        timed!(Phase::Update, {
            let (s, d) = (self.stage.as_flattened().as_flattened(), self.div.as_flattened());
            kernels::heun_second(
                self.state.as_flattened_mut().as_flattened_mut(),
                s,
                d.as_flattened(),
                dt,
            );
        });
    }

    fn canonical(&self) -> Box<[f64]> {
        let cells = self.shape.cells();
        let mut all = vec![0.0; 5 * cells].into_boxed_slice();
        for (v, out) in all.chunks_exact_mut(cells).enumerate() {
            for (chunk, g) in out.chunks_exact_mut(W).zip(self.state.iter()) {
                let Some(lane) = g.get(v) else { panic!("variable {v} out of range") };
                chunk.copy_from_slice(lane);
            }
        }
        all
    }
}

fn convert<const W: usize, const SPEED: bool>(
    state: &[Group<W>],
    prim: &mut [Group<W>],
    shape: Shape,
) -> f64 {
    let (gn, gp) = Hybrid::<W>::widths(shape);
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let mut speed = 0.0_f64;
    let mut physical = true;
    for k in 0..n {
        for j in 0..n {
            let u = row(state, gn, k * n + j);
            let w = sub_mut(row_mut(prim, gp, (k + g) * p + j + g), 1, gn);
            for (ug, wg) in u.iter().zip(w.iter_mut()) {
                let (s, ok) = kernels::convert_row::<SPEED>(lanes(ug), lanes_mut(wg));
                speed = speed.max(s);
                physical &= ok;
            }
        }
    }
    assert!(physical, "backstop: non-physical or non-finite state in the block");
    speed
}

/// x ghosts: the last two lanes of the low pad group and the first two of the high pad group.
fn fill_ghosts<const W: usize>(prim: &mut [Group<W>], shape: Shape) {
    let (_, gp) = Hybrid::<W>::widths(shape);
    let (n, p) = (shape.n(), shape.padded());
    assert_eq!(prim.len(), gp * p * p, "primitives include ghosts");
    for r in prim.chunks_exact_mut(gp) {
        let Some((low, rest)) = r.split_first_mut() else { panic!("row has a low pad") };
        let Some((high, interior)) = rest.split_last_mut() else { panic!("row has a high pad") };
        let (Some(first), Some(last)) = (interior.first(), interior.last()) else {
            panic!("row has interior groups");
        };
        for (((lo, hi), fi), la) in low.iter_mut().zip(high.iter_mut()).zip(first).zip(last) {
            let (Some(lo_tail), Some(la_tail)) = (lo.last_chunk_mut::<2>(), la.last_chunk::<2>())
            else {
                panic!("groups hold the two ghost lanes");
            };
            *lo_tail = *la_tail;
            let (Some(hi_head), Some(fi_head)) = (hi.first_chunk_mut::<2>(), fi.first_chunk::<2>())
            else {
                panic!("groups hold the two ghost lanes");
            };
            *hi_head = *fi_head;
        }
    }
    for plane in prim.chunks_exact_mut(gp * p) {
        grid::periodic_fill(plane, gp, n);
    }
    grid::periodic_fill(prim, gp * p, n);
}

#[inline(never)]
fn sweep_x<const W: usize>(
    prim: &[Group<W>],
    div: &mut [Group<W>],
    s: &mut Scratch<W>,
    shape: Shape,
) {
    let (gn, gp) = Hybrid::<W>::widths(shape);
    let (n, p, g) = (shape.n(), shape.padded(), shape.ghost());
    let inv_dx = 1.0 / shape.dx();
    for k in 0..n {
        for j in 0..n {
            let padded = row(prim, gp, (k + g) * p + j + g);
            timed!(Phase::Repack, { unpack(padded, &mut s.unpacked) });
            let w = s.unpacked.each_ref().map(|r| sub(r, W - g, n + 2 * g));
            timed!(Phase::Reconstruct, {
                for ((src, m), pl) in w.into_iter().zip(s.minus_x.iter_mut()).zip(&mut s.plus_x) {
                    kernels::plm_along(src, m, pl);
                }
            });
            let left = s.plus_x.each_ref().map(|r| sub(r, 0, n + 1));
            let right = s.minus_x.each_ref().map(|r| sub(r, 1, n + 1));
            let flux = s.flux_x.each_mut().map(|r| &mut **r);
            timed!(Phase::Riemann, { kernels::riemann_row(left, right, flux) });
            timed!(Phase::FluxDiff, {
                for (f, dv) in s.flux_x.iter().zip(s.div_x.iter_mut()) {
                    kernels::diff_set(f, dv, inv_dx);
                }
            });
            let d = row_mut(div, gn, k * n + j);
            timed!(Phase::Repack, { pack(&s.div_x, d) });
        }
    }
}

fn unpack<const W: usize>(groups: &[Group<W>], out: &mut Vars) {
    for o in out.iter() {
        assert_eq!(o.len(), groups.len() * W, "unpacked rows hold every lane");
    }
    for (v, o) in out.iter_mut().enumerate() {
        for (chunk, g) in o.chunks_exact_mut(W).zip(groups) {
            let Some(lane) = g.get(v) else { panic!("variable {v} out of range") };
            chunk.copy_from_slice(lane);
        }
    }
}

fn pack<const W: usize>(src: &Vars, groups: &mut [Group<W>]) {
    for r in src {
        assert_eq!(r.len(), groups.len() * W, "packed rows hold every lane");
    }
    for (v, r) in src.iter().enumerate() {
        for (chunk, g) in r.chunks_exact(W).zip(groups.iter_mut()) {
            let Some(lane) = g.get_mut(v) else { panic!("variable {v} out of range") };
            lane.copy_from_slice(chunk);
        }
    }
}

#[inline(never)]
fn sweep_transverse<const W: usize>(
    dir: Dir,
    prim: &[Group<W>],
    div: &mut [Group<W>],
    s: &mut Scratch<W>,
    shape: Shape,
) {
    assert!(dir != Dir::X, "x is not a transverse sweep");
    let (gn, gp) = Hybrid::<W>::widths(shape);
    let n = shape.n();
    let inv_dx = 1.0 / shape.dx();
    for other in 0..n {
        let stencil = |at: usize| sub(row(prim, gp, dir.padded_row(other, at, shape)), 1, gn);
        for centre in 1..=n + 2 {
            let (l, c, r) = (stencil(centre - 1), stencil(centre), stencil(centre + 1));
            timed!(Phase::Reconstruct, { reconstruct_groups(l, c, r, &mut s.minus, &mut s.plus) });
            if centre >= 2 {
                timed!(Phase::Riemann, {
                    let faces = s.plus_prev.iter().zip(s.minus.iter()).zip(s.flux.iter_mut());
                    for ((pp, m), f) in faces {
                        let out = dir.permute(lanes_mut(f));
                        riemann_lanes(dir.permute(lanes(pp)), dir.permute(lanes(m)), out);
                    }
                });
                if centre >= 3 {
                    let d = row_mut(div, gn, dir.interior_row(other, centre - 3, shape));
                    timed!(Phase::FluxDiff, { diff_groups(&s.flux_prev, &s.flux, d, inv_dx) });
                }
                core::mem::swap(&mut s.flux, &mut s.flux_prev);
            }
            core::mem::swap(&mut s.plus, &mut s.plus_prev);
        }
    }
}

#[inline(always)]
fn reconstruct_groups<const W: usize>(
    l: &[Group<W>],
    c: &[Group<W>],
    r: &[Group<W>],
    minus: &mut [Group<W>],
    plus: &mut [Group<W>],
) {
    assert!(l.len() == c.len() && r.len() == c.len(), "stencil rows match");
    assert!(minus.len() == c.len() && plus.len() == c.len(), "outputs match the stencil");
    for ((((gl, gc), gr), gm), gpl) in
        l.iter().zip(c).zip(r).zip(minus.iter_mut()).zip(plus.iter_mut())
    {
        let out = gm.iter_mut().zip(gpl.iter_mut());
        for (((vl, vc), vr), (vm, vp)) in gl.iter().zip(gc).zip(gr).zip(out) {
            plm_lanes(vl, vc, vr, vm, vp);
        }
    }
}

#[inline(always)]
fn diff_groups<const W: usize>(
    minus: &[Group<W>],
    plus: &[Group<W>],
    div: &mut [Group<W>],
    inv_dx: f64,
) {
    assert!(minus.len() == div.len() && plus.len() == div.len(), "face rows match cell row");
    for ((gm, gpl), gd) in minus.iter().zip(plus).zip(div.iter_mut()) {
        for ((vm, vp), vd) in gm.iter().zip(gpl).zip(gd.iter_mut()) {
            kernels::diff_add(vm, vp, vd, inv_dx);
        }
    }
}

/// Per-group Riemann call. With the `outlined-lanes` feature it goes through a call boundary.
#[inline(always)]
fn riemann_lanes(left: Rows<'_>, right: Rows<'_>, flux: RowsMut<'_>) {
    if cfg!(feature = "outlined-lanes") {
        kernels::riemann_row_outlined(left, right, flux);
    } else {
        kernels::riemann_row(left, right, flux);
    }
}

/// Per-group PLM call. With the `outlined-lanes` feature it goes through a call boundary.
#[inline(always)]
fn plm_lanes(l: &[f64], c: &[f64], r: &[f64], minus: &mut [f64], plus: &mut [f64]) {
    if cfg!(feature = "outlined-lanes") {
        kernels::plm_across_outlined(l, c, r, minus, plus);
    } else {
        kernels::plm_across(l, c, r, minus, plus);
    }
}
