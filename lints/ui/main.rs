// UI test for `nami_recursion`: every case the lint must catch, and the false positives it must
// avoid.

fn direct(n: u32) -> u32 {
    if n == 0 { 0 } else { direct(n - 1) }
}

fn mutual_a(n: u32) -> u32 {
    if n == 0 { 0 } else { mutual_b(n - 1) }
}

fn mutual_b(n: u32) -> u32 {
    mutual_a(n)
}

fn via_closure(n: u32) -> u32 {
    let step = |m: u32| via_closure(m);
    if n == 0 { 0 } else { step(n - 1) }
}

trait Flux {
    fn flux(&self) -> f64;
}

struct Euler;

impl Flux for Euler {
    fn flux(&self) -> f64 {
        1.0
    }
}

// Not recursive: a wrapper whose method has the same name as the one it calls.
struct Wrapper {
    inner: Euler,
}

impl Flux for Wrapper {
    fn flux(&self) -> f64 {
        self.inner.flux()
    }
}

// Recursive through a trait method resolved to this very impl.
struct Looping;

impl Flux for Looping {
    fn flux(&self) -> f64 {
        Looping.flux()
    }
}

// Not recursive: a plain call chain.
fn leaf() -> u32 {
    1
}

fn caller() -> u32 {
    leaf() + leaf()
}

fn main() {
    let _ = direct(3) + mutual_a(3) + via_closure(3) + caller();
    let _ = Wrapper { inner: Euler }.flux() + Looping.flux();
}
