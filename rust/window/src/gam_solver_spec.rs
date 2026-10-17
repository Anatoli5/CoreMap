// Независимые точные решения малых задач, без чтения реализации решателя.
use crate::gam_solver::{FitError, fit};

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1.0e-6, "expected {expected}, got {actual}");
}

fn line_rows() -> Vec<Vec<f64>> {
    vec![vec![1.0, 0.0], vec![1.0, 1.0], vec![1.0, 2.0]]
}

#[test]
fn signed_least_squares_recovers_an_exact_line() {
    let fitted = fit(&line_rows(), &[1.0, 3.0, 5.0], &[false, false], &[0.0, 0.0], 0.0).expect("full rank data");
    assert!(fitted.converged);
    close(fitted.coefficients[0], 1.0);
    close(fitted.coefficients[1], 2.0);
    close(fitted.predict(&[1.0, 1.5]).expect("valid prediction"), 4.0);
}

#[test]
fn signed_background_can_be_negative_while_own_slope_is_nonnegative() {
    let fitted =
        fit(&line_rows(), &[-3.0, -1.0, 1.0], &[false, true], &[0.0, 0.0], 0.0).expect("mixed constraint data");
    assert!(fitted.converged);
    close(fitted.coefficients[0], -3.0);
    close(fitted.coefficients[1], 2.0);
}

#[test]
fn boundary_solution_reoptimizes_free_coefficients_instead_of_clipping_ols() {
    let fitted =
        fit(&line_rows(), &[3.0, 2.0, 1.0], &[false, true], &[0.0, 0.0], 0.0).expect("boundary solution exists");
    assert!(fitted.converged);
    // Без ограничения было бы [3, −1]; простая обрезка дала бы неверное [3, 0].
    close(fitted.coefficients[0], 2.0);
    close(fitted.coefficients[1], 0.0);
    close(fitted.predict(&[1.0, 100.0]).expect("valid prediction"), 2.0);
}

#[test]
fn penalties_use_mean_residual_scale_and_per_coefficient_weights() {
    let rows = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
    let fitted = fit(&rows, &[4.0, 6.0], &[true, true], &[1.0, 3.0], 0.5).expect("diagonal ridge solution");
    assert!(fitted.converged);
    // Для двух строк: (X'X/N + 0.5 diag(1,3)) β = X'y/N.
    close(fitted.coefficients[0], 2.0);
    close(fitted.coefficients[1], 1.5);
}

#[test]
fn a_constrained_negative_signal_has_zero_optimum() {
    let rows = vec![vec![1.0], vec![1.0]];
    let fitted = fit(&rows, &[-2.0, -4.0], &[true], &[0.0], 0.0).expect("zero boundary optimum");
    assert!(fitted.converged);
    close(fitted.coefficients[0], 0.0);
    assert!(fitted.projected_gradient.is_finite());
    assert!(fitted.projected_gradient < 1.0e-6);
}

#[test]
fn changing_column_units_preserves_predictions() {
    let rows = vec![vec![1.0, 1.0], vec![1.0, 2.0], vec![1.0, 3.0]];
    let scaled_rows = vec![vec![1.0, 1.0e6], vec![1.0, 2.0e6], vec![1.0, 3.0e6]];
    let fitted = fit(&rows, &[3.0, 5.0, 7.0], &[false, true], &[0.0, 0.0], 0.0).expect("original units");
    let scaled = fit(&scaled_rows, &[3.0, 5.0, 7.0], &[false, true], &[0.0, 0.0], 0.0).expect("scaled units");
    assert!(fitted.converged);
    assert!(scaled.converged);
    close(fitted.predict(&[1.0, 2.5]).expect("valid prediction"), 6.0);
    close(scaled.predict(&[1.0, 2.5e6]).expect("valid prediction"), 6.0);
}

#[test]
fn an_unobserved_column_does_not_break_the_observed_prediction() {
    let rows = vec![vec![1.0, 0.0], vec![2.0, 0.0], vec![3.0, 0.0]];
    let fitted = fit(&rows, &[2.0, 4.0, 6.0], &[false, true], &[0.0, 0.0], 0.0)
        .expect("rank deficient prediction remains defined");
    assert!(fitted.converged);
    close(fitted.predict(&[2.5, 0.0]).expect("observed column"), 5.0);
    assert!(fitted.coefficients.iter().all(|value| value.is_finite()));
}

#[test]
fn invalid_shapes_values_penalties_and_prediction_inputs_are_explicit() {
    assert!(matches!(fit(&[], &[], &[], &[], 0.0), Err(FitError::EmptyInput)));
    assert!(matches!(
        fit(&[vec![1.0]], &[], &[false], &[0.0], 0.0),
        Err(FitError::Dimensions)
    ));
    assert!(matches!(
        fit(&[vec![1.0], vec![1.0, 2.0]], &[1.0, 2.0], &[false], &[0.0], 0.0),
        Err(FitError::Dimensions)
    ));
    assert!(matches!(
        fit(&[vec![f64::NAN]], &[1.0], &[false], &[0.0], 0.0),
        Err(FitError::NonFiniteInput)
    ));
    assert!(matches!(
        fit(&[vec![1.0]], &[f64::INFINITY], &[false], &[0.0], 0.0),
        Err(FitError::NonFiniteInput)
    ));
    assert!(matches!(
        fit(&[vec![1.0]], &[1.0], &[false], &[-1.0], 1.0),
        Err(FitError::InvalidPenalty)
    ));
    assert!(fit(&[vec![1.0]], &[1.0], &[false], &[1.0], -1.0).is_err());
    let fitted = fit(&[vec![1.0]], &[1.0], &[false], &[0.0], 0.0).expect("one observed coefficient");
    assert!(fitted.predict(&[]).is_none());
    assert!(fitted.predict(&[f64::NAN]).is_none());
    assert!(fitted.predict(&[1.0, 2.0]).is_none());
}
