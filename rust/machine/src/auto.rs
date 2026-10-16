// Регулятор автомодуля — две отсечки и возврат к профилю (DECISIONS #40, подзапись «две отсечки»; бриф
// outputs/cutoffs-brief-2026-10-16.md). По каждому показанию блоков (опрос CoreMapTray раз в 0,25 с) решает, какой блок
// опустить на ступень и какой вернуть. Окна, таймеров и вызовов Windows здесь нет: время и показания приходят снаружи,
// ответ — решения. Поэтому правила проверяются тестами на многих минутах «времени» за миллисекунды
// (machine/tests/cutoffs.rs — по брифу, не по этому коду).
//
// Владелец 16.10: «нужно сделать… два лимита… по мгновенной отсечке с целью безопасности… повыше… предотвратить…
// жесткого перегрева… и… отсечку… которая срабатывает… потому что мы не хотим на такой температуре работать». Нагрузка
// не входит ни в одно правило: «если он не загружен… и у него температура выше предела… как это так».
// - Аварийная отсечка: одно показание блока не ниже аварийного предела — ступень вниз сразу. Следующая аварийная
//   проверка этого блока — через 1 с (модель на его журнале мерила местный отклик на нагрузку около секунды); всё ещё
//   не ниже предела — ещё ступень.
// - Целевая отсечка: раз в 2 с (выбор владельца по таблице удвоения окна: «1 секунда, 2 секунды, 4 секунды… плюс-минус
//   одинаковый результат… 2 секунды нормально») у каждого блока — медиана его показаний за эти 2 с. Выше предела и не
//   ниже прошлой 2-с медианы — ступень вниз; выше, но падает — ждать: прошлая ступень ещё действует. Это устройство
//   пассивного охлаждения ACPI (_TC1·(Tn − Tn−1) + _TC2·(Tn − Tt)), сведённое к знакам, чтобы не было коэффициентов,
//   которые пришлось бы подбирать. Прошлой медианы нет (первое окно после начала или перерыва) — тренд не известен:
//   ждать одно окно.
// - Ступень вниз у обеих — на одну ниже фактического множителя блока (горячие ядра идут на 2–4 ступени ниже потолка —
//   измерено, снижение одного потолка над ними ничего бы не дало), не ниже нижней границы допустимого потолка
//   (`Frame::floor`, та же, что у ручного, machine::limits). Множитель не известен (блок спал) — на одну ниже потолка.
// - Возврат к профилю (пока его не заменит оптимизатор): блок, опущенный отсечкой, получает +1 ступень, когда все его
//   2-с медианы всю паузу были не выше предела; выше потолка профиля — никогда. За паузу — один блок, самый прохладный.
// - Без качелей: возврат на потолок, с которого блок был срезан, — только пока уровень кристалла (среднее последних 2-с
//   медиан остальных блоков) ниже, чем был при срезе. Срезы помнит machine::memory; регулятор спрашивает её через
//   `allowed` и отдаёт в каждом решении уровень кристалла, чтобы срез было с чем записать.

use std::time::Duration;

use crate::thermal::Reading;

/// Выбор владельца: целевой предел и аварийный, °C.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub limit: f64,
    pub emergency: f64,
}

/// Сроки регулятора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Окно медианы целевой отсечки.
    pub window: Duration,
    /// Через сколько после аварийной ступени аварийная проверка блока возобновляется.
    pub recheck: Duration,
    /// Сколько все 2-с медианы блока должны быть не выше предела, прежде чем он вернётся на ступень; и не чаще этого —
    /// возврат любого блока.
    pub pause: Duration,
}

impl Timing {
    /// Настоящие сроки. Окно 2 с — выбор владельца; 1 с — местный отклик на нагрузку, измеренный моделью на его
    /// журнале. Пауза 2 мин — ЗАГЛУШКА, принятая владельцем «для начала» («две минуты… для начала… окей»): её заменит
    /// время выхода на плато, которое измерит модель.
    pub const REAL: Self = Self {
        window: Duration::from_secs(2),
        recheck: Duration::from_secs(1),
        pause: Duration::from_mins(2),
    };
}

/// Какое правило решило.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Аварийная отсечка: одно показание не ниже аварийного предела.
    Emergency,
    /// Целевая: 2-с медиана выше предела и не падает.
    Target,
    /// Возврат на ступень к профилю.
    Return,
}

impl Cause {
    /// Как действие пишется в журнал действий (data\auto-log.jsonl, поле act); окно читает эти слова.
    #[must_use]
    pub fn act(self) -> &'static str {
        match self {
            Self::Emergency => "emergency",
            Self::Target => "target",
            Self::Return => "return",
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
    /// Температура, по которой решено, °C: показание (аварийная) или 2-с медиана (целевая, возврат).
    pub temp: f64,
    /// Уровень кристалла: среднее последних 2-с медиан остальных блоков; None — ни одной нет.
    pub die: Option<f64>,
    /// Фактический множитель и доля работы блока: у аварийной — за показание, у целевой и возврата — за 2-с окно.
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
    /// Показания текущего 2-с окна (буфер на всё время работы: 8 показаний в окне).
    window: Vec<Reading>,
    /// Медиана последнего закрытого окна; None — окна не было или показаний в нём меньше половины опросов.
    median: Option<f64>,
    /// Медиана окна перед ним — с ней сравнивается последняя (падает ли).
    prior: Option<f64>,
    /// Фактический множитель и доля работы за последнее закрытое окно.
    ratio: Option<f64>,
    busy: Option<f64>,
    /// До этого времени аварийная проверка блока стоит (после его аварийной ступени).
    recheck_at: Option<Duration>,
    /// Последнее изменение потолка блока регулятором.
    acted: Option<Duration>,
    /// С какого времени все 2-с медианы блока не выше предела; None — последняя выше, её нет или блок только что
    /// изменён.
    cool_since: Option<Duration>,
}

impl Track {
    fn act(&mut self, now: Duration) {
        self.acted = Some(now);
        self.cool_since = None;
    }
}

/// Регулятор: окна блоков, медианы, сроки, выбор владельца.
#[derive(Debug, Clone)]
pub struct Controller {
    settings: Settings,
    timing: Timing,
    tracks: Vec<Track>,
    /// Начало текущего окна (общего для всех блоков); None — показаний с последнего «забыть» не было.
    start: Option<Duration>,
    /// Опросов в текущем окне: медиана блока считается, если у него показаний не меньше половины — иначе он почти всё
    /// окно не читался, и это не 2 с его температуры.
    polls: usize,
    /// Последний возврат любого блока: следующий — не раньше паузы («один блок за паузу»).
    returned: Option<Duration>,
    /// Блоки, изменённые в этом опросе: второго решения тому же блоку в том же опросе нет — первое ещё не дошло.
    touched: Vec<bool>,
    /// Буфер для медианы.
    scratch: Vec<f64>,
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

/// Медиана (`values` переставляется); пусто — None.
fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        f64::midpoint(values[mid - 1], values[mid])
    } else {
        values[mid]
    })
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

impl Controller {
    #[must_use]
    pub fn new(settings: Settings, timing: Timing, blocks: usize) -> Self {
        Self {
            settings,
            timing,
            tracks: vec![Track::default(); blocks],
            start: None,
            polls: 0,
            returned: None,
            touched: vec![false; blocks],
            scratch: Vec::with_capacity(16),
        }
    }

    #[must_use]
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Последние 2-с медианы блоков (для журнала действий).
    pub fn medians(&self) -> impl Iterator<Item = Option<f64>> + '_ {
        self.tracks.iter().map(|t| t.median)
    }

    /// Блоки не читались (опрос остановлен, потолки держит другая часть, сон): прежние окна, медианы и паузы — уже не
    /// подряд, всё начинается заново.
    pub fn forget(&mut self) {
        for track in &mut self.tracks {
            *track = Track {
                window: std::mem::take(&mut track.window),
                ..Track::default()
            };
            track.window.clear();
        }
        self.start = None;
        self.polls = 0;
    }

    /// Потолок блока изменён не регулятором (владелец, чужая программа, профиль): этот блок судится заново.
    pub fn forget_block(&mut self, block: usize) {
        if let Some(track) = self.tracks.get_mut(block) {
            *track = Track {
                window: std::mem::take(&mut track.window),
                ..Track::default()
            };
            track.window.clear();
        }
    }

    /// Уровень кристалла для блока `block`: среднее последних 2-с медиан остальных блоков.
    fn die(&self, block: usize) -> Option<f64> {
        let (sum, n) = self
            .tracks
            .iter()
            .enumerate()
            .filter(|&(b, _)| b != block)
            .filter_map(|(_, t)| t.median)
            .fold((0.0, 0_u32), |(s, n), m| (s + m, n + 1));
        (n > 0).then(|| sum / f64::from(n))
    }

    /// Один опрос в момент `now`. `allowed(блок, потолок, уровень кристалла)` — пускает ли память срезов вернуть блок на
    /// этот потолок. Решения — в `out` (прежнее содержимое стирается): аварийные — сразу, целевые и возврат — когда
    /// закрылось 2-с окно.
    pub fn step(
        &mut self,
        now: Duration,
        frame: &Frame<'_>,
        allowed: &dyn Fn(usize, i32, Option<f64>) -> bool,
        out: &mut Vec<Decision>,
    ) {
        out.clear();
        self.touched.fill(false);
        let start = *self.start.get_or_insert(now);
        self.polls += 1;
        self.emergency(now, frame, out);
        if now.saturating_sub(start) >= self.timing.window {
            self.close(now, start, frame, allowed, out);
            self.start = Some(now);
            self.polls = 0;
        }
    }

    /// Показания — в окна; аварийная отсечка по каждому.
    fn emergency(&mut self, now: Duration, frame: &Frame<'_>, out: &mut Vec<Decision>) {
        for b in 0..self.tracks.len() {
            let Some(reading) = frame.readings.get(b).copied().flatten() else {
                continue;
            };
            let Some(temp) = reading.temp else { continue };
            self.tracks[b].window.push(reading);
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
            track.recheck_at = Some(now + self.timing.recheck);
            track.act(now);
            self.touched[b] = true;
        }
    }

    /// Окно закрыто: медианы, целевая отсечка, часы прохлады, возврат.
    fn close(
        &mut self,
        now: Duration,
        start: Duration,
        frame: &Frame<'_>,
        allowed: &dyn Fn(usize, i32, Option<f64>) -> bool,
        out: &mut Vec<Decision>,
    ) {
        let limit = self.settings.limit;
        let mut scratch = std::mem::take(&mut self.scratch);
        // Медианы — у всех блоков сначала: уровень кристалла для каждого считается по новым медианам остальных.
        for track in &mut self.tracks {
            scratch.clear();
            scratch.extend(track.window.iter().filter_map(|r| r.temp));
            let enough = !scratch.is_empty() && scratch.len() * 2 >= self.polls;
            track.prior = track.median;
            track.median = if enough { median(&mut scratch) } else { None };
            (track.busy, track.ratio) = means(&track.window);
            track.window.clear();
        }
        self.scratch = scratch;
        for b in 0..self.tracks.len() {
            let Some(m) = self.tracks[b].median else {
                self.tracks[b].cool_since = None;
                continue;
            };
            if m <= limit {
                let track = &mut self.tracks[b];
                let since = track.acted.map_or(start, |acted| acted.max(start));
                track.cool_since.get_or_insert(since);
                continue;
            }
            self.tracks[b].cool_since = None;
            // Выше предела: ступень, если медиана не падает; падает или тренд не известен — ждать.
            let rising = self.tracks[b].prior.is_some_and(|p| m >= p);
            if !rising || self.touched[b] {
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
            self.touched[b] = true;
        }
        self.give_back(now, frame, allowed, out);
    }

    /// Возврат: из блоков ниже профиля, у которых все 2-с медианы всю паузу не выше предела и которых пускает память
    /// срезов, — самый прохладный, на одну ступень; один за паузу.
    fn give_back(
        &mut self,
        now: Duration,
        frame: &Frame<'_>,
        allowed: &dyn Fn(usize, i32, Option<f64>) -> bool,
        out: &mut Vec<Decision>,
    ) {
        let pause = self.timing.pause;
        if self.returned.is_some_and(|t| now.saturating_sub(t) < pause) {
            return;
        }
        let mut coolest: Option<(usize, f64)> = None;
        for (b, track) in self.tracks.iter().enumerate() {
            let (Some(&ceiling), Some(&profile), Some(m)) =
                (frame.ceilings.get(b), frame.accepted.get(b), track.median)
            else {
                continue;
            };
            let cool = track.cool_since.is_some_and(|since| now.saturating_sub(since) >= pause);
            if ceiling >= profile || !cool || self.touched[b] || coolest.is_some_and(|(_, c)| m >= c) {
                continue;
            }
            if allowed(b, ceiling + 1, self.die(b)) {
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
        self.touched[b] = true;
        self.returned = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_an_even_count_is_the_mean_of_the_middle_two() {
        assert_eq!(
            median(&mut [70.0, 96.0, 70.0, 71.0, 70.0, 72.0, 70.0, 70.0]),
            Some(70.0)
        );
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(&mut [5.0]), Some(5.0));
        assert_eq!(median(&mut []), None);
    }

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
    fn a_block_read_in_under_half_the_polls_of_a_window_has_no_median() {
        let settings = Settings {
            limit: 85.0,
            emergency: 95.0,
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
