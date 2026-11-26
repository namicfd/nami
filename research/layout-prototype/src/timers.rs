//! Optional per-phase wall-clock timers (feature `phase-timers`). Without the feature, `timed!`
//! is a plain block and costs nothing.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug)]
pub enum Phase {
    Convert,
    Ghost,
    Reconstruct,
    Riemann,
    FluxDiff,
    Update,
    Repack,
}

pub const PHASES: [Phase; 7] = [
    Phase::Convert,
    Phase::Ghost,
    Phase::Reconstruct,
    Phase::Riemann,
    Phase::FluxDiff,
    Phase::Update,
    Phase::Repack,
];

static NANOS: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Convert => "cons->prim, dt, backstop",
            Phase::Ghost => "ghost fill",
            Phase::Reconstruct => "reconstruction (PLM)",
            Phase::Riemann => "Riemann solver (HLLC)",
            Phase::FluxDiff => "flux difference",
            Phase::Update => "RK update",
            Phase::Repack => "hybrid repacking",
        }
    }

    fn counter(self) -> &'static AtomicU64 {
        let [a, b, c, d, e, f, g] = &NANOS;
        match self {
            Phase::Convert => a,
            Phase::Ghost => b,
            Phase::Reconstruct => c,
            Phase::Riemann => d,
            Phase::FluxDiff => e,
            Phase::Update => f,
            Phase::Repack => g,
        }
    }

    pub fn add(self, nanos: u64) {
        let _previous = self.counter().fetch_add(nanos, Ordering::Relaxed);
    }

    pub fn seconds(self) -> f64 {
        let nanos = self.counter().load(Ordering::Relaxed);
        let millis = u32::try_from(nanos / 1_000_000).expect("phase time fits in u32 ms");
        f64::from(millis) * 1.0e-3
    }
}

pub fn reset() {
    for counter in &NANOS {
        counter.store(0, Ordering::Relaxed);
    }
}

#[cfg(feature = "phase-timers")]
#[macro_export]
macro_rules! timed {
    ($phase:expr, $body:block) => {{
        let start = std::time::Instant::now();
        let value = $body;
        let nanos = u64::try_from(start.elapsed().as_nanos()).expect("elapsed fits in u64");
        $phase.add(nanos);
        value
    }};
}

#[cfg(not(feature = "phase-timers"))]
#[macro_export]
macro_rules! timed {
    ($phase:expr, $body:block) => {{
        let _ = $phase;
        $body
    }};
}
