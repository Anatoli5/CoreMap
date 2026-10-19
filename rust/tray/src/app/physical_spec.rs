// Независимые проверки описания физических наблюдений: подключить в auto_state как тестовый модуль.
// Числа98/50,5/0,88/54 — форма ранее разобранного события; остальные значения — синтетические условия.
use super::physical_value;
use machine::thermal::Reading;
use serde_json::json;

fn reading(temp: Option<f64>, ratio: Option<f64>, busy: Option<f64>) -> Reading {
    Reading { temp, ratio, busy }
}

#[test]
fn emergency_observation_keeps_achieved_ratio_distinct_from_source_ceiling() {
    let context = physical_value(
        &[Some(reading(Some(98.0), Some(50.5), Some(0.88)))],
        &[Some(0.6)],
        Some(0.7),
    );
    let action = json!({"ceilingsBefore": [54], "physical": context});
    assert_eq!(action["physical"]["temp"], json!([98.0]));
    assert_eq!(action["physical"]["ratio"], json!([50.5]));
    assert_eq!(action["physical"]["busy"], json!([0.88]));
    assert_eq!(action["ceilingsBefore"], json!([54]));
    assert!(
        action["physical"].get("ceilings").is_none(),
        "потолки — отдельный контекст команды"
    );
}

#[test]
fn achieved_ratio_is_not_divided_by_a_requested_ceiling_or_rounded_to_its_step() {
    let context = physical_value(
        &[Some(reading(Some(70.0), Some(43.125_678), Some(0.73)))],
        &[Some(0.42)],
        Some(0.61),
    );
    assert_eq!(context["ratio"], json!([43.125_678]));
    assert_eq!(context["busy"], json!([0.73]));
    assert_eq!(context["relativeBusy"], json!([0.42]));
    let a = json!({"ceilingsBefore": [44], "physical": context});
    let b = json!({"ceilingsBefore": [54], "physical": a["physical"]});
    assert_eq!(
        a["physical"], b["physical"],
        "одинаковые наблюдения не заменяются разными командами"
    );
}

#[test]
fn unknown_values_keep_their_original_block_positions_as_null() {
    let context = physical_value(
        &[
            Some(reading(Some(74.5), None, Some(0.0))),
            None,
            Some(reading(None, Some(41.25), None)),
        ],
        &[None, None, Some(0.5)],
        None,
    );
    assert_eq!(context["temp"], json!([74.5, null, null]));
    assert_eq!(context["ratio"], json!([null, null, 41.25]));
    assert_eq!(context["busy"], json!([0.0, null, null]));
    assert_eq!(context["relativeBusy"], json!([null, null, 0.5]));
    assert!(context["globalBusy"].is_null());
}

#[test]
fn nonfinite_inputs_are_unknown_not_zero_or_nominal_temperature() {
    let context = physical_value(
        &[
            Some(reading(Some(f64::NAN), Some(f64::INFINITY), Some(f64::NEG_INFINITY))),
            Some(reading(Some(62.25), Some(38.75), Some(0.5))),
        ],
        &[Some(f64::NAN), Some(0.25)],
        Some(f64::INFINITY),
    );
    assert_eq!(context["temp"], json!([null, 62.25]));
    assert_eq!(context["ratio"], json!([null, 38.75]));
    assert_eq!(context["busy"], json!([null, 0.5]));
    assert_eq!(context["relativeBusy"], json!([null, 0.25]));
    assert!(context["globalBusy"].is_null());
}

#[test]
fn passed_p_and_e_representatives_remain_in_the_same_order_and_units() {
    let context = physical_value(
        &[
            Some(reading(Some(75.0), Some(50.5), Some(0.7))),
            Some(reading(Some(68.0), Some(47.25), Some(0.3))),
            Some(reading(Some(62.0), Some(39.5), Some(0.25))),
            Some(reading(Some(64.0), Some(38.75), Some(0.75))),
        ],
        &[Some(0.7), Some(0.3), Some(0.25), Some(0.75)],
        Some(0.55),
    );
    assert_eq!(context["temp"], json!([75.0, 68.0, 62.0, 64.0]));
    assert_eq!(context["ratio"], json!([50.5, 47.25, 39.5, 38.75]));
    assert_eq!(context["busy"], json!([0.7, 0.3, 0.25, 0.75]));
    assert_eq!(context["relativeBusy"], json!([0.7, 0.3, 0.25, 0.75]));
    assert_eq!(context["globalBusy"], json!(0.55));
}

#[test]
fn metadata_declares_a_sequential_interval_observation_without_atomic_claim() {
    let context = physical_value(
        &[Some(reading(Some(60.0), Some(40.5), Some(0.5)))],
        &[Some(1.0)],
        Some(0.5),
    );
    assert_eq!(context["basis"], "actual");
    assert_eq!(context["sampling"], "sequential-poll");
    assert_eq!(context["ratioBasis"], "awake-interval-average");
}
