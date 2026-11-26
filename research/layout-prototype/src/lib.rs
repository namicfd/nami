//! Layout prototype for nami (docs/ARCHITECTURE.md § 4(a)): 3D Euler, PLM + HLLC + Heun RK2 on
//! one periodic block, in three data layouts with identical arithmetic.

pub mod algebraic;
pub mod aos;
pub mod grid;
pub mod hybrid;
pub mod kernels;
pub mod math;
pub mod physics;
pub mod problem;
pub mod soa;
pub mod timers;

use grid::Shape;

/// Courant number, as in Athena++'s `athinput.linear_wave3d`.
pub const CFL: f64 = 0.3;

/// Upper bound on the cycles of one run (every loop is bounded).
const MAX_CYCLES: u32 = 1_000_000;

/// One block in some layout. All allocation happens in `new`.
pub trait Block: Sized {
    const NAME: &'static str;

    /// Builds the block from a state in canonical `[var][k][j][i]` order.
    fn new(shape: Shape, canonical: &[f64]) -> Self;

    /// Converts the state to primitives and returns the largest signal speed.
    fn prepare(&mut self) -> f64;

    /// Advances by `dt` with Heun's method. `prepare` must have run on the current state.
    fn advance(&mut self, dt: f64);

    /// The state in canonical order.
    fn canonical(&self) -> Box<[f64]>;
}

#[derive(Clone, Copy, Debug)]
pub enum Stop {
    Cycles(u32),
    Time(f64),
}

#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    pub cycles: u32,
    pub time: f64,
}

/// The time loop, as Athena++'s: dt from the largest signal speed, the last step cut at `tlim`.
pub fn run<B: Block>(block: &mut B, shape: Shape, stop: Stop) -> Outcome {
    let mut time = 0.0_f64;
    let mut cycles = 0_u32;
    for _ in 0..MAX_CYCLES {
        let done = match stop {
            Stop::Cycles(limit) => cycles >= limit,
            Stop::Time(limit) => time >= limit,
        };
        if done {
            return Outcome { cycles, time };
        }
        let speed = block.prepare();
        assert!(speed > 0.0 && speed.is_finite(), "signal speed must be positive and finite");
        let mut dt = CFL * shape.dx() / speed;
        if let Stop::Time(limit) = stop
            && time + dt > limit
        {
            dt = limit - time;
        }
        block.advance(dt);
        time += dt;
        cycles += 1;
    }
    panic!("run exceeded {MAX_CYCLES} cycles");
}

/// FNV-1a over the bits of a state, for comparing layouts.
pub fn hash_bits(state: &[f64]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in state {
        for byte in value.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}
