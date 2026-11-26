//! Benchmark driver.
//!
//! `layout-prototype run <soa|aos|hybrid4|hybrid8> <n> <cycles|period>` runs one layout and prints
//! one line: zone-cycles per second, the hash of the final state's bits, and the L1 error.
//! `layout-prototype compare <n> <cycles|period>` runs every layout and asserts identical bits.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use layout_prototype::aos::Aos;
use layout_prototype::grid::Shape;
use layout_prototype::hybrid::Hybrid;
use layout_prototype::problem::LinearWave;
use layout_prototype::soa::Soa;
use layout_prototype::timers::{self, PHASES};
use layout_prototype::{Block, Stop, hash_bits, run};

/// Counts allocations, so the driver can assert that the time loop makes none (ADR-0003).
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

#[expect(unsafe_code, reason = "a counting global allocator that forwards to System")]
// SAFETY: every method forwards to `System` with the caller's arguments unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _previous = ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded unchanged; the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; `ptr` came from `alloc` with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

struct Report {
    name: &'static str,
    cycles: u32,
    seconds: f64,
    hash: u64,
    rms: f64,
    per_var: [f64; 5],
}

fn measure<B: Block>(shape: Shape, stop: Stop) -> Report {
    let wave = LinearWave::unit_cube();
    let mut block = B::new(shape, &wave.canonical(shape));
    timers::reset();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let start = Instant::now();
    let outcome = run(&mut block, shape, stop);
    let seconds = start.elapsed().as_secs_f64();
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(before, after, "the time loop must not allocate");
    let state = block.canonical();
    let (rms, per_var) = wave.l1_error(shape, &state);
    Report { name: B::NAME, cycles: outcome.cycles, seconds, hash: hash_bits(&state), rms, per_var }
}

fn print(report: &Report, shape: Shape) {
    let zone_cycles = shape.cells_f64() * f64::from(report.cycles);
    println!(
        "variant={} n={} cycles={} seconds={:.4} zone_cycles_per_second={:.4e} hash={:016x} \
         rms_l1={:.6e} l1=[{:.6e} {:.6e} {:.6e} {:.6e} {:.6e}]",
        report.name,
        shape.n(),
        report.cycles,
        report.seconds,
        zone_cycles / report.seconds,
        report.hash,
        report.rms,
        report.per_var[0],
        report.per_var[1],
        report.per_var[2],
        report.per_var[3],
        report.per_var[4],
    );
    if cfg!(feature = "phase-timers") {
        for phase in PHASES {
            let share = phase.seconds() / report.seconds;
            println!(
                "  phase {:<28} {:8.3} s {:5.1} %",
                phase.name(),
                phase.seconds(),
                100.0 * share
            );
        }
    }
}

fn measure_named(name: &str, shape: Shape, stop: Stop) -> Report {
    match name {
        "soa" => measure::<Soa>(shape, stop),
        "aos" => measure::<Aos>(shape, stop),
        "hybrid4" => measure::<Hybrid<4>>(shape, stop),
        "hybrid8" => measure::<Hybrid<8>>(shape, stop),
        other => panic!("unknown variant {other}; use soa, aos, hybrid4 or hybrid8"),
    }
}

fn parse_stop(arg: &str) -> Stop {
    if arg == "period" {
        Stop::Time(LinearWave::unit_cube().period())
    } else {
        Stop::Cycles(arg.parse().expect("stop is a cycle count or `period`"))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    match strs.as_slice() {
        [_, "run", variant, n, stop] => {
            let shape = Shape::new(n.parse().expect("n is a cell count"));
            print(&measure_named(variant, shape, parse_stop(stop)), shape);
        }
        [_, "compare", n, stop] => {
            let shape = Shape::new(n.parse().expect("n is a cell count"));
            let reports: Vec<Report> = ["soa", "aos", "hybrid4", "hybrid8"]
                .iter()
                .map(|name| measure_named(name, shape, parse_stop(stop)))
                .collect();
            for r in &reports {
                print(r, shape);
            }
            let first = reports.first().expect("at least one variant").hash;
            assert!(reports.iter().all(|r| r.hash == first), "layouts must give identical bits");
            println!("all layouts bitwise identical: hash={first:016x}");
        }
        _ => panic!("usage: run <variant> <n> <cycles|period> | compare <n> <cycles|period>"),
    }
}
