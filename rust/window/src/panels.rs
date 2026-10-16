// Что показывают панель «Профили», полосы тепловой связи соседей на схеме и подсказка плитки: из сообщений движка и
// сводки статистики (store.rs) — строки, цвета и места для ui/app.slint (график статистики — chart.rs). Здесь нет ни
// Slint-окна, ни потоков: функции получают данные и возвращают, что показать, — их проверяют модульные тесты.
use std::fmt::Write as _;

use slint::Color;

use crate::die::Geometry;
use crate::samples;
use crate::store::Digest;
use crate::wire::{AutoMark, BlockInfo, Hello, Limits, Profiles, State};
use crate::{LinkData, SetRow};

pub fn rgb(hex: u32) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    Color::from_rgb_u8(r, g, b)
}

/// Название набора для надписей: профиль в кавычках или «заводские».
pub fn titled(key: &str) -> String {
    if key.is_empty() {
        "заводские".into()
    } else {
        format!("«{key}»")
    }
}

/// Одинаковые ли названия профилей (как сравнивает программа у часов: без учёта регистра).
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
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

// ----- потолки блоков: сейчас, в выбранном профиле, поправка автомодуля -----

/// Потолки блока: сейчас (текущее состояние), в выбранном профиле, поправка автомодуля (на сколько ступеней он сдвинул
/// блок от потолка владельца).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ceiling {
    pub now: Option<i64>,
    pub profile: Option<i64>,
    pub auto: i64,
    pub ban: Option<i64>,
}

impl Ceiling {
    /// Разница текущего состояния с выбранным профилем (сейчас минус профиль, с поправкой автомодуля); 0 — нет или не
    /// известно.
    pub fn delta(&self) -> i64 {
        match (self.now, self.profile) {
            (Some(now), Some(profile)) => now - profile,
            _ => 0,
        }
    }
}

/// Надстройка автомодуля из состояния (программа входа): что он держит по блокам и потолки владельца по ядрам — от них
/// считается его поправка.
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

/// Потолки блока: `limit` — что стоит по ядрам (состояние), `profile` — выбранный профиль по ядрам.
pub fn ceiling(b: &BlockInfo, limit: Option<&[i64]>, profile: Option<&[i64]>, overlay: Overlay) -> Ceiling {
    let known = |v: i64| (v > 0).then_some(v);
    let now = limit.and_then(|l| l.get(b.first).copied()).and_then(known);
    let in_profile = profile.and_then(|p| p.get(b.first).copied()).and_then(known);
    let mark = overlay.marks.and_then(|a| a.get(b.index)).and_then(Option::as_ref);
    let intended = overlay
        .intended
        .and_then(|i| i.get(b.first).copied().flatten())
        .and_then(known);
    let auto = mark
        .and_then(|m| Some(m.ceiling()? - m.base().or(intended)?))
        .unwrap_or(0);
    Ceiling {
        now,
        profile: in_profile,
        auto,
        ban: mark.and_then(|m| m.ban().map(|(at, _)| at)),
    }
}

/// Потолки выбранного профиля по ядрам: «» — заводские (из потолков), иначе сохранённый набор; None — ничего не
/// выбрано или такого нет.
pub fn chosen_ratios<'a>(
    profiles: Option<&'a Profiles>,
    limits: Option<&'a Limits>,
    chosen: Option<&str>,
) -> Option<&'a [i64]> {
    match chosen? {
        "" => limits.map(|l| l.fused.as_slice()),
        key => profiles?
            .sets
            .iter()
            .find(|s| s.name == key)
            .map(|s| s.ratios.as_slice()),
    }
}

/// Выбор по умолчанию, когда окно открылось (или выбранного больше нет): профиль, который загружается при старте
/// Windows; его нет — тот, что совпадает с текущим состоянием; и его нет — «заводские».
pub fn default_choice(profiles: &Profiles) -> String {
    let listed = |name: &str| {
        profiles
            .sets
            .iter()
            .find(|s| same_name(&s.name, name))
            .map(|s| s.name.clone())
    };
    profiles
        .startup
        .exists
        .then(|| profiles.startup.name.as_deref().and_then(listed))
        .flatten()
        .or_else(|| {
            profiles
                .standing
                .as_deref()
                .filter(|s| s.is_empty() || listed(s).is_some())
                .map(str::to_string)
        })
        .unwrap_or_default()
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
    reason = "one flag per button and box of the panel, read by name"
)]
pub struct ProfileShown {
    /// Строка: выбранный профиль и текущее состояние (владелец 15.10: «в дельте… разницу между… параметрам профиля и
    /// параметрам текущего состояния»).
    pub status: String,
    pub status_tip: String,
    pub attention: bool,
    pub rows: Vec<SetShown>,
    pub can_apply: bool,
    pub can_save: bool,
    pub can_save_as: bool,
    pub can_delete: bool,
    /// «Загружать при старте Windows»: можно ли отметить (только сохранённый профиль) и отмечен ли выбранный.
    pub can_startup: bool,
    pub startup_on: bool,
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
    /// Совпадает с текущим состоянием.
    pub standing: bool,
    /// Загружается при старте Windows (знак в строке — единственное место, где это видно).
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

/// «23 ×46 → ×45 (авто −1)»: блок, в профиле, сейчас; поправка автомодуля — в скобках (она входит в текущее
/// состояние). `now_word` — «сейчас» перед текущим (у первого блока строки).
fn difference(b: &BlockInfo, c: &Ceiling, now_word: bool) -> Option<String> {
    let (Some(profile), Some(now)) = (c.profile, c.now) else {
        return None;
    };
    (profile != now).then(|| {
        let auto = if c.auto == 0 {
            String::new()
        } else {
            format!(" (авто {})", signed(c.auto))
        };
        format!(
            "{} ×{profile} → {}×{now}{auto}",
            b.name,
            if now_word { "сейчас " } else { "" }
        )
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "the panel's rules read top to bottom: rows, status, buttons"
)]
pub fn profile_panel(input: &ProfileInput) -> ProfileShown {
    let p = input.profiles;
    let blocks = &input.hello.blocks;
    let fused = input.limits.map(|l| l.fused.as_slice());
    let startup_name = p
        .startup
        .exists
        .then(|| p.startup.name.as_deref().unwrap_or(""))
        .filter(|n| !n.is_empty());

    // Строки: заводские, затем сохранённые наборы в порядке программы у часов.
    let mut rows = Vec::with_capacity(p.sets.len() + 1);
    let entries =
        std::iter::once(("", fused)).chain(p.sets.iter().map(|s| (s.name.as_str(), Some(s.ratios.as_slice()))));
    for (key, ratios) in entries {
        let mut differ = Vec::new();
        for b in blocks {
            let c = ceiling(b, input.limit, ratios, input.auto);
            if let Some(d) = difference(b, &c, false) {
                differ.push(d);
            }
        }
        let (p_mean, e_mean) = kind_means(blocks, ratios);
        let one = |m: Option<f64>| m.map_or_else(String::new, |m| format!("{m:.1}"));
        let title = titled(key);
        let level = match (p_mean, e_mean) {
            (Some(p), Some(e)) => format!("\nсредний потолок: P-ядра ×{p:.1}, E-ядра ×{e:.1}"),
            (Some(p), None) => format!("\nсредний потолок P-ядер ×{p:.1}"),
            (None, Some(e)) => format!("\nсредний потолок E-ядер ×{e:.1}"),
            (None, None) => String::new(),
        };
        let startup = startup_name.is_some_and(|s| same_name(s, key));
        let mut tip = if differ.is_empty() {
            format!("{title}: совпадает с текущим состоянием{level}")
        } else {
            format!(
                "{title}: отличается от текущего состояния у {} из {} блоков (в профиле → сейчас){level}\n• {}",
                differ.len(),
                blocks.len(),
                differ.join("\n• ")
            )
        };
        if startup {
            // Профиль запуска виден только знаком в строке (владелец 15.10); его состояние — здесь, в подсказке строки.
            tip.push_str("\n► загружается при старте Windows");
            for extra in [p.startup.details.as_deref(), p.startup.note.as_deref()]
                .into_iter()
                .flatten()
            {
                for line in extra.lines().skip_while(|l| l.starts_with("Профиль запуска")) {
                    tip.push_str("\n• ");
                    tip.push_str(line.trim_start_matches("• ").trim());
                }
            }
        }
        rows.push(SetShown {
            key: key.to_string(),
            name: if key.is_empty() {
                "заводские".into()
            } else {
                key.to_string()
            },
            standing: p.standing.as_deref() == Some(key),
            startup,
            disabled: p.startup.disabled,
            selected: input.chosen == Some(key),
            p_mean: one(p_mean),
            e_mean: one(e_mean),
            tip,
        });
    }

    // Строка: выбранный профиль против текущего состояния — по всем блокам, где известны оба.
    let chosen = input.chosen;
    let ratios = chosen_ratios(Some(p), input.limits, chosen);
    let ceilings: Vec<Ceiling> = blocks
        .iter()
        .map(|b| ceiling(b, input.limit, ratios, input.auto))
        .collect();
    let compared = ceilings.iter().any(|c| c.now.is_some() && c.profile.is_some());
    let differing: Vec<(&BlockInfo, &Ceiling)> = blocks
        .iter()
        .zip(&ceilings)
        .filter(|(_, c)| matches!((c.profile, c.now), (Some(p), Some(n)) if p != n))
        .collect();
    let diffs: Vec<String> = differing.iter().filter_map(|(b, c)| difference(b, c, false)).collect();
    let title = chosen.map(titled);
    let (status, attention) = match (&title, compared) {
        (None, _) => ("Выбери профиль в списке".to_string(), false),
        (Some(t), _) if input.busy => (format!("{t} — идёт измерение: потолки временные"), false),
        (Some(t), false) => (format!("{t} — текущие потолки неизвестны"), false),
        (Some(t), true) if diffs.is_empty() => (format!("{t} — совпадает с текущим"), false),
        (Some(t), true) => {
            // «сейчас» — один раз, у первого блока: «23 ×46 → сейчас ×45, 2–5 ×44 → ×42 (авто −2)».
            let items: Vec<String> = differing
                .iter()
                .enumerate()
                .filter_map(|(i, (b, c))| difference(b, c, i == 0))
                .collect();
            (format!("{t} — отличается: {}", items.join(", ")), true)
        }
    };
    let autos = ceilings.iter().any(|c| c.auto != 0);
    let mut status_tip = String::from(
        "Выбранный профиль и текущее состояние\nТекущее состояние — потолки, которые стоят в процессоре сейчас; выбор строки в списке ничего не ставит.",
    );
    if !diffs.is_empty() {
        let _ = write!(
            status_tip,
            "\n• отличается (в профиле → сейчас):\n   {}",
            diffs.join("\n   ")
        ); // into a String
    }
    if autos {
        status_tip.push_str("\n• авто — поправка автомодуля, она входит в текущее состояние");
    }
    status_tip.push_str(
        "\n• «применить» ставит потолки профиля, «сохранить» пишет текущее состояние в профиль\n• на плитках — та же разница: сейчас минус профиль",
    );

    let autos_named: Vec<String> = blocks
        .iter()
        .zip(&ceilings)
        .filter(|(_, c)| c.auto != 0)
        .map(|(b, c)| format!("{} {}", b.name, signed(c.auto)))
        .collect();
    let save_note = if autos_named.is_empty() {
        "Сохраняется текущее состояние".to_string()
    } else {
        format!(
            "Сохраняется текущее состояние, с поправками автомодуля: {}",
            autos_named.join(", ")
        )
    };

    let chosen_saved = chosen.filter(|c| !c.is_empty() && p.sets.iter().any(|s| s.name == *c));
    let differs = compared && !diffs.is_empty();
    let name = input.name.trim();
    let exists = p.sets.iter().any(|s| same_name(&s.name, name));
    ProfileShown {
        status,
        status_tip,
        attention,
        rows,
        can_apply: !input.busy && chosen.is_some() && differs,
        can_save: !input.busy && chosen_saved.is_some() && differs,
        can_save_as: !input.busy && !name.is_empty() && name.chars().count() <= NAME_LENGTH,
        can_delete: chosen_saved.is_some(),
        can_startup: !input.busy && chosen_saved.is_some(),
        startup_on: chosen_saved.is_some_and(|c| startup_name.is_some_and(|s| same_name(s, c))),
        save_tip: chosen_saved.map_or_else(
            || "Сохранить в выбранный профиль\nВыбери сохранённый профиль в списке (не «заводские»).".to_string(),
            |c| {
                format!(
                    "Сохранить в «{c}»\nЗаписывает в него текущее состояние — профиль перезаписывается.\n• поправки автомодуля становятся потолками профиля\n• загрузку при старте Windows не меняет; если «{c}» загружается при старте — при старте будет новое"
                )
            },
        ),
        save_as_text: if exists { "перезаписать".into() } else { "сохранить как".into() },
        save_note,
    }
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
    /// Выбранный профиль: «csmart2» или «заводские»; None — ничего не выбрано.
    pub profile: Option<&'a str>,
    /// ▲ и ø по потолкам с последнего сброса (`ceilings_of`); пусто — программа их не шлёт (до 0.5.6).
    pub by_ceiling: &'a [crate::wire::ByCeiling],
}

/// ▲ и ø блока по потолкам из состояния: список блока (поле byCeiling — по блокам, как их знакомство), по убыванию
/// потолка (программа шлёт так же; порядок здесь — чтобы подсказка от него не зависела).
#[must_use]
pub fn ceilings_of(
    block: &BlockInfo,
    row: Option<&[Option<Vec<crate::wire::ByCeiling>>]>,
) -> Vec<crate::wire::ByCeiling> {
    let mut out = row
        .and_then(|r| r.get(block.index))
        .and_then(Option::as_ref)
        .cloned() // a few ceilings of one block, per tile refresh
        .unwrap_or_default();
    out.sort_by_key(|e| std::cmp::Reverse(e.ceiling));
    out
}

/// «по потолкам с последнего сброса» в подсказке плитки (дополнение брифа W): строка на потолок — ▲, ø и время под
/// нагрузкой, нынешний помечен. Программа до 0.5.6 их не шлёт — части нет.
fn ceiling_lines(t: &TileFacts, tip: &mut String) {
    if t.by_ceiling.is_empty() {
        return;
    }
    tip.push_str(
        "\nпо потолкам с последнего сброса (копятся, пока включён автомодуль или журнал, либо открыто окно; сбрасываются кнопкой или когда ты меняешь потолок блока):",
    );
    for e in t.by_ceiling {
        let _ = write!(
            tip,
            "\n• ×{}{}: ▲ {}, ø {}, под нагрузкой {}",
            e.ceiling,
            if t.ceiling.now == Some(e.ceiling) {
                " (сейчас)"
            } else {
                ""
            },
            e.max.map_or_else(|| "–".to_string(), |v| format!("{v:.0} °C")),
            e.mean.map_or_else(|| "–".to_string(), |v| format!("{v:.1} °C")),
            e.secs.map_or_else(|| "–".to_string(), span_of)
        ); // into a String
    }
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
    ceiling_lines(t, &mut tip);
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
        if let (Some(in_profile), Some(profile)) = (c.profile, t.profile)
            && in_profile != now
        {
            let _ = write!(tip, "; в {profile} ×{in_profile}"); // into a String
        }
    }
    if c.auto != 0 {
        let _ = write!(
            tip,
            "\n• автомодуль сдвинул на {} {} от потолка владельца",
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
                details: Some(
                    "Профиль запуска «тихий», сохранён 15.10 22:00\n• пробный, потому что в нём есть потолки выше заводских: пережил перезапусков 1, часов 2.0"
                        .into(),
                ),
                ..Startup::default()
            },
            sets: vec![
                ProfileSet {
                    name: "тихий".into(),
                    ratios: vec![50, 44, 44, 44, 44],
                },
                ProfileSet {
                    name: "жара".into(),
                    ratios: vec![48, 40, 40, 40, 40],
                },
            ],
            standing: None,
        }
    }

    fn limits() -> Limits {
        Limits {
            fused: vec![53, 46, 46, 46, 46],
            choices: Vec::new(),
        }
    }

    /// Панель при выбранном `chosen` и текущих потолках `limit` с надстройкой автомодуля `auto` (потолки владельца под
    /// ней — `intended`).
    fn panel(
        profiles: &Profiles,
        chosen: Option<&str>,
        limit: Option<&[i64]>,
        auto: &[Option<AutoMark>],
        intended: Option<&[Option<i64>]>,
    ) -> ProfileShown {
        profile_panel(&ProfileInput {
            hello: &hello(),
            profiles,
            limits: Some(&limits()),
            limit,
            auto: Overlay {
                marks: Some(auto),
                intended,
            },
            chosen,
            name: "",
            busy: false,
        })
    }

    #[test]
    fn the_status_line_compares_the_chosen_profile_with_the_current_state() {
        let p = profiles();
        let none = [None, None];
        // Совпадает: ни применить, ни сохранить нечего.
        let same = panel(&p, Some("тихий"), Some(&[50, 44, 44, 44, 44]), &none, None);
        assert_eq!(same.status, "«тихий» — совпадает с текущим");
        assert!(!same.attention && !same.can_apply && !same.can_save);
        // Отличается: каждый блок «в профиле → сейчас», «сейчас» — у первого; поправка автомодуля помечена.
        let intended = [Some(52), Some(44), Some(44), Some(44), Some(44)];
        let auto = [None, Some(AutoMark::Ceiling(43))];
        let moved = panel(&p, Some("тихий"), Some(&[52, 43, 43, 43, 43]), &auto, Some(&intended));
        assert_eq!(
            moved.status,
            "«тихий» — отличается: 0 ×50 → сейчас ×52, 2–5 ×44 → ×43 (авто −1)"
        );
        assert!(moved.attention && moved.can_apply && moved.can_save);
        assert!(
            moved.status_tip.contains("авто — поправка автомодуля"),
            "{}",
            moved.status_tip
        );
        // Только автомодуль сдвинул блок — это тоже отличие текущего состояния от профиля.
        let auto_only = panel(
            &p,
            Some("тихий"),
            Some(&[50, 42, 42, 42, 42]),
            &[None, Some(AutoMark::Ceiling(42))],
            Some(&[Some(50), Some(44), Some(44), Some(44), Some(44)]),
        );
        assert_eq!(auto_only.status, "«тихий» — отличается: 2–5 ×44 → сейчас ×42 (авто −2)");
        assert!(auto_only.can_save, "the auto correction can be saved into the profile");
        // Заводские: сравниваются заводские потолки; сохранить в них нельзя, применить — можно.
        let factory = panel(&p, Some(""), Some(&[50, 44, 44, 44, 44]), &none, None);
        assert_eq!(
            factory.status,
            "заводские — отличается: 0 ×53 → сейчас ×50, 2–5 ×46 → ×44"
        );
        assert!(factory.can_apply && !factory.can_save && !factory.can_delete);
        let factory_same = panel(&p, Some(""), Some(&[53, 46, 46, 46, 46]), &none, None);
        assert_eq!(factory_same.status, "заводские — совпадает с текущим");
        // Текущих потолков нет (процессором владеет другая часть): сравнивать не с чем, кнопки серые.
        let unknown = panel(&p, Some("тихий"), None, &none, None);
        assert_eq!(unknown.status, "«тихий» — текущие потолки неизвестны");
        assert!(!unknown.can_apply && !unknown.can_save);
        // Ничего не выбрано.
        assert_eq!(
            panel(&p, None, Some(&[50, 44, 44, 44, 44]), &none, None).status,
            "Выбери профиль в списке"
        );
    }

    #[test]
    fn the_startup_box_is_for_saved_profiles_only_and_shows_the_marked_one() {
        let p = profiles();
        let none = [None, None];
        let now: &[i64] = &[50, 44, 44, 44, 44];
        let marked = panel(&p, Some("тихий"), Some(now), &none, None);
        assert!(marked.can_startup && marked.startup_on);
        let other = panel(&p, Some("жара"), Some(now), &none, None);
        assert!(other.can_startup && !other.startup_on);
        let factory = panel(&p, Some(""), Some(now), &none, None);
        assert!(
            !factory.can_startup && !factory.startup_on,
            "«заводские» is not a saved profile"
        );
        // Профиля запуска нет — ни один не отмечен, но отметить можно.
        let mut unmarked = profiles();
        unmarked.startup = Startup::default();
        let shown = panel(&unmarked, Some("тихий"), Some(now), &none, None);
        assert!(shown.can_startup && !shown.startup_on);
        assert!(shown.rows.iter().all(|r| !r.startup));
        // Знак в списке — только у профиля запуска; его состояние (пробный) — в подсказке строки.
        let rows = &marked.rows;
        assert_eq!(
            rows.iter()
                .filter(|r| r.startup)
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["тихий"]
        );
        let tip = &rows[1].tip;
        assert!(
            tip.contains("► загружается при старте Windows") && tip.contains("• пробный"),
            "{tip}"
        );
        assert!(!tip.contains("Профиль запуска «тихий», сохранён"), "{tip}");
    }

    #[test]
    fn the_list_marks_what_matches_the_current_state() {
        let mut p = profiles();
        p.standing = Some("жара".into());
        let shown = panel(&p, Some("тихий"), Some(&[48, 40, 40, 40, 40]), &[None, None], None);
        let standing: Vec<&str> = shown
            .rows
            .iter()
            .filter(|r| r.standing)
            .map(|r| r.key.as_str())
            .collect();
        assert_eq!(standing, ["жара"]);
        assert!(shown.rows[1].selected && !shown.rows[2].selected);
        assert!(shown.rows[1].tip.contains("у 2 из 2 блоков"), "{}", shown.rows[1].tip);
        // Средние потолки набора: P — одно ядро ×50, E — кластер ×44; у заводских — заводские.
        assert_eq!(
            (shown.rows[1].p_mean.as_str(), shown.rows[1].e_mean.as_str()),
            ("50.0", "44.0")
        );
        assert_eq!(
            (shown.rows[0].p_mean.as_str(), shown.rows[0].e_mean.as_str()),
            ("53.0", "46.0")
        );
    }

    #[test]
    fn the_window_opens_on_the_startup_profile() {
        let p = profiles();
        assert_eq!(default_choice(&p), "тихий");
        // Название в файле профиля — другим регистром: выбирается строка списка.
        let mut cased = profiles();
        cased.startup.name = Some("ТИХИЙ".into());
        assert_eq!(default_choice(&cased), "тихий");
        // Профиля запуска нет — то, что совпадает с текущим; и его нет — заводские.
        let mut none = profiles();
        none.startup = Startup::default();
        none.standing = Some("жара".into());
        assert_eq!(default_choice(&none), "жара");
        none.standing = None;
        assert_eq!(default_choice(&none), "");
        none.standing = Some(String::new());
        assert_eq!(default_choice(&none), "");
    }

    #[test]
    fn busy_greys_apply_and_save() {
        let p = profiles();
        let shown = profile_panel(&ProfileInput {
            hello: &hello(),
            profiles: &p,
            limits: Some(&limits()),
            limit: Some(&[52, 44, 44, 44, 44]),
            auto: Overlay::default(),
            chosen: Some("тихий"),
            name: "",
            busy: true,
        });
        assert!(!shown.can_apply && !shown.can_save && !shown.can_startup);
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
            !shown.can_save && !shown.can_apply && !shown.can_delete && !shown.can_startup,
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
    fn the_tile_delta_is_the_current_state_minus_the_chosen_profile() {
        let b = block(1, "2–5", vec![1, 2, 3, 4]);
        let limit = [50, 42, 42, 42, 42];
        let profile = [50, 45, 45, 45, 45];
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
            Some(&profile),
            Overlay {
                marks: Some(&detail),
                intended: None,
            },
        );
        // В профиле ×45, владелец поставил ×44, автомодуль ещё −2: сейчас ×42 — разница −3, из них −2 автомодуля.
        assert_eq!((c.delta(), c.auto, c.ban), (-3, -2, Some(44)));
        // Программа входа шлёт только потолок надстройки и потолки владельца по ядрам — тот же ответ.
        let plain = [None, Some(AutoMark::Ceiling(42))];
        let intended = [Some(50), Some(44), Some(44), Some(44), Some(44)];
        let c = ceiling(
            &b,
            Some(&limit),
            Some(&profile),
            Overlay {
                marks: Some(&plain),
                intended: Some(&intended),
            },
        );
        assert_eq!((c.delta(), c.auto, c.ban), (-3, -2, None));
        // Профиль не выбран — разницы нет.
        assert_eq!(ceiling(&b, Some(&limit), None, Overlay::default()).delta(), 0);
    }

    #[test]
    fn the_window_has_no_accept_revert_history_or_depth() {
        // Владелец 15.10: «принять, откатить… вообще непонятно», «История… нахуй мне», глубину «убрать».
        let ui = include_str!("../ui/app.slint");
        for gone in [
            "принять",
            "откатить",
            "История",
            "глубина",
            "Глубина",
            "depth",
            "«было»",
            "было",
        ] {
            assert!(!ui.contains(gone), "app.slint still has «{gone}»");
        }
        for kept in ["применить", "сохранить", "загружать при старте Windows"] {
            assert!(ui.contains(kept), "app.slint lacks «{kept}»");
        }
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

    #[test]
    fn tile_tooltip_lists_maxima_and_means_by_ceiling_since_the_reset() {
        // Дополнение брифа W: «по потолкам с последнего сброса», строка на потолок — ▲, ø, время под нагрузкой; нынешний
        // помечен; как они копятся и сбрасываются — в заголовке части. Поле — по блокам. Программа 0.5.5 его не шлёт —
        // части нет. Ловит: список не того блока, нынешний не помечен, часть без данных.
        use crate::wire::ByCeiling;
        let e = |ceiling, max, mean, secs| ByCeiling {
            ceiling,
            max: Some(max),
            mean: Some(mean),
            secs: Some(secs),
        };
        let cluster = block(1, "2–5", vec![1, 2, 3, 4]);
        let by_block: Vec<Option<Vec<ByCeiling>>> = vec![
            Some(vec![e(50, 90.0, 80.0, 600.0)]),
            Some(vec![e(44, 72.0, 61.5, 300.0), e(45, 75.0, 62.3, 120.0)]),
        ];
        let merged = ceilings_of(&cluster, Some(&by_block));
        assert_eq!(
            merged.iter().map(|x| x.ceiling).collect::<Vec<_>>(),
            [45, 44],
            "список своего блока, по убыванию потолка"
        );
        assert_eq!(ceilings_of(&cluster, None), Vec::<ByCeiling>::new());
        assert_eq!(
            ceilings_of(&cluster, Some(&by_block[..1])),
            Vec::<ByCeiling>::new(),
            "ряд короче — части нет"
        );
        let facts = |by: &[ByCeiling]| {
            tile_tip(&TileFacts {
                block: &cluster,
                cores: &[],
                cur: &[],
                temp: None,
                peak: None,
                mean: None,
                ratio: None,
                mean_ratio: None,
                throttled: false,
                ceiling: Ceiling {
                    now: Some(44),
                    ..Ceiling::default()
                },
                fused: None,
                profile: None,
                by_ceiling: by,
            })
        };
        let tip = facts(&merged);
        assert!(
            tip.contains("по потолкам с последнего сброса (копятся, пока включён автомодуль"),
            "{tip}"
        );
        assert!(
            tip.contains("• ×44 (сейчас): ▲ 72 °C, ø 61.5 °C, под нагрузкой 5 мин 0 с"),
            "{tip}"
        );
        assert!(
            tip.contains("• ×45: ▲ 75 °C, ø 62.3 °C, под нагрузкой 2 мин 0 с"),
            "{tip}"
        );
        assert!(!facts(&[]).contains("по потолкам"), "без поля (0.5.5) — части нет");
    }
}
