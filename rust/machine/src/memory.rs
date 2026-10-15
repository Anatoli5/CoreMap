// Память перегрева автомодуля (DECISIONS #32): блок, опущенный из-за перегрева, не поднимается обратно на ту ступень,
// где перегрелся, пока не выйдет запрет. Владелец 15.10: «если он видит, что оно не догревается, но он видит, что он
// уже пробовал большее значение и оно перегревается… уже не надо ставить это большее значение».
//
// Запрет — 30 минут. Перегрелся снова на той же ступени после конца запрета — вдвое дольше, но не больше суток. Сутки
// без перегрева на этой ступени после конца запрета — счёт заново: комната, вентилятор и игра меняются, вчерашний
// перегрев не приговор. Запрет навсегда (как у подборщиков разгона для нестабильных значений) отвергнут по той же
// причине. Числа — выбор агента, не мерены.
//
// Запрет действует, только пока перегрев, из-за которого он поставлен, выше нынешнего предела: предел подняли выше
// той температуры — прежний перегрев уже не перегрев. Остальные условия (соседи, вентиляторы, корпус) в правило пока
// не входят: они пишутся в журнал действий, правило по ним — когда журнал покажет, какие из них решают.
//
// Время — отметки `Stamp` (мировое): запреты переживают перезапуск. Своего файла у памяти нет: программа входа
// восстанавливает её из журнала действий (data\auto-log.jsonl), повторяя записанные перегревы по порядку, — у факта
// один владелец.

use std::collections::HashMap;
use std::time::Duration;

use crate::clock::Stamp;

/// Сроки запретов. Скрытая проверка программы входа их укорачивает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spans {
    /// Первый запрет.
    pub first: Duration,
    /// Дольше запрет не растёт.
    pub longest: Duration,
    /// Столько без перегрева на ступени после конца запрета — и счёт повторов начинается заново.
    pub forgive: Duration,
}

impl Spans {
    pub const REAL: Self = Self {
        first: Duration::from_mins(30),
        longest: Duration::from_hours(24),
        forgive: Duration::from_hours(24),
    };
}

/// Запрет поднимать блок на ступень `at` и выше.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ban {
    pub at: i32,
    pub until: Stamp,
    /// Который это перегрев на ступени подряд (1 — первый).
    pub strikes: u32,
    /// Средняя температура блока, из-за которой он был опущен, °C.
    pub mean: f64,
}

/// Перегревы по блокам (блок — по номеру своего первого ядра: он не зависит от порядка блоков).
#[derive(Debug, Clone)]
pub struct Memory {
    spans: Spans,
    bans: HashMap<(usize, i32), Ban>,
}

fn ticks(span: Duration) -> i64 {
    i64::try_from(span.as_millis()).map_or(i64::MAX, |ms| ms.saturating_mul(10_000))
}

impl Memory {
    #[must_use]
    pub fn new(spans: Spans) -> Self {
        Self {
            spans,
            bans: HashMap::new(),
        }
    }

    /// Блок перегрелся на ступени `at` (средняя `mean`) и опущен: поставить запрет и вернуть его.
    pub fn overheated(&mut self, core: usize, at: i32, mean: f64, now: Stamp) -> Ban {
        let strikes = match self.bans.get(&(core, at)) {
            Some(ban) if now.0 <= ban.until.0.saturating_add(ticks(self.spans.forgive)) => {
                ban.strikes.saturating_add(1)
            }
            _ => 1,
        };
        let factor = 1_u32.checked_shl(strikes - 1).unwrap_or(u32::MAX);
        let length = self.spans.first.saturating_mul(factor).min(self.spans.longest);
        let ban = Ban {
            at,
            until: Stamp(now.0.saturating_add(ticks(length))),
            strikes,
            mean,
        };
        self.bans.insert((core, at), ban);
        ban
    }

    /// Запрет, который не пускает блок на потолок `to` при пределе `limit`: из действующих на `to` и ниже — тот, что
    /// кончается позже. None — подниматься можно.
    #[must_use]
    pub fn blocking(&self, core: usize, to: i32, limit: f64, now: Stamp) -> Option<Ban> {
        self.bans
            .iter()
            .filter(|&(&(c, at), ban)| c == core && at <= to && ban.until > now && ban.mean > limit)
            .map(|(_, ban)| *ban)
            .max_by_key(|ban| ban.until)
    }

    /// Действующие запреты блока при пределе `limit` — для подсказки и окна.
    #[must_use]
    pub fn active(&self, core: usize, limit: f64, now: Stamp) -> Vec<Ban> {
        let mut bans: Vec<Ban> = self
            .bans
            .iter()
            .filter(|&(&(c, _), ban)| c == core && ban.until > now && ban.mean > limit)
            .map(|(_, ban)| *ban)
            .collect();
        bans.sort_by_key(|ban| ban.at);
        bans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60 * 10_000_000;

    fn at(minutes: i64) -> Stamp {
        Stamp(1_000_000 * MINUTE + minutes * MINUTE)
    }

    #[test]
    fn the_first_ban_is_half_an_hour_and_covers_the_step_and_above() {
        let mut m = Memory::new(Spans::REAL);
        let ban = m.overheated(1, 50, 88.0, at(0));
        assert_eq!((ban.strikes, ban.until), (1, at(30)));
        assert!(m.blocking(1, 50, 85.0, at(29)).is_some(), "the step itself");
        assert!(m.blocking(1, 51, 85.0, at(29)).is_some(), "above it");
        assert!(m.blocking(1, 49, 85.0, at(29)).is_none(), "below it is allowed");
        assert!(m.blocking(1, 50, 85.0, at(30)).is_none(), "over at its end");
        assert!(m.blocking(2, 50, 85.0, at(10)).is_none(), "another block");
    }

    #[test]
    fn a_repeat_after_the_ban_doubles_it_up_to_a_day() {
        let mut m = Memory::new(Spans::REAL);
        let mut now = at(0);
        let mut lengths = Vec::new();
        for _ in 0..8 {
            let ban = m.overheated(1, 50, 88.0, now);
            lengths.push((ban.until.0 - now.0) / MINUTE);
            now = Stamp(ban.until.0 + 5 * MINUTE); // raised again after the ban and overheated five minutes later
        }
        assert_eq!(lengths, [30, 60, 120, 240, 480, 960, 1440, 1440]);
    }

    #[test]
    fn a_day_without_overheating_after_the_ban_starts_the_count_afresh() {
        let mut m = Memory::new(Spans::REAL);
        let first = m.overheated(1, 50, 88.0, at(0));
        let second = m.overheated(1, 50, 88.0, Stamp(first.until.0 + 23 * 60 * MINUTE));
        assert_eq!(second.strikes, 2);
        let third = m.overheated(1, 50, 88.0, Stamp(second.until.0 + 24 * 60 * MINUTE + MINUTE));
        assert_eq!(third.strikes, 1, "forgiven after a quiet day");
    }

    #[test]
    fn a_ban_from_an_overheat_below_the_new_limit_does_not_hold() {
        // Lowered at 88 °C under an 85 °C limit; the owner moved the limit to 90 - 88 °C is no overheat any more.
        let mut m = Memory::new(Spans::REAL);
        m.overheated(1, 50, 88.0, at(0));
        assert!(m.blocking(1, 50, 85.0, at(5)).is_some());
        assert!(m.blocking(1, 50, 90.0, at(5)).is_none());
        assert_eq!(m.active(1, 90.0, at(5)), []);
    }

    #[test]
    fn steps_are_remembered_apart() {
        let mut m = Memory::new(Spans::REAL);
        m.overheated(1, 52, 88.0, at(0));
        let ban = m.overheated(1, 50, 88.0, at(40));
        assert_eq!(ban.strikes, 1, "another step, its own count");
        let active: Vec<i32> = m.active(1, 85.0, at(45)).iter().map(|b| b.at).collect();
        assert_eq!(active, [50], "the ban at x52 ended at minute 30");
    }
}
