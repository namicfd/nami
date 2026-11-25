// UI test for `nami_recursion`: calls whose generic arguments grow with every call. The search
// from a function that reaches such a call never ends on its own, so it is cut off at a bound,
// and the function is reported as not shown to be free of recursion.

// Recursive: the search comes back to `deeper` at once, with another argument.
fn deeper<T: Copy>(x: T, n: u32) -> u32 {
    if n == 0 { 0 } else { deeper((x, x), n - 1) }
}

// Cut off at the depth bound: `deeper::<u8>`, `deeper::<(u8, u8)>`, and so on.
fn calls_deeper() -> u32 {
    deeper(1_u8, 3)
}

// Recursive: the search comes back to `wider` at once, with another argument.
fn wider<T>(n: u32) -> u32 {
    if n == 0 { 0 } else { wider::<(T, u8)>(n - 1) + wider::<(T, u16)>(n - 1) }
}

// Cut off at the instance bound: every call doubles the number of instances.
fn calls_wider() -> u32 {
    wider::<u8>(3)
}

trait Probe {
    fn value(&self, depth: u32) -> u32;
}

#[derive(Clone, Copy)]
struct Leaf;

impl Probe for Leaf {
    fn value(&self, depth: u32) -> u32 {
        depth
    }
}

#[derive(Clone, Copy)]
struct Nest<T> {
    inner: T,
}

// Recursive: `value` of `Nest<T>` calls `value` of `Nest<Nest<T>>`, which is the same function.
impl<T: Probe + Copy> Probe for Nest<T> {
    fn value(&self, depth: u32) -> u32 {
        if depth == 0 { self.inner.value(0) } else { Nest { inner: *self }.value(depth - 1) }
    }
}

// Cut off at the depth bound. Without it the search would go on to the instance bound, and the
// compiler would overflow its stack on the way, resolving `value` for ever deeper nested types.
fn calls_nest() -> u32 {
    Nest { inner: Leaf }.value(3)
}

fn main() {}
