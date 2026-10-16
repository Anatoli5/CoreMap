// Регулятор автомодуля (DECISIONS #31 «Auto module», #32, #38): по показаниям блоков решает, какой блок опустить на
// ступень и какой поднять. Окна, таймеров и вызовов Windows здесь нет: время и показания приходят снаружи, ответ —
// решение. Поэтому правило проверяется тестами на многих минутах «времени» за миллисекунды.
//
// Правило — как у регуляторов в Linux (thermald) и ACPI: снижать быстро, поднимать медленно и по одной ступени, с
// зазором между порогами (без зазора, как у ThrottleStop, — скачки туда-сюда; outputs/auto-control-research-2026-10-14.md).
// - Опустить: блок нагружен (доля работы ≥ 50 %), его средняя температура за окно (20 с) выше предела, после его
//   прошлого изменения прошла пауза (15 с). Новый потолок — на ступень ниже фактического множителя, а не потолка:
//   горячие ядра и так идут на 2–4 ступени ниже потолка (измерено), снижение потолка над ними ничего бы не дало. Ниже
//   нижней границы допустимого потолка (`Frame::floor`, та же, что у ручного потолка, machine::limits) — никогда;
//   выше неё — сколько нужно (владелец 15.10 убрал глубину: «функционально не очень понятно, для чего это сделано»).
//   За один шаг — один блок, самый горячий. Вместе с решением уходит ступень, на которой блок перегрелся
//   (`Decision::ban`): выше неё память перегрева (memory.rs) не пустит.
// - Поднять: блок пробыл прохладным (ниже «предел − 5 °C») под нагрузкой 2 минуты — простой не считается и не
//   сбрасывает счёт (DECISIONS #32: опущенный блок «остывал» в меню игры, поднимался и снова перегревался в игре).
//   Поднимается на одну ступень, до следующей — снова 2 минуты; ступень, на которую запрещает память, — нет.
//   До принятого потолка — всегда; выше принятого, до заводского — только если включено подтягивание и блок на деле
//   работает на своём потолке (иначе более высокий потолок ничего не меняет).
// - Между порогами — ничего. После любого изменения потолка окна всех блоков начинаются заново: прежние показания
//   были до него, а сосед греет соседа (связь 21–48 %, KNOWLEDGE.md) — следующее решение только по свежему окну.

use std::collections::VecDeque;
use std::time::Duration;

use crate::thermal::Reading;

/// Зазор между порогом снижения и порогом подъёма, °C: два шага датчика ядра (2 °C) и одна ступень потолка.
pub const MARGIN: f64 = 5.0;
/// С какой доли работы блок считается нагруженным: простаивающий блок греет сосед, а не он сам.
pub const BUSY: f64 = 0.5;
/// Блок «на своём потолке», если его средний фактический множитель не ниже потолка больше чем на столько.
pub const AT_CEILING: f64 = 0.5;
/// Больше этого между двумя показаниями «прохладен под нагрузкой» к счёту прохлады не прибавляется: был перерыв.
const COOL_GAP: Duration = Duration::from_secs(10);

/// Выбор владельца: предел температуры и можно ли подтягивать выше принятого.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub limit: f64,
    pub step_up: bool,
}

/// Сроки: окно усреднения, пауза после изменения, сколько держаться прохладным под нагрузкой перед подъёмом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub window: Duration,
    pub settle: Duration,
    pub release: Duration,
}

impl Timing {
    /// Настоящие сроки: кристалл выходит на ровную температуру за 5–10 с, вентиляторы раскручиваются за 4–15 с
    /// (измерено), окно и пауза — с запасом над этим.
    pub const REAL: Self = Self {
        window: Duration::from_secs(20),
        settle: Duration::from_secs(15),
        release: Duration::from_secs(120),
    };
}

/// Что сделать с блоком: поставить ему потолок `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Lower { block: usize, to: i32 },
    Raise { block: usize, to: i32 },
}

/// Окно блока на момент решения — для журнала действий.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub mean: f64,
    pub max: f64,
    pub busy: f64,
    /// Средний фактический множитель; None — не известен.
    pub ratio: Option<f64>,
}

/// Решение регулятора.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    pub action: Action,
    /// Окно блока, по которому решено; None — окна нет (подъём сразу после чужого действия).
    pub window: Option<Window>,
    /// Снижение: ступень, на которой блок перегрелся, — поднимать на неё и выше запрещает память перегрева.
    pub ban: Option<i32>,
    /// Подъём: сколько блок пробыл прохладным под нагрузкой.
    pub cool: Duration,
}

/// Один опрос: показания и потолки по блокам.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// None — блок в этом опросе не читался.
    pub readings: &'a [Option<Reading>],
    /// Действующий потолок блока.
    pub ceilings: &'a [i32],
    /// Принятый потолок блока: до него регулятор поднимает всегда, выше — только при подтягивании.
    pub accepted: &'a [i32],
    /// Заводской предел блока: выше никогда.
    pub fused: &'a [i32],
    /// Нижняя граница допустимого потолка блока (`Limits::floor` его первого ядра): ниже никогда.
    pub floor: &'a [i32],
}

#[derive(Debug, Clone, Copy)]
struct Point {
    at: Duration,
    temp: f64,
    busy: f64,
    ratio: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct Track {
    points: VecDeque<Point>,
    acted: Option<Duration>,
    /// Сколько блок пробыл прохладным под нагрузкой с последнего своего изменения или перегрева.
    cool: Duration,
    /// Когда было последнее показание «нагружен и прохладен»: следующее такое же прибавляет промежуток.
    cool_at: Option<Duration>,
}

impl Track {
    /// Окно заполнено: показания тянутся хотя бы на три четверти окна и их не меньше трёх.
    fn covered(&self, now: Duration, window: Duration) -> bool {
        self.points
            .front()
            .is_some_and(|first| self.points.len() >= 3 && now.saturating_sub(first.at) * 4 >= window * 3)
    }

    fn window(&self) -> Option<Window> {
        let count = f64::from(u32::try_from(self.points.len()).ok().filter(|&n| n > 0)?);
        let ratios: Vec<f64> = self.points.iter().filter_map(|p| p.ratio).collect();
        Some(Window {
            mean: self.points.iter().map(|p| p.temp).sum::<f64>() / count,
            max: self.points.iter().map(|p| p.temp).fold(f64::MIN, f64::max),
            busy: self.points.iter().map(|p| p.busy).sum::<f64>() / count,
            ratio: (!ratios.is_empty())
                .then(|| ratios.iter().sum::<f64>() / f64::from(u32::try_from(ratios.len()).unwrap_or(u32::MAX))),
        })
    }
}

/// Регулятор: память блоков и выбор владельца.
#[derive(Debug, Clone)]
pub struct Controller {
    settings: Settings,
    timing: Timing,
    tracks: Vec<Track>,
}

impl Controller {
    #[must_use]
    pub fn new(settings: Settings, timing: Timing, blocks: usize) -> Self {
        Self {
            settings,
            timing,
            tracks: vec![Track::default(); blocks],
        }
    }

    #[must_use]
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Нужно ли читать блоки в этом опросе по самой температуре: корпус близко к пределу (или не прочитан), или
    /// какой-то блок не на принятом — за ним надо следить, чтобы вернуть.
    #[must_use]
    pub fn watch_blocks(&self, package: Option<f64>, overlay: bool) -> bool {
        overlay || package.is_none_or(|p| p >= self.settings.limit - MARGIN)
    }

    /// Блоки не читались: прежние показания больше не окно подряд. Набранная прохлада под нагрузкой остаётся —
    /// перерыв её не сбрасывает, следующая прохлада прибавляется к ней.
    pub fn forget(&mut self) {
        for track in &mut self.tracks {
            track.points.clear();
            track.cool_at = None;
        }
    }

    /// Потолок блока изменился не регулятором (принят чужой, поставлен профиль): память блока начинается заново.
    pub fn forget_block(&mut self, block: usize) {
        if let Some(track) = self.tracks.get_mut(block) {
            *track = Track::default();
        }
    }

    /// Один опрос. `banned(блок, потолок)` — запрещает ли память перегрева поднять блок на этот потолок. Возвращает
    /// не больше одного решения.
    pub fn step(&mut self, now: Duration, frame: &Frame<'_>, banned: &dyn Fn(usize, i32) -> bool) -> Option<Decision> {
        self.observe(now, frame.readings);
        if let Some(decision) = self.lower(now, frame) {
            return Some(decision);
        }
        self.raise(now, frame, banned)
    }

    fn observe(&mut self, now: Duration, readings: &[Option<Reading>]) {
        let Settings { limit, .. } = self.settings;
        let window = self.timing.window;
        for (track, reading) in self.tracks.iter_mut().zip(readings) {
            let Some(Reading {
                temp: Some(temp),
                busy,
                ratio,
                ..
            }) = *reading
            else {
                track.cool_at = None;
                continue;
            };
            let loaded = busy.is_some_and(|b| b >= BUSY);
            if loaded && temp < limit - MARGIN {
                if let Some(at) = track.cool_at {
                    track.cool += now.saturating_sub(at).min(COOL_GAP);
                }
                track.cool_at = Some(now);
            } else if loaded {
                // Нагружен и не прохладен: прохлада не доказана — счёт заново.
                track.cool = Duration::ZERO;
                track.cool_at = None;
            } else {
                // Простой: часы прохлады стоят — не идут и не сбрасываются.
                track.cool_at = None;
            }
            // Без доли работы (первый опрос после перерыва) точка в окно не идёт: ноль занизил бы среднюю (ревизия L3).
            let Some(busy) = busy else { continue };
            track.points.push_back(Point {
                at: now,
                temp,
                busy,
                ratio,
            });
            while track.points.front().is_some_and(|p| now.saturating_sub(p.at) > window) {
                track.points.pop_front();
            }
        }
    }

    fn settled(&self, track: &Track, now: Duration) -> bool {
        track.acted.is_none_or(|t| now.saturating_sub(t) >= self.timing.settle)
    }

    /// Опустить: самый горячий из нагруженных и перегретых за всё окно — на ступень ниже фактического множителя, не
    /// ниже нижней границы допустимого потолка.
    fn lower(&mut self, now: Duration, frame: &Frame<'_>) -> Option<Decision> {
        let limit = self.settings.limit;
        let mut hottest: Option<(usize, Window, i32, i32)> = None; // блок, окно, куда, ступень перегрева
        for (block, track) in self.tracks.iter().enumerate() {
            let (Some(&ceiling), Some(&floor)) = (frame.ceilings.get(block), frame.floor.get(block)) else {
                continue;
            };
            if !track.covered(now, self.timing.window) || !self.settled(track, now) {
                continue;
            }
            let Some(window) = track.window() else { continue };
            if window.mean <= limit || window.busy < BUSY || ceiling <= floor {
                continue;
            }
            // Множитель — целые ступени; дробный средний округляется вниз, а ступень перегрева — вверх: блок, шедший
            // в среднем на ×49,9, работал и на ×50.
            #[expect(clippy::cast_possible_truncation, reason = "a multiplier is a small positive number")]
            let (actual, ran) = window.ratio.map_or((ceiling, ceiling), |r| {
                ((r.floor() as i32).min(ceiling), (r.ceil() as i32).min(ceiling))
            });
            let to = (actual - 1).min(ceiling - 1).max(floor);
            if hottest.is_none_or(|(_, w, ..)| window.mean > w.mean) {
                hottest = Some((block, window, to, ran.max(to + 1)));
            }
        }
        let (block, window, to, ran) = hottest?;
        self.after_action();
        let track = &mut self.tracks[block];
        track.acted = Some(now);
        track.cool = Duration::ZERO;
        track.cool_at = None;
        Some(Decision {
            action: Action::Lower { block, to },
            window: Some(window),
            ban: Some(ran),
            cool: Duration::ZERO,
        })
    }

    /// Поднять: из прохладных под нагрузкой дольше всех прохладный, если память не запрещает.
    fn raise(&mut self, now: Duration, frame: &Frame<'_>, banned: &dyn Fn(usize, i32) -> bool) -> Option<Decision> {
        let mut coolest: Option<(usize, Duration)> = None;
        for (block, track) in self.tracks.iter().enumerate() {
            let (Some(&ceiling), Some(&base), Some(&fused)) = (
                frame.ceilings.get(block),
                frame.accepted.get(block),
                frame.fused.get(block),
            ) else {
                continue;
            };
            if track.cool < self.timing.release || !self.settled(track, now) || ceiling >= fused {
                continue;
            }
            let wanted = ceiling < base || (self.settings.step_up && self.at_ceiling(track, now, ceiling));
            if !wanted || banned(block, ceiling + 1) {
                continue;
            }
            if coolest.is_none_or(|(_, c)| track.cool > c) {
                coolest = Some((block, track.cool));
            }
        }
        let (block, cool) = coolest?;
        let to = frame.ceilings[block] + 1;
        let window = self.tracks[block].window();
        self.after_action();
        let track = &mut self.tracks[block];
        track.acted = Some(now);
        track.cool = Duration::ZERO; // до следующей ступени — снова полный срок прохлады
        track.cool_at = None;
        Some(Decision {
            action: Action::Raise { block, to },
            window,
            ban: None,
            cool,
        })
    }

    /// Блок нагружен и на деле работает на своём потолке — только тогда выше потолка есть смысл.
    fn at_ceiling(&self, track: &Track, now: Duration, ceiling: i32) -> bool {
        track.covered(now, self.timing.window)
            && track
                .window()
                .is_some_and(|w| w.busy >= BUSY && w.ratio.is_some_and(|r| r >= f64::from(ceiling) - AT_CEILING))
    }

    /// После любого изменения потолка — свежие окна у всех блоков (их показания были до него).
    fn after_action(&mut self) {
        for track in &mut self.tracks {
            track.points.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: f64 = 85.0;
    const FUSED: i32 = 54;
    /// Нижняя граница допустимого потолка P-ядра 275HX: базовый множитель ×27 (`machine::limits`, DECISIONS #34).
    const FLOOR: i32 = 27;

    fn controller(blocks: usize) -> Controller {
        with(blocks, false)
    }

    fn with(blocks: usize, step_up: bool) -> Controller {
        Controller::new(Settings { limit: LIMIT, step_up }, Timing::REAL, blocks)
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "readings are Option per block; the helper builds one in place"
    )]
    fn hot(temp: f64, busy: f64, ratio: f64) -> Option<Reading> {
        Some(Reading {
            temp: Some(temp),
            busy: Some(busy),
            ratio: Some(ratio),
            volts: None,
        })
    }

    /// Прокручивает `seconds` секунд опросами раз в 2 с с одними показаниями; возвращает решения по порядку.
    fn run_with(
        c: &mut Controller,
        from: u64,
        seconds: u64,
        readings: &[Option<Reading>],
        ceilings: &mut [i32],
        accepted: &[i32],
        banned: &dyn Fn(usize, i32) -> bool,
    ) -> Vec<(u64, Decision)> {
        let fused = vec![FUSED; ceilings.len()];
        let floor = vec![FLOOR; ceilings.len()];
        let mut decisions = Vec::new();
        for t in (from..from + seconds).step_by(2) {
            let frame = Frame {
                readings,
                ceilings,
                accepted,
                fused: &fused,
                floor: &floor,
            };
            if let Some(decision) = c.step(Duration::from_secs(t), &frame, banned) {
                match decision.action {
                    Action::Lower { block, to } | Action::Raise { block, to } => ceilings[block] = to,
                }
                decisions.push((t, decision));
            }
        }
        decisions
    }

    /// То же без запретов памяти; возвращает действия.
    fn run(
        c: &mut Controller,
        from: u64,
        seconds: u64,
        readings: &[Option<Reading>],
        ceilings: &mut [i32],
        accepted: &[i32],
    ) -> Vec<(u64, Action)> {
        run_with(c, from, seconds, readings, ceilings, accepted, &|_, _| false)
            .into_iter()
            .map(|(t, d)| (t, d.action))
            .collect()
    }

    #[test]
    fn a_hot_busy_block_is_lowered_once_the_window_is_full_not_before() {
        let mut c = controller(1);
        let mut ceilings = [50];
        let early = run(&mut c, 0, 14, &[hot(90.0, 1.0, 50.0)], &mut ceilings, &[50]);
        assert!(
            early.is_empty(),
            "no action before three quarters of the 20 s window: {early:?}"
        );
        let later = run(&mut c, 14, 4, &[hot(90.0, 1.0, 50.0)], &mut ceilings, &[50]);
        assert_eq!(later.len(), 1, "{later:?}");
        assert_eq!(later[0].1, Action::Lower { block: 0, to: 49 });
    }

    #[test]
    fn an_idle_block_heated_by_a_neighbour_is_not_lowered() {
        let mut c = controller(1);
        let mut ceilings = [50];
        assert_eq!(run(&mut c, 0, 60, &[hot(90.0, 0.2, 50.0)], &mut ceilings, &[50]), []);
    }

    #[test]
    fn the_step_is_below_the_actual_multiplier_not_below_the_ceiling() {
        // Hot cores run 2-4 steps under their ceiling (measured): ceiling x50, actual x47.6 -> x46.
        let mut c = controller(1);
        let mut ceilings = [50];
        let actions = run(&mut c, 0, 20, &[hot(90.0, 1.0, 47.6)], &mut ceilings, &[50]);
        assert_eq!(actions[0].1, Action::Lower { block: 0, to: 46 });
    }

    #[test]
    fn a_block_that_stays_hot_goes_down_to_the_range_bottom_and_not_below() {
        // The owner removed the depth (15.10): a block that stays hot is lowered as far as needed - past the old
        // "4 steps below where it ran" (x36 for a block that ran x40) - down to the bottom of the allowed range, as a
        // manual ceiling, and never below it.
        let mut c = controller(1);
        let mut ceilings = [50];
        let actions = run(&mut c, 0, 1800, &[hot(99.0, 1.0, 40.0)], &mut ceilings, &[50]);
        assert_eq!(ceilings[0], FLOOR, "{actions:?}");
        assert!(ceilings[0] < 36, "deeper than the old depth limit");
        assert!(
            actions
                .iter()
                .all(|(_, a)| matches!(a, Action::Lower { to, .. } if *to >= FLOOR)),
            "{actions:?}"
        );
        let more = run(
            &mut c,
            1800,
            600,
            &[hot(99.0, 1.0, f64::from(FLOOR))],
            &mut ceilings,
            &[50],
        );
        assert_eq!(more, [], "at the range bottom nothing more happens");
    }

    #[test]
    fn a_step_that_would_cross_the_range_bottom_stops_at_it() {
        // Ceiling x29, the block throttles to x20 under it: one step below the actual multiplier would be x19 - below
        // the bottom (x27) - so the step is to x27 exactly.
        let mut c = controller(1);
        let mut ceilings = [29];
        let actions = run(&mut c, 0, 20, &[hot(95.0, 1.0, 20.0)], &mut ceilings, &[50]);
        let lowered: Vec<Action> = actions.iter().map(|&(_, a)| a).collect();
        assert_eq!(lowered, [Action::Lower { block: 0, to: FLOOR }]);
    }

    #[test]
    fn a_hot_core_running_under_its_ceiling_steps_from_where_it_runs() {
        // Measured on the 275HX: core 1 at ceiling x53 ran x49.87 - the first step is to x48, not to x52.
        let mut c = controller(1);
        let mut ceilings = [53];
        let actions = run(&mut c, 0, 20, &[hot(95.0, 1.0, 49.87)], &mut ceilings, &[53]);
        assert_eq!(
            actions.first().map(|a| a.1),
            Some(Action::Lower { block: 0, to: 48 }),
            "the first step bites"
        );
    }

    #[test]
    fn after_any_step_every_block_waits_for_a_fresh_window() {
        // A neighbour heats a neighbour: the second block is judged only on readings taken after the first step.
        let mut c = controller(2);
        let mut ceilings = [50, 50];
        let actions = run(
            &mut c,
            0,
            60,
            &[hot(93.0, 1.0, 50.0), hot(90.0, 1.0, 50.0)],
            &mut ceilings,
            &[50, 50],
        );
        let times: Vec<u64> = actions.iter().map(|(t, _)| *t).collect();
        assert!(times.len() >= 2, "{actions:?}");
        assert!(
            times.windows(2).all(|w| w[1] - w[0] >= 14),
            "no step on a window older than the last step: {times:?}"
        );
    }

    #[test]
    fn the_hottest_block_goes_first_and_one_action_per_poll() {
        let mut c = controller(2);
        let mut ceilings = [50, 50];
        let readings = [hot(88.0, 1.0, 50.0), hot(93.0, 1.0, 50.0)];
        let actions = run(&mut c, 0, 18, &readings, &mut ceilings, &[50, 50]);
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].1, Action::Lower { block: 1, to: 49 });
    }

    #[test]
    fn after_a_step_the_block_waits_a_full_window_again() {
        let mut c = controller(1);
        let mut ceilings = [50];
        let actions = run(&mut c, 0, 40, &[hot(90.0, 1.0, 50.0)], &mut ceilings, &[50]);
        let times: Vec<u64> = actions.iter().map(|(t, _)| *t).collect();
        assert!(
            times.windows(2).all(|w| w[1] - w[0] >= 15),
            "steps spaced by the window and the pause: {times:?}"
        );
    }

    #[test]
    fn between_the_thresholds_nothing_happens() {
        let mut c = controller(1);
        let mut ceilings = [48];
        // 82 °C: not above the 85 °C limit, not below 80 °C - no lowering, no return.
        assert_eq!(run(&mut c, 0, 600, &[hot(82.0, 1.0, 48.0)], &mut ceilings, &[50]), []);
    }

    #[test]
    fn a_cool_block_returns_one_step_per_two_minutes_up_to_the_accepted_ceiling() {
        let mut c = controller(1);
        let mut ceilings = [47];
        let raises = run(&mut c, 0, 900, &[hot(70.0, 1.0, 47.0)], &mut ceilings, &[50]);
        assert_eq!(
            ceilings[0], 50,
            "back to the accepted ceiling and not above: {raises:?}"
        );
        assert_eq!(raises.len(), 3);
        assert!(
            raises.windows(2).all(|w| w[1].0 - w[0].0 >= 120),
            "two minutes of cool between steps: {raises:?}"
        );
        assert!(raises[0].0 >= 120, "the first step only after two minutes of cool");
    }

    #[test]
    fn an_idle_block_is_not_raised_however_long_it_idles() {
        // DECISIONS #32: a lowered block cooled in the game's menu, went up and overheated again in play.
        let mut c = controller(1);
        let mut ceilings = [47];
        assert_eq!(run(&mut c, 0, 1800, &[hot(55.0, 0.05, 47.0)], &mut ceilings, &[50]), []);
    }

    #[test]
    fn idle_pauses_the_cool_clock_and_does_not_reset_it() {
        // 60 s cool under load, 10 min menu, 60 s cool under load: two minutes of cool in all - the step comes then.
        let mut c = controller(1);
        let mut ceilings = [47];
        let cool = [hot(70.0, 1.0, 47.0)];
        assert_eq!(run(&mut c, 0, 62, &cool, &mut ceilings, &[50]), []);
        assert_eq!(run(&mut c, 62, 600, &[hot(55.0, 0.05, 47.0)], &mut ceilings, &[50]), []);
        let back = run(&mut c, 662, 70, &cool, &mut ceilings, &[50]);
        assert_eq!(back.len(), 1, "{back:?}");
        assert!(
            back[0].0 >= 662 + 58,
            "not before the second minute is made up: {back:?}"
        );
    }

    #[test]
    fn heat_under_load_starts_the_cool_count_afresh() {
        let mut c = controller(1);
        let mut ceilings = [47];
        assert_eq!(run(&mut c, 0, 100, &[hot(70.0, 1.0, 47.0)], &mut ceilings, &[50]), []);
        assert_eq!(run(&mut c, 100, 4, &[hot(82.0, 1.0, 47.0)], &mut ceilings, &[50]), []);
        assert_eq!(
            run(&mut c, 104, 100, &[hot(70.0, 1.0, 47.0)], &mut ceilings, &[50]),
            [],
            "100 s + 100 s of cool split by heat are not two minutes"
        );
    }

    #[test]
    fn a_banned_step_is_not_raised_to_but_the_one_below_is() {
        let mut c = controller(1);
        let mut ceilings = [47];
        let decisions = run_with(
            &mut c,
            0,
            1200,
            &[hot(70.0, 1.0, 47.0)],
            &mut ceilings,
            &[50],
            &|_, to| to >= 49,
        );
        assert_eq!(ceilings[0], 48, "{decisions:?}");
    }

    #[test]
    fn a_lowering_names_the_step_it_overheated_at_and_its_window() {
        // Ceiling x53, ran x49.87 - it ran at x50 too: the ban starts at x50, the new ceiling is x48.
        let mut c = controller(1);
        let mut ceilings = [53];
        let decisions = run_with(
            &mut c,
            0,
            20,
            &[hot(91.0, 0.9, 49.87)],
            &mut ceilings,
            &[53],
            &|_, _| false,
        );
        let d = decisions[0].1;
        assert_eq!(d.action, Action::Lower { block: 0, to: 48 });
        assert_eq!(d.ban, Some(50));
        let w = d.window.expect("a lowering is decided on a window");
        assert!((w.mean - 91.0).abs() < 1e-9 && (w.max - 91.0).abs() < 1e-9 && (w.busy - 0.9).abs() < 1e-9);
        assert!(w.ratio.is_some_and(|r| (r - 49.87).abs() < 1e-9));
    }

    #[test]
    fn without_step_up_a_cool_block_stays_at_the_accepted_ceiling() {
        let mut c = controller(1);
        let mut ceilings = [50];
        assert_eq!(run(&mut c, 0, 900, &[hot(65.0, 1.0, 50.0)], &mut ceilings, &[50]), []);
    }

    #[test]
    fn step_up_raises_a_cool_block_running_at_its_ceiling_up_to_fused() {
        let mut c = with(1, true);
        let mut ceilings = [50];
        // The simulated block always runs at its ceiling.
        let mut t = 0;
        while t < 3000 && ceilings[0] < FUSED {
            let r = [hot(65.0, 1.0, f64::from(ceilings[0]))];
            run(&mut c, t, 2, &r, &mut ceilings, &[50]);
            t += 2;
        }
        assert_eq!(ceilings[0], FUSED, "up to fused");
        let more = run(
            &mut c,
            t,
            900,
            &[hot(65.0, 1.0, f64::from(FUSED))],
            &mut ceilings,
            &[50],
        );
        assert_eq!(more, [], "never above fused");
    }

    #[test]
    fn step_up_leaves_a_block_that_does_not_reach_its_ceiling() {
        // Cool, but runs x48.6 under a x50 ceiling: a higher ceiling would change nothing.
        let mut c = with(1, true);
        let mut ceilings = [50];
        assert_eq!(run(&mut c, 0, 900, &[hot(65.0, 1.0, 48.6)], &mut ceilings, &[50]), []);
    }

    #[test]
    fn a_block_not_read_is_not_judged() {
        let mut c = controller(1);
        let mut ceilings = [50];
        assert_eq!(run(&mut c, 0, 60, &[None], &mut ceilings, &[50]), []);
    }

    #[test]
    fn blocks_are_watched_near_the_limit_or_while_one_is_off_its_accepted_ceiling() {
        let c = controller(1);
        assert!(!c.watch_blocks(Some(70.0), false));
        assert!(c.watch_blocks(Some(81.0), false));
        assert!(c.watch_blocks(Some(60.0), true));
        assert!(c.watch_blocks(None, false), "no package reading - read the blocks");
    }
}
