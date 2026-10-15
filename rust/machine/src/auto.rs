// Регулятор автомодуля (DECISIONS #31, «Auto module»): по показаниям блоков решает, какой блок опустить на ступень и
// какой вернуть. Окна, таймеров и вызовов Windows здесь нет: время и показания приходят снаружи, ответ — действие.
// Поэтому правило проверяется тестами на многих минутах «времени» за миллисекунды.
//
// Правило — как у регуляторов в Linux (thermald) и ACPI: снижать быстро, возвращать медленно и по одной ступени, с
// зазором между порогами (без зазора, как у ThrottleStop, — скачки туда-сюда; outputs/auto-control-research-2026-10-14.md).
// - Опустить: блок нагружен (доля работы ≥ 50 %), его средняя температура за окно (20 с) выше предела, после его
//   прошлого изменения прошла пауза (15 с). Новый потолок — на ступень ниже фактического множителя, а не потолка:
//   горячие ядра и так идут на 2–4 ступени ниже потолка (измерено), снижение потолка над ними ничего бы не дало. Не
//   глубже «принятый минус глубина». За один шаг — один блок, самый горячий.
// - Вернуть: блок ниже принятого, и его температура держится ниже «предел − 5 °C» уже 2 минуты. На одну ступень; до
//   следующей — снова 2 минуты прохлады. Выше принятого — никогда.
// - Между порогами — ничего. После любого изменения потолка память всех блоков начинается заново: прежние показания
//   были до него, а сосед греет соседа (связь 21–48 %, KNOWLEDGE.md) — следующее решение только по свежему окну.
// - Глубина считается от того, где блок работал при первом снижении (фактический множитель), а не от принятого
//   потолка: горячие ядра и так идут на 3–4 ступени ниже потолка, и «принятый − 4» оставлял регулятору одну
//   ступень или ни одной (ревизия, outputs/review-auto-2026-10-14.md H2).

use std::collections::VecDeque;
use std::time::Duration;

use crate::thermal::Reading;

/// Зазор между порогом снижения и порогом возврата, °C: два шага датчика ядра (2 °C) и одна ступень потолка.
pub const MARGIN: f64 = 5.0;
/// С какой доли работы блок считается нагруженным: простаивающий блок греет сосед, а не он сам.
pub const BUSY: f64 = 0.5;

/// Выбор владельца: предел температуры и насколько глубоко можно опускать ниже принятых потолков.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub limit: f64,
    pub depth: i32,
}

/// Сроки: окно усреднения, пауза после изменения, сколько держаться прохладным перед возвратом ступени.
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
    cool_since: Option<Duration>,
    /// Где блок работал при первом снижении (целый фактический множитель, не выше принятого): от него считается
    /// глубина, пока блок не вернётся к принятому.
    anchor: Option<i32>,
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

    /// Нужно ли читать блоки в этом опросе: корпус близко к пределу (или не прочитан), или какой-то блок опущен —
    /// за ним надо следить, чтобы вернуть.
    #[must_use]
    pub fn watch_blocks(&self, package: Option<f64>, lowered: bool) -> bool {
        lowered || package.is_none_or(|p| p >= self.settings.limit - MARGIN)
    }

    /// Блоки не читались (корпус далеко от предела): прежние показания больше не окно подряд.
    pub fn forget(&mut self) {
        for track in &mut self.tracks {
            track.points.clear();
            track.cool_since = None;
        }
    }

    /// Потолок блока изменился не регулятором (принят чужой, поставлен профиль): память блока начинается заново.
    pub fn forget_block(&mut self, block: usize) {
        if let Some(track) = self.tracks.get_mut(block) {
            *track = Track::default();
        }
    }

    /// Один опрос. `readings` — по блокам (None — блок в этом опросе не читался), `ceilings` — действующий потолок
    /// блока, `accepted` — принятый (выше него регулятор не поднимает). Возвращает не больше одного действия.
    pub fn step(
        &mut self,
        now: Duration,
        readings: &[Option<Reading>],
        ceilings: &[i32],
        accepted: &[i32],
    ) -> Option<Action> {
        let Settings { limit, depth } = self.settings;
        let window = self.timing.window;
        for (track, reading) in self.tracks.iter_mut().zip(readings) {
            let Some(Reading {
                temp: Some(temp),
                busy,
                ratio,
            }) = *reading
            else {
                continue;
            };
            if temp < limit - MARGIN {
                track.cool_since.get_or_insert(now);
            } else {
                track.cool_since = None;
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
        let settled = |track: &Track| track.acted.is_none_or(|t| now.saturating_sub(t) >= self.timing.settle);

        // Опустить: самый горячий из нагруженных и перегретых за всё окно.
        let mut hottest: Option<(usize, f64, i32, i32)> = None; // блок, средняя, куда, якорь
        for (block, track) in self.tracks.iter().enumerate() {
            let (Some(&ceiling), Some(&base)) = (ceilings.get(block), accepted.get(block)) else {
                continue;
            };
            let Some(first) = track.points.front() else { continue };
            // Окно заполнено: показания тянутся хотя бы на три четверти окна и их не меньше трёх.
            let covered = track.points.len() >= 3 && now.saturating_sub(first.at) * 4 >= window * 3;
            if !covered || !settled(track) {
                continue;
            }
            let count = f64::from(u32::try_from(track.points.len()).unwrap_or(u32::MAX));
            let mean_temp = track.points.iter().map(|p| p.temp).sum::<f64>() / count;
            let mean_busy = track.points.iter().map(|p| p.busy).sum::<f64>() / count;
            if mean_temp <= limit || mean_busy < BUSY {
                continue;
            }
            let ratios: Vec<f64> = track.points.iter().filter_map(|p| p.ratio).collect();
            let actual = if ratios.is_empty() {
                ceiling
            } else {
                let mean = ratios.iter().sum::<f64>() / f64::from(u32::try_from(ratios.len()).unwrap_or(u32::MAX));
                // Множитель — целые ступени; дробный средний округляется вниз.
                #[expect(clippy::cast_possible_truncation, reason = "a multiplier is a small positive number")]
                let whole = mean.floor() as i32;
                whole.min(ceiling)
            };
            let anchor = track.anchor.unwrap_or_else(|| actual.min(base));
            let floor = anchor - depth;
            if ceiling <= floor {
                continue;
            }
            let to = (actual - 1).min(ceiling - 1).max(floor);
            if hottest.is_none_or(|(_, t, _, _)| mean_temp > t) {
                hottest = Some((block, mean_temp, to, anchor));
            }
        }
        if let Some((block, _, to, anchor)) = hottest {
            self.after_action();
            let track = &mut self.tracks[block];
            track.acted = Some(now);
            track.cool_since = None;
            track.anchor = Some(anchor);
            return Some(Action::Lower { block, to });
        }

        // Вернуть: дольше всех прохладный из опущенных.
        let mut coolest: Option<(usize, Duration)> = None;
        for (block, track) in self.tracks.iter().enumerate() {
            let (Some(&ceiling), Some(&base)) = (ceilings.get(block), accepted.get(block)) else {
                continue;
            };
            let Some(since) = track.cool_since else { continue };
            if ceiling >= base || !settled(track) || now.saturating_sub(since) < self.timing.release {
                continue;
            }
            if coolest.is_none_or(|(_, s)| since < s) {
                coolest = Some((block, since));
            }
        }
        let (block, _) = coolest?;
        let to = ceilings[block] + 1;
        self.after_action();
        let track = &mut self.tracks[block];
        track.acted = Some(now);
        track.cool_since = Some(now); // до следующей ступени — снова полный срок прохлады
        if accepted.get(block).is_some_and(|&base| to >= base) {
            track.anchor = None; // вернулся к принятому: следующее снижение считает глубину заново
        }
        Some(Action::Raise { block, to })
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

    fn controller(blocks: usize) -> Controller {
        Controller::new(Settings { limit: LIMIT, depth: 4 }, Timing::REAL, blocks)
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
        })
    }

    /// Прокручивает `seconds` секунд опросами раз в 2 с с одними показаниями; возвращает действия по порядку.
    fn run(
        c: &mut Controller,
        from: u64,
        seconds: u64,
        readings: &[Option<Reading>],
        ceilings: &mut [i32],
        accepted: &[i32],
    ) -> Vec<(u64, Action)> {
        let mut actions = Vec::new();
        for t in (from..from + seconds).step_by(2) {
            if let Some(action) = c.step(Duration::from_secs(t), readings, ceilings, accepted) {
                match action {
                    Action::Lower { block, to } | Action::Raise { block, to } => ceilings[block] = to,
                }
                actions.push((t, action));
            }
        }
        actions
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
    fn never_deeper_than_depth_below_where_it_ran_at_the_first_step() {
        // Accepted x50, the block ran at x40 (far under the ceiling): depth 4 counts from x40, not from x50.
        let mut c = controller(1);
        let mut ceilings = [50];
        let actions = run(&mut c, 0, 900, &[hot(99.0, 1.0, 40.0)], &mut ceilings, &[50]);
        assert_eq!(ceilings[0], 36, "{actions:?}");
        assert!(
            actions
                .iter()
                .all(|(_, a)| matches!(a, Action::Lower { to, .. } if *to >= 36))
        );
    }

    #[test]
    fn a_hot_core_running_under_its_ceiling_still_has_the_whole_depth() {
        // Measured on the 275HX: core 1 at ceiling x53 ran x49.87. Accepted x53, depth 4 -> down to x45, not x49.
        let mut c = controller(1);
        let mut ceilings = [53];
        let actions = run(&mut c, 0, 900, &[hot(95.0, 1.0, 49.87)], &mut ceilings, &[53]);
        assert_eq!(
            actions.first().map(|a| a.1),
            Some(Action::Lower { block: 0, to: 48 }),
            "the first step bites"
        );
        assert_eq!(ceilings[0], 45, "{actions:?}");
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
        let actions = run(&mut c, 0, 900, &[hot(70.0, 1.0, 47.0)], &mut ceilings, &[50]);
        let raises: Vec<(u64, Action)> = actions.into_iter().collect();
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
    fn a_block_not_read_is_not_judged() {
        let mut c = controller(1);
        let mut ceilings = [50];
        assert_eq!(run(&mut c, 0, 60, &[None], &mut ceilings, &[50]), []);
    }

    #[test]
    fn blocks_are_watched_near_the_limit_or_while_one_is_lowered() {
        let c = controller(1);
        assert!(!c.watch_blocks(Some(70.0), false));
        assert!(c.watch_blocks(Some(81.0), false));
        assert!(c.watch_blocks(Some(60.0), true));
        assert!(c.watch_blocks(None, false), "no package reading - read the blocks");
    }
}
