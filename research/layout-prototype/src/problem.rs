//! Athena++'s `linear_wave` problem (v24.0 `src/pgen/linear_wave.cpp`), adiabatic hydro,
//! `wave_flag = 0` (the left-going sound wave), on the unit cube with the wave vector along the
//! grid diagonal (Athena++ derives `ang_2`, `ang_3` from the box when they are not given).

use crate::grid::{Shape, to_f64};
use crate::math;
use crate::physics::GAMMA;

const AMPLITUDE: f64 = 1.0e-6;
const DENSITY: f64 = 1.0;
const PRESSURE: f64 = 1.0 / GAMMA;

pub struct LinearWave {
    sin_a2: f64,
    cos_a2: f64,
    sin_a3: f64,
    cos_a3: f64,
    wavelength: f64,
    wavenumber: f64,
    sound_speed: f64,
    enthalpy: f64,
}

impl LinearWave {
    pub fn unit_cube() -> Self {
        let (x1size, x2size, x3size) = (1.0, 1.0, 1.0);
        let ang_3 = math::atan(x1size / x2size);
        let (sin_a3, cos_a3) = (math::sin(ang_3), math::cos(ang_3));
        let ang_2 = math::atan(0.5 * (x1size * cos_a3 + x2size * sin_a3) / x3size);
        let (sin_a2, cos_a2) = (math::sin(ang_2), math::cos(ang_2));
        let wavelength =
            (x1size * cos_a2 * cos_a3).min(x2size * cos_a2 * sin_a3).min(x3size * sin_a2);
        let gm1 = GAMMA - 1.0;
        let enthalpy = (PRESSURE / gm1 + PRESSURE) / DENSITY;
        let sound_speed = math::sqrt(gm1 * enthalpy);
        assert!(wavelength > 0.0, "wavelength must be positive");
        assert!((sound_speed - 1.0).abs() < 1.0e-12, "background sound speed is 1");
        Self {
            sin_a2,
            cos_a2,
            sin_a3,
            cos_a3,
            wavelength,
            wavenumber: 2.0 * core::f64::consts::PI / wavelength,
            sound_speed,
            enthalpy,
        }
    }

    /// Time for the wave to travel one wavelength.
    pub fn period(&self) -> f64 {
        assert!(self.sound_speed > 0.0, "sound speed must be positive");
        self.wavelength / self.sound_speed
    }

    /// Conserved state `[d, m1, m2, m3, E]` at cell centre `x` (Athena++ evaluates the initial
    /// and the analytic solution at cell centres, `correct_ic = false`).
    pub fn conserved_at(&self, x: [f64; 3]) -> [f64; 5] {
        let [x1, x2, x3] = x;
        let along = self.cos_a2 * (x1 * self.cos_a3 + x2 * self.sin_a3) + x3 * self.sin_a2;
        let sn = math::sin(self.wavenumber * along);
        let d = DENSITY + AMPLITUDE * sn;
        let mx = AMPLITUDE * sn * (-self.sound_speed);
        let m1 = mx * self.cos_a2 * self.cos_a3;
        let m2 = mx * self.cos_a2 * self.sin_a3;
        let m3 = mx * self.sin_a2;
        let e = PRESSURE / (GAMMA - 1.0) + AMPLITUDE * sn * self.enthalpy;
        [d, m1, m2, m3, e]
    }

    /// Initial (and, after whole periods, analytic) state in the canonical `[var][k][j][i]` order.
    pub fn canonical(&self, shape: Shape) -> Box<[f64]> {
        let (n, cells) = (shape.n(), shape.cells());
        let dx = shape.dx();
        let mut out = vec![0.0; 5 * cells].into_boxed_slice();
        let mut position = 0_usize;
        for k in 0..n {
            for j in 0..n {
                for i in 0..n {
                    let centre = |c: usize| (to_f64(c) + 0.5) * dx;
                    let state = self.conserved_at([centre(i), centre(j), centre(k)]);
                    for (v, value) in state.into_iter().enumerate() {
                        let Some(slot) = out.get_mut(v * cells + position) else {
                            panic!("cell {position} out of range");
                        };
                        *slot = value;
                    }
                    position += 1;
                }
            }
        }
        assert_eq!(position, cells, "every cell is set");
        out
    }

    /// Athena++'s error norm: per-variable L1 (volume-weighted mean), and their RMS.
    pub fn l1_error(&self, shape: Shape, state: &[f64]) -> (f64, [f64; 5]) {
        let exact = self.canonical(shape);
        assert_eq!(exact.len(), state.len(), "states must have the same shape");
        let mut per_var = [0.0; 5];
        let cells = shape.cells();
        for ((e, s), out) in
            exact.chunks_exact(cells).zip(state.chunks_exact(cells)).zip(&mut per_var)
        {
            *out = neumaier(e.iter().zip(s).map(|(a, b)| (a - b).abs())) / shape.cells_f64();
        }
        let rms = math::sqrt(neumaier(per_var.iter().map(|v| v * v)));
        assert!(rms.is_finite(), "error must be finite");
        (rms, per_var)
    }
}

/// Compensated (Neumaier) sum, as `kernels::reduce::sum_compensated`.
fn neumaier(values: impl Iterator<Item = f64>) -> f64 {
    let mut sum = 0.0_f64;
    let mut compensation = 0.0_f64;
    for v in values {
        let t = sum + v;
        compensation += if sum.abs() >= v.abs() { (sum - t) + v } else { (v - t) + sum };
        sum = t;
    }
    sum + compensation
}
