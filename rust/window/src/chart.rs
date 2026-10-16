// График статистики блоков: температура от фактического множителя у выбранных блоков (DECISIONS #32 шаг 2, правка по
// словам владельца 15.10). Здесь — что нарисовать и что написать в подсказках, в долях поля графика; как — ui/app.slint
// (Chart), когда — view.rs. Ни окна, ни потоков: функции получают сводку статистики и выбор и возвращают показанное.
//
// Почему так (outputs/thermal-model-2026-10-15.md, KNOWLEDGE «What drives core temperatures in his play»):
// • шкала по умолчанию — температура над остальным кристаллом: 68–96 % температуры любого блока — общий уровень
//   кристалла, а корзина множителя по абсолютной температуре говорит в основном о том, когда она набралась (владелец:
//   «на более высоких… множителях почему-то у нас ядро холоднее»). Абсолютная остаётся переключателем — владелец ею
//   пользовался;
// • вторая линия — 95-й процентиль по времени, а не наибольшая одна строка: наибольшая растёт с числом строк в корзине
//   (у аналитика разрыв с средней — 7,8 °C при 8–20 строках и 14,2 °C при 201–1000) и ловит миллисекундные всплески
//   датчика. Наибольшая со строкой, где она была, — в подсказке столбца;
// • точка тем бледнее, чем меньше времени за ней (`ink`), — корзины не отсекаются (владелец не принимает порогов);
// • оси можно поменять местами (владелец: «возможность переключения осей. Посмотрим, какой вариант будет более
//   оптимальным»): всё ниже считается в долях поля по `Frame`, который один знает, какая ось где.
use std::fmt::Write as _;

use slint::{ModelRc, VecModel};

use crate::panels::{capital, f, grouped, percent, rgb, span_of, whom};
use crate::samples::{self, Bin, P95};
use crate::store::Digest;
use crate::wire::BlockInfo;
use crate::{Column, LegendItem, LineRow, Mark, Run, Series, Tick};

const PEAK: u32 = 0x00ff_8a7a;
const E_TEXT: u32 = 0x006f_d0c8;

// ----- выбор владельца -----

/// Какая температура на графике.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    /// Блок минус средняя остальных блоков в той же строке (`samples::above_die`).
    #[default]
    AboveDie,
    Absolute,
}

/// Какие линии показаны (владелец 15.10: «друг друга там… мусорят… избыточное количество информации»).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Curves {
    Mean,
    P95,
    #[default]
    Both,
}

impl Curves {
    #[must_use]
    pub fn mean(self) -> bool {
        self != Curves::P95
    }

    #[must_use]
    pub fn p95(self) -> bool {
        self != Curves::Mean
    }

    /// Номер для окна и файла окна: 0 — средние, 1 — 95 %, 2 — обе.
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Curves::Mean => 0,
            Curves::P95 => 1,
            Curves::Both => 2,
        }
    }

    #[must_use]
    pub fn from_code(code: i64) -> Option<Curves> {
        match code {
            0 => Some(Curves::Mean),
            1 => Some(Curves::P95),
            2 => Some(Curves::Both),
            _ => None,
        }
    }
}

// ----- цвета блоков -----

/// Цвета линий P-ядер: тёплые (розовый, оранжевый, жёлтый). Владелец 15.10: «12 цветов глаз вполне в состоянии надежно
/// различить, если эти цвета правильно подобрать… две явные группы… 8 цветов разделить И четыре цвета на Е».
/// Подобраны перебором по `OKLab` на подложке панели #1c1f25 (скрипт и числа — в отчёте агента 15.10): любые два из
/// двенадцати различаются на ΔE ≥ 15,3 (`OKLab` ×100) обычным зрением, ≥ 6,7 при протанопии и дейтеранопии (модель
/// Machado 2009) — сравнить можно любые блоки сразу, поэтому проверены все пары, а не соседние; контраст с подложкой
/// ≥ 3:1; от красной линии отсечки (#ff5d5d) каждый — на ΔE ≥ 15. Светлее полосы 0,48–0,67 из навыка dataviz: на
/// тёмной подложке восемь тёплых цветов в ней различаются не больше чем на ΔE 11,6 (замер перебором).
pub const P_COLORS: [u32; 8] = [
    0x00fd_ab74,
    0x00fd_49dd,
    0x00f6_f005,
    0x00c0_2e73,
    0x00ab_a710,
    0x00fb_a2e0,
    0x00b6_6001,
    0x00bc_7dad,
];
/// Цвета линий кластеров E-ядер: холодные (бирюзовый, голубой, синий) — как бирюзовые номера E-ядер на схеме.
pub const E_COLORS: [u32; 4] = [0x001a_c8b6, 0x0043_97e4, 0x007a_f5f8, 0x0034_5bfa];

/// Цвет блока: по его месту среди блоков своего рода в знакомстве (первое P-ядро — первый тёплый, первый кластер —
/// первый холодный). Цвет — у блока, а не у порядка выбора: тот же блок всегда того же цвета, и обводка его плитки
/// тоже. Процессор с большим числом блоков одного рода, чем цветов, повторит цвета по кругу (у 275HX их 8 и 4).
#[must_use]
pub fn color_of(blocks: &[BlockInfo], block: usize) -> u32 {
    let Some(me) = blocks.iter().find(|b| b.index == block) else {
        return P_COLORS[0];
    };
    let rank = blocks
        .iter()
        .filter(|b| b.kind == me.kind && b.index < me.index)
        .count();
    if me.kind == "E" {
        E_COLORS[rank % E_COLORS.len()]
    } else {
        P_COLORS[rank % P_COLORS.len()]
    }
}

// ----- бледность тонких корзин -----

/// Меньше одной строки нового журнала в корзине не бывает: от неё бледность и отсчитывается.
const ONE_ROW: f64 = 0.25;
/// Самая бледная точка: её ещё видно на панели, но она не спорит с насыщенными.
pub const FAINT: f64 = 0.2;

/// Насколько густо рисовать точку (0,2…1), по времени за ней: доля логарифма от одной строки (0,25 с) до самой
/// полной корзины любого блока (`fullest`, с). Логарифм — потому что время в корзинах разнится на пять порядков
/// (секунды по краям оси, часы на ходовых множителях): каждое удесятерение времени добавляет одну и ту же долю краски,
/// и корзина в полчаса рядом с корзиной в двадцать часов не пропадает. Порога нет: меньше времени — бледнее, но видно.
#[must_use]
pub fn ink(secs: f64, fullest: f64) -> f64 {
    if fullest <= ONE_ROW || !secs.is_finite() {
        return 1.0;
    }
    let share = (secs.max(ONE_ROW) / ONE_ROW).ln() / (fullest / ONE_ROW).ln();
    share.clamp(0.0, 1.0).max(FAINT)
}

/// Отрезок между точками рисуется с краской слабейшей из них, округлённой вверх до пятой доли: так у линии блока не
/// больше пяти путей на кривую, а не путь на отрезок.
fn ink_level(ink: f64) -> usize {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ink is within 0..=1, so the product is 0..=5"
    )]
    let level = (ink * 5.0).ceil() as usize;
    level.clamp(1, 5) - 1
}

// ----- поле графика -----

/// Поле графика: ось множителя (`lo`…`hi`, столбец на каждый) и ось температуры (`t0`…`t1`, °C). Обычно множитель —
/// по горизонтали, температура — по вертикали; `swap` — наоборот. Место — доли поля: x слева направо, y снизу вверх.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub lo: i32,
    pub hi: i32,
    pub t0: f64,
    pub t1: f64,
    pub swap: bool,
}

impl Frame {
    /// Сколько столбцов на оси множителя.
    #[must_use]
    pub fn bands(&self) -> i32 {
        self.hi - self.lo + 1
    }

    /// Середина столбца множителя — доля оси множителя.
    #[must_use]
    pub fn along(&self, ratio: i32) -> f64 {
        (f64::from(ratio - self.lo) + 0.5) / f64::from(self.bands())
    }

    /// Температура — доля оси температуры.
    #[must_use]
    pub fn value(&self, temp: f64) -> f64 {
        (temp - self.t0) / (self.t1 - self.t0)
    }

    /// Точка (множитель, температура) на поле: (x, y), доли, y снизу вверх.
    #[must_use]
    pub fn place(&self, ratio: i32, temp: f64) -> (f64, f64) {
        let (m, v) = (self.along(ratio), self.value(temp));
        if self.swap { (v, m) } else { (m, v) }
    }

    /// Столбец множителя на поле: (x, y, ширина, высота) в долях, y снизу вверх.
    #[must_use]
    pub fn band(&self, ratio: i32) -> (f64, f64, f64, f64) {
        let size = 1.0 / f64::from(self.bands());
        let start = f64::from(ratio - self.lo) * size;
        if self.swap {
            (0.0, start, 1.0, size)
        } else {
            (start, 0.0, size, 1.0)
        }
    }
}

// ----- что нужно и что показать -----

/// Ось множителя одна на все блоки (владелец 15.10: «общую числовую ось вообще для всех ядер»): от заводского +1
/// (выше программа не ставит) до самого низкого заводского −14 — ниже ядра под нагрузкой не ходят. Срезы за краями
/// расширяют ось для всех блоков сразу, поэтому от выбора она не зависит.
const AXIS_OVER: i64 = 1;
const AXIS_BELOW: i64 = 14;
/// Абсолютная температура: 40–100 °C, как шкала цвета плиток; срезы за краями расширяют её.
const ABS_LOW: f64 = 40.0;
const ABS_HIGH: f64 = 100.0;
/// Над кристаллом: от 0 (уровень кристалла всегда на оси) хотя бы на 5 °C.
const ABOVE_SPAN: f64 = 5.0;
/// Шаг подписей оси температуры — первый из этих, при котором делений не больше восьми.
const STEPS: [f64; 6] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0];
const MAX_TICKS: f64 = 8.0;
/// Курсор ближе этого к линии блока — она подсвечивается (пиксели поля графика).
const NEAR_PX: f64 = 10.0;

/// Что нужно панели статистики.
#[derive(Debug)]
pub struct StatsInput<'a> {
    pub blocks: &'a [BlockInfo],
    /// Блоки в сравнении (номера) по порядку добавления.
    pub compared: &'a [usize],
    /// Подсвеченный блок: его линия поверх остальных, остальные бледнее; None — все одинаковы.
    pub highlight: Option<usize>,
    /// Заводские потолки по ядрам — края оси множителя.
    pub fused: Option<&'a [i64]>,
    /// Отсечка автомодуля, °C — линией на графике абсолютной температуры.
    pub cutoff: Option<f64>,
    pub digest: Option<&'a Digest>,
    pub scale: Scale,
    pub curves: Curves,
    pub swap: bool,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct StatsShown {
    pub note: String,
    pub legend: Vec<LegendShown>,
    /// Линии в порядке рисования: подсвеченная — последней, поверх.
    pub series: Vec<SeriesShown>,
    /// Каждый множитель оси по порядку: место и подсказка столбца.
    pub columns: Vec<ColumnShown>,
    /// Деления оси температуры: доля оси и подпись.
    pub ticks: Vec<(f32, String)>,
    /// 0 °C над кристаллом — доля оси температуры (линия уровня кристалла); < 0 — нет.
    pub zero: f32,
    /// Отсечка автомодуля — доля оси температуры; < 0 — линии нет.
    pub cutoff: f32,
    pub cutoff_text: String,
    pub value_title: String,
    pub swap: bool,
    pub curves: Curves,
    pub empty: String,
    pub actions: Vec<ActionShown>,
    pub actions_empty: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LegendShown {
    pub block: usize,
    pub name: String,
    pub color: u32,
    pub tip: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SeriesShown {
    pub block: usize,
    pub color: u32,
    pub faded: bool,
    /// Пути ø средней и 95 % в рамке 1000 × 1000 (y вниз) с густотой краски; скрытая кривая — пусто.
    pub mean: Vec<(f32, String)>,
    pub p95: Vec<(f32, String)>,
    pub points: Vec<PointShown>,
}

/// Точка линии: множитель, места ø средней (x, y) и 95 % (qx, qy) — доли поля, y снизу вверх; строк в корзине; ø доля
/// работы (0…1; < 0 — не известна); густота краски (`ink`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PointShown {
    pub ratio: i32,
    pub x: f32,
    pub y: f32,
    pub qx: f32,
    pub qy: f32,
    pub samples: u32,
    pub load: f32,
    pub ink: f32,
}

/// Столбец множителя: множитель, середина — доля оси множителя, подсказка.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ColumnShown {
    pub ratio: i32,
    pub at: f32,
    pub tip: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ActionShown {
    pub time: String,
    pub change: String,
    pub why: String,
    pub lower: bool,
    pub color: u32,
}

impl StatsShown {
    pub fn tick_rows(&self) -> Vec<Tick> {
        self.ticks
            .iter()
            .map(|(f, t)| Tick {
                f: *f,
                text: t.as_str().into(),
            })
            .collect()
    }

    pub fn series_rows(&self) -> Vec<Series> {
        let runs = |paths: &[(f32, String)]| {
            ModelRc::new(VecModel::from(
                paths
                    .iter()
                    .map(|(a, d)| Run {
                        d: d.as_str().into(),
                        a: *a,
                    })
                    .collect::<Vec<_>>(),
            ))
        };
        self.series
            .iter()
            .map(|s| Series {
                mean: runs(&s.mean),
                p95: runs(&s.p95),
                color: rgb(s.color),
                faded: s.faded,
                marks: ModelRc::new(VecModel::from(
                    s.points
                        .iter()
                        .map(|p| Mark {
                            x: p.x,
                            y: p.y,
                            qx: p.qx,
                            qy: p.qy,
                            load: p.load,
                            a: p.ink,
                        })
                        .collect::<Vec<_>>(),
                )), // a few dozen points per changed series
            })
            .collect()
    }

    pub fn column_rows(&self) -> Vec<Column> {
        self.columns
            .iter()
            .map(|c| Column {
                f: c.at,
                v: c.ratio,
                tip: c.tip.as_str().into(),
            })
            .collect()
    }

    pub fn legend_rows(&self) -> Vec<LegendItem> {
        self.legend
            .iter()
            .map(|l| LegendItem {
                name: l.name.as_str().into(),
                color: rgb(l.color),
                tip: l.tip.as_str().into(),
            })
            .collect()
    }

    pub fn action_rows(&self) -> Vec<LineRow> {
        self.actions
            .iter()
            .map(|a| LineRow {
                time: a.time.as_str().into(),
                head: a.change.as_str().into(),
                text: a.why.as_str().into(),
                color: rgb(if a.lower { PEAK } else { E_TEXT }),
                mark: rgb(a.color),
            })
            .collect()
    }
}

// ----- счёт -----

/// Корзина блока на выбранной шкале: множитель, сводка, 95-й процентиль.
#[derive(Clone, Copy, Debug)]
struct Stat {
    ratio: i32,
    bin: Bin,
    p95: f64,
}

/// Корзины блока на шкале `scale`, где были срезы (при любой нагрузке), по возрастанию множителя.
fn stats_of(digest: &Digest, block: &BlockInfo, scale: Scale) -> Vec<Stat> {
    let Some(t) = digest.tally.blocks.get(&block.name) else {
        return Vec::new();
    };
    let (bins, hists) = match scale {
        Scale::Absolute => (&t.bins, &t.hist),
        Scale::AboveDie => (&t.above, &t.above_hist),
    };
    bins.iter()
        .filter(|(_, b)| b.n > 0 && b.secs > 0.0)
        .map(|(&ratio, &bin)| Stat {
            ratio,
            bin,
            // Гистограммы нет только у строк вне ±200 °C (испорчены): тогда — наибольшая.
            p95: hists.get(&ratio).and_then(|h| h.quantile(P95)).unwrap_or(bin.peak.temp),
        })
        .collect()
}

/// Ось множителя: (наименьший, наибольший) — одна на все блоки, от выбора не зависит; None — ни заводских, ни срезов.
pub fn axis(blocks: &[BlockInfo], fused: Option<&[i64]>, digest: Option<&Digest>) -> Option<(i32, i32)> {
    let mut lo: Option<i64> = None;
    let mut hi: Option<i64> = None;
    let mut take = |v: i64| {
        lo = Some(lo.map_or(v, |l| l.min(v)));
        hi = Some(hi.map_or(v, |h| h.max(v)));
    };
    for b in blocks {
        if let Some(f) = fused.and_then(|f| f.get(b.first).copied()).filter(|&f| f > 0) {
            take(f + AXIS_OVER);
            take(f - AXIS_BELOW);
        }
        // Ось — по корзинам абсолютной температуры: в корзинах над кристаллом те же строки или меньше, поэтому ось при
        // смене шкалы не прыгает.
        if let Some(t) = digest.and_then(|d| d.tally.blocks.get(&b.name)) {
            for (&r, _) in t.bins.iter().filter(|(_, bin)| bin.n > 0) {
                take(i64::from(r));
            }
        }
    }
    Some((i32::try_from(lo?).ok()?, i32::try_from(hi?).ok()?))
}

/// Ось температуры: (от, до, шаг подписей), °C — одна на все блоки (по всем блокам, от выбора и от показанных линий не
/// зависит): абсолютная — 40–100 °C и шире, если ø или 95 % какого-нибудь блока за краями; над кристаллом — от
/// наименьшего до наибольшего, 0 всегда на оси.
fn value_range(scale: Scale, every: &[Vec<Stat>]) -> (f64, f64, f64) {
    let values = every
        .iter()
        .flatten()
        .flat_map(|s| [s.bin.mean().unwrap_or(s.p95), s.p95]);
    let (mut lo, mut hi) = match scale {
        Scale::Absolute => (ABS_LOW, ABS_HIGH),
        Scale::AboveDie => (0.0, 0.0),
    };
    for v in values.filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if scale == Scale::AboveDie && hi - lo < ABOVE_SPAN {
        hi = lo + ABOVE_SPAN;
    }
    let step = STEPS
        .iter()
        .copied()
        .find(|s| (hi - lo) / s <= MAX_TICKS)
        .unwrap_or(STEPS[STEPS.len() - 1]);
    let (lo, hi) = ((lo / step).floor() * step, (hi / step).ceil() * step);
    (lo, if hi > lo { hi } else { lo + step }, step)
}

/// Самая полная корзина любого блока на шкале, с (бледность точек — от неё; от выбора не зависит).
fn fullest(every: &[Vec<Stat>]) -> f64 {
    every.iter().flatten().map(|s| s.bin.secs).fold(0.0, f64::max)
}

/// «+12.3», «−3.1», «0.0»: температура над кристаллом со знаком.
fn signed_temp(v: f64, decimals: usize) -> String {
    let text = format!("{:.*}", decimals, v.abs());
    let zero = text.trim_start_matches(['0', '.']).is_empty();
    if zero {
        text
    } else if v > 0.0 {
        format!("+{text}")
    } else {
        format!("−{text}")
    }
}

/// Температура для подписи: абсолютная — как есть, над кристаллом — со знаком.
fn temp_text(scale: Scale, v: f64, decimals: usize) -> String {
    match scale {
        Scale::Absolute => format!("{v:.decimals$}"),
        Scale::AboveDie => signed_temp(v, decimals),
    }
}

/// Подпись блока в легенде: короткое название (номер ядра или кластера, как на схеме), полное — в подсказке.
fn legend(blocks: &[BlockInfo], compared: &[&BlockInfo], digest: Option<&Digest>) -> Vec<LegendShown> {
    compared
        .iter()
        .map(|&b| {
            let counted = digest
                .and_then(|d| d.tally.blocks.get(&b.name))
                .map(|t| {
                    let rows: u32 = t.bins.values().map(|bin| bin.n).sum();
                    let secs: f64 = t.bins.values().map(|bin| bin.secs).sum();
                    format!(": в срезах {} ({} строк)", span_of(secs), grouped(u64::from(rows)))
                })
                .unwrap_or_default();
            LegendShown {
                block: b.index,
                name: b.name.clone(),
                color: color_of(blocks, b.index),
                tip: format!(
                    "{}{counted}\nЩелчок по плитке убирает блок из сравнения, щелчок по другой — добавляет.\n• цвет у блока свой: P-ядра — тёплые, кластеры E-ядер — холодные; обводка плитки — того же цвета",
                    capital(&whom(b))
                ),
            }
        })
        .collect()
}

/// Пути одной кривой: отрезки между соседними множителями (через пустую корзину линии нет — там только точки), каждый
/// с краской слабейшей из двух точек; отрезки одной густоты подряд — одной ломаной.
fn runs(stats: &[Stat], at: &[(f64, f64)], inks: &[f64]) -> Vec<(f32, String)> {
    let mut paths: [String; 5] = Default::default();
    let mut last: Option<(usize, usize)> = None; // (густота, точка, на которой кончился отрезок)
    let xy = |i: usize| (at[i].0 * 1000.0, (1.0 - at[i].1) * 1000.0);
    for i in 1..stats.len() {
        if stats[i].ratio - stats[i - 1].ratio != 1 {
            continue;
        }
        let level = ink_level(inks[i].min(inks[i - 1]));
        let (x, y) = xy(i);
        let path = &mut paths[level];
        if last != Some((level, i - 1)) {
            let (x0, y0) = xy(i - 1);
            let _ = write!(path, "M {x0:.1} {y0:.1} "); // into a String
        }
        let _ = write!(path, "L {x:.1} {y:.1} "); // into a String
        last = Some((level, i));
    }
    paths
        .into_iter()
        .enumerate()
        .filter(|(_, d)| !d.is_empty())
        .map(|(level, d)| (f32::from(u8::try_from(level + 1).unwrap_or(5)) / 5.0, d))
        .collect()
}

/// Пробелы внутри числа, времени и единицы — неразрывные: узкая подсказка переносит строку между ними, а не внутри
/// «4 870» или «−0.1 °C».
const GLUE: char = '\u{a0}';

fn glued(text: &str) -> String {
    text.replace(' ', "\u{a0}")
}

/// Подсказка столбца множителя: что у каждого блока сравнения на нём — сколько времени, ø нагрузка, обороты; ø, 95 % и
/// ▲ наибольшая со строкой, где она была (фактический множитель, нагрузка, время).
fn column_tip(ratio: i32, scale: Scale, compared: &[&BlockInfo], all: &[Vec<Stat>]) -> String {
    let lines: Vec<String> = compared
        .iter()
        .zip(all)
        .filter_map(|(b, stats)| {
            let s = stats.iter().find(|s| s.ratio == ratio)?;
            let bin = &s.bin;
            let load = bin
                .load()
                .map(|l| format!(", нагрузка ø{GLUE}{}", glued(&percent(l))))
                .unwrap_or_default();
            let fan = bin
                .fan()
                .map(|rpm| format!(", вент.{GLUE}≈{GLUE}{}", glued(&grouped(rpm_u64(rpm)))))
                .unwrap_or_default();
            let peak = bin.peak;
            let peak_load = peak
                .busy
                .map(|l| format!(", нагрузка {}", glued(&percent(l))))
                .unwrap_or_default();
            let (p95_digits, peak_digits) = match scale {
                Scale::Absolute => (0, 0),
                Scale::AboveDie => (1, 1),
            };
            Some(format!(
                "{} — {}{load}{fan}\n    ø{GLUE}{}{GLUE}°C · 95{GLUE}% ниже {}{GLUE}°C · ▲{GLUE}{}{GLUE}°C: ×{:.1}{peak_load}, {}",
                capital(&whom(b)),
                glued(&span_of(bin.secs)),
                temp_text(scale, bin.mean().unwrap_or(s.p95), 1),
                temp_text(scale, s.p95, p95_digits),
                temp_text(scale, peak.temp, peak_digits),
                peak.ratio,
                glued(&samples::day_time(peak.at))
            ))
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let what = match scale {
        Scale::Absolute => "",
        Scale::AboveDie => " · над остальным кристаллом",
    };
    format!("×{ratio}{what}\n{}", lines.join("\n"))
}

#[expect(
    clippy::too_many_lines,
    reason = "the chart's scales, lines and columns read top to bottom"
)]
pub fn stats_panel(input: &StatsInput) -> StatsShown {
    let mut shown = StatsShown {
        zero: -1.0,
        cutoff: -1.0,
        swap: input.swap,
        curves: input.curves,
        value_title: match input.scale {
            Scale::AboveDie => "°C над кристаллом".into(),
            Scale::Absolute => "°C".into(),
        },
        ..StatsShown::default()
    };
    let compared: Vec<&BlockInfo> = input
        .compared
        .iter()
        .filter_map(|&b| input.blocks.iter().find(|x| x.index == b))
        .collect();
    shown.legend = legend(input.blocks, &compared, input.digest);
    let Some(digest) = input.digest else {
        shown.empty = "Статистика читается…".into();
        return shown;
    };

    let names: Vec<&str> = compared.iter().map(|b| b.name.as_str()).collect();
    let several = compared.len() > 1;
    shown.actions = samples::actions_for(&digest.auto, &names)
        .into_iter()
        .map(|(which, a)| ActionShown {
            time: a.time,
            change: if several {
                format!("{} {}", names[which], a.change)
            } else {
                a.change
            },
            why: a.why,
            lower: a.lower,
            color: color_of(input.blocks, compared[which].index),
        })
        .collect();
    shown.actions_empty = if compared.is_empty() {
        String::new()
    } else if digest.auto.is_empty() {
        "журнала автомодуля нет — он ещё ничего не делал".into()
    } else if several {
        "эти блоки автомодуль не трогал".into()
    } else {
        "этот блок автомодуль не трогал".into()
    };

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
        "{} срезов, {}{period}",
        grouped(digest.tally.rows),
        span_of(digest.tally.secs)
    );
    if let Some(trouble) = &digest.trouble {
        shown.note.clone_from(trouble);
    }

    if compared.is_empty() {
        shown.empty = format!(
            "Щёлкни по плитке — её статистика появится здесь. Сравнить можно любые блоки, хоть все {}: щелчок добавляет блок, повторный — убирает.",
            input.blocks.len()
        );
        return shown;
    }
    // Корзины всех блоков — для осей и бледности (от выбора они не зависят); выбранных — для линий.
    let every: Vec<Vec<Stat>> = input.blocks.iter().map(|b| stats_of(digest, b, input.scale)).collect();
    let all: Vec<Vec<Stat>> = compared
        .iter()
        .map(|b| {
            input
                .blocks
                .iter()
                .position(|x| x.index == b.index)
                .map(|i| every[i].clone()) // a few dozen bins of the compared blocks, per refresh
                .unwrap_or_default()
        })
        .collect();
    if all.iter().all(Vec::is_empty) {
        let whose = if several {
            "У выбранных блоков".to_string()
        } else {
            format!("У блока {}", compared[0].name)
        };
        shown.empty = if digest.tally.rows == 0 {
            "Срезов пока нет. Их пишет CoreMapTray 4 раза в секунду, пока включён автомодуль и процессор нагружен."
                .into()
        } else if input.scale == Scale::AboveDie {
            format!(
                "{whose} нет ни одного среза, где известны множитель, температура и уровень остального кристалла (срезов всего {}). Абсолютная температура — переключателем «абсолютная».",
                grouped(digest.tally.rows)
            )
        } else {
            format!(
                "{whose} нет ни одного среза с известными множителем и температурой (срезов всего {}).",
                grouped(digest.tally.rows)
            )
        };
        return shown;
    }
    let Some((lo, hi)) = axis(input.blocks, input.fused, Some(digest)) else {
        return shown;
    };
    let (t0, t1, step) = value_range(input.scale, &every);
    let frame = Frame {
        lo,
        hi,
        t0,
        t1,
        swap: input.swap,
    };
    let mut t = t0;
    while t <= t1 + 1e-9 {
        let text = match input.scale {
            Scale::Absolute => format!("{t:.0}°"),
            Scale::AboveDie => format!("{}°", signed_temp(t, 0)),
        };
        shown.ticks.push((f(frame.value(t)), text));
        t += step;
    }
    match input.scale {
        Scale::AboveDie => shown.zero = f(frame.value(0.0)),
        // Отсечка автомодуля — абсолютная температура: на шкале «над кристаллом» её места нет.
        Scale::Absolute => {
            if let Some(c) = input.cutoff
                && (t0..=t1).contains(&c)
            {
                shown.cutoff = f(frame.value(c));
                shown.cutoff_text = format!("{c:.0}°");
            }
        }
    }

    let most = fullest(&every);
    // Подсвеченная линия рисуется последней — поверх остальных (порядок остальных — порядок добавления).
    let mut order: Vec<usize> = (0..compared.len()).collect();
    order.sort_by_key(|&i| input.highlight == Some(compared[i].index));
    for i in order {
        let b = compared[i];
        let stats = &all[i];
        let inks: Vec<f64> = stats.iter().map(|s| ink(s.bin.secs, most)).collect();
        let means: Vec<(f64, f64)> = stats
            .iter()
            .map(|s| frame.place(s.ratio, s.bin.mean().unwrap_or(s.p95)))
            .collect();
        let highs: Vec<(f64, f64)> = stats.iter().map(|s| frame.place(s.ratio, s.p95)).collect();
        shown.series.push(SeriesShown {
            block: b.index,
            color: color_of(input.blocks, b.index),
            faded: input.highlight.is_some_and(|h| h != b.index),
            mean: if input.curves.mean() {
                runs(stats, &means, &inks)
            } else {
                Vec::new()
            },
            p95: if input.curves.p95() {
                runs(stats, &highs, &inks)
            } else {
                Vec::new()
            },
            points: stats
                .iter()
                .zip(means.iter().zip(&highs))
                .zip(&inks)
                .map(|((s, (m, q)), &k)| PointShown {
                    ratio: s.ratio,
                    x: f(m.0),
                    y: f(m.1),
                    qx: f(q.0),
                    qy: f(q.1),
                    samples: s.bin.n,
                    load: s.bin.load().map_or(-1.0, |l| f(l.clamp(0.0, 1.0))),
                    ink: f(k),
                })
                .collect(),
        });
    }

    for ratio in lo..=hi {
        shown.columns.push(ColumnShown {
            ratio,
            at: f(frame.along(ratio)),
            tip: column_tip(ratio, input.scale, &compared, &all),
        });
    }
    shown
}

/// Место точки одной кривой: ø средней или 95 %.
type PlaceOf = fn(&PointShown) -> (f32, f32);

/// Линия какого блока ближе всего к курсору: поле графика w × h пикселей, курсор (x, y) от его левого верхнего угла;
/// None — ни одна не ближе `NEAR_PX`. Считаются показанные кривые блока — ø средняя и 95 %; места точек уже повёрнуты
/// (`Frame::place`), поэтому одинаково при любой раскладке осей.
#[must_use]
pub fn nearest(series: &[SeriesShown], cursor: (f64, f64), field: (f64, f64), curves: Curves) -> Option<usize> {
    let (width, height) = field;
    let mut kinds: Vec<PlaceOf> = Vec::with_capacity(2);
    if curves.mean() {
        kinds.push(|p| (p.x, p.y));
    }
    if curves.p95() {
        kinds.push(|p| (p.qx, p.qy));
    }
    let mut best: Option<(f64, usize)> = None;
    for s in series {
        for &kind in &kinds {
            let at = |p: &PointShown| {
                let (x, y) = kind(p);
                (f64::from(x) * width, (1.0 - f64::from(y)) * height)
            };
            let mut previous: Option<&PointShown> = None;
            for p in &s.points {
                // Соседние множители соединены линией; через пустую корзину линии нет — там только точки.
                let gap = match previous.filter(|q| p.ratio - q.ratio == 1) {
                    Some(q) => segment_distance(at(q), at(p), cursor),
                    None => distance(at(p), cursor),
                };
                if gap <= NEAR_PX && best.is_none_or(|(closest, _)| gap < closest) {
                    best = Some((gap, s.block));
                }
                previous = Some(p);
            }
        }
    }
    best.map(|(_, b)| b)
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Расстояние от точки p до отрезка a–b.
fn segment_distance(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    if length <= f64::EPSILON {
        return distance(a, p);
    }
    let k = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0);
    distance((a.0 + k * dx, a.1 + k * dy), p)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fan speeds are 0..10 000, rounded, non-negative"
)]
fn rpm_u64(rpm: f64) -> u64 {
    rpm.max(0.0).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::{Columns, Scratch, Tally};

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

    fn blocks() -> Vec<BlockInfo> {
        vec![block(0, "0", vec![0]), block(1, "2–5", vec![1, 2, 3, 4])]
    }

    fn digest(rows: &[String]) -> Digest {
        let header =
            "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp,2–5.ceiling,2–5.ratio,2–5.busy,2–5.temp";
        let columns = Columns::parse(header).expect("заголовок");
        let mut tally = Tally::default();
        Scratch::default().count(&columns, &rows.join("\n"), &mut None, &mut tally);
        Digest {
            tally,
            ..Digest::default()
        }
    }

    /// 30 срезов раз в секунду: ядро 0 занято (20 на ×50 при 80 °C, 10 на ×48 при 70 °C); у кластера множитель есть,
    /// только если `cluster` (×44 при 60 °C, нагрузка 0,5), иначе пусто — он спал и множителя не было; температура
    /// кластера известна всегда (уровень кристалла для ядра 0 — она).
    fn thirty(cluster: bool) -> Digest {
        let mut rows = Vec::new();
        for i in 0..30 {
            let (ratio, temp) = if i < 20 { (50, 80) } else { (48, 70) };
            rows.push(format!(
                "2026-10-15 10:00:{i:02},60,{},,53,{ratio},1,{temp},46,{},0.5,60",
                4000 + i,
                if cluster { "44" } else { "" }
            ));
        }
        digest(&rows)
    }

    fn stats(digest: &Digest, compared: &[usize], highlight: Option<usize>, fused: Option<&[i64]>) -> StatsShown {
        stats_with(digest, compared, highlight, fused, Scale::Absolute, false)
    }

    fn stats_with(
        digest: &Digest,
        compared: &[usize],
        highlight: Option<usize>,
        fused: Option<&[i64]>,
        scale: Scale,
        swap: bool,
    ) -> StatsShown {
        stats_panel(&StatsInput {
            blocks: &blocks(),
            compared,
            highlight,
            fused,
            cutoff: None,
            digest: Some(digest),
            scale,
            curves: Curves::Both,
            swap,
        })
    }

    #[test]
    fn chart_points_follow_bins_and_counts() {
        let d = thirty(false);
        let s = stats(&d, &[0], None, None);
        assert_eq!(s.empty, "");
        assert_eq!(s.series.len(), 1);
        let line = &s.series[0];
        let ratios: Vec<i32> = line.points.iter().map(|p| p.ratio).collect();
        assert_eq!(ratios, [48, 50], "только корзины с данными, по возрастанию множителя");
        assert_eq!(line.points.iter().map(|p| p.samples).collect::<Vec<_>>(), [10, 20]);
        assert!(
            line.points[0].x < line.points[1].x && line.points[0].y < line.points[1].y,
            "правее и выше — горячее"
        );
        // ×49 пуст: линия средней рвётся (два «M»), а не проводится через пустую корзину; отрезков нет вовсе.
        assert!(line.mean.iter().all(|(_, d)| !d.contains('L')), "{:?}", line.mean);
        // Первая строка — шаг прежнего журнала (2 с), дальше — по секунде между строками: 2 + 29 = 31 с.
        assert_eq!(s.note, "30 срезов, 31 с · 15.10 10:00–10:00");
        let axis: Vec<i32> = s.columns.iter().map(|c| c.ratio).collect();
        assert_eq!(axis, [48, 49, 50]);
        assert_eq!(s.columns[1].tip, "");
        // Подсказка столбца: время, ø нагрузка; ø, 95 % и ▲ — со множителем, нагрузкой и временем строки (из равных
        // 80 °C — поздняя, 10:00:19).
        let tip = s.columns[2].tip.replace('\u{a0}', " ");
        assert!(
            tip.contains("Ядро 0 — 21 с, нагрузка ø 100 %")
                && tip.contains("ø 80.0 °C · 95 % ниже 80 °C · ▲ 80 °C: ×50.0, нагрузка 100 %, 15.10 10:00:19"),
            "{tip}"
        );
        assert_eq!(s.ticks.first().map(|t| t.1.as_str()), Some("40°"));
        assert_eq!(s.ticks.last().map(|t| t.1.as_str()), Some("100°"));
        let e = stats(&d, &[1], None, None);
        assert!(
            e.series.is_empty() && e.empty.contains("нет ни одного среза"),
            "{}",
            e.empty
        );
        let none = stats(&d, &[], None, None);
        assert!(none.series.is_empty() && none.empty.contains("Щёлкни по плитке"));
    }

    #[test]
    fn above_the_die_is_the_default_and_the_axis_follows_it() {
        // Ядро 0 на 80 и 70 °C, кластер — всегда 60 °C: над кристаллом ядро на +20 и +10.
        let d = thirty(false);
        let s = stats_with(&d, &[0], None, None, Scale::AboveDie, false);
        assert_eq!(Scale::default(), Scale::AboveDie);
        assert_eq!(s.value_title, "°C над кристаллом");
        let tip = s.columns[2].tip.replace('\u{a0}', " ");
        assert!(
            tip.contains("над остальным кристаллом") && tip.contains("ø +20.0 °C"),
            "{tip}"
        );
        // Ось: 0…+20 с шагом 5, ноль — на оси, подписи со знаком.
        let ticks: Vec<&str> = s.ticks.iter().map(|t| t.1.as_str()).collect();
        assert_eq!(ticks, ["0°", "+5°", "+10°", "+15°", "+20°"]);
        assert!((s.zero - 0.0).abs() < 1e-6);
        // Отсечка автомодуля (абсолютная) на этой шкале не рисуется.
        let with_cutoff = stats_panel(&StatsInput {
            blocks: &blocks(),
            compared: &[0],
            highlight: None,
            fused: None,
            cutoff: Some(85.0),
            digest: Some(&d),
            scale: Scale::AboveDie,
            curves: Curves::Both,
            swap: false,
        });
        assert!(with_cutoff.cutoff < 0.0);
    }

    #[test]
    fn a_point_shows_the_mean_load_of_every_row_at_its_multiplier() {
        let rows: Vec<String> = [0.1, 0.1, 0.1, 0.9]
            .iter()
            .enumerate()
            .map(|(i, busy)| format!("2026-10-15 10:00:{:02},60,,,53,50,{busy},70,46,,,", i * 2))
            .collect();
        let d = digest(&rows);
        let s = stats(&d, &[0], None, None);
        let points = &s.series[0].points;
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].samples, 4);
        assert!((points[0].load - 0.3).abs() < 1e-6, "{}", points[0].load);
        assert!(
            s.columns
                .iter()
                .any(|c| c.tip.replace('\u{a0}', " ").contains("нагрузка ø 30 %"))
        );
    }

    #[test]
    fn the_multiplier_axis_is_one_for_all_blocks_whatever_is_compared() {
        let d = thirty(true);
        let fused = [53, 46, 46, 46, 46];
        assert_eq!(axis(&blocks(), Some(&fused), Some(&d)), Some((32, 54)));
        let p = stats(&d, &[0], None, Some(&fused));
        let e = stats(&d, &[1], None, Some(&fused));
        let both = stats(&d, &[0, 1], None, Some(&fused));
        let columns = |s: &StatsShown| s.columns.iter().map(|c| (c.ratio, c.at)).collect::<Vec<_>>();
        assert_eq!(columns(&p), columns(&e), "P и E — на одной оси");
        assert_eq!(columns(&p), columns(&both));
        assert_eq!(p.ticks, e.ticks, "и температура — на одной");
        let low = digest(&["2026-10-15 10:00:00,60,,,53,30,1,70,46,44,0,60".to_string()]);
        assert_eq!(axis(&blocks(), Some(&fused), Some(&low)), Some((30, 54)));
    }

    #[test]
    fn colours_belong_to_blocks_and_the_highlighted_one_is_drawn_last() {
        let d = thirty(true);
        let s = stats(&d, &[1, 0], None, None);
        assert_eq!(
            s.series.iter().map(|l| (l.block, l.color, l.faded)).collect::<Vec<_>>(),
            [(1, E_COLORS[0], false), (0, P_COLORS[0], false)],
            "цвет — у блока по его роду, порядок — порядок добавления"
        );
        assert_eq!(
            s.legend.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            ["2–5", "0"]
        );
        let lit = stats(&d, &[1, 0], Some(1), None);
        assert_eq!(
            lit.series.iter().map(|l| (l.block, l.faded)).collect::<Vec<_>>(),
            [(0, true), (1, false)],
            "подсвеченный — последним (поверх), остальные бледнее"
        );
        let at44 = lit.columns.iter().find(|c| c.ratio == 44).map(|c| c.tip.clone());
        assert!(at44.is_some_and(|t| t.contains("Кластер 2–5") && !t.contains("Ядро 0")));
    }

    #[test]
    fn curve_switch_draws_only_the_chosen_lines() {
        // Ядро 0 на ×48 и ×49 подряд — у обеих кривых есть отрезок.
        let rows: Vec<String> = (0..20)
            .map(|i| {
                format!(
                    "2026-10-15 10:00:{i:02},60,,,53,{},1,{},46,44,0.5,60",
                    if i < 10 { 48 } else { 49 },
                    70 + i % 4
                )
            })
            .collect();
        let d = digest(&rows);
        let with = |curves| {
            stats_panel(&StatsInput {
                blocks: &blocks(),
                compared: &[0],
                highlight: None,
                fused: None,
                cutoff: None,
                digest: Some(&d),
                scale: Scale::Absolute,
                curves,
                swap: false,
            })
        };
        let both = with(Curves::Both);
        assert!(!both.series[0].mean.is_empty() && !both.series[0].p95.is_empty());
        let mean = with(Curves::Mean);
        assert!(!mean.series[0].mean.is_empty() && mean.series[0].p95.is_empty());
        let p95 = with(Curves::P95);
        assert!(p95.series[0].mean.is_empty() && !p95.series[0].p95.is_empty());
        assert_eq!(both.ticks, mean.ticks, "ось температуры от выбора линий не зависит");
        for c in [Curves::Mean, Curves::P95, Curves::Both] {
            assert_eq!(Curves::from_code(i64::from(c.code())), Some(c));
        }
    }

    #[test]
    fn nearest_line_is_found_within_ten_pixels_in_both_layouts() {
        let sampled = thirty(true);
        for swap in [false, true] {
            let shown = stats_with(&sampled, &[0, 1], None, None, Scale::Absolute, swap);
            let field = (400.0, 300.0);
            let point = |block: usize, ratio: i32| {
                let found = shown
                    .series
                    .iter()
                    .find(|l| l.block == block)
                    .and_then(|l| l.points.iter().find(|p| p.ratio == ratio))
                    .expect("точка");
                (f64::from(found.x) * field.0, (1.0 - f64::from(found.y)) * field.1)
            };
            let cluster = point(1, 44);
            assert_eq!(
                nearest(&shown.series, (cluster.0 + 3.0, cluster.1 + 6.0), field, Curves::Both),
                Some(1),
                "swap {swap}"
            );
            let core = point(0, 50);
            assert_eq!(
                nearest(&shown.series, (core.0 - 3.0, core.1 - 4.0), field, Curves::Both),
                Some(0),
                "swap {swap}"
            );
            assert_eq!(
                nearest(&shown.series, (core.0 + 60.0, core.1 - 60.0), field, Curves::Both),
                None,
                "далеко от линий — никого (swap {swap})"
            );
        }
    }

    #[test]
    fn the_cutoff_line_sits_at_its_temperature() {
        let d = thirty(false);
        let s = stats_panel(&StatsInput {
            blocks: &blocks(),
            compared: &[0],
            highlight: None,
            fused: None,
            cutoff: Some(85.0),
            digest: Some(&d),
            scale: Scale::Absolute,
            curves: Curves::Both,
            swap: false,
        });
        assert!(
            (s.cutoff - 0.75).abs() < 1e-6,
            "85 °C на шкале 40–100 — три четверти: {}",
            s.cutoff
        );
        assert_eq!(s.cutoff_text, "85°");
    }

    #[test]
    fn thin_bins_fade_by_the_log_of_their_time() {
        // Одна строка — самая бледная, самая полная корзина — полная краска, каждое удесятерение — равный шаг.
        assert!((ink(0.25, 2500.0) - FAINT).abs() < 1e-12);
        assert!((ink(2500.0, 2500.0) - 1.0).abs() < 1e-12);
        let (a, b, c) = (ink(2.5, 2500.0), ink(25.0, 2500.0), ink(250.0, 2500.0));
        assert!(((b - a) - (c - b)).abs() < 1e-9 && a < b && b < c, "{a} {b} {c}");
        assert!(ink(0.01, 2500.0) >= FAINT, "меньше строки не бывает, но и тогда видно");
        assert!(
            (ink(10.0, 0.25) - 1.0).abs() < 1e-12,
            "все корзины в одну строку — без бледности"
        );
        // Путь отрезка — с краской слабейшего конца, уровни — пятые доли.
        assert_eq!(ink_level(1.0), 4);
        assert_eq!(ink_level(0.2), 0);
        assert_eq!(ink_level(0.41), 2);
    }

    #[test]
    fn frame_swaps_axes_and_bands() {
        let normal = Frame {
            lo: 40,
            hi: 49,
            t0: 40.0,
            t1: 100.0,
            swap: false,
        };
        let swapped = Frame { swap: true, ..normal };
        let (x, y) = normal.place(45, 70.0);
        assert!((x - 0.55).abs() < 1e-12 && (y - 0.5).abs() < 1e-12);
        assert_eq!(swapped.place(45, 70.0), (y, x));
        assert_eq!(normal.band(45), (0.5, 0.0, 0.1, 1.0));
        assert_eq!(swapped.band(45), (0.0, 0.5, 1.0, 0.1));
    }

    #[test]
    fn palette_families_split_and_every_block_has_its_own_colour() {
        let mut all = vec![block(0, "0", vec![0]), block(1, "1", vec![1])];
        all.push(block(2, "2–5", vec![2, 3, 4, 5]));
        all.push(block(3, "6–9", vec![6, 7, 8, 9]));
        for (k, name) in ["10", "11", "12", "13"].iter().enumerate() {
            all.push(block(4 + k, name, vec![10 + k]));
        }
        all.push(block(8, "14–17", vec![14, 15, 16, 17]));
        all.push(block(9, "18–21", vec![18, 19, 20, 21]));
        all.push(block(10, "22", vec![22]));
        all.push(block(11, "23", vec![23]));
        let colours: Vec<u32> = all.iter().map(|b| color_of(&all, b.index)).collect();
        let mut unique = colours.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 12, "двенадцать блоков — двенадцать цветов");
        for (b, c) in all.iter().zip(&colours) {
            if b.kind == "E" {
                assert!(E_COLORS.contains(c));
            } else {
                assert!(P_COLORS.contains(c));
            }
        }
    }

    #[test]
    fn signed_temperatures_read_with_a_sign() {
        assert_eq!(signed_temp(12.34, 1), "+12.3");
        assert_eq!(signed_temp(-3.06, 1), "−3.1");
        assert_eq!(signed_temp(0.01, 1), "0.0");
        assert_eq!(signed_temp(-0.01, 0), "0");
    }
}
