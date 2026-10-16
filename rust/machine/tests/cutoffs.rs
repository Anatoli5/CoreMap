// Правила двух отсечек и возврата — по брифу T (outputs/cutoffs-brief-2026-10-16.md, DECISIONS #40, подзапись
// «две отсечки»), а не по коду регулятора: каждый тест — строка брифа. Опрос — раз в 0,25 с, как у CoreMapTray;
// сроки — настоящие (`Timing::REAL`: окно медианы 2 с, повтор аварийной проверки 1 с, пауза возврата 2 мин).
// Память срезов — настоящая (machine::memory), как её спрашивает программа входа.

use std::time::Duration;

use machine::auto::{Cause, Controller, Decision, Frame, Settings, Timing};
use machine::memory::Memory;
use machine::thermal::Reading;

const LIMIT: f64 = 85.0;
const EMERGENCY: f64 = 95.0;
const STEP: Duration = Duration::from_millis(250);
const FLOOR: i32 = 27;

/// Один блок в опросе: температура, доля работы, фактический множитель (None — как у простаивающего ядра).
#[derive(Debug, Clone, Copy)]
struct Feel {
    temp: f64,
    busy: f64,
    ratio: Option<f64>,
}

/// Нагруженный блок, работающий на своём потолке.
fn hot(temp: f64) -> Feel {
    Feel {
        temp,
        busy: 1.0,
        ratio: None, // filled with the ceiling by `Bench::poll`
    }
}

/// Простаивающий блок: доля работы 0, множитель не известен.
fn idle(temp: f64) -> Feel {
    Feel {
        temp,
        busy: 0.0,
        ratio: Some(f64::NAN), // marker: no ratio at all
    }
}

/// Стенд: регулятор, потолки, профиль, память срезов, часы. Решения ставятся сразу, как их ставит программа входа.
struct Bench {
    c: Controller,
    ceilings: Vec<i32>,
    accepted: Vec<i32>,
    floor: Vec<i32>,
    memory: Memory,
    t: Duration,
    acts: Vec<(Duration, Decision)>,
    out: Vec<Decision>,
}

impl Bench {
    fn new(ceilings: &[i32]) -> Self {
        Self {
            c: Controller::new(
                Settings {
                    limit: LIMIT,
                    emergency: EMERGENCY,
                },
                Timing::REAL,
                ceilings.len(),
            ),
            ceilings: ceilings.to_vec(),
            accepted: ceilings.to_vec(),
            floor: vec![FLOOR; ceilings.len()],
            memory: Memory::default(),
            t: Duration::ZERO,
            acts: Vec::new(),
            out: Vec::new(),
        }
    }

    /// Один опрос в момент `self.t` с этими показаниями; время идёт дальше на 0,25 с.
    fn poll(&mut self, feels: &[Feel]) {
        let readings: Vec<Option<Reading>> = feels
            .iter()
            .zip(&self.ceilings)
            .map(|(f, &c)| {
                Some(Reading {
                    temp: Some(f.temp),
                    busy: Some(f.busy),
                    ratio: match f.ratio {
                        None => Some(f64::from(c)),
                        Some(r) if r.is_nan() => None,
                        Some(r) => Some(r),
                    },
                })
            })
            .collect();
        let frame = Frame {
            readings: &readings,
            ceilings: &self.ceilings,
            accepted: &self.accepted,
            floor: &self.floor,
        };
        let memory = &self.memory;
        let allowed = |b: usize, to: i32, die: Option<f64>| memory.allows(b, to, LIMIT, die);
        self.c.step(self.t, &frame, &allowed, &mut self.out);
        for d in std::mem::take(&mut self.out) {
            assert_eq!(
                self.ceilings[d.block], d.from,
                "a decision names the ceiling that stands: {d:?}"
            );
            self.ceilings[d.block] = d.to;
            if d.cause != Cause::Return {
                self.memory.cut(d.block, d.from, d.temp, d.die);
            }
            self.acts.push((self.t, d));
        }
        self.t += STEP;
    }

    /// `seconds` секунд опросов; `feel(t)` — показания в момент t.
    fn run(&mut self, seconds: f64, feel: impl Fn(Duration) -> Vec<Feel>) {
        let end = self.t + Duration::from_secs_f64(seconds);
        while self.t < end {
            let feels = feel(self.t);
            self.poll(&feels);
        }
    }

    fn acts(&self, cause: Cause) -> Vec<(Duration, Decision)> {
        self.acts.iter().filter(|(_, d)| d.cause == cause).copied().collect()
    }

    fn since(&self, from: Duration, cause: Cause) -> Vec<(Duration, Decision)> {
        self.acts(cause).into_iter().filter(|(t, _)| *t >= from).collect()
    }
}

fn secs(t: Duration) -> f64 {
    t.as_secs_f64()
}

// ----- аварийная отсечка -----

#[test]
fn one_reading_at_the_emergency_limit_cuts_that_block_at_once() {
    // «Trigger: one reading of a block ≥ the emergency limit → that block loses one step at once».
    let mut b = Bench::new(&[50]);
    b.run(4.0, |_| vec![hot(70.0)]);
    let at = b.t;
    b.poll(&[hot(95.0)]);
    b.run(6.0, |_| vec![hot(70.0)]);
    let cuts = b.acts(Cause::Emergency);
    assert_eq!(cuts.len(), 1, "{:?}", b.acts);
    assert_eq!(cuts[0].0, at, "at the reading itself, not at the end of a window");
    assert_eq!((cuts[0].1.from, cuts[0].1.to), (50, 49));
    assert!((cuts[0].1.temp - 95.0).abs() < 1e-9, "the reading that triggered it");
    assert!(
        b.acts(Cause::Target).is_empty(),
        "one hot reading moves no 2-s median: {:?}",
        b.acts
    );
}

#[test]
fn the_emergency_check_of_a_block_resumes_one_second_later_and_cuts_again_if_still_hot() {
    // «Repeat: that block's emergency check resumes 1 s later; if a reading is still ≥ the limit, another step.»
    let mut b = Bench::new(&[50]);
    b.run(4.0, |_| vec![hot(70.0)]);
    let from = b.t;
    b.run(3.1, |_| vec![hot(97.0)]);
    let times: Vec<f64> = b
        .acts(Cause::Emergency)
        .iter()
        .map(|(t, _)| secs(t.saturating_sub(from)))
        .collect();
    assert_eq!(
        times,
        [0.0, 1.0, 2.0, 3.0],
        "a step at once, then one per second, none between"
    );
}

#[test]
fn a_reading_below_the_emergency_limit_after_the_pause_cuts_nothing_more() {
    let mut b = Bench::new(&[50]);
    b.run(4.0, |_| vec![hot(70.0)]);
    let from = b.t;
    // Hot for 0.75 s (inside the 1-s pause), then cool again.
    b.run(0.75, |_| vec![hot(97.0)]);
    b.run(5.0, |_| vec![hot(70.0)]);
    assert_eq!(b.since(from, Cause::Emergency).len(), 1, "{:?}", b.acts);
}

#[test]
fn every_block_has_its_own_emergency_check() {
    let mut b = Bench::new(&[50, 50, 50]);
    b.run(4.0, |_| vec![hot(70.0); 3]);
    let at = b.t;
    b.poll(&[hot(96.0), hot(70.0), hot(99.0)]);
    let cut: Vec<usize> = b
        .acts(Cause::Emergency)
        .iter()
        .filter(|(t, _)| *t == at)
        .map(|(_, d)| d.block)
        .collect();
    assert_eq!(cut, [0, 2], "both hot blocks in the same reading");
}

// ----- целевая отсечка -----

#[test]
fn the_target_cut_fires_every_two_seconds_while_the_median_is_above_and_not_falling() {
    // «every 2 s, per block … median above the limit and not lower than the previous 2-s median: −1 step».
    let mut b = Bench::new(&[50]);
    b.run(4.0, |_| vec![hot(70.0)]);
    let from = b.t;
    b.run(12.0, |_| vec![hot(88.0)]); // above 85, below the emergency limit; flat = not falling
    let cuts = b.since(from, Cause::Target);
    assert!((5..=6).contains(&cuts.len()), "one per 2-s window: {cuts:?}");
    assert!(
        cuts.windows(2)
            .all(|w| w[1].0.saturating_sub(w[0].0) == Duration::from_secs(2)),
        "spaced by the window: {cuts:?}"
    );
    assert!(cuts.iter().all(|(_, d)| d.to == d.from - 1), "one step each");
    assert_eq!(b.acts(Cause::Emergency), []);
}

#[test]
fn the_target_cut_waits_while_the_median_falls_even_above_the_limit() {
    // «If it is above but falling: wait. The last step is still working.»
    let mut b = Bench::new(&[50]);
    b.run(4.0, |_| vec![hot(93.0)]);
    let from = b.t;
    // 93 → 86 over 28 s (longer than any averaging window of the old controller): every 2-s median lower than the one
    // before, all above 85.
    b.run(28.0, move |t| {
        vec![hot(93.0 - 0.25 * t.saturating_sub(from).as_secs_f64())]
    });
    assert_eq!(b.since(from + STEP, Cause::Target), [], "falling: no step");
    // Flat again above the limit: the cut comes back.
    let flat = b.t;
    b.run(6.0, |_| vec![hot(87.0)]);
    assert!(!b.since(flat, Cause::Target).is_empty(), "{:?}", b.acts);
}

#[test]
fn a_rising_median_above_the_limit_is_cut_and_a_spike_under_the_emergency_limit_is_not() {
    let mut b = Bench::new(&[50, 50]);
    b.run(4.0, |_| vec![hot(70.0), hot(70.0)]);
    let from = b.t;
    // Block 0: rising 86 → 92. Block 1: 70, with one 92 reading in every window - the median stays 70.
    b.run(6.0, move |t| {
        let spike = t.saturating_sub(from).as_millis() % 2000 == 750; // one reading in every 2-s window
        vec![
            hot(86.0 + t.saturating_sub(from).as_secs_f64()),
            hot(if spike { 92.0 } else { 70.0 }),
        ]
    });
    let cut: Vec<usize> = b.since(from, Cause::Target).iter().map(|(_, d)| d.block).collect();
    assert!(!cut.is_empty() && cut.iter().all(|&blk| blk == 0), "{cut:?}");
}

#[test]
fn no_load_condition_an_idle_block_above_a_limit_is_cut() {
    // «если он не загружен… и у него температура выше предела… как это так» — no load conditions anywhere.
    let mut b = Bench::new(&[50]);
    b.run(4.0, |_| vec![idle(70.0)]);
    let from = b.t;
    b.run(6.0, |_| vec![idle(88.0)]);
    assert!(!b.since(from, Cause::Target).is_empty(), "target: {:?}", b.acts);
    let at = b.t;
    b.poll(&[idle(96.0)]);
    assert_eq!(b.since(at, Cause::Emergency).len(), 1, "emergency: {:?}", b.acts);
    // Without a known multiplier the step is one below the ceiling.
    assert!(b.acts.iter().all(|(_, d)| d.to == d.from - 1), "{:?}", b.acts);
}

#[test]
fn the_step_goes_below_the_actual_multiplier_and_never_below_the_floor() {
    // «the existing lowering target («one step below its actual multiplier», never below the range floor)».
    let mut b = Bench::new(&[53]);
    b.run(4.0, |_| vec![hot(70.0)]);
    b.poll(&[Feel {
        temp: 96.0,
        busy: 1.0,
        ratio: Some(49.87),
    }]);
    let cut = b.acts(Cause::Emergency);
    assert_eq!(
        (cut[0].1.from, cut[0].1.to),
        (53, 48),
        "ran ×49.87 under ×53: next ceiling ×48"
    );

    let mut b = Bench::new(&[29]);
    b.run(4.0, |_| vec![hot(70.0)]);
    b.poll(&[Feel {
        temp: 96.0,
        busy: 1.0,
        ratio: Some(20.0),
    }]);
    let cut = b.acts(Cause::Emergency);
    assert_eq!(cut[0].1.to, FLOOR, "throttled to ×20: the step stops at the floor");
    b.run(10.0, |_| vec![hot(99.0)]);
    assert_eq!(b.acts.len(), 1, "at the floor nothing more happens: {:?}", b.acts);
}

// ----- возврат к профилю -----

/// Блок 0 срезан один раз (с ×50 на ×49) целевой отсечкой при остальных блоках на `others` °C.
fn cut_once(blocks: usize, others: f64) -> Bench {
    let mut b = Bench::new(&vec![50; blocks]);
    let row = move |first: f64| {
        let mut v = vec![hot(others); blocks];
        v[0] = hot(first);
        v
    };
    b.run(4.0, move |_| row(70.0));
    while b.acts.is_empty() {
        b.poll(&row(88.0));
    }
    assert_eq!(b.ceilings[0], 49, "{:?}", b.acts);
    b
}

#[test]
fn a_cut_block_returns_one_step_after_a_full_pause_below_the_limit_and_never_above_its_profile() {
    let mut b = cut_once(2, 60.0);
    let cut = b.acts[0].0;
    // Cooler than at the cut: the die (block 1) is 55 now, it was 60.
    b.run(400.0, |_| vec![hot(70.0), hot(55.0)]);
    let back = b.acts(Cause::Return);
    let times: Vec<f64> = back.iter().map(|(t, _)| secs(t.saturating_sub(cut))).collect();
    assert_eq!(
        back.len(),
        1,
        "×49 → ×50, the profile's ceiling, and no further: {times:?}"
    );
    assert!(
        (120.0..=124.0).contains(&times[0]),
        "after the 2-min pause, not before: {times:?}"
    );
    assert_eq!((back[0].1.from, back[0].1.to), (49, 50));

    // Three steps below the profile: one step per full pause, up to the profile ceiling.
    let mut b2 = cut_once(2, 60.0);
    b2.ceilings[0] = 47; // lowered three steps in all (as by three cuts); profile ×50
    let cool = b2.t;
    b2.run(600.0, |_| vec![hot(70.0), hot(55.0)]);
    let times: Vec<f64> = b2
        .acts(Cause::Return)
        .iter()
        .map(|(t, _)| secs(t.saturating_sub(cool)))
        .collect();
    assert_eq!(b2.ceilings[0], 50, "{times:?}");
    assert_eq!(times.len(), 3, "{times:?}");
    assert!(
        times.windows(2).all(|w| w[1] - w[0] >= 120.0),
        "a full pause per step: {times:?}"
    );
}

#[test]
fn one_median_above_the_limit_in_the_pause_restarts_it() {
    // «returns +1 step when all of its 2-s medians stayed at or below the limit for the whole pause».
    let mut b = cut_once(2, 60.0);
    b.floor[0] = 49; // at the floor: the hot window below cannot cut again, only the pause is tested
    let cool = b.t;
    b.run(60.0, |_| vec![hot(70.0), hot(55.0)]);
    let hot_at = b.t;
    // 4 s: at least one whole 2-s window of them, wherever the windows begin.
    b.run(4.0, |_| vec![hot(88.0), hot(55.0)]);
    let after = b.t;
    b.run(200.0, |_| vec![hot(70.0), hot(55.0)]);
    let back = b.acts(Cause::Return);
    assert_eq!(back.len(), 1, "{back:?}");
    let at = back[0].0;
    assert!(
        at >= after + Duration::from_secs(116) && at > cool + Duration::from_secs(124),
        "not 2 min after the cut ({}), but 2 min after the hot window at {} s: returned at {} s",
        secs(cool),
        secs(hot_at),
        secs(at)
    );
}

#[test]
fn one_block_per_pause_the_coolest_first() {
    let mut b = Bench::new(&[50, 50, 50]);
    b.run(4.0, |_| vec![hot(70.0), hot(70.0), hot(60.0)]);
    while b.ceilings[0] == 50 || b.ceilings[1] == 50 {
        let a = if b.ceilings[0] == 50 { 88.0 } else { 70.0 };
        let c = if b.ceilings[1] == 50 { 88.0 } else { 70.0 };
        b.poll(&[hot(a), hot(c), hot(60.0)]);
    }
    let cool = b.t;
    // Block 1 is the cooler of the two; the die for each is lower than at its cut.
    b.run(300.0, |_| vec![hot(72.0), hot(64.0), hot(50.0)]);
    let back = b.acts(Cause::Return);
    assert!(back.len() >= 2, "{back:?}");
    assert_eq!((back[0].1.block, back[1].1.block), (1, 0), "the coolest first");
    assert!(back[0].0 >= cool + Duration::from_secs(118));
    assert!(
        back[1].0.saturating_sub(back[0].0) >= Duration::from_secs(120),
        "one block per pause: {back:?}"
    );
}

#[test]
fn no_return_to_the_cut_ceiling_while_the_die_is_as_hot_as_at_the_cut() {
    // «A return to that ceiling is allowed only while the current die level is lower than the recorded one.»
    let mut b = cut_once(3, 60.0);
    let cut = b.acts(Cause::Target)[0].1;
    assert!(
        cut.die.is_some_and(|d| (d - 60.0).abs() < 1e-9),
        "die = mean of the others' medians: {cut:?}"
    );
    b.run(600.0, |_| vec![hot(70.0), hot(60.0), hot(60.0)]);
    assert_eq!(b.acts(Cause::Return), [], "die 60 now, 60 at the cut: no return");
    b.run(600.0, |_| vec![hot(70.0), hot(65.0), hot(57.0)]);
    assert_eq!(b.acts(Cause::Return), [], "die 61 now: no return");
    let cooler = b.t;
    b.run(200.0, |_| vec![hot(70.0), hot(60.0), hot(59.0)]);
    let back = b.acts(Cause::Return);
    assert_eq!(back.len(), 1, "die 59.5 < 60: back to ×50");
    assert!(
        back[0].0 >= cooler && back[0].1.die.is_some_and(|d| d < 60.0),
        "{back:?}"
    );
}

#[test]
fn the_die_level_is_the_mean_of_the_other_blocks_two_second_medians() {
    let mut b = Bench::new(&[50, 50, 50]);
    b.run(4.0, |_| vec![hot(70.0), hot(60.0), hot(66.0)]);
    b.poll(&[hot(96.0), hot(60.0), hot(66.0)]);
    let cut = b.acts(Cause::Emergency)[0].1;
    assert!(cut.die.is_some_and(|d| (d - 63.0).abs() < 1e-9), "{cut:?}");
}
