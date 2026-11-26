//! Copy of the `kernels::math` pattern: the only entry point for `libm`, with domain asserts.
//!
//! The default build is the faithful copy: `assert!` on the argument and the result, and
//! `libm::sqrt`. Two features change it, each only as a measured data point:
//! `sqrt-debug-asserts` turns the asserts into `debug_assert!`, and `intrinsic-sqrt` calls
//! `f64::sqrt` (the LLVM `sqrt` intrinsic, correctly rounded per IEEE 754, like `libm::sqrt`).

/// Square root of a non-negative, finite value. Correctly rounded on every platform.
#[inline(always)]
pub fn sqrt(value: f64) -> f64 {
    #[cfg(not(feature = "sqrt-debug-asserts"))]
    {
        assert!(value.is_finite(), "sqrt argument must be finite");
        assert!(value >= 0.0, "sqrt argument must be non-negative");
    }
    #[cfg(feature = "sqrt-debug-asserts")]
    {
        debug_assert!(value.is_finite(), "sqrt argument must be finite");
        debug_assert!(value >= 0.0, "sqrt argument must be non-negative");
    }
    let root = root(value);
    #[cfg(not(feature = "sqrt-debug-asserts"))]
    assert!(root >= 0.0, "sqrt result must be non-negative");
    #[cfg(feature = "sqrt-debug-asserts")]
    debug_assert!(root >= 0.0, "sqrt result must be non-negative");
    root
}

/// Square root with the same three checks as `sqrt`, accumulated into `ok` without branching
/// (configuration E). The caller asserts `ok` once per row, so the per-face loop has no exit.
#[inline(always)]
pub fn sqrt_deferred(value: f64, ok: &mut bool) -> f64 {
    *ok &= value.is_finite() & (value >= 0.0);
    let root = root(value);
    *ok &= root >= 0.0;
    root
}

#[cfg(not(feature = "intrinsic-sqrt"))]
#[inline(always)]
fn root(value: f64) -> f64 {
    libm::sqrt(value)
}

#[cfg(feature = "intrinsic-sqrt")]
#[inline(always)]
#[expect(
    clippy::disallowed_methods,
    reason = "data point: IEEE squareRoot through the LLVM intrinsic, which can vectorise"
)]
fn root(value: f64) -> f64 {
    value.sqrt()
}

/// Sine of a finite argument (setup only).
pub fn sin(value: f64) -> f64 {
    assert!(value.is_finite(), "sin argument must be finite");
    let result = libm::sin(value);
    assert!((-1.0..=1.0).contains(&result), "sin result must lie in [-1, 1]");
    result
}

/// Cosine of a finite argument (setup only).
pub fn cos(value: f64) -> f64 {
    assert!(value.is_finite(), "cos argument must be finite");
    let result = libm::cos(value);
    assert!((-1.0..=1.0).contains(&result), "cos result must lie in [-1, 1]");
    result
}

/// Arctangent of a finite argument (setup only).
pub fn atan(value: f64) -> f64 {
    assert!(value.is_finite(), "atan argument must be finite");
    let result = libm::atan(value);
    assert!(result.abs() <= core::f64::consts::FRAC_PI_2, "atan result must lie in [-pi/2, pi/2]");
    result
}
