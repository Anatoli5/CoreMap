// Независимые проверки подготовки данных по публичному контракту.
use crate::conditional_data::{Prepared, prepare};
use crate::model::{Die, Log};

fn die() -> Die {
    Die {
        names: vec!["A".into(), "B".into(), "C".into(), "E".into()],
        e: vec![false, false, false, true],
        neighbours: vec![vec![1, 2], vec![0], vec![0], vec![]],
    }
}

fn log() -> Log {
    // Порядок источника отличается от порядка карты кристалла.
    Log::new(vec!["E".into(), "C".into(), "A".into(), "B".into()])
}

fn push(
    log: &mut Log,
    at: i64,
    ratio: Option<f64>,
    busy: Option<f64>,
    temperature: Option<f64>,
    fan: Option<f64>,
    peer_ceiling: Option<f64>,
) {
    log.push_with_environment(
        at,
        [
            [Some(20.0), Some(0.9), Some(90.0), Some(60.0)],
            [Some(40.0), Some(0.5), Some(70.0), Some(30.0)],
            [ratio, busy, temperature, Some(40.0)],
            [Some(30.0), Some(0.3), Some(50.0), peer_ceiling],
        ]
        .into_iter(),
        [
            Some(20.0),
            Some(40.0),
            Some(15.0),
            temperature,
            fan,
            None,
            Some(45.0),
            Some(0.2),
        ],
    );
}

fn ordinary(log: &mut Log, at: i64, fan: Option<f64>) {
    push(log, at, Some(35.0), Some(0.5), Some(80.0), fan, Some(50.0));
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1.0e-5, "expected {expected}, got {actual}");
}

fn context<'a>(data: &'a Prepared, name: &str) -> &'a [f64] {
    data.contexts.get(name).expect("documented context").as_slice()
}

#[test]
fn past_second_bins_use_only_rows_inside_the_bin_and_actual_elapsed_time() {
    let mut source = log();
    for (at, ratio, busy, temperature) in [
        (0, 10.0, 0.1, 50.0),
        (250, 20.0, 0.2, 60.0),
        (750, 40.0, 0.6, 80.0),
        (1_000, 50.0, 0.7, 90.0),
        (1_250, 41.0, 0.9, 65.0),
        (1_750, 43.0, 1.0, 75.0),
        (2_000, 45.0, 1.0, 85.0),
    ] {
        push(
            &mut source,
            at,
            Some(ratio),
            Some(busy),
            Some(temperature),
            None,
            Some(50.0),
        );
    }
    let data = prepare(&[&source], &die(), 0);
    assert_eq!(data.times, [1_000, 2_000]);
    close(data.ratio[0], 110.0 / 3.0);
    close(data.busy[0], 0.5);
    close(data.target[0], 230.0 / 3.0);
    close(data.ratio[1], 43.0);
    close(data.busy[1], 2.9 / 3.0);
    close(data.target[1], 75.0);
}

#[test]
fn sparse_shared_sensors_are_causal_and_expire_after_three_seconds() {
    let mut source = log();
    ordinary(&mut source, 0, Some(2_000.0));
    ordinary(&mut source, 250, None);
    ordinary(&mut source, 750, None);
    ordinary(&mut source, 1_250, Some(8_000.0));
    ordinary(&mut source, 1_750, None);
    ordinary(&mut source, 2_250, None);
    ordinary(&mut source, 3_250, None);
    ordinary(&mut source, 4_250, None);
    ordinary(&mut source, 5_250, None);
    let data = prepare(&[&source], &die(), 0);
    assert_eq!(data.times, [1_000, 2_000, 3_000, 4_000, 5_000]);
    let fan = context(&data, "fan_cpu");
    // Чтение 1250 мс не может заполнить прошлое окно в 1000 мс.
    close(fan[0], 2_000.0);
    close(fan[1], 8_000.0);
    close(fan[2], 8_000.0);
    close(fan[3], 8_000.0);
    assert!(fan[4].is_nan());
    assert!(context(&data, "fan_gpu").iter().all(|value| value.is_nan()));
}

#[test]
fn exact_three_second_sensor_age_is_supported_but_four_is_unknown() {
    let mut source = log();
    ordinary(&mut source, 0, Some(2_000.0));
    for at in [1_000, 2_000, 3_000, 4_000] {
        ordinary(&mut source, at, None);
    }
    let data = prepare(&[&source], &die(), 0);
    let fan = context(&data, "fan_cpu");
    close(fan[2], 2_000.0);
    assert!(fan[3].is_nan());
}

#[test]
fn die_level_excludes_the_target_and_neighbours_follow_block_names() {
    let mut source = log();
    push(&mut source, 0, Some(35.0), Some(0.5), Some(100.0), None, Some(50.0));
    push(&mut source, 1_000, Some(35.0), Some(0.5), Some(100.0), None, Some(50.0));
    push(&mut source, 2_000, Some(35.0), Some(0.5), Some(10.0), None, Some(50.0));
    let data = prepare(&[&source], &die(), 0);
    close(data.target[0], 100.0);
    close(data.target[1], 10.0);
    for value in context(&data, "die_leave_target_out") {
        close(*value, 70.0);
    }
    for value in context(&data, "neighbour_B_temp") {
        close(*value, 50.0);
    }
    for value in context(&data, "neighbour_C_temp") {
        close(*value, 70.0);
    }
    close(context(&data, "neighbour_B_busy")[0], 0.3);
    close(context(&data, "neighbour_C_busy")[0], 0.5);
    close(context(&data, "neighbour_B_ratio")[0], 30.0);
    close(context(&data, "neighbour_C_ratio")[0], 40.0);
    close(context(&data, "other_busy")[0], 1.7 / 3.0);
    close(context(&data, "package_lag_only")[0], 100.0);
    close(context(&data, "package_lag_only")[1], 10.0);
    close(context(&data, "power_cpu")[0], 20.0);
    close(context(&data, "power_cores")[0], 15.0);
    close(context(&data, "power_gpu")[0], 40.0);
    close(context(&data, "temp_gpu")[0], 45.0);
    close(context(&data, "busy_gpu")[0], 0.2);
    assert!(!data.contexts.contains_key("neighbour_E_temp"));
}

#[test]
fn a_peer_absent_from_a_legacy_file_is_unknown_in_rank_and_neighbour_contexts() {
    let mut source = Log::new(vec!["A".into(), "C".into(), "E".into()]);
    for at in [0, 1_000] {
        source.push_with_environment(
            at,
            [
                [Some(35.0), Some(0.5), Some(80.0), Some(40.0)],
                [Some(40.0), Some(0.5), Some(70.0), Some(30.0)],
                [Some(20.0), Some(0.9), Some(90.0), Some(60.0)],
            ]
            .into_iter(),
            [None; 8],
        );
    }
    let data = prepare(&[&source], &die(), 0);
    assert_eq!(data.times, [1_000]);
    assert!(context(&data, "ceiling_rank")[0].is_nan());
    assert!(context(&data, "neighbour_B_temp")[0].is_nan());
    assert!(context(&data, "neighbour_B_busy")[0].is_nan());
    assert!(context(&data, "neighbour_B_ratio")[0].is_nan());
    close(context(&data, "neighbour_C_temp")[0], 70.0);
}

#[test]
fn ceiling_rank_uses_same_kind_peers_and_requires_known_peer_ceilings() {
    let mut source = log();
    ordinary(&mut source, 0, None);
    ordinary(&mut source, 1_000, None);
    push(&mut source, 2_000, Some(35.0), Some(0.5), Some(80.0), None, None);
    ordinary(&mut source, 3_000, None);
    let data = prepare(&[&source], &die(), 0);
    let rank = context(&data, "ceiling_rank");
    // Среди P-ядер A имеет одного более высокого и одного более низкого соседа.
    close(rank[0], 0.5);
    assert!(rank[1].is_nan());
    close(rank[2], 0.5);
    let e_data = prepare(&[&source], &die(), 3);
    for value in context(&e_data, "ceiling_rank") {
        close(*value, 0.0);
    }
}

#[test]
fn unknown_active_ratio_is_not_zero_but_observed_sleep_can_be_zero() {
    let mut source = log();
    ordinary(&mut source, 0, None);
    push(&mut source, 1_000, None, Some(0.0), Some(50.0), None, Some(50.0));
    push(&mut source, 2_000, None, Some(0.2), Some(50.0), None, Some(50.0));
    push(&mut source, 3_000, None, None, Some(50.0), None, Some(50.0));
    push(&mut source, 4_000, Some(35.0), Some(0.5), None, None, Some(50.0));
    let data = prepare(&[&source], &die(), 0);
    assert_eq!(data.times, [1_000, 2_000, 3_000, 4_000]);
    close(data.ratio[0], 0.0);
    close(data.busy[0], 0.0);
    assert!(data.ratio[1].is_nan());
    assert!(data.ratio[2].is_nan());
    assert!(data.busy[2].is_nan());
    assert!(data.target[3].is_nan());
}

#[test]
fn files_and_long_or_backward_time_gaps_start_independent_segments() {
    let mut first = log();
    for at in [0, 1_000, 6_001, 7_001] {
        ordinary(&mut first, at, Some(2_000.0));
    }
    let mut second = log();
    for at in [7_250, 8_250, 7_750, 8_750] {
        ordinary(&mut second, at, Some(3_000.0));
    }
    let data = prepare(&[&first, &second], &die(), 0);
    assert_eq!(data.times, [1_000, 7_001, 8_250, 8_750]);
    assert_eq!(data.segments.len(), 4);
    assert!(data.segments.windows(2).all(|pair| pair[0] != pair[1]));
}

#[test]
fn a_gap_of_exactly_five_seconds_does_not_split_or_invent_block_readings() {
    let mut source = log();
    for at in [0, 1_000, 6_000, 7_000] {
        ordinary(&mut source, at, None);
    }
    let data = prepare(&[&source], &die(), 0);
    assert_eq!(data.times, [1_000, 2_000, 3_000, 4_000, 5_000, 6_000, 7_000]);
    assert!(data.segments.iter().all(|segment| *segment == data.segments[0]));
    assert!(data.ratio[1..5].iter().all(|value| value.is_nan()));
    assert!(data.busy[1..5].iter().all(|value| value.is_nan()));
    assert!(data.target[1..5].iter().all(|value| value.is_nan()));
    close(data.target[5], 80.0);
}

#[test]
fn future_mutation_cannot_change_an_earlier_prepared_row() {
    let mut first = log();
    let mut second = log();
    for source in [&mut first, &mut second] {
        ordinary(source, 0, Some(2_000.0));
        ordinary(source, 500, None);
        ordinary(source, 1_000, None);
    }
    ordinary(&mut first, 1_500, Some(3_000.0));
    push(
        &mut second,
        1_500,
        Some(50.0),
        Some(1.0),
        Some(100.0),
        Some(9_000.0),
        Some(80.0),
    );
    ordinary(&mut first, 2_000, None);
    ordinary(&mut second, 2_000, None);
    let before = prepare(&[&first], &die(), 0);
    let after = prepare(&[&second], &die(), 0);
    assert_eq!(before.times[0], after.times[0]);
    assert_eq!(before.ratio[0], after.ratio[0]);
    assert_eq!(before.busy[0], after.busy[0]);
    assert_eq!(before.target[0], after.target[0]);
    for (name, values) in &before.contexts {
        let other = context(&after, name)[0];
        if values[0].is_nan() {
            assert!(other.is_nan(), "{name}");
        } else {
            assert_eq!(values[0], other, "{name}");
        }
    }
}

#[test]
fn preparation_does_not_mutate_original_raw_rows_or_sparse_unknowns() {
    let mut source = log();
    ordinary(&mut source, 0, Some(2_000.0));
    ordinary(&mut source, 1_000, None);
    ordinary(&mut source, 2_000, None);
    let times = source.times().to_vec();
    let rows = source.rows();
    let _prepared = prepare(&[&source], &die(), 0);
    assert_eq!(source.rows(), rows);
    assert_eq!(source.times(), times);
    assert_eq!(source.environment(1, 4), None);
    assert_eq!(source.environment(2, 4), None);
}

#[test]
fn json_round_trip_keeps_unknown_numeric_values_distinct_from_zero() {
    let mut source = log();
    ordinary(&mut source, 0, None);
    push(&mut source, 1_000, None, None, None, None, Some(50.0));
    ordinary(&mut source, 2_000, None);
    let prepared = prepare(&[&source], &die(), 0);
    let encoded = serde_json::to_value(&prepared).expect("serialize prepared data");
    assert!(encoded["ratio"][0].is_null());
    assert!(encoded["busy"][0].is_null());
    assert!(encoded["target"][0].is_null());
    assert!(encoded["contexts"]["fan_gpu"][0].is_null());
    let decoded: Prepared = serde_json::from_value(encoded).expect("restore unknowns");
    assert_eq!(decoded.times, prepared.times);
    assert_eq!(decoded.segments, prepared.segments);
    assert!(decoded.ratio[0].is_nan());
    assert!(decoded.busy[0].is_nan());
    assert!(decoded.target[0].is_nan());
    assert!(context(&decoded, "fan_gpu")[0].is_nan());
    close(decoded.ratio[1], 35.0);
    close(decoded.busy[1], 0.5);
    close(decoded.target[1], 80.0);
}

#[test]
fn incomplete_json_does_not_invent_required_time_or_value_columns() {
    let mut source = log();
    ordinary(&mut source, 0, None);
    ordinary(&mut source, 1_000, None);
    let prepared = prepare(&[&source], &die(), 0);
    for name in ["times", "segments", "ratio", "busy", "target", "contexts"] {
        let mut encoded = serde_json::to_value(&prepared).expect("serialize prepared data");
        encoded.as_object_mut().expect("prepared object").remove(name);
        assert!(serde_json::from_value::<Prepared>(encoded).is_err(), "missing {name}");
    }
}
