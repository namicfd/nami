//! Data point only (feature `algebraic`): PLM and HLLC with `algebraic_*` arithmetic, which lets
//! LLVM reassociate, contract to FMA and use reciprocals. ADR-0005 bans this; the prototype
//! measures what the ban costs. The operations are those of `physics`, in the same order.
#![expect(clippy::disallowed_methods, reason = "algebraic_* is the point of this data point")]

use core::ops::{Add, Div, Mul, Neg, Sub};

use crate::math;
use crate::physics::{Conserved, GAMMA, Primitive};

/// An `f64` whose arithmetic is `algebraic_*`.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
struct R(f64);

impl Add for R {
    type Output = R;
    #[inline(always)]
    fn add(self, o: R) -> R {
        R(self.0.algebraic_add(o.0))
    }
}

impl Sub for R {
    type Output = R;
    #[inline(always)]
    fn sub(self, o: R) -> R {
        R(self.0.algebraic_sub(o.0))
    }
}

impl Mul for R {
    type Output = R;
    #[inline(always)]
    fn mul(self, o: R) -> R {
        R(self.0.algebraic_mul(o.0))
    }
}

impl Div for R {
    type Output = R;
    #[inline(always)]
    fn div(self, o: R) -> R {
        R(self.0.algebraic_div(o.0))
    }
}

impl Neg for R {
    type Output = R;
    #[inline(always)]
    fn neg(self) -> R {
        R(-self.0)
    }
}

const ZERO: R = R(0.0);
const HALF: R = R(0.5);
const ONE: R = R(1.0);
const TWO: R = R(2.0);
const G: R = R(GAMMA);
const IGM1: R = R(1.0 / (GAMMA - 1.0));
const SHOCK: R = R((GAMMA + 1.0) / (2.0 * GAMMA));
const TINY: R = R(1.0e-20);

#[inline(always)]
fn sqrt(x: R) -> R {
    R(math::sqrt(x.0))
}

#[inline(always)]
pub fn plm(left: f64, centre: f64, right: f64) -> [f64; 2] {
    let (l, c, r) = (R(left), R(centre), R(right));
    let dl = c - l;
    let dr = r - c;
    let product = dl * dr;
    let slope = if product > ZERO { TWO * product / (dl + dr) } else { ZERO };
    let half = HALF * slope;
    [(c - half).0, (c + half).0]
}

#[inline(always)]
fn wave_factor(pmid: R, pressure: R) -> R {
    let ratio = if pmid > pressure { pmid / pressure } else { ONE };
    sqrt(ONE + SHOCK * (ratio - ONE))
}

struct Side {
    d: R,
    v: [R; 3],
    p: R,
}

impl Side {
    #[inline(always)]
    fn new(w: Primitive) -> Self {
        let [x, y, z] = w.velocity;
        Self { d: R(w.density), v: [R(x), R(y), R(z)], p: R(w.pressure) }
    }
}

#[inline(always)]
pub fn hllc(left: Primitive, right: Primitive) -> Conserved {
    let (l, r) = (Side::new(left), Side::new(right));
    let ([lx, ly, lz], [rx, ry, rz]) = (l.v, r.v);
    let el = l.p * IGM1 + HALF * l.d * (lx * lx + ly * ly + lz * lz);
    let er = r.p * IGM1 + HALF * r.d * (rx * rx + ry * ry + rz * rz);
    let cl = sqrt(G * l.p / l.d);
    let cr = sqrt(G * r.p / r.d);
    let rhoa = HALF * (l.d + r.d);
    let ca = HALF * (cl + cr);
    let pmid = HALF * (l.p + r.p + (lx - rx) * rhoa * ca);
    let al = lx - cl * wave_factor(pmid, l.p);
    let ar = rx + cr * wave_factor(pmid, r.p);
    let bp = if ar > ZERO { ar } else { TINY };
    let bm = if al < ZERO { al } else { -TINY };
    let (vxl, vxr) = (lx - al, rx - ar);
    let tl = l.p + vxl * l.d * lx;
    let tr = r.p + vxr * r.d * rx;
    let ml = l.d * vxl;
    let mr = -(r.d * vxr);
    let am = (tl - tr) / (ml + mr);
    let cp_raw = (ml * tr + mr * tl) / (ml + mr);
    let cp = if cp_raw > ZERO { cp_raw } else { ZERO };
    let (vxl, vxr) = (lx - bm, rx - bp);
    let fl = [l.d * vxl, l.d * lx * vxl + l.p, l.d * ly * vxl, l.d * lz * vxl, el * vxl + l.p * lx];
    let fr = [r.d * vxr, r.d * rx * vxr + r.p, r.d * ry * vxr, r.d * rz * vxr, er * vxr + r.p * rx];
    let (sl, sr, sm) = if am >= ZERO {
        (am / (am - bm), ZERO, -bm / (am - bm))
    } else {
        (ZERO, -am / (bp - am), bp / (bp - am))
    };
    let [fl0, fl1, fl2, fl3, fl4] = fl;
    let [fr0, fr1, fr2, fr3, fr4] = fr;
    Conserved {
        density: (sl * fl0 + sr * fr0).0,
        momentum: [
            (sl * fl1 + sr * fr1 + sm * cp).0,
            (sl * fl2 + sr * fr2).0,
            (sl * fl3 + sr * fr3).0,
        ],
        energy: (sl * fl4 + sr * fr4 + sm * cp * am).0,
    }
}
