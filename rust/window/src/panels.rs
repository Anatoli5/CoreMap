// Что показывают панели «Профили» и «Статистика» и перемычки соседей на схеме: из сообщений движка и сводки
// статистики (store.rs) — строки, цвета и точки для ui/app.slint. Здесь нет ни Slint-окна, ни потоков: функции
// получают данные и возвращают, что показать, — их проверяют модульные тесты.
use std::fmt::Write as _;

use serde_json::{Map, Value};
use slint::{Color, ModelRc, SharedString, VecModel};

use crate::die::Geometry;
use crate::samples::{self, Bin};
use crate::store::Digest;
use crate::wire::{AutoMark, BlockInfo, Hello, Limits, Profiles, State};
use crate::{ChartPoint, LineRow, LinkData, SetRow, Tick};

pub fn rgb(hex: u32) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    Color::from_rgb_u8(r, g, b)
}

const TEXT: u32 = 0x00e6_e8ec;
const DIM: u32 = 0x008b_93a1;
const SAME: u32 = 0x003a_404b;
const LOWER: u32 = 0x006f_9be0;
const HIGHER: u32 = 0x00f0_b860;
const PEAK: u32 = 0x00ff_8a7a;
const E_TEXT: u32 = 0x006f_d0c8;

/// Название набора для надписей: профиль в кавычках, заводские или несохранённый (как Titled у прежнего окна).
pub fn titled(name: Option<&str>) -> String {
    match name {
        None => "не сохранено".into(),
        Some("") => "заводские".into(),
        Some(n) => format!("«{n}»"),
    }
}

/// «−2», «+1»; 0 — пусто.
pub fn signed(d: i64) -> String {
    match d {
        0 => String::new(),
        d if d > 0 => format!("+{d}"),
        d => format!("−{}", d.abs()),
    }
}

/// Блок: номер для надписей — «ядро 12» или «кластер 2–5».
pub fn whom(b: &BlockInfo) -> String {
    if b.members.len() > 1 {
        format!("кластер {}", b.name)
    } else {
        format!("ядро {}", b.name)
    }
}

// ----- потолки блоков: сейчас, было, поправка автомодуля -----

/// Потолки блока: сейчас, «было», поправка автомодуля (на сколько ступеней он сдвинул блок от принятого).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ceiling {
    pub now: Option<i64>,
    pub was: Option<i64>,
    pub auto: i64,
    pub ban: Option<i64>,
}

impl Ceiling {
    /// Разница с «было» без поправки автомодуля — то, что изменил владелец (или профиль); 0 — нет.
    pub fn owner_delta(&self) -> i64 {
        match (self.now, self.was) {
            (Some(now), Some(was)) => now - self.auto - was,
            _ => 0,
        }
    }
}

/// Надстройка автомодуля из состояния (программа входа): что он держит по блокам и принятые потолки по ядрам —
/// от них считается его поправка. Нет принятых — от «было».
#[derive(Clone, Copy, Debug, Default)]
pub struct Overlay<'a> {
    pub marks: Option<&'a [Option<AutoMark>]>,
    pub intended: Option<&'a [Option<i64>]>,
}

impl<'a> Overlay<'a> {
    pub fn of(state: Option<&'a State>) -> Self {
        Overlay {
            marks: state.and_then(|s| s.auto.as_deref()),
            intended: state.and_then(|s| s.intended.as_deref()),
        }
    }
}

pub fn ceiling(b: &BlockInfo, limit: Option<&[i64]>, limits: Option<&Limits>, overlay: Overlay) -> Ceiling {
    let known = |v: i64| (v > 0).then_some(v);
    let now = limit.and_then(|l| l.get(b.first).copied()).and_then(known);
    let was = limits.and_then(|l| l.was.get(b.first).copied()).and_then(known);
    let mark = overlay.marks.and_then(|a| a.get(b.index)).and_then(Option::as_ref);
    let intended = overlay
        .intended
        .and_then(|i| i.get(b.first).copied().flatten())
        .and_then(known);
    let auto = mark
        .and_then(|m| Some(m.ceiling()? - m.base().or(intended).or(was)?))
        .unwrap_or(0);
    Ceiling {
        now,
        was,
        auto,
        ban: mark.and_then(|m| m.ban().map(|(at, _)| at)),
    }
}

// ----- профили -----

/// Что окно знает для панели профилей.
#[derive(Debug)]
pub struct ProfileInput<'a> {
    pub hello: &'a Hello,
    pub profiles: &'a Profiles,
    pub limits: Option<&'a Limits>,
    pub limit: Option<&'a [i64]>,
    pub auto: Overlay<'a>,
    pub chosen: Option<&'a str>,
    pub name: &'a str,
    pub busy: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per button of the panel, read by name"
)]
pub struct ProfileShown {
    pub was: String,
    pub now: String,
    pub attention: bool,
    pub rows: Vec<SetShown>,
    pub can_open: bool,
    pub can_save: bool,
    pub can_save_as: bool,
    pub can_accept: bool,
    pub can_revert: bool,
    pub can_delete: bool,
    pub save_tip: String,
    pub save_as_text: String,
    pub save_note: String,
}

/// Строка списка наборов (то же, что `SetRow`, но сравнимое и без Slint-моделей — для тестов и сравнения).
#[derive(Debug, Default, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per mark of the list row (SetRow in app.slint)"
)]
pub struct SetShown {
    pub key: String,
    pub name: String,
    pub standing: bool,
    pub was: bool,
    pub startup: bool,
    pub disabled: bool,
    pub selected: bool,
    pub cells: Vec<Color>,
    pub tip: String,
}

impl SetShown {
    pub fn row(&self) -> SetRow {
        SetRow {
            name: self.name.as_str().into(),
            standing: self.standing,
            was: self.was,
            startup: self.startup,
            disabled: self.disabled,
            selected: self.selected,
            cells: ModelRc::new(VecModel::from(self.cells.clone())), // a dozen colours per changed row
            tip: self.tip.as_str().into(),
        }
    }
}

/// Название длиннее не принимает движок (Commands.NameLength).
pub const NAME_LENGTH: usize = 64;

#[expect(
    clippy::too_many_lines,
    reason = "the panel's rules read top to bottom: rows, status, buttons"
)]
pub fn profile_panel(input: &ProfileInput) -> ProfileShown {
    let p = input.profiles;
    let blocks = &input.hello.blocks;
    let ceilings: Vec<Ceiling> = blocks
        .iter()
        .map(|b| ceiling(b, input.limit, input.limits, input.auto))
        .collect();
    let startup_name = p
        .startup
        .exists
        .then(|| p.startup.name.as_deref().unwrap_or(""))
        .filter(|n| !n.is_empty());
    let same_name = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();

    // Строки: заводские, затем сохранённые наборы в порядке движка.
    let fused = input.limits.map(|l| l.fused.as_slice());
    let mut rows = Vec::with_capacity(p.sets.len() + 1);
    let entries =
        std::iter::once(("", fused)).chain(p.sets.iter().map(|s| (s.name.as_str(), Some(s.ratios.as_slice()))));
    for (key, ratios) in entries {
        let mut cells = Vec::with_capacity(blocks.len());
        let mut differ = Vec::new();
        for (b, c) in blocks.iter().zip(&ceilings) {
            let set = ratios.and_then(|r| r.get(b.first).copied()).filter(|&v| v > 0);
            cells.push(match (set, c.now) {
                (Some(s), Some(n)) if s == n => rgb(SAME),
                (Some(s), Some(n)) => {
                    differ.push(format!("{} ×{s} (сейчас ×{n})", b.name));
                    let steps = (s - n).unsigned_abs().min(4);
                    let k = 0.45 + 0.55 * f32::from(u8::try_from(steps).unwrap_or(4)) / 4.0;
                    let base = rgb(if s < n { LOWER } else { HIGHER });
                    rgb(SAME).mix(&base, 1.0 - k)
                }
                _ => Color::from_argb_u8(0, 0, 0, 0),
            });
        }
        let title = titled(Some(key));
        let tip = if differ.is_empty() {
            format!("{title}: то же, что стоит сейчас")
        } else {
            format!(
                "{title}: отличается от того, что стоит сейчас, у {} из {} блоков\n• {}\nКлетки справа — блоки по порядку: синяя — в наборе ниже, жёлтая — выше; ярче — больше ступеней.",
                differ.len(),
                blocks.len(),
                differ.join("\n• ")
            )
        };
        rows.push(SetShown {
            key: key.to_string(),
            name: if key.is_empty() {
                "заводские".into()
            } else {
                key.to_string()
            },
            standing: p.standing.as_deref() == Some(key),
            was: p.was_named.as_deref() == Some(key),
            startup: startup_name.is_some_and(|s| same_name(s, key)),
            disabled: p.startup.disabled,
            selected: input.chosen == Some(key),
            cells,
            tip,
        });
    }

    let was = format!(
        "Было — {} · {}",
        titled(p.was_named.as_deref()),
        p.startup.describe.as_deref().unwrap_or("профиль запуска неизвестен")
    );
    let diffs: Vec<String> = blocks
        .iter()
        .zip(&ceilings)
        .filter_map(|(b, c)| {
            let d = match (c.now, c.was) {
                (Some(n), Some(w)) => n - w,
                _ => 0,
            };
            (d != 0).then(|| format!("{} {}", b.name, signed(d)))
        })
        .collect();
    let (now, attention) = if input.busy {
        ("Сейчас — измерение: потолки временные".to_string(), false)
    } else if diffs.is_empty() {
        ("Сейчас — то же".to_string(), false)
    } else {
        (
            format!("Сейчас — {}: {}", titled(p.standing.as_deref()), diffs.join(", ")),
            true,
        )
    };
    let autos: Vec<String> = blocks
        .iter()
        .zip(&ceilings)
        .filter(|(_, c)| c.auto != 0)
        .map(|(b, c)| format!("{} {}", b.name, signed(c.auto)))
        .collect();
    let save_note = if autos.is_empty() {
        "Сохраняется то, что стоит сейчас".to_string()
    } else {
        format!(
            "Сохраняется то, что стоит сейчас, с поправками автомодуля: {}",
            autos.join(", ")
        )
    };

    let chosen_saved = input
        .chosen
        .filter(|c| !c.is_empty() && p.sets.iter().any(|s| s.name == *c));
    let name = input.name.trim();
    let exists = p.sets.iter().any(|s| same_name(&s.name, name));
    ProfileShown {
        was,
        now,
        attention,
        rows,
        can_open: !input.busy && input.chosen.is_some_and(|c| p.standing.as_deref() != Some(c)),
        can_save: !input.busy && chosen_saved.is_some(),
        can_save_as: !input.busy && !name.is_empty() && name.chars().count() <= NAME_LENGTH,
        can_accept: p.can_accept && !input.busy,
        can_revert: !input.busy && !diffs.is_empty(),
        can_delete: chosen_saved.is_some(),
        save_tip: chosen_saved.map_or_else(
            || "Сохранить в выбранный профиль\nВыбери сохранённый профиль в списке (не «заводские»).".to_string(),
            |c| format!("Сохранить в «{c}»\nЗаписывает в него то, что стоит сейчас, и принимает: он становится «было» и ставится при входе в Windows."),
        ),
        save_as_text: if exists { "перезаписать".into() } else { "сохранить как".into() },
        save_note,
    }
}

/// История смен профиля: новые первыми.
pub fn history_rows(entries: &[Map<String, Value>], hello: Option<&Hello>) -> Vec<LineRow> {
    let blocks: Vec<samples::Named> = hello
        .map(|h| {
            h.blocks
                .iter()
                .map(|b| samples::Named {
                    name: &b.name,
                    first: b.first,
                })
                .collect()
        })
        .unwrap_or_default();
    entries
        .iter()
        .rev()
        .map(|e| {
            let (time, text) = samples::history_line(e, &blocks);
            LineRow {
                time: time.into(),
                head: SharedString::new(),
                text: text.into(),
                color: rgb(TEXT),
            }
        })
        .collect()
}

// ----- статистика блока -----

#[derive(Debug, Default, Clone, PartialEq)]
pub struct StatsShown {
    pub title: String,
    pub note: String,
    pub points: Vec<PointShown>,
    pub yticks: Vec<(f32, String)>,
    pub mean_path: String,
    pub peak_path: String,
    pub empty: String,
    pub fan_top: String,
    pub columns: f32,
    pub actions: Vec<(String, String, String, bool)>,
    pub actions_empty: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct PointShown {
    pub ratio: i32,
    pub x: f32,
    pub mean: f32,
    pub peak: f32,
    pub fan: f32,
    pub label: String,
    pub count: String,
    pub tip: String,
    pub samples: u32,
}

impl PointShown {
    pub fn point(&self) -> ChartPoint {
        ChartPoint {
            x: self.x,
            mean: self.mean,
            peak: self.peak,
            fan: self.fan,
            label: self.label.as_str().into(),
            count: self.count.as_str().into(),
            tip: self.tip.as_str().into(),
        }
    }
}

impl StatsShown {
    pub fn ticks(&self) -> Vec<Tick> {
        self.yticks
            .iter()
            .map(|(f, t)| Tick {
                f: *f,
                text: t.as_str().into(),
            })
            .collect()
    }

    pub fn action_rows(&self) -> Vec<LineRow> {
        self.actions
            .iter()
            .map(|(time, change, why, lower)| LineRow {
                time: time.as_str().into(),
                head: change.as_str().into(),
                text: why.as_str().into(),
                color: rgb(if *lower { PEAK } else { E_TEXT }),
            })
            .collect()
    }
}

/// Сколько корзин помещается подписанными; больше — подписывается каждая вторая (числа — в подсказке столбца).
const LABELED: usize = 18;

#[expect(
    clippy::cast_possible_truncation,
    reason = "chart fractions and degrees, well inside f32"
)]
fn f(v: f64) -> f32 {
    v as f32
}

/// «2 мин 10 с» — сколько времени набралось срезов по 2 с.
fn span_of(samples: u32) -> String {
    let s = u64::from(samples) * 2;
    match s {
        s if s < 60 => format!("{s} с"),
        s if s < 3600 => format!("{} мин {} с", s / 60, s % 60),
        s => format!("{} ч {} мин", s / 3600, s % 3600 / 60),
    }
}

/// Число с пробелами между тысячами: «12 400».
pub fn grouped(v: u64) -> String {
    let digits = v.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[expect(clippy::too_many_lines, reason = "the chart's scales and points read top to bottom")]
pub fn stats_panel(block: &BlockInfo, digest: Option<&Digest>) -> StatsShown {
    let mut shown = StatsShown {
        title: format!("Статистика · {}", whom(block)),
        columns: 1.0,
        ..StatsShown::default()
    };
    let Some(digest) = digest else {
        shown.empty = "Статистика читается…".into();
        return shown;
    };
    let actions = samples::actions_for(&digest.auto, &block.name);
    shown.actions = actions
        .into_iter()
        .map(|a| (a.time, a.change, a.why, a.lower))
        .collect();
    shown.actions_empty = if digest.auto.is_empty() {
        "журнала автомодуля нет — он ещё ничего не делал".into()
    } else {
        "этот блок автомодуль не трогал".into()
    };
    let bins: Vec<(i32, Bin)> = digest
        .tally
        .blocks
        .get(&block.name)
        .map(|t| t.bins.iter().filter(|(_, b)| b.n > 0).map(|(r, b)| (*r, *b)).collect())
        .unwrap_or_default();
    let busy: u64 = bins.iter().map(|(_, b)| u64::from(b.n)).sum();
    // Срок срезов: «15.10 01:25–23:10» за один день, «14.10 – 15.10» за несколько.
    let period = match (&digest.tally.first, &digest.tally.last) {
        (Some(a), Some(b)) => {
            let (a, b) = (samples::short_time(a), samples::short_time(b));
            match (a.split_once(' '), b.split_once(' ')) {
                (Some((da, ta)), Some((db, tb))) if da == db => format!(" · {da} {ta}–{tb}"),
                (Some((da, _)), Some((db, _))) => format!(" · {da} – {db}"),
                _ => format!(" · {a} – {b}"),
            }
        }
        _ => String::new(),
    };
    shown.note = format!(
        "{} из {} срезов под нагрузкой{period}",
        grouped(busy),
        grouped(digest.tally.rows)
    );
    if let Some(trouble) = &digest.trouble {
        shown.note.clone_from(trouble);
    }
    let (Some(&(lo, _)), Some(&(hi, _))) = (bins.first(), bins.last()) else {
        shown.empty = if digest.tally.rows == 0 {
            "Срезов пока нет. Их пишет программа входа каждые 2 с, пока включён автомодуль и процессор нагружен.".into()
        } else {
            format!(
                "{} ни разу не был занят (доля работы ≥ 50 %) в {} срезах.",
                whom(block),
                grouped(digest.tally.rows)
            )
        };
        return shown;
    };
    // По оси множителя — все корзины от наименьшей до наибольшей, не меньше трёх (одна точка — посередине).
    let width = (hi - lo + 1).max(3);
    let first = lo - (width - (hi - lo + 1)) / 2;
    let columns = f64::from(width);
    shown.columns = f(columns);
    let x_of = |ratio: i32| (f64::from(ratio - first) + 0.5) / columns;

    let low = bins.iter().filter_map(|(_, b)| b.mean()).fold(f64::INFINITY, f64::min);
    let high = bins.iter().map(|(_, b)| b.temp_max).fold(f64::NEG_INFINITY, f64::max);
    let mut y0: f64 = ((low - 3.0) / 5.0).floor() * 5.0;
    let mut y1: f64 = ((high + 3.0) / 5.0).ceil() * 5.0;
    if y1 - y0 < 15.0 {
        let pad = ((15.0 - (y1 - y0)) / 10.0).ceil() * 5.0;
        y0 -= pad;
        y1 += pad;
    }
    let y_of = |t: f64| (t - y0) / (y1 - y0);
    let step = if y1 - y0 <= 30.0 { 5.0 } else { 10.0 };
    let mut t = (y0 / step).ceil() * step;
    while t <= y1 + 1e-9 {
        shown.yticks.push((f(y_of(t)), format!("{t:.0}°")));
        t += step;
    }

    let fan_high = bins.iter().filter_map(|(_, b)| b.fan()).fold(0.0, f64::max);
    let fan_top = (fan_high / 500.0).ceil() * 500.0;
    shown.fan_top = if fan_top > 0.0 {
        format!("{fan_top:.0}")
    } else {
        String::new()
    };

    let every = if bins.len() > LABELED { 2 } else { 1 };
    let path = |value: &dyn Fn(&Bin) -> f64| {
        let mut d = String::with_capacity(bins.len() * 16);
        let mut previous: Option<i32> = None;
        for (ratio, b) in &bins {
            let verb = if previous.is_some_and(|p| ratio - p == 1) {
                'L'
            } else {
                'M'
            };
            let _ = write!(
                d,
                "{verb} {:.1} {:.1} ",
                x_of(*ratio) * 1000.0,
                (1.0 - y_of(value(b))) * 1000.0
            ); // into a String
            previous = Some(*ratio);
        }
        d
    };
    shown.mean_path = path(&|b| b.mean().unwrap_or(0.0));
    shown.peak_path = path(&|b| b.temp_max);
    for (i, (ratio, b)) in bins.iter().enumerate() {
        let mean = b.mean().unwrap_or(0.0);
        let fan = b.fan();
        let labeled = i.is_multiple_of(every);
        let mut tip = format!(
            "×{ratio}: {} срезов ({})\nø средняя {mean:.1} °C, ▲ наибольшая {:.0} °C",
            grouped(u64::from(b.n)),
            span_of(b.n),
            b.temp_max
        );
        match fan {
            Some(rpm) => {
                let _ = write!(
                    tip,
                    "\nвентилятор процессора ≈ {} об/мин (в {} срезах)",
                    grouped(rpm_u64(rpm)),
                    b.fan_n
                ); // into a String
            }
            None => tip.push_str("\nобороты вентилятора в эти срезы неизвестны"),
        }
        shown.points.push(PointShown {
            ratio: *ratio,
            x: f(x_of(*ratio)),
            mean: f(y_of(mean)),
            peak: f(y_of(b.temp_max)),
            fan: match fan {
                Some(rpm) if fan_top > 0.0 => f(rpm / fan_top),
                _ => -1.0,
            },
            label: if labeled { format!("×{ratio}") } else { String::new() },
            count: if labeled {
                grouped(u64::from(b.n))
            } else {
                String::new()
            },
            tip,
            samples: b.n,
        });
    }
    shown
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fan speeds are 0..10 000, rounded, non-negative"
)]
fn rpm_u64(rpm: f64) -> u64 {
    rpm.max(0.0).round() as u64
}

// ----- перемычки соседей -----

/// Сила связи соседей, °C → цвет: серый (нет разницы), через оранжевый к красному на 6 °C и выше.
pub fn link_color(delta: f64) -> Color {
    let grey = rgb(DIM);
    let orange = rgb(0x00ff_b347);
    let red = rgb(0x00ff_5d5d);
    let d = f(delta);
    if d <= 0.5 {
        grey
    } else if d <= 3.0 {
        orange.mix(&grey, (d - 0.5) / 2.5)
    } else {
        red.mix(&orange, ((d - 3.0) / 3.0).min(1.0))
    }
}

/// Толщина перемычки (пиксели при масштабе 1): 3 при нуле, до 10 на 6 °C.
pub fn link_thickness(delta: f64) -> f32 {
    3.0 + f(delta.clamp(0.0, 6.0)) * 7.0 / 6.0
}

/// Перемычки между соседями, где есть с чем сравнить; Some(°C) — средняя по двум направлениям.
pub fn links(hello: &Hello, geometry: &Geometry, digest: &Digest) -> Vec<(LinkData, f64)> {
    let block_of = |tile: usize| hello.layout.tiles.get(tile).and_then(|t| hello.blocks.get(t.block));
    let mut out = Vec::new();
    for link in &geometry.links {
        let (Some(a), Some(b)) = (block_of(link.a), block_of(link.b)) else {
            continue;
        };
        let effect = |me: &BlockInfo, other: &BlockInfo| {
            digest
                .tally
                .blocks
                .get(&me.name)
                .and_then(|t| t.with.get(&other.name))
                .and_then(samples::effect)
        };
        let (ab, ba) = (effect(a, b), effect(b, a));
        let weight = |e: Option<samples::Effect>| e.map_or(0.0, |e| f64::from(e.together.min(e.alone)));
        let (wa, wb) = (weight(ab), weight(ba));
        if wa + wb <= 0.0 {
            continue;
        }
        let delta = (ab.map_or(0.0, |e| e.delta) * wa + ba.map_or(0.0, |e| e.delta) * wb) / (wa + wb);
        let line = |me: &BlockInfo, other: &BlockInfo, e: Option<samples::Effect>| match e {
            Some(e) => format!(
                "• {} {} на {:.1} °C, когда {} тоже занят (вместе {} срезов, порознь {})",
                whom(me),
                if e.delta >= 0.0 {
                    "горячее"
                } else {
                    "холоднее"
                },
                e.delta.abs(),
                other.name,
                grouped(u64::from(e.together)),
                grouped(u64::from(e.alone))
            ),
            None => format!(
                "• {}: не с чем сравнить — мало срезов, где он занят без соседа или с ним",
                whom(me)
            ),
        };
        let tip = format!(
            "Соседи {} и {}\n{}\n{}\nСравнение — при одном и том же множителе блока: занят ли сосед в тот же срез.",
            a.name,
            b.name,
            line(a, b, ab),
            line(b, a, ba)
        );
        let place = link.place(link_thickness(delta), geometry.width, geometry.height);
        out.push((
            LinkData {
                x: place.x,
                y: place.y,
                w: place.w,
                h: place.h,
                color: link_color(delta),
                tip: tip.into(),
            },
            delta,
        ));
    }
    out
}

/// Подсказка плитки: всё, что о блоке известно сейчас, словами.
#[derive(Debug)]
pub struct TileFacts<'a> {
    pub block: &'a BlockInfo,
    pub cores: &'a [crate::wire::CoreInfo],
    pub cur: &'a [Option<f64>],
    pub temp: Option<f64>,
    pub peak: Option<f64>,
    pub mean: Option<f64>,
    pub ratio: Option<f64>,
    pub mean_ratio: Option<f64>,
    pub throttled: bool,
    pub ceiling: Ceiling,
    pub fused: Option<i64>,
}

pub fn tile_tip(t: &TileFacts) -> String {
    let b = t.block;
    let mut tip = format!(
        "{} ({}) — щелчок: статистика, правая кнопка: потолок{}",
        capital(&whom(b)),
        if b.kind == "E" {
            "E-ядра, потолок общий"
        } else {
            "P-ядро"
        },
        ""
    );
    if b.members.len() > 1 {
        let parts: Vec<String> = b
            .members
            .iter()
            .map(|&i| {
                let lp = t.cores.get(i).map_or_else(|| "?".into(), |c| c.lp.to_string());
                let temp = t
                    .cur
                    .get(i)
                    .copied()
                    .flatten()
                    .map_or_else(|| "–".into(), |v| format!("{v:.0}°"));
                format!("{lp}: {temp}")
            })
            .collect();
        let _ = write!(tip, "\n• ядра сейчас — {}", parts.join(", ")); // into a String
    } else if let Some(v) = t.temp {
        let _ = write!(tip, "\n• сейчас {v:.0} °C"); // into a String
    }
    if let Some(r) = t.ratio {
        let _ = write!(tip, ", множитель {r:.1}"); // into a String
    }
    if let Some(p) = t.peak {
        let _ = write!(tip, "\n• ▲ наибольшая с последнего сброса: {p:.0} °C"); // into a String
    }
    match (t.mean, t.mean_ratio) {
        (None, None) => tip.push_str("\n• ø под нагрузкой: ещё нет — считается, пока блок нагружен"),
        (m, r) => {
            let _ = write!(
                tip,
                "\n• ø под нагрузкой: {}{}",
                m.map_or_else(String::new, |m| format!("{m:.1} °C")),
                r.map_or_else(String::new, |r| format!(", множитель {r:.1}"))
            ); // into a String
        }
    }
    tip.push_str("\n• ▲ и ø обнуляет кнопка над схемой");
    if t.throttled {
        tip.push_str("\n• сейчас сбрасывает частоту из-за перегрева");
    }
    let c = t.ceiling;
    if let Some(now) = c.now {
        let _ = write!(tip, "\n• потолок ×{now}"); // into a String
        if let Some(fused) = t.fused {
            let _ = write!(tip, ", заводской ×{fused}"); // into a String
        }
        if let Some(was) = c.was
            && was != now
        {
            let _ = write!(tip, "; было ×{was}"); // into a String
        }
    }
    if c.auto != 0 {
        let _ = write!(
            tip,
            "\n• автомодуль сдвинул на {} {} от принятого",
            c.auto.abs(),
            if c.auto.abs() == 1 {
                "ступень"
            } else if c.auto.abs() < 5 {
                "ступени"
            } else {
                "ступеней"
            }
        ); // into a String
    }
    if let Some(ban) = c.ban {
        let _ = write!(
            tip,
            "\n• автомодулю нельзя поднимать до ×{ban}: блок на нём перегревался"
        ); // into a String
    }
    tip
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::{Columns, Scratch, Tally};
    use crate::wire::{Layout, ProfileSet, Startup, Tile};

    fn block(index: usize, name: &str, members: Vec<usize>) -> BlockInfo {
        BlockInfo {
            index,
            first: members[0],
            lp: 0,
            kind: if members.len() > 1 { "E".into() } else { "P".into() },
            name: name.into(),
            members,
        }
    }

    fn hello() -> Hello {
        Hello {
            cpu: String::new(),
            simulated: true,
            cores: Vec::new(),
            blocks: vec![block(0, "0", vec![0]), block(1, "2–5", vec![1, 2, 3, 4])],
            layout: Layout {
                note: None,
                confirmed: true,
                cols: 1,
                rows: 2,
                tiles: vec![
                    Tile {
                        block: 0,
                        col: 0,
                        row: 0,
                        places: vec![0],
                    },
                    Tile {
                        block: 1,
                        col: 0,
                        row: 1,
                        places: vec![1, 2, 3, 4],
                    },
                ],
            },
        }
    }

    fn profiles() -> Profiles {
        Profiles {
            startup: Startup {
                exists: true,
                name: Some("тихий".into()),
                describe: Some("при входе ставится «тихий»".into()),
                ..Startup::default()
            },
            sets: vec![ProfileSet {
                name: "тихий".into(),
                ratios: vec![50, 44, 44, 44, 44],
            }],
            standing: None,
            was_named: Some("тихий".into()),
            can_accept: true,
        }
    }

    #[test]
    fn profile_panel_names_differences_and_what_save_keeps() {
        let hello = hello();
        let profiles = profiles();
        let limits = Limits {
            fused: vec![53, 46, 46, 46, 46],
            was: vec![50, 44, 44, 44, 44],
            choices: Vec::new(),
        };
        // Сейчас: ядро 0 владелец поднял до ×52 (+2), кластер автомодуль опустил с ×44 на ×43.
        let limit = [52, 43, 43, 43, 43];
        let auto = [None, Some(AutoMark::Ceiling(43))];
        let shown = profile_panel(&ProfileInput {
            hello: &hello,
            profiles: &profiles,
            limits: Some(&limits),
            limit: Some(&limit),
            auto: Overlay {
                marks: Some(&auto),
                intended: None,
            },
            chosen: Some("тихий"),
            name: "",
            busy: false,
        });
        assert_eq!(shown.was, "Было — «тихий» · при входе ставится «тихий»");
        assert_eq!(shown.now, "Сейчас — не сохранено: 0 +2, 2–5 −1");
        assert!(shown.attention && shown.can_revert && shown.can_open && shown.can_save && shown.can_delete);
        assert!(!shown.can_save_as, "без названия «сохранить как» недоступно");
        assert!(
            shown.save_note.contains("2–5 −1"),
            "поправка автомодуля названа: {}",
            shown.save_note
        );
        assert_eq!(shown.rows.len(), 2);
        assert_eq!(shown.rows[0].name, "заводские");
        let quiet = &shown.rows[1];
        assert!(quiet.was && quiet.startup && quiet.selected && !quiet.standing);
        assert!(quiet.tip.contains("у 2 из 2 блоков"), "{}", quiet.tip);
        // Клетка кластера: в наборе ×44, сейчас ×43 — набор выше, жёлтая сторона.
        assert_ne!(quiet.cells[1], rgb(SAME));
    }

    #[test]
    fn save_as_names_an_existing_profile_as_overwrite() {
        let hello = hello();
        let profiles = profiles();
        let shown = profile_panel(&ProfileInput {
            hello: &hello,
            profiles: &profiles,
            limits: None,
            limit: None,
            auto: Overlay::default(),
            chosen: None,
            name: "  Тихий ",
            busy: false,
        });
        assert!(shown.can_save_as);
        assert_eq!(shown.save_as_text, "перезаписать");
        assert!(
            !shown.can_save && !shown.can_open && !shown.can_delete,
            "ничего не выбрано"
        );
        let long = "щ".repeat(NAME_LENGTH + 1);
        let refused = profile_panel(&ProfileInput {
            name: &long,
            ..ProfileInput {
                hello: &hello,
                profiles: &profiles,
                limits: None,
                limit: None,
                auto: Overlay::default(),
                chosen: None,
                name: "",
                busy: false,
            }
        });
        assert!(!refused.can_save_as, "название длиннее {NAME_LENGTH} движок не примет");
    }

    #[test]
    fn owner_delta_excludes_the_auto_offset() {
        let b = block(1, "2–5", vec![1, 2, 3, 4]);
        let limits = Limits {
            fused: vec![53, 46, 46, 46, 46],
            was: vec![50, 45, 45, 45, 45],
            choices: Vec::new(),
        };
        let limit = [50, 42, 42, 42, 42];
        let detail = [
            None,
            Some(AutoMark::Detail {
                to: Some(42),
                base: Some(44),
                ban: Some(44),
                until: None,
            }),
        ];
        let c = ceiling(
            &b,
            Some(&limit),
            Some(&limits),
            Overlay {
                marks: Some(&detail),
                intended: None,
            },
        );
        // Принятый ×44 (владелец −1 от «было» ×45), автомодуль ещё −2.
        assert_eq!((c.auto, c.owner_delta(), c.ban), (-2, -1, Some(44)));
        // Программа входа шлёт только потолок надстройки и принятые потолки по ядрам — тот же ответ.
        let plain = [None, Some(AutoMark::Ceiling(42))];
        let intended = [Some(50), Some(44), Some(44), Some(44), Some(44)];
        let c = ceiling(
            &b,
            Some(&limit),
            Some(&limits),
            Overlay {
                marks: Some(&plain),
                intended: Some(&intended),
            },
        );
        assert_eq!((c.auto, c.owner_delta(), c.ban), (-2, -1, None));
    }

    fn digest(rows: &[String]) -> Digest {
        let header =
            "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp,2–5.ceiling,2–5.ratio,2–5.busy,2–5.temp";
        let columns = Columns::parse(header).expect("заголовок");
        let mut tally = Tally::default();
        Scratch::default().count(&columns, &rows.join("\n"), &mut tally);
        Digest {
            tally,
            ..Digest::default()
        }
    }

    #[test]
    fn chart_points_follow_bins_and_counts() {
        let mut rows = Vec::new();
        for i in 0..30 {
            let (ratio, temp) = if i < 20 { (50, 80) } else { (48, 70) };
            rows.push(format!(
                "2026-10-15 10:00:{i:02},60,{},,53,{ratio},1,{temp},46,45,0,60",
                4000 + i
            ));
        }
        let d = digest(&rows);
        let s = stats_panel(&hello().blocks[0], Some(&d));
        assert_eq!(s.empty, "");
        let ratios: Vec<i32> = s.points.iter().map(|p| p.ratio).collect();
        assert_eq!(ratios, [48, 50], "только корзины с данными, по возрастанию множителя");
        assert_eq!(s.points.iter().map(|p| p.samples).collect::<Vec<_>>(), [10, 20]);
        assert_eq!(s.points[1].count, "20");
        assert!(
            s.points[0].x < s.points[1].x && s.points[0].mean < s.points[1].mean,
            "правее и выше — горячее"
        );
        assert!(
            s.points
                .iter()
                .all(|p| (0.0..=1.0).contains(&p.mean) && (0.0..=1.0).contains(&p.peak) && p.fan > 0.0 && p.fan <= 1.0)
        );
        // ×49 пуст: линия средней рвётся (два «M»), а не проводится через пустую корзину.
        assert_eq!(s.mean_path.matches('M').count(), 2);
        assert_eq!(s.note, "30 из 30 срезов под нагрузкой · 15.10 10:00–10:00");
        // Кластер ни разу не занят — строка вместо графика.
        let e = stats_panel(&hello().blocks[1], Some(&d));
        assert!(e.points.is_empty() && e.empty.contains("ни разу не был занят"));
    }

    #[test]
    fn link_strength_grows_with_the_difference() {
        assert_eq!(link_color(0.0), rgb(DIM));
        assert!(link_thickness(6.0) > link_thickness(3.0) && link_thickness(3.0) > link_thickness(0.0));
        assert_eq!(link_thickness(20.0), link_thickness(6.0));
        assert_ne!(link_color(3.0), link_color(6.0));
    }

    #[test]
    fn grouped_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(43_200), "43 200");
        assert_eq!(grouped(1_234_567), "1 234 567");
    }
}
