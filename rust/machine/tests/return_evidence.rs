// Независимые проверки по задаче владельца; реализация при составлении не читалась.
use std::time::Duration;

use machine::auto::{Cause, Controller, Decision, Frame, LoadFrame, Protection, Settings, Timing, Workload};
use machine::thermal::Reading;

const TICK: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
struct State {
    temp: f64,
    busy: Option<f64>,
    ratio: Option<f64>,
    global: Option<f64>,
    relative: Option<f64>,
}

impl State {
    fn working(ratio: f64) -> Self {
        Self {
            temp: 60.0,
            busy: Some(0.9),
            ratio: Some(ratio),
            global: Some(0.7),
            relative: Some(0.5),
        }
    }

    fn idle() -> Self {
        Self {
            temp: 50.0,
            busy: Some(0.0),
            ratio: None,
            global: Some(0.0),
            relative: None,
        }
    }
}

struct Bench {
    controller: Controller,
    cap: [i32; 1],
    now: Duration,
    actions: Vec<(Duration, Decision)>,
}

impl Bench {
    fn new() -> Self {
        Self {
            controller: Controller::new(
                Settings {
                    limit: 86.0,
                    emergency: 95.0,
                    return_gap: 0.0,
                },
                Timing::REAL,
                1,
            ),
            cap: [54],
            now: Duration::ZERO,
            actions: Vec::new(),
        }
    }

    fn held() -> Self {
        let mut bench = Self::new();
        bench.cap[0] = 49;
        bench.controller.seed_protection(
            0,
            Protection {
                held: 49,
                reference: Some(Workload {
                    busy: 0.88,
                    ratio: 50.5,
                    global_busy: 0.6,
                    relative: 0.5,
                }),
                trial: None,
            },
        );
        bench
    }

    fn poll(&mut self, state: State) {
        let readings = [Some(Reading {
            temp: Some(state.temp),
            busy: state.busy,
            ratio: state.ratio,
        })];
        let relative = [state.relative];
        let frame = Frame {
            readings: &readings,
            ceilings: &self.cap,
            accepted: &[54],
            floor: &[27],
        };
        let load = LoadFrame {
            global_busy: state.global,
            relative: &relative,
        };
        let mut out = Vec::new();
        self.controller
            .step_loaded(self.now, &frame, &load, &|_, _, _| true, &mut out);
        for decision in out {
            self.cap[decision.block] = decision.to;
            self.actions.push((self.now, decision));
        }
        self.now += TICK;
    }

    fn run(&mut self, seconds: u64, state: State) {
        for _ in 0..seconds * 4 {
            self.poll(state);
        }
    }

    fn run_at_cap(&mut self, seconds: u64) {
        for _ in 0..seconds * 4 {
            self.poll(State::working(f64::from(self.cap[0])));
        }
    }

    fn returns(&self) -> Vec<(Duration, Decision)> {
        self.actions
            .iter()
            .copied()
            .filter(|(_, d)| d.cause == Cause::Return)
            .collect()
    }

    fn first_trial(&mut self) {
        for _ in 0..1000 {
            self.poll(State::working(f64::from(self.cap[0])));
            if self.cap[0] == 50 {
                return;
            }
        }
        panic!("сопоставимая работа должна разрешить пробу +1; постоянный запрет не является исправлением");
    }
}

#[test]
fn idle_after_real_shaped_emergency_never_restores() {
    let mut b = Bench::new();
    b.run(4, State::working(50.5));
    b.poll(State {
        temp: 98.0,
        busy: Some(0.88),
        ..State::working(50.5)
    });
    assert_eq!(b.cap[0], 49);
    b.run(1200, State::idle());
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
    assert!(b.controller.protection(0).is_some());
}

#[test]
fn busy_one_at_low_actual_ratio_is_not_cap_exposure() {
    let mut b = Bench::held();
    b.run(
        1200,
        State {
            busy: Some(1.0),
            ..State::working(20.0)
        },
    );
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
}

#[test]
fn reference_ratio_above_reduced_cap_does_not_deadlock_relevant_work() {
    let mut b = Bench::held();
    b.first_trial();
    assert_eq!(b.returns().len(), 1);
    assert_eq!(b.cap[0], 50);
    assert_eq!(b.controller.protection(0).expect("сохранённая защита").held, 49);
}

#[test]
fn every_upward_step_requires_post_write_loaded_time() {
    let mut b = Bench::held();
    b.run_at_cap(700);
    let returns = b.returns();
    assert!(returns.len() >= 2, "положительная ветка должна работать");
    for pair in returns.windows(2) {
        assert!(pair[1].0.saturating_sub(pair[0].0) >= Timing::REAL.pause);
        assert_eq!(pair[1].1.to, pair[0].1.to + 1);
    }
    assert!(returns.iter().all(|(_, d)| d.to == d.from + 1 && d.to <= 54));
}

#[test]
fn idle_trial_rolls_back_and_does_not_create_a_staircase() {
    let mut b = Bench::held();
    b.first_trial();
    b.run(300, State::idle());
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns().len(), 1);
    assert!(
        b.actions
            .iter()
            .any(|(_, d)| d.cause == Cause::Revert && d.from == 50 && d.to == 49)
    );
    assert_eq!(
        b.controller
            .protection(0)
            .expect("защита после неподтверждённой пробы")
            .held,
        49
    );
}

#[test]
fn trial_acknowledgement_without_achieved_ratio_is_not_validation() {
    let mut b = Bench::held();
    b.first_trial();
    b.run(300, State::working(20.0));
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns().len(), 1);
}

#[test]
fn every_missing_workload_input_holds_the_reduced_cap() {
    let base = State::working(49.0);
    for state in [
        State { busy: None, ..base },
        State { ratio: None, ..base },
        State { global: None, ..base },
        State { relative: None, ..base },
    ] {
        let mut b = Bench::held();
        b.run(600, state);
        assert_eq!(b.cap[0], 49);
        assert_eq!(b.returns(), []);
    }
}

#[test]
fn nonfinite_or_out_of_domain_workload_is_not_evidence() {
    let base = State::working(49.0);
    for state in [
        State {
            busy: Some(f64::NAN),
            ..base
        },
        State {
            busy: Some(1.1),
            ..base
        },
        State {
            ratio: Some(f64::INFINITY),
            ..base
        },
        State {
            global: Some(-0.1),
            ..base
        },
        State {
            relative: Some(f64::NAN),
            ..base
        },
    ] {
        let mut b = Bench::held();
        b.run(300, state);
        assert_eq!(b.cap[0], 49);
        assert_eq!(b.returns(), []);
    }
}

#[test]
fn old_protection_without_workload_reference_stays_unknown() {
    let mut b = Bench::held();
    b.controller.seed_protection(
        0,
        Protection {
            held: 49,
            reference: None,
            trial: None,
        },
    );
    b.run_at_cap(600);
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
}

#[test]
fn global_activity_and_relative_distribution_both_matter() {
    let base = State::working(49.0);
    for state in [
        State {
            global: Some(0.1),
            ..base
        },
        State {
            relative: Some(0.1),
            ..base
        },
    ] {
        let mut b = Bench::held();
        b.run(600, state);
        assert_eq!(b.cap[0], 49);
        assert_eq!(b.returns(), []);
    }
}

#[test]
fn active_peers_cannot_validate_a_starved_target() {
    let mut b = Bench::held();
    b.run(
        600,
        State {
            busy: Some(0.01),
            global: Some(0.9),
            relative: Some(0.01),
            ..State::working(20.0)
        },
    );
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
}

#[test]
fn missing_poll_breaks_otherwise_qualifying_exposure() {
    let mut b = Bench::held();
    b.run_at_cap(100);
    b.poll(State {
        global: None,
        ..State::working(49.0)
    });
    b.run_at_cap(30);
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
}

#[test]
fn unobserved_time_gap_is_not_loaded_exposure() {
    let mut b = Bench::held();
    b.run_at_cap(100);
    b.now += Duration::from_secs(600);
    b.run_at_cap(30);
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
}

#[test]
fn automatic_forget_retains_cut_but_resets_exposure() {
    let mut b = Bench::held();
    b.run_at_cap(100);
    b.controller.forget();
    assert_eq!(b.controller.protection(0).expect("защита не забыта").held, 49);
    b.run_at_cap(30);
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns(), []);
}

#[test]
fn new_cut_preempts_trial_even_when_workload_is_unknown() {
    let mut b = Bench::held();
    b.first_trial();
    b.poll(State {
        temp: 98.0,
        busy: None,
        ratio: Some(49.0),
        global: None,
        relative: None,
    });
    assert_eq!(b.cap[0], 48);
    assert!(b.actions.last().is_some_and(|(_, d)| d.cause == Cause::Emergency));
    b.run(600, State::idle());
    assert_eq!(b.cap[0], 48);
    assert_eq!(b.returns().len(), 1);
}

#[test]
fn restoring_protection_does_not_restore_elapsed_certification() {
    let mut original = Bench::held();
    original.run_at_cap(100);
    let saved = original.controller.protection(0).expect("сохраняемая защита");
    let mut restored = Bench::new();
    restored.cap[0] = saved.held;
    restored.controller.seed_protection(0, saved);
    restored.now = Duration::from_secs(3600);
    restored.run_at_cap(30);
    assert_eq!(restored.cap[0], 49);
    assert_eq!(restored.returns(), []);
}

#[test]
fn unknown_new_cut_cannot_reuse_an_older_workload_envelope() {
    let mut b = Bench::held();
    b.poll(State {
        temp: 98.0,
        busy: None,
        global: None,
        ..State::working(49.0)
    });
    assert_eq!(b.cap[0], 48);
    b.run_at_cap(600);
    assert_eq!(b.cap[0], 48);
    assert_eq!(b.returns(), []);
    assert!(
        b.controller
            .protection(0)
            .expect("защита после новой отсечки")
            .reference
            .is_none()
    );
}

#[test]
fn unchanged_old_cap_frequency_does_not_certify_the_next_cap() {
    let mut b = Bench::held();
    b.first_trial();
    b.run(150, State::working(49.0));
    assert_eq!(b.cap[0], 49);
    assert_eq!(b.returns().len(), 1);
    assert!(b.actions.iter().any(|(_, d)| d.cause == Cause::Revert));
}

#[test]
fn one_pending_trial_keeps_other_cores_from_invalidating_it() {
    let mut c = Controller::new(
        Settings {
            limit: 86.0,
            emergency: 95.0,
            return_gap: 0.0,
        },
        Timing::REAL,
        2,
    );
    let reference = Some(Workload {
        busy: 0.88,
        ratio: 50.5,
        global_busy: 0.6,
        relative: 0.5,
    });
    for block in 0..2 {
        c.seed_protection(
            block,
            Protection {
                held: 49,
                reference,
                trial: None,
            },
        );
    }
    let mut cap = [49, 49];
    let mut seen = [false, false];
    for tick in 0..2800_u64 {
        let readings = cap.map(|ceiling| {
            Some(Reading {
                temp: Some(60.0),
                busy: Some(0.9),
                ratio: Some(f64::from(ceiling)),
            })
        });
        let frame = Frame {
            readings: &readings,
            ceilings: &cap,
            accepted: &[54, 54],
            floor: &[27, 27],
        };
        let load = LoadFrame {
            global_busy: Some(0.7),
            relative: &[Some(0.5), Some(0.5)],
        };
        let mut out = Vec::new();
        c.step_loaded(
            Duration::from_millis(tick * 250),
            &frame,
            &load,
            &|_, _, _| true,
            &mut out,
        );
        for action in out {
            cap[action.block] = action.to;
            if action.cause == Cause::Return {
                seen[action.block] = true;
            }
        }
        let trials = (0..2)
            .filter(|&block| c.protection(block).is_some_and(|p| p.trial.is_some()))
            .count();
        assert!(trials <= 1, "параллельные пробы меняют общий профиль друг у друга");
    }
    assert!(
        seen.into_iter().all(|value| value),
        "одна проба не должна навсегда блокировать другое ядро"
    );
}
