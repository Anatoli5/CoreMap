use crate::history::{HistoryError, Signal};

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1.0e-9, "expected {expected}, got {actual}");
}

#[test]
fn jittered_samples_are_weighted_by_elapsed_time() {
    let mut signal = Signal::new(1_000).expect("positive hold");
    signal.push(0, Some(10.0)).expect("first reading");
    signal.push(300, Some(20.0)).expect("irregular reading");
    signal.push(1_100, Some(30.0)).expect("irregular reading");

    let window = signal.window(1_500, 0, 1_500).expect("valid window");
    assert_eq!(window.span_ms, 1_500);
    assert_eq!(window.covered_ms, 1_500);
    close(window.coverage(), 1.0);
    // 300 мс по 10, 800 мс по 20, 400 мс по 30.
    close(window.mean.expect("fully observed"), 31_000.0 / 1_500.0);
}

#[test]
fn exact_hold_endpoint_is_unknown_and_does_not_fill_the_rest_of_a_window() {
    let mut signal = Signal::new(1_000).expect("positive hold");
    signal.push(0, Some(10.0)).expect("first reading");

    assert!(signal.at(-1).is_none());
    let before_expiry = signal.at(999).expect("held before endpoint");
    assert_eq!(before_expiry.observed_at, 0);
    close(before_expiry.value, 10.0);
    assert!(signal.at(1_000).is_none());
    assert!(signal.at(2_000).is_none());

    let window = signal.window(2_000, 0, 2_000).expect("valid window");
    assert_eq!(window.covered_ms, 1_000);
    assert_eq!(window.span_ms, 2_000);
    close(window.mean.expect("partially observed"), 10.0);
    close(window.coverage(), 0.5);
}

#[test]
fn explicit_unknown_breaks_a_hold_without_becoming_zero() {
    let mut signal = Signal::new(1_000).expect("positive hold");
    signal.push(0, Some(10.0)).expect("first reading");
    signal.push(250, Some(20.0)).expect("second reading");
    signal.push(700, None).expect("recording gap");
    signal.push(1_000, Some(30.0)).expect("resumed recording");

    assert!(signal.at(700).is_none());
    assert!(signal.at(999).is_none());
    close(signal.at(1_000).expect("new reading").value, 30.0);
    let window = signal.window(1_500, 0, 1_500).expect("valid window");
    assert_eq!(window.covered_ms, 1_200);
    close(window.coverage(), 0.8);
    close(window.mean.expect("known intervals only"), 26_500.0 / 1_200.0);
}

#[test]
fn unknown_full_partial_and_measured_zero_are_distinct() {
    let mut signal = Signal::new(500).expect("positive hold");
    let empty = signal.window(1_000, 0, 1_000).expect("valid window");
    assert!(empty.mean.is_none());
    assert_eq!(empty.covered_ms, 0);
    close(empty.coverage(), 0.0);

    signal.push(500, Some(0.0)).expect("measured zero");
    let partial = signal.window(1_000, 0, 1_000).expect("valid window");
    assert_eq!(partial.mean, Some(0.0));
    assert_eq!(partial.covered_ms, 500);
    close(partial.coverage(), 0.5);
    let full = signal.window(999, 0, 499).expect("fully covered window");
    assert_eq!(full.mean, Some(0.0));
    assert_eq!(full.covered_ms, 499);
    close(full.coverage(), 1.0);
}

#[test]
fn an_older_age_band_does_not_include_the_current_endpoint() {
    let mut signal = Signal::new(20_000).expect("positive hold");
    signal.push(0, Some(10.0)).expect("first reading");
    signal.push(4_000, Some(20.0)).expect("second reading");
    signal.push(8_000, Some(100.0)).expect("current reading");

    let old = signal.window(10_000, 2_000, 6_000).expect("age band");
    assert_eq!(old.covered_ms, 4_000);
    close(old.mean.expect("known band"), 20.0);
    let recent = signal.window(10_000, 0, 2_000).expect("recent band");
    close(recent.mean.expect("known recent band"), 100.0);
}

#[test]
fn equal_current_readings_preserve_different_fan_histories() {
    let mut recent = Signal::new(20_000).expect("positive hold");
    let mut sustained = Signal::new(20_000).expect("positive hold");
    recent.push(0, Some(1_000.0)).expect("earlier fan");
    sustained.push(0, Some(5_000.0)).expect("earlier fan");
    recent.push(9_000, Some(5_000.0)).expect("fan just rose");
    sustained.push(9_000, Some(5_000.0)).expect("same fan");

    let recent_now = recent.at(10_000).expect("current fan");
    let sustained_now = sustained.at(10_000).expect("current fan");
    close(recent_now.value, sustained_now.value);
    assert_eq!(recent_now.observed_at, sustained_now.observed_at);
    let recent_history = recent.window(10_000, 0, 10_000).expect("fan history");
    let sustained_history = sustained.window(10_000, 0, 10_000).expect("fan history");
    close(recent_history.mean.expect("known history"), 1_400.0);
    close(sustained_history.mean.expect("known history"), 5_000.0);
}

#[test]
fn future_points_never_change_an_earlier_reading_or_window() {
    let mut signal = Signal::new(1_000).expect("positive hold");
    signal.push(0, Some(10.0)).expect("first reading");
    signal.push(300, Some(20.0)).expect("second reading");
    let before = signal.window(700, 0, 700).expect("past window");
    signal.push(800, Some(900.0)).expect("future reading");
    signal.push(900, None).expect("future invalidation");
    let after = signal.window(700, 0, 700).expect("past window");

    assert_eq!(before.covered_ms, after.covered_ms);
    assert_eq!(before.mean, after.mean);
    let reading = signal.at(700).expect("past reading");
    assert_eq!(reading.observed_at, 300);
    close(reading.value, 20.0);
}

#[test]
fn invalid_configuration_order_values_and_windows_fail_explicitly() {
    assert!(matches!(Signal::new(0), Err(HistoryError::InvalidHold)));
    assert!(matches!(Signal::new(-1), Err(HistoryError::InvalidHold)));
    let mut signal = Signal::new(1_000).expect("positive hold");
    signal.push(0, Some(1.0)).expect("first reading");
    assert_eq!(signal.push(0, Some(2.0)), Err(HistoryError::NonIncreasingTime));
    assert_eq!(signal.push(-1, Some(2.0)), Err(HistoryError::NonIncreasingTime));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(signal.push(100, Some(value)), Err(HistoryError::NonFiniteValue));
    }
    signal.push(50, Some(2.0)).expect("failed push kept prior state");
    close(signal.at(75).expect("valid later reading").value, 2.0);
    for (recent, oldest) in [(-1, 10), (0, 0), (10, 10), (11, 10), (0, -1)] {
        assert_eq!(
            signal.window(100, recent, oldest).err(),
            Some(HistoryError::InvalidWindow)
        );
    }
    assert_eq!(signal.window(i64::MIN, 0, 1).err(), Some(HistoryError::TimeOverflow));
}

#[test]
fn timestamp_translation_preserves_physical_histories() {
    let offset = 1_000_000;
    let mut first = Signal::new(2_000).expect("positive hold");
    let mut shifted = Signal::new(2_000).expect("positive hold");
    for (at, value) in [(0, Some(10.0)), (230, Some(30.0)), (910, None), (1_300, Some(5.0))] {
        first.push(at, value).expect("ordered reading");
        shifted.push(at + offset, value).expect("translated reading");
    }
    let original = first.window(1_700, 100, 1_700).expect("valid window");
    let translated = shifted.window(1_700 + offset, 100, 1_700).expect("translated window");
    assert_eq!(original.mean, translated.mean);
    assert_eq!(original.covered_ms, translated.covered_ms);
    assert_eq!(original.span_ms, translated.span_ms);
}
