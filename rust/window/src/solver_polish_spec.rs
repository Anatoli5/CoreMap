use crate::gam_solver::{Fit, FitError, fit};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    rows: Vec<Vec<f64>>,
    targets: Vec<f64>,
    nonnegative: Vec<bool>,
    penalties: Vec<f64>,
    ridge: f64,
    coefficient_unique: bool,
    oracle: Oracle,
}

#[derive(Deserialize)]
struct Oracle {
    coefficients: Vec<f64>,
    predictions: Vec<f64>,
    objective: f64,
    normalized_kkt: f64,
    threshold: f64,
}

fn cases() -> Vec<Case> {
    serde_json::from_str::<Fixtures>(include_str!("solver-polish-oracles.json"))
        .expect("синтетический эталон SciPy BVLS")
        .cases
}

fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(actual.is_finite(), "неконечный результат {actual}");
    assert!(
        (actual - expected).abs() <= tolerance,
        "{actual} != {expected}; допуск {tolerance}"
    );
}

// Независимая проверка по исходным остаткам, без матрицы Грама решателя.
// Нормировка и относительный порог — исходный математический контракт.
fn validate(case: &Case, fitted: &Fit, target_scale: f64) {
    assert!(fitted.converged, "нет сходимости: {}", case.name);
    assert_eq!(fitted.coefficients.len(), case.nonnegative.len());
    for (&coefficient, &bounded) in fitted.coefficients.iter().zip(&case.nonnegative) {
        assert!(coefficient.is_finite());
        assert!(!bounded || coefficient >= 0.0, "нарушена граница: {coefficient}");
    }
    let predictions: Vec<_> = case
        .rows
        .iter()
        .map(|row| fitted.predict(row).expect("конечный прогноз"))
        .collect();
    let residuals: Vec<_> = predictions
        .iter()
        .zip(&case.targets)
        .map(|(prediction, y)| prediction - y)
        .collect();
    let count = f64::from(u32::try_from(case.rows.len()).expect("маленький эталон"));
    let objective = residuals.iter().map(|r| r * r).sum::<f64>() / (2.0 * count)
        + case.ridge
            * case
                .penalties
                .iter()
                .zip(&fitted.coefficients)
                .map(|(penalty, b)| penalty * b * b)
                .sum::<f64>()
            / 2.0;
    close(objective, case.oracle.objective, 1.0e-8 * target_scale * target_scale);
    for (&prediction, &expected) in predictions.iter().zip(&case.oracle.predictions) {
        close(prediction, expected, 1.0e-4 * target_scale);
    }
    let mut projected: f64 = 0.0;
    let mut rhs_max: f64 = 0.0;
    for (column, &coefficient) in fitted.coefficients.iter().enumerate() {
        let diagonal = case.rows.iter().map(|row| row[column] * row[column]).sum::<f64>() / count
            + case.ridge * case.penalties[column];
        let gradient = case
            .rows
            .iter()
            .zip(&residuals)
            .map(|(row, residual)| row[column] * residual)
            .sum::<f64>()
            / count
            + case.ridge * case.penalties[column] * coefficient;
        let rhs = case
            .rows
            .iter()
            .zip(&case.targets)
            .map(|(row, y)| row[column] * y)
            .sum::<f64>()
            / count;
        if diagonal == 0.0 {
            close(coefficient, 0.0, 0.0);
            close(gradient, 0.0, 0.0);
            close(rhs, 0.0, 0.0);
            continue;
        }
        let scale = diagonal.sqrt();
        let normalized_gradient = gradient / scale;
        let component = if case.nonnegative[column] && coefficient == 0.0 {
            normalized_gradient.min(0.0).abs()
        } else {
            normalized_gradient.abs()
        };
        projected = projected.max(component);
        rhs_max = rhs_max.max((rhs / scale).abs());
    }
    let threshold = 1.0e-8 * rhs_max;
    close(threshold, case.oracle.threshold, case.oracle.threshold * 1.0e-12);
    assert!(case.oracle.normalized_kkt <= threshold, "невалидный эталон");
    assert!(
        projected <= threshold * (1.0 + 1.0e-6),
        "ложная сходимость {}: KKT {projected} > {threshold}",
        case.name
    );
    assert!(fitted.projected_gradient.is_finite());
    assert!(fitted.projected_gradient <= threshold * (1.0 + 1.0e-6));
}

#[test]
fn polish_correlated_penalized_mixed_sign_problems_matches_saved_bvls() {
    for case in cases().into_iter().filter(|case| case.name.starts_with("correlated-")) {
        let fitted = fit(
            &case.rows,
            &case.targets,
            &case.nonnegative,
            &case.penalties,
            case.ridge,
        )
        .expect("конечная выпуклая задача");
        validate(&case, &fitted, 1.0);
        assert!(fitted.coefficients[0] < -1.0, "знаковый фон нельзя обрезать нулём");
        close(fitted.coefficients[3], 0.0, 1.0e-12);
        assert!(case.coefficient_unique);
    }
}

#[test]
fn polish_releases_a_bounded_coordinate_when_the_initial_free_set_is_wrong() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "active-set-release-seed1")
        .expect("эталон освобождения ограничения");
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("выпуклая задача со сменой активных ограничений");
    validate(&case, &fitted, 1.0);
    assert!(fitted.coefficients[1] < -0.5);
    assert!(fitted.coefficients[3] > 0.005);
    close(fitted.coefficients[4], 0.0, 1.0e-12);
    close(fitted.coefficients[5], 0.0, 1.0e-12);
}

#[test]
fn polish_binds_a_positive_coordinate_instead_of_rejecting_the_negative_free_candidate() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "active-set-bind-analytic")
        .expect("эталон добавления ограничения");
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("допустимый минимум на границе");
    validate(&case, &fitted, 1.0);
    close(fitted.coefficients[2], 0.0, 1.0e-12);
    assert!(fitted.coefficients[3] > 0.9);
}

#[test]
fn polish_active_set_oracles_survive_reversed_columns_and_a_tiny_response() {
    for mut case in cases().into_iter().filter(|case| case.name.starts_with("active-set-")) {
        let order: Vec<_> = (0..case.nonnegative.len()).rev().collect();
        for row in &mut case.rows {
            *row = order.iter().map(|&index| row[index]).collect();
        }
        case.nonnegative = order.iter().map(|&index| case.nonnegative[index]).collect();
        case.penalties = order.iter().map(|&index| case.penalties[index]).collect();
        case.oracle.coefficients = order.iter().map(|&index| case.oracle.coefficients[index]).collect();
        let factor = 1.0e-7;
        for y in &mut case.targets {
            *y *= factor;
        }
        for value in &mut case.oracle.coefficients {
            *value *= factor;
        }
        for value in &mut case.oracle.predictions {
            *value *= factor;
        }
        case.oracle.objective *= factor * factor;
        case.oracle.normalized_kkt *= factor;
        case.oracle.threshold *= factor;
        let fitted = fit(
            &case.rows,
            &case.targets,
            &case.nonnegative,
            &case.penalties,
            case.ridge,
        )
        .expect("тот же минимум при перестановке и смене единиц отклика");
        validate(&case, &fitted, factor);
    }
}

#[test]
fn polish_kkt_tolerance_has_no_absolute_floor_when_response_units_are_small() {
    let mut case = cases()
        .into_iter()
        .find(|case| case.name == "correlated-0.002")
        .expect("коррелированный эталон");
    let factor = 1.0e-7;
    for y in &mut case.targets {
        *y *= factor;
    }
    for value in &mut case.oracle.coefficients {
        *value *= factor;
    }
    for value in &mut case.oracle.predictions {
        *value *= factor;
    }
    case.oracle.objective *= factor * factor;
    case.oracle.normalized_kkt *= factor;
    case.oracle.threshold *= factor;
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("та же задача в меньших единицах отклика");
    validate(&case, &fitted, factor);
}

#[test]
fn polish_reoptimizes_the_active_boundary_and_keeps_a_negative_signed_slope() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "active-bound")
        .expect("граничный эталон");
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("граничный минимум существует");
    validate(&case, &fitted, 1.0);
    close(fitted.coefficients[0], 1.270_069_520_673_253_3, 1.0e-6);
    close(fitted.coefficients[1], -1.613_611_416_026_345_3, 1.0e-6);
    close(fitted.coefficients[2], 0.0, 1.0e-12);
    close(fitted.coefficients[3], 0.0, 1.0e-12);
}

#[test]
fn polish_boundary_result_does_not_depend_on_putting_bounded_columns_first() {
    let mut case = cases()
        .into_iter()
        .find(|case| case.name == "active-bound")
        .expect("граничный эталон");
    // У последнего ограниченного столбца положительный X'y: ранний проход
    // может ввести его в свободные переменные, хотя в оптимуме он равен нулю.
    let order = [3, 2, 1, 0];
    for row in &mut case.rows {
        *row = order.iter().map(|&index| row[index]).collect();
    }
    case.nonnegative = order.iter().map(|&index| case.nonnegative[index]).collect();
    case.penalties = order.iter().map(|&index| case.penalties[index]).collect();
    case.oracle.coefficients = order.iter().map(|&index| case.oracle.coefficients[index]).collect();
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("порядок столбцов не меняет минимум");
    validate(&case, &fitted, 1.0);
    close(fitted.coefficients[0], 0.0, 1.0e-12);
    close(fitted.coefficients[1], 0.0, 1.0e-12);
    assert!(fitted.coefficients[2] < -1.0);
}

#[test]
fn polish_singular_duplicate_columns_and_zero_column_keep_the_unique_prediction() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "singular-zero-column")
        .expect("вырожденный эталон");
    assert!(!case.coefficient_unique);
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("неединственные коэффициенты допускают точный прогноз");
    validate(&case, &fitted, 1.0);
    close(fitted.coefficients[0], -2.0, 1.0e-6);
    close(fitted.coefficients[1] + fitted.coefficients[2], 3.0, 1.0e-6);
    close(fitted.coefficients[3], 0.0, 0.0);
}

#[test]
fn polish_zero_data_column_with_positive_penalty_has_zero_signed_coefficient() {
    let case = cases()
        .into_iter()
        .find(|case| case.name == "ridge-zero-column")
        .expect("нулевой столбец со штрафом");
    let fitted = fit(
        &case.rows,
        &case.targets,
        &case.nonnegative,
        &case.penalties,
        case.ridge,
    )
    .expect("штрафная задача с нулевым столбцом");
    validate(&case, &fitted, 1.0);
    close(fitted.coefficients[2], 0.0, 0.0);
}

#[test]
fn polish_a_rejected_near_singular_candidate_retains_an_honest_failure() {
    let fitted = fit(
        &[vec![1.0, 1.0], vec![0.0, 0.0001]],
        &[0.0, 1.0],
        &[false, false],
        &[0.0, 0.0],
        0.0,
    )
    .expect("исходный контракт возвращает несошедшийся конечный результат");
    assert!(!fitted.converged, "отказ защитной проверки нельзя называть сходимостью");
    assert_eq!(fitted.iterations, 10_000);
    assert!(fitted.coefficients.iter().all(|b| b.is_finite()));
    let residual_zero = fitted.coefficients[0] + fitted.coefficients[1];
    let residual_one = 0.0001 * fitted.coefficients[1] - 1.0;
    let first = (0.5 * residual_zero) / 0.5_f64.sqrt();
    let second = (0.5 * residual_zero + 0.00005 * residual_one) / 0.500_000_005_f64.sqrt();
    assert!(first.abs().max(second.abs()) > 1.0e-6);
    assert!(fitted.projected_gradient > 1.0e-6);
}

#[test]
fn polish_does_not_turn_original_input_errors_into_claimed_convergence() {
    assert!(matches!(fit(&[], &[], &[], &[], 0.0), Err(FitError::EmptyInput)));
    assert!(matches!(
        fit(&[vec![1.0]], &[1.0], &[], &[0.0], 0.0),
        Err(FitError::Dimensions)
    ));
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            fit(&[vec![invalid]], &[1.0], &[false], &[0.0], 0.0),
            Err(FitError::NonFiniteInput)
        ));
        assert!(matches!(
            fit(&[vec![1.0]], &[invalid], &[false], &[0.0], 0.0),
            Err(FitError::NonFiniteInput)
        ));
    }
    assert!(matches!(
        fit(&[vec![1.0]], &[1.0], &[false], &[-1.0], 1.0),
        Err(FitError::InvalidPenalty)
    ));
    assert!(fit(&[vec![1.0]], &[1.0], &[false], &[1.0], -1.0).is_err());
}
