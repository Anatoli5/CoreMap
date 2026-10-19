// Адаптер статистики по времени: выбранные блоки × выбранные величины, общие шкалы по единицам.
// Совместный показ использует до двух шкал; остальные единицы получают отдельные панели.
// Перестановка осей меняет только проекцию. Диапазоны и отсечение общие с остальными графиками.
// Линия обзора усредняет отсчёты по времени; потолок остаётся ступенями. Прежнее M4 сохранено
// для старого одиночного режима и его контракта. Чтение дня и сжатие перерывов не меняются.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::plot::{Range, Viewport};
use crate::samples::{self, Columns, Shared, What};

/// Видеокарта и вентиляторы пишутся раз в 2 с (в строке, где прочитаны): у них перерыв — больше этого.
const SPARSE_GAP_MS: i64 = 5000;
/// Самый узкий показ, мс: 10 с — сорок строк.
pub const MIN_SPAN_MS: i64 = 10_000;
/// Колесо: одна ступень Windows — 60 точек сдвига у Slint (winit: строка × 60); на ступень показ сужается до 0,8.
const WHEEL_STEP: f64 = 60.0;
const WHEEL_ZOOM: f64 = 0.8;
/// Расстояние между полосами раскладки «параметры», точки (то же в ui/app.slint, TimeChart.gap).
pub const STRIP_GAP: f64 = 4.0;

/// Величина блока по времени.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quantity {
    #[default]
    Temp,
    Ratio,
    Ceiling,
    Busy,
    Level,
    GpuTemp,
    GpuPower,
    GpuBusy,
    FanCpu,
    FanGpu,
}

impl Quantity {
    pub const BLOCK: [Quantity; 5] = [
        Quantity::Temp,
        Quantity::Ratio,
        Quantity::Ceiling,
        Quantity::Busy,
        Quantity::Level,
    ];
    pub const ALL: [Quantity; 10] = [
        Quantity::Temp,
        Quantity::Ratio,
        Quantity::Ceiling,
        Quantity::Busy,
        Quantity::Level,
        Quantity::GpuTemp,
        Quantity::GpuPower,
        Quantity::GpuBusy,
        Quantity::FanCpu,
        Quantity::FanGpu,
    ];

    /// Номер для окна (app.slint) и файла окна.
    #[must_use]
    pub fn index(self) -> i32 {
        match self {
            Quantity::Temp => 0,
            Quantity::Ratio => 1,
            Quantity::Ceiling => 2,
            Quantity::Busy => 3,
            Quantity::Level => 4,
            Quantity::GpuTemp => 5,
            Quantity::GpuPower => 6,
            Quantity::GpuBusy => 7,
            Quantity::FanCpu => 8,
            Quantity::FanGpu => 9,
        }
    }

    #[must_use]
    pub fn of(index: i32) -> Quantity {
        match index {
            1 => Quantity::Ratio,
            2 => Quantity::Ceiling,
            3 => Quantity::Busy,
            4 => Quantity::Level,
            5 => Quantity::GpuTemp,
            6 => Quantity::GpuPower,
            7 => Quantity::GpuBusy,
            8 => Quantity::FanCpu,
            9 => Quantity::FanGpu,
            _ => Quantity::Temp,
        }
    }

    /// Подпись полосы.
    pub fn title(self) -> &'static str {
        match self {
            Quantity::Temp => "температура, °C",
            Quantity::Ratio => "фактический множитель",
            Quantity::Ceiling => "потолок (ступени — его смены)",
            Quantity::Busy => "нагрузка, %",
            Quantity::Level => "остальные блоки (средняя температура), °C",
            Quantity::GpuTemp => "видеокарта, °C",
            Quantity::GpuPower => "видеокарта, Вт",
            Quantity::GpuBusy => "видеокарта, %",
            Quantity::FanCpu => "вентилятор процессора, об/мин",
            Quantity::FanGpu => "вентилятор видеокарты, об/мин",
        }
    }

    #[must_use]
    pub fn unit(self) -> Unit {
        match self {
            Self::Temp | Self::Level | Self::GpuTemp => Unit::Temperature,
            Self::Ratio | Self::Ceiling => Unit::Ratio,
            Self::Busy | Self::GpuBusy => Unit::Percent,
            Self::GpuPower => Unit::Power,
            Self::FanCpu | Self::FanGpu => Unit::Rpm,
        }
    }

    #[must_use]
    pub fn global(self) -> bool {
        matches!(
            self,
            Self::GpuTemp | Self::GpuPower | Self::GpuBusy | Self::FanCpu | Self::FanGpu
        )
    }

    #[must_use]
    pub fn line_hint(self) -> &'static str {
        match self.index() % 5 {
            1 => "штрих",
            2 => "короткий штрих",
            3 => "точки",
            4 => "штрих-точка",
            _ => "сплошная",
        }
    }

    fn line_pattern(self) -> &'static [f64] {
        match self.index() % 5 {
            1 => &[8.0, 4.0],
            2 => &[4.0, 3.0],
            3 => &[1.0, 4.0],
            4 => &[8.0, 3.0, 1.0, 3.0],
            _ => &[],
        }
    }
}

/// Общая шкала для величин с одинаковой единицей.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unit {
    Temperature,
    Ratio,
    Percent,
    Power,
    Rpm,
}

impl Unit {
    pub const ALL: [Self; 5] = [Self::Temperature, Self::Ratio, Self::Percent, Self::Power, Self::Rpm];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Ratio => "×",
            Self::Percent => "%",
            Self::Power => "Вт",
            Self::Rpm => "об/мин",
        }
    }

    #[must_use]
    pub fn index(self) -> i32 {
        match self {
            Self::Temperature => 0,
            Self::Ratio => 1,
            Self::Percent => 2,
            Self::Power => 3,
            Self::Rpm => 4,
        }
    }

    #[must_use]
    pub fn of(index: i32) -> Self {
        match index {
            1 => Self::Ratio,
            2 => Self::Percent,
            3 => Self::Power,
            4 => Self::Rpm,
            _ => Self::Temperature,
        }
    }
}

/// Раскладка графика по времени.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// Выбранные блоки × одна величина.
    #[default]
    Blocks,
    /// Один-два блока × все величины полосами.
    Params,
    /// Выбранные величины совместно; более двух единиц разделяются на панели.
    Overlay,
}

/// Ряды одного блока за день, по строкам (NaN — в строке нет); ряд, о котором не просили, пуст.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockDay {
    pub name: String,
    pub temp: Vec<f32>,
    pub ratio: Vec<f32>,
    pub ceiling: Vec<f32>,
    pub busy: Vec<f32>,
    /// Уровень кристалла: средняя температура остальных блоков (то же правило, что `samples::die_level`).
    pub level: Vec<f32>,
}

impl BlockDay {
    /// Ряд величины (нагрузка — долей, в проценты её переводит показ).
    #[must_use]
    pub fn series(&self, q: Quantity) -> &[f32] {
        match q {
            Quantity::Temp => &self.temp,
            Quantity::Ratio => &self.ratio,
            Quantity::Ceiling => &self.ceiling,
            Quantity::Busy => &self.busy,
            Quantity::Level => &self.level,
            Quantity::GpuTemp | Quantity::GpuPower | Quantity::GpuBusy | Quantity::FanCpu | Quantity::FanGpu => &[],
        }
    }

    fn series_mut(&mut self, q: Quantity) -> &mut Vec<f32> {
        match q {
            Quantity::Temp => &mut self.temp,
            Quantity::Ratio => &mut self.ratio,
            Quantity::Ceiling => &mut self.ceiling,
            Quantity::Busy => &mut self.busy,
            Quantity::Level => &mut self.level,
            Quantity::GpuTemp | Quantity::GpuPower | Quantity::GpuBusy | Quantity::FanCpu | Quantity::FanGpu => {
                unreachable!("shared series belong to Day")
            }
        }
    }
}

/// Непрерывный кусок записи: где он начинается на сжатой оси, его начало и конец по часам (мс).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stretch {
    pub x: i64,
    pub t0: i64,
    pub t1: i64,
}

/// День журнала: строки всех файлов дня по времени, их место на сжатой оси (только записанное время) и ряды.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Day {
    pub date: String,
    /// Мс по местным часам (`samples::row_time`), по возрастанию.
    pub t: Vec<i64>,
    /// Место строки на сжатой оси, мс: перерывы записи выброшены.
    pub x: Vec<i64>,
    /// Строка начинает новый кусок (перед ней — перерыв записи или начало дня).
    pub starts: Vec<bool>,
    pub stretches: Vec<Stretch>,
    pub blocks: Vec<BlockDay>,
    /// Какие ряды блоков прочитаны (по просьбе окна).
    pub quantities: Vec<Quantity>,
    pub gpu_temp: Vec<f32>,
    pub gpu_power: Vec<f32>,
    pub gpu_busy: Vec<f32>,
    pub fan_cpu: Vec<f32>,
    pub fan_gpu: Vec<f32>,
    /// Шаг записи строки, мс (250 или 2000) — по нему перерыв.
    step: Vec<i64>,
    /// Что не прочиталось (строка владельцу).
    pub trouble: Option<String>,
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "logged values have 2-3 decimals; f32 keeps 7 significant digits"
)]
fn stored(v: Option<f64>) -> f32 {
    v.map_or(f32::NAN, |v| v as f32)
}

impl Day {
    /// Начало и конец дня на сжатой оси; None — строк нет.
    #[must_use]
    pub fn bounds(&self) -> Option<(i64, i64)> {
        Some((*self.x.first()?, *self.x.last()?))
    }

    /// Время по часам для места на сжатой оси.
    #[must_use]
    pub fn real(&self, x: i64) -> Option<i64> {
        let k = self.stretches.partition_point(|s| s.x <= x).checked_sub(1)?;
        let s = self.stretches[k];
        Some((s.t0 + (x - s.x)).min(s.t1))
    }

    /// Место на сжатой оси для времени по часам: в куске — своё, в перерыве — конец куска перед ним.
    #[must_use]
    pub fn place(&self, t: i64) -> Option<i64> {
        let k = self.stretches.partition_point(|s| s.t0 <= t);
        if k == 0 {
            return self.stretches.first().map(|s| s.x);
        }
        let s = self.stretches[k - 1];
        Some(s.x + (t.min(s.t1) - s.t0))
    }

    /// Перерывы записи: место на сжатой оси, начало и конец по часам.
    #[must_use]
    pub fn gaps(&self) -> Vec<(i64, i64, i64)> {
        self.stretches.windows(2).map(|w| (w[1].x, w[0].t1, w[1].t0)).collect()
    }

    /// Строки по порядку времени, перерывы и сжатая ось. Строки с часами назад переставляются по времени.
    fn finish(&mut self) {
        if !self.t.windows(2).all(|w| w[0] <= w[1]) {
            let mut order: Vec<usize> = (0..self.t.len()).collect();
            order.sort_by_key(|&i| self.t[i]);
            let take = |v: &mut Vec<f32>| {
                if !v.is_empty() {
                    *v = order.iter().map(|&i| v[i]).collect();
                }
            };
            self.t = order.iter().map(|&i| self.t[i]).collect();
            self.step = order.iter().map(|&i| self.step[i]).collect();
            for b in &mut self.blocks {
                for q in Quantity::BLOCK {
                    take(b.series_mut(q));
                }
            }
            for v in [
                &mut self.gpu_temp,
                &mut self.gpu_power,
                &mut self.gpu_busy,
                &mut self.fan_cpu,
                &mut self.fan_gpu,
            ] {
                take(v);
            }
        }
        let n = self.t.len();
        self.x = Vec::with_capacity(n);
        self.starts = Vec::with_capacity(n);
        self.stretches.clear();
        let mut at = 0;
        for i in 0..n {
            let start = i == 0 || self.t[i] - self.t[i - 1] > 2 * self.step[i];
            if start {
                if let Some(last) = self.stretches.last_mut() {
                    last.t1 = self.t[i - 1];
                }
                self.stretches.push(Stretch {
                    x: at,
                    t0: self.t[i],
                    t1: self.t[i],
                });
            } else {
                at += self.t[i] - self.t[i - 1];
            }
            self.x.push(at);
            self.starts.push(start);
        }
        if let (Some(last), Some(&t)) = (self.stretches.last_mut(), self.t.last()) {
            last.t1 = t;
        }
    }
}

/// Читает день `date` из файлов `names` (файл дня и его продолжения, по порядку) папки срезов: у блоков `blocks` (по
/// названию; блока нет в файле — его ряд в строках файла пуст) — ряды величин `wanted`, и видеокарту с вентиляторами.
#[must_use]
pub fn read_day(samples_dir: &Path, date: &str, names: &[String], blocks: &[String], wanted: &[Quantity]) -> Day {
    let mut day = Day {
        date: date.to_string(),
        quantities: wanted.to_vec(),
        blocks: blocks
            .iter()
            .map(|name| BlockDay {
                name: name.clone(), // a dozen names at most, per read
                ..BlockDay::default()
            })
            .collect(),
        ..Day::default()
    };
    let mut fields = Vec::new();
    for name in names {
        let text = match std::fs::read_to_string(samples_dir.join(name)) {
            Ok(text) => text,
            Err(e) => {
                day.trouble = Some(format!("срезы {name} не прочитаны: {e}"));
                continue;
            }
        };
        let mut lines = text.lines();
        let Some(columns) = lines.next().and_then(Columns::parse) else {
            continue;
        };
        let places: Vec<Option<usize>> = blocks.iter().map(|b| columns.block(b)).collect();
        for line in lines {
            let line = line.trim_end_matches('\r');
            if !columns.split(line, &mut fields) {
                continue;
            }
            let Some(at) = samples::row_time(&line[fields[0].0..fields[0].1]) else {
                continue;
            };
            push_row(&mut day, (&columns, line, &fields), at, &places, wanted);
        }
    }
    day.finish();
    day
}

fn push_row(
    day: &mut Day,
    (columns, line, fields): (&Columns, &str, &[(usize, usize)]),
    at: samples::RowTime,
    places: &[Option<usize>],
    wanted: &[Quantity],
) {
    day.t.push(at.ms);
    day.step.push(at.step);
    let (mut sum, mut known) = (0.0, 0);
    if wanted.contains(&Quantity::Level) {
        for b in 0..columns.blocks() {
            if let Some(t) = columns.value(line, fields, b, What::Temp) {
                sum += t;
                known += 1;
            }
        }
    }
    let others = columns.blocks().saturating_sub(1);
    for (into, &block) in day.blocks.iter_mut().zip(places) {
        let value = |what| block.and_then(|b| columns.value(line, fields, b, what));
        for &q in wanted.iter().filter(|q| !q.global()) {
            let v = match q {
                Quantity::Temp => value(What::Temp),
                Quantity::Ratio => value(What::Ratio),
                Quantity::Ceiling => value(What::Ceiling),
                Quantity::Busy => value(What::Busy),
                Quantity::Level => value(What::Temp).and_then(|t| samples::die_level(sum - t, known - 1, others)),
                Quantity::GpuTemp | Quantity::GpuPower | Quantity::GpuBusy | Quantity::FanCpu | Quantity::FanGpu => {
                    None
                }
            };
            into.series_mut(q).push(stored(v));
        }
    }
    let shared = |what| stored(columns.shared(line, fields, what));
    day.gpu_temp.push(shared(Shared::TempGpu));
    day.gpu_power.push(shared(Shared::PowerGpu));
    day.gpu_busy.push(shared(Shared::BusyGpu));
    day.fan_cpu.push(shared(Shared::FanCpu));
    day.fan_gpu.push(shared(Shared::FanGpu));
}

// ----- прореживание -----

/// Точка ряда после прореживания: место на сжатой оси, значение и «перо поднято перед ней» (перерыв записи или пустые
/// строки).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub at: i64,
    pub v: f32,
    pub pen_up: bool,
}

/// Прореживание M4: показ `view` (сжатая ось) делится на `columns` столбцов пикселей; в каждом — первая, наименьшая,
/// наибольшая и последняя строка (по времени, без повторов). За краями показа — по одной соседней строке, чтобы линия
/// доходила до края. Перо поднимается в начале куска записи (`starts`); у плотного ряда (`sparse` — нет) — и на пустом
/// значении; у редкого (видеокарта, вентиляторы) пустые строки пропускаются, а между чтениями дальше `SPARSE_GAP_MS`
/// линии нет.
pub fn decimate(
    x: &[i64],
    starts: &[bool],
    v: &[f32],
    view: (i64, i64),
    columns: usize,
    sparse: bool,
    out: &mut Vec<Point>,
) {
    out.clear();
    let (from, to) = view;
    if columns == 0 || to <= from || x.is_empty() || v.len() != x.len() {
        return;
    }
    let lo = x.partition_point(|&a| a < from).saturating_sub(1);
    let hi = (x.partition_point(|&a| a <= to) + 1).min(x.len());
    #[expect(
        clippy::cast_precision_loss,
        reason = "a few thousand pixel columns, milliseconds of a day"
    )]
    let width = (to - from) as f64 / columns as f64;
    let last_column = f64::from(u32::try_from(columns).unwrap_or(u32::MAX));
    let bucket_of = |at: i64| -> i64 {
        #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
        let k = ((at - from) as f64 / width).floor().clamp(-1.0, last_column);
        #[expect(clippy::cast_possible_truncation, reason = "clamped to the column count first")]
        let k = k as i64;
        k
    };
    // Столбец: (номер, первая, наименьшая, наибольшая, последняя — номера строк).
    let mut open: Option<(i64, [usize; 4])> = None;
    let mut pen_up = true;
    let mut last_at: Option<i64> = None;
    let flush = |open: &mut Option<(i64, [usize; 4])>, pen_up: &mut bool, out: &mut Vec<Point>| {
        if let Some((_, mut rows)) = open.take() {
            rows.sort_unstable();
            let mut previous = None;
            for r in rows {
                if previous == Some(r) {
                    continue;
                }
                previous = Some(r);
                out.push(Point {
                    at: x[r],
                    v: v[r],
                    pen_up: *pen_up,
                });
                *pen_up = false;
            }
        }
    };
    for i in lo..hi {
        if starts.get(i).copied().unwrap_or(false) {
            flush(&mut open, &mut pen_up, out);
            pen_up = true;
            last_at = None;
        }
        if !v[i].is_finite() {
            if !sparse {
                flush(&mut open, &mut pen_up, out);
                pen_up = true;
                last_at = None;
            }
            continue;
        }
        if sparse && last_at.is_some_and(|l| x[i] - l > SPARSE_GAP_MS) {
            flush(&mut open, &mut pen_up, out);
            pen_up = true;
        }
        last_at = Some(x[i]);
        let k = bucket_of(x[i]);
        match &mut open {
            Some((bucket, rows)) if *bucket == k => {
                if v[i] < v[rows[1]] {
                    rows[1] = i;
                }
                if v[i] > v[rows[2]] {
                    rows[2] = i;
                }
                rows[3] = i;
            }
            _ => {
                flush(&mut open, &mut pen_up, out);
                open = Some((k, [i; 4]));
            }
        }
    }
    flush(&mut open, &mut pen_up, out);
}

/// Линия обзора: среднее по времени на четыре точки окна; разрывы остаются разрывами.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "day milliseconds and pixel buckets; averaged sample values remain f32 like the source"
)]
fn trend(day: &Day, values: &[f32], view: (i64, i64), pixels: usize, sparse: bool, out: &mut Vec<Point>) {
    out.clear();
    if values.len() != day.x.len() || pixels == 0 || view.1 <= view.0 {
        return;
    }
    let first = day.x.partition_point(|&x| x < view.0).saturating_sub(1);
    let end = (day.x.partition_point(|&x| x <= view.1) + 1).min(day.x.len());
    if end.saturating_sub(first) <= pixels {
        raw_points(day, values, first..end, sparse, out);
        return;
    }
    let buckets = (pixels / 4).max(1) as f64;
    let bucket_width = (view.1 - view.0) as f64 / buckets;
    // Номер столбца, взвешенные время и значение, сумма длительностей.
    let mut open: Option<(i64, f64, f64, f64)> = None;
    let mut pen_up = true;
    let mut previous: Option<usize> = None;
    let flush = |open: &mut Option<(i64, f64, f64, f64)>, pen_up: &mut bool, out: &mut Vec<Point>| {
        if let Some((_, time, value, weight)) = open.take() {
            out.push(Point {
                at: (time / weight).round() as i64,
                v: (value / weight) as f32,
                pen_up: *pen_up,
            });
            *pen_up = false;
        }
    };
    for (i, &value) in values.iter().enumerate().take(end).skip(first) {
        if day.starts.get(i).copied().unwrap_or(false) {
            flush(&mut open, &mut pen_up, out);
            pen_up = true;
            previous = None;
        }
        if !value.is_finite() {
            if !sparse {
                flush(&mut open, &mut pen_up, out);
                pen_up = true;
                previous = None;
            }
            continue;
        }
        let elapsed = previous.map(|j| day.x[i] - day.x[j]);
        if sparse && elapsed.is_some_and(|elapsed| elapsed > SPARSE_GAP_MS) {
            flush(&mut open, &mut pen_up, out);
            pen_up = true;
            previous = None;
        }
        let nominal = day.step.get(i).copied().unwrap_or(250).max(1);
        let weight = previous.map_or(nominal, |j| {
            let elapsed = day.x[i] - day.x[j];
            if elapsed > 0 && (sparse || elapsed <= nominal * 3 / 2) {
                elapsed
            } else {
                nominal
            }
        }) as f64;
        previous = Some(i);
        let at = day.x[i];
        let bucket = (((at - view.0) as f64 / bucket_width).floor().clamp(-1.0, buckets)) as i64;
        if open.as_ref().is_some_and(|a| a.0 != bucket) {
            flush(&mut open, &mut pen_up, out);
        }
        let item = open.get_or_insert((bucket, 0.0, 0.0, 0.0));
        item.1 += at as f64 * weight;
        item.2 += f64::from(value) * weight;
        item.3 += weight;
    }
    flush(&mut open, &mut pen_up, out);
}

fn raw_points(day: &Day, values: &[f32], rows: std::ops::Range<usize>, sparse: bool, out: &mut Vec<Point>) {
    let mut pen_up = true;
    let mut previous: Option<usize> = None;
    for (i, &v) in values.iter().enumerate().take(rows.end).skip(rows.start) {
        if day.starts.get(i).copied().unwrap_or(false) {
            pen_up = true;
            previous = None;
        }
        if !v.is_finite() {
            if !sparse {
                pen_up = true;
                previous = None;
            }
            continue;
        }
        if sparse && previous.is_some_and(|j| day.x[i] - day.x[j] > SPARSE_GAP_MS) {
            pen_up = true;
        }
        out.push(Point {
            at: day.x[i],
            v,
            pen_up,
        });
        pen_up = false;
        previous = Some(i);
    }
}

// ----- картинка полосы -----

/// Картинка полосы: RGBA без предумножения, строки сверху вниз; где рядов нет — прозрачно.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Picture {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Picture {
        Picture {
            width,
            height,
            rgba: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    /// Альфа пикселя (0…255); за краями — 0.
    #[must_use]
    pub fn alpha(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.rgba[((y as usize) * (self.width as usize) + x as usize) * 4 + 3]
    }

    /// Ряд из маски — поверх нарисованного, густотой `alpha` (0…1): каждый задетый пиксель один раз, как бы часто его
    /// ни задели шаги линии. Следующий ряд ложится поверх прежних, но при густоте меньше 1 прежние видны сквозь него.
    fn blend(&mut self, mask: &Mask, color: u32, alpha: f32) {
        let [_, red, green, blue] = color.to_be_bytes();
        for (k, _) in mask.hit.iter().enumerate().filter(|(_, hit)| **hit) {
            let dst = &mut self.rgba[k * 4..k * 4 + 4];
            let below = f32::from(dst[3]) / 255.0;
            let out = alpha + below * (1.0 - alpha);
            let mix = |src: u8, d: u8| byte((f32::from(src) * alpha + f32::from(d) * below * (1.0 - alpha)) / out);
            dst[0] = mix(red, dst[0]);
            dst[1] = mix(green, dst[1]);
            dst[2] = mix(blue, dst[2]);
            dst[3] = byte(out * 255.0);
        }
    }
}

/// Какие пиксели картинки задел один ряд: ряд рисуется сюда, затем смешивается с картинкой один раз (`Picture::blend`),
/// чтобы полупрозрачная толстая или ломаная линия не темнела там, где её шаги ложатся друг на друга.
#[derive(Debug)]
struct Mask {
    width: u32,
    height: u32,
    hit: Vec<bool>,
}

impl Mask {
    fn new(width: u32, height: u32) -> Mask {
        Mask {
            width,
            height,
            hit: vec![false; (width as usize) * (height as usize)],
        }
    }

    fn clear(&mut self) {
        self.hit.fill(false);
    }

    /// Пиксель задет; за краями — ничего.
    fn plot(&mut self, x: i64, y: i64) {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return;
        };
        if x < self.width && y < self.height {
            self.hit[(y as usize) * (self.width as usize) + x as usize] = true;
        }
    }

    /// Точка толщиной `thick` пикселей у места (x, y).
    fn dot(&mut self, (x, y): (f64, f64), thick: i64) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "pixel coordinates; plot drops what is outside"
        )]
        let (x, y) = (x.round() as i64, y.round() as i64);
        for dx in 0..thick {
            for dy in 0..thick {
                self.plot(x + dx, y + dy);
            }
        }
    }

    /// Отрезок шагами не длиннее пикселя: задет каждый столбец и каждая строка между концами (вертикальный размах
    /// столбца прореживания виден целиком).
    fn line(&mut self, a: (f64, f64), b: (f64, f64), thick: i64) {
        let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil().clamp(1.0, 100_000.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 1..=100000 first"
        )]
        let n = steps as u32;
        for k in 0..=n {
            let s = f64::from(k) / f64::from(n);
            self.dot((a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s), thick);
        }
    }
}

/// Доля 0…255 в байт.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 first"
)]
fn byte(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

// ----- масштаб и сдвиг -----

/// Колесо мыши над полем: курсор на доле `at` (0…1 слева направо) остаётся на том же месте оси, показ сужается
/// (`delta` > 0 — колесо от себя) или расширяется; не уже `MIN_SPAN_MS` и не шире дня (`bounds`). Упёрся в край дня —
/// показ сдвигается внутрь (тогда место под курсором смещается: дальше края данных нет).
#[must_use]
pub fn zoom(view: (i64, i64), bounds: (i64, i64), at: f64, delta: f64) -> (i64, i64) {
    let factor = WHEEL_ZOOM.powf(delta / WHEEL_STEP);
    if at >= 1.0 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "elapsed milliseconds across the i64 timestamp domain"
        )]
        let width = (i128::from(view.1) - i128::from(view.0)).max(1) as f64;
        #[expect(clippy::cast_precision_loss, reason = "the minimum wheel window is ten seconds")]
        let wanted = (width * factor).max(MIN_SPAN_MS as f64);
        return zoom_end(view, bounds, wanted / width);
    }
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let span = (view.1 - view.0) as f64;
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let whole = (bounds.1 - bounds.0).max(MIN_SPAN_MS) as f64;
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let least = (MIN_SPAN_MS as f64).min(whole);
    let wanted = (span * factor).clamp(least, whole);
    let at = at.clamp(0.0, 1.0);
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let cursor = view.0 as f64 + at * span;
    #[expect(clippy::cast_possible_truncation, reason = "milliseconds of a day, rounded")]
    let from = (cursor - at * wanted).round() as i64;
    #[expect(clippy::cast_possible_truncation, reason = "milliseconds of a day, rounded")]
    let wanted = wanted.round() as i64;
    inside((from, from + wanted), bounds)
}

/// Перетаскивание: поле сдвинули на долю `shift` своей ширины (> 0 — вправо, к более раннему времени).
#[must_use]
pub fn pan(view: (i64, i64), bounds: (i64, i64), shift: f64) -> (i64, i64) {
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let span = (view.1 - view.0) as f64;
    #[expect(clippy::cast_possible_truncation, reason = "milliseconds of a day, rounded")]
    let by = (shift * span).round() as i64;
    inside((view.0 - by, view.1 - by), bounds)
}

/// Последний отсчёт остаётся у края, ширина показа сохраняется до размера всех данных.
#[must_use]
pub fn follow(view: (i64, i64), bounds: (i64, i64)) -> (i64, i64) {
    let (lo, hi, width) = bounded_width(view, bounds);
    narrow_window(hi - width, width, lo, hi)
}

/// Масштаб от конца видимого отрезка: прошлый правый край остаётся тем же даже у начала записи.
#[must_use]
pub fn zoom_end(view: (i64, i64), bounds: (i64, i64), factor: f64) -> (i64, i64) {
    let (lo, hi, _) = bounded_width(view, bounds);
    let end = i128::from(view.1).clamp(lo, hi);
    let available = end - lo;
    if available == 0 {
        return narrow_window(end, 0, lo, hi);
    }
    let width = (i128::from(view.1) - i128::from(view.0)).clamp(1, available);
    let desired = if !factor.is_finite() || factor <= 0.0 || (factor - 1.0).abs() < f64::EPSILON {
        width
    } else {
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "floating scale factor, then exact bounds and endpoint reconstruction in i128"
        )]
        let scaled = (width as f64 * factor).clamp(1.0, available as f64).round() as i128;
        scaled.clamp(1, available)
    };
    narrow_window(end - desired, desired, lo, end)
}

/// Начало показа — доля всей истории; у конца сохраняется ширина окна, а не пустое место.
#[must_use]
pub fn scroll(view: (i64, i64), bounds: (i64, i64), start_fraction: f64) -> (i64, i64) {
    let (lo, hi, width) = bounded_width(view, bounds);
    let fraction = if start_fraction.is_finite() {
        start_fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a fraction of at most the complete i64 domain, then bounded exactly with i128"
    )]
    let offset = ((hi - lo) as f64 * fraction).round() as i128;
    narrow_window(lo + offset, width, lo, hi)
}

fn bounded_width(view: (i64, i64), bounds: (i64, i64)) -> (i128, i128, i128) {
    let lo = i128::from(bounds.0.min(bounds.1));
    let hi = i128::from(bounds.0.max(bounds.1));
    let width = (i128::from(view.1) - i128::from(view.0)).clamp(0, hi - lo);
    (lo, hi, width)
}

fn narrow_window(from: i128, width: i128, lo: i128, hi: i128) -> (i64, i64) {
    let width = width.clamp(0, hi - lo);
    let from = from.clamp(lo, hi - width);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "both endpoints stay inside bounds originally supplied as i64"
    )]
    let result = (from as i64, (from + width) as i64);
    result
}

/// Показ шириной `span` с местом `x` посередине (у края дня — сдвинутый внутрь).
#[must_use]
pub fn centred(x: i64, span: i64, bounds: (i64, i64)) -> (i64, i64) {
    inside((x - span / 2, x - span / 2 + span), bounds)
}

/// Показ той же ширины, сдвинутый внутрь дня.
fn inside((from, to): (i64, i64), (lo, hi): (i64, i64)) -> (i64, i64) {
    let span = (to - from).min((hi - lo).max(MIN_SPAN_MS));
    let from = from.max(lo).min(hi.max(lo + span) - span);
    (from, from + span)
}

// ----- что нарисовать -----

/// Горизонтальная линия полосы (цель, аварийная): доля высоты полосы снизу, цвет, подпись.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RefShown {
    pub f: f32,
    pub color: u32,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripShown {
    pub title: String,
    pub unit: Option<Unit>,
    pub right_unit: Option<Unit>,
    /// Деления: доля высоты снизу и подпись.
    pub ticks: Vec<(f32, String)>,
    pub right_ticks: Vec<(f32, String)>,
    pub picture: Picture,
    pub refs: Vec<RefShown>,
    /// Ось значений: (низ, верх); None — данных в показе нет.
    pub range: Option<(f64, f64)>,
    pub right_range: Option<(f64, f64)>,
}

/// Отметка на оси времени (событие): доля ширины, цвет.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkShown {
    pub f: f32,
    pub color: u32,
}

/// Отметка события на линии блока (раскладка «блоки»): доли поля полосы (x слева, y снизу), цвет, вид: 1 — опускает
/// потолок (отсечка), 2 — остальное.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DotShown {
    pub x: f32,
    pub y: f32,
    pub color: u32,
    pub kind: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeShown {
    pub swap: bool,
    pub parameter_ranges: BTreeMap<Quantity, Range>,
    pub strips: Vec<StripShown>,
    /// Деления оси времени: доля ширины и подпись.
    pub ticks: Vec<(f32, String)>,
    /// События под полосами (раскладка «параметры» и события без блока на линии).
    pub marks: Vec<MarkShown>,
    pub dots: Vec<DotShown>,
    /// Перерывы записи — тонкие отметки через все полосы: доли ширины.
    pub gaps: Vec<f32>,
    /// Строка над полосами: день, показанный отрезок по часам, сколько перерывов сжато.
    pub head: String,
    pub empty: String,
}

/// Блок показа: название для подписей и цвет его линий.
#[derive(Clone, Debug)]
pub struct Shown<'a> {
    pub label: &'a str,
    pub color: u32,
}

/// Событие на графике: когда (мс по часам), цвет, опускает ли потолок, чей блок (номер среди показанных; None — не
/// показанного блока или всех), подсказка.
#[derive(Clone, Debug)]
pub struct Act {
    pub at: i64,
    pub color: u32,
    pub cut: bool,
    pub block: Option<usize>,
    pub tip: String,
}

/// Что нужно, чтобы нарисовать день.
#[derive(Debug)]
pub struct TimeInput<'a> {
    pub day: &'a Day,
    /// Показ на сжатой оси, мс.
    pub view: (i64, i64),
    pub layout: Layout,
    pub quantity: Quantity,
    /// None сохраняет старый одиночный выбор; пустой набор скрывает все ряды.
    pub selected: Option<&'a [Quantity]>,
    pub manual_ranges: Option<&'a BTreeMap<Unit, Range>>,
    pub swap: bool,
    pub normalized: bool,
    pub parameter_ranges: Option<&'a BTreeMap<Quantity, Range>>,
    /// Поле в пикселях картинки: ширина — столбцы прореживания; высота — всё поле (полосы делят её).
    pub columns: usize,
    pub height: usize,
    /// Пикселей картинки на точку окна (масштаб экрана).
    pub scale: f64,
    pub blocks: &'a [Shown<'a>],
    /// Цель и аварийная отсечка автомодуля, °C (линиями на полосе температуры).
    pub limit: Option<f64>,
    pub emergency: Option<f64>,
    pub events: &'a [Act],
}

const TARGET: u32 = 0x00ff_b454;
const EMERGENCY: u32 = 0x00ff_5d5d;
const GPU: u32 = 0x009a_d46a;
const FAN_CPU: u32 = 0x00c8_ccd4;
const FAN_GPU: u32 = 0x0079_808c;
/// Густота линии блока, когда блоков на полосе несколько: под ней видны линии других блоков.
const SHARED_ALPHA: f32 = 0.8;
/// Густота ступеней потолка под линией множителя.
const CEILING_ALPHA: f32 = 0.5;
/// Делений оси значений — одно на столько точек высоты полосы (от двух до восьми).
const TICK_POINTS: f64 = 40.0;

/// Горизонтальная линия полосы: значение, цвет, подпись.
type Level = (f64, u32, String);

/// Ряд полосы: значения, множитель значения (нагрузка — в %), цвет, редкий ли, ступенями ли (потолок), толщина
/// (пиксели картинки) и густота.
struct Row<'a> {
    v: &'a [f32],
    times: f32,
    color: u32,
    sparse: bool,
    steps: bool,
    thick: i64,
    alpha: f32,
    unit: Unit,
    quantity: Quantity,
}

/// Полоса до рисования: подпись, ряды, горизонтальные линии.
struct Plan<'a> {
    title: String,
    rows: Vec<Row<'a>>,
    refs: Vec<Level>,
    units: Vec<Unit>,
}

/// Высота полосы в пикселях картинки при `n` полосах на поле высотой `height`.
#[must_use]
pub fn strip_height(height: usize, n: usize, scale: f64) -> usize {
    #[expect(clippy::cast_precision_loss, reason = "a few hundred pixels")]
    let free = height as f64 - STRIP_GAP * scale * n.saturating_sub(1) as f64;
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a few hundred pixels, at least 8"
    )]
    let one = (free / n.max(1) as f64).floor().max(8.0) as usize;
    one
}

/// Что нарисовать: полосы раскладки, деления оси времени, перерывы и события.
#[must_use]
pub fn time_panel(input: &TimeInput) -> TimeShown {
    let day = input.day;
    let mut shown = TimeShown {
        swap: input.swap,
        ..TimeShown::default()
    };
    let (Some(from), Some(to)) = (day.real(input.view.0), day.real(input.view.1)) else {
        shown.empty = day
            .trouble
            .clone()
            .unwrap_or_else(|| format!("За {} срезов нет.", short_date(&day.date)));
        return shown;
    };
    let view = input.view;
    let inside = |x: i64| (view.0..=view.1).contains(&x);
    let gaps: Vec<i64> = day.gaps().iter().map(|g| g.0).filter(|&x| inside(x)).collect();
    shown.head = format!(
        "{} · {}–{}{}",
        short_date(&day.date),
        clock(from, 60_000),
        clock(to, 60_000),
        if gaps.is_empty() {
            String::new()
        } else {
            format!(" · перерывов записи сжато: {}", gaps.len())
        }
    );
    shown.parameter_ranges = parameter_ranges(input);
    let plans = plans(input);
    if input.selected.is_some_and(<[Quantity]>::is_empty) {
        shown.empty = "Выбери величины для графика.".into();
    }
    if input.layout == Layout::Overlay && plans.len() > 1 && !input.normalized {
        shown.head.push_str(" · больше двух единиц: отдельные панели");
    }
    let height = if input.swap {
        input.height
    } else {
        strip_height(input.height, plans.len(), input.scale)
    };
    let width = if input.swap {
        strip_height(input.columns, plans.len(), input.scale)
    } else {
        input.columns
    };
    let mut points = Vec::new();
    for plan in &plans {
        shown
            .strips
            .push(strip(plan, input, width, height, &shown.parameter_ranges, &mut points));
    }
    shown.ticks = time_ticks(
        day,
        view,
        if input.swap { input.height } else { input.columns },
        input.scale,
    );
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let share = |x: i64| (x - view.0) as f64 / (view.1 - view.0).max(1) as f64;
    shown.gaps = gaps.iter().map(|&x| crate::panels::f(share(x))).collect();
    let range = shown.strips.first().and_then(|s| s.range);
    for e in input.events {
        let Some(x) = day.place(e.at).filter(|&x| inside(x)) else {
            continue;
        };
        let on_line = (input.layout == Layout::Blocks && input.selected.is_none())
            .then(|| {
                let b = day.blocks.get(e.block?)?;
                let row = nearest_row(&day.x, x)?;
                let v = f64::from(*b.series(input.quantity).get(row)?) * f64::from(scale_of(input.quantity));
                let (lo, hi) = range?;
                v.is_finite().then(|| ((v - lo) / (hi - lo)).clamp(0.0, 1.0))
            })
            .flatten();
        match on_line {
            Some(y) => shown.dots.push(DotShown {
                x: crate::panels::f(if input.swap { y } else { share(x) }),
                y: crate::panels::f(if input.swap { share(x) } else { y }),
                color: e.color,
                kind: if e.cut { 1 } else { 2 },
            }),
            None => shown.marks.push(MarkShown {
                f: crate::panels::f(share(x)),
                color: e.color,
            }),
        }
    }
    shown
}

/// Нагрузка показывается в процентах.
fn scale_of(q: Quantity) -> f32 {
    if matches!(q, Quantity::Busy | Quantity::GpuBusy) {
        100.0
    } else {
        1.0
    }
}

/// Толщина линии в пикселях картинки.
fn thickness(scale: f64, points: f64) -> i64 {
    #[expect(clippy::cast_possible_truncation, reason = "one to four pixels")]
    let px = (scale * points).round().max(1.0) as i64;
    px
}

/// Полосы раскладки.
fn plans<'a>(input: &TimeInput<'a>) -> Vec<Plan<'a>> {
    if let Some(selected) = input.selected {
        return selected_plans(input, selected);
    }
    let day = input.day;
    let colour = |k: usize| input.blocks.get(k).map_or(FAN_CPU, |b| b.color);
    let thick = thickness(input.scale, 1.5);
    let row = |v: &'a [f32], color: u32| Row {
        v,
        times: 1.0,
        color,
        sparse: false,
        steps: false,
        thick,
        alpha: 1.0,
        unit: Unit::Temperature,
        quantity: Quantity::Temp,
    };
    let shared = if day.blocks.len() > 1 { SHARED_ALPHA } else { 1.0 };
    let mut refs = Vec::new();
    if let Some(l) = input.limit {
        refs.push((l, TARGET, format!("цель {l:.0}°")));
    }
    if let Some(e) = input.emergency {
        refs.push((e, EMERGENCY, format!("аварийная {e:.0}°")));
    }
    let of = |q: Quantity| -> Vec<Row<'a>> {
        day.blocks
            .iter()
            .enumerate()
            .map(|(k, b)| Row {
                times: scale_of(q),
                steps: q == Quantity::Ceiling,
                alpha: shared,
                unit: q.unit(),
                quantity: q,
                ..row(b.series(q), colour(k))
            })
            .collect()
    };
    let plan = |title: &str, rows: Vec<Row<'a>>, refs: Vec<Level>| {
        let units = vec![rows.first().map_or(Unit::Temperature, |r| r.unit)];
        Plan {
            title: title.to_string(),
            rows,
            refs,
            units,
        }
    };
    match input.layout {
        Layout::Blocks | Layout::Overlay => vec![plan(
            input.quantity.title(),
            of(input.quantity),
            if input.quantity == Quantity::Temp {
                refs
            } else {
                Vec::new()
            },
        )],
        Layout::Params => {
            let sparse = |v: &'a [f32], color: u32, unit: Unit| Row {
                sparse: true,
                unit,
                ..row(v, color)
            };
            vec![
                plan(Quantity::Temp.title(), of(Quantity::Temp), refs),
                plan("множитель · потолок ступенями", legacy_ratio_rows(input), Vec::new()),
                plan(Quantity::Busy.title(), of(Quantity::Busy), Vec::new()),
                plan(Quantity::Level.title(), of(Quantity::Level), Vec::new()),
                plan(
                    "видеокарта, °C",
                    vec![sparse(&day.gpu_temp, GPU, Unit::Temperature)],
                    Vec::new(),
                ),
                plan(
                    "видеокарта, Вт",
                    vec![sparse(&day.gpu_power, GPU, Unit::Power)],
                    Vec::new(),
                ),
                plan(
                    "вентиляторы, об/мин: процессора · видеокарты",
                    vec![
                        sparse(&day.fan_cpu, FAN_CPU, Unit::Rpm),
                        sparse(&day.fan_gpu, FAN_GPU, Unit::Rpm),
                    ],
                    Vec::new(),
                ),
            ]
        }
    }
}

fn legacy_ratio_rows<'a>(input: &TimeInput<'a>) -> Vec<Row<'a>> {
    let mut rows = Vec::new();
    for (k, b) in input.day.blocks.iter().enumerate() {
        let color = input.blocks.get(k).map_or(FAN_CPU, |b| b.color);
        for (quantity, v) in [(Quantity::Ceiling, &b.ceiling), (Quantity::Ratio, &b.ratio)] {
            let steps = quantity == Quantity::Ceiling;
            rows.push(Row {
                v,
                times: 1.0,
                color,
                sparse: false,
                steps,
                thick: thickness(input.scale, if steps { 1.0 } else { 1.5 }),
                alpha: if steps {
                    CEILING_ALPHA
                } else if input.day.blocks.len() > 1 {
                    SHARED_ALPHA
                } else {
                    1.0
                },
                unit: Unit::Ratio,
                quantity,
            });
        }
    }
    rows
}

fn global_series(day: &Day, q: Quantity) -> &[f32] {
    match q {
        Quantity::GpuTemp => &day.gpu_temp,
        Quantity::GpuPower => &day.gpu_power,
        Quantity::GpuBusy => &day.gpu_busy,
        Quantity::FanCpu => &day.fan_cpu,
        Quantity::FanGpu => &day.fan_gpu,
        _ => &[],
    }
}

fn parameter_ranges(input: &TimeInput) -> BTreeMap<Quantity, Range> {
    let first = input.day.x.partition_point(|&x| x < input.view.0);
    let end = input.day.x.partition_point(|&x| x <= input.view.1);
    let wanted = input.selected.unwrap_or_else(|| std::slice::from_ref(&input.quantity));
    let mut ranges = BTreeMap::new();
    for &q in wanted {
        let block = input
            .day
            .blocks
            .iter()
            .flat_map(|b| b.series(q).get(first..end).unwrap_or(&[]));
        let global = global_series(input.day, q).get(first..end).unwrap_or(&[]).iter();
        let values = block.chain(global).map(|&v| f64::from(v) * f64::from(scale_of(q)));
        let manual = input
            .parameter_ranges
            .and_then(|m| m.get(&q))
            .and_then(|r| Range::new(r.min, r.max));
        if let Some(range) = manual.or_else(|| Range::fit(values)) {
            ranges.insert(q, range);
        }
    }
    ranges
}

/// Выбор величин — единственный источник состава рядов и шкал.
fn selected_plans<'a>(input: &TimeInput<'a>, selected: &[Quantity]) -> Vec<Plan<'a>> {
    let mut groups: BTreeMap<(Unit, i32), Plan<'a>> = BTreeMap::new();
    for q in Quantity::ALL.into_iter().filter(|q| selected.contains(q)) {
        let unit = q.unit();
        let parameter = if input.layout == Layout::Params { q.index() } else { -1 };
        let plan = groups.entry((unit, parameter)).or_insert_with(|| Plan {
            title: String::new(),
            rows: Vec::new(),
            refs: Vec::new(),
            units: vec![unit],
        });
        if !plan.title.is_empty() {
            plan.title.push_str(" · ");
        }
        plan.title.push_str(q.title());
        let row = |v, color, alpha| Row {
            v,
            times: scale_of(q),
            color,
            sparse: q.global(),
            steps: q == Quantity::Ceiling,
            thick: thickness(input.scale, if q == Quantity::Ceiling { 1.0 } else { 1.5 }),
            alpha,
            unit,
            quantity: q,
        };
        if q.global() {
            let color = match q {
                Quantity::FanCpu => FAN_CPU,
                Quantity::FanGpu => FAN_GPU,
                _ => GPU,
            };
            plan.rows.push(row(global_series(input.day, q), color, 1.0));
        } else {
            for (k, b) in input.day.blocks.iter().enumerate() {
                let color = input.blocks.get(k).map_or(FAN_CPU, |b| b.color);
                plan.rows.push(row(b.series(q), color, SHARED_ALPHA));
            }
        }
        if q == Quantity::Temp {
            if let Some(v) = input.limit {
                plan.refs.push((v, TARGET, format!("цель {v:.0}°")));
            }
            if let Some(v) = input.emergency {
                plan.refs.push((v, EMERGENCY, format!("аварийная {v:.0}°")));
            }
        }
    }
    if input.layout != Layout::Params && (groups.len() <= 2 || input.normalized) {
        let mut combined = Plan {
            title: String::new(),
            rows: Vec::new(),
            refs: Vec::new(),
            units: Vec::new(),
        };
        for (_, mut group) in groups {
            if !combined.title.is_empty() {
                combined.title.push_str(" · ");
            }
            combined.title.push_str(&group.title);
            combined.rows.append(&mut group.rows);
            combined.refs.append(&mut group.refs);
            combined.units.append(&mut group.units);
        }
        if combined.rows.is_empty() {
            Vec::new()
        } else {
            vec![combined]
        }
    } else {
        groups.into_values().collect()
    }
}

fn unit_ranges(plan: &Plan, input: &TimeInput) -> BTreeMap<Unit, Range> {
    let first = input.day.x.partition_point(|&x| x < input.view.0);
    let end = input.day.x.partition_point(|&x| x <= input.view.1);
    let mut ranges = BTreeMap::new();
    for &unit in plan.units.iter().filter(|_| !input.normalized) {
        let values = plan.rows.iter().filter(|r| r.unit == unit).flat_map(|r| {
            r.v.get(first..end)
                .unwrap_or(&[])
                .iter()
                .map(move |&v| f64::from(v) * f64::from(r.times))
        });
        let auto = if input.selected.is_none() {
            Range::fit(values.chain(plan.refs.iter().map(|(v, _, _)| *v)))
        } else {
            Range::fit(values)
        };
        let manual = input
            .manual_ranges
            .and_then(|m| m.get(&unit))
            .and_then(|r| Range::new(r.min, r.max));
        if let Some(range) = manual.or(auto) {
            ranges.insert(unit, range);
        }
    }
    ranges
}

/// Одна полоса: диапазон из видимых исходных значений, общая проекция и отсечение.
fn strip(
    plan: &Plan,
    input: &TimeInput,
    columns: usize,
    height: usize,
    parameters: &BTreeMap<Quantity, Range>,
    points: &mut Vec<Point>,
) -> StripShown {
    let day = input.day;
    let (from, to) = input.view;
    let width = u32::try_from(columns).unwrap_or(1).max(2);
    let tall = u32::try_from(height).unwrap_or(8).max(8);
    let mut shown = StripShown {
        title: if input.normalized {
            format!("относительные шкалы · {}", plan.title)
        } else {
            plan.title.clone()
        },
        unit: if input.normalized {
            None
        } else {
            plan.units.first().copied()
        },
        right_unit: if input.normalized {
            None
        } else {
            plan.units.get(1).copied()
        },
        picture: Picture::new(width, tall),
        ..StripShown::default()
    };
    let ranges = unit_ranges(plan, input);
    let axis_pixels = if input.swap { width } else { tall };
    for (index, &unit) in plan.units.iter().filter(|_| !input.normalized).enumerate() {
        if let Some(&range) = ranges.get(&unit) {
            let ticks = value_ticks(range, axis_pixels, input.scale);
            if index == 0 {
                shown.range = Some((range.min, range.max));
                shown.ticks = ticks;
            } else {
                shown.right_range = Some((range.min, range.max));
                shown.right_ticks = ticks;
            }
        }
    }
    if input.normalized {
        let relative = Range { min: 0.0, max: 100.0 };
        shown.range = Some((relative.min, relative.max));
        shown.ticks = value_ticks(relative, axis_pixels, input.scale);
    }
    if ranges.is_empty() && !input.normalized || input.normalized && parameters.is_empty() {
        shown.title.push_str(" — нет данных");
        return shown;
    }
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let argument = Range::new(from as f64, to.max(from + 1) as f64);
    let Some(argument) = argument else {
        return shown;
    };
    let mut mask = Mask::new(width, tall);
    for r in &plan.rows {
        let Some(&value) = (if input.normalized {
            parameters.get(&r.quantity)
        } else {
            ranges.get(&r.unit)
        }) else {
            continue;
        };
        let viewport = Viewport {
            argument,
            value,
            swap: input.swap,
        };
        let pixels = if input.swap { height } else { columns };
        if input.selected.is_some() && !r.steps {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "positive physical pixels divided by the display scale"
            )]
            let logical_pixels = (pixels as f64 / input.scale.max(0.1)).round().max(1.0) as usize;
            trend(day, r.v, input.view, logical_pixels, r.sparse, points);
        } else {
            decimate(&day.x, &day.starts, r.v, input.view, pixels, r.sparse, points);
        }
        paint_row(&mut mask, r, points, viewport, input.scale);
        shown.picture.blend(&mask, r.color, r.alpha);
    }
    let reference_range = if input.normalized {
        parameters.get(&Quantity::Temp)
    } else {
        ranges.get(&Unit::Temperature)
    };
    shown.refs = reference_lines(&plan.refs, reference_range.copied());
    shown
}

fn reference_lines(refs: &[Level], range: Option<Range>) -> Vec<RefShown> {
    refs.iter()
        .filter_map(|(v, color, text)| {
            Some(RefShown {
                f: crate::panels::f(range?.normal(*v).filter(|f| (0.0..=1.0).contains(f))?),
                color: *color,
                text: text.clone(), // two lines per strip at most
            })
        })
        .collect()
}

fn paint_row(mask: &mut Mask, row: &Row, points: &[Point], viewport: Viewport, scale: f64) {
    let (width, height) = (mask.width, mask.height);
    let screen = |(x, y): (f64, f64)| (x * f64::from(width - 1), (1.0 - y) * f64::from(height - 1));
    mask.clear();
    let mut last: Option<(f64, f64)> = None;
    let mut phase = 0.0;
    for p in points {
        #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
        let here = (p.at as f64, f64::from(p.v) * f64::from(row.times));
        let mut line = |a, b| {
            if let Some((a, b)) = viewport.segment(a, b) {
                patterned_line(mask, screen(a), screen(b), row, scale, &mut phase);
            }
        };
        match last {
            Some(prev) if !p.pen_up => {
                if row.steps {
                    line(prev, (here.0, prev.1));
                    line((here.0, prev.1), here);
                } else {
                    line(prev, here);
                }
            }
            _ => {
                phase = 0.0;
                if let Some(p) = viewport
                    .point(here.0, here.1)
                    .filter(|p| (0.0..=1.0).contains(&p.0) && (0.0..=1.0).contains(&p.1))
                {
                    mask.dot(screen(p), row.thick);
                }
            }
        }
        last = Some(here);
    }
}

fn patterned_line(mask: &mut Mask, a: (f64, f64), b: (f64, f64), row: &Row, scale: f64, phase: &mut f64) {
    let pattern = row.quantity.line_pattern();
    if pattern.is_empty() {
        mask.line(a, b, row.thick);
        return;
    }
    let scale = scale.max(0.1);
    let cycle = pattern.iter().sum::<f64>() * scale;
    let length = (b.0 - a.0).hypot(b.1 - a.1);
    if length <= f64::EPSILON {
        return;
    }
    let mut offset = phase.rem_euclid(cycle);
    let mut piece = 0;
    while offset >= pattern[piece] * scale {
        offset -= pattern[piece] * scale;
        piece = (piece + 1) % pattern.len();
    }
    let mut along = 0.0;
    // Отрезок уже отсечён по полю; предел защищает от аномального размера окна.
    for _ in 0..8192 {
        let end = (along + pattern[piece] * scale - offset).min(length);
        if piece.is_multiple_of(2) {
            let at = |distance: f64| {
                (
                    a.0 + (b.0 - a.0) * distance / length,
                    a.1 + (b.1 - a.1) * distance / length,
                )
            };
            mask.line(at(along), at(end), row.thick);
        }
        along = end;
        if along >= length {
            break;
        }
        piece = (piece + 1) % pattern.len();
        offset = 0.0;
    }
    *phase = (*phase + length).rem_euclid(cycle);
}

fn value_ticks(range: Range, pixels: u32, scale: f64) -> Vec<(f32, String)> {
    let wanted = (f64::from(pixels) / scale.max(0.1) / TICK_POINTS).clamp(2.0, 8.0);
    let step = nice_step((range.max - range.min) / wanted);
    let mut tick = (range.min / step).ceil() * step;
    let mut ticks = Vec::new();
    while tick <= range.max && ticks.len() < 32 {
        let text = if step >= 1.0 {
            format!("{tick:.0}")
        } else {
            format!("{tick:.1}")
        };
        if let Some(f) = range.normal(tick) {
            ticks.push((crate::panels::f(f), text));
        }
        tick += step;
    }
    ticks
}

/// Шаг делений: 1, 2 или 5 × 10ⁿ, не меньше `rough`.
fn nice_step(rough: f64) -> f64 {
    if !rough.is_finite() || rough <= 0.0 {
        return 1.0;
    }
    let power = 10f64.powf(rough.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|k| k * power)
        .find(|&s| s >= rough)
        .unwrap_or(10.0 * power)
}

/// Шаги оси времени, мс: от секунды до шести часов.
const TIME_STEPS: [i64; 16] = [
    1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000, 900_000, 1_800_000, 3_600_000,
    7_200_000, 10_800_000, 21_600_000,
];

/// Деления оси времени: круглое время по часам внутри кусков записи; шаг — первый, при котором подписей не больше,
/// чем по одной на 90 точек ширины; подписи ближе половины шага друг к другу (у границы кусков) не ставятся.
fn time_ticks(day: &Day, (from, to): (i64, i64), columns: usize, scale: f64) -> Vec<(f32, String)> {
    let span = (to - from).max(1);
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a few thousand pixels"
    )]
    let room = ((columns as f64 / scale.max(0.1)) / 90.0).floor().max(1.0) as i64;
    let step = TIME_STEPS
        .iter()
        .copied()
        .find(|&s| span / s <= room)
        .unwrap_or(TIME_STEPS[TIME_STEPS.len() - 1]);
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let share = |x: i64| (x - from) as f64 / span as f64;
    let closest = span / room / 2;
    let mut out: Vec<(f32, String)> = Vec::new();
    let mut last: Option<i64> = None;
    for s in &day.stretches {
        if s.x + (s.t1 - s.t0) < from || s.x > to {
            continue;
        }
        let mut t = s.t0.div_euclid(step) * step;
        if t < s.t0 {
            t += step;
        }
        while t <= s.t1 {
            let x = s.x + (t - s.t0);
            if (from..=to).contains(&x) && last.is_none_or(|l| x - l >= closest) {
                out.push((crate::panels::f(share(x)), clock(t, step)));
                last = Some(x);
            }
            t += step;
        }
    }
    out
}

/// Время строки для подписи: «14:03» или «14:03:07», если шаг меньше минуты.
fn clock(ms: i64, step: i64) -> String {
    let s = ms.rem_euclid(86_400_000) / 1000;
    if step < 60_000 {
        format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 3600, s / 60 % 60)
    }
}

/// «2026-10-16» → «16.10».
#[must_use]
pub fn short_date(date: &str) -> String {
    match (date.get(5..7), date.get(8..10)) {
        (Some(m), Some(d)) => format!("{d}.{m}"),
        _ => date.to_string(),
    }
}

/// Мс → секунды.
#[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
fn seconds(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

/// Что показать под курсором: место на сжатой оси `x`, ширина «рядом» `near`.
#[derive(Debug)]
pub struct Under<'a> {
    pub x: i64,
    pub near: i64,
    pub layout: Layout,
    pub quantity: Quantity,
    pub selected: Option<&'a [Quantity]>,
    pub blocks: &'a [Shown<'a>],
    pub events: &'a [Act],
}

/// Подсказка под курсором: перерыв записи ближе `near` — его начало, конец и длительность; иначе время ближайшей строки
/// и значения в ней (в раскладке «блоки» — величина у каждого блока, в «параметрах» — все величины, видеокарта и
/// вентиляторы — ближайшее чтение не дальше 2 с); события ближе `near` к курсору.
#[must_use]
pub fn hover(day: &Day, under: &Under) -> String {
    let x = under.x;
    if let Some(&(_, start, end)) = day.gaps().iter().find(|g| (g.0 - x).abs() <= under.near) {
        return format!(
            "Перерыв записи {}–{} ({})\nЖурнал срезов не писался: CoreMapTray закрыт, компьютер выключен или запись выключена; на оси времени его нет.",
            clock(start, 1000),
            clock(end, 1000),
            crate::panels::span_of(seconds(end - start))
        );
    }
    let Some(row) = nearest_row(&day.x, x) else {
        return String::new();
    };
    let t = day.t[row];
    let mut out = format!("{}.{:03}", clock(t, 1000), t.rem_euclid(1000));
    let v = |s: &[f32]| s.get(row).copied().filter(|v| v.is_finite()).map(f64::from);
    let wanted: &[Quantity] = under.selected.unwrap_or(match under.layout {
        Layout::Blocks => std::slice::from_ref(&under.quantity),
        Layout::Params | Layout::Overlay => &Quantity::ALL,
    });
    for (b, shown) in day.blocks.iter().zip(under.blocks) {
        let parts: Vec<String> = wanted
            .iter()
            .filter(|q| !q.global())
            .filter_map(|&q| {
                let x = v(b.series(q))?;
                Some(match q {
                    Quantity::Temp => format!("{x:.0} °C"),
                    Quantity::Ratio => format!("×{x:.1}"),
                    Quantity::Ceiling => format!("потолок ×{x:.0}"),
                    Quantity::Busy => format!("нагрузка {:.0} %", x * 100.0),
                    Quantity::Level => format!("средняя остальных блоков {x:.1} °C"),
                    Quantity::GpuTemp
                    | Quantity::GpuPower
                    | Quantity::GpuBusy
                    | Quantity::FanCpu
                    | Quantity::FanGpu => return None,
                })
            })
            .collect();
        if !parts.is_empty() {
            let _ = write!(out, "\n{} — {}", shown.label, parts.join(" · ")); // into a String
        }
    }
    if let Some(selected) = under.selected {
        for q in Quantity::ALL.into_iter().filter(|q| q.global() && selected.contains(q)) {
            if let Some(value) = nearest_reading(&day.t, global_series(day, q), row) {
                let value = value * scale_of(q);
                let _ = write!(out, "\n{} — {value:.0}", q.title());
            }
        }
    } else if under.layout != Layout::Blocks {
        let reading = |s: &[f32]| nearest_reading(&day.t, s, row).map(f64::from);
        let gpu: Vec<String> = [
            reading(&day.gpu_temp).map(|v| format!("{v:.0} °C")),
            reading(&day.gpu_power).map(|v| format!("{v:.0} Вт")),
            reading(&day.gpu_busy).map(|v| format!("{:.0} %", v * 100.0)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !gpu.is_empty() {
            let _ = write!(out, "\nВидеокарта — {}", gpu.join(" · ")); // into a String
        }
        let fans: Vec<String> = [reading(&day.fan_cpu), reading(&day.fan_gpu)]
            .into_iter()
            .map(|v| v.map_or_else(|| "–".to_string(), |v| format!("{v:.0}")))
            .collect();
        if fans.iter().any(|f| f != "–") {
            let _ = write!(out, "\nВентиляторы — {} об/мин", fans.join(" / ")); // into a String
        }
    }
    for e in under.events {
        if day.place(e.at).is_some_and(|p| (p - x).abs() <= under.near) {
            let _ = write!(out, "\n{}", e.tip); // into a String
        }
    }
    out
}

/// Строка, ближайшая к месту `at` на оси `x`.
#[must_use]
pub fn nearest_row(x: &[i64], at: i64) -> Option<usize> {
    let k = x.partition_point(|&a| a < at);
    [k.checked_sub(1), (k < x.len()).then_some(k)]
        .into_iter()
        .flatten()
        .min_by_key(|&i| (x[i] - at).abs())
}

/// Ближайшее чтение редкого ряда к строке `row` не дальше `samples::NEAREST_MS` (по часам).
fn nearest_reading(t: &[i64], v: &[f32], row: usize) -> Option<f32> {
    let at = t[row];
    let before = (0..=row)
        .rev()
        .take_while(|&i| at - t[i] <= samples::NEAREST_MS)
        .find(|&i| v.get(i).is_some_and(|x| x.is_finite()));
    let after = (row..t.len())
        .take_while(|&i| t[i] - at <= samples::NEAREST_MS)
        .find(|&i| v.get(i).is_some_and(|x| x.is_finite()));
    [before, after]
        .into_iter()
        .flatten()
        .min_by_key(|&i| (t[i] - at).abs())
        .map(|i| v[i])
}

#[cfg(test)]
#[path = "navigation_spec.rs"]
mod navigation_spec;

#[cfg(test)]
mod tests {
    use super::*;

    /// День из времён строк (мс) и шагов записи: сжатая ось по тем же правилам, что у чтения.
    fn day_of(t: &[i64], step: i64) -> Day {
        let mut day = Day {
            t: t.to_vec(),
            step: vec![step; t.len()],
            ..Day::default()
        };
        day.finish();
        day
    }

    fn metric_day() -> Day {
        let mut day = day_of(&[0, 250, 500, 750, 1000], 250);
        day.blocks = (0..3)
            .map(|k| BlockDay {
                name: k.to_string(),
                temp: vec![60.0; 5],
                ratio: vec![40.0; 5],
                ceiling: vec![45.0; 5],
                busy: vec![0.5; 5],
                level: vec![55.0; 5],
            })
            .collect();
        day.gpu_temp = vec![65.0; 5];
        day.gpu_power = vec![80.0; 5];
        day.gpu_busy = vec![0.75; 5];
        day.fan_cpu = vec![3000.0; 5];
        day.fan_gpu = vec![3200.0; 5];
        day
    }

    fn metric_input<'a>(day: &'a Day, selected: &'a [Quantity]) -> TimeInput<'a> {
        TimeInput {
            day,
            view: (0, 1000),
            layout: Layout::Overlay,
            quantity: Quantity::Temp,
            selected: Some(selected),
            manual_ranges: None,
            swap: false,
            normalized: false,
            parameter_ranges: None,
            columns: 101,
            height: 101,
            scale: 1.0,
            blocks: &[],
            limit: None,
            emergency: None,
            events: &[],
        }
    }

    #[test]
    fn selected_metrics_share_units_and_global_readings_are_not_repeated_per_block() {
        let day = metric_day();
        let selected = [Quantity::Temp, Quantity::GpuTemp, Quantity::GpuBusy];
        let input = metric_input(&day, &selected);
        let plans = plans(&input);
        assert_eq!(plans.len(), 1, "две единицы накладываются на одном поле");
        assert_eq!(plans[0].rows.len(), 5, "три температуры блоков и два глобальных ряда");
        let shown = time_panel(&input);
        assert_eq!(shown.strips[0].unit, Some(Unit::Temperature));
        assert_eq!(shown.strips[0].right_unit, Some(Unit::Percent));
        let percent = shown.strips[0].right_range.expect("вторая шкала");
        assert!(
            percent.0 > 70.0 && percent.1 < 80.0,
            "проценты один раз, без принудительного нуля"
        );
        assert!(shown.strips[0].title.contains("видеокарта, °C"));
        assert!(shown.strips[0].title.contains("видеокарта, %"));
    }

    #[test]
    fn selected_panels_keep_every_block_and_empty_selection_draws_nothing() {
        let day = metric_day();
        let input = metric_input(&day, &Quantity::ALL);
        let shown = time_panel(&input);
        assert_eq!(shown.strips.len(), 5, "более двух единиц получают отдельные панели");
        assert!(shown.head.contains("больше двух единиц"));
        let panel_plans = plans(&TimeInput {
            layout: Layout::Params,
            ..input
        });
        assert_eq!(
            panel_plans[0].rows.len(),
            3,
            "все три блока в отдельной панели температуры"
        );
        assert_eq!(panel_plans.len(), Quantity::ALL.len(), "отдельная панель на параметр");
        let empty = time_panel(&metric_input(&day, &[]));
        assert_eq!(empty.strips, [] as [StripShown; 0]);
        assert!(empty.empty.contains("Выбери"));
    }

    #[test]
    fn parameter_panels_separate_metrics_even_when_their_units_match() {
        let day = metric_day();
        let input = metric_input(&day, &[Quantity::Temp, Quantity::Level]);
        assert_eq!(
            time_panel(&input).strips.len(),
            1,
            "совместная шкала градусов в наложении"
        );
        let separate = time_panel(&TimeInput {
            layout: Layout::Params,
            ..input
        });
        assert_eq!(separate.strips.len(), 2);
        assert_eq!(separate.strips[0].title, Quantity::Temp.title());
        assert_eq!(separate.strips[1].title, Quantity::Level.title());
        assert_ne!(
            separate.strips[0].range, separate.strips[1].range,
            "автодиапазон каждой величины свой"
        );
        let parameters = BTreeMap::from([
            (Quantity::Temp, Range { min: 0.0, max: 100.0 }),
            (Quantity::Level, Range { min: 40.0, max: 60.0 }),
        ]);
        let relative = time_panel(&TimeInput {
            layout: Layout::Params,
            normalized: true,
            parameter_ranges: Some(&parameters),
            ..input
        });
        assert_eq!(relative.strips.len(), 2);
        assert_eq!(relative.parameter_ranges, parameters);
        assert!(relative.strips.iter().all(|s| s.title.contains("относительные шкалы")));
    }

    #[test]
    fn manual_range_is_retained_when_axes_are_transposed() {
        let mut day = metric_day();
        day.blocks.truncate(1);
        day.blocks[0].temp.fill(25.0);
        let manual = BTreeMap::from([(Unit::Temperature, Range { min: 0.0, max: 100.0 })]);
        let input = TimeInput {
            manual_ranges: Some(&manual),
            ..metric_input(&day, &[Quantity::Temp])
        };
        let normal = time_panel(&input);
        let transposed = time_panel(&TimeInput { swap: true, ..input });
        assert_eq!(normal.strips[0].range, Some((0.0, 100.0)));
        assert_eq!(normal.strips[0].range, transposed.strips[0].range);
        assert_eq!(normal.strips[0].ticks, transposed.strips[0].ticks);
        assert!(normal.strips[0].picture.alpha(50, 75) > 0, "время по горизонтали");
        assert!(
            transposed.strips[0].picture.alpha(25, 50) > 0,
            "та же температура по горизонтали"
        );
        assert_eq!(
            transposed.strips[0].picture.alpha(75, 50),
            0,
            "не повторная подгонка диапазона"
        );
    }

    #[test]
    fn automatic_range_uses_raw_visible_values_and_manual_range_clips_excluded_series() {
        let mut day = metric_day();
        day.blocks.truncate(1);
        day.blocks[0].temp = vec![10.0, 20.0, 50.0, 20.0, 10.0];
        let input = metric_input(&day, &[Quantity::Temp]);
        let automatic = time_panel(&input);
        let range = automatic.strips[0].range.expect("шкала");
        assert!(
            range.0 < 10.0 && range.1 > 50.0,
            "сглаживание не прячет экстремумы от автодиапазона"
        );
        let manual = BTreeMap::from([(Unit::Temperature, Range { min: 60.0, max: 80.0 })]);
        for swap in [false, true] {
            let shown = time_panel(&TimeInput {
                swap,
                manual_ranges: Some(&manual),
                ..input
            });
            assert!(
                shown.strips[0].picture.rgba.iter().all(|&v| v == 0),
                "весь ряд за ручной шкалой скрыт"
            );
        }
    }

    #[test]
    fn relative_overlay_has_independent_parameter_ranges_and_original_unit_tooltips() {
        let day = metric_day();
        let parameters = BTreeMap::from([
            (Quantity::Ratio, Range { min: 30.0, max: 50.0 }),
            (Quantity::Ceiling, Range { min: 10.0, max: 70.0 }),
        ]);
        let input = TimeInput {
            normalized: true,
            parameter_ranges: Some(&parameters),
            ..metric_input(&day, &Quantity::ALL)
        };
        let shown = time_panel(&input);
        assert_eq!(shown.strips.len(), 1, "в относительном режиме все единицы совместимы");
        assert_eq!(
            shown.strips[0].unit, None,
            "относительная ось не выдает себя за физические проценты"
        );
        assert_eq!(shown.strips[0].right_unit, None);
        assert!(shown.strips[0].title.contains("относительные шкалы"));
        assert_eq!(
            shown.parameter_ranges.get(&Quantity::Ratio),
            parameters.get(&Quantity::Ratio)
        );
        assert_eq!(
            shown.parameter_ranges.get(&Quantity::Ceiling),
            parameters.get(&Quantity::Ceiling)
        );
        assert_eq!(shown.parameter_ranges.len(), Quantity::ALL.len());
        let tip = hover(
            &day,
            &Under {
                x: 500,
                near: 0,
                layout: Layout::Overlay,
                quantity: Quantity::Temp,
                selected: Some(&[Quantity::GpuTemp, Quantity::FanCpu]),
                blocks: &[],
                events: &[],
            },
        );
        assert!(tip.contains("видеокарта, °C — 65"));
        assert!(tip.contains("вентилятор процессора, об/мин — 3000"));
        assert!(!tip.contains("3200"), "подсказка следует выбору параметров");
    }

    #[test]
    fn parameter_zoom_preserves_other_metrics_and_absolute_configuration() {
        let day = metric_day();
        let selected = [Quantity::Temp, Quantity::Ratio, Quantity::Ceiling];
        let units = BTreeMap::from([(Unit::Ratio, Range { min: 20.0, max: 60.0 })]);
        let input = TimeInput {
            manual_ranges: Some(&units),
            ..metric_input(&day, &selected)
        };
        let absolute = time_panel(&input);
        let relative_input = TimeInput {
            normalized: true,
            ..input
        };
        let relative = time_panel(&relative_input);
        let ratio = relative.parameter_ranges[&Quantity::Ratio];
        let zoomed = ratio.zoom(0.5, 0.5).expect("масштаб");
        let parameters = BTreeMap::from([(Quantity::Ratio, zoomed)]);
        let after = time_panel(&TimeInput {
            parameter_ranges: Some(&parameters),
            ..relative_input
        });
        assert_eq!(after.parameter_ranges[&Quantity::Ratio], zoomed);
        assert_eq!(
            after.parameter_ranges[&Quantity::Ceiling],
            relative.parameter_ranges[&Quantity::Ceiling]
        );
        assert_eq!(
            after.parameter_ranges[&Quantity::Temp],
            relative.parameter_ranges[&Quantity::Temp]
        );
        let absolute_after = time_panel(&TimeInput {
            parameter_ranges: Some(&parameters),
            ..input
        });
        assert_eq!(
            absolute.strips, absolute_after.strips,
            "относительный масштаб не меняет абсолютные единицы"
        );
        let transposed = time_panel(&TimeInput {
            swap: true,
            ..relative_input
        });
        assert_eq!(relative.parameter_ranges, transposed.parameter_ranges);
        assert_eq!(
            relative.strips,
            time_panel(&relative_input).strips,
            "возврат осей сохраняет геометрию и настройки"
        );
    }

    #[test]
    fn relative_zoom_moves_only_its_parameter_trajectory() {
        let mut day = metric_day();
        day.blocks.truncate(1);
        let parameters = BTreeMap::from([
            (Quantity::Ratio, Range { min: 30.0, max: 50.0 }),
            (Quantity::Ceiling, Range { min: 10.0, max: 70.0 }),
        ]);
        let input = TimeInput {
            normalized: true,
            parameter_ranges: Some(&parameters),
            ..metric_input(&day, &[Quantity::Ratio, Quantity::Ceiling])
        };
        let before = time_panel(&input);
        let changed = BTreeMap::from([
            (
                Quantity::Ratio,
                parameters[&Quantity::Ratio].zoom(0.0, 0.5).expect("масштаб"),
            ),
            (Quantity::Ceiling, parameters[&Quantity::Ceiling]),
        ]);
        let after = time_panel(&TimeInput {
            parameter_ranges: Some(&changed),
            ..input
        });
        assert!(
            before.strips[0].picture.alpha(50, 50) > 0,
            "×40 посередине собственной шкалы 30–50"
        );
        assert!(
            after.strips[0].picture.alpha(50, 0) > 0,
            "×40 у верхней границы новой шкалы 30–40"
        );
        assert_eq!(after.strips[0].picture.alpha(50, 50), 0);
        assert!(
            before.strips[0].picture.alpha(50, 42) > 0 && after.strips[0].picture.alpha(50, 42) > 0,
            "потолок ×45 остается на прежней высоте своей шкалы 10–70"
        );
    }

    #[test]
    fn overlay_patterns_distinguish_metrics_in_the_same_block_colour_and_keep_gaps() {
        let mut day = metric_day();
        day.blocks.truncate(1);
        day.blocks[0].temp.fill(25.0);
        day.blocks[0].ratio.fill(25.0);
        let units = BTreeMap::from([
            (Unit::Temperature, Range { min: 0.0, max: 100.0 }),
            (Unit::Ratio, Range { min: 0.0, max: 100.0 }),
        ]);
        let temp_input = TimeInput {
            manual_ranges: Some(&units),
            ..metric_input(&day, &[Quantity::Temp])
        };
        let temperature = time_panel(&temp_input);
        let ratio = time_panel(&TimeInput {
            selected: Some(&[Quantity::Ratio]),
            ..temp_input
        });
        assert!(temperature.strips[0].picture.alpha(10, 75) > 0, "сплошная температура");
        assert_eq!(ratio.strips[0].picture.alpha(10, 75), 0, "промежуток штриха множителя");
        assert!(
            ratio.strips[0].picture.alpha(4, 75) > 0,
            "сам штрих множителя присутствует"
        );
        assert_eq!(Quantity::Temp.line_hint(), "сплошная");
        assert_eq!(Quantity::Ratio.line_hint(), "штрих");
        assert_ne!(Quantity::GpuTemp.line_hint(), Quantity::GpuPower.line_hint());
        day.blocks[0].ratio[2] = f32::NAN;
        let broken = time_panel(&TimeInput {
            manual_ranges: Some(&units),
            ..metric_input(&day, &[Quantity::Ratio])
        });
        assert_eq!(
            broken.strips[0].picture.alpha(50, 75),
            0,
            "разрыв данных не соединяется штрихом"
        );
    }

    #[test]
    fn following_appended_rows_keeps_the_window_width_at_the_new_end() {
        assert_eq!(follow((10, 30), (0, 40)), (20, 40));
        assert_eq!(follow((20, 40), (0, 55)), (35, 55));
        assert_eq!(
            follow((10, 100), (20, 40)),
            (20, 40),
            "короткая история показана целиком"
        );
        assert_eq!(follow((10, 30), (40, 40)), (40, 40));
    }

    #[test]
    fn overview_scroll_uses_the_full_history_fraction_and_clamps_without_changing_width() {
        assert_eq!(scroll((20, 40), (0, 100), 0.5), (50, 70));
        assert_eq!(scroll((20, 40), (0, 100), 1.0), (80, 100));
        assert_eq!(scroll((20, 40), (0, 100), -1.0), (0, 20));
        assert_eq!(scroll((20, 40), (0, 100), f64::NAN), (0, 20));
        assert_eq!(scroll((20, 90), (40, 60), 0.5), (40, 60));
        // Доли здесь логического времени; физическая горизонталь/вертикаль выбирается только проекцией.
        assert_eq!(scroll(follow((0, 20), (0, 100)), (0, 100), 0.25), (25, 45));
    }

    #[test]
    fn overview_helpers_do_not_overflow_at_signed_timestamp_limits() {
        let bounds = (i64::MIN, i64::MAX);
        assert_eq!(follow(bounds, bounds), bounds);
        assert_eq!(scroll(bounds, bounds, 1.0), bounds);
        assert_eq!(follow((0, 10), bounds), (i64::MAX - 10, i64::MAX));
        assert_eq!(scroll((0, 10), bounds, 0.0), (i64::MIN, i64::MIN + 10));
        assert_eq!(scroll((0, 10), bounds, 1.0), (i64::MAX - 10, i64::MAX));
    }

    #[test]
    fn zooming_historical_window_near_the_start_never_moves_its_selected_end() {
        let bounds = (0, 100_000);
        let view = (10_000, 30_000);
        assert_eq!(zoom_end(view, bounds, 2.5), (0, 30_000));
        assert_eq!(
            zoom(view, bounds, 1.0, -240.0),
            (0, 30_000),
            "колесо и кнопки используют один предел"
        );
        assert_eq!(zoom_end(view, bounds, 0.5), (20_000, 30_000));
        let latest = follow(view, (0, 120_000));
        assert_eq!(zoom_end(latest, (0, 120_000), 2.0), (80_000, 120_000));
        assert_eq!(zoom_end((9, 10), (0, 100), 0.01), (9, 10), "не меньше миллисекунды");
    }

    #[test]
    fn end_zoom_is_safe_for_nonfinite_factors_and_extreme_timestamp_bounds() {
        let bounds = (i64::MIN, i64::MAX);
        assert_eq!(zoom_end(bounds, bounds, 1.0), bounds);
        assert_eq!(zoom_end(bounds, bounds, 2.0), bounds);
        assert_eq!(
            zoom_end((i64::MAX - 20, i64::MAX), bounds, 0.5),
            (i64::MAX - 10, i64::MAX)
        );
        for factor in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(zoom_end((10, 30), (0, 100), factor), (10, 30));
        }
    }

    #[test]
    fn day_request_keeps_global_selection_order_and_reads_shared_sensors_once() {
        let directory = std::env::temp_dir().join(format!("coremap-timeline-metrics-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("каталог проверки");
        let file = directory.join("day.csv");
        std::fs::write(&file,
            "time,temp_gpu,power_gpu,busy_gpu,fan_cpu,fan_gpu,0.temp,0.ratio,0.busy,0.ceiling\n2026-10-17 10:00:00.000,65,80,0.75,3000,3200,60,40,0.5,45\n").expect("данные проверки");
        let selected = [Quantity::GpuBusy, Quantity::Temp, Quantity::FanCpu, Quantity::Ratio];
        let day = read_day(&directory, "2026-10-17", &["day.csv".into()], &["0".into()], &selected);
        std::fs::remove_file(file).expect("убрать файл проверки");
        std::fs::remove_dir(directory).expect("убрать каталог проверки");
        assert_eq!(
            day.quantities, selected,
            "ключ запроса учитывает глобальные параметры и порядок"
        );
        assert_eq!(day.blocks[0].temp, [60.0]);
        assert_eq!(day.blocks[0].ratio, [40.0]);
        assert!(day.blocks[0].busy.is_empty(), "не запрошенное не декодируется");
        assert_eq!(day.gpu_busy, [0.75]);
        assert_eq!(day.fan_cpu, [3000.0]);
    }

    #[test]
    fn overview_is_a_mean_trend_and_retains_line_breaks() {
        let times: Vec<i64> = (0..40_000).map(|k| k * 250).collect();
        let day = day_of(&times, 250);
        let mut values: Vec<f32> = (0..40_000).map(|k| if k % 2 == 0 { -10.0 } else { 10.0 }).collect();
        let mut points = Vec::new();
        trend(&day, &values, (0, 10_000_000), 400, false, &mut points);
        assert!(
            points.len() <= 101,
            "одна точка на четыре пикселя, без вертикальных min/max"
        );
        assert!(
            points.iter().all(|p| p.v.abs() < 0.01),
            "пила превращается в среднюю линию"
        );
        values[20_000] = f32::NAN;
        trend(&day, &values, (0, 10_000_000), 400, false, &mut points);
        assert_eq!(
            points.iter().filter(|p| p.pen_up).count(),
            2,
            "пустая строка разрывает линию"
        );
    }

    #[test]
    fn the_axis_keeps_logged_time_only_and_a_gap_is_more_than_two_steps() {
        // Поправка 16.10: перерыв — больше двух шагов записи (0,5 с у строк по 0,25 с): дрожь 0,375 с — не перерыв,
        // 0,75 с — перерыв; час без строк на оси не занимает места, его отметка — на границе кусков, с началом и
        // концом по часам. Ловит: перерыв по одному шагу (дрожь таймера рвала бы линию), сжатую ось с перерывами.
        let hour = 3_600_000;
        let t = [0, 250, 625, 875, 1625, 1875, hour, hour + 250];
        let day = day_of(&t, 250);
        assert_eq!(day.starts, [true, false, false, false, true, false, true, false]);
        assert_eq!(day.x, [0, 250, 625, 875, 875, 1125, 1125, 1375]);
        assert_eq!(day.gaps(), vec![(875, 875, 1625), (1125, 1875, hour)]);
        assert_eq!(day.real(1200), Some(hour + 75));
        assert_eq!(day.place(hour + 250), Some(1375));
        assert_eq!(
            day.place(30 * 60_000),
            Some(1125),
            "время в перерыве — конец куска перед ним"
        );
        // Прежние строки по 2 с: промежуток 3 с — не перерыв, 5 с — перерыв.
        let old = day_of(&[0, 2000, 5000, 10_000], 2000);
        assert_eq!(old.starts, [true, false, false, true]);
    }

    #[test]
    fn decimation_keeps_every_minimum_and_maximum_of_each_pixel_column() {
        // 100 000 строк по 0,25 с, пила с выбросами; 300 столбцов. В каждом столбце наименьшее и наибольшее значение
        // строк столбца — среди точек, точки — строки ряда по возрастанию, на столбец не больше четырёх. Ловит:
        // прореживание «каждая n-я», среднее по столбцу, потерянный выброс.
        let x: Vec<i64> = (0..100_000).map(|k| k * 250).collect();
        let starts = vec![false; x.len()];
        let v: Vec<f32> = (0..100_000u32)
            .map(|k| {
                let saw = f32::from(u16::try_from(k % 97).unwrap_or(0));
                if k % 1009 == 0 {
                    500.0
                } else if k % 1013 == 0 {
                    -500.0
                } else {
                    saw
                }
            })
            .collect();
        let view = (x[0], x[x.len() - 1]);
        let columns = 300;
        let mut out = Vec::new();
        decimate(&x, &starts, &v, view, columns, false, &mut out);
        assert!(
            out.windows(2).all(|w| w[0].at < w[1].at),
            "по возрастанию, без повторов"
        );
        assert!(
            out.iter()
                .all(|p| x.binary_search(&p.at).is_ok_and(|i| v[i].to_bits() == p.v.to_bits())),
            "только строки ряда"
        );
        assert!(out.len() <= 4 * (columns + 2), "{}", out.len());
        #[expect(clippy::cast_precision_loss, reason = "test numbers")]
        let width = (view.1 - view.0) as f64 / columns as f64;
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "test numbers"
        )]
        let column = |at: i64| ((at - view.0) as f64 / width).floor() as i64;
        let mut by_column: std::collections::BTreeMap<i64, (f32, f32)> = std::collections::BTreeMap::new();
        for (&at, &value) in x.iter().zip(&v) {
            let e = by_column.entry(column(at)).or_insert((value, value));
            e.0 = e.0.min(value);
            e.1 = e.1.max(value);
        }
        for (k, (lo, hi)) in by_column {
            let mine: Vec<f32> = out.iter().filter(|p| column(p.at) == k).map(|p| p.v).collect();
            assert!(
                mine.iter().any(|m| m.to_bits() == lo.to_bits()) && mine.iter().any(|m| m.to_bits() == hi.to_bits()),
                "столбец {k}: {lo} {hi} — {mine:?}"
            );
        }
    }

    #[test]
    fn a_gap_or_an_empty_value_lifts_the_pen_but_sparse_rows_skip_empty_values() {
        // Перерыв записи после 40-й строки; у плотного ряда пустое значение в строке 20 — рвёт; у редкого (чтение раз в
        // 8 строк) пустые между чтениями — не рвут.
        let mut t: Vec<i64> = (0..80).map(|k| k * 250).collect();
        for at in &mut t[40..] {
            *at += 10_000;
        }
        let day = day_of(&t, 250);
        let mut dense: Vec<f32> = vec![1.0; 80];
        dense[20] = f32::NAN;
        let sparse: Vec<f32> = (0..80).map(|k| if k % 8 == 0 { 2.0 } else { f32::NAN }).collect();
        let view = day.bounds().expect("строки");
        let mut out = Vec::new();
        decimate(&day.x, &day.starts, &dense, view, 1000, false, &mut out);
        assert_eq!(out.iter().filter(|p| p.pen_up).count(), 3, "начало, пустое, перерыв");
        decimate(&day.x, &day.starts, &sparse, view, 1000, true, &mut out);
        assert_eq!(out.iter().filter(|p| p.pen_up).count(), 2, "начало и перерыв");
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn a_line_paints_every_row_of_its_vertical_span() {
        // Картинка полосы: вертикальный отрезок от строки 2 до 17 закрашивает каждую строку между ними (размах столбца
        // прореживания виден целиком); за краями — ничего; густота ряда — одна на пиксель, как бы часто его ни задели.
        let mut p = Picture::new(10, 20);
        let mut mask = Mask::new(10, 20);
        mask.line((4.0, 2.0), (4.0, 17.0), 1);
        p.blend(&mask, 0x00ff_0000, 1.0);
        assert!((2..=17).all(|y| p.alpha(4, y) == 255));
        assert_eq!((p.alpha(4, 1), p.alpha(4, 18), p.alpha(3, 10)), (0, 0, 0));
        assert_eq!(
            &p.rgba[(10 * 4 + 4) * 4..(10 * 4 + 4) * 4 + 4],
            &[0xff, 0, 0, 255],
            "цвет линии как есть"
        );
        // Толстая диагональ за краями, полупрозрачная: шаги ложатся друг на друга, а пиксель — один раз (0,5 → 128),
        // поверх красной — смесь, красная видна сквозь неё.
        mask.clear();
        mask.line((-5.0, -5.0), (30.0, 30.0), 2);
        let mut q = Picture::new(10, 20);
        q.blend(&mask, 0x0000_ff00, 0.5);
        assert!(
            (0..10).all(|k| q.alpha(k, k) == 128),
            "{:?}",
            (0..10).map(|k| q.alpha(k, k)).collect::<Vec<_>>()
        );
        assert_eq!(q.alpha(9, 0), 0);
        p.blend(&mask, 0x0000_ff00, 0.5);
        let at = (4 * 10 + 4) * 4;
        assert_eq!(
            &p.rgba[at..at + 4],
            &[128, 128, 0, 255],
            "на (4, 4): красная под зелёной густотой 0,5"
        );
    }

    #[test]
    fn the_wheel_keeps_the_place_under_the_cursor() {
        // Показ 10:00–12:00 дня 08:00–14:00, курсор на 30 % ширины: после ступени колеса (и обратно) место под курсором
        // то же, показ уже в 0,8 раза. Ловит: масштаб вокруг середины или левого края.
        let hour = 3_600_000;
        let bounds = (8 * hour, 14 * hour);
        let view = (10 * hour, 12 * hour);
        #[expect(clippy::cast_precision_loss, reason = "test numbers")]
        let under = |v: (i64, i64), at: f64| v.0 as f64 + at * (v.1 - v.0) as f64;
        for at in [0.0, 0.3, 0.5, 0.97] {
            let zoomed = zoom(view, bounds, at, WHEEL_STEP);
            assert!((under(zoomed, at) - under(view, at)).abs() <= 1.0, "{at}: {zoomed:?}");
            #[expect(clippy::cast_precision_loss, reason = "test numbers")]
            let ratio = (zoomed.1 - zoomed.0) as f64 / (view.1 - view.0) as f64;
            assert!((ratio - WHEEL_ZOOM).abs() < 1e-6, "{ratio}");
            let back = zoom(zoomed, bounds, at, -WHEEL_STEP);
            assert!((under(back, at) - under(view, at)).abs() <= 2.0);
        }
        assert_eq!(zoom(view, bounds, 0.5, -100.0 * WHEEL_STEP), bounds);
        let narrow = zoom(view, bounds, 0.5, 100.0 * WHEEL_STEP);
        assert_eq!(narrow.1 - narrow.0, MIN_SPAN_MS);
        assert_eq!(zoom((8 * hour, 9 * hour), bounds, 0.0, -2.0 * WHEEL_STEP).0, 8 * hour);
        assert_eq!(
            centred(9 * hour, hour, bounds),
            (8 * hour + hour / 2, 9 * hour + hour / 2)
        );
        assert_eq!(centred(8 * hour, hour, bounds), (8 * hour, 9 * hour), "у края — внутрь");
    }

    #[test]
    fn dragging_moves_the_view_by_the_dragged_share_and_stops_at_the_day() {
        let hour = 3_600_000;
        let bounds = (8 * hour, 14 * hour);
        assert_eq!(
            pan((10 * hour, 12 * hour), bounds, 0.25),
            (10 * hour - hour / 2, 12 * hour - hour / 2)
        );
        assert_eq!(pan((10 * hour, 12 * hour), bounds, -2.0), (12 * hour, 14 * hour));
        assert_eq!(pan((10 * hour, 12 * hour), bounds, 5.0), (8 * hour, 10 * hour));
    }

    #[test]
    fn the_nearest_reading_within_two_seconds_is_taken() {
        let t: Vec<i64> = (0..20).map(|k| k * 250).collect();
        let mut v = vec![f32::NAN; 20];
        v[2] = 1.0; // 500 мс
        v[12] = 2.0; // 3000 мс
        assert_eq!(
            nearest_reading(&t, &v, 4),
            Some(1.0),
            "1000 мс: до 500 — 500 мс, до 3000 — 2000 мс"
        );
        assert_eq!(nearest_reading(&t, &v, 9), Some(2.0), "2250 мс: ближе 3000");
        v[2] = f32::NAN;
        assert_eq!(nearest_reading(&t, &v, 3), None, "750 мс: до 3000 — больше 2 с");
    }

    #[test]
    fn blocks_layout_draws_one_strip_and_puts_events_on_the_block_line() {
        // Раскладка «блоки»: одна полоса, у каждого блока своя линия; событие блока — отметкой на его линии на высоте
        // его значения; событие без блока — под полосами; перерыв — отметкой и в подсказке.
        let mut t: Vec<i64> = (0..400).map(|k| k * 250).collect();
        for at in &mut t[200..] {
            *at += 60_000;
        }
        let mut day = day_of(&t, 250);
        day.blocks = vec![
            BlockDay {
                name: "0".into(),
                ceiling: (0..400).map(|k| if k < 100 { 46.0 } else { 45.0 }).collect(),
                ..BlockDay::default()
            },
            BlockDay {
                name: "1".into(),
                ceiling: vec![40.0; 400],
                ..BlockDay::default()
            },
        ];
        let shown = [
            Shown {
                label: "Ядро 0",
                color: 0x00ff_0000,
            },
            Shown {
                label: "Ядро 1",
                color: 0x0000_ff00,
            },
        ];
        let events = [
            Act {
                at: 25_000,
                color: 0x00ff_5d5d,
                cut: true,
                block: Some(0),
                tip: "отсечка".into(),
            },
            Act {
                at: 30_000,
                color: 0x00c8_ccd4,
                cut: false,
                block: None,
                tip: "сброс".into(),
            },
        ];
        let view = day.bounds().expect("строки");
        let input = TimeInput {
            day: &day,
            view,
            layout: Layout::Blocks,
            quantity: Quantity::Ceiling,
            selected: None,
            manual_ranges: None,
            swap: false,
            normalized: false,
            parameter_ranges: None,
            columns: 400,
            height: 200,
            scale: 1.0,
            blocks: &shown,
            limit: Some(85.0),
            emergency: None,
            events: &events,
        };
        let s = time_panel(&input);
        assert_eq!(s.strips.len(), 1);
        assert!(s.strips[0].refs.is_empty(), "линии отсечки — только у температуры");
        assert_eq!(s.gaps.len(), 1, "перерыв в 60 с — одной отметкой");
        assert_eq!((s.dots.len(), s.marks.len()), (1, 1));
        let (lo, hi) = s.strips[0].range.expect("ось");
        let y = f64::from(s.dots[0].y);
        assert!(
            (y - (45.0 - lo) / (hi - lo)).abs() < 1e-3,
            "отметка — на линии блока 0 (×45): {y}"
        );
        let tip = hover(
            &day,
            &Under {
                x: day.gaps()[0].0,
                near: 500,
                layout: Layout::Blocks,
                quantity: Quantity::Ceiling,
                selected: None,
                blocks: &shown,
                events: &events,
            },
        );
        assert!(tip.contains("Перерыв записи") && tip.contains("1 мин 0 с"), "{tip}");
        let params = time_panel(&TimeInput {
            layout: Layout::Params,
            ..input
        });
        assert_eq!(params.strips.len(), 7);
        assert_eq!(params.marks.len(), 2, "в «параметрах» события — под полосами");
    }
}
