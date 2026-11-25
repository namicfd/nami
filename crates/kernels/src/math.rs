//! The only entry point for non-elementary math functions (ADR-0005).
//!
//! `core` has no `sqrt` and similar functions on stable Rust, and the `std` versions call the
//! platform libm, whose results differ between platforms. Every function here wraps the pure-Rust
//! `libm` crate and asserts its domain, so that an invalid argument fails loudly instead of
//! producing a NaN that travels through the solver.
//!
//! Add a wrapper here when a kernel first needs a function; do not call `libm` directly.

/// Square root of a non-negative, finite value. Correctly rounded on every platform.
#[must_use]
pub fn sqrt(value: f64) -> f64 {
    assert!(value.is_finite(), "sqrt argument must be finite");
    assert!(value >= 0.0, "sqrt argument must be non-negative");
    let root = libm::sqrt(value);
    assert!(root >= 0.0, "sqrt result must be non-negative");
    root
}

#[cfg(test)]
mod tests {
    use super::sqrt;

    // Goal: exact squares give exact roots (IEEE sqrt is correctly rounded).
    #[test]
    fn sqrt_of_exact_squares_is_exact() {
        assert_eq!(sqrt(0.0).to_bits(), 0.0_f64.to_bits());
        assert_eq!(sqrt(2.25).to_bits(), 1.5_f64.to_bits());
        assert_eq!(sqrt(1.0e300).to_bits(), 1.0e150_f64.to_bits());
    }

    // Goal: the negative space is asserted; a negative argument is a programmer error.
    #[test]
    #[should_panic(expected = "sqrt argument must be non-negative")]
    fn sqrt_rejects_negative_arguments() {
        let _root = sqrt(-1.0);
    }

    // Goal: a NaN argument fails at the call, not three kernels later.
    #[test]
    #[should_panic(expected = "sqrt argument must be finite")]
    fn sqrt_rejects_nan() {
        let _root = sqrt(f64::NAN);
    }
}
