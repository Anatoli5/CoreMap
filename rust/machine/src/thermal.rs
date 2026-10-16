// Датчики автомодуля: температура корпуса и блоков, доля работы блока и его фактический множитель — те же регистры
// и та же арифметика, что у LiveBackend в Sensors.cs (DECISIONS #31, автомодуль).
//
// Температура = TjMax (0x1A2, биты 23:16) минус показание (биты 22:16 регистра 0x19C ядра или 0x1B1 корпуса), если
// показание действительно (бит 31). Так же считает ThrottleStop (сверено владельцем, KNOWLEDGE.md «Датчики»).
// Доля работы и фактический множитель — по приращениям двух счётчиков ядра: MPERF (0xE7) идёт с опорной частотой,
// пока ядро не спит; APERF (0xE8) — с фактической. Доля работы = ΔMPERF / (Δt × опорная частота); множитель =
// опорный множитель (0xCE, биты 15:8) × ΔAPERF / ΔMPERF. У кластера E-ядер датчик общий: блок меряется по первому
// ядру (Block.cs в C#).
//
// Мощность корпуса — по счётчику энергии корпуса (0x611, биты 31:0; единица — 1/2^N Дж, N — биты 12:8 регистра 0x606,
// Intel SDM том 4), как у LiveBackend в Sensors.cs: средняя за промежуток между двумя чтениями. Оба регистра модуль
// IntelMSR читать пускает (список чтения в IntelMSR.p, github.com/namazso/PawnIO.Modules); на машине владельца C#-часть
// их читала (data\joint-20261014-153258.json: 21,7 Вт). Счётчик 32-битный и переполняется: при 200 Вт и единице
// 61 мкДж — раз в 22 минуты; между чтениями раз в 0,25 с переполнение бывает не больше одного.
//
// Напряжение ядра — биты 47:32 регистра 0x198 (IA32_PERF_STATUS) этого ядра, в единицах 1/2^13 В (Intel SDM том 4,
// MSR_PERF_STATUS, начиная со 2-го поколения Core), показание в момент чтения. Модуль IntelMSR регистр читать пускает
// (KNOWLEDGE.md: 0x198 доступен, измерено); что поле напряжения на 275HX заполнено — не проверено, покажет журнал.
// Зачем (владелец 16.10): мощность ядра ≈ нагрузка × множитель × напряжение², напряжение — часть фактического состояния
// ядра для тепловой модели.

use std::time::Instant;

use crate::mailbox::MailboxCpu;
use crate::{HwError, affinity};

const THERM_STATUS: u32 = 0x19C;
const TEMPERATURE_TARGET: u32 = 0x1A2;
const PACKAGE_THERM_STATUS: u32 = 0x1B1;
const MPERF: u32 = 0xE7;
const APERF: u32 = 0xE8;
const PLATFORM_INFO: u32 = 0xCE;
const PERF_STATUS: u32 = 0x198;
const RAPL_POWER_UNIT: u32 = 0x606;
const PKG_ENERGY_STATUS: u32 = 0x611;
/// Меньше стольких опорных тактов между опросами — ядро почти всё время спало, множитель не считается (как в C#).
const AWAKE_AT_LEAST: u64 = 10_000_000;
/// Промежуток длиннее — доля работы за него ничего не говорит о нынешней нагрузке: опрос начинается заново.
const GAP: std::time::Duration = std::time::Duration::from_secs(10);
/// Больше этого корпус ноутбука не берёт (у 275HX наибольшая мощность турбо — 160 Вт по спецификации): такое число —
/// не мощность, а сброс счётчика (после гибернации процессор обнуляет его) между чтениями.
const MOST_WATTS: f64 = 1000.0;
/// Напряжение ядра вне этих пределов — не напряжение (поле пустое или не то): у процессоров Intel этих лет рабочее
/// напряжение ядра — десятые доли вольта до полутора.
const VOLTS: std::ops::RangeInclusive<f64> = 0.3..=2.0;

/// Показание блока за последний промежуток между опросами.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Reading {
    /// °C; None — датчик не дал действительного показания.
    pub temp: Option<f64>,
    /// Доля времени, которую блок работал (0…1); None — первый опрос, сравнивать не с чем.
    pub busy: Option<f64>,
    /// Средний фактический множитель, пока блок работал; None — первый опрос или блок почти спал.
    pub ratio: Option<f64>,
    /// Напряжение ядра, В, в момент чтения; None — регистр не прочитался или поле не похоже на напряжение.
    pub volts: Option<f64>,
}

/// Датчики: настоящий процессор (`MailboxCpu`) или имитация (`sim::SimCpu`). Ядро — первое ядро блока.
pub trait Thermal: std::fmt::Debug {
    /// Температура корпуса, °C. Регистр общий для всего процессора: поток к ядру не привязывается.
    fn package(&mut self) -> Result<Option<f64>, HwError>;
    /// Показание блока по его первому ядру.
    fn block(&mut self, core: usize) -> Result<Reading, HwError>;
    /// Средняя мощность корпуса, Вт, с прошлого вызова. None — первый вызов, перерыв дольше 10 с или счётчик за
    /// промежуток не сдвинулся либо сброшен. Регистр общий для всего процессора: поток к ядру не привязывается.
    fn package_power(&mut self) -> Result<Option<f64>, HwError>;
}

/// Состояние датчиков настоящего процессора между опросами.
#[derive(Debug, Clone, Default)]
pub struct Counters {
    tjmax: Option<i32>,
    reference: Option<u64>,
    last: Vec<Option<(u64, u64, Instant)>>, // (APERF, MPERF, когда) по ядрам
    /// Джоулей на единицу счётчика энергии: читается один раз.
    joules: Option<f64>,
    /// Счётчик энергии корпуса на прошлом чтении и когда.
    energy: Option<(u32, Instant)>,
}

impl Counters {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            tjmax: None,
            reference: None,
            last: vec![None; cores],
            joules: None,
            energy: None,
        }
    }
}

/// Напряжение ядра по регистру 0x198: биты 47:32 × 1/2^13 В; None — вне `VOLTS`.
fn core_volts(value: u64) -> Option<f64> {
    let volts = f64::from(u32::try_from(value >> 32 & 0xFFFF).unwrap_or(0)) / 8192.0;
    VOLTS.contains(&volts).then_some(volts)
}

fn temperature(value: u64, tjmax: i32) -> Option<f64> {
    let valid = value >> 31 & 1 == 1;
    let readout = i32::try_from(value >> 16 & 0x7F).unwrap_or(0);
    valid.then(|| f64::from(tjmax - readout))
}

/// Джоулей на единицу счётчика энергии по регистру 0x606: 1/2^N, N — биты 12:8.
fn joules_per_count(unit: u64) -> f64 {
    let shift = u32::try_from(unit >> 8 & 0x1F).unwrap_or(0);
    1.0 / f64::from(1_u32 << shift)
}

/// Средняя мощность, Вт, между двумя показаниями 32-битного счётчика энергии `seconds` секунд друг от друга. Одно
/// переполнение между ними учтено (разность по модулю 2^32). None — промежуток не больше нуля или длиннее 10 с (за
/// такой счётчик мог переполниться не раз, а мощность — давно не нынешняя); счётчик не сдвинулся (корпус без нуля
/// ватт не бывает — значит, счётчик не идёт); вышло больше `MOST_WATTS` (счётчик сброшен, а не переполнен).
#[must_use]
pub fn watts(previous: u32, now: u32, joules_per_count: f64, seconds: f64) -> Option<f64> {
    if seconds.is_nan() || seconds <= 0.0 || seconds > GAP.as_secs_f64() {
        return None;
    }
    let counts = now.wrapping_sub(previous);
    if counts == 0 {
        return None;
    }
    let watts = f64::from(counts) * joules_per_count / seconds;
    (watts <= MOST_WATTS).then_some(watts)
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
        // Напряжение — лишний столбец журнала: его отказ не должен останавливать температуру и множитель, он виден
        // пустым столбцом.
        let volts = self.msr.read(PERF_STATUS).ok().and_then(core_volts);
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
            volts,
        })
    }

    fn package_power(&mut self) -> Result<Option<f64>, HwError> {
        let unit = if let Some(unit) = self.sensors.joules {
            unit
        } else {
            let unit = joules_per_count(self.msr.read(RAPL_POWER_UNIT)?);
            self.sensors.joules = Some(unit);
            unit
        };
        let raw = u32::try_from(self.msr.read(PKG_ENERGY_STATUS)? & 0xFFFF_FFFF).unwrap_or(0);
        let now = Instant::now();
        let previous = self.sensors.energy.replace((raw, now));
        Ok(previous.and_then(|(was, at)| watts(was, raw, unit, now.duration_since(at).as_secs_f64())))
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

    #[test]
    fn core_voltage_is_bits_47_to_32_in_units_of_2_to_minus_13() {
        // 1.25 V = 10240 / 8192; the low 32 bits (ratio fields) do not matter.
        assert_eq!(core_volts(10240 << 32 | 0x2D00), Some(1.25));
        assert_eq!(core_volts(0x2D00), None, "an empty field is no voltage");
        assert_eq!(core_volts(0xFFFF << 32), None, "8 V is not a core voltage");
    }

    // Счётчик энергии по Intel SDM (том 4, MSR 606H и 611H): единица 1/2^ESU Дж, ESU — биты 12:8; счётчик 32-битный,
    // переполняется. Числа — не копия формулы: энергия задана в джоулях, ожидаемые ватты посчитаны отдельно.
    const ESU_14: u64 = 0x000A_0E03; // типичное значение 0x606 у Intel: мощность 1/8 Вт, энергия 1/16384 Дж, время 1/1024 с

    #[test]
    fn energy_unit_comes_from_bits_12_to_8() {
        assert!((joules_per_count(ESU_14) - 1.0 / 16384.0).abs() < 1e-15);
        assert!((joules_per_count(0x0000_1000) - 1.0 / 65536.0).abs() < 1e-15, "ESU 16");
        assert!((joules_per_count(0) - 1.0).abs() < 1e-15, "ESU 0: one joule per count");
    }

    #[test]
    fn watts_are_energy_over_the_interval() {
        let unit = joules_per_count(ESU_14);
        // 25 J in 0.25 s = 100 W.
        let counts = 25 * 16384;
        let w = watts(1_000_000, 1_000_000 + counts, unit, 0.25).expect("a normal interval");
        assert!((w - 100.0).abs() < 1e-9, "{w}");
    }

    #[test]
    fn one_wrap_of_the_32_bit_counter_is_counted() {
        let unit = joules_per_count(ESU_14);
        // 10 J in 0.25 s across the wrap: 40 W, not a negative or a huge number.
        let before = u32::MAX - 100_000;
        let after = before.wrapping_add(10 * 16384);
        assert!(after < before, "the test interval crosses the wrap");
        let w = watts(before, after, unit, 0.25).expect("a wrap is not an error");
        assert!((w - 40.0).abs() < 1e-9, "{w}");
    }

    #[test]
    fn a_reset_counter_a_still_counter_and_a_bad_interval_give_no_power() {
        let unit = joules_per_count(ESU_14);
        // Counter reset to near zero after hibernation: the modular difference is ~1.3e9 counts = 79 kJ in 0.25 s.
        assert_eq!(watts(3_000_000_000, 5_000, unit, 0.25), None);
        assert_eq!(
            watts(42, 42, unit, 0.25),
            None,
            "a counter that did not move is not 0 W"
        );
        assert_eq!(watts(0, 16384, unit, 0.0), None);
        assert_eq!(
            watts(0, 16384, unit, 30.0),
            None,
            "after a pause the interval is not this row's"
        );
        assert!(watts(0, 16384, unit, 10.0).is_some(), "10 s is still an interval");
    }
}
