// Независимые проверки непрерывности фильтра: часы задаются явно, ожиданий нет.
use super::FilterTrace;
use std::time::Duration;

const GAP: Duration = Duration::from_secs(4);
const A: &str = "23:59:59.750";
const B: &str = "00:00:00.250";

fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

#[test]
fn startup_and_restart_cannot_claim_filter_without_successful_anchor() {
    let mut trace = FilterTrace::default();
    trace.filtered(at(1000), GAP);
    trace.filtered(at(2000), GAP);
    assert_eq!(trace.token(at(2000), GAP), None);
}

#[test]
fn successful_write_alone_is_not_filter_proof() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    assert_eq!(trace.token(at(0), GAP), None);
    assert_eq!(trace.token(at(250), GAP), None);
}

#[test]
fn continuous_filtering_preserves_exact_original_timestamp() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    for tick in [2000, 4000, 6000, 8000] {
        trace.filtered(at(tick), GAP);
    }
    assert_eq!(trace.token(at(8250), GAP), Some(A));
}

#[test]
fn successful_resume_consumes_proof_and_next_filter_anchors_to_new_row() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    trace.filtered(at(250), GAP);
    assert_eq!(trace.token(at(500), GAP), Some(A));
    trace.written(at(500), B);
    assert_eq!(trace.token(at(500), GAP), None);
    trace.filtered(at(750), GAP);
    assert_eq!(trace.token(at(1000), GAP), Some(B));
}

#[test]
fn explicit_disruption_requires_new_successfully_written_anchor() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    trace.filtered(at(250), GAP);
    trace.disrupt();
    assert_eq!(trace.token(at(500), GAP), None);
    trace.filtered(at(750), GAP);
    trace.filtered(at(1000), GAP);
    assert_eq!(trace.token(at(1250), GAP), None);
    trace.written(at(1500), B);
    trace.filtered(at(1750), GAP);
    assert_eq!(trace.token(at(2000), GAP), Some(B));
}

#[test]
fn late_first_filter_cannot_hide_preceding_active_sampling_stall() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    trace.filtered(at(4001), GAP);
    assert_eq!(trace.token(at(4001), GAP), None);
    trace.filtered(at(4250), GAP);
    assert_eq!(trace.token(at(4500), GAP), None);
}

#[test]
fn late_suppressed_heartbeat_invalidates_proof_until_new_written_row() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    trace.filtered(at(250), GAP);
    trace.filtered(at(4251), GAP);
    trace.filtered(at(4500), GAP);
    assert_eq!(trace.token(at(4750), GAP), None);
    trace.written(at(5000), B);
    trace.filtered(at(5250), GAP);
    assert_eq!(trace.token(at(5500), GAP), Some(B));
}

#[test]
fn exact_heartbeat_budget_remains_eligible() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    trace.filtered(at(4000), GAP);
    trace.filtered(at(8000), GAP);
    assert_eq!(trace.token(at(12_000), GAP), Some(A));
}

#[test]
fn resumed_row_after_stale_filter_heartbeat_has_no_proof() {
    let mut trace = FilterTrace::default();
    trace.written(at(0), A);
    trace.filtered(at(250), GAP);
    assert_eq!(trace.token(at(4251), GAP), None);
}

#[test]
fn backward_suppressed_heartbeat_disqualifies_until_new_anchor() {
    let mut trace = FilterTrace::default();
    trace.written(at(1000), A);
    trace.filtered(at(2000), GAP);
    trace.filtered(at(1999), GAP);
    trace.filtered(at(2250), GAP);
    assert_eq!(trace.token(at(2500), GAP), None);
}

#[test]
fn backward_first_filter_and_backward_resume_are_not_continuity() {
    let mut trace = FilterTrace::default();
    trace.written(at(1000), A);
    trace.filtered(at(999), GAP);
    assert_eq!(trace.token(at(1100), GAP), None);
    trace.written(at(2000), B);
    trace.filtered(at(2250), GAP);
    assert_eq!(trace.token(at(2249), GAP), None);
}

#[test]
fn per_core_selection_keeps_work_heat_unknowns_and_window_corrections() {
    use machine::thermal::Reading;
    let cool = Reading {
        temp: Some(50.0),
        busy: Some(0.1),
        ratio: Some(45.0),
    };
    let mut cores = [cool; 24];
    assert!(!super::relevant(cores.into_iter(), 85.0, false));
    cores[23].busy = Some(0.4);
    assert!(super::relevant(cores.into_iter(), 85.0, false));
    cores[23].busy = Some(0.399);
    assert!(!super::relevant(cores.into_iter(), 85.0, false));
    cores[23].temp = Some(85.0);
    assert!(super::relevant(cores.into_iter(), 85.0, false));
    cores[23] = cool;
    assert!(super::relevant(cores.into_iter(), 85.0, true));
    cores[5].busy = None;
    assert!(super::relevant(cores.into_iter(), 85.0, false));
    cores[5].busy = Some(f64::NAN);
    assert!(super::relevant(cores.into_iter(), 85.0, false));
    cores[5] = cool;
    cores[9].temp = None;
    assert!(super::relevant(cores.into_iter(), 85.0, false));
    assert!(super::relevant(std::iter::empty(), 85.0, false));
}
