//! Deterministic reductions (ADR-0005).
//!
//! A naive sequential sum is deterministic but slow, because the compiler may not reorder
//! floating-point additions. Letting it reorder (`algebraic_add`) is fast but not reproducible.
//! The functions here fix the order of operations in code, so they are both fast and bitwise
//! reproducible on every platform.

/// Number of independent accumulators in [`sum_fixed`]. Changing it changes results.
const SUM_LANES: usize = 8;

/// Sums `values` with 8 accumulator lanes combined in a fixed tree.
///
/// The result depends only on the input values and their order, never on the platform or the
/// optimiser. Use this for every sum over cells. For conservation totals, where round-off
/// matters, use [`sum_compensated`].
#[must_use]
pub fn sum_fixed(values: &[f64]) -> f64 {
    let (chunks, remainder) = values.as_chunks::<SUM_LANES>();
    assert!(remainder.len() < SUM_LANES, "the remainder is shorter than one chunk");
    assert_eq!(chunks.len() * SUM_LANES + remainder.len(), values.len(), "chunks cover the input");
    let mut lanes = [0.0_f64; SUM_LANES];
    for chunk in chunks {
        for (lane, &value) in lanes.iter_mut().zip(chunk) {
            *lane += value;
        }
    }
    // Fixed combination tree. Lane i pairs with lane i + 4, then neighbours pair up.
    let [l0, l1, l2, l3, l4, l5, l6, l7] = lanes;
    let mut total = ((l0 + l4) + (l1 + l5)) + ((l2 + l6) + (l3 + l7));
    for &value in remainder {
        total += value;
    }
    assert!(total.is_finite(), "sum must be finite; NaN or overflow is a bug (ADR-0005)");
    total
}

/// Sums `values` with Neumaier compensation, for conservation totals.
///
/// Slower than [`sum_fixed`], but the error does not grow with the number of values, so totals
/// can be compared to round-off between time steps.
#[must_use]
pub fn sum_compensated(values: &[f64]) -> f64 {
    let mut sum = 0.0_f64;
    let mut compensation = 0.0_f64;
    for &value in values {
        let total = sum + value;
        if sum.abs() >= value.abs() {
            compensation += (sum - total) + value;
        } else {
            compensation += (value - total) + sum;
        }
        sum = total;
    }
    assert!(compensation.is_finite(), "compensation must be finite");
    let result = sum + compensation;
    assert!(result.is_finite(), "sum must be finite; NaN or overflow is a bug (ADR-0005)");
    result
}

/// Returns true if every value is finite: neither NaN nor infinite.
///
/// This is the per-pencil backstop of ADR-0004: assert it once after a kernel's loop, not per
/// cell. It visits every value without an early exit, so the compiler can vectorise it.
#[must_use]
pub fn all_finite(values: &[f64]) -> bool {
    values.iter().fold(true, |finite, value| finite & value.is_finite())
}

#[cfg(test)]
mod tests {
    use super::{all_finite, sum_compensated, sum_fixed};

    // Goal: one NaN or infinity anywhere in the slice is found; an empty slice is finite.
    #[test]
    fn all_finite_finds_a_single_bad_value() {
        assert!(all_finite(&[]), "an empty pencil has no bad value");
        assert!(all_finite(&[0.0, -1.0e300, f64::MIN_POSITIVE]), "extreme values are finite");
        assert!(!all_finite(&[1.0, f64::NAN, 2.0]), "NaN in the middle is found");
        assert!(!all_finite(&[f64::INFINITY, 1.0]), "infinity at the start is found");
        assert!(!all_finite(&[1.0, f64::NEG_INFINITY]), "infinity at the end is found");
    }

    // Goal: integers below 2^53 sum exactly, whatever the order, including a remainder that
    // does not fill a whole chunk.
    #[test]
    fn sum_fixed_is_exact_on_small_integers() {
        let values: Vec<f64> = (1..=1003).map(f64::from).collect();
        assert_eq!(sum_fixed(&values).to_bits(), 503_506.0_f64.to_bits());
        assert_eq!(sum_fixed(&[]).to_bits(), 0.0_f64.to_bits());
    }

    // Goal: the lane order is part of the contract. The expected bits were computed
    // independently (Python, same lane tree, IEEE f64). If they change, results change
    // everywhere, and that needs a human (ADR-0005, ADR-0008).
    #[test]
    fn sum_fixed_order_is_stable() {
        let values: Vec<f64> = (1_u32..=100).map(|i| 1.0 / f64::from(i)).collect();
        assert_eq!(sum_fixed(&values).to_bits(), 0x4014_bfdf_e459_1243, "harmonic sum H_100");
    }

    // Goal: compensation recovers the small terms that a plain sum loses.
    #[test]
    fn sum_compensated_recovers_lost_terms() {
        let values = [1.0_f64, 1.0e100, 1.0, -1.0e100];
        assert_eq!(sum_compensated(&values).to_bits(), 2.0_f64.to_bits());
    }

    // Goal: NaN input is rejected at the reduction (negative space).
    #[test]
    #[should_panic(expected = "sum must be finite")]
    fn sum_fixed_rejects_nan() {
        let _sum = sum_fixed(&[1.0, f64::NAN]);
    }
}
