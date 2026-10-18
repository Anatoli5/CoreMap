// Независимые точные случаи запросов; реализация curve_query не читалась.
use crate::conditional::{CurveLoad, Error, Surface};
use crate::conditional_data::Prepared;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn close(actual: f64, expected: f64) {
    assert!(actual.is_finite(), "нечисловой результат: {actual}");
    assert!(
        (actual - expected).abs() <= 1.0e-10 * expected.abs().max(1.0),
        "ожидалось {expected}, получено {actual}"
    );
}

fn current_json() -> Value {
    json!({
        "metadata": {
            "knots": [0.0, 10.0],
            "contexts": {"fan_cpu": [0.0, 10.0]},
            "own_bands": []
        },
        "bands": [],
        "weights": [1.0],
        "context_basis": [
            {"context": "fan_cpu", "side": 0, "lag": null},
            {"context": "fan_cpu", "side": 1, "lag": null}
        ],
        "background_basis": [{
            "kind": "intercept", "name": "intercept",
            "group": "background", "penalty": 0.00001
        }],
        "context_coefficients": [[8.0, 2.0]],
        "background_coefficients": [30.0]
    })
}

fn decode(value: Value) -> Surface {
    serde_json::from_value(value).expect("точная модель публичного формата")
}

fn data(ratio: &[f64], busy: &[f64], fan: &[f64]) -> Prepared {
    assert_eq!(ratio.len(), busy.len());
    assert_eq!(ratio.len(), fan.len());
    let count = ratio.len();
    let mut contexts = BTreeMap::new();
    contexts.insert("fan_cpu".into(), fan.to_vec());
    Prepared {
        times: (0..count)
            .map(|i| i64::try_from(i).expect("малый пример") * 1000)
            .collect(),
        segments: vec![0; count],
        ratio: ratio.to_vec(),
        busy: busy.to_vec(),
        target: vec![30.0; count],
        contexts,
    }
}

fn own_lag_json() -> Value {
    let mut value = current_json();
    value["metadata"]["own_bands"] = json!([[0, 1]]);
    value["weights"] = json!([0.0, 1.0]);
    value["context_basis"] = json!([{"constant": true}]);
    value["context_coefficients"] = json!([[8.0]]);
    value
}

#[test]
fn observed_query_averages_joint_fan_and_busy_and_divides_averages() {
    let surface = decode(current_json());
    let source = data(&[5.0, 5.0], &[1.0, 0.0], &[0.0, 10.0]);
    let curve = surface
        .curve_query(&source, &[true, true], &[10.0], CurveLoad::ObservedHistory)
        .expect("наблюдаемая история");
    assert_eq!(curve.rows, 2);
    close(curve.mean_busy, 0.5);
    let point = &curve.points[0];
    close(point.ratio, 10.0);
    // Среднее произведений: (8×1 + 2×0)/2=4; произведение средних дало бы 2.5.
    close(point.own_heat, 4.0);
    close(point.performance, 5.0);
    close(point.efficiency.expect("положительный знаменатель"), 1.25);
}

#[test]
fn sustained_full_changes_busy_but_preserves_the_same_context_cohort() {
    let surface = decode(current_json());
    let source = data(&[5.0, 5.0], &[1.0, 0.0], &[0.0, 10.0]);
    let curve = surface
        .curve_query(&source, &[true, true], &[10.0], CurveLoad::SustainedFull)
        .expect("условный полный режим");
    assert_eq!(curve.rows, 2);
    close(curve.mean_busy, 1.0);
    let point = &curve.points[0];
    close(point.own_heat, 5.0);
    close(point.performance, 10.0);
    close(point.efficiency.expect("положительный знаменатель"), 2.0);
}

#[test]
fn zero_ratio_has_no_efficiency_and_small_positive_heat_is_not_clamped() {
    let surface = decode(current_json());
    let source = data(&[5.0], &[1.0], &[0.0]);
    for mode in [
        CurveLoad::ObservedHistory,
        CurveLoad::SustainedFull,
        CurveLoad::Sustained(0.25),
        CurveLoad::Sustained(0.0),
    ] {
        let curve = surface
            .curve_query(&source, &[true], &[0.0], mode)
            .expect("нулевой множитель допустим");
        close(curve.points[0].own_heat, 0.0);
        close(curve.points[0].performance, 0.0);
        assert!(curve.points[0].efficiency.is_none());
    }
    let mut tiny = current_json();
    tiny["context_coefficients"] = json!([[8.0e-14, 2.0e-14]]);
    let curve = decode(tiny)
        .curve_query(&source, &[true], &[10.0], CurveLoad::ObservedHistory)
        .expect("малый положительный нагрев");
    close(curve.points[0].own_heat / 8.0e-14, 1.0);
    close(curve.points[0].efficiency.expect("без порога epsilon"), 1.25e14);
}

#[test]
fn actual_invalid_rows_stay_excluded_even_when_counterfactual_ratio_is_known() {
    let surface = decode(current_json());
    let source = data(
        &[5.0, f64::NAN, 5.0, 5.0],
        &[1.0, 1.0, f64::NAN, 1.0],
        &[0.0, 0.0, 0.0, f64::NAN],
    );
    for (mode, busy, heat) in [
        (CurveLoad::ObservedHistory, 1.0, 8.0),
        (CurveLoad::SustainedFull, 1.0, 8.0),
        (CurveLoad::Sustained(0.25), 0.25, 2.0),
        (CurveLoad::Sustained(0.0), 0.0, 0.0),
    ] {
        let curve = surface
            .curve_query(&source, &[true; 4], &[0.0, 10.0, 5.0], mode)
            .expect("одна исходно годная строка");
        assert_eq!(curve.rows, 1);
        close(curve.mean_busy, busy);
        close(curve.points[1].own_heat, heat);
        close(curve.points[2].own_heat, heat / 2.0);
    }
}

#[test]
fn selected_rows_are_uniform_not_reweighted_by_segment_or_elapsed_gap() {
    let surface = decode(current_json());
    let mut source = data(&[5.0; 3], &[1.0, 0.0, 1.0], &[0.0; 3]);
    source.times = vec![0, 1000, 100_000];
    source.segments = vec![0, 0, 1];
    let curve = surface
        .curve_query(&source, &[true; 3], &[10.0], CurveLoad::ObservedHistory)
        .expect("три секундные корзины из двух эпизодов");
    assert_eq!(curve.rows, 3);
    close(curve.mean_busy, 2.0 / 3.0);
    close(curve.points[0].own_heat, 16.0 / 3.0);
    close(curve.points[0].performance, 20.0 / 3.0);
}

#[test]
fn own_ratio_is_replaced_through_history_but_actual_busy_history_remains() {
    let surface = decode(own_lag_json());
    let retained = data(&[0.0, 2.0], &[1.0, 0.0], &[0.0, 0.0]);
    let cold = data(&[0.0, 2.0], &[0.0, 0.0], &[0.0, 0.0]);
    let chosen = [false, true];
    let warm_curve = surface
        .curve_query(&retained, &chosen, &[10.0], CurveLoad::ObservedHistory)
        .expect("предыдущая работа сохраняется");
    let cold_curve = surface
        .curve_query(&cold, &chosen, &[10.0], CurveLoad::ObservedHistory)
        .expect("предыдущей работы нет");
    close(warm_curve.mean_busy, 0.0);
    close(warm_curve.points[0].own_heat, 8.0);
    close(warm_curve.points[0].performance, 0.0);
    close(warm_curve.points[0].efficiency.expect("0/8 определено"), 0.0);
    close(cold_curve.points[0].own_heat, 0.0);
    assert!(cold_curve.points[0].efficiency.is_none());
    for source in [&retained, &cold] {
        let full = surface
            .curve_query(source, &chosen, &[10.0], CurveLoad::SustainedFull)
            .expect("полный режим заменяет также прошлую занятость");
        close(full.points[0].own_heat, 8.0);
        close(full.points[0].performance, 10.0);
    }
}

#[test]
fn context_nonlinearity_and_history_are_preserved_by_both_query_modes() {
    let mut value = current_json();
    value["metadata"]["own_bands"] = json!([[0, 2]]);
    value["weights"] = json!([1.0, 0.0]);
    value["bands"] = json!([[0, 2]]);
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 2]}]);
    value["context_coefficients"] = json!([[8.0]]);
    let surface = decode(value);
    let source = data(&[5.0; 3], &[1.0; 3], &[-10.0, 10.0, 10.0]);
    for mode in [CurveLoad::ObservedHistory, CurveLoad::SustainedFull] {
        let curve = surface
            .curve_query(&source, &[false, false, true], &[10.0], mode)
            .expect("сохранённая история вентилятора");
        // Сначала ограниченные шапочки 1 и 0, затем среднее 0.5; не шапочка среднего.
        close(curve.points[0].own_heat, 4.0);
    }
}

#[test]
fn unknown_context_history_is_not_filled_from_current_or_future_values() {
    let mut value = own_lag_json();
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 1]}]);
    let surface = decode(value);
    let source = data(&[5.0; 3], &[1.0; 3], &[f64::NAN, 0.0, 0.0]);
    for mode in [
        CurveLoad::ObservedHistory,
        CurveLoad::SustainedFull,
        CurveLoad::Sustained(0.25),
        CurveLoad::Sustained(0.0),
    ] {
        assert!(matches!(
            surface.curve_query(&source, &[false, true, false], &[10.0], mode),
            Err(Error::InsufficientRows)
        ));
    }
}

#[test]
fn episode_start_cannot_borrow_past_from_another_segment_or_future() {
    let surface = decode(own_lag_json());
    let mut source = data(&[5.0; 4], &[1.0; 4], &[0.0; 4]);
    source.times = vec![0, 1000, 100_000, 101_000];
    source.segments = vec![0, 0, 1, 1];
    for mode in [
        CurveLoad::SustainedFull,
        CurveLoad::Sustained(0.25),
        CurveLoad::Sustained(0.0),
    ] {
        assert!(matches!(
            surface.curve_query(&source, &[false, false, true, false], &[10.0], mode),
            Err(Error::InsufficientRows)
        ));
    }
    let curve = surface
        .curve_query(&source, &[true; 4], &[10.0], CurveLoad::ObservedHistory)
        .expect("по одной годной строке на эпизод");
    assert_eq!(curve.rows, 2);
    close(curve.points[0].own_heat, 8.0);
}

#[test]
fn future_mutations_do_not_change_selected_past_and_query_preserves_inputs() {
    let surface = decode(own_lag_json());
    let source = data(&[1.0, 2.0, 3.0, 4.0], &[1.0, 0.0, 0.5, 1.0], &[0.0; 4]);
    let before_model = serde_json::to_value(&surface).expect("модель до запроса");
    let before_source = serde_json::to_value(&source).expect("данные до запроса");
    let before_predictions =
        serde_json::to_value(surface.predict(&source).expect("исходный прогноз")).expect("исходные числа");
    let first = surface
        .curve_query(
            &source,
            &[false, true, false, false],
            &[10.0],
            CurveLoad::ObservedHistory,
        )
        .expect("выбран прошлый момент");
    let mut changed = data(&[1.0, 2.0, 90.0, 90.0], &[1.0, 0.0, 0.0, 0.0], &[0.0, 0.0, 10.0, 10.0]);
    changed.target.fill(999.0);
    let second = surface
        .curve_query(
            &changed,
            &[false, true, false, false],
            &[10.0],
            CurveLoad::ObservedHistory,
        )
        .expect("будущее изменено");
    close(first.points[0].own_heat, second.points[0].own_heat);
    close(first.points[0].performance, second.points[0].performance);
    assert_eq!(serde_json::to_value(&surface).expect("модель после"), before_model);
    assert_eq!(serde_json::to_value(&source).expect("данные после"), before_source);
    assert_eq!(
        serde_json::to_value(surface.predict(&source).expect("неизменённый прогноз")).expect("повторные числа"),
        before_predictions
    );
}

#[test]
fn selection_controls_donors_and_query_order_does_not_change_the_cohort() {
    let surface = decode(current_json());
    let source = data(&[5.0, 5.0], &[1.0, 0.0], &[0.0, 10.0]);
    let curve = surface
        .curve_query(
            &source,
            &[false, true],
            &[10.0, 0.0, 5.0, 10.0],
            CurveLoad::SustainedFull,
        )
        .expect("выбран один исходный режим");
    assert_eq!(curve.rows, 1);
    assert_eq!(curve.points.len(), 4);
    for (point, (ratio, heat)) in curve
        .points
        .iter()
        .zip([(10.0, 2.0), (0.0, 0.0), (5.0, 1.0), (10.0, 2.0)])
    {
        close(point.ratio, ratio);
        close(point.own_heat, heat);
    }
}

#[test]
fn empty_cohort_bad_selection_and_invalid_ratios_are_explicit_errors() {
    let surface = decode(current_json());
    let source = data(&[5.0], &[1.0], &[0.0]);
    assert!(matches!(
        surface.curve_query(&source, &[false], &[10.0], CurveLoad::ObservedHistory),
        Err(Error::InsufficientRows)
    ));
    assert!(matches!(
        surface.curve_query(&source, &[], &[10.0], CurveLoad::ObservedHistory),
        Err(Error::Dimensions)
    ));
    for ratio in [f64::NAN, f64::INFINITY, -1.0] {
        assert!(
            surface
                .curve_query(&source, &[true], &[ratio], CurveLoad::ObservedHistory)
                .is_err()
        );
    }
}

#[test]
fn explicit_nonconvergence_cannot_be_presented_as_a_curve() {
    let mut value = current_json();
    value["fit_status"] = json!({
        "converged": false, "outer_iterations": 0,
        "max_kernel_change": null, "stop": "iteration_limit"
    });
    let surface = decode(value);
    let source = data(&[5.0], &[1.0], &[0.0]);
    for mode in [
        CurveLoad::ObservedHistory,
        CurveLoad::SustainedFull,
        CurveLoad::Sustained(0.25),
        CurveLoad::Sustained(0.0),
    ] {
        assert!(matches!(
            surface.curve_query(&source, &[true], &[10.0], mode),
            Err(Error::NotConverged)
        ));
    }
}

#[test]
fn sustained_quarter_load_has_exact_heat_performance_and_efficiency_on_the_known_surface() {
    let surface = decode(current_json());
    let source = data(&[5.0], &[0.7], &[0.0]);
    let curve = surface
        .curve_query(&source, &[true], &[10.0], CurveLoad::Sustained(0.25))
        .expect("заданная четверть полной занятости");
    assert_eq!(curve.rows, 1);
    close(curve.mean_busy, 0.25);
    close(curve.points[0].ratio, 10.0);
    close(curve.points[0].own_heat, 2.0);
    close(curve.points[0].performance, 2.5);
    close(curve.points[0].efficiency.expect("2.5/2 определено"), 1.25);
}

#[test]
fn sustained_load_accepts_unrounded_finite_values_including_zero_and_one() {
    let surface = decode(current_json());
    let source = data(&[5.0], &[0.7], &[0.0]);
    for busy in [0.0, -0.0, f64::MIN_POSITIVE, 1.0e-15, 0.137_625, 0.25, 0.999_999, 1.0] {
        let curve = surface
            .curve_query(&source, &[true], &[10.0], CurveLoad::Sustained(busy))
            .expect("любая конечная занятость в допустимом интервале");
        assert_eq!(curve.rows, 1);
        close(curve.mean_busy, busy);
        let point = &curve.points[0];
        if busy > 0.0 {
            // Эти числа заданы точной искусственной моделью, а не измерены на процессоре.
            close(point.own_heat / busy, 8.0);
            close(point.performance / busy, 10.0);
            close(point.efficiency.expect("положительный нагрев не обрезается"), 1.25);
        } else {
            close(point.own_heat, 0.0);
            close(point.performance, 0.0);
            assert!(point.efficiency.is_none());
        }
    }
}

#[test]
fn sustained_full_is_backward_compatible_with_explicit_one_for_current_and_past_busy() {
    let source = data(&[1.0, 3.0, 7.0, 2.0], &[1.0, 0.25, 0.0, 0.75], &[0.0, 5.0, 10.0, 3.0]);
    for surface in [decode(current_json()), decode(own_lag_json())] {
        let legacy = surface
            .curve_query(&source, &[true; 4], &[10.0, 0.0, 5.0], CurveLoad::SustainedFull)
            .expect("прежнее имя полной занятости");
        let explicit = surface
            .curve_query(&source, &[true; 4], &[10.0, 0.0, 5.0], CurveLoad::Sustained(1.0))
            .expect("явно заданная полная занятость");
        assert_eq!(legacy.rows, explicit.rows);
        close(legacy.mean_busy, explicit.mean_busy);
        assert_eq!(legacy.points.len(), explicit.points.len());
        for (a, b) in legacy.points.iter().zip(&explicit.points) {
            close(a.ratio, b.ratio);
            close(a.own_heat, b.own_heat);
            close(a.performance, b.performance);
            assert_eq!(a.efficiency.is_none(), b.efficiency.is_none());
            if let (Some(a), Some(b)) = (a.efficiency, b.efficiency) {
                close(a, b);
            }
        }
    }
}

#[test]
fn fixed_load_replaces_current_and_past_busy_while_observed_mode_retains_heat() {
    let surface = decode(own_lag_json());
    let source = data(&[0.0, 2.0], &[1.0, 0.0], &[0.0, 0.0]);
    for (mode, heat, performance, efficiency) in [
        (CurveLoad::ObservedHistory, 8.0, 0.0, Some(0.0)),
        (CurveLoad::Sustained(0.25), 2.0, 2.5, Some(1.25)),
        (CurveLoad::Sustained(1.0), 8.0, 10.0, Some(1.25)),
        (CurveLoad::Sustained(0.0), 0.0, 0.0, None),
    ] {
        let curve = surface
            .curve_query(&source, &[false, true], &[10.0], mode)
            .expect("один и тот же исходно допустимый конец истории");
        assert_eq!(curve.rows, 1);
        let point = &curve.points[0];
        close(point.own_heat, heat);
        close(point.performance, performance);
        assert_eq!(point.efficiency.is_none(), efficiency.is_none());
        if let (Some(actual), Some(expected)) = (point.efficiency, efficiency) {
            close(actual, expected);
        }
    }
}

#[test]
fn fixed_load_keeps_context_history_and_its_nonlinear_transformation() {
    let mut value = current_json();
    value["metadata"]["own_bands"] = json!([[0, 2]]);
    value["weights"] = json!([1.0, 0.0]);
    value["bands"] = json!([[0, 2]]);
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 2]}]);
    value["context_coefficients"] = json!([[8.0]]);
    let surface = decode(value);
    let changed_fan = data(&[5.0; 3], &[0.4; 3], &[-10.0, 10.0, 10.0]);
    let sustained_fan = data(&[5.0; 3], &[0.4; 3], &[10.0; 3]);
    let changed = surface
        .curve_query(&changed_fan, &[false, false, true], &[10.0], CurveLoad::Sustained(0.25))
        .expect("история вентилятора сохранена");
    let sustained = surface
        .curve_query(
            &sustained_fan,
            &[false, false, true],
            &[10.0],
            CurveLoad::Sustained(0.25),
        )
        .expect("при том же текущем вентиляторе другая история");
    assert_eq!(changed.rows, 1);
    assert_eq!(sustained.rows, 1);
    close(changed.points[0].own_heat, 1.0);
    close(changed.points[0].performance, 2.5);
    close(changed.points[0].efficiency.expect("2.5/1 определено"), 2.5);
    close(sustained.points[0].own_heat, 0.0);
    assert!(sustained.points[0].efficiency.is_none());
}

#[test]
fn fixed_load_query_ignores_future_changes_and_does_not_mutate_model_or_input() {
    let mut value = own_lag_json();
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 1]}]);
    let surface = decode(value);
    let source = data(&[1.0, 2.0, 3.0, 4.0], &[1.0, 0.0, 0.5, 1.0], &[0.0; 4]);
    let mut future = data(&[1.0, 2.0, 90.0, 90.0], &[1.0, 0.0, 0.0, 0.0], &[0.0, 0.0, 10.0, 10.0]);
    future.target.fill(999.0);
    let before_model = serde_json::to_value(&surface).expect("модель до запроса");
    let before_source = serde_json::to_value(&source).expect("данные до запроса");
    let before_predictions =
        serde_json::to_value(surface.predict(&source).expect("исходный прогноз")).expect("исходные числа");
    for busy in [0.0, 0.25, 1.0] {
        let original = surface
            .curve_query(
                &source,
                &[false, true, false, false],
                &[10.0],
                CurveLoad::Sustained(busy),
            )
            .expect("поддержанный прошлый момент");
        let changed = surface
            .curve_query(
                &future,
                &[false, true, false, false],
                &[10.0],
                CurveLoad::Sustained(busy),
            )
            .expect("будущее не влияет на прошлое");
        assert_eq!(original.rows, 1);
        assert_eq!(original.rows, changed.rows);
        close(original.points[0].own_heat, changed.points[0].own_heat);
        close(original.points[0].performance, changed.points[0].performance);
        close(original.mean_busy, changed.mean_busy);
    }
    assert_eq!(serde_json::to_value(&surface).expect("модель после"), before_model);
    assert_eq!(serde_json::to_value(&source).expect("данные после"), before_source);
    assert_eq!(
        serde_json::to_value(surface.predict(&source).expect("прогноз после")).expect("повторные числа"),
        before_predictions
    );
}

#[test]
fn sustained_load_rejects_nonfinite_and_out_of_range_values() {
    let surface = decode(current_json());
    let source = data(&[5.0], &[1.0], &[0.0]);
    for busy in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -f64::EPSILON,
        1.0 + f64::EPSILON,
    ] {
        assert!(
            surface
                .curve_query(&source, &[true], &[10.0], CurveLoad::Sustained(busy))
                .is_err()
        );
    }
}

#[test]
fn queried_ratios_report_knot_domain_without_claiming_joint_support() {
    let surface = decode(current_json());
    let source = data(&[5.0], &[1.0], &[0.0]);
    for mode in [
        CurveLoad::ObservedHistory,
        CurveLoad::SustainedFull,
        CurveLoad::Sustained(0.25),
        CurveLoad::Sustained(0.0),
    ] {
        let curve = surface
            .curve_query(&source, &[true], &[0.0, 5.0, 10.0, 20.0], mode)
            .expect("внешний множитель разрешён как диагностический запрос");
        assert_eq!(curve.rows, 1);
        assert_eq!(curve.points.len(), 4);
        for (point, (ratio, within)) in curve
            .points
            .iter()
            .zip([(0.0, true), (5.0, true), (10.0, true), (20.0, false)])
        {
            close(point.ratio, ratio);
            assert_eq!(point.within_knots, within);
        }
    }
}
