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

trait Probe {
    fn value(&self, depth: u32) -> u32;
}

// Not recursive on its own: which `value` runs depends on `P`.
fn through_generic<P: Probe>(probe: &P, depth: u32) -> u32 {
    probe.value(depth)
}

// Recursive through a generic function that calls back into this impl.
struct Cycle;

impl Probe for Cycle {
    fn value(&self, depth: u32) -> u32 {
        if depth == 0 { 0 } else { through_generic(self, depth - 1) }
    }
}

// Recursive: a generic function that calls itself.
fn generic_direct<T: Copy>(x: T, n: u32) -> u32 {
    if n == 0 { 0 } else { generic_direct(x, n - 1) }
}

// Recursive: a generic function that calls itself with other generic arguments.
fn generic_other<T: Copy>(x: T, n: u32) -> u32 {
    if n == 0 { 0 } else { generic_other(n, n - 1) }
}

// Recursive: a trait default method that calls itself.
trait Countdown {
    fn count(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.count(n - 1) }
    }
}

struct Timer;

impl Countdown for Timer {}

// Recursive through a default method: `area` calls `doubled`, which calls `area` of the same
// type. The default method is not recursive on its own, because another impl may not call it.
trait Shape {
    fn area(&self, n: u32) -> u32;

    fn doubled(&self, n: u32) -> u32 {
        2 * self.area(n)
    }
}

struct Square;

impl Shape for Square {
    fn area(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.doubled(n - 1) }
    }
}

// Not recursive: the same default method, reached from an impl that does not call it back.
struct Circle;

impl Shape for Circle {
    fn area(&self, n: u32) -> u32 {
        n
    }
}

// Not recursive on its own: a generic decorator, whose method calls the same method of the value
// it wraps.
struct Nest<T> {
    inner: T,
}

impl<T: Probe> Probe for Nest<T> {
    fn value(&self, depth: u32) -> u32 {
        self.inner.value(depth)
    }
}

// Recursive: a concrete impl that wraps itself in the decorator, whose method calls it back.
struct Leaf;

impl Probe for Leaf {
    fn value(&self, depth: u32) -> u32 {
        if depth == 0 { 0 } else { Nest { inner: Leaf }.value(depth - 1) }
    }
}

// Not recursive: a concrete impl that wraps another type in the decorator. It reaches the cycle
// of `Cycle`, but it is not on it.
struct Stem;

impl Probe for Stem {
    fn value(&self, depth: u32) -> u32 {
        Nest { inner: Nest { inner: Cycle } }.value(depth)
    }
}

trait System {
    type Inner: Probe;

    fn inner(&self) -> &Self::Inner;
}

// Not recursive on its own: which `value` runs depends on `S::Inner`.
fn through_inner<S: System>(system: &S, depth: u32) -> u32 {
    system.inner().value(depth)
}

struct Hub {
    spoke: Spoke,
}

impl System for Hub {
    type Inner = Spoke;

    fn inner(&self) -> &Spoke {
        &self.spoke
    }
}

// Recursive through an associated type: `through_inner::<Hub>` calls `value` of `Hub::Inner`,
// which is this impl.
struct Spoke;

impl Probe for Spoke {
    fn value(&self, depth: u32) -> u32 {
        if depth == 0 { 0 } else { through_inner(&Hub { spoke: Spoke }, depth - 1) }
    }
}

// Not recursive: two generic decorators, nested in each other and in themselves.
struct Scaled<T> {
    inner: T,
}

impl<T: Flux> Flux for Scaled<T> {
    fn flux(&self) -> f64 {
        2.0 * self.inner.flux()
    }
}

struct Shifted<T> {
    inner: T,
}

impl<T: Flux> Flux for Shifted<T> {
    fn flux(&self) -> f64 {
        1.0 + self.inner.flux()
    }
}

fn nested_decorators() -> f64 {
    let mixed = Scaled { inner: Shifted { inner: Euler } };
    let twice = Scaled { inner: Scaled { inner: Euler } };
    mixed.flux() + twice.flux()
}

// Not recursive: a generic helper, called from an impl that does not recurse.
fn total<F: Flux>(f: &F) -> f64 {
    f.flux()
}

struct Pair {
    left: Euler,
    right: Scaled<Euler>,
}

impl Flux for Pair {
    fn flux(&self) -> f64 {
        total(&self.left) + total(&self.right)
    }
}

fn main() {
    let _ = direct(3) + mutual_a(3) + via_closure(3) + caller();
    let _ = Wrapper { inner: Euler }.flux() + Looping.flux();
    let _ = Cycle.value(3) + generic_direct(1.0, 3) + generic_other(1.0, 3) + Timer.count(3);
    let _ = Square.area(3) + Circle.doubled(3) + Leaf.value(3) + Stem.value(3);
    let _ = Spoke.value(3);
    let pair = Pair { left: Euler, right: Scaled { inner: Euler } };
    let _ = nested_decorators() + pair.flux();
}
