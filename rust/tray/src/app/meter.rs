// Показания ядер, когда их читает один опрос, а берут несколько: регулятор и журнал срезов (раз в 0,25 с,
// app/sense.rs), окно (раз в полсекунды) и наибольшие и средние плиток.
//
// Доля работы и фактический множитель — по приращениям счётчиков ядра между двумя чтениями (machine::thermal), и
// счётчики у ядра одни. Последнее показание каждого ядра (`Meter::latest`) — показание опроса срезов: его берут
// регулятор автомодуля и строка среза. Каждое чтение идёт ещё в `Kept` — наибольшие и средние под нагрузкой плиток, по
// блоку и потолку, — и, если окно подключено, в `Watch` (app/sense.rs, `record`): одно чтение — один раз каждому.
//
// `Watch` — что видит окно из своего опроса раз в полсекунды: доля работы и множитель — за полсекунды с прошлого
// опроса окна, кто бы ни читал ядро в это время (ревизия 15.10, п. 1). `Kept` копит из всех чтений, открыто окно или
// нет, и переживает перезапуск программы (владелец 16.10).

use std::time::{Duration, Instant};

use machine::clock::Stamp;
use machine::thermal::Reading;
use machine::topology::Block;
use serde_json::{Map, Value, json};

/// С какой доли работы ядро «нагружено» для окна: признак нагрузки в состоянии и средняя под нагрузкой. Это не правило
/// автомодуля (у его отсечек условий нагрузки нет, бриф T): нагрузку теперь даёт игра, а не кнопки программы, и
/// «нагружено» — ядро работает хотя бы половину времени (прежде это число было и у автомодуля).
pub const BUSY: f64 = 0.5;

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

/// Чтения ядер: когда каждое ядро прочитано последний раз и что дало.
#[derive(Debug, Clone)]
pub struct Meter {
    last: Vec<Option<Instant>>,
    latest: Vec<Reading>,
    /// Сколько ядер прочитано всего — скрытая проверка видит, не читает ли программа без окна и не читает ли дважды.
    pub reads: u64,
}

impl Meter {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            last: vec![None; cores],
            latest: vec![Reading::default(); cores],
            reads: 0,
        }
    }

    /// Ядро прочитано в `at`: показание покрывает время с его прошлого чтения. Возвращает это время, секунды (None —
    /// первое чтение: сравнивать не с чем); его складывает окно (`Watch::core`).
    pub fn record(&mut self, core: usize, reading: Reading, at: Instant) -> Option<f64> {
        self.reads += 1;
        let slot = self.latest.get_mut(core)?;
        *slot = reading;
        self.last[core]
            .replace(at)
            .map(|t| at.saturating_duration_since(t).as_secs_f64())
    }

    /// Последнее показание ядра как оно прочитано: доля работы и множитель — за промежуток до прошлого чтения.
    #[must_use]
    pub fn latest(&self, core: usize) -> Reading {
        self.latest.get(core).copied().unwrap_or_default()
    }
}

/// Столько секунд подряд блок должен быть нагружен, прежде чем его показания идут в среднюю: он ещё греется
/// (`AverageSkip` в Core.cs — 4 опроса окна по полсекунды, 2 с).
const WARM_SECS: f64 = 2.0;

/// Наибольшая и средние под нагрузкой блока при одном потолке, который у него стоял (владелец 16.10: максимум,
/// достигнутый на ×48, после среза автомодуля до ×47 — не про ×47). Средние — по времени: каждое показание весит
/// промежуток, который оно покрывает (опрос срезов — 0,25 с, опрос окна — 0,5 с).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    ceiling: i32,
    max: Option<f64>,
    /// Когда наибольшая достигнута (мировое время, тики `Stamp`).
    max_at: Option<i64>,
    /// Σ температура × секунды и Σ секунды (в средней).
    temp: f64,
    secs: f64,
    /// Σ множитель × доля работы × секунды и Σ доля работы × секунды (множитель — средний по времени работы).
    ratio: f64,
    run: f64,
}

impl Entry {
    fn new(ceiling: i32) -> Self {
        Self {
            ceiling,
            max: None,
            max_at: None,
            temp: 0.0,
            secs: 0.0,
            ratio: 0.0,
            run: 0.0,
        }
    }

    fn mean(&self) -> Option<f64> {
        (self.secs > 0.0).then(|| self.temp / self.secs)
    }

    fn mean_ratio(&self) -> Option<f64> {
        (self.run > 0.0).then(|| self.ratio / self.run)
    }
}

/// Итог блока на одном потолке — для журнала событий (app/timeline.rs): потолок, секунды в средней, наибольшая и когда
/// она достигнута, средняя.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Period {
    pub ceiling: i32,
    pub secs: f64,
    pub max: Option<f64>,
    pub max_at: Option<Stamp>,
    pub mean: Option<f64>,
}

/// Одна строка «по потолкам» блока для окна (поле byCeiling состояния).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtCeiling {
    pub ceiling: i32,
    pub max: Option<f64>,
    pub mean: Option<f64>,
    pub secs: f64,
}

/// Наибольшие и средние под нагрузкой плиток — по блоку и по каждому потолку, который у него стоял с последнего сброса
/// (владелец 16.10). Копятся из каждого показания, которое читает программа (`app/sense.rs`, `record`): опроса срезов,
/// пока включён автомодуль или журнал, и опроса окна, — открыто окно или нет (владелец играет без окна). У блока
/// копит его первое ядро: у кластера E-ядер датчик температуры общий, P-блок — само ядро; один владелец у каждого числа,
/// и показание, которое приходит один раз, считается один раз. Оба выключателя выключены и окна нет — ничего не
/// читается и ничего не копится.
///
/// Поля плитки (`max`, `avg`, `avg_ratio`, по ядрам — всем ядрам блока одно) — значения потолка, который стоит сейчас (с
/// поправкой автомодуля); прежние — списком `by_ceiling`. Срез и возврат автомодуля только переключают, какой потолок
/// нынешний. Список блока сбрасывается, когда владелец меняет его потолок (профиль, плитка; `align`), и весь — кнопкой
/// сброса. Средняя — правило Core.Average в C#: считается, только пока блок нагружен (доля работы не меньше `BUSY`), и
/// только после `WARM_SECS` нагрузки подряд.
///
/// Всё это переживает перезапуск программы (владелец 16.10: «сохранять их пока я не нажму reset вручную»): data\maxima.json
/// (`to_json`, `restore`; когда — app/serve.rs).
#[derive(Debug, Clone)]
pub struct Kept {
    /// По ядрам: номер блока, если ядро — первое ядро блока; его показания копятся.
    first_of: Vec<Option<usize>>,
    /// По блокам: первое ядро и все ядра (поля плитки — всем им).
    firsts: Vec<usize>,
    members: Vec<Vec<usize>>,
    /// По блокам: потолок владельца, при котором копится список; None — ещё не известен.
    owner: Vec<Option<i32>>,
    /// По блокам: потолок, который стоит сейчас (с поправкой автомодуля); None — не известен, показания не копятся.
    current: Vec<Option<i32>>,
    /// По блокам: значения по потолкам, по убыванию потолка.
    entries: Vec<Vec<Entry>>,
    /// По блокам: сколько секунд подряд блок нагружен.
    loaded_for: Vec<f64>,
    pub pkg_max: Option<f64>,
    /// Поля плитки по ядрам — значения нынешнего потолка их блока (обновляются при каждом изменении).
    pub max: Vec<Option<f64>>,
    avg: Vec<Option<f64>>,
    avg_ratio: Vec<Option<f64>>,
    /// С какого времени считаются (мировое время: переживает перезапуск).
    since: Stamp,
    /// Наибольшая выросла или блок сброшен: файл надо переписать.
    changed: bool,
}

impl Kept {
    #[must_use]
    pub fn new(blocks: &[Block], cores: usize) -> Self {
        let mut first_of = vec![None; cores];
        for (b, block) in blocks.iter().enumerate() {
            if let Some(slot) = first_of.get_mut(block.first) {
                *slot = Some(b);
            }
        }
        Self {
            first_of,
            firsts: blocks.iter().map(|b| b.first).collect(),
            members: blocks.iter().map(|b| b.members.clone()).collect(),
            owner: vec![None; blocks.len()],
            current: vec![None; blocks.len()],
            entries: vec![Vec::new(); blocks.len()],
            loaded_for: vec![0.0; blocks.len()],
            pkg_max: None,
            max: vec![None; cores],
            avg: vec![None; cores],
            avg_ratio: vec![None; cores],
            since: Stamp::now(),
            changed: false,
        }
    }

    /// Значения блока `b` при потолке `ceiling` — есть или новые (список держится по убыванию потолка).
    fn entry(&mut self, b: usize, ceiling: i32) -> &mut Entry {
        let list = &mut self.entries[b];
        let at = match list.iter().position(|e| e.ceiling <= ceiling) {
            Some(at) if list[at].ceiling == ceiling => at,
            Some(at) => {
                list.insert(at, Entry::new(ceiling));
                at
            }
            None => {
                list.push(Entry::new(ceiling));
                list.len() - 1
            }
        };
        &mut list[at]
    }

    /// Поля плитки ядер блока `b` — заново из значений нынешнего потолка.
    fn show(&mut self, b: usize) {
        let now = self.current[b]
            .and_then(|c| self.entries[b].iter().find(|e| e.ceiling == c))
            .copied();
        for &m in &self.members[b] {
            if m < self.max.len() {
                self.max[m] = now.and_then(|e| e.max);
                self.avg[m] = now.as_ref().and_then(Entry::mean);
                self.avg_ratio[m] = now.as_ref().and_then(Entry::mean_ratio);
            }
        }
    }

    /// Потолки сейчас, по ядрам: владельца (`intended`) и стоящие в процессоре (`current`, с поправкой автомодуля).
    /// Блок, у которого потолок владельца другой, чем при накоплении, теряет весь список — владелец сменил ему потолок
    /// (профиль, плитка, принятая чужая правка); не был известен — запоминается. Стоящий потолок выбирает, куда копить и
    /// что на плитке. true — что-то сброшено.
    pub fn align(&mut self, intended: &[i32], current: &[i32]) -> bool {
        let mut dropped = false;
        for b in 0..self.firsts.len() {
            let first = self.firsts[b];
            if let Some(now) = intended.get(first).copied() {
                let was = self.owner[b].replace(now);
                if was.is_some_and(|w| w != now) {
                    self.entries[b].clear();
                    self.loaded_for[b] = 0.0;
                    dropped = true;
                }
            }
            self.current[b] = current.get(first).copied();
            self.show(b);
        }
        self.changed |= dropped;
        dropped
    }

    /// Показание ядра (`seconds` — промежуток, который оно покрывает, `Meter::record`): у первого ядра блока — в
    /// значения нынешнего потолка блока; у остальных ядер — ничего (их блок копит первое).
    pub fn read(&mut self, core: usize, reading: Reading, seconds: Option<f64>) {
        let Some(b) = self.first_of.get(core).copied().flatten() else {
            return;
        };
        let Some(ceiling) = self.current[b] else {
            return;
        };
        if let Some(t) = reading.temp {
            let entry = self.entry(b, ceiling);
            if entry.max.is_none_or(|m| t > m) {
                entry.max = Some(t);
                entry.max_at = Some(Stamp::now().0);
                self.changed = true;
                self.show(b);
            }
        }
        let (Some(dt), Some(busy)) = (seconds, reading.busy) else {
            return;
        };
        if busy < BUSY {
            self.loaded_for[b] = 0.0;
            return;
        }
        let warmed = self.loaded_for[b] >= WARM_SECS;
        self.loaded_for[b] += dt;
        if !warmed {
            return;
        }
        let entry = self.entry(b, ceiling);
        if let Some(t) = reading.temp {
            entry.temp += t * dt;
            entry.secs += dt;
        }
        if let Some(r) = reading.ratio {
            entry.ratio += r * busy * dt;
            entry.run += busy * dt;
        }
        self.show(b);
    }

    /// Корпус прочитан: наибольшая корпуса.
    pub fn package(&mut self, pkg: Option<f64>) {
        if let Some(p) = pkg
            && self.pkg_max.is_none_or(|m| p > m)
        {
            self.pkg_max = Some(p);
            self.changed = true;
        }
    }

    /// «Сбросить максимумы»: списки — пустые, наибольшие нынешних потолков — с последних показаний (`latest`, по первому
    /// ядру блока), наибольшая корпуса — со следующего чтения, средние — заново (Monitor.ResetMax и Core.ResetAverage).
    pub fn reset(&mut self, latest: impl Fn(usize) -> Option<f64>) {
        for b in 0..self.firsts.len() {
            self.entries[b].clear();
            self.loaded_for[b] = 0.0;
            if let (Some(ceiling), Some(t)) = (self.current[b], latest(self.firsts[b])) {
                let entry = self.entry(b, ceiling);
                entry.max = Some(t);
                entry.max_at = Some(Stamp::now().0);
            }
            self.show(b);
        }
        self.pkg_max = None;
        self.since = Stamp::now();
        self.changed = true;
    }

    /// Секунд с последнего сброса (поле «since» состояния окна).
    #[must_use]
    pub fn since_secs(&self) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "100-ns ticks of a span of days: far inside f64's exact range"
        )]
        let ticks = Stamp::now().0.saturating_sub(self.since.0).max(0) as f64;
        ticks / 1e7
    }

    /// Надо ли переписать файл, — и с этого момента не надо (вызывающий его пишет).
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    #[must_use]
    pub fn avg(&self) -> &[Option<f64>] {
        &self.avg
    }

    #[must_use]
    pub fn avg_ratio(&self) -> &[Option<f64>] {
        &self.avg_ratio
    }

    /// Итог блока `block` на потолке `ceiling` — что он там накопил с последнего сброса (пусто — ничего).
    #[must_use]
    pub fn period(&self, block: usize, ceiling: i32) -> Period {
        let entry = self
            .entries
            .get(block)
            .and_then(|list| list.iter().find(|e| e.ceiling == ceiling));
        Period {
            ceiling,
            secs: entry.map_or(0.0, |e| e.secs),
            max: entry.and_then(|e| e.max),
            max_at: entry.and_then(|e| e.max_at).map(Stamp),
            mean: entry.and_then(Entry::mean),
        }
    }

    /// Значения блока по потолкам, по убыванию потолка (подсказка плитки в окне).
    pub fn by_ceiling(&self, block: usize) -> impl Iterator<Item = AtCeiling> + '_ {
        self.entries.get(block).into_iter().flatten().map(|e| AtCeiling {
            ceiling: e.ceiling,
            max: e.max,
            mean: e.mean(),
            secs: e.secs,
        })
    }

    /// Текст data\maxima.json: процессор и раскладка (чужой файл не берётся), с какого времени, наибольшая корпуса, по
    /// блокам — первое ядро, потолок владельца, при котором копилось, и значения по потолкам: наибольшая и суммы
    /// средних — чтобы средние копились и после перезапуска.
    #[must_use]
    pub fn to_json(&self, cpu: &str, lps: &[u32]) -> String {
        let blocks: Vec<Value> = (0..self.firsts.len())
            .map(|b| {
                let list: Vec<Value> = self.entries[b]
                    .iter()
                    .map(|e| {
                        json!({
                            "ceiling": e.ceiling, "max": e.max, "maxAt": e.max_at, "temp": e.temp, "secs": e.secs,
                            "ratio": e.ratio, "run": e.run,
                        })
                    })
                    .collect();
                json!({ "first": self.firsts[b], "owner": self.owner[b], "byCeiling": list })
            })
            .collect();
        json!({ "cpu": cpu, "lps": lps, "since": self.since.0, "pkgMax": self.pkg_max, "blocks": blocks }).to_string()
    }

    /// Наибольшие и средние из data\maxima.json. Файл другого процессора, другой раскладки или с другими блоками не
    /// берётся (false); негодная запись — без неё. Отбор по потолкам владельца — `align` после.
    pub fn restore(&mut self, map: &Map<String, Value>, cpu: &str, lps: &[u32]) -> bool {
        let same_lps = map.get("lps").and_then(Value::as_array).is_some_and(|a| {
            a.len() == lps.len() && a.iter().zip(lps).all(|(v, &lp)| v.as_u64() == Some(u64::from(lp)))
        });
        let blocks = map.get("blocks").and_then(Value::as_array);
        let same_blocks = blocks.is_some_and(|list| {
            list.len() == self.firsts.len()
                && list
                    .iter()
                    .zip(&self.firsts)
                    .all(|(v, &first)| v.get("first").and_then(Value::as_u64) == u64::try_from(first).ok())
        });
        let (Some(blocks), true, true) = (blocks, same_blocks, same_lps) else {
            return false;
        };
        if map.get("cpu").and_then(Value::as_str) != Some(cpu) {
            return false;
        }
        if let Some(since) = map.get("since").and_then(Value::as_i64) {
            self.since = Stamp(since);
        }
        self.pkg_max = map.get("pkgMax").and_then(Value::as_f64);
        let number = |e: &Value, name: &str| e.get(name).and_then(Value::as_f64);
        for (b, block) in blocks.iter().enumerate() {
            let Some(owner) = block
                .get("owner")
                .and_then(Value::as_i64)
                .and_then(|c| i32::try_from(c).ok())
            else {
                continue;
            };
            self.owner[b] = Some(owner);
            for e in block.get("byCeiling").and_then(Value::as_array).into_iter().flatten() {
                let ceiling = e
                    .get("ceiling")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                let (Some(ceiling), Some(temp), Some(secs), Some(ratio), Some(run)) = (
                    ceiling,
                    number(e, "temp"),
                    number(e, "secs"),
                    number(e, "ratio"),
                    number(e, "run"),
                ) else {
                    continue;
                };
                let entry = self.entry(b, ceiling);
                *entry = Entry {
                    ceiling,
                    max: number(e, "max"),
                    max_at: e.get("maxAt").and_then(Value::as_i64),
                    temp,
                    secs,
                    ratio,
                    run,
                };
            }
            self.show(b);
        }
        true
    }
}

/// Что видит окно из последнего опроса окна: показания ядер (температура — последняя, доля работы и множитель — за
/// время от позапрошлого опроса) и корпус. Наибольшие и средние — не здесь, а в `Kept`: они копятся и без окна.
#[derive(Debug, Clone)]
pub struct Watch {
    pub cur: Vec<Option<f64>>,
    pub ratio: Vec<Option<f64>>,
    pub busy: Vec<Option<f64>>,
    pub pkg: Option<f64>,
    /// Сколько опросов окна было (скрытая проверка: каждое чтение — в одном опросе).
    pub polls: u64,
    /// Когда был прошлый опрос окна; None — ещё не было.
    polled_at: Option<Instant>,
    /// По ядрам, с прошлого опроса окна: последняя температура, сумма промежутков, было ли чтение.
    temp: Vec<Option<f64>>,
    sums: Vec<Sum>,
    fresh: Vec<bool>,
}

impl Watch {
    #[must_use]
    pub fn new(cores: usize) -> Self {
        Self {
            cur: vec![None; cores],
            ratio: vec![None; cores],
            busy: vec![None; cores],
            pkg: None,
            polls: 0,
            polled_at: None,
            temp: vec![None; cores],
            sums: vec![Sum::default(); cores],
            fresh: vec![false; cores],
        }
    }

    /// Подключилось новое окно: показаний прежнего опроса окна у него нет.
    pub fn new_session(&mut self) {
        self.cur.fill(None);
        self.ratio.fill(None);
        self.busy.fill(None);
        self.pkg = None;
        self.polled_at = None;
        self.temp.fill(None);
        self.sums.fill(Sum::default());
        self.fresh.fill(false);
    }

    /// Ядро прочитано (`seconds` — время с его прошлого чтения, `Meter::record`): в опрос окна.
    pub fn core(&mut self, core: usize, reading: Reading, seconds: Option<f64>) {
        if core >= self.cur.len() {
            return;
        }
        self.temp[core] = reading.temp;
        if let Some(seconds) = seconds {
            self.sums[core].add(seconds, reading);
        }
        self.fresh[core] = true;
    }

    pub fn package(&mut self, pkg: Option<f64>) {
        self.pkg = pkg;
    }

    /// Пора ли опросу окна: прошлый был не ближе `min` (или его не было).
    #[must_use]
    pub fn due(&self, now: Instant, min: Duration) -> bool {
        self.polled_at.is_none_or(|t| now.saturating_duration_since(t) >= min)
    }

    /// Опрос окна в `now`: у ядер, прочитанных с прошлого, — последняя температура, доля работы и множитель за всё
    /// время с него. С прошлого опроса ничего не прочитано — ничего не меняется.
    pub fn polled(&mut self, now: Instant) {
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
        self.fresh.fill(false);
        self.polls += 1;
        self.polled_at = Some(now);
    }

    /// Датчики не читались (держит другая часть `CoreMap` или ошибка): показаний нет.
    pub fn blank(&mut self) {
        self.cur.fill(None);
        self.ratio.fill(None);
        self.busy.fill(None);
        self.pkg = None;
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
        }
    }

    #[test]
    fn a_read_covers_the_time_since_the_last_one_and_stays_as_the_latest() {
        let mut meter = Meter::new(1);
        let t0 = Instant::now();
        assert_eq!(
            meter.record(0, Reading::default(), t0),
            None,
            "the first read has nothing to compare with"
        );
        let read = reading(72.0, 0.2, Some(44.0));
        let seconds = meter.record(0, read, t0 + Duration::from_millis(250));
        assert!(seconds.is_some_and(|s| (s - 0.25).abs() < 1e-9));
        assert_eq!(meter.latest(0), read);
        assert_eq!(meter.reads, 2);
        assert_eq!(
            meter.latest(5),
            Reading::default(),
            "no such core — nothing, not a panic"
        );
        assert_eq!(meter.record(5, read, t0), None);
    }

    #[test]
    fn a_poll_without_a_new_reading_changes_nothing() {
        // Review 15.10, finding 1: a window poll before the next reading must not take the last reading again.
        let mut watch = Watch::new(1);
        watch.core(0, reading(80.0, 1.0, Some(45.0)), Some(0.5));
        watch.polled(Instant::now());
        let (polls, cur) = (watch.polls, watch.cur.clone());
        watch.polled(Instant::now());
        assert_eq!(
            (watch.polls, watch.cur.clone()),
            (polls, cur),
            "a poll with nothing new is no poll"
        );
    }

    #[test]
    fn a_window_poll_takes_every_reading_since_the_last_one() {
        // Review 15.10, finding 1: the first core of a block is read every 0.25 s, the window polls every 0.5 s; the
        // window's load and multiplier are those of its whole half second, not of the last quarter.
        let mut watch = Watch::new(2);
        watch.core(0, reading(90.0, 0.8, Some(50.0)), Some(0.25));
        watch.core(0, reading(82.0, 0.3, Some(40.0)), Some(0.25));
        watch.core(1, reading(60.0, 0.6, Some(30.0)), Some(0.5));
        watch.polled(Instant::now());
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
        assert_eq!(watch.polls, 1);
    }

    #[test]
    fn the_window_poll_is_due_twice_a_second() {
        let mut watch = Watch::new(1);
        let t0 = Instant::now();
        let min = Duration::from_millis(375);
        assert!(watch.due(t0, min), "no poll yet");
        watch.polled(t0);
        assert!(watch.due(t0, min), "nothing was read: that was no poll");
        watch.core(0, reading(60.0, 0.2, None), Some(0.5));
        watch.polled(t0);
        assert!(
            !watch.due(t0 + Duration::from_millis(250), min),
            "the next quarter-second poll is not the window's"
        );
        assert!(watch.due(t0 + Duration::from_millis(500), min), "the one after it is");
    }

    // ----- Kept: per block, per ceiling (owner 16.10) -----

    /// Two blocks: P-core 0 (core 0) and an E-cluster 1 (cores 1, 2).
    fn blocks() -> Vec<Block> {
        let block = |members: Vec<usize>, name: &str| Block {
            first: members[0],
            lp: machine::Lp(u32::try_from(members[0]).unwrap_or(0)),
            kind: if members.len() == 1 {
                machine::topology::Kind::P
            } else {
                machine::topology::Kind::E
            },
            name: name.into(),
            members,
        };
        vec![block(vec![0], "0"), block(vec![1, 2], "1–2")]
    }

    fn kept() -> Kept {
        Kept::new(&blocks(), 3)
    }

    /// `seconds` of quarter-second readings: core 0 at `t0`, busy `b0`, running at its ceiling; cores 1 and 2 idle at
    /// `t1`.
    fn feed(kept: &mut Kept, current: &[i32], seconds: f64, t0: f64, b0: f64, t1: f64) {
        let steps = (0..).take_while(|k| f64::from(*k) * 0.25 < seconds);
        for _ in steps {
            kept.read(0, reading(t0, b0, Some(f64::from(current[0]))), Some(0.25));
            kept.read(1, reading(t1, 0.0, None), Some(0.25));
            kept.read(2, reading(t1, 0.0, None), Some(0.25));
        }
    }

    fn listed(kept: &Kept, block: usize) -> Vec<(i32, Option<f64>, Option<f64>)> {
        kept.by_ceiling(block).map(|e| (e.ceiling, e.max, e.mean)).collect()
    }

    #[test]
    fn the_mean_counts_only_the_block_s_own_load_after_two_seconds_weighted_by_time() {
        let mut k = kept();
        k.align(&[50, 46, 46], &[50, 46, 46]);
        feed(&mut k, &[50, 46, 46], 2.0, 99.0, 1.0, 40.0);
        assert_eq!(k.avg()[0], None, "the first 2 s under load do not count");
        feed(&mut k, &[50, 46, 46], 1.0, 80.0, 1.0, 40.0);
        feed(&mut k, &[50, 46, 46], 0.5, 70.0, 0.1, 40.0);
        assert_eq!(
            k.avg()[0],
            Some(80.0),
            "idle readings neither count nor change the mean"
        );
        assert_eq!(k.avg_ratio()[0], Some(50.0));
        assert_eq!(k.max[0], Some(99.0), "the max counts every reading, warm or not");
        assert_eq!(
            (k.avg()[1], k.avg()[2]),
            (None, None),
            "an idle block has no mean under load"
        );
        assert_eq!(
            (k.max[1], k.max[2]),
            (Some(40.0), Some(40.0)),
            "every core of the block shows the block's"
        );
        let secs: Vec<f64> = k.by_ceiling(0).map(|e| e.secs).collect();
        assert_eq!(secs, [1.0], "one second counted");
    }

    #[test]
    fn only_the_first_core_of_a_block_feeds_it() {
        // A window poll reads every core: the other cores of a block must not count a second time.
        let mut k = kept();
        k.align(&[50, 46, 46], &[50, 46, 46]);
        k.read(2, reading(95.0, 1.0, Some(46.0)), Some(0.25));
        assert_eq!(k.max[1], None, "core 2 is not the block's first core");
        k.read(1, reading(61.0, 0.0, None), Some(0.25));
        assert_eq!((k.max[1], k.max[2]), (Some(61.0), Some(61.0)));
    }

    #[test]
    fn readings_before_the_ceilings_are_known_are_not_kept() {
        let mut k = kept();
        k.read(0, reading(90.0, 1.0, Some(50.0)), Some(0.25));
        assert_eq!(listed(&k, 0), []);
    }

    #[test]
    fn an_auto_cut_switches_the_tile_to_the_new_ceiling_and_keeps_the_old_one_listed() {
        // Owner 16.10: a max reached at ×48 is not the max at ×47. The tile shows the ceiling that stands; a return
        // shows the old values again.
        let mut k = kept();
        k.align(&[48, 46, 46], &[48, 46, 46]);
        feed(&mut k, &[48, 46, 46], 3.0, 80.0, 1.0, 50.0);
        assert_eq!((k.max[0], k.avg()[0]), (Some(80.0), Some(80.0)));
        assert!(!k.align(&[48, 46, 46], &[47, 46, 46]), "a cut drops nothing");
        assert_eq!((k.max[0], k.avg()[0]), (None, None), "nothing read at ×47 yet");
        feed(&mut k, &[47, 46, 46], 1.0, 76.0, 1.0, 50.0);
        assert_eq!(
            (k.max[0], k.avg()[0]),
            (Some(76.0), Some(76.0)),
            "still loaded through the cut: no new warm-up"
        );
        assert_eq!(
            listed(&k, 0),
            [(48, Some(80.0), Some(80.0)), (47, Some(76.0), Some(76.0))],
            "both ceilings, highest first"
        );
        k.align(&[48, 46, 46], &[48, 46, 46]);
        assert_eq!(
            (k.max[0], k.avg()[0]),
            (Some(80.0), Some(80.0)),
            "a return shows ×48 again"
        );
    }

    #[test]
    fn the_period_of_a_ceiling_is_its_final_entry() {
        // The event log writes only the final values of the ceiling a block leaves (owner: «максимум пишется только
        // последний для данной конфигурации»).
        let mut k = kept();
        k.align(&[48, 46, 46], &[48, 46, 46]);
        feed(&mut k, &[48, 46, 46], 2.5, 80.0, 1.0, 50.0);
        feed(&mut k, &[48, 46, 46], 0.5, 84.0, 1.0, 50.0);
        let p = k.period(0, 48);
        assert_eq!((p.ceiling, p.max), (48, Some(84.0)));
        assert!((p.secs - 1.0).abs() < 1e-9, "{p:?}");
        assert!(p.mean.is_some_and(|m| (m - 82.0).abs() < 1e-9), "{p:?}");
        assert!(p.max_at.is_some(), "when the max was reached");
        let none = k.period(0, 47);
        assert_eq!(
            (none.ceiling, none.secs, none.max, none.mean),
            (47, 0.0, None, None),
            "nothing at ×47"
        );
    }

    #[test]
    fn the_owner_s_new_ceiling_clears_that_block_s_list_only_and_reset_clears_all() {
        // Owner 16.10: «если я отредактировал ядро, то максимум по нему от старого множителя не нужен».
        let mut k = kept();
        k.align(&[48, 46, 46], &[48, 46, 46]);
        feed(&mut k, &[48, 46, 46], 3.0, 80.0, 1.0, 60.0);
        assert!(k.take_changed(), "the maxima rose");
        assert!(k.align(&[45, 46, 46], &[45, 46, 46]), "the owner set ×45 for block 0");
        assert_eq!(listed(&k, 0), [], "the whole list of block 0 is gone");
        assert_eq!(listed(&k, 1), [(46, Some(60.0), None)], "block 1 keeps its own");
        assert!(k.take_changed(), "a drop is written");
        assert!(
            !k.align(&[45, 46, 46], &[45, 46, 46]),
            "the same ceilings again drop nothing"
        );
        k.reset(|core| (core == 1).then_some(55.0));
        assert_eq!(listed(&k, 0), []);
        assert_eq!(
            listed(&k, 1),
            [(46, Some(55.0), None)],
            "the max starts from the reading that stands now"
        );
        assert_eq!(k.pkg_max, None);
        assert!(k.take_changed(), "a reset is written");
    }

    #[test]
    fn the_file_round_trips_every_ceiling_and_the_mean_goes_on_accumulating() {
        let lps = [0, 2, 3];
        let mut k = kept();
        k.align(&[48, 46, 46], &[48, 46, 46]);
        feed(&mut k, &[48, 46, 46], 3.0, 80.0, 1.0, 50.0);
        k.align(&[48, 46, 46], &[47, 46, 46]);
        feed(&mut k, &[47, 46, 46], 1.0, 76.0, 1.0, 50.0);
        k.package(Some(84.0));
        let map: Map<String, Value> = serde_json::from_str(&k.to_json("cpu", &lps)).unwrap_or_default();
        let mut again = kept();
        assert!(again.restore(&map, "cpu", &lps));
        assert!(
            !again.align(&[48, 46, 46], &[47, 46, 46]),
            "the same owner's ceilings: everything kept"
        );
        assert_eq!(listed(&again, 0), listed(&k, 0));
        assert_eq!(listed(&again, 1), listed(&k, 1));
        assert_eq!(
            (again.max.clone(), again.avg().to_vec()),
            (k.max.clone(), k.avg().to_vec())
        );
        assert_eq!(again.avg_ratio(), k.avg_ratio());
        assert_eq!(again.pkg_max, Some(84.0));
        assert!(
            (again.since_secs() - k.since_secs()).abs() < 1.0,
            "the reset time is kept"
        );
        // ×47 had 1 s at 76 °C; after the restart the block warms up again for 2 s, then 1 s at 86 °C counts.
        feed(&mut again, &[47, 46, 46], 3.0, 86.0, 1.0, 50.0);
        assert_eq!(
            again.avg()[0],
            Some(81.0),
            "(76 · 1 s + 86 · 1 s) / 2 s: the sums came back"
        );
    }

    #[test]
    fn at_start_a_block_of_another_owner_s_ceiling_is_dropped_and_another_file_is_not_taken() {
        let lps = [0, 2, 3];
        let mut k = kept();
        k.align(&[48, 46, 46], &[48, 46, 46]);
        feed(&mut k, &[48, 46, 46], 3.0, 80.0, 1.0, 60.0);
        let map: Map<String, Value> = serde_json::from_str(&k.to_json("cpu", &lps)).unwrap_or_default();
        let mut again = kept();
        assert!(again.restore(&map, "cpu", &lps));
        assert!(
            again.align(&[47, 46, 46], &[47, 46, 46]),
            "block 0's ceiling changed while the program was closed"
        );
        assert_eq!(listed(&again, 0), []);
        assert_eq!(listed(&again, 1), [(46, Some(60.0), None)]);
        // Another processor, another layout, other blocks: nothing taken.
        let mut other = kept();
        assert!(!other.restore(&map, "other", &lps));
        assert!(!other.restore(&map, "cpu", &[0, 2, 4]));
        let mut single = Kept::new(&blocks()[..1], 3);
        assert!(!single.restore(&map, "cpu", &lps));
        assert!(listed(&other, 0).is_empty() && listed(&single, 0).is_empty());
    }
}
