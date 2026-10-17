// Подготовка наблюдений для условной модели: семантика эталона outputs/conditional-prototype/data.py.
// Выходная сетка — 1 с, среднее по точечным показаниям в (t−1 с,t], а не новый независимый датчик с частотой 1 Гц.
// Редкие общие датчики удерживаются только из прошлого, не дольше 3 с от последней строки с показанием.
// Разные файлы, возврат часов и разрывы больше 5 с образуют независимые эпизоды. Исходные Log не меняются.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{
    BUSY, BUSY_GPU, Die, FAN_CPU, FAN_GPU, Log, PACKAGE_TEMP, POWER_CORES, POWER_CPU, POWER_GPU, RATIO, TEMP, TEMP_GPU,
};

const RESOLUTION_MS: i64 = 1000;
const GAP_MS: i64 = 5000;
const HOLD_MS: i64 = 3000;
const SHARED: [(&str, usize); 7] = [
    ("fan_cpu", FAN_CPU),
    ("fan_gpu", FAN_GPU),
    ("temp_gpu", TEMP_GPU),
    ("power_gpu", POWER_GPU),
    ("busy_gpu", BUSY_GPU),
    ("power_cpu", POWER_CPU),
    ("power_cores", POWER_CORES),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Prepared {
    pub times: Vec<i64>,
    pub segments: Vec<usize>,
    #[serde(deserialize_with = "deserialize_values")]
    pub ratio: Vec<f64>,
    #[serde(deserialize_with = "deserialize_values")]
    pub busy: Vec<f64>,
    #[serde(deserialize_with = "deserialize_values")]
    pub target: Vec<f64>,
    /// Неизвестное — NaN. `package_lag_only` предназначен только для прошлых окон, не для текущего входа.
    #[serde(deserialize_with = "deserialize_contexts")]
    pub contexts: BTreeMap<String, Vec<f64>>,
}

// JSON представляет NaN как null; обратное чтение сохраняет неизвестное, не подставляя ноль.
fn deserialize_values<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<f64>, D::Error> {
    Vec::<Option<f64>>::deserialize(deserializer)
        .map(|values| values.into_iter().map(|value| value.unwrap_or(f64::NAN)).collect())
}

fn deserialize_contexts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<BTreeMap<String, Vec<f64>>, D::Error> {
    BTreeMap::<String, Vec<Option<f64>>>::deserialize(deserializer).map(|columns| {
        columns
            .into_iter()
            .map(|(name, values)| {
                (
                    name,
                    values.into_iter().map(|value| value.unwrap_or(f64::NAN)).collect(),
                )
            })
            .collect()
    })
}

#[derive(Clone, Copy, Default)]
struct Mean {
    sum: f64,
    count: usize,
}

impl Mean {
    fn add(&mut self, value: Option<f64>) {
        if let Some(value) = value.filter(|v| v.is_finite()) {
            self.sum += value;
            self.count += 1;
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "a bin's in-memory row count is below 2^53")]
    fn value(self) -> f64 {
        if self.count == 0 {
            f64::NAN
        } else {
            self.sum / self.count as f64
        }
    }
}

#[derive(Clone, Copy)]
enum Source {
    Shared(usize),
    DieTemp,
    OtherBusy,
    Rank,
    Neighbour(usize, usize),
    Package,
}

struct Context {
    name: String,
    source: Source,
}

fn contexts(die: &Die, block: usize) -> Vec<Context> {
    let mut result: Vec<Context> = SHARED
        .iter()
        .enumerate()
        .map(|(slot, (name, _))| Context {
            name: (*name).to_string(),
            source: Source::Shared(slot),
        })
        .collect();
    for (name, source) in [
        ("die_leave_target_out", Source::DieTemp),
        ("other_busy", Source::OtherBusy),
        ("ceiling_rank", Source::Rank),
        ("package_lag_only", Source::Package),
    ] {
        result.push(Context {
            name: name.to_string(),
            source,
        });
    }
    if let Some(neighbours) = die.neighbours.get(block) {
        for &neighbour in neighbours {
            if let Some(name) = die.names.get(neighbour) {
                for (quantity, label) in [(TEMP, "temp"), (BUSY, "busy"), (RATIO, "ratio")] {
                    result.push(Context {
                        name: format!("neighbour_{name}_{label}"),
                        source: Source::Neighbour(neighbour, quantity),
                    });
                }
            }
        }
    }
    result
}

fn ratio(cells: &[[Mean; 4]], block: usize) -> f64 {
    let value = cells[block][RATIO].value();
    if !value.is_finite() && cells[block][BUSY].value() == 0.0 {
        0.0
    } else {
        value
    }
}

fn others(cells: &[[Mean; 4]], block: usize, quantity: usize) -> f64 {
    let mut mean = Mean::default();
    for (i, cell) in cells.iter().enumerate() {
        if i != block {
            mean.add(Some(cell[quantity].value()));
        }
    }
    mean.value()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "at most the number of CPU blocks, far below 2^53"
)]
fn rank(cells: &[[Mean; 4]], block: usize, group: &[usize]) -> f64 {
    let own = cells[block][3].value();
    if !own.is_finite() || group.is_empty() {
        return f64::NAN;
    }
    let mut greater = 0;
    for &peer in group {
        let ceiling = cells[peer][3].value();
        if !ceiling.is_finite() {
            return f64::NAN;
        }
        greater += usize::from(ceiling > own);
    }
    greater as f64 / group.len().saturating_sub(1).max(1) as f64
}

struct Episode<'a> {
    log: &'a Log,
    range: std::ops::Range<usize>,
    file_blocks: &'a [Option<usize>],
    block: usize,
    group: &'a [usize],
    contexts: &'a [Context],
}

impl Episode<'_> {
    fn append(&self, segment: usize, output: &mut Prepared, columns: &mut [Vec<f64>]) {
        let times = self.log.times();
        let Some(mut at) = times[self.range.start].checked_add(RESOLUTION_MS) else {
            return;
        };
        let last = times[self.range.end - 1];
        let mut start = self.range.start;
        let mut end = self.range.start;
        let mut latest: [Option<(i64, f64)>; 7] = [None; 7];
        // Буфер средних переиспользуется для каждого окна; подготовка выполняется вне опроса датчиков.
        let mut cells = vec![[Mean::default(); 4]; self.file_blocks.len()];
        while at <= last {
            let lower = at - RESOLUTION_MS;
            while start < self.range.end && times[start] <= lower {
                start += 1;
            }
            while end < self.range.end && times[end] <= at {
                for (slot, (_, which)) in SHARED.iter().enumerate() {
                    if let Some(value) = self.log.environment(end, *which).filter(|v| v.is_finite()) {
                        latest[slot] = Some((times[end], value));
                    }
                }
                end += 1;
            }
            cells.fill([Mean::default(); 4]);
            let mut package = Mean::default();
            for row in start..end {
                package.add(self.log.environment(row, PACKAGE_TEMP));
                for (cell, file_block) in cells.iter_mut().zip(self.file_blocks) {
                    if let Some(file_block) = file_block {
                        for quantity in [RATIO, BUSY, TEMP] {
                            cell[quantity].add(self.log.get(row, *file_block, quantity));
                        }
                        cell[3].add(self.log.ceiling(row, *file_block).map(f64::from));
                    }
                }
            }
            output.times.push(at);
            output.segments.push(segment);
            output.ratio.push(ratio(&cells, self.block));
            output.busy.push(cells[self.block][BUSY].value());
            output.target.push(cells[self.block][TEMP].value());
            for (context, values) in self.contexts.iter().zip(columns.iter_mut()) {
                let value = match context.source {
                    Source::Shared(slot) => latest[slot]
                        .filter(|(observed, _)| at - observed <= HOLD_MS)
                        .map_or(f64::NAN, |(_, value)| value),
                    Source::DieTemp => others(&cells, self.block, TEMP),
                    Source::OtherBusy => others(&cells, self.block, BUSY),
                    Source::Rank => rank(&cells, self.block, self.group),
                    Source::Neighbour(neighbour, RATIO) => ratio(&cells, neighbour),
                    Source::Neighbour(neighbour, quantity) => cells[neighbour][quantity].value(),
                    Source::Package => package.value(),
                };
                values.push(value);
            }
            let Some(next) = at.checked_add(RESOLUTION_MS) else {
                break;
            };
            at = next;
        }
    }
}

pub fn prepare(logs: &[&Log], die: &Die, block: usize) -> Prepared {
    if block >= die.names.len() {
        return Prepared::default();
    }
    let schema = contexts(die, block);
    let mut columns = vec![Vec::new(); schema.len()];
    let group: Vec<usize> = die.e.get(block).map_or_else(Vec::new, |kind| {
        die.e
            .iter()
            .enumerate()
            .filter_map(|(i, e)| (e == kind && i < die.names.len()).then_some(i))
            .collect()
    });
    let mut output = Prepared::default();
    let mut segment = 0;
    for log in logs {
        let file_blocks: Vec<Option<usize>> = die
            .names
            .iter()
            .map(|name| log.names().iter().position(|n| n == name))
            .collect();
        let mut start = 0;
        for end in 1..=log.rows() {
            let boundary = end == log.rows()
                || log.times()[end]
                    .checked_sub(log.times()[end - 1])
                    .is_none_or(|gap| gap <= 0 || gap > GAP_MS);
            if boundary {
                Episode {
                    log,
                    range: start..end,
                    file_blocks: &file_blocks,
                    block,
                    group: &group,
                    contexts: &schema,
                }
                .append(segment, &mut output, &mut columns);
                start = end;
                segment += 1;
            }
        }
    }
    output.contexts = schema
        .into_iter()
        .zip(columns)
        .map(|(context, values)| (context.name, values))
        .collect();
    output
}
