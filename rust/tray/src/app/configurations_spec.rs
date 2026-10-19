// Независимые проверки физической памяти: подключить как тестовый модуль configurations.
// Сценарии записаны по согласованной спецификации до чтения реализации.
use super::{Kind, Ledger, Rule, Trigger};
use machine::auto::Cause;
use machine::thermal::Reading;
use serde_json::{Value, json};
use std::rc::Rc;

const RULE: Rule = Rule {
    limit: 85,
    emergency: 95,
    window_secs: 2,
};

fn ledger() -> Ledger {
    let mut memory = Ledger::default();
    memory.initialize("independent-physical-topology".into());
    memory
}

fn active(ratio: f64) -> Reading {
    Reading {
        temp: Some(70.0),
        ratio: Some(ratio),
        busy: Some(0.8),
    }
}

fn fail(memory: &mut Ledger, own: Option<f64>, peers: &[Option<Reading>], kind: Kind) {
    let cause = match kind {
        Kind::Instant => Cause::Emergency,
        Kind::Window => Cause::Target,
    };
    assert!(memory.failure(
        Trigger {
            block: 0,
            cause,
            kind,
            ratio: own
        },
        peers,
        RULE,
        Rc::new(json!({"original": "failure"}))
    ));
}

fn state(memory: &Ledger) -> Value {
    memory.value().expect("сохранение независимого сценария")
}

#[test]
fn candidate_is_an_upper_bound_not_a_prediction_of_its_achieved_ratio() {
    let mut memory = ledger();
    let failed = [Some(active(50.5)), Some(active(40.25)), Some(active(38.75))];
    fail(&mut memory, Some(50.5), &failed, Kind::Instant);
    let current = [Some(active(20.0)), Some(active(40.4)), Some(active(38.6))];
    assert!(!memory.allows(0, 51, &current, None, RULE));
    assert!(!memory.allows(0, 54, &current, None, RULE));
    assert!(memory.allows(0, 50, &current, None, RULE));
}

#[test]
fn a_fractional_failed_ratio_is_not_replaced_by_its_rounded_index() {
    let mut memory = ledger();
    let row = [Some(active(49.25)), Some(active(40.0))];
    fail(&mut memory, Some(49.25), &row, Kind::Instant);
    assert!(!memory.allows(0, 50, &row, None, RULE));
    assert!(memory.allows(0, 49, &row, None, RULE));
    assert_eq!(state(&memory)["failures"][0]["ratio"], json!(49.25));
}

#[test]
fn only_known_peer_step_changes_unlock_the_physical_failure() {
    let mut memory = ledger();
    let original = [Some(active(50.5)), Some(active(40.1)), Some(active(38.9))];
    fail(&mut memory, Some(50.5), &original, Kind::Instant);
    assert!(!memory.allows(
        0,
        51,
        &[Some(active(49.0)), Some(active(40.49)), Some(active(38.6))],
        None,
        RULE
    ));
    assert!(memory.allows(
        0,
        51,
        &[Some(active(49.0)), Some(active(40.5)), Some(active(38.6))],
        None,
        RULE
    ));
    assert!(memory.allows(
        0,
        51,
        &[Some(active(49.0)), Some(active(40.49)), Some(active(38.4))],
        None,
        RULE
    ));
}

#[test]
fn peer_positions_are_ordered_and_own_cooling_is_not_peer_redistribution() {
    let mut memory = ledger();
    let original = [Some(active(50.5)), Some(active(40.0)), Some(active(38.0))];
    fail(&mut memory, Some(50.5), &original, Kind::Instant);
    assert!(!memory.allows(
        0,
        51,
        &[Some(active(10.0)), Some(active(40.0)), Some(active(38.0))],
        None,
        RULE
    ));
    assert!(memory.allows(
        0,
        51,
        &[Some(active(10.0)), Some(active(38.0)), Some(active(40.0))],
        None,
        RULE
    ));
}

#[test]
fn an_unknown_peer_is_not_evidence_of_a_different_physical_context() {
    let mut memory = ledger();
    let original = [Some(active(50.5)), Some(active(40.0)), Some(active(38.0))];
    fail(&mut memory, Some(50.5), &original, Kind::Instant);
    let missing = [Some(active(49.0)), None, Some(active(38.0))];
    assert!(!memory.allows(0, 51, &missing, None, RULE));
    assert!(memory.allows(0, 50, &missing, None, RULE));
    assert!(memory.allows(0, 51, &[Some(active(49.0)), None, Some(active(39.0))], None, RULE));
}

#[test]
fn known_idle_and_unknown_remain_distinct() {
    let mut memory = ledger();
    let idle = Some(Reading {
        temp: Some(50.0),
        ratio: None,
        busy: Some(0.0),
    });
    fail(&mut memory, Some(50.5), &[Some(active(50.5)), idle], Kind::Instant);
    assert!(!memory.allows(0, 51, &[Some(active(49.0)), idle], None, RULE));
    assert!(!memory.allows(0, 51, &[Some(active(49.0)), None], None, RULE));
    assert!(memory.allows(0, 51, &[Some(active(49.0)), Some(active(35.0))], None, RULE));
    let mut unknown_failure = ledger();
    fail(
        &mut unknown_failure,
        Some(50.5),
        &[Some(active(50.5)), None],
        Kind::Instant,
    );
    assert!(!unknown_failure.allows(0, 51, &[Some(active(49.0)), idle], None, RULE));
}

#[test]
fn changed_busy_and_temperatures_do_not_replace_the_actual_peer_key() {
    let mut memory = ledger();
    fail(
        &mut memory,
        Some(50.5),
        &[Some(active(50.5)), Some(active(40.0))],
        Kind::Instant,
    );
    let peer = Some(Reading {
        temp: Some(35.0),
        ratio: Some(40.49),
        busy: Some(0.01),
    });
    assert!(!memory.allows(0, 51, &[Some(active(49.0)), peer], None, RULE));
}

#[test]
fn closed_window_failures_compare_window_evidence_and_missing_window_fails_closed() {
    let mut memory = ledger();
    fail(
        &mut memory,
        Some(50.5),
        &[Some(active(50.5)), Some(active(40.0))],
        Kind::Window,
    );
    let instant_changed = [Some(active(49.0)), Some(active(30.0))];
    let window_same = [Some(active(49.0)), Some(active(40.0))];
    assert!(!memory.allows(0, 51, &instant_changed, Some(&window_same), RULE));
    assert!(!memory.allows(0, 51, &instant_changed, None, RULE));
    assert!(memory.allows(0, 50, &instant_changed, None, RULE));
    assert!(memory.allows(0, 51, &window_same, Some(&instant_changed), RULE));
}

#[test]
fn instantaneous_failures_compare_instant_evidence_even_if_window_differs() {
    let mut memory = ledger();
    fail(
        &mut memory,
        Some(50.5),
        &[Some(active(50.5)), Some(active(40.0))],
        Kind::Instant,
    );
    let same = [Some(active(49.0)), Some(active(40.0))];
    let changed = [Some(active(49.0)), Some(active(30.0))];
    assert!(!memory.allows(0, 51, &same, Some(&changed), RULE));
    assert!(memory.allows(0, 51, &changed, Some(&same), RULE));
}

#[test]
fn high_maximum_and_nonthermal_revert_do_not_create_a_failure() {
    let mut memory = ledger();
    let row = [Some(active(50.5)), Some(active(40.0))];
    memory.observe(Kind::Instant, &row, || json!({"temp": [99.0, 70.0]}));
    for cause in [Cause::Return, Cause::Revert] {
        assert!(!memory.failure(
            Trigger {
                block: 0,
                cause,
                kind: Kind::Instant,
                ratio: Some(50.5)
            },
            &row,
            RULE,
            Rc::new(json!({}))
        ));
    }
    assert_eq!(
        state(&memory)["failures"].as_array().expect("массив неудач").as_slice(),
        &[] as &[Value]
    );
    assert!(memory.allows(0, 51, &row, None, RULE));
}

#[test]
fn maximum_retains_one_whole_winning_frame_and_fractional_ratios() {
    let mut memory = ledger();
    let first = [
        Some(Reading {
            temp: Some(80.0),
            ..active(43.125_678)
        }),
        Some(active(38.25)),
    ];
    let first_snapshot = json!({"tag":"first", "ratio":[43.125_678,38.25], "ceilings":[54,40], "busy":[0.8,0.8]});
    memory.observe(Kind::Instant, &first, || first_snapshot.clone());
    let lower = [
        Some(active(43.4)),
        Some(Reading {
            temp: Some(90.0),
            ..active(39.75)
        }),
    ];
    memory.observe(
        Kind::Instant,
        &lower,
        || json!({"tag":"second", "ratio":[43.4,39.75], "ceilings":[44,39]}),
    );
    let saved = state(&memory);
    let profile = saved["profiles"]
        .as_array()
        .expect("профили")
        .iter()
        .find(|p| p["block"] == 0 && p["ratioState"] == 43)
        .expect("максимум блока0");
    assert_eq!(profile["max"], json!(80.0));
    assert_eq!(profile["snapshot"], first_snapshot);
}

#[test]
fn statistics_reset_restart_and_cool_observations_do_not_erase_thermal_failure() {
    let mut memory = ledger();
    let row = [Some(active(50.5)), Some(active(40.0))];
    fail(&mut memory, Some(50.5), &row, Kind::Instant);
    memory.observe(Kind::Instant, &row, || json!({"temperature":70.0}));
    memory.reset_statistics();
    let saved = state(&memory);
    assert_eq!(
        saved["profiles"].as_array().expect("профили").as_slice(),
        &[] as &[Value]
    );
    let mut restored = ledger();
    restored.restore(&saved, 2).expect("восстановление");
    restored.observe(
        Kind::Instant,
        &[Some(active(49.0)), Some(active(40.0))],
        || json!({"cool":true}),
    );
    assert!(!restored.allows(0, 51, &row, None, RULE));
    assert_eq!(state(&restored)["failures"][0]["ratio"], json!(50.5));
}

#[test]
fn each_thermal_rule_axis_has_independent_failure_memory() {
    let mut memory = ledger();
    let row = [Some(active(50.5)), Some(active(40.0))];
    fail(&mut memory, Some(50.5), &row, Kind::Instant);
    for rule in [
        Rule { limit: 86, ..RULE },
        Rule { emergency: 96, ..RULE },
        Rule { window_secs: 3, ..RULE },
    ] {
        assert!(memory.allows(0, 51, &row, None, rule));
    }
    assert!(!memory.allows(0, 51, &row, None, RULE));
}

#[test]
fn failed_restore_is_transactional_and_topology_aware() {
    let mut memory = ledger();
    let row = [Some(active(50.5)), Some(active(40.0))];
    fail(&mut memory, Some(50.5), &row, Kind::Instant);
    let original = state(&memory);
    for bad in [json!({"version":"bogus"}), {
        let mut v = original.clone();
        v["layout"] = json!("foreign");
        v
    }] {
        assert!(memory.restore(&bad, 2).is_err());
        assert!(!memory.allows(0, 51, &row, None, RULE));
        assert_eq!(state(&memory)["failures"], original["failures"]);
    }
}

#[test]
fn unknown_failed_own_ratio_has_no_invented_safe_boundary() {
    let mut memory = ledger();
    let row = [None, Some(active(40.0))];
    fail(&mut memory, None, &row, Kind::Instant);
    for candidate in [28, 49, 54] {
        assert!(!memory.allows(0, candidate, &[Some(active(27.0)), Some(active(40.0))], None, RULE));
    }
    assert!(memory.allows(0, 49, &[Some(active(27.0)), Some(active(41.0))], None, RULE));
    assert!(state(&memory)["failures"][0]["ratio"].is_null());
}

#[test]
fn nonwinning_observation_does_not_build_an_expensive_snapshot() {
    let mut memory = ledger();
    let mut row = [Some(active(43.5))];
    memory.observe(Kind::Instant, &row, || json!({"winner":true}));
    row[0].as_mut().expect("показание").temp = Some(60.0);
    let built = std::cell::Cell::new(false);
    memory.observe(Kind::Instant, &row, || {
        built.set(true);
        json!({"winner":false})
    });
    assert!(!built.get(), "полный снимок нужен только новому максимуму");
}

#[test]
fn cap_command_changes_in_linked_snapshots_do_not_unlock_the_same_physical_state() {
    let mut memory = ledger();
    let row = [Some(active(50.5)), Some(active(40.0))];
    memory.observe(Kind::Instant, &row, || json!({"ceilings":[54,40],"ratio":[50.5,40.0]}));
    fail(&mut memory, Some(50.5), &row, Kind::Instant);
    memory.observe(Kind::Instant, &row, || json!({"ceilings":[51,39],"ratio":[50.5,40.0]}));
    assert!(!memory.allows(0, 51, &row, None, RULE));
}

#[test]
fn idle_unknown_and_window_profiles_keep_separate_original_contexts() {
    let mut memory = ledger();
    let unknown = [Some(Reading {
        temp: Some(70.0),
        ratio: None,
        busy: None,
    })];
    let idle = [Some(Reading {
        temp: Some(65.0),
        ratio: None,
        busy: Some(0.0),
    })];
    memory.observe(Kind::Instant, &unknown, || json!({"tag":"unknown-instant"}));
    memory.observe(Kind::Instant, &idle, || json!({"tag":"idle-instant"}));
    memory.observe(Kind::Window, &unknown, || json!({"tag":"unknown-window"}));
    let saved = state(&memory);
    let profiles = saved["profiles"].as_array().expect("профили");
    assert_eq!(profiles.len(), 3);
    let unknown_profile = profiles
        .iter()
        .find(|p| p["snapshot"]["tag"] == "unknown-instant")
        .expect("неизвестное показание");
    let idle_profile = profiles
        .iter()
        .find(|p| p["snapshot"]["tag"] == "idle-instant")
        .expect("известный простой");
    assert!(unknown_profile["ratioState"].is_null());
    assert_eq!(idle_profile["ratioState"], json!("idle"));
    let mut restored = ledger();
    restored.restore(&saved, 1).expect("восстановление разных видов");
    assert_eq!(state(&restored)["profiles"], saved["profiles"]);
}

#[test]
fn partial_window_without_a_temperature_mean_cannot_prove_peer_redistribution() {
    let mut memory = ledger();
    fail(
        &mut memory,
        Some(50.5),
        &[Some(active(50.5)), Some(active(40.0))],
        Kind::Window,
    );
    let instant = [Some(active(49.0)), Some(active(30.0))];
    let window = [
        active(49.0),
        Reading {
            temp: None,
            ratio: Some(30.0),
            busy: Some(0.8),
        },
    ];
    assert!(!memory.allows_closed(0, 51, &instant, &window, RULE));
    assert!(memory.allows_closed(0, 50, &instant, &window, RULE));
}

#[test]
fn positive_actual_keys_use_nearest_integer_with_exact_half_rounded_up() {
    let mut memory = ledger();
    for (ratio, marker) in [
        (49.4, "below"),
        (49.49, "justbelow"),
        (49.5, "half"),
        (50.5, "nexthalf"),
    ] {
        memory.observe(
            Kind::Instant,
            &[Some(active(ratio))],
            || json!({"tag":marker,"ratio":[ratio]}),
        );
    }
    let saved = state(&memory);
    let profiles = saved["profiles"].as_array().expect("профили округления");
    assert_eq!(profiles.len(), 3, "49.4 и49.49 — одна корзина49;49.5 —50;50.5 —51");
    for (key, tag) in [(49, "below"), (50, "half"), (51, "nexthalf")] {
        let p = profiles
            .iter()
            .find(|p| p["ratioState"] == key)
            .expect("заданная корзина");
        assert_eq!(p["snapshot"]["tag"], tag);
    }
}

#[test]
fn peer_half_step_changes_context_but_subhalf_changes_do_not() {
    let mut memory = ledger();
    fail(
        &mut memory,
        Some(50.5),
        &[Some(active(50.5)), Some(active(49.4))],
        Kind::Instant,
    );
    assert!(!memory.allows(0, 51, &[Some(active(49.0)), Some(active(49.49))], None, RULE));
    assert!(memory.allows(0, 51, &[Some(active(49.0)), Some(active(49.5))], None, RULE));
}

#[test]
fn raw_hot_boundary_is_not_lowered_to_the_nearest_profile_key() {
    let mut memory = ledger();
    let row = [Some(active(50.25)), Some(active(40.0))];
    fail(&mut memory, Some(50.25), &row, Kind::Instant);
    assert!(memory.allows(0, 50, &row, None, RULE));
    assert!(!memory.allows(0, 51, &row, None, RULE));
    assert_eq!(state(&memory)["failures"][0]["ratio"], json!(50.25));
}

#[test]
fn one_nearest_own_bin_keeps_the_lowest_raw_hot_boundary_and_its_own_snapshot() {
    let mut memory = ledger();
    let first = [Some(active(49.51)), Some(active(40.0))];
    assert!(memory.failure(
        Trigger {
            block: 0,
            cause: Cause::Emergency,
            kind: Kind::Instant,
            ratio: Some(49.51)
        },
        &first,
        RULE,
        Rc::new(json!({"event":"first","temp":[99.0,70.0],"ratio":[49.51,40.0]}))
    ));
    let higher = [Some(active(49.73)), Some(active(40.0))];
    assert!(!memory.failure(
        Trigger {
            block: 0,
            cause: Cause::Emergency,
            kind: Kind::Instant,
            ratio: Some(49.73)
        },
        &higher,
        RULE,
        Rc::new(json!({"event":"higher","temp":[100.0,70.0],"ratio":[49.73,40.0]}))
    ));
    let saved = state(&memory);
    assert_eq!(saved["failures"].as_array().expect("неудачи").len(), 1);
    assert_eq!(saved["failures"][0]["ratio"], json!(49.51));
    assert_eq!(saved["failures"][0]["snapshot"]["event"], "first");
    let lower = [Some(active(49.50)), Some(active(40.0))];
    let lower_snapshot = json!({"event":"lower","temp":[96.0,71.0],"ratio":[49.50,40.0]});
    assert!(memory.failure(
        Trigger {
            block: 0,
            cause: Cause::Emergency,
            kind: Kind::Instant,
            ratio: Some(49.50)
        },
        &lower,
        RULE,
        Rc::new(lower_snapshot.clone())
    ));
    let saved = state(&memory);
    assert_eq!(saved["failures"].as_array().expect("неудачи").len(), 1);
    assert_eq!(saved["failures"][0]["ratio"], json!(49.50));
    assert_eq!(saved["failures"][0]["snapshot"], lower_snapshot);
}

#[test]
fn unknown_own_failure_is_not_merged_with_a_known_own_nearest_bin() {
    let mut memory = ledger();
    fail(&mut memory, None, &[None, Some(active(40.0))], Kind::Instant);
    fail(
        &mut memory,
        Some(49.6),
        &[Some(active(49.6)), Some(active(40.0))],
        Kind::Instant,
    );
    let saved = state(&memory);
    let failures = saved["failures"].as_array().expect("неудачи");
    assert_eq!(failures.len(), 2);
    assert!(failures.iter().any(|f| f["ratio"].is_null()));
    assert!(failures.iter().any(|f| f["ratio"] == json!(49.6)));
}

#[test]
fn maxima_of_different_configured_windows_are_distinct_profiles_after_restart() {
    let mut memory = ledger();
    memory.set_window_secs(2);
    memory.observe(Kind::Window, &[Some(active(40.0))], || json!({"event":"two-seconds"}));
    memory.set_window_secs(60);
    memory.observe(Kind::Window, &[Some(active(40.0))], || json!({"event":"sixty-seconds"}));
    let saved = state(&memory);
    let profiles = saved["profiles"].as_array().expect("профили");
    assert_eq!(profiles.len(), 2);
    for (window, event) in [(2, "two-seconds"), (60, "sixty-seconds")] {
        let profile = profiles.iter().find(|p| p["windowSecs"] == window).expect("своё окно");
        assert_eq!(profile["snapshot"]["event"], event);
    }
    let mut restored = ledger();
    restored.restore(&saved, 1).expect("восстановление разных окон");
    assert_eq!(state(&restored)["profiles"], saved["profiles"]);
}
