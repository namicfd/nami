// UI test for `nami_recursion`: calls that are not written as calls. A function named as a value
// counts as called by the function that names it, and an overloaded operator or deref counts as
// a call to the trait method behind it.

use std::ops::{Add, AddAssign, Deref, DerefMut, Index, IndexMut, Neg, Not};

fn apply<F: Fn(u32) -> u32>(f: F, n: u32) -> u32 {
    f(n)
}

fn apply_pointer(f: fn(u32) -> u32, n: u32) -> u32 {
    f(n)
}

// Recursive: `walk` passes itself to a function that calls it.
fn walk(n: u32) -> u32 {
    if n == 0 { 0 } else { apply(walk, n - 1) }
}

// Recursive: the same, through a function pointer and a mutual partner.
fn ping(n: u32) -> u32 {
    if n == 0 { 0 } else { apply_pointer(pong, n - 1) }
}

fn pong(n: u32) -> u32 {
    ping(n)
}

struct Counter;

impl Counter {
    // Recursive: a method that passes itself by path to `Option::map`.
    fn step(n: u32) -> u32 {
        n.checked_sub(1).map(Counter::step).unwrap_or(0)
    }

    // Not recursive: a method that passes another function by path.
    fn halve(n: u32) -> u32 {
        apply(half, n)
    }
}

// Not recursive: passed by name, and calls nothing.
fn half(n: u32) -> u32 {
    n / 2
}

// Recursive: an `Add` impl that evaluates `+` of its own type.
#[derive(Clone, Copy)]
struct Steps(u32);

impl Add for Steps {
    type Output = Steps;

    fn add(self, other: Steps) -> Steps {
        if self.0 == 0 { other } else { Steps(self.0 - 1) + Steps(other.0 + 1) }
    }
}

// Not recursive: an `Add` impl that adds the fields, which is built in.
#[derive(Clone, Copy)]
struct Meters(f64);

impl Add for Meters {
    type Output = Meters;

    fn add(self, other: Meters) -> Meters {
        Meters(self.0 + other.0)
    }
}

// Recursive: a `Neg` impl that negates its own type.
struct Flip(bool);

impl Neg for Flip {
    type Output = Flip;

    fn neg(self) -> Flip {
        if self.0 { Flip(false) } else { -Flip(true) }
    }
}

// Not recursive: a `Neg` impl that negates the field.
struct Charge(f64);

impl Neg for Charge {
    type Output = Charge;

    fn neg(self) -> Charge {
        Charge(-self.0)
    }
}

// Recursive: an `AddAssign` impl that evaluates `+=` of its own type.
struct Tally(u32);

impl AddAssign for Tally {
    fn add_assign(&mut self, other: Tally) {
        if other.0 > 0 {
            self.0 += 1;
            *self += Tally(other.0 - 1);
        }
    }
}

// Not recursive: an `AddAssign` impl that adds to the field.
struct Total(f64);

impl AddAssign for Total {
    fn add_assign(&mut self, other: Total) {
        self.0 += other.0;
    }
}

// Recursive: an `Index` impl that indexes its own type.
struct Ramp([f64; 4]);

impl Index<u32> for Ramp {
    type Output = f64;

    fn index(&self, i: u32) -> &f64 {
        if i == 0 { &self.0[0] } else { &self[i - 1] }
    }
}

// Not recursive: an `Index` impl that indexes the array, which is built in.
struct Cells([f64; 4]);

impl Index<u32> for Cells {
    type Output = f64;

    fn index(&self, i: u32) -> &f64 {
        if i == 0 { &self.0[0] } else { &self.0[1] }
    }
}

// Recursive: a `Deref` impl that derefs its own type with `*`.
struct Chain {
    value: f64,
    next: Option<Box<Chain>>,
}

impl Deref for Chain {
    type Target = f64;

    fn deref(&self) -> &f64 {
        match &self.next {
            Some(next) => &***next,
            None => &self.value,
        }
    }
}

// Recursive: a `Deref` impl that derefs its own type by deref coercion.
struct Link {
    value: f64,
    next: Option<Box<Link>>,
}

impl Deref for Link {
    type Target = f64;

    fn deref(&self) -> &f64 {
        match &self.next {
            Some(next) => next,
            None => &self.value,
        }
    }
}

// Not recursive: a `Deref` impl, used with `*` and through autoderef by its callers.
struct Cell {
    value: f64,
}

impl Deref for Cell {
    type Target = f64;

    fn deref(&self) -> &f64 {
        &self.value
    }
}

fn read_cell(cell: &Cell) -> f64 {
    **cell + cell.abs()
}

// Not recursive: deref coercion through a `Deref` impl that does not call back.
fn cell_value(cell: &Cell) -> &f64 {
    cell
}

// Not recursive: a `DerefMut` impl, used with `*` in a mutated place.
impl DerefMut for Cell {
    fn deref_mut(&mut self) -> &mut f64 {
        &mut self.value
    }
}

fn reset_cell(cell: &mut Cell) {
    **cell = 0.0;
}

// Recursive: a `DerefMut` impl that derefs its own type with `*` in a mutated place.
impl DerefMut for Chain {
    fn deref_mut(&mut self) -> &mut f64 {
        match &mut self.next {
            Some(next) => &mut ***next,
            None => &mut self.value,
        }
    }
}

// Recursive: a `Deref` impl that derefs its own type by autoderef of a method receiver.
struct Node {
    value: f64,
    next: Option<Box<Node>>,
}

impl Deref for Node {
    type Target = f64;

    fn deref(&self) -> &f64 {
        match &self.next {
            Some(next) if next.is_nan() => &self.value,
            _ => &self.value,
        }
    }
}

// Recursive: an `IndexMut` impl that indexes its own type in a mutated place.
impl IndexMut<u32> for Ramp {
    fn index_mut(&mut self, i: u32) -> &mut f64 {
        if i == 0 { &mut self.0[0] } else { &mut self[i - 1] }
    }
}

// Not recursive: an `IndexMut` impl that indexes the array.
impl IndexMut<u32> for Cells {
    fn index_mut(&mut self, i: u32) -> &mut f64 {
        if i == 0 { &mut self.0[0] } else { &mut self.0[1] }
    }
}

// Recursive: a `PartialEq` impl that compares its own type with `==`.
struct Depth(u32);

impl PartialEq for Depth {
    fn eq(&self, other: &Depth) -> bool {
        if self.0 == 0 || other.0 == 0 { self.0 == other.0 } else { Depth(self.0 - 1) == *other }
    }
}

// Not recursive: a `PartialEq` impl that compares the fields, which is built in.
struct Level(u32);

impl PartialEq for Level {
    fn eq(&self, other: &Level) -> bool {
        self.0 == other.0
    }
}

// Recursive: a `Not` impl that negates its own type with `!`.
struct Gate(bool);

impl Not for Gate {
    type Output = Gate;

    fn not(self) -> Gate {
        if self.0 { Gate(false) } else { !Gate(true) }
    }
}

// Not recursive: a `Not` impl that negates the field.
struct Mask(bool);

impl Not for Mask {
    type Output = Mask;

    fn not(self) -> Mask {
        Mask(!self.0)
    }
}

fn main() {
    let _ = walk(3) + ping(3) + Counter::step(3) + Counter::halve(3);
    let _ = (Steps(2) + Steps(1)).0;
    let _ = (Meters(1.0) + Meters(2.0)).0;
    let _ = (-Flip(true)).0;
    let _ = (-Charge(1.0)).0;
    let mut tally = Tally(0);
    tally += Tally(2);
    let mut total = Total(0.0);
    total += Total(1.0);
    let _ = Ramp([0.0; 4])[2] + Cells([0.0; 4])[2];
    let _ = *Chain { value: 1.0, next: None } + *Link { value: 1.0, next: None };
    let mut cell = Cell { value: 1.0 };
    reset_cell(&mut cell);
    let _ = read_cell(&cell) + *cell_value(&cell);
    let mut chain = Chain { value: 1.0, next: None };
    *chain = 2.0;
    let _ = *Node { value: 1.0, next: None };
    let mut ramp = Ramp([0.0; 4]);
    ramp[2] = 1.0;
    let mut cells = Cells([0.0; 4]);
    cells[2] = 1.0;
    let _ = Depth(2) == Depth(1) || Level(2) == Level(1);
    let _ = (!Gate(true)).0 || (!Mask(true)).0;
}
