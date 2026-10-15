// Показания ядер, когда их читают двое: окно (все ядра раз в полсекунды) и автомодуль (первые ядра блоков раз в 2 с).
//
// Доля работы и фактический множитель — по приращениям счётчиков ядра между двумя чтениями (machine::thermal), и
// счётчики у ядра одни. Если бы автомодуль читал ядро сам, пока его читает окно, его показание покрывало бы не 2 с, а
// то, что осталось с последнего чтения окна (доли секунды). Поэтому каждое чтение ядра идёт сюда (`Meter::record`), а
// автомодуль забирает сумму за всё время со своего прошлого раза (`Meter::drain`): доля работы — среднее по времени,
// множитель — среднее по времени работы. Это ровно то, что дало бы одно чтение через 2 с: приращения счётчиков
// складываются. Окна нет — автомодуль читает сам, и сумма состоит из одного его чтения, как раньше.
//
// `Watch` — что видит окно: последнее показание каждого ядра, наибольшая температура с последнего сброса и средняя
// под нагрузкой (Core.Average в C#-части, правило ниже).

use std::time::Instant;

use machine::auto::BUSY;
use machine::thermal::Reading;

/// Сумма показаний одного ядра со времени прошлого `drain`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Sum {
    /// Секунд с известной долей работы.
    time: f64,
    /// Доля работы × секунды.
    busy: f64,
    /// Доля работы × секунды там, где известен множитель (время работы, по которому он средний).
    run: f64,
    /// Множитель × доля работы × секунды.
    ratio: f64,
}

/// Чтения ядер и их суммы для автомодуля.
#[derive(Debug, Clone)]
pub struct Meter {
    last: Vec<Option<Instant>>,
    temp: Vec<Option<f64>>,
    sums: Vec<Sum>,
    /// Сколько ядер прочитано всего — скрытая проверка видит, не читает ли программа без окна.
    pub reads: u64,
}

impl Meter {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            last: vec![None; cores],
            temp: vec![None; cores],
            sums: vec![Sum::default(); cores],
            reads: 0,
        }
    }

    /// Ядро прочитано в `at`: показание покрывает время с его прошлого чтения.
    pub fn record(&mut self, core: usize, reading: Reading, at: Instant) {
        self.reads += 1;
        let Some(slot) = self.sums.get_mut(core) else { return };
        let since = self.last[core]
            .replace(at)
            .map(|t| at.saturating_duration_since(t).as_secs_f64());
        self.temp[core] = reading.temp;
        let (Some(seconds), Some(busy)) = (since, reading.busy) else {
            return; // первое чтение или перерыв: за этот отрезок известно только температура
        };
        slot.time += seconds;
        slot.busy += busy * seconds;
        if let Some(ratio) = reading.ratio {
            slot.run += busy * seconds;
            slot.ratio += ratio * busy * seconds;
        }
    }

    /// Было ли чтение ядра после прошлого `drain`.
    #[must_use]
    pub fn has(&self, core: usize) -> bool {
        self.sums.get(core).is_some_and(|s| s.time > 0.0)
    }

    /// Показание ядра за всё время с прошлого раза (температура — последняя) и начать сумму заново.
    pub fn drain(&mut self, core: usize) -> Reading {
        let Some(sum) = self.sums.get_mut(core).map(std::mem::take) else {
            return Reading::default();
        };
        Reading {
            temp: self.temp[core],
            busy: (sum.time > 0.0).then(|| sum.busy / sum.time),
            ratio: (sum.run > 0.0).then(|| sum.ratio / sum.run),
        }
    }

    /// Суммы всех ядер — заново (автомодуль блоков не судил: старое не должно попасть в следующее показание).
    pub fn forget(&mut self) {
        self.sums.fill(Sum::default());
    }
}

/// Столько опросов окна (по полсекунды) подряд ядро должно быть нагружено, прежде чем идти в среднюю: оно ещё греется
/// (`AverageSkip` в Core.cs — 4 опроса, 2 с).
const WARM: u32 = 4;

/// Средняя температура и средний фактический множитель ядра под его собственной нагрузкой (Core.Average в C#).
///
/// Правило C#: считается, только пока само ядро нагружено, и относится к одной комбинации — отсчёт заново, когда
/// меняется набор нагруженных ядер, сила нагрузки или потолки; первые 2 с после этого не в счёт. Нагрузку тогда давали
/// кнопки программы, и набор нагруженных ядер стоял минутами. Теперь нагружает игра (владелец 15.10), и «нагружено» —
/// доля работы ядра не меньше `BUSY` (как у автомодуля); набор таких ядер в игре меняется каждые полсекунды, и отсчёт
/// заново по нему не накопил бы ничего. Поэтому:
/// - отсчёт заново — когда меняются потолки (средняя относится к одним потолкам) и по сбросу владельца;
/// - каждое ядро идёт в среднюю, только пока само нагружено, и только после `WARM` опросов нагрузки подряд;
/// - у ядра без нагрузки значение остаётся то, что было под его последней нагрузкой (как в C#).
#[derive(Debug, Clone)]
struct Means {
    key: Vec<i32>,
    streak: Vec<u32>,
    temp: Vec<(f64, u32)>,
    ratio: Vec<(f64, u32)>,
    avg: Vec<Option<f64>>,
    avg_ratio: Vec<Option<f64>>,
}

impl Means {
    fn new(cores: usize) -> Self {
        Self {
            key: Vec::new(),
            streak: vec![0; cores],
            temp: vec![(0.0, 0); cores],
            ratio: vec![(0.0, 0); cores],
            avg: vec![None; cores],
            avg_ratio: vec![None; cores],
        }
    }

    /// Отсчёт заново: суммы и счёт нагрузки подряд — с нуля; у нагруженных сейчас ядер прежней средней больше нет.
    fn restart(&mut self, busy: &[Option<f64>]) {
        self.streak.fill(0);
        self.temp.fill((0.0, 0));
        self.ratio.fill((0.0, 0));
        for (i, b) in busy.iter().enumerate() {
            if b.is_some_and(|b| b >= BUSY) {
                self.avg[i] = None;
                self.avg_ratio[i] = None;
            }
        }
    }

    fn add(&mut self, ceilings: &[i32], cur: &[Option<f64>], ratio: &[Option<f64>], busy: &[Option<f64>]) {
        if self.key != ceilings {
            ceilings.clone_into(&mut self.key);
            self.restart(busy);
        }
        for i in 0..self.streak.len() {
            if !busy.get(i).copied().flatten().is_some_and(|b| b >= BUSY) {
                self.streak[i] = 0;
                continue;
            }
            self.streak[i] += 1;
            if self.streak[i] <= WARM {
                continue;
            }
            if let Some(t) = cur.get(i).copied().flatten() {
                let (sum, n) = &mut self.temp[i];
                *sum += t;
                *n += 1;
                self.avg[i] = Some(*sum / f64::from(*n));
            }
            if let Some(r) = ratio.get(i).copied().flatten() {
                let (sum, n) = &mut self.ratio[i];
                *sum += r;
                *n += 1;
                self.avg_ratio[i] = Some(*sum / f64::from(*n));
            }
        }
    }

    /// Сброс владельцем («сбросить максимумы»): средних нет ни у кого, отсчёт заново (Core.ResetAverage).
    fn reset(&mut self) {
        self.key.clear();
        self.avg.fill(None);
        self.avg_ratio.fill(None);
        self.streak.fill(0);
        self.temp.fill((0.0, 0));
        self.ratio.fill((0.0, 0));
    }
}

/// Что видит окно: последние показания ядер и корпуса, наибольшие с последнего сброса, средние под нагрузкой.
#[derive(Debug, Clone)]
pub struct Watch {
    pub cur: Vec<Option<f64>>,
    pub max: Vec<Option<f64>>,
    pub ratio: Vec<Option<f64>>,
    pub busy: Vec<Option<f64>>,
    pub pkg: Option<f64>,
    pub pkg_max: Option<f64>,
    /// С какого времени считаются наибольшие и средние.
    pub since: Instant,
    /// Сколько опросов прошло (номер состояния для окна).
    pub polls: u64,
    means: Means,
}

impl Watch {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            cur: vec![None; cores],
            max: vec![None; cores],
            ratio: vec![None; cores],
            busy: vec![None; cores],
            pkg: None,
            pkg_max: None,
            since: Instant::now(),
            polls: 0,
            means: Means::new(cores),
        }
    }

    /// Показание ядра из опроса.
    pub fn core(&mut self, core: usize, reading: Reading) {
        if core >= self.cur.len() {
            return;
        }
        self.cur[core] = reading.temp;
        self.ratio[core] = reading.ratio;
        self.busy[core] = reading.busy;
        if let Some(t) = reading.temp {
            self.max[core] = Some(self.max[core].map_or(t, |m| m.max(t)));
        }
    }

    pub fn package(&mut self, pkg: Option<f64>) {
        self.pkg = pkg;
        if let Some(p) = pkg {
            self.pkg_max = Some(self.pkg_max.map_or(p, |m| m.max(p)));
        }
    }

    /// Опрос окончен: средние под нагрузкой при этих потолках.
    pub fn polled(&mut self, ceilings: &[i32]) {
        self.polls += 1;
        self.means.add(ceilings, &self.cur, &self.ratio, &self.busy);
    }

    /// Датчики не читались (держит другая часть `CoreMap` или ошибка): показаний нет, наибольшие и средние остаются.
    pub fn blank(&mut self) {
        self.cur.fill(None);
        self.ratio.fill(None);
        self.busy.fill(None);
        self.pkg = None;
        self.polls += 1;
    }

    /// «Сбросить максимумы»: наибольшие — с нынешних, средние — заново (Monitor.ResetMax и Core.ResetAverage).
    pub fn reset(&mut self) {
        self.max.clone_from(&self.cur);
        self.pkg_max = self.pkg;
        self.since = Instant::now();
        self.means.reset();
    }

    #[must_use]
    pub fn avg(&self) -> &[Option<f64>] {
        &self.means.avg
    }

    #[must_use]
    pub fn avg_ratio(&self) -> &[Option<f64>] {
        &self.means.avg_ratio
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn reading(temp: f64, busy: f64, ratio: Option<f64>) -> Reading {
        Reading {
            temp: Some(temp),
            busy: Some(busy),
            ratio,
        }
    }

    #[test]
    fn drain_equals_one_long_read() {
        // Four half-second reads: busy 1, 0, 1, 0 and ratio 50 / none / 40 / none. One read over the same 2 s
        // would see busy 0.5 and the ratio averaged over the running time: (50 + 40) / 2 = 45.
        let mut meter = Meter::new(1);
        let t0 = Instant::now();
        meter.record(0, Reading::default(), t0);
        meter.record(0, reading(70.0, 1.0, Some(50.0)), t0 + Duration::from_millis(500));
        meter.record(0, reading(68.0, 0.0, None), t0 + Duration::from_millis(1000));
        meter.record(0, reading(72.0, 1.0, Some(40.0)), t0 + Duration::from_millis(1500));
        meter.record(0, reading(66.0, 0.0, None), t0 + Duration::from_millis(2000));
        let r = meter.drain(0);
        assert!((r.busy.unwrap_or(f64::NAN) - 0.5).abs() < 1e-9);
        assert!((r.ratio.unwrap_or(f64::NAN) - 45.0).abs() < 1e-9);
        assert_eq!(r.temp, Some(66.0), "temperature is the latest, not a mean");
        assert!(!meter.has(0), "a drain starts the sum afresh");
        assert_eq!(meter.drain(0).busy, None);
    }

    #[test]
    fn ratio_is_weighted_by_running_time_not_by_reads() {
        // 1.5 s at busy 1 and ratio 50, then 0.5 s at busy 0.2 and ratio 30: running time 1.5 vs 0.1 s.
        let mut meter = Meter::new(1);
        let t0 = Instant::now();
        meter.record(0, Reading::default(), t0);
        meter.record(0, reading(70.0, 1.0, Some(50.0)), t0 + Duration::from_millis(1500));
        meter.record(0, reading(70.0, 0.2, Some(30.0)), t0 + Duration::from_millis(2000));
        let r = meter.drain(0);
        let expected = (50.0 * 1.5 + 30.0 * 0.1) / 1.6;
        assert!((r.ratio.unwrap_or(f64::NAN) - expected).abs() < 1e-9);
    }

    fn poll(watch: &mut Watch, ceilings: &[i32], temps: &[f64], busy: &[f64]) {
        for (i, (&t, &b)) in temps.iter().zip(busy).enumerate() {
            watch.core(i, reading(t, b, (b > 0.05).then_some(45.0)));
        }
        watch.polled(ceilings);
    }

    #[test]
    fn mean_counts_only_the_core_s_own_load_after_warming() {
        let mut watch = Watch::new(2);
        let ceilings = [50, 46];
        for _ in 0..WARM {
            poll(&mut watch, &ceilings, &[80.0, 50.0], &[1.0, 0.0]);
        }
        assert_eq!(watch.avg(), [None, None], "the first 2 s under load do not count");
        poll(&mut watch, &ceilings, &[90.0, 50.0], &[1.0, 0.0]);
        poll(&mut watch, &ceilings, &[70.0, 50.0], &[1.0, 0.0]);
        assert_eq!(watch.avg()[0], Some(80.0));
        assert_eq!(watch.avg_ratio()[0], Some(45.0));
        assert_eq!(watch.avg()[1], None, "an idle core has no mean under load");
        // Idle for a while: the mean stays; loaded again: warming first, then the same sum goes on.
        poll(&mut watch, &ceilings, &[40.0, 50.0], &[0.1, 0.0]);
        assert_eq!(watch.avg()[0], Some(80.0));
        for _ in 0..WARM {
            poll(&mut watch, &ceilings, &[99.0, 50.0], &[1.0, 0.0]);
        }
        assert_eq!(watch.avg()[0], Some(80.0));
        poll(&mut watch, &ceilings, &[83.0, 50.0], &[1.0, 0.0]);
        assert_eq!(watch.avg()[0], Some(81.0));
    }

    #[test]
    fn new_ceilings_start_afresh_and_reset_clears_everything() {
        let mut watch = Watch::new(2);
        for _ in 0..=WARM {
            poll(&mut watch, &[50, 46], &[80.0, 60.0], &[1.0, 1.0]);
        }
        assert_eq!(watch.avg(), [Some(80.0), Some(60.0)]);
        // Core 1 idles, then the ceilings change: the loaded core loses its mean, the idle one keeps its last.
        poll(&mut watch, &[49, 46], &[78.0, 45.0], &[1.0, 0.0]);
        assert_eq!(watch.avg(), [None, Some(60.0)]);
        watch.reset();
        assert_eq!(watch.avg(), [None, None]);
        assert_eq!(watch.max, watch.cur, "the maxima start from what stands now");
    }
}
