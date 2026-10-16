// Статистика из журналов программы входа (DECISIONS #32): срезы data\samples\<дата>.csv и журнал действий автомодуля
// data\auto-log.jsonl. Здесь только разбор и счёт — без окна и без потоков: что и когда читать, решает store.rs, как
// показать — panels.rs.
//
// Срезы пишутся, пока работает автомодуль и процессор занят: до 0.5.2 — строка раз в 2 с, время до секунды; с 0.5.2 —
// раз в 0,25 с, время до миллисекунды (выбор владельца 15.10, вариант А), обороты — только в строке, где прочитаны
// (раз в 2 с). В одном файле бывают оба вида (день, когда поставлена новая сборка), поэтому вес строки — время, которое
// она покрывает, по её времени и времени прошлой строки (`span_ms`), а не «строки × 2 с».
//
// Что считается (слова владельца 15.10: «какую температуру на каких множителях, на какой нагрузке… с каким-нибудь
// показанием вентилятора»; «если два ядра работают вместе они греются например сильнее»; о нагрузке: «зачем вот эта
// градация… давай использовать от 0 до 100»):
// • по блоку — корзины по фактическому множителю (округлённому до целого) из каждой строки, где множитель и
//   температура известны, при любой нагрузке: время, средняя температура и средняя доля работы (по времени),
//   наибольшая температура со строкой, где она была (множитель, доля работы, время), средние обороты вентилятора;
// • по паре блоков A, B — температура A, когда он занят (доля работы ≥ BUSY), отдельно «B тоже занят» и «B свободен»,
//   в той же корзине множителя A: так разница не смешивается с тем, что при работе соседа A чаще стоит на другом
//   множителе. Это сравнение — как было (граница «занят» здесь остаётся).
// • по блоку и корзине — та же сводка в двух видах (статистика окна, владелец 15.10: «кривые максимальных температур и
//   кривые средних температур так сильно расходятся»): абсолютная температура и температура над остальным кристаллом
//   (`above_die`; аналитик 15.10, outputs/thermal-model-2026-10-15.md: 68–96 % температуры любого блока — общий уровень
//   кристалла, корзина множителя по абсолютной температуре говорит в основном о том, когда она набралась); у каждого
//   вида — гистограмма по времени (`Hist`), из неё 95-й процентиль вместо наибольшей одной строки.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{Map, Value};

/// Блок занят — доля работы не меньше этой (та же граница, что у автомодуля: `machine::auto::BUSY`). Только для
/// сравнения с соседом: корзины графика берут строки при любой нагрузке.
pub const BUSY: f64 = 0.5;
/// Множители ×0…×80: больше у этих процессоров не бывает (machine: заводской вне 8..80 — не ответ процессора).
pub const BINS: usize = 81;
const TOP: f64 = 80.0;
/// Сколько секунд работы нужно в корзине с каждой стороны, чтобы сравнивать «вместе» и «порознь» (было «5 срезов» —
/// те же 10 с при прежнем шаге 2 с).
pub const MIN_PAIR: f64 = 10.0;
/// Шаг записи срезов, мс: прежний (время строки до секунды) и с 0.5.2 (время с долями секунды).
pub const OLD_STEP_MS: i64 = 2000;
pub const STEP_MS: i64 = 250;

/// Время строки среза: миллисекунды от 1970-01-01 по местным часам (как их пишет программа входа) и шаг, которым она
/// записана (по виду времени).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowTime {
    pub ms: i64,
    pub step: i64,
}

/// «ГГГГ-ММ-ДД чч:мм:сс» (прежние срезы, шаг 2 с) или «ГГГГ-ММ-ДД чч:мм:сс.ммм» (с 0.5.2, шаг 0,25 с; долей секунды
/// может быть от 1 до 9 знаков, берутся миллисекунды). Другое — None: строка не в счёт.
pub fn row_time(text: &str) -> Option<RowTime> {
    let number = |at: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(at)?;
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let b = text.as_bytes();
    let marks = [(4, b'-'), (7, b'-'), (10, b' '), (13, b':'), (16, b':')];
    if b.len() < 19 || marks.iter().any(|&(i, c)| b[i] != c) {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let (millis, step) = match &text[19..] {
        "" => (0, OLD_STEP_MS),
        rest => {
            let digits = rest.strip_prefix('.')?;
            if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // Первые три знака — миллисекунды («.5» — 500, «.25» — 250, «.1234» — 123).
            let mut ms = 0;
            for k in 0..3 {
                ms = ms * 10 + digits.as_bytes().get(k).map_or(0, |d| i64::from(d - b'0'));
            }
            (ms, STEP_MS)
        }
    };
    let days = days_from_civil(year, month, day);
    Some(RowTime {
        ms: ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis,
        step,
    })
}

/// Сколько времени покрывает строка, мс: промежуток до прошлой строки того же файла, если он не длиннее полутора шагов
/// записи (таймеры Windows дрожат, время старых строк — до секунды); иначе — первая строка, перерыв записи (процессор
/// был свободен, программа не работала) или часы назад — один шаг: перерыв в счёт не идёт.
pub fn span_ms(previous: Option<i64>, at: RowTime) -> i64 {
    match previous.map(|p| at.ms - p) {
        Some(gap) if gap > 0 && gap * 2 <= at.step * 3 => gap,
        _ => at.step,
    }
}

/// Ячейка гистограммы абсолютной температуры, °C: датчики ядер отдают целые градусы (шагом 2 °C), поэтому ячейка в
/// 1 °C ничего не огрубляет — процентиль по ней точный.
pub const ABS_CELL: f64 = 1.0;
/// Ячейка гистограммы температуры над кристаллом, °C. Её шаг в данных — 2 °C / число остальных блоков (≈ 0,18 °C при
/// одиннадцати): ячейка того же порядка не огрубляет то, что различают датчики, ошибка процентиля — не больше половины
/// ячейки (0,125 °C: 2 px на графике высотой 700 px, меньше пикселя при 1280×800); мельче — гистограммы растут, а
/// точности у датчика с шагом 2 °C не прибавляется. Память: ≈ 120 ячеек по 4 байта на корзину множителя.
pub const ABOVE_CELL: f64 = 0.25;
/// Дальше этого от нуля, °C, — не температура, а испорченная строка: ячеек под неё гистограмма не заводит (средняя и
/// наибольшая её всё равно покажут — их не трогаем).
const HIST_LIMIT: f64 = 200.0;
/// 95-й процентиль: доля времени, ниже которой температура, — 95 из 100.
pub const P95: (u64, u64) = (95, 100);

/// Гистограмма температуры по времени: ячейка — `cell` °C с центром в k · cell, в ячейке — миллисекунды, которые
/// покрывают строки с такой температурой (вес строки — её время, как у средней: строка прежнего журнала — 2 с, нового —
/// 0,25 с). Хранятся только ячейки от наименьшей занятой до наибольшей.
#[derive(Clone, Debug, PartialEq)]
pub struct Hist {
    cell: f64,
    first: i32,
    ms: Vec<u32>,
}

impl Hist {
    #[must_use]
    pub fn new(cell: f64) -> Hist {
        Hist {
            cell,
            first: 0,
            ms: Vec::new(),
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "finite and within ±HIST_LIMIT / cell (±800 cells), so the rounded value fits i32"
    )]
    fn cell_of(&self, value: f64) -> Option<i32> {
        (value.is_finite() && value.abs() <= HIST_LIMIT).then(|| (value / self.cell).round() as i32)
    }

    /// Строка со значением `value` покрывает `ms` миллисекунд.
    pub fn add(&mut self, value: f64, ms: u32) {
        if let Some(cell) = self.cell_of(value) {
            self.put(cell, ms);
        }
    }

    #[expect(
        clippy::cast_sign_loss,
        reason = "offsets are taken only after checking cell >= first, so they are non-negative"
    )]
    fn put(&mut self, cell: i32, ms: u32) {
        if self.ms.is_empty() {
            self.first = cell;
            self.ms.push(0);
        } else if cell < self.first {
            let grow = (self.first - cell) as usize;
            self.ms.splice(0..0, std::iter::repeat_n(0, grow));
            self.first = cell;
        }
        let at = (cell - self.first) as usize;
        if at >= self.ms.len() {
            self.ms.resize(at + 1, 0);
        }
        // Сутки — 86,4 млн мс, предел u32 — 4 295 млн: 49 суток в одной ячейке; насыщение — только страховка.
        self.ms[at] = self.ms[at].saturating_add(ms);
    }

    pub fn merge(&mut self, other: &Hist) {
        debug_assert!(
            self.ms.is_empty() || other.ms.is_empty() || (self.cell - other.cell).abs() < f64::EPSILON,
            "гистограммы одного вида — с одной ячейкой"
        );
        if self.ms.is_empty() {
            self.cell = other.cell;
        }
        for (k, &ms) in other.ms.iter().enumerate() {
            if ms > 0 {
                self.put(other.first + i32::try_from(k).unwrap_or(i32::MAX), ms);
            }
        }
    }

    fn clear(&mut self) {
        self.first = 0;
        self.ms.clear();
    }

    /// Миллисекунд всего.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.ms.iter().map(|&m| u64::from(m)).sum()
    }

    /// Процентиль по времени: наименьшая температура v (центр ячейки), при которой время с температурой не выше v —
    /// не меньше доли `share` = (числитель, знаменатель) всего времени. Для значений на сетке ячеек — точно; иначе — не
    /// дальше половины ячейки от точного. None — гистограмма пуста.
    #[must_use]
    pub fn quantile(&self, share: (u64, u64)) -> Option<f64> {
        let total = self.total();
        if total == 0 || share.1 == 0 {
            return None;
        }
        let mut below = 0u64;
        for (k, &ms) in self.ms.iter().enumerate() {
            below += u64::from(ms);
            if below * share.1 >= total * share.0 {
                return Some(f64::from(self.first + i32::try_from(k).ok()?) * self.cell);
            }
        }
        // Доля больше 1: всё время не выше наибольшей ячейки.
        Some(f64::from(self.first + i32::try_from(self.ms.len() - 1).ok()?) * self.cell)
    }
}

/// Уровень остального кристалла: средняя температура остальных блоков (`sum` — их сумма, `known` — у скольких она в
/// строке известна, `others` — сколько их в файле). None — известна меньше чем у половины остальных (или ни у одного):
/// по нескольким блокам «уровень кристалла» — это они сами, а не кристалл.
#[must_use]
pub fn die_level(sum: f64, known: usize, others: usize) -> Option<f64> {
    #[expect(clippy::cast_precision_loss, reason = "a dozen blocks")]
    let level = sum / known as f64;
    (known >= 1 && known * 2 >= others).then_some(level)
}

/// Температура блока `a` над остальным кристаллом в одной строке: его температура минус средняя остальных блоков,
/// у которых она в этой строке известна. None — у блока температуры нет или остальных известно меньше половины.
/// Правило одной строки, как его задаёт спецификация, — для тестов: счёт строк (`Scratch::row`) применяет то же
/// правило через `die_level`, без второго прохода по блокам.
#[cfg(test)]
#[must_use]
pub fn above_die(temps: &[Option<f64>], a: usize) -> Option<f64> {
    let own = temps.get(a).copied().flatten()?;
    let (sum, known) = temps
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != a)
        .filter_map(|(_, t)| *t)
        .fold((0.0, 0), |(s, k), t| (s + t, k + 1));
    Some(own - die_level(sum, known, temps.len().saturating_sub(1))?)
}

/// Строка в корзине: что о ней помнить.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seen {
    temp: f64,
    ratio: f64,
    busy: Option<f64>,
    secs: f64,
    at: i64,
}

/// Строка наибольшей температуры корзины: температура, фактический множитель, доля работы и время (мс по местным
/// часам от 1970-01-01, `row_time`).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Peak {
    pub temp: f64,
    pub ratio: f64,
    pub busy: Option<f64>,
    pub at: i64,
}

impl Peak {
    /// Выше ли эта, чем `other`: горячее; при равной температуре — позже (итог не зависит от порядка файлов).
    fn above(&self, other: &Peak) -> bool {
        (self.temp, self.at) > (other.temp, other.at)
    }
}

/// Корзина одного множителя.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Bin {
    /// Строк в корзине.
    pub n: u32,
    /// Секунд, которые эти строки покрывают (`span_ms`).
    pub secs: f64,
    /// Температура × секунды: средняя — по времени, а не по числу строк (строка прежнего журнала — 2 с, нового — 0,25 с).
    pub temp_sum: f64,
    /// Доля работы × секунды и секунды, где она известна.
    pub busy_sum: f64,
    pub busy_secs: f64,
    /// Строка с наибольшей температурой.
    pub peak: Peak,
    pub fan_sum: f64,
    pub fan_n: u32,
}

impl Bin {
    fn add(&mut self, seen: Seen, fan: Option<f64>) {
        let peak = Peak {
            temp: seen.temp,
            ratio: seen.ratio,
            busy: seen.busy,
            at: seen.at,
        };
        if self.n == 0 || peak.above(&self.peak) {
            self.peak = peak;
        }
        self.n += 1;
        self.secs += seen.secs;
        self.temp_sum += seen.temp * seen.secs;
        if let Some(busy) = seen.busy {
            self.busy_sum += busy * seen.secs;
            self.busy_secs += seen.secs;
        }
        // Обороты — средняя по строкам, где они прочитаны: и в прежнем журнале, и в новом такая строка — раз в 2 с.
        if let Some(f) = fan {
            self.fan_sum += f;
            self.fan_n += 1;
        }
    }

    fn merge(&mut self, other: &Bin) {
        if other.n == 0 {
            return;
        }
        if self.n == 0 || other.peak.above(&self.peak) {
            self.peak = other.peak;
        }
        self.n += other.n;
        self.secs += other.secs;
        self.temp_sum += other.temp_sum;
        self.busy_sum += other.busy_sum;
        self.busy_secs += other.busy_secs;
        self.fan_sum += other.fan_sum;
        self.fan_n += other.fan_n;
    }

    /// Средняя температура по времени.
    pub fn mean(&self) -> Option<f64> {
        (self.n > 0 && self.secs > 0.0).then(|| self.temp_sum / self.secs)
    }

    /// Средняя доля работы по времени (0…1); None — ни в одной строке не известна.
    pub fn load(&self) -> Option<f64> {
        (self.busy_secs > 0.0).then(|| self.busy_sum / self.busy_secs)
    }

    pub fn fan(&self) -> Option<f64> {
        (self.fan_n > 0).then(|| self.fan_sum / f64::from(self.fan_n))
    }
}

/// Время и сумма температур × секунды.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Side {
    pub secs: f64,
    pub sum: f64,
}

impl Side {
    fn add(&mut self, temp: f64, secs: f64) {
        self.secs += secs;
        self.sum += temp * secs;
    }

    fn merge(&mut self, other: Side) {
        self.secs += other.secs;
        self.sum += other.sum;
    }

    fn mean(self) -> f64 {
        if self.secs > 0.0 { self.sum / self.secs } else { 0.0 }
    }
}

/// Температура блока в корзине его множителя: когда другой блок тоже занят и когда свободен.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Split {
    pub together: Side,
    pub alone: Side,
}

/// Сводка по блоку: корзины по множителю — по абсолютной температуре (`bins`) и над остальным кристаллом (`above`: та
/// же корзина, температура — `above_die`; строки без уровня кристалла в неё не идут), у каждой — гистограмма по
/// времени; и сравнение с каждым другим блоком (по названию).
#[derive(Clone, Default, Debug)]
pub struct BlockTally {
    pub bins: BTreeMap<i32, Bin>,
    pub hist: BTreeMap<i32, Hist>,
    pub above: BTreeMap<i32, Bin>,
    pub above_hist: BTreeMap<i32, Hist>,
    pub with: BTreeMap<String, BTreeMap<i32, Split>>,
}

/// Гистограмму — в словарь корзин: новая корзина — с ячейкой `cell`.
fn merge_hist(into: &mut BTreeMap<i32, Hist>, bin: i32, cell: f64, hist: &Hist) {
    if hist.ms.is_empty() {
        return;
    }
    into.entry(bin).or_insert_with(|| Hist::new(cell)).merge(hist);
}

/// Сводка по строкам срезов: складывается из сводок файлов (`merge`).
#[derive(Clone, Default, Debug)]
pub struct Tally {
    pub rows: u64,
    /// Секунд, которые строки покрывают (`span_ms`).
    pub secs: f64,
    pub first: Option<String>,
    pub last: Option<String>,
    pub blocks: BTreeMap<String, BlockTally>,
}

impl Tally {
    pub fn merge(&mut self, other: &Tally) {
        self.rows += other.rows;
        self.secs += other.secs;
        if let Some(f) = &other.first
            && self.first.as_ref().is_none_or(|mine| f < mine)
        {
            self.first = Some(f.clone()); // once per file merge
        }
        if let Some(l) = &other.last
            && self.last.as_ref().is_none_or(|mine| l > mine)
        {
            self.last = Some(l.clone()); // once per file merge
        }
        for (name, theirs) in &other.blocks {
            let mine = self.blocks.entry(name.clone()).or_default(); // a dozen names per merge
            for (bin, b) in &theirs.bins {
                mine.bins.entry(*bin).or_default().merge(b);
            }
            for (bin, b) in &theirs.above {
                mine.above.entry(*bin).or_default().merge(b);
            }
            for (bin, h) in &theirs.hist {
                merge_hist(&mut mine.hist, *bin, ABS_CELL, h);
            }
            for (bin, h) in &theirs.above_hist {
                merge_hist(&mut mine.above_hist, *bin, ABOVE_CELL, h);
            }
            for (other_name, splits) in &theirs.with {
                let pair = mine.with.entry(other_name.clone()).or_default();
                for (bin, s) in splits {
                    let into = pair.entry(*bin).or_default();
                    into.together.merge(s.together);
                    into.alone.merge(s.alone);
                }
            }
        }
    }
}

/// Насколько блок горячее, когда другой блок тоже занят, чем когда тот свободен: средняя по корзинам множителя,
/// где обеих сторон не меньше `MIN_PAIR` секунд, с весом min(вместе, порознь). None — таких корзин нет. Вместе и
/// порознь — секунды.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effect {
    pub delta: f64,
    pub together: f64,
    pub alone: f64,
}

pub fn effect(splits: &BTreeMap<i32, Split>) -> Option<Effect> {
    let mut weight = 0.0;
    let mut sum = 0.0;
    let (mut together, mut alone) = (0.0, 0.0);
    for s in splits.values() {
        if s.together.secs < MIN_PAIR || s.alone.secs < MIN_PAIR {
            continue;
        }
        let w = s.together.secs.min(s.alone.secs);
        weight += w;
        sum += w * (s.together.mean() - s.alone.mean());
        together += s.together.secs;
        alone += s.alone.secs;
    }
    (weight > 0.0).then(|| Effect {
        delta: sum / weight,
        together,
        alone,
    })
}

// ----- срезы: столбцы и строки -----

/// Где в строке что лежит — по заголовку файла (а не по порядку): файл с другим набором блоков тоже читается.
#[derive(Clone, Debug)]
pub struct Columns {
    width: usize,
    fan_cpu: Option<usize>,
    blocks: Vec<BlockColumns>,
}

#[derive(Clone, Debug)]
struct BlockColumns {
    name: String,
    ratio: Option<usize>,
    busy: Option<usize>,
    temp: Option<usize>,
}

impl Columns {
    /// None — это не заголовок срезов (нет столбца time первым или ни одного блока).
    pub fn parse(header: &str) -> Option<Columns> {
        let names: Vec<&str> = header.trim_end_matches('\r').split(',').collect();
        if names.first() != Some(&"time") {
            return None;
        }
        let mut blocks: Vec<BlockColumns> = Vec::new();
        for (i, column) in names.iter().enumerate() {
            let Some((block, what)) = column.rsplit_once('.') else {
                continue;
            };
            let at = if let Some(at) = blocks.iter().position(|b| b.name == block) {
                at
            } else {
                blocks.push(BlockColumns {
                    name: block.to_string(),
                    ratio: None,
                    busy: None,
                    temp: None,
                });
                blocks.len() - 1
            };
            let b = &mut blocks[at];
            match what {
                "ratio" => b.ratio = Some(i),
                "busy" => b.busy = Some(i),
                "temp" => b.temp = Some(i),
                _ => {}
            }
        }
        (!blocks.is_empty()).then(|| Columns {
            width: names.len(),
            fan_cpu: names.iter().position(|c| *c == "fan_cpu"),
            blocks,
        })
    }

    #[cfg(test)]
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.blocks.iter().map(|b| b.name.as_str())
    }
}

/// Счёт строк в плотных массивах (по месту столбца), потом сброс в `Tally` по названиям. Плотный счёт — чтобы на
/// строку не было ни поиска по названию, ни выделения памяти; массивы живут в рабочем потоке и переиспользуются.
#[derive(Debug, Default)]
pub struct Scratch {
    n: usize,
    rows: u64,
    secs: f64,
    first: Option<String>,
    last: Option<String>,
    bins: Vec<Bin>,              // [блок × BINS], абсолютная температура
    above: Vec<Bin>,             // [блок × BINS], над остальным кристаллом
    hists: Vec<Hist>,            // [(блок × BINS) × 2]: абсолютная, над кристаллом
    pairs: Vec<Split>,           // [(блок × n + другой) × BINS]
    cells: Vec<Cell>,            // по блокам, текущая строка
    fields: Vec<(usize, usize)>, // начало и конец полей текущей строки
}

#[derive(Clone, Copy, Debug, Default)]
struct Cell {
    ratio: Option<f64>,
    busy: Option<f64>,
    temp: Option<f64>,
}

fn number(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Корзина множителя: округление до целого; вне ×0…×80 — нет корзины.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and range-checked to 0..BINS before the cast"
)]
pub fn bin_of(ratio: f64) -> Option<usize> {
    let r = ratio.round();
    (0.0..=TOP).contains(&r).then_some(r as usize)
}

impl Scratch {
    fn reset(&mut self, n: usize) {
        if self.n == n && self.hists.len() == 2 * n * BINS {
            self.bins.fill(Bin::default());
            self.above.fill(Bin::default());
            self.hists.iter_mut().for_each(Hist::clear);
            self.pairs.fill(Split::default());
        } else {
            self.n = n;
            self.bins = vec![Bin::default(); n * BINS];
            self.above = vec![Bin::default(); n * BINS];
            self.hists = (0..n * BINS)
                .flat_map(|_| [Hist::new(ABS_CELL), Hist::new(ABOVE_CELL)])
                .collect();
            self.pairs = vec![Split::default(); n * n * BINS];
            self.cells = vec![Cell::default(); n];
        }
        self.rows = 0;
        self.secs = 0.0;
        self.first = None;
        self.last = None;
    }

    /// Строки срезов (полные, без заголовка) — в сводку файла. `previous` — время последней посчитанной строки этого
    /// файла (мс, `row_time`): файл дочитывается порциями, и первая строка порции считает промежуток от неё.
    pub fn count(&mut self, columns: &Columns, text: &str, previous: &mut Option<i64>, into: &mut Tally) {
        let n = columns.blocks.len();
        self.reset(n);
        // Самая ранняя и самая поздняя строка по времени, не по месту в файле: часы могут отойти назад (сверка
        // времени, ручная правка), и тогда последняя строка файла — не самая поздняя.
        let mut earliest: Option<(i64, &str)> = None;
        let mut latest: Option<(i64, &str)> = None;
        for line in text.lines() {
            if let Some((time, ms)) = self.row(columns, line.trim_end_matches('\r'), previous) {
                if earliest.is_none_or(|(e, _)| ms < e) {
                    earliest = Some((ms, time));
                }
                if latest.is_none_or(|(l, _)| ms >= l) {
                    latest = Some((ms, time));
                }
            }
        }
        self.first = earliest.map(|(_, t)| t.to_string()); // once per chunk
        self.last = latest.map(|(_, t)| t.to_string());
        self.flush(columns, into);
    }

    /// Одна строка; Some((время, мс)) — строка принята.
    fn row<'a>(&mut self, columns: &Columns, line: &'a str, previous: &mut Option<i64>) -> Option<(&'a str, i64)> {
        self.fields.clear();
        let mut start = 0;
        for (i, b) in line.bytes().enumerate() {
            if b == b',' {
                self.fields.push((start, i));
                start = i + 1;
            }
        }
        self.fields.push((start, line.len()));
        // Строка короче заголовка — испорчена (или дописывалась при сбое): не в счёт.
        if self.fields.len() < columns.width {
            return None;
        }
        let time = &line[self.fields[0].0..self.fields[0].1];
        // Без времени у строки нет длительности: не в счёт.
        let at = row_time(time)?;
        let span = span_ms(*previous, at);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a span of milliseconds between two rows, far below 2^52"
        )]
        let secs = span as f64 / 1000.0;
        // Промежуток — не больше полутора шагов записи (span_ms), то есть 3 с: в u32 помещается всегда.
        let ms = u32::try_from(span).unwrap_or(0);
        *previous = Some(at.ms);
        let field = |column: Option<usize>| column.and_then(|i| self.fields.get(i)).map(|&(s, e)| &line[s..e]);
        let fan = field(columns.fan_cpu).and_then(number);
        for (cell, b) in self.cells.iter_mut().zip(&columns.blocks) {
            *cell = Cell {
                ratio: field(b.ratio).and_then(number),
                busy: field(b.busy).and_then(number),
                temp: field(b.temp).and_then(number),
            };
        }
        let n = self.n;
        // Уровень кристалла в этой строке: сумма и число известных температур всех блоков (у блока — без него самого).
        let (sum, known) = self
            .cells
            .iter()
            .filter_map(|c| c.temp)
            .fold((0.0, 0), |(s, k), t| (s + t, k + 1));
        for a in 0..n {
            let me = self.cells[a];
            let (Some(temp), Some(ratio)) = (me.temp, me.ratio) else {
                continue;
            };
            let Some(bin) = bin_of(ratio) else { continue };
            // График: каждая строка, при любой нагрузке (владелец 15.10: «от 0 до 100»).
            let seen = Seen {
                temp,
                ratio,
                busy: me.busy,
                secs,
                at: at.ms,
            };
            let at_bin = a * BINS + bin;
            self.bins[at_bin].add(seen, fan);
            self.hists[2 * at_bin].add(temp, ms);
            // Над остальным кристаллом: то же правило, что `above_die`, без второго прохода по блокам.
            if let Some(level) = die_level(sum - temp, known - 1, n - 1) {
                let above = temp - level;
                self.above[at_bin].add(Seen { temp: above, ..seen }, fan);
                self.hists[2 * at_bin + 1].add(above, ms);
            }
            // Связь соседей: только пока блок сам занят, сосед — занят или свободен (это правило не менялось).
            if me.busy.is_none_or(|busy| busy < BUSY) {
                continue;
            }
            for b in 0..n {
                let Some(other) = self.cells[b].busy else { continue };
                if b == a {
                    continue;
                }
                let split = &mut self.pairs[(a * n + b) * BINS + bin];
                if other >= BUSY {
                    split.together.add(temp, secs);
                } else {
                    split.alone.add(temp, secs);
                }
            }
        }
        self.rows += 1;
        self.secs += secs;
        Some((time, at.ms))
    }

    fn flush(&self, columns: &Columns, into: &mut Tally) {
        let chunk = Tally {
            rows: self.rows,
            secs: self.secs,
            first: self.first.clone(),
            last: self.last.clone(),
            blocks: BTreeMap::new(),
        };
        into.merge(&chunk);
        let n = self.n;
        for (a, block) in columns.blocks.iter().enumerate() {
            let mut touched = false;
            for bin in 0..BINS {
                let b = &self.bins[a * BINS + bin];
                if b.n == 0 {
                    continue;
                }
                touched = true;
                let tally = into.blocks.entry(block.name.clone()).or_default(); // one name per used bin per chunk
                tally.bins.entry(ratio_of(bin)).or_default().merge(b);
                let at = a * BINS + bin;
                merge_hist(&mut tally.hist, ratio_of(bin), ABS_CELL, &self.hists[2 * at]);
                let up = &self.above[at];
                if up.n > 0 {
                    tally.above.entry(ratio_of(bin)).or_default().merge(up);
                    merge_hist(
                        &mut tally.above_hist,
                        ratio_of(bin),
                        ABOVE_CELL,
                        &self.hists[2 * at + 1],
                    );
                }
                for (o, other) in columns.blocks.iter().enumerate() {
                    let s = self.pairs[(a * n + o) * BINS + bin];
                    if s.together.secs <= 0.0 && s.alone.secs <= 0.0 {
                        continue;
                    }
                    let into = tally
                        .with
                        .entry(other.name.clone()) // a dozen names per used bin per chunk
                        .or_default()
                        .entry(ratio_of(bin))
                        .or_default();
                    into.together.merge(s.together);
                    into.alone.merge(s.alone);
                }
            }
            if !touched {
                into.blocks.entry(block.name.clone()).or_default();
            }
        }
    }
}

/// Номер корзины (0..BINS) как множитель.
fn ratio_of(bin: usize) -> i32 {
    i32::try_from(bin).unwrap_or(i32::MAX)
}

// ----- журналы, которые дописываются -----

/// Докуда файл прочитан. Программа входа только дописывает строки в конец; строка без перевода строки в конце —
/// ещё дописывается (или запись оборвалась) и читается в следующий раз, когда станет полной.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tail {
    offset: u64,
    len: u64,
    modified: Option<SystemTime>,
}

/// Новый текст из файла: полные строки после прочитанного.
#[derive(Debug)]
pub enum Fresh {
    Same,
    /// Дописаны строки.
    More(String),
    /// Файл стал короче или переписан — читается с начала: всё прежнее из него забыть.
    Again(String),
}

pub fn read_new(path: &Path, tail: &mut Tail) -> io::Result<Fresh> {
    let meta = std::fs::metadata(path)?;
    let len = meta.len();
    let modified = meta.modified().ok();
    if len == tail.len && modified == tail.modified {
        return Ok(Fresh::Same);
    }
    let again = len < tail.offset || (len == tail.len && modified != tail.modified);
    let from = if again { 0 } else { tail.offset };
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take(len - from).read_to_end(&mut bytes)?;
    let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    bytes.truncate(whole);
    tail.offset = from + whole as u64;
    tail.len = len;
    tail.modified = modified;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // Испорченный байт не должен лишать всего файла: он заменяется, строка с ним, скорее всего, не разберётся.
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    Ok(if again { Fresh::Again(text) } else { Fresh::More(text) })
}

// ----- журнал действий автомодуля -----

/// Строка `data\auto-log.jsonl` (пишет `rust\tray\src\app\auto.rs`: `record` и `put_back`). Поля, которых окно не
/// показывает (температуры всех блоков, вентиляторы…), пропускаются.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AutoEntry {
    #[serde(default)]
    pub time: Option<String>,
    pub act: String,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
    #[serde(default)]
    pub mean: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub cool: Option<u64>,
    #[serde(default)]
    pub ban: Option<i64>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub strikes: Option<u32>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
}

pub fn auto_entries(text: &str, into: &mut Vec<AutoEntry>) {
    into.extend(text.lines().filter_map(|l| serde_json::from_str::<AutoEntry>(l).ok()));
}

/// Действие автомодуля над блоком — для списка под графиком.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub time: String,
    pub change: String,
    pub why: String,
    pub lower: bool,
}

/// Действия над блоками `names` (сравнение нескольких блоков), новые первыми: номер названия в `names` и действие.
pub fn actions_for(entries: &[AutoEntry], names: &[&str]) -> Vec<(usize, Action)> {
    let mut out = Vec::new();
    for e in entries {
        let time = || e.time.as_deref().map(short_time).unwrap_or_default();
        match e.act.as_str() {
            "lower" | "raise" => {
                let Some(which) = e.block.as_deref().and_then(|b| names.iter().position(|n| *n == b)) else {
                    continue;
                };
                let lower = e.act == "lower";
                let change = match (e.from, e.to) {
                    (Some(f), Some(t)) => format!("×{f} → ×{t}"),
                    (_, Some(t)) => format!("→ ×{t}"),
                    _ => String::new(),
                };
                let why = if lower {
                    let mut why = match (e.mean, e.max) {
                        (Some(m), Some(x)) => format!("перегрев: ø {m:.0} °C, ▲ {x:.0} °C"),
                        (Some(m), None) => format!("перегрев: ø {m:.0} °C"),
                        _ => "перегрев".to_string(),
                    };
                    if let Some(ban) = e.ban {
                        let until = e.until.as_deref().map(short_time).unwrap_or_default();
                        let strikes = e.strikes.map(|s| format!(", {s}-й раз")).unwrap_or_default();
                        let _ = write!(why, "; ×{ban} не раньше {until}{strikes}"); // into a String
                    }
                    why
                } else {
                    e.cool.map_or_else(
                        || "прохладен под нагрузкой".to_string(),
                        |c| format!("прохладен под нагрузкой {c} с"),
                    )
                };
                out.push((
                    which,
                    Action {
                        time: time(),
                        change,
                        why,
                        lower,
                    },
                ));
            }
            // Возврат принятых — одна строка журнала на все блоки: по действию на каждый выбранный из них.
            "back" => {
                for (which, name) in names.iter().enumerate() {
                    let prefix = format!("{name} ×");
                    if let Some(item) = e.blocks.iter().find(|b| b.starts_with(&prefix)) {
                        out.push((
                            which,
                            Action {
                                time: time(),
                                change: format!("→ {}", &item[name.len() + 1..]),
                                why: format!("принятый возвращён: {}", e.why.as_deref().unwrap_or("")),
                                lower: false,
                            },
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    out.reverse();
    out
}

// ----- журнал смен профиля -----

/// Блок для журнала профилей: название и первое ядро (потолки в журнале — по ядрам).
#[derive(Debug, Clone, Copy)]
pub struct Named<'a> {
    pub name: &'a str,
    pub first: usize,
}

/// Строка журнала смен профиля (тема «history» программы входа или `data\profile-log.jsonl`; пишет
/// `rust\tray\src\app\history.rs`): время и что сделано. Поля: time; act (logon, menu, open, save, accept, revert,
/// delete, limit, wake, foreign); name (null — потолки не из набора, "" — заводские); before и after — потолки по
/// ядрам; block — у ручной правки; note — почему постановка неполная. Незнакомое — показывается как есть, коротко.
pub fn history_line(entry: &Map<String, Value>, blocks: &[Named]) -> (String, String) {
    let text_of = |key: &str| entry.get(key).and_then(Value::as_str);
    let time = text_of("time").map(short_time).unwrap_or_default();
    let act = text_of("act").unwrap_or("");
    let named = match entry.get("name") {
        Some(Value::String(n)) if n.is_empty() => "заводские".to_string(),
        Some(Value::String(n)) => format!("«{n}»"),
        _ => String::new(),
    };
    let verb = match act {
        "logon" => "при входе в Windows поставлен".to_string(),
        "menu" => "выбран в меню значка".to_string(),
        "open" => "открыт".to_string(),
        "save" => "сохранён".to_string(),
        "accept" => "принят".to_string(),
        "revert" => "откат к «было»".to_string(),
        "delete" => "удалён".to_string(),
        "limit" => format!("потолок {} вручную", text_of("block").unwrap_or("")),
        "wake" => "после сна поставлен заново".to_string(),
        "foreign" => "чужая правка принята".to_string(),
        other => other.to_string(),
    };
    let mut text = [verb.as_str(), named.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    // Что изменилось: блоки, у которых потолок первого ядра до и после разный.
    let ceilings = |key: &str| entry.get(key).and_then(Value::as_array);
    if let (Some(before), Some(after)) = (ceilings("before"), ceilings("after")) {
        let parts: Vec<String> = blocks
            .iter()
            .filter_map(|b| {
                let was = before.get(b.first).and_then(Value::as_i64)?;
                let now = after.get(b.first).and_then(Value::as_i64)?;
                (was != now).then(|| format!("{} ×{was}→×{now}", b.name))
            })
            .collect();
        if !parts.is_empty() {
            text.push_str(": ");
            text.push_str(&parts.join(", "));
        }
    }
    if let Some(note) = text_of("note") {
        let _ = write!(text, " ({note})"); // into a String
    }
    if text.is_empty() {
        // Незнакомая запись: её поля как есть, без времени.
        text = entry
            .iter()
            .filter(|(k, _)| k.as_str() != "time" && k.as_str() != "at")
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ");
    }
    (time, text)
}

pub fn history_entries(text: &str, into: &mut Vec<Map<String, Value>>) {
    into.extend(
        text.lines()
            .filter_map(|l| serde_json::from_str::<Map<String, Value>>(l).ok()),
    );
}

// ----- время -----

/// «2026-10-15T01:25:08.1234567+03:00» или «2026-10-15 01:25:08» → «15.10 01:25»; другое — как есть.
pub fn short_time(text: &str) -> String {
    let b = text.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    if b.len() >= 16 && digits(0..4) && digits(5..7) && digits(8..10) && digits(11..13) && digits(14..16) {
        format!("{}.{} {}", &text[8..10], &text[5..7], &text[11..16])
    } else {
        text.to_string()
    }
}

/// Дата по числу дней от 1970-01-01 (григорианский календарь; алгоритм H. Hinnant `civil_from_days`).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, u32::try_from(month).unwrap_or(1), u32::try_from(day).unwrap_or(1))
}

/// Число дней от 1970-01-01 по дате (обратное к `civil`; H. Hinnant `days_from_civil`).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Время строки (`row_time`, мс) для владельца: «15.10 14:03:07».
pub fn day_time(ms: i64) -> String {
    let (_, month, day) = civil(ms.div_euclid(86_400_000));
    let second = ms.rem_euclid(86_400_000) / 1000;
    format!(
        "{day:02}.{month:02} {:02}:{:02}:{:02}",
        second / 3600,
        second / 60 % 60,
        second % 60
    )
}

/// «ГГГГ-ММ-ДД» за `back` дней до сегодняшней даты (по UTC: граница окна в 30 дней — с точностью до часов пояса).
pub fn date_before(now: SystemTime, back: i64) -> String {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let (y, m, d) = civil(secs.div_euclid(86_400) - back);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp,2–5.ceiling,2–5.ratio,2–5.busy,2–5.temp";

    fn tally(rows: &[&str]) -> Tally {
        let columns = Columns::parse(HEADER).expect("заголовок");
        let mut scratch = Scratch::default();
        let mut tally = Tally::default();
        scratch.count(&columns, &rows.join("\n"), &mut None, &mut tally);
        tally
    }

    /// Время прежней строки: шаг 2 с от 10:00:00.
    fn old(i: u32) -> String {
        format!("2026-10-15 10:{:02}:{:02}", i * 2 / 60, i * 2 % 60)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn header_maps_columns_by_name_with_dash_names() {
        let columns = Columns::parse(HEADER).expect("заголовок");
        assert_eq!(columns.names().collect::<Vec<_>>(), ["0", "2–5"]);
        assert!(Columns::parse("2026-10-15 01:25:08,55").is_none());
    }

    #[test]
    fn old_and_millisecond_times_parse_and_name_their_step() {
        let day = days_from_civil(2026, 10, 15) * 86_400_000;
        let second = day + (3600 + 25 * 60 + 8) * 1000;
        assert_eq!(
            row_time("2026-10-15 01:25:08"),
            Some(RowTime {
                ms: second,
                step: OLD_STEP_MS
            })
        );
        assert_eq!(
            row_time("2026-10-15 01:25:08.250"),
            Some(RowTime {
                ms: second + 250,
                step: STEP_MS
            })
        );
        assert_eq!(row_time("2026-10-15 01:25:08.5").map(|t| t.ms), Some(second + 500));
        assert_eq!(
            row_time("2026-10-15 01:25:08.1234567").map(|t| t.ms),
            Some(second + 123)
        );
        for bad in [
            "",
            "t1",
            "2026-10-15 01:25",
            "2026-10-15T01:25:08",
            "2026-10-15 24:00:00",
            "2026-13-15 01:25:08",
            "2026-10-15 01:25:08.",
            "2026-10-15 01:25:08,5",
            "2026-10-15 01:25:08.25x",
        ] {
            assert_eq!(row_time(bad), None, "{bad}");
        }
        // Сутки и месяцы — по календарю: 1 марта 2028 — через день после 28 февраля (високосный год).
        let feb = row_time("2028-02-28 12:00:00").map_or(0, |t| t.ms);
        let mar = row_time("2028-03-01 12:00:00").map_or(0, |t| t.ms);
        assert_eq!(mar - feb, 2 * 86_400_000);
    }

    #[test]
    fn days_from_civil_undoes_civil() {
        for days in [-1, 0, 59, 60, 10_957, 11_016, 20_376, 40_000] {
            let (y, m, d) = civil(days);
            assert_eq!(days_from_civil(y, i64::from(m), i64::from(d)), days, "{y}-{m}-{d}");
        }
        assert_eq!(
            day_time(row_time("2026-10-15 14:03:07.900").map_or(0, |t| t.ms)),
            "15.10 14:03:07"
        );
    }

    #[test]
    fn a_row_covers_the_gap_to_the_previous_one_but_not_a_pause() {
        let at = |text: &str| row_time(text).expect("время");
        let new = at("2026-10-15 10:00:00.266");
        assert_eq!(span_ms(None, new), STEP_MS, "первая строка — шаг");
        assert_eq!(span_ms(Some(new.ms - 266), new), 266, "дрожание таймера — как есть");
        assert_eq!(span_ms(Some(new.ms - 375), new), 375, "полтора шага — ещё как есть");
        assert_eq!(span_ms(Some(new.ms - 376), new), STEP_MS, "больше — перерыв: один шаг");
        assert_eq!(span_ms(Some(new.ms + 10), new), STEP_MS, "часы назад — один шаг");
        assert_eq!(span_ms(Some(new.ms), new), STEP_MS, "то же время — один шаг");
        let old = at("2026-10-15 10:00:03");
        assert_eq!(
            span_ms(Some(old.ms - 3000), old),
            3000,
            "прежние строки до секунды: 3 с между ними бывает"
        );
        assert_eq!(span_ms(Some(old.ms - 4000), old), OLD_STEP_MS);
    }

    #[test]
    fn mixed_old_and_new_rows_weigh_by_their_time() {
        // Прежние строки по 2 с при 60 °C, перерыв, новые по 0,25 с при 80 °C — одна корзина ×52. Средняя по
        // времени: (60·4 + 80·2) / 6 = 66,7; по числу строк вышло бы (60·2 + 80·8) / 10 = 76.
        let mut rows = vec![
            "2026-10-15 10:00:00,60,,,53,52.0,0.9,60,46,,,".to_string(),
            "2026-10-15 10:00:02,60,,,53,52.0,0.9,60,46,,,".to_string(),
        ];
        for k in 0..8 {
            let ms = k * 250;
            rows.push(format!(
                "2026-10-15 10:05:{:02}.{:03},60,,,53,52.0,0.9,80,46,,,",
                ms / 1000,
                ms % 1000
            ));
        }
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let t = tally(&rows);
        let bin = t.blocks["0"].bins[&52];
        assert_eq!(bin.n, 10);
        assert!(close(bin.secs, 6.0) && close(t.secs, 6.0), "{} {}", bin.secs, t.secs);
        assert!(
            close(bin.mean().unwrap_or(0.0), (60.0 * 4.0 + 80.0 * 2.0) / 6.0),
            "{:?}",
            bin.mean()
        );
    }

    #[test]
    fn empty_fields_are_unknown_not_zero() {
        // Блок 0, температура пуста — строка не даёт ему точки; 2–5 без множителя — тоже; без доли работы точка есть,
        // но в среднюю нагрузку не идёт.
        let t = tally(&[
            "2026-10-15 01:00:00,60,,,53,52.0,0.9,,46,,0.9,70",
            "2026-10-15 01:00:02,60,,,53,52.0,,80,46,45.0,0.9,70",
        ]);
        assert_eq!(t.rows, 2);
        let b0 = &t.blocks["0"].bins;
        assert_eq!(b0.len(), 1);
        assert_eq!(b0[&52].n, 1);
        assert_eq!(b0[&52].fan(), None, "пустые обороты — неизвестны, а не 0");
        assert_eq!(b0[&52].load(), None, "пустая доля работы — неизвестна, а не 0");
        assert_eq!(t.blocks["2–5"].bins[&45].n, 1);
        assert_eq!(t.first.as_deref(), Some("2026-10-15 01:00:00"));
        assert_eq!(t.last.as_deref(), Some("2026-10-15 01:00:02"));
    }

    #[test]
    fn every_load_goes_into_the_bins_with_its_mean_load() {
        // Нагрузка 0,05 … 1,00: в корзину идут все строки (владелец 15.10: «от 0 до 100»), средняя нагрузка — по
        // времени.
        let t = tally(&[
            &format!("{},60,3000,,53,52.0,0.05,50,46,45.0,,70", old(0)),
            &format!("{},60,3000,,53,52.0,0.49,60,46,45.0,,70", old(1)),
            &format!("{},60,3000,,53,52.0,0.50,70,46,45.0,,70", old(2)),
            &format!("{},60,3000,,53,52.0,1.00,80,46,45.0,,70", old(3)),
        ]);
        let b = t.blocks["0"].bins[&52];
        assert_eq!(b.n, 4, "ни одна нагрузка не отсекается");
        assert_eq!(b.mean(), Some(65.0));
        assert!(close(b.load().unwrap_or(0.0), (0.05 + 0.49 + 0.5 + 1.0) / 4.0));
        assert_eq!(b.fan(), Some(3000.0));
        assert_eq!(t.blocks["2–5"].bins[&45].n, 4, "без доли работы точки тоже есть");
    }

    #[test]
    fn the_peak_carries_its_row_multiplier_load_and_time() {
        let t = tally(&[
            "2026-10-15 10:00:00.000,60,,,53,51.8,0.40,88,46,,,",
            "2026-10-15 10:00:00.250,60,,,53,52.3,0.70,91,46,,,",
            "2026-10-15 10:00:00.500,60,,,53,52.1,0.95,90,46,,,",
        ]);
        let peak = t.blocks["0"].bins[&52].peak;
        assert_eq!(peak.temp, 91.0);
        assert_eq!(peak.ratio, 52.3, "фактический множитель строки, а не корзина");
        assert_eq!(peak.busy, Some(0.7));
        assert_eq!(day_time(peak.at), "15.10 10:00:00");
        assert_eq!(peak.at, row_time("2026-10-15 10:00:00.250").map_or(0, |t| t.ms));
    }

    #[test]
    fn bins_by_rounded_actual_multiplier() {
        let t = tally(&[
            &format!("{},60,2000,,53,44.6,1,70,46,,0,40", old(0)),
            &format!("{},60,4000,,53,45.4,1,74,46,,0,40", old(1)),
            &format!("{},60,5000,,53,45.6,1,90,46,,0,40", old(2)),
        ]);
        let bins = &t.blocks["0"].bins;
        assert_eq!(bins.keys().copied().collect::<Vec<_>>(), [45, 46]);
        assert_eq!(bins[&45].n, 2);
        assert_eq!(bins[&45].mean(), Some(72.0));
        assert_eq!(bins[&45].peak.temp, 74.0);
        assert_eq!(bins[&45].fan(), Some(3000.0));
        assert_eq!(bins[&46].peak.temp, 90.0);
    }

    #[test]
    fn rows_without_a_time_are_not_counted() {
        let t = tally(&[
            "t1,60,3000,,53,52.0,0.9,90,46,45.0,,70",
            &format!("{},60,,,53,52.0,0.9,80,46,,,", old(0)),
        ]);
        assert_eq!(t.rows, 1);
        assert_eq!(t.blocks["0"].bins[&52].peak.temp, 80.0);
    }

    #[test]
    fn neighbour_effect_is_found_inside_multiplier_bins() {
        // Блок 0 всегда занят. На ×40 ему 50 °C, на ×50 — 80 °C; когда занят 2–5, на 6 °C больше. Сосед занят на
        // ×50 почти всегда, а на ×40 редко: простая разница средних «вместе − порознь» дала бы около 27 °C, а
        // ответ — ровно 6.
        let mut rows = Vec::new();
        for i in 0..100 {
            let (ratio, base, busy_share) = if i < 50 { (40.0, 50.0, 0.1) } else { (50.0, 80.0, 0.9) };
            let k = f64::from(i % 50) / 50.0;
            let neighbour = k < busy_share;
            let temp = base + if neighbour { 6.0 } else { 0.0 };
            rows.push(format!(
                "{},60,,,53,{ratio},1,{temp},46,45,{},60",
                old(i),
                if neighbour { "0.9" } else { "0.1" }
            ));
        }
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let t = tally(&rows);
        let e = effect(&t.blocks["0"].with["2–5"]).expect("есть корзины с обеими сторонами");
        assert!((e.delta - 6.0).abs() < 1e-9, "разница {}", e.delta);
        assert!(close(e.together + e.alone, 200.0), "100 строк по 2 с");
        // Обратная пара: 2–5 занят только с занятым 0 — «порознь» нет, сравнивать не с чем.
        assert_eq!(effect(&t.blocks["2–5"].with["0"]), None);
    }

    #[test]
    fn effect_needs_ten_seconds_on_both_sides_whatever_the_step() {
        assert!(close(MIN_PAIR, 10.0));
        // Прежние строки: 4 «вместе» по 2 с — 8 с, мало; 5 — 10 с, хватает.
        // Новые строки: 39 «вместе» по 0,25 с — 9,75 с, мало; 40 — 10 с, хватает.
        for (together, stamp, enough) in [
            (4, None, false),
            (5, None, true),
            (39, Some(250), false),
            (40, Some(250), true),
        ] {
            let mut rows = Vec::new();
            for i in 0..(together + 60) {
                let time = match stamp {
                    None => old(i),
                    Some(step) => {
                        let ms = i * step;
                        format!(
                            "2026-10-15 10:{:02}:{:02}.{:03}",
                            ms / 60_000,
                            ms / 1000 % 60,
                            ms % 1000
                        )
                    }
                };
                let neighbour = i < together;
                rows.push(format!(
                    "{time},60,,,53,45,1,{},46,45,{},60",
                    if neighbour { 70 } else { 60 },
                    u8::from(neighbour)
                ));
            }
            let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
            let found = effect(&tally(&rows).blocks["0"].with["2–5"]);
            assert_eq!(found.is_some(), enough, "{together} строк «вместе», шаг {stamp:?}");
            if let Some(e) = found {
                assert!(close(e.delta, 10.0), "{}", e.delta);
            }
        }
    }

    #[test]
    fn merge_adds_counts_and_keeps_the_max() {
        let mut a = tally(&["2026-10-14 10:00:00,60,,,53,45,1,70,46,,,"]);
        let b = tally(&[
            "2026-10-15 10:00:00,60,,,53,45,1,90,46,,,",
            "2026-10-15 10:00:02,60,,,53,45,1,80,46,,,",
        ]);
        a.merge(&b);
        let bin = a.blocks["0"].bins[&45];
        assert_eq!((bin.n, bin.peak.temp), (3, 90.0));
        assert_eq!(bin.mean(), Some(80.0));
        assert_eq!(a.rows, 3);
        assert!(close(a.secs, 6.0));
        assert_eq!(a.first.as_deref(), Some("2026-10-14 10:00:00"));
        assert_eq!(a.last.as_deref(), Some("2026-10-15 10:00:02"));
        // Равные наибольшие: остаётся поздняя строка, в каком порядке ни сливай.
        let early = tally(&["2026-10-14 10:00:00,60,,,53,45,1,90,46,,,"]);
        let mut x = early.clone();
        x.merge(&b);
        let mut y = b.clone();
        y.merge(&early);
        assert_eq!(x.blocks["0"].bins[&45].peak, y.blocks["0"].bins[&45].peak);
        assert_eq!(day_time(x.blocks["0"].bins[&45].peak.at), "15.10 10:00:00");
    }

    #[test]
    fn a_cut_last_line_waits_until_it_is_whole() {
        let path = std::env::temp_dir().join(format!("coremap-window-tail-{}.csv", std::process::id()));
        std::fs::write(&path, format!("{HEADER}\nt1,60,,,53,45,1,70,46,,,\nt2,60,,,53,4")).expect("запись");
        let mut tail = Tail::default();
        let Ok(Fresh::More(first)) = read_new(&path, &mut tail) else {
            panic!("первое чтение")
        };
        assert_eq!(
            first.lines().count(),
            2,
            "заголовок и одна полная строка; обрезанная ждёт"
        );
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).expect("дописать");
        std::io::Write::write_all(&mut file, b"5,1,72,46,,,\n").expect("дописать");
        drop(file);
        let Ok(Fresh::More(second)) = read_new(&path, &mut tail) else {
            panic!("второе чтение")
        };
        assert_eq!(second, "t2,60,,,53,45,1,72,46,,,\n");
        assert!(matches!(read_new(&path, &mut tail), Ok(Fresh::Same)));
        std::fs::write(&path, format!("{HEADER}\n")).expect("переписать короче");
        assert!(matches!(read_new(&path, &mut tail), Ok(Fresh::Again(_))));
        let _ = std::fs::remove_file(&path); // временный файл теста; не удалился — не беда
    }

    #[test]
    fn auto_actions_of_one_block_newest_first() {
        let log = [
            r#"{"time":"2026-10-15T01:25:08.0000000+03:00","act":"lower","block":"2–5","from":46,"to":45,"mean":88.4,"max":93.0,"ban":46,"until":"2026-10-15T01:55:08.0000000+03:00","strikes":1,"temps":[1,2]}"#,
            r#"{"time":"2026-10-15T01:26:00.0000000+03:00","act":"lower","block":"0","from":53,"to":52,"mean":90.0,"max":95.0}"#,
            r#"{"time":"2026-10-15T01:30:00.0000000+03:00","act":"raise","block":"2–5","from":45,"to":46,"cool":120}"#,
            r#"{"time":"2026-10-15T01:40:00.0000000+03:00","act":"back","why":"выход","blocks":["0 ×53","2–5 ×46"]}"#,
            "не json",
        ];
        let mut entries = Vec::new();
        auto_entries(&log.join("\n"), &mut entries);
        assert_eq!(entries.len(), 4);
        let actions: Vec<Action> = actions_for(&entries, &["2–5"]).into_iter().map(|(_, a)| a).collect();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].time, "15.10 01:40");
        assert_eq!(actions[0].change, "→ ×46");
        assert_eq!(actions[1].change, "×45 → ×46");
        assert!(actions[1].why.contains("120 с"));
        assert!(
            actions[2].lower && actions[2].why.contains("88") && actions[2].why.contains("×46 не раньше 15.10 01:55")
        );
        assert!(actions_for(&entries, &["2"]).is_empty(), "«2» — не «2–5»");
        // Два блока сразу: одно время по журналу, новые первыми; возврат принятых — по строке на каждый блок.
        let both = actions_for(&entries, &["0", "2–5"]);
        let order: Vec<(usize, &str)> = both.iter().map(|(w, a)| (*w, a.change.as_str())).collect();
        assert_eq!(
            order,
            [
                (1, "→ ×46"),
                (0, "→ ×53"),
                (1, "×45 → ×46"),
                (0, "×53 → ×52"),
                (1, "×46 → ×45")
            ]
        );
    }

    #[test]
    fn history_lines_read_the_logon_program_shape() {
        let entry = |text: &str| serde_json::from_str::<Map<String, Value>>(text).expect("json");
        let blocks = [
            Named { name: "0", first: 0 },
            Named {
                name: "2–5", first: 1
            },
        ];
        // Строка в том виде, в каком её пишет rust\tray\src\app\history.rs.
        let (time, text) = history_line(
            &entry(
                r#"{"time":"2026-10-15T01:20:00.0000000+03:00","at":1,"act":"open","name":"csmart2","before":[53,46,46,46,46],"after":[53,44,44,44,44]}"#,
            ),
            &blocks,
        );
        assert_eq!(time, "15.10 01:20");
        assert_eq!(text, "открыт «csmart2»: 2–5 ×46→×44");
        let (_, text) = history_line(
            &entry(r#"{"act":"menu","name":"","before":[50,44],"after":[53,46]}"#),
            &blocks,
        );
        assert_eq!(text, "выбран в меню значка заводские: 0 ×50→×53, 2–5 ×44→×46");
        let (_, text) = history_line(
            &entry(r#"{"act":"limit","name":null,"block":"0","before":[53,46],"after":[52,46]}"#),
            &blocks,
        );
        assert_eq!(text, "потолок 0 вручную: 0 ×53→×52");
        let (_, text) = history_line(
            &entry(r#"{"act":"logon","name":"тихий","before":[53],"after":[53],"note":"блок 1 не ответил"}"#),
            &blocks,
        );
        assert_eq!(text, "при входе в Windows поставлен «тихий» (блок 1 не ответил)");
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(10_957), (2000, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_hours(20_376 * 24 + 1);
        assert_eq!(date_before(at, 0), "2025-10-15");
        assert_eq!(date_before(at, 30), "2025-09-15");
    }

    #[test]
    fn above_the_die_needs_half_of_the_others() {
        // Блок 0 на 80 °C, остальные 60 и 70: над кристаллом +15.
        assert_eq!(above_die(&[Some(80.0), Some(60.0), Some(70.0)], 0), Some(15.0));
        // Из трёх остальных известен один — меньше половины: значения нет; известны два — есть.
        assert_eq!(above_die(&[Some(80.0), Some(60.0), None, None], 0), None);
        assert_eq!(above_die(&[Some(80.0), Some(60.0), Some(70.0), None], 0), Some(15.0));
        // Своей температуры нет — нет и значения; остальных нет вовсе — тоже.
        assert_eq!(above_die(&[None, Some(60.0), Some(70.0)], 0), None);
        assert_eq!(above_die(&[Some(80.0)], 0), None);
    }

    #[test]
    fn rows_count_above_the_die_only_where_the_die_level_is_known() {
        // Две строки по 2 с: в первой кластер 60 °C — ядро 0 над ним на +20; во второй температуры кластера нет — у
        // ядра 0 точка в абсолютной шкале есть, над кристаллом — нет.
        let t = tally(&[
            "2026-10-15 10:00:00,60,,,53,52.0,0.9,80,46,44.0,0.5,60",
            "2026-10-15 10:00:02,60,,,53,52.0,0.9,84,46,44.0,0.5,",
        ]);
        let core = &t.blocks["0"];
        assert_eq!(core.bins[&52].n, 2);
        assert_eq!(core.above[&52].n, 1);
        assert_eq!(core.above[&52].mean(), Some(20.0));
        assert_eq!(core.hist[&52].quantile(P95), Some(84.0));
        assert_eq!(core.above_hist[&52].quantile(P95), Some(20.0));
        // Кластер во второй строке без температуры — его корзина только из первой: −20.
        assert_eq!(t.blocks["2–5"].above[&44].mean(), Some(-20.0));
    }

    #[test]
    fn the_95th_percentile_is_by_time_not_by_rows() {
        // 19 секунд при 70 °C и 1 с при 90 °C: 95 % времени — не выше 70. По строкам (10 строк по 2 с против 4 по
        // 0,25 с) было бы иначе: 4 из 14 строк — 90.
        let mut h = Hist::new(ABS_CELL);
        for _ in 0..19 {
            h.add(70.0, 1000);
        }
        for _ in 0..4 {
            h.add(90.0, 250);
        }
        assert_eq!(h.total(), 20_000);
        assert_eq!(h.quantile(P95), Some(70.0));
        // Ещё полсекунды при 90 °C — 70 °C уже меньше 95 % времени.
        h.add(90.0, 500);
        assert_eq!(h.quantile(P95), Some(90.0));
        // Слияние — как одна гистограмма всех строк; пустая — нет процентиля.
        let mut a = Hist::new(ABOVE_CELL);
        let mut b = Hist::new(ABOVE_CELL);
        let mut whole = Hist::new(ABOVE_CELL);
        for (k, v) in [-3.1, 0.2, 5.0, 7.3, 12.6, -8.0].iter().enumerate() {
            let ms = 250 * (u32::try_from(k).unwrap_or(0) + 1);
            if k % 2 == 0 {
                a.add(*v, ms);
            } else {
                b.add(*v, ms);
            }
            whole.add(*v, ms);
        }
        a.merge(&b);
        assert_eq!(a, whole);
        assert_eq!(Hist::new(ABS_CELL).quantile(P95), None);
        // Значение вне сетки — центр своей ячейки: 7,3 → 7,25.
        let mut one = Hist::new(ABOVE_CELL);
        one.add(7.3, 250);
        assert_eq!(one.quantile(P95), Some(7.25));
    }
}
