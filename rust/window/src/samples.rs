// Статистика из журналов программы входа (DECISIONS #32): срезы data\samples\<дата>.csv (каждые 2 с, пока
// процессор занят) и журнал действий автомодуля data\auto-log.jsonl. Здесь только разбор и счёт — без окна и без
// потоков: что и когда читать, решает store.rs, как показать — panels.rs.
//
// Что считается (слова владельца 15.10: «какую температуру на каких множителях, на какой нагрузке… с каким-нибудь
// показанием вентилятора»; «если два ядра работают вместе они греются например сильнее»):
// • по блоку — корзины по фактическому множителю (округлённому до целого): число срезов, средняя и наибольшая
//   температура, средние обороты вентилятора процессора; только срезы, где блок занят (доля работы ≥ BUSY);
// • по паре блоков A, B — температура A, когда он занят, отдельно «B тоже занят» и «B свободен», в той же корзине
//   множителя A: так разница не смешивается с тем, что при работе соседа A чаще стоит на другом множителе.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::{Map, Value};

/// Блок занят — доля работы не меньше этой (та же граница, что у автомодуля: `machine::auto::BUSY`).
pub const BUSY: f64 = 0.5;
/// Множители ×0…×80: больше у этих процессоров не бывает (machine: заводской вне 8..80 — не ответ процессора).
pub const BINS: usize = 81;
const TOP: f64 = 80.0;
/// Сколько срезов нужно в корзине с каждой стороны, чтобы сравнивать «вместе» и «порознь»: 5 срезов — 10 с работы.
pub const MIN_PAIR: u32 = 5;

/// Корзина одного множителя.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Bin {
    pub n: u32,
    pub temp_sum: f64,
    pub temp_max: f64,
    pub fan_sum: f64,
    pub fan_n: u32,
}

impl Bin {
    fn add(&mut self, temp: f64, fan: Option<f64>) {
        self.temp_max = if self.n == 0 { temp } else { self.temp_max.max(temp) };
        self.n += 1;
        self.temp_sum += temp;
        if let Some(f) = fan {
            self.fan_sum += f;
            self.fan_n += 1;
        }
    }

    fn merge(&mut self, other: &Bin) {
        if other.n == 0 {
            return;
        }
        self.temp_max = if self.n == 0 {
            other.temp_max
        } else {
            self.temp_max.max(other.temp_max)
        };
        self.n += other.n;
        self.temp_sum += other.temp_sum;
        self.fan_sum += other.fan_sum;
        self.fan_n += other.fan_n;
    }

    pub fn mean(&self) -> Option<f64> {
        (self.n > 0).then(|| self.temp_sum / f64::from(self.n))
    }

    pub fn fan(&self) -> Option<f64> {
        (self.fan_n > 0).then(|| self.fan_sum / f64::from(self.fan_n))
    }
}

/// Сумма температур и число срезов.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Side {
    pub n: u32,
    pub sum: f64,
}

impl Side {
    fn add(&mut self, temp: f64) {
        self.n += 1;
        self.sum += temp;
    }

    fn merge(&mut self, other: Side) {
        self.n += other.n;
        self.sum += other.sum;
    }

    fn mean(self) -> f64 {
        self.sum / f64::from(self.n.max(1))
    }
}

/// Температура блока в корзине его множителя: когда другой блок тоже занят и когда свободен.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Split {
    pub together: Side,
    pub alone: Side,
}

/// Сводка по блоку: корзины по множителю и сравнение с каждым другим блоком (по названию).
#[derive(Clone, Default, Debug)]
pub struct BlockTally {
    pub bins: BTreeMap<i32, Bin>,
    pub with: BTreeMap<String, BTreeMap<i32, Split>>,
}

/// Сводка по строкам срезов: складывается из сводок файлов (`merge`).
#[derive(Clone, Default, Debug)]
pub struct Tally {
    pub rows: u64,
    pub first: Option<String>,
    pub last: Option<String>,
    pub blocks: BTreeMap<String, BlockTally>,
}

impl Tally {
    pub fn merge(&mut self, other: &Tally) {
        self.rows += other.rows;
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
/// где обеих сторон не меньше `MIN_PAIR`, с весом min(вместе, порознь). None — таких корзин нет.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effect {
    pub delta: f64,
    pub together: u32,
    pub alone: u32,
}

pub fn effect(splits: &BTreeMap<i32, Split>) -> Option<Effect> {
    let mut weight = 0.0;
    let mut sum = 0.0;
    let (mut together, mut alone) = (0, 0);
    for s in splits.values() {
        if s.together.n < MIN_PAIR || s.alone.n < MIN_PAIR {
            continue;
        }
        let w = f64::from(s.together.n.min(s.alone.n));
        weight += w;
        sum += w * (s.together.mean() - s.alone.mean());
        together += s.together.n;
        alone += s.alone.n;
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
    first: Option<String>,
    last: Option<String>,
    bins: Vec<Bin>,              // [блок × BINS]
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
        if self.n == n {
            self.bins.fill(Bin::default());
            self.pairs.fill(Split::default());
        } else {
            self.n = n;
            self.bins = vec![Bin::default(); n * BINS];
            self.pairs = vec![Split::default(); n * n * BINS];
            self.cells = vec![Cell::default(); n];
        }
        self.rows = 0;
        self.first = None;
        self.last = None;
    }

    /// Строки срезов (полные, без заголовка) — в сводку файла.
    pub fn count(&mut self, columns: &Columns, text: &str, into: &mut Tally) {
        let n = columns.blocks.len();
        self.reset(n);
        let mut last_time = None;
        for line in text.lines() {
            if let Some(time) = self.row(columns, line.trim_end_matches('\r')) {
                if self.first.is_none() {
                    self.first = Some(time.to_string()); // once per chunk
                }
                last_time = Some(time);
            }
        }
        self.last = last_time.map(str::to_string);
        self.flush(columns, into);
    }

    /// Одна строка; Some(время) — строка принята.
    fn row<'a>(&mut self, columns: &Columns, line: &'a str) -> Option<&'a str> {
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
        let field = |at: Option<usize>| at.and_then(|i| self.fields.get(i)).map(|&(s, e)| &line[s..e]);
        let fan = field(columns.fan_cpu).and_then(number);
        for (cell, b) in self.cells.iter_mut().zip(&columns.blocks) {
            *cell = Cell {
                ratio: field(b.ratio).and_then(number),
                busy: field(b.busy).and_then(number),
                temp: field(b.temp).and_then(number),
            };
        }
        let n = self.n;
        for a in 0..n {
            let me = self.cells[a];
            let (Some(busy), Some(temp), Some(ratio)) = (me.busy, me.temp, me.ratio) else {
                continue;
            };
            if busy < BUSY {
                continue;
            }
            let Some(bin) = bin_of(ratio) else { continue };
            self.bins[a * BINS + bin].add(temp, fan);
            for b in 0..n {
                let Some(other) = self.cells[b].busy else { continue };
                if b == a {
                    continue;
                }
                let split = &mut self.pairs[(a * n + b) * BINS + bin];
                if other >= BUSY {
                    split.together.add(temp);
                } else {
                    split.alone.add(temp);
                }
            }
        }
        self.rows += 1;
        Some(&line[self.fields[0].0..self.fields[0].1])
    }

    fn flush(&self, columns: &Columns, into: &mut Tally) {
        let chunk = Tally {
            rows: self.rows,
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
                for (o, other) in columns.blocks.iter().enumerate() {
                    let s = self.pairs[(a * n + o) * BINS + bin];
                    if s.together.n == 0 && s.alone.n == 0 {
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

/// Действия над блоком `name`, новые первыми.
pub fn actions_for(entries: &[AutoEntry], name: &str) -> Vec<Action> {
    let prefix = format!("{name} ×");
    let mut out: Vec<Action> = entries
        .iter()
        .filter_map(|e| {
            let time = e.time.as_deref().map(short_time).unwrap_or_default();
            match e.act.as_str() {
                "lower" | "raise" if e.block.as_deref() == Some(name) => {
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
                    Some(Action {
                        time,
                        change,
                        why,
                        lower,
                    })
                }
                "back" => {
                    let item = e.blocks.iter().find(|b| b.starts_with(&prefix))?;
                    Some(Action {
                        time,
                        change: format!("→ {}", &item[name.len() + 1..]),
                        why: format!("принятый возвращён: {}", e.why.as_deref().unwrap_or("")),
                        lower: false,
                    })
                }
                _ => None,
            }
        })
        .collect();
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
        scratch.count(&columns, &rows.join("\n"), &mut tally);
        tally
    }

    #[test]
    fn header_maps_columns_by_name_with_dash_names() {
        let columns = Columns::parse(HEADER).expect("заголовок");
        assert_eq!(columns.names().collect::<Vec<_>>(), ["0", "2–5"]);
        assert!(Columns::parse("2026-10-15 01:25:08,55").is_none());
    }

    #[test]
    fn empty_fields_are_unknown_not_zero() {
        // Блок 0 занят, но температура пуста — строка не даёт ему точки; 2–5 без множителя — тоже.
        let t = tally(&[
            "2026-10-15 01:00:00,60,,,53,52.0,0.9,,46,,0.9,70",
            "2026-10-15 01:00:02,60,,,53,52.0,0.9,80,46,45.0,0.9,70",
        ]);
        assert_eq!(t.rows, 2);
        let b0 = &t.blocks["0"].bins;
        assert_eq!(b0.len(), 1);
        assert_eq!(b0[&52].n, 1);
        assert_eq!(b0[&52].fan(), None, "пустые обороты — неизвестны, а не 0");
        assert_eq!(t.blocks["2–5"].bins[&45].n, 1);
        assert_eq!(t.first.as_deref(), Some("2026-10-15 01:00:00"));
        assert_eq!(t.last.as_deref(), Some("2026-10-15 01:00:02"));
    }

    #[test]
    fn busy_filter_takes_half_and_above_only() {
        let t = tally(&[
            "t1,60,3000,,53,52.0,0.49,90,46,45.0,,70",
            "t2,60,3000,,53,52.0,0.50,80,46,45.0,,70",
            "t3,60,3000,,53,52.0,1.00,70,46,45.0,,70",
        ]);
        let b = t.blocks["0"].bins[&52];
        assert_eq!(b.n, 2, "0.49 — не занят; 0.50 и 1.00 — заняты");
        assert_eq!(b.mean(), Some(75.0));
        assert_eq!(b.temp_max, 80.0);
        assert_eq!(b.fan(), Some(3000.0));
        assert!(t.blocks["2–5"].bins.is_empty(), "без доли работы — не в счёт");
    }

    #[test]
    fn bins_by_rounded_actual_multiplier() {
        let t = tally(&[
            "a,60,2000,,53,44.6,1,70,46,,0,40",
            "b,60,4000,,53,45.4,1,74,46,,0,40",
            "c,60,5000,,53,45.6,1,90,46,,0,40",
        ]);
        let bins = &t.blocks["0"].bins;
        assert_eq!(bins.keys().copied().collect::<Vec<_>>(), [45, 46]);
        assert_eq!(bins[&45].n, 2);
        assert_eq!(bins[&45].mean(), Some(72.0));
        assert_eq!(bins[&45].temp_max, 74.0);
        assert_eq!(bins[&45].fan(), Some(3000.0));
        assert_eq!(bins[&46].temp_max, 90.0);
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
                "t{i:03},60,,,53,{ratio},1,{temp},46,45,{},60",
                if neighbour { "0.9" } else { "0.1" }
            ));
        }
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let t = tally(&rows);
        let e = effect(&t.blocks["0"].with["2–5"]).expect("есть корзины с обеими сторонами");
        assert!((e.delta - 6.0).abs() < 1e-9, "разница {}", e.delta);
        assert_eq!(e.together + e.alone, 100);
        // Обратная пара: 2–5 занят только с занятым 0 — «порознь» нет, сравнивать не с чем.
        assert_eq!(effect(&t.blocks["2–5"].with["0"]), None);
    }

    #[test]
    fn effect_needs_enough_samples_on_both_sides() {
        let mut rows = Vec::new();
        for i in 0..20 {
            let neighbour = i < 4; // 4 «вместе» — меньше MIN_PAIR
            rows.push(format!(
                "t{i:02},60,,,53,45,1,{},46,45,{},60",
                if neighbour { 70 } else { 60 },
                u8::from(neighbour)
            ));
        }
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        assert_eq!(effect(&tally(&rows).blocks["0"].with["2–5"]), None);
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
        assert_eq!((bin.n, bin.temp_max), (3, 90.0));
        assert_eq!(bin.mean(), Some(80.0));
        assert_eq!(a.rows, 3);
        assert_eq!(a.first.as_deref(), Some("2026-10-14 10:00:00"));
        assert_eq!(a.last.as_deref(), Some("2026-10-15 10:00:02"));
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
        let actions = actions_for(&entries, "2–5");
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].time, "15.10 01:40");
        assert_eq!(actions[0].change, "→ ×46");
        assert_eq!(actions[1].change, "×45 → ×46");
        assert!(actions[1].why.contains("120 с"));
        assert!(
            actions[2].lower && actions[2].why.contains("88") && actions[2].why.contains("×46 не раньше 15.10 01:55")
        );
        assert!(actions_for(&entries, "2").is_empty(), "«2» — не «2–5»");
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
}
