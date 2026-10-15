// Датчики автомодуля: температура корпуса и блоков, доля работы блока и его фактический множитель — те же регистры
// и та же арифметика, что у LiveBackend в Sensors.cs (DECISIONS #31, автомодуль).
//
// Температура = TjMax (0x1A2, биты 23:16) минус показание (биты 22:16 регистра 0x19C ядра или 0x1B1 корпуса), если
// показание действительно (бит 31). Так же считает ThrottleStop (сверено владельцем, KNOWLEDGE.md «Датчики»).
// Доля работы и фактический множитель — по приращениям двух счётчиков ядра: MPERF (0xE7) идёт с опорной частотой,
// пока ядро не спит; APERF (0xE8) — с фактической. Доля работы = ΔMPERF / (Δt × опорная частота); множитель =
// опорный множитель (0xCE, биты 15:8) × ΔAPERF / ΔMPERF. У кластера E-ядер датчик общий: блок меряется по первому
// ядру (Block.cs в C#).

use std::time::Instant;

use crate::mailbox::MailboxCpu;
use crate::{HwError, affinity};

const THERM_STATUS: u32 = 0x19C;
const TEMPERATURE_TARGET: u32 = 0x1A2;
const PACKAGE_THERM_STATUS: u32 = 0x1B1;
const MPERF: u32 = 0xE7;
const APERF: u32 = 0xE8;
const PLATFORM_INFO: u32 = 0xCE;
/// Меньше стольких опорных тактов между опросами — ядро почти всё время спало, множитель не считается (как в C#).
const AWAKE_AT_LEAST: u64 = 10_000_000;
/// Промежуток длиннее — доля работы за него ничего не говорит о нынешней нагрузке: опрос начинается заново.
const GAP: std::time::Duration = std::time::Duration::from_secs(10);

/// Показание блока за последний промежуток между опросами.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Reading {
    /// °C; None — датчик не дал действительного показания.
    pub temp: Option<f64>,
    /// Доля времени, которую блок работал (0…1); None — первый опрос, сравнивать не с чем.
    pub busy: Option<f64>,
    /// Средний фактический множитель, пока блок работал; None — первый опрос или блок почти спал.
    pub ratio: Option<f64>,
}

/// Датчики: настоящий процессор (`MailboxCpu`) или имитация (`sim::SimCpu`). Ядро — первое ядро блока.
pub trait Thermal: std::fmt::Debug {
    /// Температура корпуса, °C. Регистр общий для всего процессора: поток к ядру не привязывается.
    fn package(&mut self) -> Result<Option<f64>, HwError>;
    /// Показание блока по его первому ядру.
    fn block(&mut self, core: usize) -> Result<Reading, HwError>;
}

/// Состояние датчиков настоящего процессора между опросами.
#[derive(Debug, Clone, Default)]
pub struct Counters {
    tjmax: Option<i32>,
    reference: Option<u64>,
    last: Vec<Option<(u64, u64, Instant)>>, // (APERF, MPERF, когда) по ядрам
}

impl Counters {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            tjmax: None,
            reference: None,
            last: vec![None; cores],
        }
    }
}

fn temperature(value: u64, tjmax: i32) -> Option<f64> {
    let valid = value >> 31 & 1 == 1;
    let readout = i32::try_from(value >> 16 & 0x7F).unwrap_or(0);
    valid.then(|| f64::from(tjmax - readout))
}

impl MailboxCpu {
    /// `TjMax` и опорный множитель: читаются один раз.
    fn constants(&mut self) -> Result<(i32, u64), HwError> {
        if let (Some(tjmax), Some(reference)) = (self.sensors.tjmax, self.sensors.reference) {
            return Ok((tjmax, reference));
        }
        let tjmax = i32::try_from(self.msr.read(TEMPERATURE_TARGET)? >> 16 & 0xFF).unwrap_or(100);
        let reference = self.msr.read(PLATFORM_INFO)? >> 8 & 0xFF;
        self.sensors.tjmax = Some(tjmax);
        self.sensors.reference = Some(reference);
        Ok((tjmax, reference))
    }
}

impl Thermal for MailboxCpu {
    fn package(&mut self) -> Result<Option<f64>, HwError> {
        let (tjmax, _) = self.constants()?;
        Ok(temperature(self.msr.read(PACKAGE_THERM_STATUS)?, tjmax))
    }

    fn block(&mut self, core: usize) -> Result<Reading, HwError> {
        let (tjmax, reference) = self.constants()?;
        let Some(_pinned) = affinity::pin(self.lps[core]) else {
            return Ok(Reading::default()); // поток не привязался — показаний этого блока в опросе нет
        };
        let status = self.msr.read(THERM_STATUS)?;
        let aperf = self.msr.read(APERF)?;
        let mperf = self.msr.read(MPERF)?;
        let now = Instant::now();
        let previous = self.sensors.last[core].replace((aperf, mperf, now));
        // Счётчик пошёл назад (после гибернации процессор их обнуляет) или перерыв долгий — как первый опрос
        // (ревизия, outputs/review-auto-2026-10-14.md L2).
        let previous = previous.filter(|&(a, m, at)| aperf >= a && mperf >= m && now.duration_since(at) <= GAP);
        let (busy, ratio) = match previous {
            Some((a, m, at)) => {
                let ran = mperf.wrapping_sub(m);
                let seconds = now.duration_since(at).as_secs_f64();
                // Опорная частота — опорный множитель × 100 МГц.
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "counter deltas over seconds stay far below 2^52"
                )]
                let busy = (seconds > 0.0 && reference > 0)
                    .then(|| (ran as f64 / (seconds * reference as f64 * 1e8)).clamp(0.0, 1.0));
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "counter deltas over seconds stay far below 2^52"
                )]
                let ratio =
                    (ran >= AWAKE_AT_LEAST).then(|| reference as f64 * aperf.wrapping_sub(a) as f64 / ran as f64);
                (busy, ratio)
            }
            None => (None, None),
        };
        Ok(Reading {
            temp: temperature(status, tjmax),
            busy,
            ratio,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_is_tjmax_minus_readout_when_valid() {
        // 105 °C TjMax (measured on the 275HX), readout 30 -> 75 °C; bit 31 = valid.
        assert_eq!(temperature(1 << 31 | 30 << 16, 105), Some(75.0));
        assert_eq!(temperature(30 << 16, 105), None, "an invalid reading is no reading");
    }
}
