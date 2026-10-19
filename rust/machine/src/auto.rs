// Регулятор двух отсечек и возврата к профилю. Время, показания и полная конфигурация приходят снаружи;
// ответ — решения, без вызовов Windows и самостоятельных часов.
// Аварийная отсечка использует одно показание и отдельную секундную перепроверку каждого блока.
// Целевая использует арифметическое среднее полного окна, без проверки тренда. После любой смены конфигурации
// обычные решения ждут установления, затем нового полного окна. Аварийная отсечка действует и во время установления.
// Возврат требует температуры не выше target − gap и непрерывной работы, сопоставимой с отсечкой по собственной,
// общей и относительной активности, с фактическим множителем у потолка. После +1 новая проверка; без неё — откат.
// Одновременно проверяется одна ступень одного блока. Память полных конфигураций проверяется владельцем callback
// `allowed`; уровень остальных блоков остаётся диагностическим полем, не самостоятельным правилом регулятора.

use std::time::Duration;

use crate::returning::Guard;
pub use crate::returning::{LoadFrame, Protection, Trial, Workload};
use crate::thermal::Reading;

/// Допуск пробной ступени по блоку, новому потолку, уровню окружения и фактической сводке текущего окна.
pub type PhysicalPermit<'a> = dyn Fn(usize, i32, Option<f64>, &[Reading]) -> bool + 'a;

/// Выбор владельца: целевой предел и аварийный, °C.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub limit: f64,
    pub emergency: f64,
    /// Возврат допустим при среднем не выше `limit` − `return_gap`.
    pub return_gap: f64,
}

/// Сроки регулятора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Окно среднего целевой отсечки.
    pub window: Duration,
    /// Через сколько после аварийной ступени аварийная проверка блока возобновляется.
    pub recheck: Duration,
    /// Пауза установления после изменения полной конфигурации потолков.
    pub settle: Duration,
    /// Длительность непрерывной прохладной работы до пробы и отдельного доказательства после неё.
    pub pause: Duration,
}

impl Timing {
    /// Начальные ручные настройки. Это параметры, выбранные для начала, не измеренные постоянные ноутбука.
    pub const REAL: Self = Self {
        window: Duration::from_secs(2),
        recheck: Duration::from_secs(1),
        settle: Duration::from_secs(2),
        pause: Duration::from_mins(2),
    };
}

/// Какое правило решило.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Аварийная отсечка: одно показание не ниже аварийного предела.
    Emergency,
    /// Целевая: среднее полного окна выше предела.
    Target,
    /// Возврат на ступень к профилю.
    Return,
    /// Проверка повышенного потолка не состоялась: вернуть сохранённое ограничение.
    Revert,
}

impl Cause {
    /// Как действие пишется в журнал действий (data\auto-log.jsonl, поле act); окно читает эти слова.
    #[must_use]
    pub fn act(self) -> &'static str {
        match self {
            Self::Emergency => "emergency",
            Self::Target => "target",
            Self::Return => "return",
            Self::Revert => "return-revert",
        }
    }
}

/// Решение: поставить блоку `block` потолок `to` вместо `from`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    pub cause: Cause,
    pub block: usize,
    pub from: i32,
    pub to: i32,
    /// Температура, по которой решено, °C: показание (аварийная) или среднее окна (целевая, возврат).
    /// При откате непроверенной ступени может быть NaN: температура неизвестна, причина отката — отсутствие проверки.
    pub temp: f64,
    /// Уровень кристалла: среднее последних окон остальных блоков; None — ни одного нет.
    pub die: Option<f64>,
    /// Фактический множитель и доля работы блока: у аварийной — за показание, у целевой и возврата — за окно.
    pub ratio: Option<f64>,
    pub busy: Option<f64>,
}

/// Один опрос: показания и потолки по блокам.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// None — блок в этом опросе не судится (не прочитан, отставлен после чужой правки).
    pub readings: &'a [Option<Reading>],
    /// Действующий потолок блока.
    pub ceilings: &'a [i32],
    /// Потолок профиля: выше него регулятор не возвращает.
    pub accepted: &'a [i32],
    /// Нижняя граница допустимого потолка блока (`Limits::floor` его первого ядра): ниже никогда.
    pub floor: &'a [i32],
}

#[derive(Debug, Clone, Default)]
struct Track {
    guard: Guard,
    workload: Vec<Option<Workload>>,
    /// Показания текущего окна; буфер переиспользуется.
    window: Vec<Reading>,
    /// Среднее последнего закрытого окна; None — окна не было или показаний меньше половины опросов.
    mean: Option<f64>,
    /// Фактический множитель и доля работы за последнее закрытое окно.
    ratio: Option<f64>,
    busy: Option<f64>,
    /// До этого времени аварийная проверка блока стоит (после его аварийной ступени).
    recheck_at: Option<Duration>,
    /// Последнее изменение потолка блока регулятором.
    acted: Option<Duration>,
    /// С какого времени средние блока не выше порога возврата; None — последняя выше, её нет или блок только что
    /// изменён.
    cool_since: Option<Duration>,
}

impl Track {
    fn act(&mut self, now: Duration) {
        self.acted = Some(now);
        self.cool_since = None;
    }
}

/// Регулятор: окна блоков, средние, сроки, выбор владельца.
#[derive(Debug, Clone)]
pub struct Controller {
    settings: Settings,
    timing: Timing,
    tracks: Vec<Track>,
    window_readings: Vec<Reading>,
    /// Начало текущего окна (общего для всех блоков); None — показаний с последнего «забыть» не было.
    start: Option<Duration>,
    last_window: Option<(Duration, Duration)>,
    /// Опросов в текущем окне: среднее блока известно, если показаний не меньше половины.
    polls: usize,
    /// Последний возврат любого блока: следующий — не раньше паузы («один блок за паузу»).
    returned: Option<Duration>,
    /// Блоки, изменённые в этом опросе: второго решения тому же блоку в том же опросе нет — первое ещё не дошло.
    touched: Vec<bool>,
    last_poll: Option<Duration>,
    ceilings_seen: Vec<i32>,
    accepted_seen: Vec<i32>,
    settle_until: Option<Duration>,
}

/// Ступень вниз: на одну ниже фактического множителя (не известен — ниже потолка), не ниже `floor`. None — блок уже
/// на нижней границе.
#[must_use]
pub fn below(ratio: Option<f64>, ceiling: i32, floor: i32) -> Option<i32> {
    if ceiling <= floor {
        return None;
    }
    // Множитель — целые ступени; дробный средний округляется вниз: блок, шедший в среднем на ×49,9, бывал и на ×49.
    #[expect(clippy::cast_possible_truncation, reason = "a multiplier is a small positive number")]
    let actual = ratio.map_or(ceiling, |r| (r.floor() as i32).min(ceiling));
    Some((actual - 1).min(ceiling - 1).max(floor))
}

/// Доля работы (среднее) и фактический множитель (среднее по времени работы) за показания окна. Показания идут ровным
/// шагом опроса, поэтому каждое весит одинаково.
fn means(window: &[Reading]) -> (Option<f64>, Option<f64>) {
    let (mut busy, mut n) = (0.0, 0_u32);
    let (mut run, mut weighted) = (0.0, 0.0);
    for r in window {
        if let Some(b) = r.busy {
            busy += b;
            n += 1;
        }
        if let Some(ratio) = r.ratio {
            let w = r.busy.unwrap_or(1.0);
            run += w;
            weighted += ratio * w;
        }
    }
    (
        (n > 0).then(|| busy / f64::from(n)),
        (run > 0.0).then(|| weighted / run),
    )
}

fn workload(reading: Option<Reading>, load: &LoadFrame<'_>, block: usize) -> Option<Workload> {
    let reading = reading?;
    if !reading.temp?.is_finite() {
        return None;
    }
    let work = Workload {
        busy: reading.busy?,
        ratio: reading.ratio?,
        global_busy: load.global_busy?,
        relative: load.relative.get(block).copied().flatten()?,
    };
    work.valid().then_some(work)
}

fn average_workload(samples: &[Option<Workload>]) -> Option<Workload> {
    let mut total = Workload {
        busy: 0.0,
        ratio: 0.0,
        global_busy: 0.0,
        relative: 0.0,
    };
    let mut count = 0_u32;
    for sample in samples {
        let sample = (*sample)?;
        total.busy += sample.busy;
        total.ratio += sample.ratio;
        total.global_busy += sample.global_busy;
        total.relative += sample.relative;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let n = f64::from(count);
    Some(Workload {
        busy: total.busy / n,
        ratio: total.ratio / n,
        global_busy: total.global_busy / n,
        relative: total.relative / n,
    })
}

impl Controller {
    #[must_use]
    pub fn new(settings: Settings, timing: Timing, blocks: usize) -> Self {
        Self {
            settings,
            timing,
            tracks: vec![Track::default(); blocks],
            window_readings: vec![Reading::default(); blocks],
            start: None,
            last_window: None,
            polls: 0,
            returned: None,
            touched: vec![false; blocks],
            last_poll: None,
            ceilings_seen: Vec::with_capacity(blocks),
            accepted_seen: Vec::with_capacity(blocks),
            settle_until: None,
        }
    }

    #[must_use]
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Совместимое старое имя; значения — арифметические средние, не медианы.
    pub fn medians(&self) -> impl Iterator<Item = Option<f64>> + '_ {
        self.means()
    }

    /// Средние температуры последнего полного окна блоков.
    pub fn means(&self) -> impl Iterator<Item = Option<f64>> + '_ {
        self.tracks.iter().map(|t| t.mean)
    }

    /// Границы последнего завершённого окна по часам, переданным в `step_loaded`.
    #[must_use]
    pub fn window_span(&self) -> Option<(Duration, Duration)> {
        self.last_window
    }

    /// Показания последнего завершённого окна в порядке блоков: температура и занятость — средние,
    /// множитель взвешен по занятости. Это сводка окна, а не показания последнего опроса.
    pub fn window_readings(&self) -> impl Iterator<Item = Reading> + '_ {
        self.window_readings.iter().copied()
    }

    /// Блоки не читались (опрос остановлен, потолки держит другая часть, сон): прежние окна, средние и паузы — уже не
    /// подряд, всё начинается заново.
    pub fn forget(&mut self) {
        for track in &mut self.tracks {
            let mut guard = std::mem::take(&mut track.guard);
            guard.reset_episode();
            *track = Track {
                window: std::mem::take(&mut track.window),
                workload: std::mem::take(&mut track.workload),
                guard,
                ..Track::default()
            };
            track.window.clear();
            track.workload.clear();
        }
        self.start = None;
        self.last_window = None;
        self.window_readings.fill(Reading::default());
        self.polls = 0;
        self.last_poll = None;
        self.settle_until = None;
    }

    /// Потолок блока изменён не регулятором (владелец, чужая программа, профиль): этот блок судится заново.
    pub fn forget_block(&mut self, block: usize) {
        self.last_window = None;
        if let Some(reading) = self.window_readings.get_mut(block) {
            *reading = Reading::default();
        }
        if let Some(track) = self.tracks.get_mut(block) {
            *track = Track {
                window: std::mem::take(&mut track.window),
                workload: std::mem::take(&mut track.workload),
                ..Track::default()
            };
            track.window.clear();
            track.workload.clear();
        }
    }

    /// Защита сохраняется отдельно от окон наблюдения, чтобы сон и перезапуск не подтверждали возврат.
    #[must_use]
    pub fn protection(&self, block: usize) -> Option<Protection> {
        self.tracks.get(block).and_then(|t| t.guard.protection)
    }

    pub fn seed_protection(&mut self, block: usize, protection: Protection) {
        if let Some(track) = self.tracks.get_mut(block) {
            track.guard.seed(protection);
        }
    }

    pub fn clear_protection(&mut self, block: usize) {
        if let Some(track) = self.tracks.get_mut(block) {
            track.guard = Guard::default();
        }
    }

    /// Уровень кристалла для блока `block`: среднее последних окон остальных блоков.
    fn die(&self, block: usize) -> Option<f64> {
        let (sum, n) = self
            .tracks
            .iter()
            .enumerate()
            .filter(|&(b, _)| b != block)
            .filter_map(|(_, t)| t.mean)
            .fold((0.0, 0_u32), |(s, n), m| (s + m, n + 1));
        (n > 0).then(|| sum / f64::from(n))
    }

    /// Один опрос в момент `now`. `allowed(блок, потолок, уровень кристалла)` — пускает ли память срезов вернуть блок на
    /// этот потолок. Решения — в `out` (прежнее содержимое стирается): аварийные — сразу, целевые и возврат — когда
    /// закрылось полное окно после установления.
    pub fn step(
        &mut self,
        now: Duration,
        frame: &Frame<'_>,
        allowed: &dyn Fn(usize, i32, Option<f64>) -> bool,
        out: &mut Vec<Decision>,
    ) {
        self.step_loaded(
            now,
            frame,
            &LoadFrame {
                global_busy: None,
                relative: &[],
            },
            allowed,
            out,
        );
    }

    /// Опрос с известной общей и относительной активностью; без этих данных разрешены только снижения.
    pub fn step_loaded(
        &mut self,
        now: Duration,
        frame: &Frame<'_>,
        load: &LoadFrame<'_>,
        allowed: &dyn Fn(usize, i32, Option<f64>) -> bool,
        out: &mut Vec<Decision>,
    ) {
        self.step_loaded_physical(now, frame, load, &|b, to, die, _| allowed(b, to, die), out);
    }

    /// Допуск возврата получает фактическую сводку только что закрытого окна в порядке блоков.
    /// Буфер переиспользуется; значения последнего опроса вместо средних не подставляются.
    pub fn step_loaded_physical(
        &mut self,
        now: Duration,
        frame: &Frame<'_>,
        load: &LoadFrame<'_>,
        allowed: &PhysicalPermit<'_>,
        out: &mut Vec<Decision>,
    ) {
        out.clear();
        if let Some(last) = self.last_poll {
            if now < last {
                self.forget();
            } else if now.saturating_sub(last) > self.timing.window {
                // Пропуск опросов требует нового полного окна, но не сокращает уже назначенное установление.
                let settle_until = self.settle_until;
                self.forget();
                self.settle_until = settle_until;
            }
        }
        self.last_poll = Some(now);
        if self.ceilings_seen != frame.ceilings || self.accepted_seen != frame.accepted {
            if !self.ceilings_seen.is_empty() {
                self.reset_configuration(now);
            }
            self.ceilings_seen.clear();
            self.ceilings_seen.extend_from_slice(frame.ceilings);
            self.accepted_seen.clear();
            self.accepted_seen.extend_from_slice(frame.accepted);
        }
        self.touched.fill(false);
        let ordinary = self.settle_until.is_none_or(|until| now >= until);
        if ordinary {
            self.start.get_or_insert(now);
            self.polls += 1;
        }
        self.emergency(now, frame, load, ordinary, out);
        let Some(start) = self.start else { return };
        if now.saturating_sub(start) >= self.timing.window {
            self.close(now, start, frame, allowed, out);
            self.start = Some(now);
            self.polls = 0;
        }
    }

    /// Смена любого потолка меняет тепловой контекст всех блоков. Проверки начинаются заново после установления;
    /// аварийные часы и сохранённая защита остаются, незавершённая проба получает новое окно доказательства.
    fn reset_configuration(&mut self, now: Duration) {
        self.start = None;
        self.last_window = None;
        self.window_readings.fill(Reading::default());
        self.polls = 0;
        self.returned = None;
        self.settle_until = Some(now + self.timing.settle);
        for track in &mut self.tracks {
            track.window.clear();
            track.workload.clear();
            track.mean = None;
            track.ratio = None;
            track.busy = None;
            track.cool_since = None;
            track.guard.reset_episode();
        }
    }

    /// Показания — в окна; аварийная отсечка по каждому.
    fn emergency(
        &mut self,
        now: Duration,
        frame: &Frame<'_>,
        load: &LoadFrame<'_>,
        ordinary: bool,
        out: &mut Vec<Decision>,
    ) {
        for b in 0..self.tracks.len() {
            let work = workload(frame.readings.get(b).copied().flatten(), load, b);
            if ordinary {
                self.tracks[b].workload.push(work);
            }
            let Some(reading) = frame.readings.get(b).copied().flatten() else {
                continue;
            };
            let Some(temp) = reading.temp.filter(|t| t.is_finite()) else {
                continue;
            };
            if ordinary {
                self.tracks[b].window.push(reading);
            }
            let track = &self.tracks[b];
            if temp < self.settings.emergency || track.recheck_at.is_some_and(|at| now < at) {
                continue;
            }
            let (Some(&ceiling), Some(&floor)) = (frame.ceilings.get(b), frame.floor.get(b)) else {
                continue;
            };
            let Some(to) = below(reading.ratio, ceiling, floor) else {
                continue;
            };
            out.push(Decision {
                cause: Cause::Emergency,
                block: b,
                from: ceiling,
                to,
                temp,
                die: self.die(b),
                ratio: reading.ratio,
                busy: reading.busy,
            });
            let track = &mut self.tracks[b];
            track.guard.cut(to, work);
            track.recheck_at = Some(now + self.timing.recheck);
            track.act(now);
            self.touched[b] = true;
        }
    }

    /// Окно закрыто: средние, целевая отсечка, часы прохлады, возврат.
    fn close(
        &mut self,
        now: Duration,
        start: Duration,
        frame: &Frame<'_>,
        allowed: &PhysicalPermit<'_>,
        out: &mut Vec<Decision>,
    ) {
        let limit = self.settings.limit;
        self.last_window = Some((start, now));
        // Сначала средние всех блоков: уровень кристалла использует то же закрытое окно.
        for track in &mut self.tracks {
            let (sum, count) = track
                .window
                .iter()
                .filter_map(|r| r.temp.filter(|t| t.is_finite()))
                .fold((0.0, 0_u32), |(sum, n), t| (sum + t, n + 1));
            let enough = count > 0 && count as usize * 2 >= self.polls;
            track.mean = enough.then(|| sum / f64::from(count));
            (track.busy, track.ratio) = means(&track.window);
            track.window.clear();
        }
        for (reading, track) in self.window_readings.iter_mut().zip(&self.tracks) {
            *reading = Reading {
                temp: track.mean,
                busy: track.busy,
                ratio: track.ratio,
            };
        }
        for b in 0..self.tracks.len() {
            let Some(m) = self.tracks[b].mean else {
                self.tracks[b].cool_since = None;
                continue;
            };
            if m <= limit - self.settings.return_gap {
                let track = &mut self.tracks[b];
                let since = track.acted.map_or(start, |acted| acted.max(start));
                track.cool_since.get_or_insert(since);
                continue;
            }
            self.tracks[b].cool_since = None;
            if m <= limit || self.touched[b] {
                continue;
            }
            let (Some(&ceiling), Some(&floor)) = (frame.ceilings.get(b), frame.floor.get(b)) else {
                continue;
            };
            let Some(to) = below(self.tracks[b].ratio, ceiling, floor) else {
                continue;
            };
            out.push(Decision {
                cause: Cause::Target,
                block: b,
                from: ceiling,
                to,
                temp: m,
                die: self.die(b),
                ratio: self.tracks[b].ratio,
                busy: self.tracks[b].busy,
            });
            self.tracks[b].act(now);
            let work = average_workload(&self.tracks[b].workload);
            self.tracks[b].guard.cut(to, work);
            self.touched[b] = true;
        }
        self.check_trials(now, start, frame, out);
        self.give_back(now, frame, allowed, out);
        for track in &mut self.tracks {
            track.workload.clear();
        }
    }

    fn check_trials(&mut self, now: Duration, start: Duration, frame: &Frame<'_>, out: &mut Vec<Decision>) {
        for b in 0..self.tracks.len() {
            let Some(&ceiling) = frame.ceilings.get(b) else {
                continue;
            };
            if self.touched[b] || self.tracks[b].acted.is_some_and(|acted| acted > start) {
                self.tracks[b].guard.reset_episode();
                continue;
            }
            let track = &mut self.tracks[b];
            let cool = track
                .mean
                .is_some_and(|m| m <= self.settings.limit - self.settings.return_gap);
            let Some(to) = track
                .guard
                .window(now, start, self.timing.pause, ceiling, &track.workload, cool)
            else {
                continue;
            };
            let temp = track.mean.unwrap_or(f64::NAN);
            let ratio = track.ratio;
            let busy = track.busy;
            out.push(Decision {
                cause: Cause::Revert,
                block: b,
                from: ceiling,
                to,
                temp,
                die: self.die(b),
                ratio,
                busy,
            });
            self.tracks[b].act(now);
            self.touched[b] = true;
        }
    }

    /// Возврат: из блоков ниже профиля, у которых все средние всю паузу не выше порога возврата и которых пускает память
    /// срезов, — самый прохладный, на одну ступень; один за паузу.
    fn give_back(&mut self, now: Duration, frame: &Frame<'_>, allowed: &PhysicalPermit<'_>, out: &mut Vec<Decision>) {
        let pause = self.timing.pause;
        if !out.is_empty()
            || self
                .tracks
                .iter()
                .any(|t| t.guard.protection.is_some_and(|p| p.trial.is_some()))
        {
            return;
        }
        if self.returned.is_some_and(|t| now.saturating_sub(t) < pause) {
            return;
        }
        let mut coolest: Option<(usize, f64)> = None;
        for (b, track) in self.tracks.iter().enumerate() {
            let (Some(&ceiling), Some(&profile), Some(m)) = (frame.ceilings.get(b), frame.accepted.get(b), track.mean)
            else {
                continue;
            };
            let cool = track.cool_since.is_some_and(|since| now.saturating_sub(since) >= pause);
            if ceiling >= profile
                || !cool
                || !track.guard.ready(now, pause, ceiling)
                || self.touched[b]
                || coolest.is_some_and(|(_, c)| m >= c)
            {
                continue;
            }
            if allowed(b, ceiling + 1, self.die(b), &self.window_readings) {
                coolest = Some((b, m));
            }
        }
        let Some((b, m)) = coolest else { return };
        let from = frame.ceilings[b];
        out.push(Decision {
            cause: Cause::Return,
            block: b,
            from,
            to: from + 1,
            temp: m,
            die: self.die(b),
            ratio: self.tracks[b].ratio,
            busy: self.tracks[b].busy,
        });
        self.tracks[b].act(now);
        self.tracks[b].guard.begin(from);
        self.touched[b] = true;
        self.returned = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_ratio_is_weighted_by_running_time() {
        let r = |busy: f64, ratio: Option<f64>| Reading {
            temp: Some(70.0),
            busy: Some(busy),
            ratio,
        };
        let (busy, ratio) = means(&[r(1.0, Some(50.0)), r(0.2, Some(30.0)), r(0.0, None)]);
        assert!(busy.is_some_and(|b| (b - 0.4).abs() < 1e-9));
        assert!(ratio.is_some_and(|x| (x - (50.0 + 30.0 * 0.2) / 1.2).abs() < 1e-9));
        assert_eq!(means(&[]), (None, None));
    }

    #[test]
    fn a_block_read_in_under_half_the_polls_of_a_window_has_no_mean() {
        let settings = Settings {
            limit: 85.0,
            emergency: 95.0,
            return_gap: 0.0,
        };
        let mut c = Controller::new(settings, Timing::REAL, 2);
        let mut out = Vec::new();
        let hot = Some(Reading {
            temp: Some(90.0),
            busy: Some(1.0),
            ratio: Some(50.0),
        });
        for k in 0..=8_u32 {
            // Block 1 read in 3 polls of 9.
            let readings = [hot, if k < 3 { hot } else { None }];
            let frame = Frame {
                readings: &readings,
                ceilings: &[50, 50],
                accepted: &[50, 50],
                floor: &[27, 27],
            };
            c.step(
                Duration::from_millis(250 * u64::from(k)),
                &frame,
                &|_, _, _| true,
                &mut out,
            );
        }
        let medians: Vec<Option<f64>> = c.medians().collect();
        assert_eq!(medians, [Some(90.0), None]);
    }
}
