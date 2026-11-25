//! The conservative update, and the template for every pencil kernel (ADR-0003).
//!
//! Copy this shape when you write a kernel:
//!
//! 1. a free function over slices and primitives, without `self`;
//! 2. lengths and arguments asserted before the loop, which also lets the compiler drop the
//!    bounds checks;
//! 3. a branch-free loop over `array_windows` or `chunks_exact`, with no raw indexing;
//! 4. a per-pencil backstop after the loop: every value written is finite (ADR-0004).
//!
//! The tests show the matching pattern: exact expected bits, a check that the solution *moved*
//! correctly, the conservation identity, and one test per assertion (the negative space).

use crate::reduce::all_finite;

/// Applies the conservative update `u_next = u - (dt/dx) (F_right - F_left)` to one pencil.
///
/// `flux` holds one value per face, so it is one longer than `state`. Each face flux is used by
/// both of its neighbouring cells, so the total of `state_next` differs from the total of `state`
/// only by the two boundary fluxes: discrete conservation holds to round-off
/// (`TIGER_STYLE.md`, Conservation and form).
pub fn update_pencil(state: &[f64], flux: &[f64], dt_over_dx: f64, state_next: &mut [f64]) {
    assert_eq!(flux.len(), state.len() + 1, "one flux per face");
    assert_eq!(state_next.len(), state.len(), "state buffers must match");
    assert!(dt_over_dx > 0.0, "time step must be positive");
    assert!(dt_over_dx.is_finite(), "time step must be finite");

    let cells = state_next.iter_mut().zip(state).zip(flux.array_windows::<2>());
    for ((next, &current), &[flux_left, flux_right]) in cells {
        *next = current - dt_over_dx * (flux_right - flux_left);
    }

    assert!(all_finite(state_next), "updated state must be finite; NaN is always a bug (ADR-0005)");
}

#[cfg(test)]
mod tests {
    use super::update_pencil;
    use crate::reduce::sum_compensated;

    fn bits(values: &[f64]) -> Vec<u64> {
        values.iter().map(|value| value.to_bits()).collect()
    }

    // Goal: the update matches hand-computed values bit for bit. The data are small integers and
    // the factor is a power of two, so every operation is exact.
    #[test]
    fn update_matches_hand_computed_values() {
        let state = [1.0, 2.0, 4.0];
        let flux = [0.0, 2.0, 6.0, 8.0];
        let mut state_next = [f64::NAN; 3];
        update_pencil(&state, &flux, 0.5, &mut state_next);
        assert_eq!(bits(&state_next), bits(&[0.0, 0.0, 3.0]));
    }

    // Goal: the solution moves correctly. A step advected with upwind fluxes at a Courant
    // number of exactly one moves by exactly one cell; a kernel that does nothing fails this.
    #[test]
    fn step_moves_one_cell_at_unit_courant_number() {
        let state = [1.0, 1.0, 0.0, 0.0];
        // Upwind flux for unit speed: the value of the cell to the left, with inflow 1.
        let flux = [1.0, 1.0, 1.0, 0.0, 0.0];
        let mut state_next = [f64::NAN; 4];
        update_pencil(&state, &flux, 1.0, &mut state_next);
        assert_eq!(bits(&state_next), bits(&[1.0, 1.0, 1.0, 0.0]));
    }

    // Goal: conservation. The interior fluxes telescope, so the total changes by exactly
    // `-(dt/dx) (F_last - F_first)`. The data are integers, so the identity holds bitwise.
    #[test]
    fn total_changes_only_by_the_boundary_fluxes() {
        let state = [3.0, 1.0, 4.0, 1.0, 5.0];
        let flux = [2.0, 7.0, 1.0, 8.0, 2.0, 8.0];
        let mut state_next = [f64::NAN; 5];
        update_pencil(&state, &flux, 0.25, &mut state_next);
        let change = sum_compensated(&state_next) - sum_compensated(&state);
        assert_eq!(change.to_bits(), (-0.25_f64 * (8.0 - 2.0)).to_bits());
    }

    // Goal: free-stream preservation. A uniform flux leaves every cell bitwise unchanged.
    #[test]
    fn uniform_flux_leaves_the_state_unchanged() {
        let state = [0.1, -2.5, 1.0e300];
        let flux = [7.25; 4];
        let mut state_next = [f64::NAN; 3];
        update_pencil(&state, &flux, 0.9, &mut state_next);
        assert_eq!(bits(&state_next), bits(&state));
    }

    // Goal: a flux array that does not hold one value per face is rejected before the loop.
    #[test]
    #[should_panic(expected = "one flux per face")]
    fn flux_length_is_asserted() {
        let mut state_next = [0.0; 2];
        update_pencil(&[1.0, 2.0], &[0.0, 0.0], 0.5, &mut state_next);
    }

    // Goal: an output buffer of the wrong length is rejected before the loop.
    #[test]
    #[should_panic(expected = "state buffers must match")]
    fn output_length_is_asserted() {
        let mut state_next = [0.0; 3];
        update_pencil(&[1.0, 2.0], &[0.0, 0.0, 0.0], 0.5, &mut state_next);
    }

    // Goal: a zero time step is a programmer error, not a silent no-op.
    #[test]
    #[should_panic(expected = "time step must be positive")]
    fn zero_time_step_is_rejected() {
        let mut state_next = [0.0; 1];
        update_pencil(&[1.0], &[0.0, 0.0], 0.0, &mut state_next);
    }

    // Goal: an infinite time step is rejected at the call.
    #[test]
    #[should_panic(expected = "time step must be finite")]
    fn infinite_time_step_is_rejected() {
        let mut state_next = [0.0; 1];
        update_pencil(&[1.0], &[0.0, 0.0], f64::INFINITY, &mut state_next);
    }

    // Goal: the per-pencil backstop. A NaN flux fails in this kernel, not three kernels later.
    #[test]
    #[should_panic(expected = "updated state must be finite")]
    fn nan_flux_trips_the_backstop() {
        let mut state_next = [0.0; 2];
        update_pencil(&[1.0, 2.0], &[0.0, f64::NAN, 0.0], 0.5, &mut state_next);
    }
}
