// Независимые проверки ручного регулятора по задаче владельца от 19.10.
// Числа ниже — разнесённые синтетические условия, не измеренные сроки ноутбука и не настройки по умолчанию.
// Размер обычного снижения пока не выбран: проверяем только направление и допустимую границу.

use std::time::Duration;

use machine::auto::{Cause, Controller, Decision, Frame, LoadFrame, Protection, Settings, Timing, Workload};
use machine::thermal::Reading;

const TICK: Duration = Duration::from_millis(250);
const TARGET: f64 = 84.0;
const EMERGENCY: f64 = 100.0;
const FLOOR: i32 = 27;

#[test]
fn exported_window_keeps_its_actual_averages_distinct_from_the_last_poll_and_cap() {
    let mut b = Bench::new(1, timing(0, 30), 0.0);
    assert!(b.controller.window_span().is_none());
    for (temp, ratio, busy) in [
        (40.0, 20.0, 0.2),
        (50.0, 30.0, 0.4),
        (60.0, 40.0, 0.6),
        (70.0, 50.0, 0.8),
        (80.0, 60.0, 1.0),
    ] {
        b.poll(&[Sample {
            busy: Some(busy),
            ..Sample::working(temp, ratio)
        }]);
    }
    assert_eq!(
        b.controller.window_span(),
        Some((Duration::ZERO, Duration::from_secs(1)))
    );
    let summary = b.controller.window_readings().next().expect("один блок");
    assert!(summary.temp.is_some_and(|t| (t - 60.0).abs() < 1e-9));
    assert!(summary.busy.is_some_and(|t| (t - 0.6).abs() < 1e-9));
    assert!(summary.ratio.is_some_and(|t| (t - 46.666_666_666_666_664).abs() < 1e-9));
    b.poll(&[Sample::working(83.0, 33.0)]);
    assert_eq!(
        b.controller.window_span(),
        Some((Duration::ZERO, Duration::from_secs(1)))
    );
    let old = b.controller.window_readings().next().expect("предыдущее окно");
    assert_eq!(old, summary, "начатое окно не подменяет завершённое последним опросом");
}

#[test]
fn exported_window_is_invalidated_by_a_changed_peer_or_explicit_forget() {
    let mut b = Bench::new(2, timing(2, 30), 0.0);
    b.run_at_caps(5, &[60.0, 60.0]);
    assert!(b.controller.window_span().is_some());
    b.caps[1] -= 1;
    b.at_caps(&[60.0, 60.0]);
    assert!(b.controller.window_span().is_none());
    b.run_at_caps(13, &[60.0, 60.0]);
    assert!(b.controller.window_span().is_some());
    b.controller.forget_block(0);
    assert!(b.controller.window_span().is_none());
    b.run_at_caps(5, &[60.0, 60.0]);
    assert!(b.controller.window_span().is_some());
    b.controller.forget();
    assert!(b.controller.window_span().is_none());
}

#[derive(Clone, Copy)]
struct Sample {
    temp: f64,
    busy: Option<f64>,
    ratio: Option<f64>,
    global: Option<f64>,
    relative: Option<f64>,
}

impl Sample {
    fn working(temp: f64, ratio: f64) -> Self {
        Self {
            temp,
            busy: Some(0.9),
            ratio: Some(ratio),
            global: Some(0.7),
            relative: Some(0.5),
        }
    }
}

fn timing(settle: u64, proof: u64) -> Timing {
    Timing {
        window: Duration::from_secs(1),
        recheck: Duration::from_secs(1),
        pause: Duration::from_secs(proof),
        settle: Duration::from_secs(settle),
    }
}

struct Bench {
    controller: Controller,
    caps: Vec<i32>,
    accepted: Vec<i32>,
    floors: Vec<i32>,
    now: Duration,
    actions: Vec<(Duration, Decision)>,
}

impl Bench {
    fn new(blocks: usize, times: Timing, gap: f64) -> Self {
        Self {
            controller: Controller::new(
                Settings {
                    limit: TARGET,
                    emergency: EMERGENCY,
                    return_gap: gap,
                },
                times,
                blocks,
            ),
            caps: vec![50; blocks],
            accepted: vec![54; blocks],
            floors: vec![FLOOR; blocks],
            now: Duration::ZERO,
            actions: Vec::new(),
        }
    }

    fn held(times: Timing, gap: f64) -> Self {
        let mut bench = Self::new(1, times, gap);
        bench.caps[0] = 49;
        bench.controller.seed_protection(
            0,
            Protection {
                held: 49,
                reference: Some(Workload {
                    busy: 0.8,
                    ratio: 50.5,
                    global_busy: 0.6,
                    relative: 0.5,
                }),
                trial: None,
            },
        );
        bench
    }

    fn poll(&mut self, samples: &[Sample]) {
        let readings: Vec<Option<Reading>> = samples
            .iter()
            .map(|s| {
                Some(Reading {
                    temp: Some(s.temp),
                    busy: s.busy,
                    ratio: s.ratio,
                })
            })
            .collect();
        let relative: Vec<Option<f64>> = samples.iter().map(|s| s.relative).collect();
        let frame = Frame {
            readings: &readings,
            ceilings: &self.caps,
            accepted: &self.accepted,
            floor: &self.floors,
        };
        let load = LoadFrame {
            global_busy: samples[0].global,
            relative: &relative,
        };
        let mut out = Vec::new();
        self.controller
            .step_loaded(self.now, &frame, &load, &|_, _, _| true, &mut out);
        for decision in out {
            assert_eq!(
                decision.from, self.caps[decision.block],
                "решение относится к действующему потолку"
            );
            assert!(decision.to >= FLOOR, "снижение не пересекает допустимую границу");
            self.caps[decision.block] = decision.to;
            self.actions.push((self.now, decision));
        }
        self.now += TICK;
    }

    fn at_caps(&mut self, temps: &[f64]) {
        let samples: Vec<Sample> = temps
            .iter()
            .zip(&self.caps)
            .map(|(&temp, &cap)| Sample::working(temp, f64::from(cap)))
            .collect();
        self.poll(&samples);
    }

    fn run(&mut self, ticks: u64, sample: Sample) {
        for _ in 0..ticks {
            self.poll(&[sample]);
        }
    }

    fn run_at_caps(&mut self, ticks: u64, temps: &[f64]) {
        for _ in 0..ticks {
            self.at_caps(temps);
        }
    }

    fn of(&self, cause: Cause) -> Vec<(Duration, Decision)> {
        self.actions.iter().copied().filter(|(_, d)| d.cause == cause).collect()
    }

    fn begin_trial(&mut self) -> Duration {
        for _ in 0..200 {
            self.at_caps(&[60.0]);
            if let Some(&(at, _)) = self.of(Cause::Return).first() {
                return at;
            }
        }
        panic!("полная сопоставимая работа должна разрешить пробу +1");
    }
}

#[test]
fn arithmetic_mean_above_target_cuts_when_median_is_below_target() {
    let mut b = Bench::new(1, timing(0, 30), 0.0);
    for _ in 0..4 {
        for temp in [82.0, 82.0, 82.0, 98.0] {
            b.at_caps(&[temp]);
        }
    }
    let cuts = b.of(Cause::Target);
    assert!(
        !cuts.is_empty(),
        "среднее превышает 84, медиана равна 82: {:?}",
        b.actions
    );
    assert!(cuts.iter().all(|(_, d)| d.to < d.from));
    assert!(
        b.of(Cause::Emergency).is_empty(),
        "все показания ниже аварийного предела"
    );
}

#[test]
fn the_first_complete_hot_window_needs_no_previous_window() {
    let times = timing(0, 30);
    let mut b = Bench::new(1, times, 0.0);
    b.run_at_caps(6, &[90.0]);
    let first = b
        .of(Cause::Target)
        .first()
        .copied()
        .expect("первое горячее окно вызывает снижение");
    assert!(
        first.0 <= times.window + TICK,
        "прошлого окна для сравнения не требуется: {first:?}"
    );
}

#[test]
fn falling_but_still_over_target_does_not_veto_ordinary_correction() {
    let mut b = Bench::new(1, timing(0, 30), 0.0);
    for n in 0..40_u32 {
        b.at_caps(&[94.0 - f64::from(n) * 0.2]);
    }
    assert!(
        b.of(Cause::Target).len() >= 2,
        "снижающаяся температура всё ещё выше цели: {:?}",
        b.actions
    );
    assert!(
        b.of(Cause::Emergency).is_empty(),
        "проверяется обычное снижение, показания ниже аварийного предела"
    );
}

#[test]
fn a_mean_equal_to_target_does_not_cut() {
    let mut b = Bench::new(1, timing(0, 30), 0.0);
    b.run_at_caps(32, &[TARGET]);
    assert!(
        b.actions.is_empty(),
        "равенство цели не является превышением: {:?}",
        b.actions
    );
}

#[test]
fn ordinary_repeat_waits_for_settling_and_a_fresh_full_window() {
    let times = timing(4, 30);
    let mut b = Bench::new(1, times, 0.0);
    b.run_at_caps(72, &[90.0]);
    let cuts = b.of(Cause::Target);
    assert!(
        cuts.len() >= 2,
        "после ожидания снижение должно возобновляться: {cuts:?}"
    );
    for pair in cuts.windows(2) {
        assert!(
            pair[1].0.saturating_sub(pair[0].0) >= times.settle + times.window,
            "пауза и новое окно не заменяют друг друга: {pair:?}"
        );
    }
}

#[test]
fn emergency_at_threshold_bypasses_an_ordinary_settling_pause() {
    let mut b = Bench::new(1, timing(20, 30), 0.0);
    for _ in 0..16 {
        b.at_caps(&[90.0]);
        if !b.of(Cause::Target).is_empty() {
            break;
        }
    }
    let last = b
        .of(Cause::Target)
        .last()
        .copied()
        .expect("обычное снижение до аварийного события");
    let at = b.now;
    b.at_caps(&[EMERGENCY]);
    let emergencies = b.of(Cause::Emergency);
    assert_eq!(emergencies.len(), 1);
    assert_eq!(emergencies[0].0, at, "аварийное снижение происходит на этом показании");
    assert!(at.saturating_sub(last.0) < Duration::from_secs(20));
    assert!(emergencies[0].1.to < emergencies[0].1.from);
}

#[test]
fn a_forward_polling_gap_cannot_cancel_a_pending_settling_deadline() {
    let times = timing(20, 30);
    let mut b = Bench::new(1, times, 0.0);
    b.at_caps(&[EMERGENCY]);
    assert_eq!(b.of(Cause::Emergency).len(), 1);
    b.at_caps(&[90.0]);
    b.now += Duration::from_secs(3);
    b.run_at_caps(16, &[90.0]);
    assert!(b.now < times.settle);
    assert!(
        b.of(Cause::Target).is_empty(),
        "пропуск опросов сбрасывает доказательство, но не сокращает ожидаемую паузу: {:?}",
        b.actions
    );
    b.run_at_caps(96, &[90.0]);
    let first = b
        .of(Cause::Target)
        .first()
        .copied()
        .expect("после паузы и свежего окна коррекция возобновляется");
    assert!(first.0 >= times.settle + times.window);
}

#[test]
fn changing_only_a_peer_ceiling_discards_old_hot_window_samples() {
    let mut b = Bench::new(2, timing(0, 30), 0.0);
    b.run_at_caps(3, &[98.0, 60.0]);
    assert!(b.actions.is_empty(), "первое окно ещё не завершено");
    b.caps[1] = 49;
    b.run_at_caps(8, &[80.0, 60.0]);
    assert!(
        b.actions.is_empty(),
        "старые горячие отсчёты не осуждают новую полную конфигурацию: {:?}",
        b.actions
    );
}

#[test]
fn a_changed_peer_cannot_use_elapsed_time_as_a_complete_new_hot_window() {
    let times = timing(0, 30);
    let mut b = Bench::new(2, times, 0.0);
    b.run_at_caps(3, &[90.0, 60.0]);
    b.caps[1] = 49;
    let changed_at = b.now;
    b.run_at_caps(3, &[90.0, 60.0]);
    assert!(b.actions.is_empty(), "новое окно ещё не прошло");
    b.run_at_caps(8, &[90.0, 60.0]);
    let first = b
        .of(Cause::Target)
        .first()
        .copied()
        .expect("свежая горячая конфигурация всё же проверяется");
    assert!(first.0.saturating_sub(changed_at) >= times.window);
}

#[test]
fn positive_return_gap_holds_a_cooler_but_not_cool_enough_block() {
    let mut b = Bench::held(timing(0, 3), 4.0);
    b.run_at_caps(80, &[81.0]);
    assert_eq!(b.caps, [49]);
    assert!(
        b.of(Cause::Return).is_empty(),
        "81 ниже цели84, но выше порога возврата80"
    );
}

#[test]
fn equality_at_the_derived_return_threshold_allows_a_loaded_trial() {
    let mut b = Bench::held(timing(0, 3), 4.0);
    b.run_at_caps(32, &[80.0]);
    assert!(
        !b.of(Cause::Return).is_empty(),
        "равенство порогу возврата допускает пробу при работе"
    );
}

#[test]
fn zero_return_gap_allows_the_target_itself() {
    let mut b = Bench::held(timing(0, 3), 0.0);
    b.run_at_caps(32, &[TARGET]);
    assert!(!b.of(Cause::Return).is_empty(), "нулевой зазор допустим");
    assert!(
        b.of(Cause::Target).is_empty(),
        "цель без превышения не вызывает обычное снижение"
    );
}

#[test]
fn configured_work_duration_changes_when_a_trial_is_possible() {
    let mut short = Bench::held(timing(0, 3), 0.0);
    let mut long = Bench::held(timing(0, 12), 0.0);
    short.run_at_caps(32, &[60.0]);
    long.run_at_caps(32, &[60.0]);
    assert!(
        !short.of(Cause::Return).is_empty(),
        "короткий срок при той же полной работе уже прошёл"
    );
    assert!(long.of(Cause::Return).is_empty(), "длинный срок ещё не прошёл");
    long.run_at_caps(32, &[60.0]);
    assert!(
        !long.of(Cause::Return).is_empty(),
        "длинный срок тоже должен давать положительную ветку"
    );
}

#[test]
fn idle_and_each_missing_work_input_cannot_certify_an_upward_step() {
    let work = Sample::working(60.0, 49.0);
    for sample in [
        Sample {
            busy: Some(0.0),
            ratio: None,
            global: Some(0.0),
            relative: None,
            ..work
        },
        Sample { busy: None, ..work },
        Sample { ratio: None, ..work },
        Sample { global: None, ..work },
        Sample { relative: None, ..work },
        Sample {
            busy: Some(1.0),
            ratio: Some(20.0),
            ..work
        },
    ] {
        let mut b = Bench::held(timing(0, 3), 0.0);
        b.run(160, sample);
        assert_eq!(b.caps, [49]);
        assert!(
            b.of(Cause::Return).is_empty(),
            "неполные или неподходящие признаки работы не дают возврат"
        );
    }
}

#[test]
fn pretrial_work_cannot_certify_the_new_ceiling_before_fresh_duration() {
    let times = timing(0, 4);
    let mut b = Bench::held(times, 0.0);
    let at = b.begin_trial();
    assert_eq!(b.caps, [50]);
    b.run_at_caps(10, &[60.0]);
    let protection = b.controller.protection(0).expect("защита во время пробы");
    assert_eq!(
        protection.held, 49,
        "проба не подтверждается накопленным до записи временем"
    );
    assert!(protection.trial.is_some());
    assert_eq!(b.of(Cause::Return).len(), 1);
    b.run_at_caps(60, &[60.0]);
    let returns = b.of(Cause::Return);
    assert!(
        returns.len() >= 2,
        "после свежего подтверждения возможны следующие шаги"
    );
    assert!(returns[1].0.saturating_sub(at) >= times.pause);
    assert!(returns.iter().all(|(_, d)| d.to == d.from + 1));
}

#[test]
fn old_cap_frequency_during_trial_cannot_promote_the_new_cap() {
    let mut b = Bench::held(timing(0, 3), 0.0);
    b.begin_trial();
    for _ in 0..40 {
        b.poll(&[Sample::working(60.0, 49.0)]);
        assert_eq!(b.controller.protection(0).expect("защита до подтверждения").held, 49);
        if !b.of(Cause::Revert).is_empty() {
            assert_eq!(b.caps, [49]);
            return;
        }
    }
    panic!("непроверенная новая частота должна откатиться");
}

#[test]
fn an_inconclusive_idle_trial_can_be_retried_after_new_real_work() {
    let mut b = Bench::held(timing(0, 3), 0.0);
    b.begin_trial();
    let work = Sample::working(50.0, 49.0);
    b.run(
        40,
        Sample {
            busy: Some(0.0),
            ratio: None,
            global: Some(0.0),
            relative: None,
            ..work
        },
    );
    assert_eq!(b.caps, [49]);
    assert!(!b.of(Cause::Revert).is_empty(), "неподтверждённая проба откатывается");
    assert_eq!(b.of(Cause::Return).len(), 1);
    b.run_at_caps(40, &[60.0]);
    assert!(
        b.of(Cause::Return).len() >= 2,
        "нехватка доказательства не является доказанным перегревом"
    );
}
