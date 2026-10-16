// Что показывают панель «Профили», полосы тепловой связи соседей на схеме и подсказка плитки: из сообщений движка и
// сводки статистики (store.rs) — строки, цвета и места для ui/app.slint (график статистики — chart.rs). Здесь нет ни
// Slint-окна, ни потоков: функции получают данные и возвращают, что показать, — их проверяют модульные тесты.
use std::fmt::Write as _;

use serde_json::{Map, Value};
use slint::{Color, SharedString};

use crate::die::Geometry;
use crate::samples;
use crate::store::Digest;
use crate::wire::{AutoMark, BlockInfo, Hello, Limits, Profiles, State};
use crate::{LineRow, LinkData, SetRow};

pub fn rgb(hex: u32) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    Color::from_rgb_u8(r, g, b)
}

const TEXT: u32 = 0x00e6_e8ec;

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
    /// Средний потолок набора по P-ядрам и по E-ядрам («52.4»); "" — таких ядер нет или потолков нет.
    pub p_mean: String,
    pub e_mean: String,
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
            p_mean: self.p_mean.as_str().into(),
            e_mean: self.e_mean.as_str().into(),
            tip: self.tip.as_str().into(),
        }
    }
}

/// Средний потолок набора по P-ядрам и по E-ядрам — оценка уровня набора (владелец 15.10: «средний множитель по
/// эффективным ядрам и… по производительным… две такие вот цифры оценочные»). У кластера E-ядер потолок общий, поэтому
/// среднее по кластерам равно среднему по ядрам.
pub fn kind_means(blocks: &[BlockInfo], ratios: Option<&[i64]>) -> (Option<f64>, Option<f64>) {
    let mean = |kind: &str| {
        let values: Vec<i64> = blocks
            .iter()
            .filter(|b| b.kind == kind)
            .filter_map(|b| ratios?.get(b.first).copied().filter(|&v| v > 0))
            .collect();
        average(&values)
    };
    (mean("P"), mean("E"))
}

#[expect(clippy::cast_precision_loss, reason = "a dozen ceilings of tens: exact in f64")]
fn average(values: &[i64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<i64>() as f64 / values.len() as f64)
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
        let mut differ = Vec::new();
        for (b, c) in blocks.iter().zip(&ceilings) {
            let set = ratios.and_then(|r| r.get(b.first).copied()).filter(|&v| v > 0);
            if let (Some(s), Some(n)) = (set, c.now)
                && s != n
            {
                differ.push(format!("{} ×{s} (сейчас ×{n})", b.name));
            }
        }
        let (p_mean, e_mean) = kind_means(blocks, ratios);
        let one = |m: Option<f64>| m.map_or_else(String::new, |m| format!("{m:.1}"));
        let title = titled(Some(key));
        let level = match (p_mean, e_mean) {
            (Some(p), Some(e)) => format!("\nсредний потолок: P-ядра ×{p:.1}, E-ядра ×{e:.1}"),
            (Some(p), None) => format!("\nсредний потолок P-ядер ×{p:.1}"),
            (None, Some(e)) => format!("\nсредний потолок E-ядер ×{e:.1}"),
            (None, None) => String::new(),
        };
        let tip = if differ.is_empty() {
            format!("{title}: то же, что стоит сейчас{level}")
        } else {
            format!(
                "{title}: отличается от того, что стоит сейчас, у {} из {} блоков{level}\n• {}",
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
            p_mean: one(p_mean),
            e_mean: one(e_mean),
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
                mark: Color::default(),
            }
        })
        .collect()
}

// ----- общие подписи -----

#[expect(
    clippy::cast_possible_truncation,
    reason = "chart fractions and degrees, well inside f32"
)]
pub(crate) fn f(v: f64) -> f32 {
    v as f32
}

/// «2 мин 10 с» — сколько времени покрывают срезы (по их времени: строка прежнего журнала — 2 с, нового — 0,25 с).
pub(crate) fn span_of(secs: f64) -> String {
    if secs < 10.0 {
        return format!("{secs:.1} с");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded, non-negative seconds of a month of samples at most"
    )]
    let s = secs.round() as u64;
    match s {
        s if s < 60 => format!("{s} с"),
        s if s < 3600 => format!("{} мин {} с", s / 60, s % 60),
        s => format!("{} ч {} мин", s / 3600, s % 3600 / 60),
    }
}

/// Доля работы словами: «63 %».
pub(crate) fn percent(share: f64) -> String {
    format!("{:.0} %", share * 100.0)
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

// ----- тепловая связь соседей -----

/// Шкала связи соседей, °C → цвет: точки шкалы, между ними — плавный переход (владелец 15.10: «почему не плавные
/// градации»). 0 — разницы нет (серый), 6 °C и больше — красный. Те же точки — у полосы-легенды под схемой (app.slint).
const LINK_SCALE: [(f32, u32); 4] = [
    (0.0, 0x005b_616d),
    (1.5, 0x00e8_c547),
    (3.5, 0x00ff_9f43),
    (6.0, 0x00ff_5d5d),
];

/// Сила связи соседей, °C → цвет по шкале `LINK_SCALE`, без ступеней.
pub fn link_color(delta: f64) -> Color {
    let d = f(delta);
    let mut lower = LINK_SCALE[0];
    for upper in LINK_SCALE {
        if d <= upper.0 {
            let span = upper.0 - lower.0;
            let k = if span > 0.0 { (d - lower.0) / span } else { 1.0 };
            // mix: доля k — у первого цвета, остальное — у второго.
            return rgb(upper.1).mix(&rgb(lower.1), k.clamp(0.0, 1.0));
        }
        lower = upper;
    }
    rgb(lower.1)
}

/// Толщина полосы (пиксели при масштабе 1): 2 при нуле, растёт ровно до полного промежутка на 6 °C.
pub fn link_thickness(delta: f64) -> f32 {
    2.0 + f(delta.clamp(0.0, 6.0)) * (crate::die::BAND_MAX - 2.0) / 6.0
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
        let weight = |e: Option<samples::Effect>| e.map_or(0.0, |e| e.together.min(e.alone));
        let (wa, wb) = (weight(ab), weight(ba));
        if wa + wb <= 0.0 {
            continue;
        }
        let delta = (ab.map_or(0.0, |e| e.delta) * wa + ba.map_or(0.0, |e| e.delta) * wb) / (wa + wb);
        let line = |me: &BlockInfo, other: &BlockInfo, e: Option<samples::Effect>| match e {
            Some(e) => format!(
                "• {} {} на {:.1} °C, когда {} тоже занят (вместе {}, порознь {})",
                whom(me),
                if e.delta >= 0.0 {
                    "горячее"
                } else {
                    "холоднее"
                },
                e.delta.abs(),
                other.name,
                span_of(e.together),
                span_of(e.alone)
            ),
            None => format!(
                "• {}: не с чем сравнить — меньше {:.0} с, где он занят без соседа или с ним",
                whom(me),
                samples::MIN_PAIR
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
        "{} ({}) — щелчок: добавить в статистику или убрать, правая кнопка: потолок{}",
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
        let _ = write!(
            tip,
            "\n• ▲ наибольшая с последнего сброса: {p:.0} °C — её цвет у полоски слева"
        ); // into a String
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

pub(crate) fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // Средние потолки набора: P — одно ядро ×50, E — кластер ×44; у заводских — заводские.
        assert_eq!((quiet.p_mean.as_str(), quiet.e_mean.as_str()), ("50.0", "44.0"));
        assert_eq!(
            (shown.rows[0].p_mean.as_str(), shown.rows[0].e_mean.as_str()),
            ("53.0", "46.0")
        );
    }

    #[test]
    fn kind_means_average_each_kind_over_its_blocks() {
        let blocks = vec![
            block(0, "0", vec![0]),
            block(1, "1", vec![1]),
            block(2, "2–5", vec![2, 3, 4, 5]),
            block(3, "6–9", vec![6, 7, 8, 9]),
        ];
        // P: ×52 и ×53 → 52.5; E: кластеры ×44 и ×45 (по ядрам те же) → 44.5; неизвестный (0) не считается.
        let ratios = [52, 53, 44, 44, 44, 44, 45, 45, 45, 45];
        assert_eq!(kind_means(&blocks, Some(&ratios)), (Some(52.5), Some(44.5)));
        let partial = [0, 53, 44, 44, 44, 44, 0, 0, 0, 0];
        assert_eq!(kind_means(&blocks, Some(&partial)), (Some(53.0), Some(44.0)));
        assert_eq!(kind_means(&blocks, None), (None, None));
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

    #[test]
    fn spans_read_as_time() {
        assert_eq!(span_of(0.25), "0.2 с");
        assert_eq!(span_of(9.75), "9.8 с");
        assert_eq!(span_of(31.0), "31 с");
        assert_eq!(span_of(130.0), "2 мин 10 с");
        assert_eq!(span_of(3.0 * 3600.0 + 600.0), "3 ч 10 мин");
    }

    #[test]
    fn link_strength_grows_with_the_difference() {
        assert_eq!(link_color(0.0), rgb(LINK_SCALE[0].1));
        assert_eq!(
            link_color(-1.0),
            rgb(LINK_SCALE[0].1),
            "холоднее с соседом — как без разницы"
        );
        assert_eq!(link_color(9.0), rgb(LINK_SCALE[3].1));
        assert!(link_thickness(6.0) > link_thickness(3.0) && link_thickness(3.0) > link_thickness(0.0));
        // Плавно: на каждой сотой градуса цвет меняется мало, ступеней нет — и на точках шкалы тоже.
        let channels = |c: Color| [i32::from(c.red()), i32::from(c.green()), i32::from(c.blue())];
        let mut previous = channels(link_color(0.0));
        for step in 1..=700 {
            let now = channels(link_color(f64::from(step) / 100.0));
            let jump = now.iter().zip(&previous).map(|(a, b)| (a - b).abs()).max().unwrap_or(0);
            assert!(jump <= 3, "скачок цвета {jump} на {} °C", f64::from(step) / 100.0);
            previous = now;
        }
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
