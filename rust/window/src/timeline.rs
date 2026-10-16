// График статистики по времени (бриф W, часть 3, с поправкой 16.10): ось x — время одного дня журнала, две раскладки:
// • «блоки» — выбранные блоки (хоть все двенадцать) своими цветами × одна величина: температура, фактический
//   множитель, потолок (ступенями — журнал смены потолков), нагрузка или кристалл; события — отметками на линии блока;
// • «параметры» — один-два блока (первые в сравнении) × все величины полосами одна под другой: температура с целью и
//   аварийной отсечкой, множитель с потолком, нагрузка, кристалл, видеокарта (°C и Вт), вентиляторы; события — под
//   полосами.
// Владелец 16.10: «графики… множителей для каждого ядра… параллельно… чтобы видно было… какое ядро поменялось… на
// какой момент времени… в каком порядке… возможность отображать сразу все… учитывать только время когда… активно
// приложение». Отсюда:
// • ось — только записанное время: промежуток без строк (CoreMapTray закрыт, компьютер выключен, журнал не писался)
//   сжимается в тонкую отметку с его длительностью в подсказке, как торговые графики пропускают закрытые часы.
//   Перерыв — промежуток больше двух шагов записи строки (0,5 с у строк по 0,25 с, 4 с у прежних по 2 с): меньший —
//   обычный шаг журнала с дрожью таймера, а не пауза;
// • колесо мыши — масштаб вокруг курсора, перетаскивание — сдвиг, без заданных заранее отрезков (как у просмотрщиков
//   рядов по времени);
// • прореживание — первая, наименьшая, наибольшая и последняя строка на столбец пикселей (M4): день строк по 0,25 с —
//   около 345 тыс. точек на ряд, ни один всплеск не теряется;
// • полосы рисуются здесь в картинки (Slint только выводит их): путь в 64 тыс. точек программный отрисовщик Slint
//   рисовал 4,4 с на кадр (замер скрытой проверки 16.10), картинку — копирует.
// Одна раскладка от другой отличается только набором рядов и полос: шкалы, деления, подсказки, наведение и
// прореживание — общие. День читает поток статистики (store.rs) по просьбе окна, когда — решает view.rs.
use std::fmt::Write as _;
use std::path::Path;

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quantity {
    #[default]
    Temp,
    Ratio,
    Ceiling,
    Busy,
    Level,
}

impl Quantity {
    pub const ALL: [Quantity; 5] = [
        Quantity::Temp,
        Quantity::Ratio,
        Quantity::Ceiling,
        Quantity::Busy,
        Quantity::Level,
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
        }
    }

    #[must_use]
    pub fn of(index: i32) -> Quantity {
        match index {
            1 => Quantity::Ratio,
            2 => Quantity::Ceiling,
            3 => Quantity::Busy,
            4 => Quantity::Level,
            _ => Quantity::Temp,
        }
    }

    /// Подпись полосы.
    fn title(self) -> &'static str {
        match self {
            Quantity::Temp => "°C",
            Quantity::Ratio => "фактический множитель",
            Quantity::Ceiling => "потолок (ступени — его смены)",
            Quantity::Busy => "нагрузка, %",
            Quantity::Level => "кристалл (средняя остальных блоков), °C",
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
        }
    }

    fn series_mut(&mut self, q: Quantity) -> &mut Vec<f32> {
        match q {
            Quantity::Temp => &mut self.temp,
            Quantity::Ratio => &mut self.ratio,
            Quantity::Ceiling => &mut self.ceiling,
            Quantity::Busy => &mut self.busy,
            Quantity::Level => &mut self.level,
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
                for q in Quantity::ALL {
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
        for &q in wanted {
            let v = match q {
                Quantity::Temp => value(What::Temp),
                Quantity::Ratio => value(What::Ratio),
                Quantity::Ceiling => value(What::Ceiling),
                Quantity::Busy => value(What::Busy),
                Quantity::Level => value(What::Temp).and_then(|t| samples::die_level(sum - t, known - 1, others)),
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
    /// Деления: доля высоты снизу и подпись.
    pub ticks: Vec<(f32, String)>,
    pub picture: Picture,
    pub refs: Vec<RefShown>,
    /// Ось значений: (низ, верх); None — данных в показе нет.
    pub range: Option<(f64, f64)>,
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
}

/// Полоса до рисования: подпись, ряды, горизонтальные линии.
struct Plan<'a> {
    title: String,
    rows: Vec<Row<'a>>,
    refs: Vec<Level>,
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
    let mut shown = TimeShown::default();
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
    let plans = plans(input);
    let height = strip_height(input.height, plans.len(), input.scale);
    let mut points = Vec::new();
    for plan in &plans {
        shown.strips.push(strip(plan, input, height, &mut points));
    }
    shown.ticks = time_ticks(day, view, input.columns, input.scale);
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
    let share = |x: i64| (x - view.0) as f64 / (view.1 - view.0).max(1) as f64;
    shown.gaps = gaps.iter().map(|&x| crate::panels::f(share(x))).collect();
    let range = shown.strips.first().and_then(|s| s.range);
    for e in input.events {
        let Some(x) = day.place(e.at).filter(|&x| inside(x)) else {
            continue;
        };
        let on_line = (input.layout == Layout::Blocks)
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
                x: crate::panels::f(share(x)),
                y: crate::panels::f(y),
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
    if q == Quantity::Busy { 100.0 } else { 1.0 }
}

/// Толщина линии в пикселях картинки.
fn thickness(scale: f64, points: f64) -> i64 {
    #[expect(clippy::cast_possible_truncation, reason = "one to four pixels")]
    let px = (scale * points).round().max(1.0) as i64;
    px
}

/// Полосы раскладки.
fn plans<'a>(input: &TimeInput<'a>) -> Vec<Plan<'a>> {
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
                ..row(b.series(q), colour(k))
            })
            .collect()
    };
    let plan = |title: &str, rows: Vec<Row<'a>>, refs: Vec<Level>| Plan {
        title: title.to_string(),
        rows,
        refs,
    };
    match input.layout {
        Layout::Blocks => vec![plan(
            input.quantity.title(),
            of(input.quantity),
            if input.quantity == Quantity::Temp {
                refs
            } else {
                Vec::new()
            },
        )],
        Layout::Params => {
            let mut ratio: Vec<Row> = Vec::new();
            for (k, b) in day.blocks.iter().enumerate() {
                ratio.push(Row {
                    steps: true,
                    thick: thickness(input.scale, 1.0),
                    alpha: CEILING_ALPHA,
                    ..row(&b.ceiling, colour(k))
                });
                ratio.push(Row {
                    alpha: shared,
                    ..row(&b.ratio, colour(k))
                });
            }
            let sparse = |v: &'a [f32], color: u32| Row {
                sparse: true,
                ..row(v, color)
            };
            vec![
                plan(Quantity::Temp.title(), of(Quantity::Temp), refs),
                plan("множитель · потолок ступенями", ratio, Vec::new()),
                plan(Quantity::Busy.title(), of(Quantity::Busy), Vec::new()),
                plan(Quantity::Level.title(), of(Quantity::Level), Vec::new()),
                plan("видеокарта, °C", vec![sparse(&day.gpu_temp, GPU)], Vec::new()),
                plan("видеокарта, Вт", vec![sparse(&day.gpu_power, GPU)], Vec::new()),
                plan(
                    "вентиляторы, об/мин: процессора · видеокарты",
                    vec![sparse(&day.fan_cpu, FAN_CPU), sparse(&day.fan_gpu, FAN_GPU)],
                    Vec::new(),
                ),
            ]
        }
    }
}

/// Одна полоса: ось значений — по видимым точкам её рядов (и линиям `refs`), ряды — в картинку.
fn strip(plan: &Plan, input: &TimeInput, height: usize, points: &mut Vec<Point>) -> StripShown {
    let day = input.day;
    let (from, to) = input.view;
    let mut decimated: Vec<Vec<Point>> = Vec::with_capacity(plan.rows.len());
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for r in &plan.rows {
        decimate(&day.x, &day.starts, r.v, input.view, input.columns, r.sparse, points);
        for p in points.iter_mut() {
            p.v *= r.times;
            if (from..=to).contains(&p.at) {
                lo = lo.min(f64::from(p.v));
                hi = hi.max(f64::from(p.v));
            }
        }
        decimated.push(points.clone()); // a few thousand points per row, per redraw
    }
    for (v, _, _) in &plan.refs {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    let width = u32::try_from(input.columns).unwrap_or(1).max(2);
    let tall = u32::try_from(height).unwrap_or(8).max(8);
    let mut shown = StripShown {
        title: plan.title.clone(),
        picture: Picture::new(width, tall),
        ..StripShown::default()
    };
    if !lo.is_finite() {
        shown.title = format!("{} — нет данных", plan.title);
        return shown;
    }
    if hi - lo < 1.0 {
        (lo, hi) = (lo - 0.5, hi + 0.5);
    }
    let pad = (hi - lo) * 0.08;
    let (lo, hi) = (lo - pad, hi + pad);
    shown.range = Some((lo, hi));
    let value = |v: f64| (v - lo) / (hi - lo);
    #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day, pixels")]
    let px = |at: i64| (at - from) as f64 / (to - from).max(1) as f64 * f64::from(width - 1);
    let py = |v: f32| (1.0 - value(f64::from(v))) * f64::from(tall - 1);
    let mut mask = Mask::new(width, tall);
    for (r, pts) in plan.rows.iter().zip(&decimated) {
        mask.clear();
        let mut last: Option<(f64, f64)> = None;
        for p in pts {
            let here = (px(p.at), py(p.v));
            match last {
                Some(prev) if !p.pen_up => {
                    if r.steps {
                        mask.line(prev, (here.0, prev.1), r.thick);
                        mask.line((here.0, prev.1), here, r.thick);
                    } else {
                        mask.line(prev, here, r.thick);
                    }
                }
                _ => mask.dot(here, r.thick),
            }
            last = Some(here);
        }
        shown.picture.blend(&mask, r.color, r.alpha);
    }
    shown.refs = plan
        .refs
        .iter()
        .map(|(v, color, text)| RefShown {
            f: crate::panels::f(value(*v)),
            color: *color,
            text: text.clone(), // two lines per strip at most
        })
        .collect();
    let wanted = (f64::from(tall) / input.scale.max(0.1) / TICK_POINTS).clamp(2.0, 8.0);
    let step = nice_step((hi - lo) / wanted);
    let mut tick = (lo / step).ceil() * step;
    while tick <= hi {
        let text = if step >= 1.0 {
            format!("{tick:.0}")
        } else {
            format!("{tick:.1}")
        };
        shown.ticks.push((crate::panels::f(value(tick)), text));
        tick += step;
    }
    shown
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
    let wanted: &[Quantity] = match under.layout {
        Layout::Blocks => std::slice::from_ref(&under.quantity),
        Layout::Params => &Quantity::ALL,
    };
    for (b, shown) in day.blocks.iter().zip(under.blocks) {
        let parts: Vec<String> = wanted
            .iter()
            .filter_map(|&q| {
                let x = v(b.series(q))?;
                Some(match q {
                    Quantity::Temp => format!("{x:.0} °C"),
                    Quantity::Ratio => format!("×{x:.1}"),
                    Quantity::Ceiling => format!("потолок ×{x:.0}"),
                    Quantity::Busy => format!("нагрузка {:.0} %", x * 100.0),
                    Quantity::Level => format!("кристалл {x:.1} °C"),
                })
            })
            .collect();
        if !parts.is_empty() {
            let _ = write!(out, "\n{} — {}", shown.label, parts.join(" · ")); // into a String
        }
    }
    if under.layout == Layout::Params {
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
