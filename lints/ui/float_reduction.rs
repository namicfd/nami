// compile-flags: --test
// UI test for `nami_float_reduction`: every naive float sum the lint must catch, and the code it
// must leave alone. It is compiled as a test harness, so that the test code at the end exists.

use std::ops::{Add, AddAssign, Sub, SubAssign};

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

fn product_f64(values: &[f64]) -> f64 {
    values.iter().product::<f64>()
}

fn product_f32(values: &[f32]) -> f32 {
    Iterator::product(values.iter())
}

fn sum_into_option(values: &[f64]) -> Option<f64> {
    values.iter().map(|&value| Some(value)).sum::<Option<f64>>()
}

fn sum_into_result(values: &[f32]) -> Result<f32, String> {
    values.iter().map(|&value| Ok(value)).sum()
}

fn product_into_option(values: &[f64]) -> Option<f64> {
    values.iter().map(|&value| Some(value)).product()
}

fn reduce_add(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(|total, value| total + value)
}

fn reduce_path_call(values: &[f32]) -> Option<f32> {
    Iterator::reduce(values.iter().copied(), |total, value| value + total)
}

fn reduce_add_method(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::add)
}

fn try_fold_add(values: &[f64]) -> Option<f64> {
    values.iter().try_fold(0.0, |total, value| Some(total + value))
}

fn try_fold_add_assign(values: &[f32]) -> Result<f32, String> {
    values.iter().try_fold(0.0, |mut total, value| {
        total += value;
        Ok(total)
    })
}

fn rfold_add(values: &[f64]) -> f64 {
    values.iter().rfold(0.0, |total, value| total + value)
}

fn try_rfold_add(values: &[f64]) -> Option<f64> {
    values.iter().try_rfold(0.0, |total, value| Some(total + value))
}

fn fold_sub(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, value| total - value)
}

fn fold_sub_on_the_right(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |difference, value| value - difference)
}

fn fold_sub_assign(values: &[f32]) -> f32 {
    values.iter().fold(0.0, |mut total, value| {
        total -= value;
        total
    })
}

fn fold_add_method(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, f64::add)
}

fn fold_add_trait_method(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, core::ops::Add::add)
}

fn fold_sub_qualified_method(values: &[f32]) -> f32 {
    values.iter().copied().fold(0.0, <f32 as Sub>::sub)
}

fn rfold_add_reference_method(values: &[f64]) -> f64 {
    values.iter().rfold(0.0, <f64 as Add<&f64>>::add)
}

fn fold_add_method_call(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, value| total.add(value))
}

fn fold_sub_method_call(values: &[f32]) -> f32 {
    values.iter().fold(0.0, |total, &value| value.sub(total))
}

fn fold_add_path_call(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| f64::add(total, value))
}

fn reduce_sub_trait_path_call(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(|total, value| Sub::sub(total, value))
}

fn fold_dot_product_mul_add(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).fold(0.0, |total, (a, b)| a.mul_add(*b, total))
}

fn fold_mul_add_path_call(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).fold(0.0, |total, (&a, &b)| f32::mul_add(a, b, total))
}

fn fold_add_through_a_local(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let next = total + value;
        next
    })
}

fn fold_add_in_a_branch(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| if value > 0.0 { total + value } else { total })
}

fn fold_assign_sum(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        total = total + value;
        total
    })
}

fn fold_add_in_a_match_scrutinee(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| match total + value {
        sum => sum,
    })
}

fn fold_add_through_a_pattern_local(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let (sum, _count) = (total + value, 1);
        sum
    })
}

fn fold_add_in_an_if_let(values: &[f64]) -> f64 {
    values.iter().fold(
        0.0,
        |total, &value| {
            if let Some(sum) = Some(total + value) { sum } else { total }
        },
    )
}

fn fold_add_in_a_tuple_field(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| (total + value, 0).0)
}

fn fold_add_in_an_array(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| [total + value][0])
}

fn fold_add_in_a_discarded_statement(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        total + value;
        value
    })
}

// Reported: an accumulator update inside a condition, a guard or a compared operand. Only the
// value there is excluded, not its side effects.
fn fold_add_assign_in_a_compared_block(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        let _positive = {
            total += value;
            total
        } > 0.0;
        total
    })
}

fn fold_assign_sum_in_a_compared_block(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        let _ = 0.0 < {
            total = total + value;
            total
        };
        total
    })
}

fn fold_add_assign_in_an_if_condition(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        if {
            total += value;
            true
        } {
            total
        } else {
            total
        }
    })
}

fn fold_assign_sum_in_an_if_condition(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        if {
            total = total + value;
            total > 0.0
        } {}
        total
    })
}

fn fold_add_assign_in_a_guard(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| match value {
        x if {
            total += x;
            true
        } =>
        {
            total
        }
        _ => total,
    })
}

fn fold_add_assign_in_an_and(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        let _ = value > 0.0 && {
            total += value;
            true
        };
        total
    })
}

fn fold_add_assign_in_an_or(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        let _ = value > 0.0 || {
            total += value;
            true
        };
        total
    })
}

fn fold_sum_assigned_to_a_local_in_a_compared_block(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let sum;
        let _ = {
            sum = total + value;
            sum
        } > 0.0;
        sum
    })
}

fn fold_sum_assigned_to_a_local_in_an_if_condition(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let sum;
        if {
            sum = total + value;
            sum > 0.0
        } {
            sum
        } else {
            sum
        }
    })
}

// Reported: `add_assign` and `sub_assign`, the methods behind `+=` and `-=`.
fn fold_add_assign_method_call(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |mut total, &value| {
        total.add_assign(value);
        total
    })
}

fn fold_sub_assign_path_call(values: &[f32]) -> f32 {
    values.iter().fold(0.0, |mut total, &value| {
        SubAssign::sub_assign(&mut total, value);
        total
    })
}

// Reported: a sum inside a closure nested in the fold's closure.
fn fold_add_in_a_nested_closure(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| (|| total + value)())
}

// Reported: more shapes that a sum reaches the result through.
struct Total {
    value: f64,
}

fn fold_add_shadowed(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let total = total + value;
        total
    })
}

fn fold_add_as_a_receiver(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| (total + value).abs())
}

fn fold_add_as_an_argument(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| total.max(total + value))
}

fn fold_add_in_a_break(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| 'sum: {
        break 'sum total + value;
    })
}

fn fold_add_in_a_return(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        return total + value;
    })
}

fn fold_add_in_a_cast(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| (total + value) as f64)
}

fn fold_add_behind_a_reference(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| *&(total + value))
}

fn fold_add_in_a_struct_field(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| Total { value: total + value }.value)
}

fn fold_add_in_a_let_else(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let Some(sum) = Some(total + value) else {
            return total;
        };
        sum
    })
}

fn fold_add_tested_and_returned(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let sum = total + value;
        if sum > 0.0 { sum } else { total }
    })
}

fn fold_add_in_an_unused_local(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let _unused = total + value;
        value
    })
}

fn fold_add_assigned_later(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| {
        let sum;
        sum = total + value;
        sum
    })
}

// Reported as too complex: more expressions than the lint traces, even though it is a maximum.
macro_rules! times_four {
    ($e:expr) => {
        $e * $e * $e * $e
    };
}

fn fold_too_complex(values: &[f64]) -> f64 {
    values.iter().fold(f64::NEG_INFINITY, |greatest, &value| {
        greatest.max(times_four!(times_four!(times_four!(times_four!(times_four!(times_four!(
            times_four!(value)
        )))))))
    })
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

// Not reported: reductions that take a maximum or a minimum, in the other folding methods.
fn reduce_max(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::max)
}

fn reduce_min(values: &[f32]) -> Option<f32> {
    values.iter().copied().reduce(|least, value| least.min(value))
}

fn try_fold_max(values: &[f64]) -> Option<f64> {
    values.iter().try_fold(f64::NEG_INFINITY, |greatest, &value| Some(greatest.max(value)))
}

fn rfold_min_shifted(values: &[f64], shift: f64) -> f64 {
    values.iter().rfold(f64::INFINITY, |least, &value| least.min(value - shift))
}

fn try_rfold_max(values: &[f64]) -> Option<f64> {
    values.iter().try_rfold(f64::NEG_INFINITY, |greatest, &value| Some(greatest.max(value)))
}

// Not reported, by design: a sum used only as a condition, a guard or a compared value.
fn fold_sum_only_compared(values: &[f64], limit: f64) -> f64 {
    values.iter().fold(0.0, |total, &value| if total + value > limit { total } else { value })
}

fn fold_sum_only_in_a_guard(values: &[f64]) -> f64 {
    values.iter().fold(0.0, |total, &value| match value {
        x if total + x > 0.0 => x,
        _ => total,
    })
}

// Not reported: an argmin or argmax, where the accumulator is subtracted only in the condition,
// in a local that only the condition uses, or in an `if` statement that assigns the value.
fn fold_argmin(values: &[f64]) -> f64 {
    values.iter().fold(f64::INFINITY, |best, &value| if value - best < 0.0 { value } else { best })
}

fn reduce_argmax_through_a_local(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(|best, value| {
        let gain = value - best;
        if gain > 0.0 { value } else { best }
    })
}

fn fold_argmin_assign(values: &[f64]) -> f64 {
    values.iter().fold(f64::INFINITY, |mut best, &value| {
        if value - best < 0.0 {
            best = value;
        }
        best
    })
}

// Not reported: a `mul_add` with the accumulator as a factor is a recurrence, not a sum.
fn fold_mul_add_factor(values: &[f64], x: f64) -> f64 {
    values.iter().fold(0.0, |value, &coefficient| value.mul_add(x, coefficient))
}

// Not reported: a sum into `Result` of an integer.
fn sum_integers_into_result(values: &[u32]) -> Result<u32, String> {
    values.iter().map(|&value| Ok(value)).sum()
}

// Not reported: integer products, sums into `Option` of an integer, and integer folds that
// subtract or add through a method path.
fn product_integers(values: &[u64]) -> u64 {
    values.iter().product()
}

fn sum_integers_into_option(values: &[u32]) -> Option<u32> {
    values.iter().map(|&value| Some(value)).sum()
}

fn fold_sub_integers(values: &[i64]) -> i64 {
    values.iter().fold(0, |total, value| total - value)
}

fn fold_add_method_integers(values: &[u32]) -> u32 {
    values.iter().copied().fold(0, u32::add)
}

fn reduce_integers(values: &[u32]) -> Option<u32> {
    values.iter().copied().reduce(|total, value| total + value)
}

fn try_fold_integers(values: &[u32]) -> Option<u32> {
    values.iter().try_fold(0_u32, |total, &value| total.checked_add(value))
}

// Not reported: Horner's rule. The accumulator is multiplied, not added up, also when the
// coefficient is subtracted.
fn horner(coefficients: &[f64], x: f64) -> f64 {
    coefficients.iter().rev().fold(0.0, |value, &coefficient| value * x + coefficient)
}

fn horner_sub(coefficients: &[f64], x: f64) -> f64 {
    coefficients.iter().rfold(0.0, |value, &coefficient| value * x - coefficient)
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
    let reduced = values.iter().copied().reduce(f64::add);
    let product: f64 = values.iter().product();
    assert!(reduced.map(f64::to_bits) == Some(summed.to_bits()), "reduce adds in the same order");
    assert!(product.to_bits() == 6.0_f64.to_bits(), "a small product is exact");
    assert!(summed.to_bits() == folded.to_bits(), "both add in the same order");
    assert!(summed.to_bits() == reference::naive_sum(&values).to_bits(), "and so does this");
}
