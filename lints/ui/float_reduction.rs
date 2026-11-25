// compile-flags: --test
// UI test for `nami_float_reduction`: every naive float sum the lint must catch, and the code it
// must leave alone. It is compiled as a test harness, so that the test code at the end exists.

fn sum_f64(values: &[f64]) -> f64 {
    values.iter().sum()
}

fn sum_f32(values: &[f32]) -> f32 {
    values.iter().copied().sum::<f32>()
}

fn sum_of_products(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn sum_path_call(values: &[f64]) -> f64 {
    Iterator::sum(values.iter())
}

fn fold_add(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, value| total + value)
}

fn fold_add_on_the_right(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| value * value + total)
}

fn fold_add_assign(values: &[f32]) -> f32 {
    values.iter().fold(0.0, |mut total, value| {
        total += value;
        total
    })
}

fn fold_path_call(values: &[f64]) -> f64 {
    Iterator::fold(values.iter(), 0.0, |total, value| total + value)
}

// Not reported: integer sums.
fn sum_integers(values: &[u32]) -> u32 {
    values.iter().sum()
}

fn fold_integers(values: &[u64]) -> u64 {
    values.iter().fold(0, |total, value| total + value)
}

// Not reported: an integer count over floats.
fn count_positive(values: &[f64]) -> u32 {
    values.iter().fold(0, |count, &value| if value > 0.0 { count + 1 } else { count })
}

// Not reported: folds that take a maximum or a minimum, also of shifted values.
fn fold_max(values: &[f64]) -> f64 {
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn fold_min(values: &[f64]) -> f64 {
    values.iter().fold(f64::INFINITY, |least, &value| least.min(value))
}

fn fold_max_shifted(values: &[f64], shift: f64) -> f64 {
    values.iter().fold(f64::NEG_INFINITY, |greatest, &value| greatest.max(value + shift))
}

// Not reported: Horner's rule. The accumulator is multiplied, not added up.
fn horner(coefficients: &[f64], x: f64) -> f64 {
    coefficients.iter().rev().fold(0.0, |value, &coefficient| value * x + coefficient)
}

// Not reported: a method that is only named `sum`.
struct Ledger {
    balance: f64,
}

impl Ledger {
    fn sum(&self) -> f64 {
        self.balance
    }
}

// Not reported: a helper in a test module.
#[cfg(test)]
mod reference {
    pub fn naive_sum(values: &[f64]) -> f64 {
        values.iter().sum()
    }
}

// Goal: naive sums inside a test are not reported, because a test may use one as an independent
// check of a deterministic sum.
#[test]
fn naive_sums_in_a_test_are_not_reported() {
    let values = [1.0_f64, 2.0, 3.0];
    let summed: f64 = values.iter().sum();
    let folded = values.iter().fold(0.0, |total, value| total + value);
    assert!(summed.to_bits() == folded.to_bits(), "both add in the same order");
    assert!(summed.to_bits() == reference::naive_sum(&values).to_bits(), "and so does this");
}
