//! Per-face and per-cell physics of ideal-gas Euler, over named-field state structs.
//!
//! Every layout calls these functions, so the arithmetic is the same in all of them. The HLLC
//! solver and the PLM limiter follow Athena++ v24.0 (`hllc.cpp`, `plm.cpp`) operation by
//! operation, so that both codes do the same work.

use crate::math;

pub const GAMMA: f64 = 5.0 / 3.0;
const GAMMA_MINUS_ONE: f64 = GAMMA - 1.0;
const INV_GAMMA_MINUS_ONE: f64 = 1.0 / GAMMA_MINUS_ONE;
/// (γ + 1) / (2γ), the shock factor of the PVRS wave-speed estimate.
const SHOCK_FACTOR: f64 = (GAMMA + 1.0) / (2.0 * GAMMA);
/// Athena++'s `TINY_NUMBER`, which keeps the HLLC denominators away from zero.
const TINY: f64 = 1.0e-20;

/// Primitive state. In a sweep, `velocity[0]` is the component normal to the face.
#[derive(Clone, Copy, Debug)]
pub struct Primitive {
    pub density: f64,
    pub velocity: [f64; 3],
    pub pressure: f64,
}

/// Conserved state, or a flux of conserved quantities.
#[derive(Clone, Copy, Debug)]
pub struct Conserved {
    pub density: f64,
    pub momentum: [f64; 3],
    pub energy: f64,
}

impl Primitive {
    #[inline(always)]
    pub fn from_vars(vars: [f64; 5]) -> Self {
        let [density, vx, vy, vz, pressure] = vars;
        Self { density, velocity: [vx, vy, vz], pressure }
    }

    #[inline(always)]
    pub fn to_vars(self) -> [f64; 5] {
        let [vx, vy, vz] = self.velocity;
        [self.density, vx, vy, vz, self.pressure]
    }

    /// Density and pressure are positive and finite (the per-block backstop).
    #[inline(always)]
    pub fn is_physical(self) -> bool {
        (self.density > 0.0)
            & (self.density < f64::INFINITY)
            & (self.pressure > 0.0)
            & (self.pressure < f64::INFINITY)
    }
}

impl Conserved {
    #[inline(always)]
    pub fn from_vars(vars: [f64; 5]) -> Self {
        let [density, mx, my, mz, energy] = vars;
        Self { density, momentum: [mx, my, mz], energy }
    }

    #[inline(always)]
    pub fn to_vars(self) -> [f64; 5] {
        let [mx, my, mz] = self.momentum;
        [self.density, mx, my, mz, self.energy]
    }
}

/// Conserved to primitive, as Athena++'s `adiabatic_hydro.cpp` (without its silent floors).
#[inline(always)]
pub fn primitive_from_conserved(u: Conserved) -> Primitive {
    debug_assert!(u.density > 0.0, "density must be positive");
    let inv_density = 1.0 / u.density;
    let [mx, my, mz] = u.momentum;
    let kinetic = 0.5 * inv_density * (mx * mx + my * my + mz * mz);
    let pressure = GAMMA_MINUS_ONE * (u.energy - kinetic);
    debug_assert!(pressure > 0.0, "pressure must be positive");
    Primitive {
        density: u.density,
        velocity: [mx * inv_density, my * inv_density, mz * inv_density],
        pressure,
    }
}

/// The square root of the per-face physics. With `deferred-checks` (configuration E) its domain
/// checks go into `ok`, which the row loop asserts once; otherwise `math::sqrt` asserts per call
/// (or debug-asserts, with `sqrt-debug-asserts`) and `ok` stays true.
#[inline(always)]
fn face_sqrt(value: f64, ok: &mut bool) -> f64 {
    if cfg!(feature = "deferred-checks") {
        math::sqrt_deferred(value, ok)
    } else {
        math::sqrt(value)
    }
}

#[inline(always)]
pub fn sound_speed(w: Primitive, ok: &mut bool) -> f64 {
    face_sqrt(GAMMA * w.pressure / w.density, ok)
}

/// Largest signal speed |v_d| + c over the three directions (cells are cubes).
#[inline(always)]
pub fn max_signal_speed(w: Primitive, ok: &mut bool) -> f64 {
    let [vx, vy, vz] = w.velocity;
    vx.abs().max(vy.abs()).max(vz.abs()) + sound_speed(w, ok)
}

/// PLM with the van Leer limiter on a uniform grid: returns the values at the minus and the plus
/// face of the centre cell.
#[inline(always)]
fn plm_exact(left: f64, centre: f64, right: f64) -> [f64; 2] {
    let delta_left = centre - left;
    let delta_right = right - centre;
    let product = delta_left * delta_right;
    let slope = if product > 0.0 { 2.0 * product / (delta_left + delta_right) } else { 0.0 };
    let half = 0.5 * slope;
    [centre - half, centre + half]
}

/// PVRS wave-speed factor of Athena++'s HLLC: 1 for a rarefaction, the shock factor otherwise.
/// `ratio` is pmid / p clamped below at 1, which gives exactly 1.0 where Athena++ selects 1.0.
#[inline(always)]
fn wave_factor(pmid: f64, pressure: f64, ok: &mut bool) -> f64 {
    let ratio = if pmid > pressure { pmid / pressure } else { 1.0 };
    face_sqrt(1.0 + SHOCK_FACTOR * (ratio - 1.0), ok)
}

/// HLLC flux between two primitive states, normal velocity first (Athena++ `hllc.cpp`).
#[inline(always)]
fn hllc_exact(l: Primitive, r: Primitive, ok: &mut bool) -> Conserved {
    let [lvx, lvy, lvz] = l.velocity;
    let [rvx, rvy, rvz] = r.velocity;
    let el =
        l.pressure * INV_GAMMA_MINUS_ONE + 0.5 * l.density * (lvx * lvx + lvy * lvy + lvz * lvz);
    let er =
        r.pressure * INV_GAMMA_MINUS_ONE + 0.5 * r.density * (rvx * rvx + rvy * rvy + rvz * rvz);
    let cl = sound_speed(l, ok);
    let cr = sound_speed(r, ok);

    let rhoa = 0.5 * (l.density + r.density);
    let ca = 0.5 * (cl + cr);
    let pmid = 0.5 * (l.pressure + r.pressure + (lvx - rvx) * rhoa * ca);

    let al = lvx - cl * wave_factor(pmid, l.pressure, ok);
    let ar = rvx + cr * wave_factor(pmid, r.pressure, ok);
    let bp = if ar > 0.0 { ar } else { TINY };
    let bm = if al < 0.0 { al } else { -TINY };

    let vxl = lvx - al;
    let vxr = rvx - ar;
    let tl = l.pressure + vxl * l.density * lvx;
    let tr = r.pressure + vxr * r.density * rvx;
    let ml = l.density * vxl;
    let mr = -(r.density * vxr);
    let am = (tl - tr) / (ml + mr);
    let cp_raw = (ml * tr + mr * tl) / (ml + mr);
    let cp = if cp_raw > 0.0 { cp_raw } else { 0.0 };

    let vxl = lvx - bm;
    let vxr = rvx - bp;
    let fl = Conserved {
        density: l.density * vxl,
        momentum: [
            l.density * lvx * vxl + l.pressure,
            l.density * lvy * vxl,
            l.density * lvz * vxl,
        ],
        energy: el * vxl + l.pressure * lvx,
    };
    let fr = Conserved {
        density: r.density * vxr,
        momentum: [
            r.density * rvx * vxr + r.pressure,
            r.density * rvy * vxr,
            r.density * rvz * vxr,
        ],
        energy: er * vxr + r.pressure * rvx,
    };
    combine(fl, fr, am, bm, bp, cp)
}

/// Last step of Athena++'s HLLC: weights of the left and right fluxes and the contact term.
#[inline(always)]
fn combine(fl: Conserved, fr: Conserved, am: f64, bm: f64, bp: f64, cp: f64) -> Conserved {
    let (sl, sr, sm) = if am >= 0.0 {
        (am / (am - bm), 0.0, -bm / (am - bm))
    } else {
        (0.0, -am / (bp - am), bp / (bp - am))
    };
    let [flx, fly, flz] = fl.momentum;
    let [frx, fry, frz] = fr.momentum;
    Conserved {
        density: sl * fl.density + sr * fr.density,
        momentum: [sl * flx + sr * frx + sm * cp, sl * fly + sr * fry, sl * flz + sr * frz],
        energy: sl * fl.energy + sr * fr.energy + sm * cp * am,
    }
}

/// PLM face values; `algebraic::plm` under the `algebraic` data-point feature.
#[inline(always)]
pub fn plm(left: f64, centre: f64, right: f64) -> [f64; 2] {
    if cfg!(feature = "algebraic") {
        crate::algebraic::plm(left, centre, right)
    } else {
        plm_exact(left, centre, right)
    }
}

/// HLLC flux; `algebraic::hllc` under the `algebraic` data-point feature.
#[inline(always)]
pub fn hllc(l: Primitive, r: Primitive, ok: &mut bool) -> Conserved {
    if cfg!(feature = "algebraic") { crate::algebraic::hllc(l, r) } else { hllc_exact(l, r, ok) }
}
