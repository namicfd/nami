//! Row kernels over contiguous slices, shared by the layouts. A "row" is any contiguous run of
//! cells along i: a whole row in SoA, or the lanes of one group in the hybrid. Each kernel asserts
//! its lengths before the loop and reads through iterator zips, never by index (ADR-0003).

use crate::grid::Dir;
use crate::physics::{self, Conserved, Primitive};

/// Five parallel rows, one per variable.
pub type Rows<'a> = [&'a [f64]; 5];
/// Five parallel mutable rows, one per variable.
pub type RowsMut<'a> = [&'a mut [f64]; 5];

/// Iterates five rows in lockstep, yielding one cell's five values.
#[inline(always)]
pub fn zip5<'a>(rows: Rows<'a>) -> impl Iterator<Item = [f64; 5]> + 'a {
    let [a, b, c, d, e] = rows;
    a.iter().zip(b).zip(c).zip(d).zip(e).map(|((((a, b), c), d), e)| [*a, *b, *c, *d, *e])
}

/// Iterates five mutable rows in lockstep.
#[inline(always)]
pub fn zip5_mut<'a>(rows: RowsMut<'a>) -> impl Iterator<Item = [&'a mut f64; 5]> + 'a {
    let [a, b, c, d, e] = rows;
    a.iter_mut()
        .zip(b.iter_mut())
        .zip(c.iter_mut())
        .zip(d.iter_mut())
        .zip(e.iter_mut())
        .map(|((((a, b), c), d), e)| [a, b, c, d, e])
}

#[inline(always)]
pub fn store5(out: [&mut f64; 5], values: [f64; 5]) {
    let [a, b, c, d, e] = out;
    let [va, vb, vc, vd, ve] = values;
    *a = va;
    *b = vb;
    *c = vc;
    *d = vd;
    *e = ve;
}

#[inline(always)]
pub fn assert_rows_len(rows: &Rows<'_>, len: usize) {
    for r in rows {
        assert_eq!(r.len(), len, "rows must have equal length");
    }
}

#[inline(always)]
pub fn assert_rows_mut_len(rows: &RowsMut<'_>, len: usize) {
    for r in rows {
        assert_eq!(r.len(), len, "rows must have equal length");
    }
}

/// Conserved to primitive over a row, with the backstop flag and (if `SPEED`) the largest signal
/// speed. Returns `(max_speed, all_physical)`.
#[inline(always)]
pub fn convert_row<const SPEED: bool>(u: Rows<'_>, w: RowsMut<'_>) -> (f64, bool) {
    let len = u[0].len();
    assert_rows_len(&u, len);
    assert_rows_mut_len(&w, len);
    let mut speed = 0.0_f64;
    let mut physical = true;
    for (cons, prim) in zip5(u).zip(zip5_mut(w)) {
        let state = physics::primitive_from_conserved(Conserved::from_vars(cons));
        physical &= state.is_physical();
        if SPEED {
            speed = speed.max(physics::max_signal_speed(state, &mut physical));
        }
        store5(prim, state.to_vars());
    }
    (speed, physical)
}

/// PLM along a row: `minus[c]`/`plus[c]` are the face values of cell `c + 1` of `src`.
#[inline(always)]
pub fn plm_along(src: &[f64], minus: &mut [f64], plus: &mut [f64]) {
    assert!(src.len() >= 3, "PLM needs three cells");
    assert_eq!(minus.len(), src.len() - 2, "minus row must have one entry per inner cell");
    assert_eq!(plus.len(), minus.len(), "plus row must match the minus row");
    for ([l, c, r], (m, p)) in src.array_windows::<3>().zip(minus.iter_mut().zip(plus.iter_mut())) {
        [*m, *p] = physics::plm(*l, *c, *r);
    }
}

/// PLM across three parallel rows (a transverse sweep).
#[inline(always)]
pub fn plm_across(l: &[f64], c: &[f64], r: &[f64], minus: &mut [f64], plus: &mut [f64]) {
    let len = c.len();
    assert!(l.len() == len && r.len() == len, "stencil rows must have equal length");
    assert!(minus.len() == len && plus.len() == len, "output rows must match the stencil");
    for (((l, c), r), (m, p)) in l.iter().zip(c).zip(r).zip(minus.iter_mut().zip(plus.iter_mut())) {
        [*m, *p] = physics::plm(*l, *c, *r);
    }
}

/// HLLC over a row of faces. The rows are already permuted so the normal velocity comes first.
/// Returns whether every face passed its deferred checks (always true unless `deferred-checks`).
#[inline(always)]
#[must_use]
pub fn riemann_row(left: Rows<'_>, right: Rows<'_>, flux: RowsMut<'_>) -> bool {
    let len = flux[0].len();
    assert_rows_len(&left, len);
    assert_rows_len(&right, len);
    assert_rows_mut_len(&flux, len);
    let mut ok = true;
    for ((wl, wr), out) in zip5(left).zip(zip5(right)).zip(zip5_mut(flux)) {
        let f = physics::hllc(Primitive::from_vars(wl), Primitive::from_vars(wr), &mut ok);
        debug_assert!(f.density.is_finite(), "flux must be finite");
        store5(out, f.to_vars());
    }
    ok
}

/// First sweep direction: `div[c] = (flux[c+1] - flux[c]) / dx`.
#[inline(always)]
pub fn diff_set(flux: &[f64], div: &mut [f64], inv_dx: f64) {
    assert_eq!(flux.len(), div.len() + 1, "one more face than cells");
    for (d, [minus, plus]) in div.iter_mut().zip(flux.array_windows::<2>()) {
        *d = (plus - minus) * inv_dx;
    }
}

/// Later sweep directions: `div += (plus - minus) / dx`, with the faces in two rows.
#[inline(always)]
pub fn diff_add(minus: &[f64], plus: &[f64], div: &mut [f64], inv_dx: f64) {
    let len = div.len();
    assert!(minus.len() == len && plus.len() == len, "face rows must match the cell row");
    for ((d, m), p) in div.iter_mut().zip(minus).zip(plus) {
        *d += (p - m) * inv_dx;
    }
}

/// First Heun stage: `stage = state - dt * div`.
#[inline(always)]
pub fn heun_first(state: &[f64], div: &[f64], stage: &mut [f64], dt: f64) {
    assert!(state.len() == div.len() && stage.len() == div.len(), "update arrays must match");
    assert!(dt > 0.0 && dt.is_finite(), "dt must be positive and finite");
    for ((s, u), d) in stage.iter_mut().zip(state).zip(div) {
        *s = u - dt * d;
    }
}

/// Second Heun stage: `state = 0.5 * state + 0.5 * stage - 0.5 * dt * div`.
#[inline(always)]
pub fn heun_second(state: &mut [f64], stage: &[f64], div: &[f64], dt: f64) {
    assert!(state.len() == div.len() && stage.len() == div.len(), "update arrays must match");
    assert!(dt > 0.0 && dt.is_finite(), "dt must be positive and finite");
    let half_dt = 0.5 * dt;
    for ((u, s), d) in state.iter_mut().zip(stage).zip(div) {
        *u = 0.5 * *u + 0.5 * s - half_dt * d;
    }
}

/// A row whose faces failed their deferred checks (configuration E2 returns it as an error).
#[derive(Clone, Copy, Debug)]
pub struct RowFailure {
    pub sweep: Dir,
    /// The two fixed indices of the row (for x: j, k; for y: k, j; for z: j, k), interior.
    pub row: (usize, usize),
}

/// Ends a row of faces: with `deferred-result` a failed row becomes an `Err` for the caller to
/// propagate; otherwise it is asserted here, once per row.
#[inline(always)]
pub fn check_row(ok: bool, sweep: Dir, row: (usize, usize)) -> Result<(), RowFailure> {
    if cfg!(feature = "deferred-result") {
        if ok { Ok(()) } else { Err(RowFailure { sweep, row }) }
    } else {
        assert!(ok, "{sweep:?}-sweep, row {row:?}: a face failed its sqrt checks (deferred)");
        Ok(())
    }
}

/// Permutes a cell's five values for a sweep in `dir`.
#[inline(always)]
pub fn permute_rows<T>(dir: Dir, rows: [T; 5]) -> [T; 5] {
    dir.permute(rows)
}

/// `riemann_row` behind a call boundary, so that a short fixed-length lane loop (the hybrid's
/// groups) reaches the loop vectoriser as a loop instead of being fully unrolled first.
#[inline(never)]
#[must_use]
pub fn riemann_row_outlined(left: Rows<'_>, right: Rows<'_>, flux: RowsMut<'_>) -> bool {
    riemann_row(left, right, flux)
}

/// `plm_across` behind a call boundary (see `riemann_row_outlined`).
#[inline(never)]
pub fn plm_across_outlined(l: &[f64], c: &[f64], r: &[f64], minus: &mut [f64], plus: &mut [f64]) {
    plm_across(l, c, r, minus, plus);
}

#[cfg(test)]
mod tests {
    use super::{check_row, riemann_row};
    use crate::grid::Dir;

    // Goal: the deferred checks fire. One face of a row of 9 has a negative pressure, so its
    // sound speed takes the sqrt of a negative number; the row must fail as a whole.
    #[test]
    fn a_bad_face_fails_its_row() {
        let density = [1.0; 9];
        let zero = [0.0; 9];
        let mut pressure = [0.6; 9];
        pressure[4] = -0.6;
        let left = [&density[..], &zero[..], &zero[..], &zero[..], &pressure[..]];
        let right = [&density[..], &zero[..], &zero[..], &zero[..], &[0.6; 9][..]];
        let mut flux = [[0.0; 9]; 5];
        let [a, b, c, d, e] = &mut flux;
        let ok = riemann_row(left, right, [&mut a[..], &mut b[..], &mut c[..], &mut d[..], &mut e[..]]);
        assert!(!ok, "a negative sqrt argument must fail the row");
        let caught = std::panic::catch_unwind(|| check_row(ok, Dir::Y, (3, 7)));
        if cfg!(feature = "deferred-result") {
            assert!(matches!(caught, Ok(Err(_))), "E2 returns the failure as an error");
        } else {
            assert!(caught.is_err(), "E asserts the failure once per row");
        }
    }
}
