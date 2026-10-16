// Показания ядер, когда их читает один опрос, а берут трое: журнал срезов (раз в 0,25 с, app/sense.rs), окно (раз в
// полсекунды) и автомодуль (раз в 2 с).
//
// Доля работы и фактический множитель — по приращениям счётчиков ядра между двумя чтениями (machine::thermal), и
// счётчики у ядра одни. Если бы автомодуль читал ядро сам, пока его читает опрос срезов или окно, его показание
// покрывало бы не 2 с, а то, что осталось с последнего чужого чтения (доли секунды). Поэтому каждое чтение ядра идёт
// сюда (`Meter::record`), а автомодуль забирает сумму за всё время со своего прошлого раза (`Meter::drain`): доля
// работы — среднее по времени, множитель — среднее по времени работы. Это ровно то, что дало бы одно чтение через 2 с:
// приращения счётчиков складываются. Никто больше не читает — автомодуль читает сам, и сумма состоит из одного его
// чтения, как раньше. Последнее показание каждого ядра (`Meter::latest`) — строка среза; последняя температура корпуса
// (`Meter::package_within`) — автомодулю, чтобы не читать её второй раз.
//
// `Watch` — что видит окно. Так же, как автомодуль, окно складывает каждое чтение ядра в свою сумму и забирает её в
// своём опросе раз в полсекунды (`Watch::polled`): доля работы и множитель в состоянии окна и в средней под нагрузкой —
// за полсекунды с прошлого опроса окна, кто бы ни читал ядро в это время. Прежде, пока шёл журнал срезов, окно брало
// последнее показание за 0,25 с — каждое второе, а после начала журнала порой одно и то же дважды (ревизия 15.10,
// п. 1). Наибольшая температура — по каждому чтению сразу: пик между опросами окна не теряется. Средняя под
// нагрузкой — правило Core.Average в C#-части (ниже).

use std::time::{Duration, Instant};

use machine::auto::BUSY;
use machine::thermal::Reading;

/// Сумма показаний одного ядра со времени, когда её забрали в прошлый раз.
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

impl Sum {
    /// Показание за `seconds` с прошлого чтения ядра; без доли работы за этот отрезок известна только температура.
    fn add(&mut self, seconds: f64, reading: Reading) {
        let Some(busy) = reading.busy else { return };
        self.time += seconds;
        self.busy += busy * seconds;
        if let Some(ratio) = reading.ratio {
            self.run += busy * seconds;
            self.ratio += ratio * busy * seconds;
        }
    }

    /// Доля работы (среднее по времени) и множитель (среднее по времени работы) за всё сложенное.
    fn mean(&self) -> (Option<f64>, Option<f64>) {
        (
            (self.time > 0.0).then(|| self.busy / self.time),
            (self.run > 0.0).then(|| self.ratio / self.run),
        )
    }
}

/// Температура корпуса из опроса: когда прочитана и что дала (None — датчик не дал действительного показания).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PackageRead {
    pub value: Option<f64>,
    pub at: Instant,
}

/// Чтения ядер и их суммы для автомодуля.
#[derive(Debug, Clone)]
pub struct Meter {
    last: Vec<Option<Instant>>,
    latest: Vec<Reading>,
    sums: Vec<Sum>,
    /// Последняя температура корпуса и когда она прочитана.
    package: Option<PackageRead>,
    /// Сколько ядер прочитано всего — скрытая проверка видит, не читает ли программа без окна и не читает ли дважды.
    pub reads: u64,
}

impl Meter {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            last: vec![None; cores],
            latest: vec![Reading::default(); cores],
            sums: vec![Sum::default(); cores],
            package: None,
            reads: 0,
        }
    }

    /// Ядро прочитано в `at`: показание покрывает время с его прошлого чтения. Возвращает это время, секунды (None —
    /// первое чтение: сравнивать не с чем); его же складывает окно (`Watch::core`).
    pub fn record(&mut self, core: usize, reading: Reading, at: Instant) -> Option<f64> {
        self.reads += 1;
        let slot = self.sums.get_mut(core)?;
        let since = self.last[core]
            .replace(at)
            .map(|t| at.saturating_duration_since(t).as_secs_f64());
        self.latest[core] = reading;
        if let Some(seconds) = since {
            slot.add(seconds, reading);
        }
        since
    }

    /// Было ли чтение ядра после прошлого `drain`.
    #[must_use]
    pub fn has(&self, core: usize) -> bool {
        self.sums.get(core).is_some_and(|s| s.time > 0.0)
    }

    /// Последнее показание ядра как оно прочитано: доля работы и множитель — за промежуток до прошлого чтения.
    #[must_use]
    pub fn latest(&self, core: usize) -> Reading {
        self.latest.get(core).copied().unwrap_or_default()
    }

    /// Корпус прочитан в `at`.
    pub fn record_package(&mut self, value: Option<f64>, at: Instant) {
        self.package = Some(PackageRead { value, at });
    }

    /// Температура корпуса, прочитанная не раньше чем `age` назад; None — такой нет (читать самому).
    #[must_use]
    pub fn package_within(&self, age: Duration) -> Option<PackageRead> {
        self.package.filter(|read| read.at.elapsed() <= age)
    }

    /// Показание ядра за всё время с прошлого раза (температура и напряжение — последние) и начать сумму заново.
    pub fn drain(&mut self, core: usize) -> Reading {
        let Some(sum) = self.sums.get_mut(core).map(std::mem::take) else {
            return Reading::default();
        };
        let (busy, ratio) = sum.mean();
        Reading {
            temp: self.latest[core].temp,
            busy,
            ratio,
            volts: self.latest[core].volts,
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

    /// Опрос окна при этих потолках. В счёт — только ядра, прочитанные с прошлого опроса (`fresh`): прежнее показание
    /// второй раз не считается.
    fn add(
        &mut self,
        ceilings: &[i32],
        cur: &[Option<f64>],
        ratio: &[Option<f64>],
        busy: &[Option<f64>],
        fresh: &[bool],
    ) {
        if self.key != ceilings {
            ceilings.clone_into(&mut self.key);
            self.restart(busy);
        }
        for i in 0..self.streak.len() {
            if !fresh.get(i).copied().unwrap_or(false) {
                continue;
            }
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

/// Что видит окно: показания ядер на прошлый опрос окна (температура — последняя, доля работы и множитель — за время
/// от позапрошлого опроса), корпус, наибольшие с последнего сброса, средние под нагрузкой.
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
    /// Сколько опросов окна дошло до средних под нагрузкой (скрытая проверка: каждое чтение — в одном опросе).
    pub polls: u64,
    /// Когда был прошлый опрос окна; None — ещё не было.
    polled_at: Option<Instant>,
    /// По ядрам, с прошлого опроса окна: последняя температура, сумма промежутков, было ли чтение.
    temp: Vec<Option<f64>>,
    sums: Vec<Sum>,
    fresh: Vec<bool>,
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
            polled_at: None,
            temp: vec![None; cores],
            sums: vec![Sum::default(); cores],
            fresh: vec![false; cores],
            means: Means::new(cores),
        }
    }

    /// Ядро прочитано (`seconds` — время с его прошлого чтения, `Meter::record`): наибольшая — сразу, остальное — в
    /// опрос окна.
    pub fn core(&mut self, core: usize, reading: Reading, seconds: Option<f64>) {
        if core >= self.cur.len() {
            return;
        }
        if let Some(t) = reading.temp {
            self.max[core] = Some(self.max[core].map_or(t, |m| m.max(t)));
        }
        self.temp[core] = reading.temp;
        if let Some(seconds) = seconds {
            self.sums[core].add(seconds, reading);
        }
        self.fresh[core] = true;
    }

    pub fn package(&mut self, pkg: Option<f64>) {
        self.pkg = pkg;
        if let Some(p) = pkg {
            self.pkg_max = Some(self.pkg_max.map_or(p, |m| m.max(p)));
        }
    }

    /// Пора ли опросу окна: прошлый был не ближе `min` (или его не было).
    #[must_use]
    pub fn due(&self, now: Instant, min: Duration) -> bool {
        self.polled_at.is_none_or(|t| now.saturating_duration_since(t) >= min)
    }

    /// Опрос окна в `now` при этих потолках: у ядер, прочитанных с прошлого, — последняя температура, доля работы и
    /// множитель за всё время с него; затем средние под нагрузкой. С прошлого опроса ничего не прочитано — ничего не
    /// меняется.
    pub fn polled(&mut self, ceilings: &[i32], now: Instant) {
        if !self.fresh.contains(&true) {
            return;
        }
        for i in 0..self.cur.len() {
            if !self.fresh[i] {
                continue;
            }
            let (busy, ratio) = std::mem::take(&mut self.sums[i]).mean();
            self.cur[i] = self.temp[i];
            self.busy[i] = busy;
            self.ratio[i] = ratio;
        }
        self.means
            .add(ceilings, &self.cur, &self.ratio, &self.busy, &self.fresh);
        self.fresh.fill(false);
        self.polls += 1;
        self.polled_at = Some(now);
    }

    /// Датчики не читались (держит другая часть `CoreMap` или ошибка): показаний нет, наибольшие и средние остаются.
    pub fn blank(&mut self) {
        self.cur.fill(None);
        self.ratio.fill(None);
        self.busy.fill(None);
        self.pkg = None;
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
    use super::*;

    fn reading(temp: f64, busy: f64, ratio: Option<f64>) -> Reading {
        Reading {
            temp: Some(temp),
            busy: Some(busy),
            ratio,
            volts: None,
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
    fn the_row_takes_the_last_interval_and_the_auto_module_the_sum() {
        // Two reads 0.25 s apart: the samples row shows the second read as it is (its own interval), the auto module's
        // drain sums both intervals — one does not disturb the other.
        let mut meter = Meter::new(1);
        let t0 = Instant::now();
        meter.record(0, Reading::default(), t0);
        meter.record(0, reading(70.0, 1.0, Some(50.0)), t0 + Duration::from_millis(250));
        meter.record(0, reading(72.0, 0.2, Some(44.0)), t0 + Duration::from_millis(500));
        assert_eq!(meter.latest(0), reading(72.0, 0.2, Some(44.0)));
        let sum = meter.drain(0);
        assert!((sum.busy.unwrap_or(f64::NAN) - 0.6).abs() < 1e-9, "{sum:?}");
        assert_eq!(
            meter.latest(0),
            reading(72.0, 0.2, Some(44.0)),
            "a drain leaves the latest read"
        );
        assert_eq!(
            meter.latest(5),
            Reading::default(),
            "no such core — nothing, not a panic"
        );
    }

    #[test]
    fn a_package_read_is_reused_only_while_fresh() {
        let mut meter = Meter::new(1);
        assert!(
            meter.package_within(Duration::from_secs(1)).is_none(),
            "nothing read yet"
        );
        meter.record_package(Some(80.0), Instant::now());
        assert_eq!(
            meter.package_within(Duration::from_secs(1)).map(|p| p.value),
            Some(Some(80.0))
        );
        let old = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("the machine has been up for 2 s");
        meter.record_package(None, old);
        assert!(
            meter.package_within(Duration::from_secs(1)).is_none(),
            "2 s old is not fresh for a 1 s age"
        );
        assert_eq!(
            meter.package_within(Duration::from_secs(3)).map(|p| p.value),
            Some(None),
            "an invalid reading is still a reading"
        );
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

    /// One window poll: every core read once over the last half second.
    fn poll(watch: &mut Watch, ceilings: &[i32], temps: &[f64], busy: &[f64]) {
        for (i, (&t, &b)) in temps.iter().zip(busy).enumerate() {
            watch.core(i, reading(t, b, (b > 0.05).then_some(45.0)), Some(0.5));
        }
        watch.polled(ceilings, Instant::now());
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
    fn a_poll_without_a_new_reading_adds_nothing() {
        // Review 15.10, finding 1: a window poll that comes before the next reading must not count the last reading
        // a second time (it did when the samples log started between two window polls).
        let mut watch = Watch::new(1);
        for _ in 0..=WARM {
            poll(&mut watch, &[50], &[80.0], &[1.0]);
        }
        assert_eq!(watch.avg()[0], Some(80.0));
        watch.core(0, reading(90.0, 1.0, Some(45.0)), Some(0.5));
        watch.polled(&[50], Instant::now());
        assert_eq!(watch.avg()[0], Some(85.0));
        let polls = watch.polls;
        watch.polled(&[50], Instant::now());
        assert_eq!(watch.avg()[0], Some(85.0), "the same reading counted twice");
        assert_eq!(watch.polls, polls, "a poll with nothing new is no poll");
    }

    #[test]
    fn a_window_poll_takes_every_reading_since_the_last_one() {
        // Review 15.10, finding 1: while the samples log runs, the first core of a block is read every 0.25 s and the
        // window polls every 0.5 s. The window's load and multiplier must be those of its whole half second (as when it
        // read the core itself), not of the last quarter: 0.8 then 0.3 is 0.55 — loaded; the last read alone (0.3)
        // is not. Core 1 stands for a non-first E core, read once per window poll.
        let mut watch = Watch::new(2);
        for _ in 0..=WARM {
            watch.core(0, reading(90.0, 0.8, Some(50.0)), Some(0.25));
            watch.core(0, reading(82.0, 0.3, Some(40.0)), Some(0.25));
            watch.core(1, reading(60.0, 0.6, Some(30.0)), Some(0.5));
            watch.polled(&[50, 46], Instant::now());
        }
        assert!(
            (watch.busy[0].unwrap_or(f64::NAN) - 0.55).abs() < 1e-9,
            "{:?}",
            watch.busy
        );
        let ratio = (50.0 * 0.8 + 40.0 * 0.3) / (0.8 + 0.3);
        assert!(
            (watch.ratio[0].unwrap_or(f64::NAN) - ratio).abs() < 1e-9,
            "{:?}",
            watch.ratio
        );
        assert_eq!(
            watch.cur,
            [Some(82.0), Some(60.0)],
            "the temperature is the latest read"
        );
        assert_eq!(watch.max[0], Some(90.0), "a peak between window polls is kept");
        assert_eq!(
            watch.avg(),
            [Some(82.0), Some(60.0)],
            "both loaded over their half second"
        );
        assert!((watch.avg_ratio()[0].unwrap_or(f64::NAN) - ratio).abs() < 1e-9);
        assert_eq!(watch.polls, u64::from(WARM) + 1);
    }

    #[test]
    fn the_window_poll_is_due_twice_a_second() {
        let mut watch = Watch::new(1);
        let t0 = Instant::now();
        let min = Duration::from_millis(375);
        assert!(watch.due(t0, min), "no poll yet");
        watch.polled(&[50], t0);
        assert!(watch.due(t0, min), "nothing was read: that was no poll");
        watch.core(0, reading(60.0, 0.2, None), Some(0.5));
        watch.polled(&[50], t0);
        assert!(
            !watch.due(t0 + Duration::from_millis(250), min),
            "the next quarter-second poll is not the window's"
        );
        assert!(watch.due(t0 + Duration::from_millis(500), min), "the one after it is");
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
