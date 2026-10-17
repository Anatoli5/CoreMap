// Независимые точные случаи для публичного формата модели; подгонка не вызывается.
use crate::conditional::Surface;
use crate::conditional_data::Prepared;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1.0e-8, "expected {expected}, got {actual}");
}

fn model_json() -> Value {
    json!({
        "metadata": {
            "knots": [0.0, 10.0, 20.0, 30.0],
            "contexts": {"fan_cpu": [0.0, 10.0]},
            "own_bands": [[0, 2]]
        },
        "bands": [[0, 2]],
        "weights": [0.25, 0.75],
        "context_basis": [{"constant": true}],
        "background_basis": [{"kind": "intercept", "name": "intercept", "group": "background", "penalty": 0.00001}],
        "context_coefficients": [[4.0], [2.0], [1.0]],
        "background_coefficients": [30.0]
    })
}

fn decode(value: Value) -> Surface {
    serde_json::from_value(value).expect("valid documented surface JSON")
}

fn prepared(ratio: Vec<f64>, fan: Vec<f64>) -> Prepared {
    assert_eq!(ratio.len(), fan.len());
    let count = ratio.len();
    let times: Vec<i64> = (0..count)
        .map(|index| i64::try_from(index).expect("small fixture") * 1_000)
        .collect();
    let mut contexts = BTreeMap::new();
    contexts.insert("fan_cpu".into(), fan);
    Prepared {
        times,
        segments: vec![0; count],
        ratio,
        busy: vec![1.0; count],
        target: vec![0.0; count],
        contexts,
    }
}

#[test]
fn conditional_heat_is_product_of_retained_own_drive_and_past_context() {
    let mut value = model_json();
    value["metadata"]["knots"] = json!([0.0, 10.0]);
    value["weights"] = json!([0.0, 1.0]);
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 2]}]);
    value["context_coefficients"] = json!([[4.0]]);
    let surface = decode(value);
    let data = prepared(vec![0.0, 10.0, 10.0], vec![0.0, 10.0, 10.0]);
    let predictions = surface.predict(&data).expect("valid histories");
    let last = predictions[2].as_ref().expect("two seconds of history");
    // Средняя работа 0.5 × средний холодный режим 0.5 × коэффициент 4.
    // Усреднение их произведения или только текущий вентилятор дали бы ноль.
    close(last.own_heat, 1.0);
    close(last.temperature, 31.0);
}

#[test]
fn joint_lag_basis_multiplies_separately_averaged_hats() {
    let mut value = model_json();
    value["metadata"]["knots"] = json!([0.0, 10.0]);
    value["metadata"]["contexts"]["temp_gpu"] = json!([0.0, 10.0]);
    value["weights"] = json!([0.0, 1.0]);
    value["context_basis"] = json!([{"joint": [["fan_cpu", 0], ["temp_gpu", 0]], "lag": [0, 2]}]);
    value["context_coefficients"] = json!([[4.0]]);
    let surface = decode(value);
    let mut data = prepared(vec![10.0; 3], vec![0.0, 10.0, 10.0]);
    data.contexts.insert("temp_gpu".into(), vec![10.0, 0.0, 0.0]);
    let predictions = surface.predict(&data).expect("valid joint history");
    let last = predictions[2].as_ref().expect("full history");
    close(last.own_heat, 1.0);
    close(last.temperature, 31.0);
}

#[test]
fn sustained_curve_is_nonnegative_nondecreasing_and_can_be_nonconvex() {
    let surface = decode(model_json());
    let mut heat = Vec::new();
    for ratio in [0.0, 5.0, 10.0, 15.0, 20.0, 25.0, 30.0] {
        let data = prepared(vec![ratio; 3], vec![5.0; 3]);
        let predictions = surface.predict(&data).expect("sustained inputs");
        heat.push(predictions[2].as_ref().expect("full history").own_heat);
    }
    for (actual, expected) in heat.iter().zip([0.0, 2.0, 4.0, 5.0, 6.0, 6.5, 7.0]) {
        close(*actual, expected);
    }
    assert!(heat.iter().all(|value| *value >= 0.0));
    assert!(heat.windows(2).all(|pair| pair[1] >= pair[0]));
    assert!(heat[4] > f64::midpoint(heat[2], heat[6]));
}

#[test]
fn changing_only_current_ratio_differs_from_sustained_ratio_history() {
    let surface = decode(model_json());
    let recent = prepared(vec![0.0, 0.0, 10.0], vec![5.0; 3]);
    let sustained = prepared(vec![10.0; 3], vec![5.0; 3]);
    let recent_prediction = surface.predict(&recent).expect("recent change");
    let sustained_prediction = surface.predict(&sustained).expect("sustained activity");
    close(recent_prediction[2].as_ref().expect("full history").own_heat, 1.0);
    close(sustained_prediction[2].as_ref().expect("full history").own_heat, 4.0);
}

#[test]
fn same_current_fan_with_different_histories_changes_conditional_heat() {
    let mut value = model_json();
    value["metadata"]["knots"] = json!([0.0, 10.0]);
    value["weights"] = json!([0.0, 1.0]);
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 2]}]);
    value["context_coefficients"] = json!([[4.0]]);
    let surface = decode(value);
    let recent = prepared(vec![10.0; 3], vec![0.0, 0.0, 10.0]);
    let sustained = prepared(vec![10.0; 3], vec![10.0; 3]);
    let first = surface.predict(&recent).expect("recent fan change");
    let second = surface.predict(&sustained).expect("sustained fan");
    close(first[2].as_ref().expect("full history").own_heat, 4.0);
    close(second[2].as_ref().expect("full history").own_heat, 0.0);
}

#[test]
fn new_segments_and_missing_history_return_unknown_instead_of_old_history() {
    let surface = decode(model_json());
    let mut data = prepared(vec![10.0; 5], vec![5.0; 5]);
    data.segments = vec![0, 0, 0, 1, 1];
    let predictions = surface.predict(&data).expect("separate segments");
    assert!(predictions[0].is_none());
    assert!(predictions[1].is_none());
    assert!(predictions[2].is_some());
    assert!(predictions[3].is_none());
    assert!(predictions[4].is_none());

    let mut missing = prepared(vec![10.0; 3], vec![5.0; 3]);
    missing.ratio[1] = f64::NAN;
    let predictions = surface
        .predict(&missing)
        .expect("unknown drive is data, not fatal error");
    assert!(predictions[2].is_none());
}

#[test]
fn incomplete_context_coverage_is_unknown_not_zero() {
    let mut value = model_json();
    value["context_basis"] = json!([{"context": "fan_cpu", "side": 0, "lag": [0, 2]}]);
    let surface = decode(value);
    let data = prepared(vec![10.0; 3], vec![0.0, f64::NAN, 0.0]);
    let predictions = surface.predict(&data).expect("missing context is allowed");
    assert!(predictions[2].is_none());
}

#[test]
fn future_rows_and_response_values_do_not_change_past_predictions() {
    let surface = decode(model_json());
    let mut first = prepared(vec![10.0; 4], vec![5.0; 4]);
    first.contexts.insert("package_lag_only".into(), vec![60.0; 4]);
    let mut second = prepared(vec![10.0, 10.0, 10.0, 30.0], vec![5.0, 5.0, 5.0, 10.0]);
    second.target.fill(f64::NAN);
    second
        .contexts
        .insert("package_lag_only".into(), vec![100.0, 50.0, 105.0, 80.0]);
    let before = surface.predict(&first).expect("original data");
    let after = surface.predict(&second).expect("future and response poison");
    let original = before[2].as_ref().expect("supported past");
    let changed = after[2].as_ref().expect("supported past");
    close(original.temperature, changed.temperature);
    close(original.own_heat, changed.own_heat);
}

#[test]
fn current_package_context_is_rejected_instead_of_copying_the_response() {
    let mut value = model_json();
    value["metadata"]["contexts"]["package_lag_only"] = json!([50.0, 70.0]);
    value["context_basis"] = json!([{"context": "package_lag_only", "side": 1, "lag": null}]);
    let mut data = prepared(vec![10.0; 3], vec![5.0; 3]);
    data.contexts.insert("package_lag_only".into(), vec![60.0; 3]);
    let decoded: Result<Surface, _> = serde_json::from_value(value);
    if let Ok(surface) = decoded {
        assert!(surface.predict(&data).is_err());
    }
}

#[test]
fn lagged_package_uses_fixed_scaling_once_and_same_row_affects_only_future() {
    let mut value = model_json();
    value["background_basis"] = json!([
        {"kind": "intercept", "name": "intercept", "group": "background", "penalty": 0.00001},
        {"kind": "background", "name": "package", "group": "package", "penalty": 2,
         "basis": {"context": "package_lag_only", "transform": "package_linear", "lag": [0, 2]}}
    ]);
    value["background_coefficients"] = json!([30.0, 2.0]);
    let surface = decode(value);
    let mut first = prepared(vec![0.0; 4], vec![5.0; 4]);
    first
        .contexts
        .insert("package_lag_only".into(), vec![50.0, 70.0, 90.0, 50.0]);
    let mut second = prepared(vec![0.0; 4], vec![5.0; 4]);
    second
        .contexts
        .insert("package_lag_only".into(), vec![50.0, 70.0, 110.0, 50.0]);
    let first = surface.predict(&first).expect("lagged package allowed");
    let second = surface.predict(&second).expect("same-row package changed");
    close(first[2].as_ref().expect("complete history").temperature, 31.0);
    close(second[2].as_ref().expect("complete history").temperature, 31.0);
    close(first[3].as_ref().expect("later history").temperature, 33.0);
    close(second[3].as_ref().expect("later history").temperature, 34.0);
}

#[test]
fn inconsistent_vector_lengths_fail_without_indexing_panics() {
    let surface = decode(model_json());
    let mut ratio = prepared(vec![10.0; 3], vec![5.0; 3]);
    assert!(ratio.ratio.pop().is_some());
    assert!(surface.predict(&ratio).is_err());
    let mut busy = prepared(vec![10.0; 3], vec![5.0; 3]);
    assert!(busy.busy.pop().is_some());
    assert!(surface.predict(&busy).is_err());
    let mut context = prepared(vec![10.0; 3], vec![5.0; 3]);
    assert!(
        context
            .contexts
            .get_mut("fan_cpu")
            .expect("fixture context")
            .pop()
            .is_some()
    );
    assert!(surface.predict(&context).is_err());
    let mut segments = prepared(vec![10.0; 3], vec![5.0; 3]);
    assert!(segments.segments.pop().is_some());
    assert!(surface.predict(&segments).is_err());
}

#[test]
fn native_evaluation_matches_independent_python_export_oracle() {
    let mut fixture: Value =
        serde_json::from_str(include_str!("factorized-native-oracle.json")).expect("independent JSON oracle");
    let surface: Surface = serde_json::from_value(fixture["model"].take()).expect("exported model");
    let mut input = fixture["data"].take();
    if let Some(package) = input.as_object_mut().expect("oracle input").remove("package_lag_only") {
        input["contexts"]["package_lag_only"] = package;
    }
    let data: Prepared = serde_json::from_value(input).expect("documented prepared fields");
    let actual = surface.predict(&data).expect("evaluate exported model");
    let expected = fixture["expected"].as_array().expect("oracle predictions");
    assert_eq!(actual.len(), expected.len());
    let mut known = 0;
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        if expected.is_null() {
            assert!(actual.is_none(), "row {index}: unavailable reference history");
        } else {
            let actual = actual.as_ref().expect("reference supports this history");
            close(
                actual.temperature,
                expected["temperature"].as_f64().expect("reference temperature"),
            );
            close(
                actual.own_heat,
                expected["own_heat"].as_f64().expect("reference own component"),
            );
            known += 1;
        }
    }
    assert!(known > 100, "parity must cover a useful set of supported rows");
}
