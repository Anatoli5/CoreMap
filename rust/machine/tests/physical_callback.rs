// Независимые проверки физического callback возврата; как отдельный machine integration test.
use machine::auto::{Cause, Controller, Frame, LoadFrame, PhysicalPermit, Protection, Settings, Timing, Workload};
use machine::thermal::Reading;
use std::cell::Cell;
use std::time::Duration;

fn controller() -> Controller {
    let mut controller = Controller::new(
        Settings {
            limit: 85.0,
            emergency: 95.0,
            return_gap: 0.0,
        },
        Timing {
            window: Duration::from_secs(1),
            recheck: Duration::from_secs(1),
            settle: Duration::ZERO,
            pause: Duration::from_secs(2),
        },
        1,
    );
    controller.seed_protection(
        0,
        Protection {
            held: 49,
            reference: Some(Workload {
                busy: 0.8,
                ratio: 50.5,
                global_busy: 0.5,
                relative: 1.0,
            }),
            trial: None,
        },
    );
    controller
}

fn poll(controller: &mut Controller, tick: u64, allow: &PhysicalPermit<'_>) -> Vec<machine::auto::Decision> {
    let last_in_window = tick.is_multiple_of(4);
    let reading = Reading {
        temp: Some(if last_in_window { 64.0 } else { 60.0 }),
        ratio: Some(if last_in_window { 49.0 } else { 48.5 }),
        busy: Some(if last_in_window { 1.0 } else { 0.8 }),
    };
    let frame = Frame {
        readings: &[Some(reading)],
        ceilings: &[49],
        accepted: &[54],
        floor: &[27],
    };
    let mut out = Vec::new();
    controller.step_loaded_physical(
        Duration::from_millis(tick * 250),
        &frame,
        &LoadFrame {
            global_busy: Some(0.7),
            relative: &[Some(1.0)],
        },
        allow,
        &mut out,
    );
    out
}

#[test]
fn return_callback_receives_the_current_closed_window_not_the_latest_instant() {
    let mut controller = controller();
    let calls = Cell::new(0_u32);
    let allow = |block: usize, to: i32, _die: Option<f64>, window: &[Reading]| {
        calls.set(calls.get() + 1);
        assert_eq!((block, to), (0, 50));
        assert_eq!(window.len(), 1);
        let reading = window[0];
        assert!(reading.temp.is_some_and(|v| (v - 61.0).abs() < 1e-10));
        assert!(reading.busy.is_some_and(|v| (v - 0.85).abs() < 1e-10));
        assert!(
            reading
                .ratio
                .is_some_and(|v| (v - (49.0 + 48.5 * 2.4) / 3.4).abs() < 1e-10)
        );
        true
    };
    let mut returned = false;
    for tick in 0..=8 {
        returned |= poll(&mut controller, tick, &allow)
            .iter()
            .any(|d| d.cause == Cause::Return);
    }
    assert!(returned, "полная прохладная работа открывает единственную пробу");
    assert_eq!(calls.get(), 1);
}

#[test]
fn physical_callback_veto_prevents_the_single_upward_trial() {
    let mut controller = controller();
    let calls = Cell::new(0_u32);
    let veto = |_block: usize, _to: i32, _die: Option<f64>, window: &[Reading]| {
        calls.set(calls.get() + 1);
        assert_eq!(window.len(), 1);
        false
    };
    for tick in 0..=24 {
        let out = poll(&mut controller, tick, &veto);
        assert!(!out.iter().any(|d| d.cause == Cause::Return));
    }
    assert!(calls.get() > 0, "ветка veto действительно достигнута");
    assert!(controller.protection(0).expect("сохранённая защита").trial.is_none());
}
