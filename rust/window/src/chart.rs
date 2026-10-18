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
//   оптимальным»): всё ниже считается в долях поля по `Frame`, который один знает, какая ось где;
// • кривая модели нагрева (model.rs, DECISIONS #39) — температура от фактического множителя при своей нагрузке 100 %,
//   соседях и кристалле на их медианах (`BlockModel::curve`). Сплошная над наблюдёнными множителями, пунктир за ними;
//   свой переключатель «модель», может идти вместе с ø и 95 %. Условия и ошибка — в подсказке блока в легенде;
// • вид по умолчанию — «эффективность» (бриф 16.10, outputs/efficiency-brief-2026-10-16.md): у каждого блока по точке
//   на целый множитель — производительность на градус = множитель / свой нагрев при полной нагрузке, всё прочее вынесено
//   регрессией; усы — разброс по четырём подгонкам без куска; все блоки на одном графике для сравнения. Ватт нет.
use std::fmt::Write as _;

use slint::{ModelRc, VecModel};

use crate::events::{Event, Kind};
use crate::model::{BlockModel, Efficiency, Model};
use crate::panels::{capital, f, grouped, percent, rgb, span_of, whom};
use crate::plot::{Range, Viewport};
use crate::samples::{self, Bin, P95};
use crate::store::Digest;
use crate::wire::BlockInfo;
use crate::{Column, Cut, LegendItem, Mark, Run, Series, Tick};

// ----- выбор владельца -----

/// Вид графика (бриф 16.10, «эффективность»): один из трёх.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    /// Эффективность: производительность на градус = множитель / свой нагрев блока при полной нагрузке на этом целом
    /// множителе, всё прочее (кристалл, соседи, ранг, история) вынесено регрессией (`BlockModel::efficiency`). Владелец
    /// 16.10: «конечная метрика у нас производительность… частоте на загрузку».
    #[default]
    Efficiency,
    /// Блок минус средняя остальных блоков в той же строке (`samples::above_die`).
    AboveDie,
    Absolute,
    ConditionalEfficiency,
    ConditionalHeat,
}

impl Scale {
    /// Номер вида для окна (app.slint: 0 — эффективность, 1 — над кристаллом, 2 — абсолютная).
    #[must_use]
    pub fn index(self) -> i32 {
        match self {
            Scale::Efficiency => 0,
            Scale::AboveDie => 1,
            Scale::Absolute => 2,
            Scale::ConditionalEfficiency => 3,
            Scale::ConditionalHeat => 4,
        }
    }

    #[must_use]
    pub fn of(index: i32) -> Scale {
        match index {
            1 => Scale::AboveDie,
            2 => Scale::Absolute,
            3 => Scale::ConditionalEfficiency,
            4 => Scale::ConditionalHeat,
            _ => Scale::Efficiency,
        }
    }
}

/// Какие линии показаны (владелец 15.10: «друг друга там… мусорят… избыточное количество информации»). Два
/// независимых выключателя — «ø» и «95 %» (владелец 16.10: «зачем эта кнопка оба… сделать два независимых
/// выключателя»); оба выключены — пустой график с осями (`Neither`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Curves {
    Mean,
    P95,
    #[default]
    Both,
    Neither,
}

impl Curves {
    /// Линии по двум выключателям.
    #[must_use]
    pub fn of(mean: bool, p95: bool) -> Curves {
        match (mean, p95) {
            (true, true) => Curves::Both,
            (true, false) => Curves::Mean,
            (false, true) => Curves::P95,
            (false, false) => Curves::Neither,
        }
    }

    #[must_use]
    pub fn mean(self) -> bool {
        matches!(self, Curves::Mean | Curves::Both)
    }

    #[must_use]
    pub fn p95(self) -> bool {
        matches!(self, Curves::P95 | Curves::Both)
    }

    /// Щелчок по «ø»: средние показаны или скрыты, 95 % — как были.
    #[must_use]
    pub fn toggled_mean(self) -> Curves {
        Curves::of(!self.mean(), self.p95())
    }

    /// Щелчок по «95 %»: 95 % показаны или скрыты, средние — как были.
    #[must_use]
    pub fn toggled_p95(self) -> Curves {
        Curves::of(self.mean(), !self.p95())
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
    pub argument_range: Option<Range>,
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
        self.along_at(f64::from(ratio))
    }

    /// Дробный множитель — доля оси множителя (целый — середина своего столбца).
    #[must_use]
    pub fn along_at(&self, ratio: f64) -> f64 {
        self.argument_range.map_or_else(
            || (ratio - f64::from(self.lo) + 0.5) / f64::from(self.bands()),
            |range| range.normal(ratio).unwrap_or(f64::NAN),
        )
    }

    /// Температура — доля оси температуры.
    #[must_use]
    pub fn value(&self, temp: f64) -> f64 {
        (temp - self.t0) / (self.t1 - self.t0)
    }

    /// Точка (множитель, температура) на поле: (x, y), доли, y снизу вверх.
    #[must_use]
    pub fn place(&self, ratio: i32, temp: f64) -> (f64, f64) {
        self.turn(self.along(ratio), self.value(temp))
    }

    /// Доли (вдоль множителя, вдоль температуры) → (x, y) поля.
    #[must_use]
    pub fn turn(&self, along: f64, value: f64) -> (f64, f64) {
        if self.swap { (value, along) } else { (along, value) }
    }

    /// Столбец множителя на поле: (x, y, ширина, высота) в долях, y снизу вверх.
    #[must_use]
    pub fn band(&self, ratio: i32) -> (f64, f64, f64, f64) {
        let (size, start) = self.argument_range.map_or_else(
            || {
                let size = 1.0 / f64::from(self.bands());
                (size, f64::from(ratio - self.lo) * size)
            },
            |_| {
                (
                    self.along_at(f64::from(ratio) + 0.5) - self.along_at(f64::from(ratio) - 0.5),
                    self.along_at(f64::from(ratio) - 0.5),
                )
            },
        );
        if self.swap {
            (0.0, start, 1.0, size)
        } else {
            (start, 0.0, size, 1.0)
        }
    }
}

// ----- что нужно и что показать -----

/// Старые заводские края сохранены только в тесте, сравнивающем прежний вид с автодиапазоном данных.
#[cfg(test)]
const AXIS_OVER: i64 = 1;
#[cfg(test)]
const AXIS_BELOW: i64 = 14;
/// Целевое число делений оси значения.
const MAX_TICKS: f64 = 8.0;
/// Курсор ближе этого к линии блока — она подсвечивается (пиксели поля графика).
const NEAR_PX: f64 = 10.0;
/// Шаг кривой модели по множителю: десятая ступени — излом кривой напряжения между целыми множителями виден.
const MODEL_STEP: f64 = 0.1;
/// Пунктир кривой модели за наблюдёнными множителями: штрих и промежуток в рамке 1000 × 1000.
const DASH: f64 = 18.0;
const DASH_GAP: f64 = 12.0;

/// Что нужно панели статистики.
#[derive(Debug, Default)]
pub struct StatsInput<'a> {
    pub blocks: &'a [BlockInfo],
    /// Блоки в сравнении (номера) по порядку добавления.
    pub compared: &'a [usize],
    /// Подсвеченный блок: его линия поверх остальных, остальные бледнее; None — все одинаковы.
    pub highlight: Option<usize>,
    /// Отсечка автомодуля, °C — линией на графике абсолютной температуры.
    pub cutoff: Option<f64>,
    pub digest: Option<&'a Digest>,
    pub scale: Scale,
    pub curves: Curves,
    /// Линия наибольших (переключатель «макс»).
    pub max: bool,
    /// Кривые модели нагрева (переключатель «модель»).
    pub model: bool,
    pub swap: bool,
    /// Аварийная отсечка автомодуля, °C — второй линией на «абсолютной».
    pub emergency: Option<f64>,
    pub argument_range: Option<Range>,
    pub value_range: Option<Range>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct StatsShown {
    pub argument_range: Option<Range>,
    pub value_range: Option<Range>,
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
    /// Аварийная отсечка — так же.
    pub emergency: f32,
    pub emergency_text: String,
    pub max: bool,
    pub value_title: String,
    pub swap: bool,
    pub curves: Curves,
    pub model: bool,
    pub empty: String,
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
    /// Пути ø средней, 95 % и наибольших в рамке 1000 × 1000 (y вниз) с густотой краски; скрытая кривая — пусто.
    pub mean: Vec<(f32, String)>,
    pub p95: Vec<(f32, String)>,
    pub max: Vec<(f32, String)>,
    /// Отметки отсечек автомодуля на линии блока.
    pub cuts: Vec<CutShown>,
    /// Кривая модели в той же рамке: сплошная (наблюдённые множители, 5–95 % по времени) и пунктир (за ними); нет кривой —
    /// пусто.
    pub model: String,
    pub model_dash: String,
    /// Та же кривая модели ломаными в долях поля (x, y снизу вверх, оси уже повёрнуты) — для подсветки под курсором
    /// (`nearest`); скрыта или нет кривой — пусто.
    pub model_line: Vec<Vec<(f32, f32)>>,
    /// Замкнутая область чувствительности в рамке 1000 × 1000 (y вниз), отдельно от основной линии.
    pub sensitivity: String,
    pub points: Vec<PointShown>,
}

/// Точка линии: множитель, места ø средней (x, y), 95 % (qx, qy) и наибольшей (mx, my) — доли поля, y снизу вверх;
/// строк в корзине; ø доля работы (0…1; < 0 — не известна); густота краски (`ink`). В виде «эффективность» (x, y) —
/// эффективность корзины, остальных мест нет.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PointShown {
    pub ratio: i32,
    pub x: f32,
    pub y: f32,
    pub qx: f32,
    pub qy: f32,
    pub mx: f32,
    pub my: f32,
    pub samples: u32,
    pub load: f32,
    pub ink: f32,
}

/// Отметка отсечки на линии блока: место (доли поля, y снизу вверх) и вид: 1 — аварийная, 2 — по цели.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CutShown {
    pub x: f32,
    pub y: f32,
    pub kind: i32,
}

/// Столбец множителя: множитель, середина — доля оси множителя, подсказка.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ColumnShown {
    pub ratio: i32,
    pub at: f32,
    pub tip: String,
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
                max: runs(&s.max),
                model: s.model.as_str().into(),
                model_dash: s.model_dash.as_str().into(),
                sensitivity: s.sensitivity.as_str().into(),
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
                            mx: p.mx,
                            my: p.my,
                            load: p.load,
                            a: p.ink,
                        })
                        .collect::<Vec<_>>(),
                )), // a few dozen points per changed series
                cuts: ModelRc::new(VecModel::from(
                    s.cuts
                        .iter()
                        .map(|c| Cut {
                            x: c.x,
                            y: c.y,
                            kind: c.kind,
                        })
                        .collect::<Vec<_>>(),
                )), // a few cut marks per changed series
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
}

// ----- счёт -----

/// Корзина блока на выбранной шкале: множитель, сводка, 95-й процентиль.
#[derive(Clone, Copy, Debug)]
struct Stat {
    ratio: i32,
    bin: Bin,
    p95: f64,
}

/// Корзины блока на шкале `scale`, где были срезы (при любой нагрузке), по возрастанию множителя; в виде
/// «эффективность» сырых корзин нет.
fn stats_of(digest: &Digest, block: &BlockInfo, scale: Scale) -> Vec<Stat> {
    let Some(t) = digest.tally.blocks.get(&block.name) else {
        return Vec::new();
    };
    let (bins, hists) = match scale {
        Scale::Absolute => (&t.bins, &t.hist),
        Scale::AboveDie => (&t.above, &t.above_hist),
        Scale::Efficiency | Scale::ConditionalEfficiency | Scale::ConditionalHeat => return Vec::new(),
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
#[cfg(test)]
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

/// Шаг подписей ограничивает число делений и при ручных очень широких границах.
fn tick_step(range: Range) -> f64 {
    let raw = (range.max - range.min) / MAX_TICKS;
    let power = 10.0_f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|v| v * power)
        .find(|v| *v >= raw)
        .unwrap_or(raw)
}

fn resolve_frame(input: &StatsInput, ratios: &[f64], values: &[f64], shown: &mut StatsShown) -> Option<Frame> {
    let argument = input
        .argument_range
        .and_then(|r| Range::new(r.min, r.max))
        .or_else(|| Range::fit(ratios.iter().copied()))?;
    let value = input
        .value_range
        .and_then(|r| Range::new(r.min, r.max))
        .or_else(|| Range::fit(values.iter().copied()))?;
    // Столбцы ограничены реальными множителями, а не произвольными ручными границами.
    let low = ratios
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f64::INFINITY, f64::min);
    let high = ratios
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    if !low.is_finite() || !high.is_finite() {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "CPU ratios bounded by data, not manual display bounds"
    )]
    let (lo, hi) = (low.floor() as i32, high.ceil() as i32);
    shown.argument_range = Some(argument);
    shown.value_range = Some(value);
    Some(Frame {
        lo,
        hi,
        t0: value.min,
        t1: value.max,
        swap: input.swap,
        argument_range: Some(argument),
    })
}

fn fill_ticks(frame: &Frame, scale: Scale, shown: &mut StatsShown) {
    let step = tick_step(Range {
        min: frame.t0,
        max: frame.t1,
    });
    let first = (frame.t0 / step).ceil() * step;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "decimal count is clamped to 0..=12"
    )]
    let decimals = (-step.log10().floor()).clamp(0.0, 12.0) as usize;
    for k in 0..=10 {
        let tick = first + f64::from(k) * step;
        if tick > frame.t1 || !tick.is_finite() {
            break;
        }
        let text = temp_text(scale, tick, decimals);
        let unit = if matches!(scale, Scale::Absolute | Scale::AboveDie | Scale::ConditionalHeat) {
            "°"
        } else {
            ""
        };
        shown.ticks.push((f(frame.value(tick)), format!("{text}{unit}")));
    }
}

fn inside((x, y): (f64, f64)) -> bool {
    x.is_finite() && y.is_finite() && (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)
}

fn unit_viewport() -> Viewport {
    Viewport {
        argument: Range { min: 0.0, max: 1.0 },
        value: Range { min: 0.0, max: 1.0 },
        swap: false,
    }
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

/// Температура для подписи: абсолютная и эффективность — как есть, над кристаллом — со знаком.
fn temp_text(scale: Scale, v: f64, decimals: usize) -> String {
    match scale {
        Scale::Absolute | Scale::Efficiency | Scale::ConditionalEfficiency | Scale::ConditionalHeat => {
            format!("{v:.decimals$}")
        }
        Scale::AboveDie => signed_temp(v, decimals),
    }
}

/// Подпись блока в легенде: короткое название (номер ядра или кластера, как на схеме), полное — в подсказке; там же
/// модель нагрева блока: условия, ошибка, данные, что влияет.
fn legend(blocks: &[BlockInfo], compared: &[&BlockInfo], digest: Option<&Digest>, scale: Scale) -> Vec<LegendShown> {
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
                    "{}{counted}\nЩелчок по плитке убирает блок из сравнения, щелчок по другой — добавляет.\n• цвет у блока свой: P-ядра — тёплые, кластеры E-ядер — холодные; обводка плитки — того же цвета{}{}",
                    capital(&whom(b)),
                    digest
                        .and_then(|d| d.model.as_deref())
                        .map(|m| model_tip(m, b, scale))
                        .unwrap_or_default(),
                    digest
                        .and_then(|d| d.model.as_deref())
                        .and_then(|m| model_of(m, b))
                        .map(influence_tip)
                        .unwrap_or_default()
                ),
            }
        })
        .collect()
}

// ----- кривая модели -----

/// Модель блока по названию.
fn model_of<'a>(model: &'a Model, block: &BlockInfo) -> Option<&'a BlockModel> {
    model.blocks.iter().find(|m| m.name == block.name)
}

/// Кривая модели на шкале графика: абсолютная — как есть, над кристаллом — минус опорный уровень кристалла; в виде
/// «эффективность» кривой нет — там точки `BlockModel::efficiency`.
fn model_value(m: &BlockModel, scale: Scale, ratio: f64) -> Option<f64> {
    let t = m.curve(ratio)?;
    Some(match scale {
        Scale::AboveDie => t - m.reference.as_ref()?.die,
        Scale::Absolute => t,
        Scale::Efficiency | Scale::ConditionalEfficiency | Scale::ConditionalHeat => return None,
    })
}

/// Множители от `from` до `to` с шагом `MODEL_STEP`, оба края включены.
fn ratios(from: f64, to: f64) -> impl Iterator<Item = f64> {
    let steps = ((to - from) / MODEL_STEP).ceil().max(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the axis spans a few dozen multipliers: a few hundred steps"
    )]
    let steps = steps.min(10_000.0) as u32;
    (0..=steps).map(move |k| (from + f64::from(k) * MODEL_STEP).min(to))
}

/// Значения сплошной части кривой модели (наблюдённые множители) — для оси температуры; кривой нет — пусто.
fn model_solid(m: &BlockModel, scale: Scale) -> Vec<f64> {
    let Some(r) = m.reference.as_ref().filter(|_| m.drawn()) else {
        return Vec::new();
    };
    ratios(r.lo, r.hi).filter_map(|x| model_value(m, scale, x)).collect()
}

/// Ломаная (доли поля: вдоль множителя, вдоль температуры), обрезанная краями оси температуры: куски внутри поля.
fn clipped(points: &[(f64, f64)]) -> Vec<Vec<(f64, f64)>> {
    let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut previous = None;
    for pair in points.windows(2) {
        if let Some((a, b)) = unit_viewport().segment(pair[0], pair[1]) {
            if previous != Some(a) {
                out.push(vec![a]);
            }
            if let Some(run) = out.last_mut() {
                run.push(b);
            }
            previous = Some(b);
        } else {
            previous = None;
        }
    }
    out
}

/// Точка доли поля → рамка пути 1000 × 1000 (y вниз).
fn frame_point(frame: &Frame, (along, value): (f64, f64)) -> (f64, f64) {
    let (x, y) = frame.turn(along, value);
    (x * 1000.0, (1.0 - y) * 1000.0)
}

/// Сплошная ломаная в путь.
fn solid_path(run: &[(f64, f64)], frame: &Frame, out: &mut String) {
    for (k, &p) in run.iter().enumerate() {
        let (x, y) = frame_point(frame, p);
        let _ = write!(out, "{} {x:.1} {y:.1} ", if k == 0 { "M" } else { "L" }); // into a String
    }
}

/// Пунктир по ломаной: штрихи `DASH` через промежутки `DASH_GAP` по длине в рамке 1000 × 1000.
fn dashed_path(run: &[(f64, f64)], frame: &Frame, out: &mut String) {
    let points: Vec<(f64, f64)> = run.iter().map(|&p| frame_point(frame, p)).collect();
    let (mut on, mut left, mut drawing) = (true, DASH, false);
    for w in points.windows(2) {
        let (mut a, b) = (w[0], w[1]);
        let mut length = (b.0 - a.0).hypot(b.1 - a.1);
        while length > 1e-9 {
            let step = left.min(length);
            let k = step / length;
            let c = (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
            if on {
                if !drawing {
                    let _ = write!(out, "M {:.1} {:.1} ", a.0, a.1); // into a String
                    drawing = true;
                }
                let _ = write!(out, "L {:.1} {:.1} ", c.0, c.1); // into a String
            }
            length -= step;
            left -= step;
            a = c;
            if left <= 1e-9 {
                on = !on;
                left = if on { DASH } else { DASH_GAP };
                drawing = false;
            }
        }
    }
}

/// Пути кривой модели блока: сплошная над наблюдёнными множителями, пунктир от края оси до них и от них до края.
fn model_paths(m: &BlockModel, scale: Scale, frame: &Frame) -> (String, String, Vec<Vec<(f32, f32)>>) {
    let Some(r) = m.reference.as_ref().filter(|_| m.drawn()) else {
        return (String::new(), String::new(), Vec::new());
    };
    let (from, to) = (f64::from(frame.lo) - 0.5, f64::from(frame.hi) + 0.5);
    let piece = |a: f64, b: f64| -> Vec<(f64, f64)> {
        if b <= a {
            return Vec::new();
        }
        ratios(a, b)
            .filter_map(|x| Some((frame.along_at(x), frame.value(model_value(m, scale, x)?))))
            .collect()
    };
    let (lo, hi) = (r.lo.clamp(from, to), r.hi.clamp(from, to));
    // Те же куски — ломаными в долях поля для подсветки под курсором (`nearest`).
    let turned = |run: &[(f64, f64)]| -> Vec<(f32, f32)> {
        run.iter()
            .map(|&(along, value)| {
                let (x, y) = frame.turn(along, value);
                (f(x), f(y))
            })
            .collect()
    };
    let mut line = Vec::new();
    let mut solid = String::new();
    for run in clipped(&piece(lo, hi)) {
        solid_path(&run, frame, &mut solid);
        line.push(turned(&run));
    }
    let mut dash = String::new();
    for run in clipped(&piece(from, lo)).iter().chain(&clipped(&piece(hi, to))) {
        dashed_path(run, frame, &mut dash);
        line.push(turned(run));
    }
    (solid, dash, line)
}

/// Часть подсказки блока в легенде о модели нагрева: что это, условия кривой, ошибка, сколько данных. «°C на ступень»
/// по своей нагрузке здесь не показывается: занятость — это время бодрствования, а не тепло работы, и Windows даёт
/// тяжёлые потоки верхним по рангу ядрам — число мерило тяжесть потока, а не ядро (DECISIONS #39). Ватт нет (бриф
/// 16.10 «эффективность»).
fn model_tip(model: &Model, block: &BlockInfo, scale: Scale) -> String {
    let Some(m) = model_of(model, block) else {
        return String::new();
    };
    let e_note = if m.e {
        "\n• нагрузка кластера — по первому ядру, за четыре ядра"
    } else {
        ""
    };
    let (Some(r), true) = (m.reference.as_ref(), m.drawn()) else {
        return format!(
            "\nМодели нагрева нет: годных срезов {}, нужно хотя бы {:.0} мин.\n• годный срез: известны его и соседей нагрузка и множитель, температура и кристалл вокруг; первые 16 с после перерыва журнала не в счёт{e_note}",
            glued(&span_of(m.minutes * 60.0)),
            crate::model::MIN_MINUTES
        );
    };
    let head = "\nМодель нагрева: регрессия по журналу срезов — температура блока из кристалла вокруг, своей работы на каждом целом множителе, соседей, предыдущих секунд и ранга потолка.";
    let near: Vec<String> = r
        .neighbours
        .iter()
        .filter_map(|&(j, p)| Some(format!("{} — {}", model.blocks.get(j)?.name, percent(p))))
        .collect();
    let near = if near.is_empty() {
        String::new()
    } else {
        format!(
            "; соседи: {} полной нагрузки на ×{:.0}",
            near.join(", "),
            crate::model::REFERENCE
        )
    };
    let error = m
        .errors
        .map(|e| format!("\n• ошибка на отложенных данных {:.2} °C (средние за 10 с)", e.full))
        .unwrap_or_default();
    let conditions = if scale == Scale::Efficiency {
        "\n• эффективность: множитель / свой нагрев при полной нагрузке на нём; кристалл, соседи, предыдущие секунды и ранг вынесены; усы — разброс по четырём частям данных".to_string()
    } else {
        format!(
            "\n• условия кривой: своя нагрузка 100 %, кристалл {:.1} °C{near} (медианы, пока блок под нагрузкой)",
            r.die
        )
    };
    format!(
        "{head}{conditions}{error}\n• данных {}{e_note}",
        glued(&span_of(m.minutes * 60.0)),
    )
}

/// «Что влияет на температуру ядра» (бриф W): по группе входов — изменчивость её вклада (°C) и насколько хуже ошибка
/// модели без неё; «не определено», где разброс по частям данных доходит до нуля. Ранг — только в скрытом отчёте.
fn influence_tip(m: &BlockModel) -> String {
    let Some(inf) = m.influence else {
        return String::new();
    };
    let groups = [
        (crate::model::DIE_GROUP, "кристалл"),
        (crate::model::OWN_GROUP, "своя работа"),
        (crate::model::NEAR_GROUP, "соседи"),
        (crate::model::HISTORY_GROUP, "предыдущие секунды"),
    ];
    let lines: Vec<String> = groups
        .iter()
        .map(|&(g, name)| {
            if inf.determined(g) {
                let without = inf.ablation[g]
                    .map(|a| format!("; без него ошибка {}{GLUE}°C", glued(&signed_temp(a, 2))))
                    .unwrap_or_default();
                format!("• {name}: {:.1}{GLUE}°C{without}", inf.size[g])
            } else {
                format!("• {name}: не определено")
            }
        })
        .collect();
    format!(
        "\nЧто влияет на температуру ядра\nРазброс вклада каждой группы по журналу, °C, и насколько хуже модель на отложенных данных без неё.\n{}\n• видеокарта, вентиляторы и комната действуют через кристалл",
        lines.join("\n")
    )
}

/// Пути одной кривой: отрезки между соседними множителями (через пустую корзину линии нет — там только точки), каждый
/// с краской слабейшей из двух точек; отрезки одной густоты подряд — одной ломаной.
fn runs(ratios: &[i32], at: &[(f64, f64)], inks: &[f64]) -> Vec<(f32, String)> {
    let mut paths: [String; 5] = Default::default();
    for i in 1..ratios.len() {
        if ratios[i].checked_sub(ratios[i - 1]) != Some(1) {
            continue;
        }
        let Some((a, b)) = unit_viewport().segment(at[i - 1], at[i]) else {
            continue;
        };
        let path = &mut paths[ink_level(inks[i].min(inks[i - 1]))];
        let _ = write!(
            path,
            "M {:.1} {:.1} L {:.1} {:.1} ",
            a.0 * 1000.0,
            (1.0 - a.1) * 1000.0,
            b.0 * 1000.0,
            (1.0 - b.1) * 1000.0
        );
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

/// Условия строки наибольшей (бриф W, «макс»): дата и время, множитель и потолок, нагрузка, уровень кристалла, соседи,
/// видеокарта, вентиляторы. `blocks` — блоки знакомства: соседи — по номеру в нём.
fn peak_conditions(peak: &samples::Peak, blocks: &[BlockInfo]) -> String {
    let w = &peak.when;
    let mut parts = vec![glued(&samples::day_time(peak.at))];
    parts.push(match w.ceiling {
        Some(c) => format!("×{:.1} / потолок{GLUE}×{c:.0}", peak.ratio),
        None => format!("×{:.1}", peak.ratio),
    });
    if let Some(b) = peak.busy {
        parts.push(format!("нагрузка {}", glued(&percent(b))));
    }
    if let Some(l) = w.level {
        parts.push(format!("кристалл {l:.1}{GLUE}°C"));
    }
    let near: Vec<String> = w
        .near
        .iter()
        .flatten()
        .filter_map(|&(j, t)| Some(format!("{}{GLUE}{t:.0}°", blocks.get(j)?.name)))
        .collect();
    if !near.is_empty() {
        parts.push(format!("соседи {}", near.join(", ")));
    }
    let gpu: Vec<String> = [
        w.gpu.temp.map(|t| format!("{t:.0}{GLUE}°C")),
        w.gpu.power.map(|p| format!("{p:.0}{GLUE}Вт")),
        w.gpu.busy.map(|b| glued(&percent(b))),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !gpu.is_empty() {
        parts.push(format!("видеокарта {}", gpu.join(", ")));
    }
    if w.fans.cpu.is_some() || w.fans.gpu.is_some() {
        let rpm = |v: Option<f64>| v.map_or_else(|| "–".to_string(), |v| glued(&grouped(rpm_u64(v))));
        parts.push(format!("вент.{GLUE}{} / {}", rpm(w.fans.cpu), rpm(w.fans.gpu)));
    }
    parts.join(" · ")
}

/// Отсечки автомодуля блоков сравнения на множителе — для подсказки столбца.
fn cut_lines(ratio: i32, compared: &[&BlockInfo], cuts: &[(usize, &Event)]) -> Vec<String> {
    cuts.iter()
        .filter(|(_, a)| a.ratio.map(f64::round) == Some(f64::from(ratio)))
        .filter_map(|(which, a)| {
            Some(format!(
                "{} {} {} {}: {}",
                if a.kind == Kind::Target { "●" } else { "◆" },
                a.time,
                compared.get(*which)?.name,
                a.change,
                a.why
            ))
        })
        .collect()
}

/// Подсказка столбца множителя: что у каждого блока сравнения на нём — сколько времени, ø нагрузка, обороты; ø, 95 % и
/// ▲ наибольшая со строкой, где она была (фактический множитель, нагрузка, время; с «макс» — все условия этой строки);
/// отсечки автомодуля на этом множителе.
fn column_tip(
    ratio: i32,
    input: &StatsInput,
    compared: &[&BlockInfo],
    all: &[Vec<Stat>],
    cuts: &[(usize, &Event)],
) -> String {
    let scale = input.scale;
    let mut lines: Vec<String> = compared
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
                _ => (1, 1),
            };
            let head = format!(
                "{} — {}{load}{fan}\n    ø{GLUE}{}{GLUE}°C · 95{GLUE}% ниже {}{GLUE}°C",
                capital(&whom(b)),
                glued(&span_of(bin.secs)),
                temp_text(scale, bin.mean().unwrap_or(s.p95), 1),
                temp_text(scale, s.p95, p95_digits),
            );
            Some(if input.max {
                format!(
                    "{head}\n    ▲{GLUE}макс {}{GLUE}°C: {}",
                    temp_text(scale, peak.temp, peak_digits),
                    peak_conditions(&peak, input.blocks)
                )
            } else {
                format!(
                    "{head} · ▲{GLUE}{}{GLUE}°C: ×{:.1}{peak_load}, {}",
                    temp_text(scale, peak.temp, peak_digits),
                    peak.ratio,
                    glued(&samples::day_time(peak.at))
                )
            })
        })
        .collect();
    lines.extend(cut_lines(ratio, compared, cuts));
    if lines.is_empty() {
        return String::new();
    }
    let what = match scale {
        Scale::AboveDie => " · над остальным кристаллом",
        _ => "",
    };
    format!("×{ratio}{what}\n{}", lines.join("\n"))
}

/// Подсказка столбца в виде «эффективность»: у каждого блока с точкой — эффективность с разбросом, свой нагрев и сколько
/// данных в корзине (время строк, ø нагрузка).
fn efficiency_column_tip(ratio: i32, compared: &[&BlockInfo], points: &[Vec<Efficiency>]) -> String {
    let lines: Vec<String> = compared
        .iter()
        .zip(points)
        .filter_map(|(b, pts)| {
            let e = pts.iter().find(|p| p.ratio == ratio)?;
            Some(format!(
                "{} — {:.2} ({:.2}–{:.2}) на °C; свой нагрев {}{GLUE}°C; данных {}, нагрузка ø{GLUE}{}",
                capital(&whom(b)),
                e.value,
                e.lo,
                e.hi,
                glued(&format!("{:.1}", e.heat)),
                glued(&span_of(e.secs)),
                glued(&percent(e.load))
            ))
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "×{ratio} · производительность на градус: множитель / свой нагрев при полной нагрузке, всё прочее вынесено\nЛиния — основная подгонка; область — чувствительность к частям обучающих данных, не доверительный интервал.\n{}",
        lines.join("\n")
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the chart's scales, lines and columns read top to bottom"
)]
pub fn stats_panel(input: &StatsInput) -> StatsShown {
    let mut shown = StatsShown {
        zero: -1.0,
        cutoff: -1.0,
        emergency: -1.0,
        swap: input.swap,
        curves: input.curves,
        max: input.max,
        value_title: match input.scale {
            Scale::Efficiency => "производительность на °C".into(),
            Scale::AboveDie => "°C над кристаллом".into(),
            Scale::Absolute => "°C".into(),
            Scale::ConditionalEfficiency => "множитель / собственный нагрев, 1/°C".into(),
            Scale::ConditionalHeat => "собственный нагрев, °C".into(),
        },
        model: input.model,
        ..StatsShown::default()
    };
    let compared: Vec<&BlockInfo> = input
        .compared
        .iter()
        .filter_map(|&b| input.blocks.iter().find(|x| x.index == b))
        .collect();
    if matches!(input.scale, Scale::ConditionalEfficiency | Scale::ConditionalHeat) {
        snapshot_panel(input, &compared, &mut shown);
        return shown;
    }
    shown.legend = legend(input.blocks, &compared, input.digest, input.scale);
    let Some(digest) = input.digest else {
        shown.empty = "Статистика читается…".into();
        return shown;
    };

    let names: Vec<&str> = compared.iter().map(|b| b.name.as_str()).collect();
    let several = compared.len() > 1;
    // Отсечки (аварийная и по цели) — отметками на линии блока и в подсказке столбца; остальные события (и прежнее
    // снижение до 0.5.6) — в списке под графиком и на оси времени.
    let cuts: Vec<(usize, &Event)> = crate::events::listed(&digest.events, &names, false)
        .into_iter()
        .filter_map(|(i, which)| Some((which?, &digest.events[i])))
        .filter(|(_, e)| matches!(e.kind, Kind::Emergency | Kind::Target))
        .collect();

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
    if input.scale == Scale::Efficiency {
        efficiency_panel(input, digest, &compared, &mut shown);
        return shown;
    }
    let all: Vec<Vec<Stat>> = compared.iter().map(|b| stats_of(digest, b, input.scale)).collect();
    if all.iter().all(Vec::is_empty) {
        let whose = if several {
            "У выбранных блоков".to_string()
        } else {
            format!("У блока {}", compared[0].name)
        };
        shown.empty = if digest.tally.rows == 0 {
            "Срезов пока нет. Их пишет CoreMapTray 4 раза в секунду, пока включена запись и процессор нагружен.".into()
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
    let fitted = digest.model.as_deref();
    let solid: Vec<f64> = if input.model {
        fitted
            .map(|m| {
                compared
                    .iter()
                    .filter_map(|b| model_of(m, b))
                    .flat_map(|b| model_solid(b, input.scale))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let ratio_values: Vec<f64> = all.iter().flatten().map(|s| f64::from(s.ratio)).collect();
    let values: Vec<f64> = all
        .iter()
        .flatten()
        .flat_map(|s| {
            [
                input.curves.mean().then(|| s.bin.mean().unwrap_or(s.p95)),
                input.curves.p95().then_some(s.p95),
                input.max.then_some(s.bin.peak.temp),
            ]
            .into_iter()
            .flatten()
        })
        .chain(solid)
        .collect();
    // Скрытые линии не растягивают шкалу; при полностью пустом рисунке оставляем оси по данным.
    let fallback: Vec<f64> = if values.is_empty() {
        all.iter().flatten().filter_map(|s| s.bin.mean()).collect()
    } else {
        Vec::new()
    };
    let Some(frame) = resolve_frame(
        input,
        &ratio_values,
        if values.is_empty() { &fallback } else { &values },
        &mut shown,
    ) else {
        return shown;
    };
    let (lo, hi, t0, t1) = (frame.lo, frame.hi, frame.t0, frame.t1);
    fill_ticks(&frame, input.scale, &mut shown);
    match input.scale {
        // Отсечки автомодуля — абсолютная температура: на шкале «над кристаллом» им места нет.
        Scale::Absolute => {
            if let Some(c) = input.cutoff
                && (t0..=t1).contains(&c)
            {
                shown.cutoff = f(frame.value(c));
                shown.cutoff_text = format!("{c:.0}°");
            }
            if let Some(e) = input.emergency
                && (t0..=t1).contains(&e)
            {
                shown.emergency = f(frame.value(e));
                shown.emergency_text = format!("{e:.0}°");
            }
        }
        _ if (t0..=t1).contains(&0.0) => shown.zero = f(frame.value(0.0)),
        _ => {}
    }

    let most = fullest(&all);
    // Подсвеченная линия рисуется последней — поверх остальных (порядок остальных — порядок добавления).
    let mut order: Vec<usize> = (0..compared.len()).collect();
    order.sort_by_key(|&i| input.highlight == Some(compared[i].index));
    for i in order {
        let b = compared[i];
        let stats = &all[i];
        let ratios: Vec<i32> = stats.iter().map(|s| s.ratio).collect();
        let inks: Vec<f64> = stats.iter().map(|s| ink(s.bin.secs, most)).collect();
        let means: Vec<(f64, f64)> = stats
            .iter()
            .map(|s| frame.place(s.ratio, s.bin.mean().unwrap_or(s.p95)))
            .collect();
        let highs: Vec<(f64, f64)> = stats.iter().map(|s| frame.place(s.ratio, s.p95)).collect();
        let tops: Vec<(f64, f64)> = stats.iter().map(|s| frame.place(s.ratio, s.bin.peak.temp)).collect();
        let (model, model_dash, model_line) = match fitted.and_then(|m| model_of(m, b)) {
            Some(m) if input.model => model_paths(m, input.scale, &frame),
            _ => (String::new(), String::new(), Vec::new()),
        };
        // Отметка отсечки — на первой показанной линии блока (ø, 95 %, наибольшие) на её множителе.
        let line_at = |ratio: i32| -> Option<(f64, f64)> {
            let k = stats.iter().position(|s| s.ratio == ratio)?;
            [
                (input.curves.mean(), means[k]),
                (input.curves.p95(), highs[k]),
                (input.max, tops[k]),
            ]
            .into_iter()
            .find_map(|(on, at)| on.then_some(at))
        };
        let block_cuts: Vec<CutShown> = cuts
            .iter()
            .filter(|(which, _)| *which == i)
            .filter_map(|(_, a)| {
                let ratio = a.ratio.map(f64::round)?;
                #[expect(clippy::cast_possible_truncation, reason = "a rounded multiplier, a few dozen")]
                let (x, y) = line_at(ratio as i32)?;
                if !inside((x, y)) {
                    return None;
                }
                Some(CutShown {
                    x: f(x),
                    y: f(y),
                    kind: if a.kind == Kind::Target { 2 } else { 1 },
                })
            })
            .collect();
        shown.series.push(SeriesShown {
            block: b.index,
            color: color_of(input.blocks, b.index),
            faded: input.highlight.is_some_and(|h| h != b.index),
            mean: if input.curves.mean() {
                runs(&ratios, &means, &inks)
            } else {
                Vec::new()
            },
            p95: if input.curves.p95() {
                runs(&ratios, &highs, &inks)
            } else {
                Vec::new()
            },
            max: if input.max {
                runs(&ratios, &tops, &inks)
            } else {
                Vec::new()
            },
            cuts: block_cuts,
            model,
            model_dash,
            model_line,
            sensitivity: String::new(),
            points: stats
                .iter()
                .enumerate()
                .map(|(k, s)| PointShown {
                    ratio: s.ratio,
                    x: f(means[k].0),
                    y: f(means[k].1),
                    qx: f(highs[k].0),
                    qy: f(highs[k].1),
                    mx: f(tops[k].0),
                    my: f(tops[k].1),
                    samples: s.bin.n,
                    load: s.bin.load().map_or(-1.0, |l| f(l.clamp(0.0, 1.0))),
                    ink: f(inks[k]),
                })
                .collect(),
        });
    }

    for ratio in (lo..=hi).filter(|&r| (0.0..=1.0).contains(&frame.along(r))) {
        shown.columns.push(ColumnShown {
            ratio,
            at: f(frame.along(ratio)),
            tip: column_tip(ratio, input, &compared, &all, &cuts),
        });
    }
    shown
}

fn snapshot_value(point: &crate::conditional_snapshot::CurvePoint, scale: Scale, alternate: bool) -> f64 {
    let heat = if alternate { point.alternate_heat } else { point.heat };
    if scale == Scale::ConditionalHeat {
        heat
    } else {
        point.ratio / heat
    }
}

fn snapshot_panel(input: &StatsInput, compared: &[&BlockInfo], shown: &mut StatsShown) {
    let Some(digest) = input.digest else {
        shown.empty = "Сохранённый расчёт читается…".into();
        return;
    };
    let Some(snapshot) = digest.conditional.as_deref() else {
        shown.empty = digest.conditional_trouble.clone().unwrap_or_else(|| {
            "Сохранённой условной модели пока нет. Сбор статистики продолжает работать независимо от графика.".into()
        });
        return;
    };
    if let Err(message) = snapshot.validate() {
        shown.empty = message;
        return;
    }
    shown.note = format!(
        "Эксперимент · {} · {} · сплошная / пунктир: две подгонки, одинаковое окружение; сбор новых срезов независим",
        snapshot.source, snapshot.conditions
    );
    let available: Vec<_> = compared
        .iter()
        .filter_map(|&block| {
            snapshot
                .blocks
                .iter()
                .find(|curve| curve.name == block.name)
                .map(|curve| (block, curve))
        })
        .collect();
    shown.legend = compared.iter().map(|block| {
        let model = snapshot.blocks.iter().find(|curve| curve.name == block.name);
        LegendShown {
            block: block.index, name: block.name.clone(), color: color_of(input.blocks, block.index),
            tip: model.map_or_else(|| "Для этого блока сохранённой условной кривой пока нет. Кривая другого ядра вместо неё не подставляется.".into(), |curve| format!("Ядро {} · экспериментальная модель\nЛиния — основная подгонка; область — размах подгонок при том же окружении, не обязательно симметричный относительно линии.\n• расхождение — чувствительность к подгонке, не влияние вентиляторов / GPU и не доверительный интервал\n• средняя абсолютная ошибка температуры на отложенных данных {:.2} °C; это не ошибка собственного нагрева\n• полная занятость ядра, собственная история установившаяся; окружение — записанная траектория\n• {}\n• модель сохранена по журналу и автоматически не переобучается; ранжирование и подбор потолков пока не подтверждены", curve.name, curve.temperature_test_mae, snapshot.conditions)),
        }
    }).collect();
    if available.is_empty() {
        let names = snapshot
            .blocks
            .iter()
            .map(|curve| curve.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        shown.empty =
            format!("Выбери ядро с сохранённым расчётом: {names}. Остальная статистика доступна в прежних видах.");
        return;
    }
    let low = available
        .iter()
        .flat_map(|(_, curve)| &curve.points)
        .map(|p| p.ratio)
        .fold(f64::INFINITY, f64::min);
    let high = available
        .iter()
        .flat_map(|(_, curve)| &curve.points)
        .map(|p| p.ratio)
        .fold(f64::NEG_INFINITY, f64::max);
    if low < 0.0 || high > 255.0 {
        shown.empty = "Множители сохранённого расчёта вне диапазона процессора.".into();
        return;
    }
    let values: Vec<f64> = available
        .iter()
        .flat_map(|(_, curve)| &curve.points)
        .flat_map(|p| {
            [
                snapshot_value(p, input.scale, false),
                snapshot_value(p, input.scale, true),
            ]
        })
        .collect();
    let ratio_values: Vec<f64> = available
        .iter()
        .flat_map(|(_, c)| c.points.iter().map(|p| p.ratio))
        .collect();
    let Some(frame) = resolve_frame(input, &ratio_values, &values, shown) else {
        return;
    };
    fill_ticks(&frame, input.scale, shown);
    let mut ordered = available;
    ordered.sort_by_key(|(block, _)| input.highlight == Some(block.index));
    snapshot_series(input, &frame, &ordered, shown);
    snapshot_columns(&frame, &ordered, shown);
}
type SnapshotCurve<'a> = (&'a BlockInfo, &'a crate::conditional_snapshot::BlockCurve);

fn snapshot_series(input: &StatsInput, frame: &Frame, ordered: &[SnapshotCurve], shown: &mut StatsShown) {
    for (block, curve) in ordered {
        let points: Vec<_> = curve
            .points
            .iter()
            .map(|p| {
                (
                    frame.along_at(p.ratio),
                    frame.value(snapshot_value(p, input.scale, false)),
                )
            })
            .collect();
        let intervals: Vec<_> = curve
            .points
            .iter()
            .map(|p| {
                let primary = snapshot_value(p, input.scale, false);
                let alternate = snapshot_value(p, input.scale, true);
                (p.ratio, primary.min(alternate), primary.max(alternate))
            })
            .collect();
        let mut series = SeriesShown {
            block: block.index,
            color: color_of(input.blocks, block.index),
            faded: input.highlight.is_some_and(|h| h != block.index),
            ..SeriesShown::default()
        };
        if input.model {
            for run in clipped(&points) {
                solid_path(&run, frame, &mut series.model);
                series.model_line.push(
                    run.iter()
                        .map(|&(r, v)| {
                            let (x, y) = frame.turn(r, v);
                            (f(x), f(y))
                        })
                        .collect(),
                );
            }
            series.sensitivity = sensitivity_path(frame, &intervals, f64::INFINITY);
        }
        shown.series.push(series);
    }
}

fn snapshot_columns(frame: &Frame, ordered: &[SnapshotCurve], shown: &mut StatsShown) {
    for ratio in (frame.lo..=frame.hi).filter(|&r| (0.0..=1.0).contains(&frame.along(r))) {
        let mut tip = format!(
            "×{ratio} · сохранённый экспериментальный расчёт\nЛиния — основная подгонка; область — размах двух подгонок при одном окружении, не доверительный интервал. Между узлами — линейная интерполяция."
        );
        for (block, curve) in ordered {
            let r = f64::from(ratio);
            let pair = curve.points.windows(2).find(|w| w[0].ratio <= r && r <= w[1].ratio);
            if let Some(w) = pair {
                let alpha = (r - w[0].ratio) / (w[1].ratio - w[0].ratio);
                let heat = w[0].heat + alpha * (w[1].heat - w[0].heat);
                let other = w[0].alternate_heat + alpha * (w[1].alternate_heat - w[0].alternate_heat);
                let _ = write!(
                    tip,
                    "\nЯдро {}: свой нагрев {:.2} / {:.2} °C; эффективность {:.2} / {:.2} 1/°C",
                    block.name,
                    heat,
                    other,
                    r / heat,
                    r / other
                ); // String не отказывает.
            } else {
                let _ = write!(tip, "\nЯдро {}: вне диапазона сохранённой кривой", block.name);
            }
        }
        shown.columns.push(ColumnShown {
            ratio,
            at: f(frame.along(ratio)),
            tip,
        });
    }
}

fn sensitivity_path(frame: &Frame, intervals: &[(f64, f64, f64)], max_gap: f64) -> String {
    let mut out = String::new();
    for pair in intervals.windows(2) {
        let (a, low_a, high_a) = pair[0];
        let (b, low_b, high_b) = pair[1];
        if b <= a || b - a > max_gap || low_a > high_a || low_b > high_b {
            continue;
        }
        let vertices = [
            frame.turn(frame.along_at(a), frame.value(low_a)),
            frame.turn(frame.along_at(b), frame.value(low_b)),
            frame.turn(frame.along_at(b), frame.value(high_b)),
            frame.turn(frame.along_at(a), frame.value(high_a)),
        ];
        let polygon = unit_viewport().polygon(&vertices);
        for (index, (x, y)) in polygon.iter().enumerate() {
            let command = if index == 0 { 'M' } else { 'L' };
            let _ = write!(out, "{command} {:.1} {:.1} ", x * 1000.0, (1.0 - y) * 1000.0);
        }
        if !polygon.is_empty() {
            out.push_str("Z ");
        }
    }
    out
}
/// Вид «эффективность» (бриф 16.10): у каждого выбранного блока — точки по целым множителям (производительность на
/// градус) своим цветом, соединённые линией между соседними множителями, с усами разброса по четырём частям данных;
/// шкалы охватывают выбранные точки с усами. Заливка точки — ø нагрузка корзины, бледность — её
/// время, как у сырых корзин.
fn efficiency_panel(input: &StatsInput, digest: &Digest, compared: &[&BlockInfo], shown: &mut StatsShown) {
    let fitted = digest.model.as_deref();
    let points: Vec<Vec<Efficiency>> = compared
        .iter()
        .map(|b| {
            fitted
                .and_then(|m| model_of(m, b))
                .map(BlockModel::efficiency)
                .unwrap_or_default()
        })
        .collect();
    if points.iter().all(Vec::is_empty) {
        shown.empty = if fitted.is_none() {
            "Модель нагрева ещё считается — эффективность появится, когда она будет готова.".into()
        } else {
            "Эффективности нет: у выбранных блоков нет модели (нужно хотя бы 10 минут годных срезов) или ни на одном множителе нет минуты полной нагрузки с определённым нагревом — подробно в подсказке блока над графиком. Срезы как есть — видами «над кристаллом» и «абсолютная».".into()
        };
        return;
    }
    let values: Vec<f64> = points.iter().flatten().flat_map(|p| [p.value, p.lo, p.hi]).collect();
    let ratio_values: Vec<f64> = points.iter().flatten().map(|p| f64::from(p.ratio)).collect();
    let Some(frame) = resolve_frame(input, &ratio_values, &values, shown) else {
        return;
    };
    let (lo, hi) = (frame.lo, frame.hi);
    fill_ticks(&frame, Scale::Efficiency, shown);
    let most = points.iter().flatten().map(|p| p.secs).fold(0.0, f64::max);
    let mut order: Vec<usize> = (0..compared.len()).collect();
    order.sort_by_key(|&i| input.highlight == Some(compared[i].index));
    for i in order {
        let b = compared[i];
        let pts = &points[i];
        let inks: Vec<f64> = pts.iter().map(|p| ink(p.secs, most)).collect();
        let at: Vec<(f64, f64)> = pts.iter().map(|p| frame.place(p.ratio, p.value)).collect();
        let ratios: Vec<i32> = pts.iter().map(|p| p.ratio).collect();
        let intervals: Vec<_> = pts.iter().map(|p| (f64::from(p.ratio), p.lo, p.hi)).collect();
        let sensitivity = sensitivity_path(&frame, &intervals, 1.0);
        shown.series.push(SeriesShown {
            block: b.index,
            color: color_of(input.blocks, b.index),
            faded: input.highlight.is_some_and(|h| h != b.index),
            mean: runs(&ratios, &at, &inks),
            sensitivity,
            points: pts
                .iter()
                .zip(&at)
                .zip(&inks)
                .filter(|((_, at), _)| inside(**at))
                .map(|((p, &(x, y)), &a)| PointShown {
                    ratio: p.ratio,
                    x: f(x),
                    y: f(y),
                    load: f(p.load.clamp(0.0, 1.0)),
                    ink: f(a),
                    ..PointShown::default()
                })
                .collect(),
            ..SeriesShown::default()
        });
    }
    for ratio in (lo..=hi).filter(|&r| (0.0..=1.0).contains(&frame.along(r))) {
        shown.columns.push(ColumnShown {
            ratio,
            at: f(frame.along(ratio)),
            tip: efficiency_column_tip(ratio, compared, &points),
        });
    }
}

// Место точки одной кривой: ø средней или 95 %.
type PlaceOf = fn(&PointShown) -> (f32, f32);

/// Линия какого блока ближе всего к курсору: поле графика w × h пикселей, курсор (x, y) от его левого верхнего угла;
/// None — ни одна не ближе `NEAR_PX`. Считаются показанные кривые блока — ø средняя и 95 % (по `curves`) и кривая модели
/// (её ломаные пусты, когда она скрыта): подсветка работает с тем, что видно. Места точек уже повёрнуты
/// (`Frame::place`), поэтому одинаково при любой раскладке осей.
#[cfg(test)]
#[must_use]
pub fn nearest(series: &[SeriesShown], cursor: (f64, f64), field: (f64, f64), curves: Curves) -> Option<usize> {
    nearest_with(series, cursor, field, curves, false)
}

/// То же, и линия наибольших, когда она показана (`max`).
#[must_use]
pub fn nearest_with(
    series: &[SeriesShown],
    cursor: (f64, f64),
    field: (f64, f64),
    curves: Curves,
    max: bool,
) -> Option<usize> {
    let (width, height) = field;
    let mut kinds: Vec<PlaceOf> = Vec::with_capacity(3);
    if curves.mean() {
        kinds.push(|p| (p.x, p.y));
    }
    if curves.p95() {
        kinds.push(|p| (p.qx, p.qy));
    }
    if max {
        kinds.push(|p| (p.mx, p.my));
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
        for run in &s.model_line {
            let at = |p: (f32, f32)| (f64::from(p.0) * width, (1.0 - f64::from(p.1)) * height);
            for w in run.windows(2) {
                let gap = segment_distance(at(w[0]), at(w[1]), cursor);
                if gap <= NEAR_PX && best.is_none_or(|(closest, _)| gap < closest) {
                    best = Some((gap, s.block));
                }
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
        _fused: Option<&[i64]>,
        scale: Scale,
        swap: bool,
    ) -> StatsShown {
        stats_panel(&StatsInput {
            blocks: &blocks(),
            compared,
            highlight,
            cutoff: None,
            digest: Some(digest),
            scale,
            curves: Curves::Both,
            model: true,
            swap,
            ..StatsInput::default()
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
        let range = s.value_range.expect("диапазон выбранных данных");
        assert!(range.min > 60.0 && range.min < 70.0 && range.max > 80.0 && range.max < 90.0);
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
    fn above_the_die_axis_follows_it() {
        // Ядро 0 на 80 и 70 °C, кластер — всегда 60 °C: над кристаллом ядро на +20 и +10.
        let d = thirty(false);
        let s = stats_with(&d, &[0], None, None, Scale::AboveDie, false);
        assert_eq!(s.value_title, "°C над кристаллом");
        let tip = s.columns[2].tip.replace('\u{a0}', " ");
        assert!(
            tip.contains("над остальным кристаллом") && tip.contains("ø +20.0 °C"),
            "{tip}"
        );
        // Шкала охватывает выбранные +10…+20; ноль вне неё не добавляется.
        let ticks: Vec<&str> = s.ticks.iter().map(|t| t.1.as_str()).collect();
        assert!(ticks.iter().all(|s| s.starts_with('+')));
        let range = s.value_range.expect("автодиапазон");
        assert!(range.min > 0.0 && range.min < 10.0 && range.max > 20.0);
        assert!(s.zero < 0.0);
        // Отсечка автомодуля (абсолютная) на этой шкале не рисуется.
        let with_cutoff = stats_panel(&StatsInput {
            blocks: &blocks(),
            compared: &[0],
            highlight: None,
            cutoff: Some(85.0),
            digest: Some(&d),
            scale: Scale::AboveDie,
            curves: Curves::Both,
            model: true,
            swap: false,
            ..StatsInput::default()
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
    fn auto_axes_follow_selected_blocks_and_manual_axes_are_shared() {
        let d = thirty(true);
        let fused = [53, 46, 46, 46, 46];
        assert_eq!(axis(&blocks(), Some(&fused), Some(&d)), Some((32, 54)));
        let p = stats(&d, &[0], None, Some(&fused));
        let e = stats(&d, &[1], None, Some(&fused));
        let both = stats(&d, &[0, 1], None, Some(&fused));
        assert!(p.argument_range.expect("P").min > e.argument_range.expect("E").max);
        let all = both.argument_range.expect("общая шкала");
        assert!(all.min < 44.0 && all.max > 50.0 && all.max < 53.0);
        for compared in [&[0][..], &[1][..], &[0, 1][..]] {
            let s = stats_panel(&StatsInput {
                blocks: &blocks(),
                compared,
                digest: Some(&d),
                scale: Scale::Absolute,
                argument_range: Range::new(40.0, 55.0),
                value_range: Range::new(50.0, 90.0),
                ..StatsInput::default()
            });
            assert_eq!(s.argument_range, Range::new(40.0, 55.0));
            assert_eq!(s.value_range, Range::new(50.0, 90.0));
        }
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
                cutoff: None,
                digest: Some(&d),
                scale: Scale::Absolute,
                curves,
                model: true,
                swap: false,
                ..StatsInput::default()
            })
        };
        let both = with(Curves::Both);
        assert!(!both.series[0].mean.is_empty() && !both.series[0].p95.is_empty());
        let mean = with(Curves::Mean);
        assert!(!mean.series[0].mean.is_empty() && mean.series[0].p95.is_empty());
        let p95 = with(Curves::P95);
        assert!(p95.series[0].mean.is_empty() && !p95.series[0].p95.is_empty());
        assert!(mean.value_range.expect("среднее").max <= both.value_range.expect("обе").max);
        // Оба выключены — пустой график с осями: ни одной линии, оси и столбцы те же, надписи «нет данных» нет.
        let neither = with(Curves::Neither);
        assert!(neither.series[0].mean.is_empty() && neither.series[0].p95.is_empty());
        assert_eq!(
            (neither.columns.len(), &neither.empty),
            (both.columns.len(), &both.empty)
        );
    }

    // Два независимых выключателя (владелец 16.10): щелчок по одному меняет только свою линию; все четыре сочетания
    // достижимы и обратимы.
    #[test]
    fn each_curve_switch_flips_only_its_own_line() {
        for c in [Curves::Mean, Curves::P95, Curves::Both, Curves::Neither] {
            let m = c.toggled_mean();
            assert_eq!((m.mean(), m.p95()), (!c.mean(), c.p95()), "{c:?}");
            let q = c.toggled_p95();
            assert_eq!((q.mean(), q.p95()), (c.mean(), !c.p95()), "{c:?}");
            assert_eq!(m.toggled_mean(), c);
            assert_eq!(q.toggled_p95(), c);
            assert_eq!(Curves::of(c.mean(), c.p95()), c);
        }
        assert_eq!(Curves::default(), Curves::Both);
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
            cutoff: Some(85.0),
            value_range: Range::new(40.0, 100.0),
            digest: Some(&d),
            scale: Scale::Absolute,
            curves: Curves::Both,
            model: true,
            swap: false,
            ..StatsInput::default()
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
            argument_range: None,
        };
        let swapped = Frame { swap: true, ..normal };
        let (x, y) = normal.place(45, 70.0);
        assert!((x - 0.55).abs() < 1e-12 && (y - 0.5).abs() < 1e-12);
        assert_eq!(swapped.place(45, 70.0), (y, x));
        assert_eq!(normal.band(45), (0.5, 0.0, 0.1, 1.0));
        assert_eq!(swapped.band(45), (0.0, 0.5, 1.0, 0.1));
    }

    #[test]
    fn sensitivity_field_clips_and_transposes_the_same_region() {
        let normal = Frame {
            lo: 40,
            hi: 50,
            t0: 60.0,
            t1: 80.0,
            swap: false,
            argument_range: Range::new(40.0, 50.0),
        };
        let swapped = Frame { swap: true, ..normal };
        let intervals = [(44.0, 55.0, 85.0), (46.0, 55.0, 85.0)];
        let a = sensitivity_path(&normal, &intervals, 2.0);
        let b = sensitivity_path(&swapped, &intervals, 2.0);
        let coords = |path: &str| {
            path.split_whitespace()
                .filter_map(|s| s.parse::<f64>().ok())
                .collect::<Vec<_>>()
        };
        let a = coords(&a);
        let b = coords(&b);
        assert_eq!(a.len(), 8);
        assert_eq!(b.len(), 8);
        for p in a.as_chunks::<2>().0 {
            assert!(
                b.as_chunks::<2>()
                    .0
                    .iter()
                    .any(|q| (p[0], p[1]) == (1000.0 - q[1], 1000.0 - q[0]))
            );
        }
        assert_eq!(a.iter().step_by(2).copied().reduce(f64::min), Some(400.0));
        assert_eq!(a.iter().step_by(2).copied().reduce(f64::max), Some(600.0));
        assert!(a.iter().all(|v| (0.0..=1000.0).contains(v)));
    }

    #[test]
    fn sensitivity_field_does_not_bridge_missing_ratios_or_invent_area_for_one_point() {
        let frame = Frame {
            lo: 30,
            hi: 34,
            t0: 0.0,
            t1: 10.0,
            swap: false,
            argument_range: Range::new(30.0, 34.0),
        };
        let intervals = [(30.0, 1.0, 5.0), (31.0, 2.0, 8.0), (33.0, 1.0, 5.0), (34.0, 2.0, 8.0)];
        let field = sensitivity_path(&frame, &intervals, 1.0);
        assert_eq!(field.matches('M').count(), 2);
        assert_eq!(field.matches('Z').count(), 2);
        assert_eq!(sensitivity_path(&frame, &intervals[..1], 1.0), "");
        assert_eq!(sensitivity_path(&frame, &[(30.0, 2.0, 2.0), (31.0, 3.0, 3.0)], 1.0), "");
    }
    #[test]
    fn fractional_manual_argument_clips_lines_and_does_not_expand_column_loop() {
        let rows: Vec<String> = (0..20)
            .map(|i| {
                format!(
                    "2026-10-15 10:00:{i:02},60,,,53,{},1,{},46,44,0.5,60",
                    48 + i / 10,
                    70 + i % 4
                )
            })
            .collect();
        let d = digest(&rows);
        let s = stats_panel(&StatsInput {
            blocks: &blocks(),
            compared: &[0],
            digest: Some(&d),
            scale: Scale::Absolute,
            argument_range: Range::new(48.2, 48.8),
            value_range: Range::new(65.0, 80.0),
            ..StatsInput::default()
        });
        assert_eq!(s.columns, [] as [ColumnShown; 0]);
        let path = &s.series[0].mean[0].1;
        let values: Vec<f64> = path.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        assert_eq!(values[0], 0.0);
        assert_eq!(values[2], 1000.0);
        let wide = stats_panel(&StatsInput {
            blocks: &blocks(),
            compared: &[0],
            digest: Some(&d),
            scale: Scale::Absolute,
            argument_range: Range::new(-1e100, 1e100),
            value_range: Range::new(-1e100, 1e100),
            ..StatsInput::default()
        });
        assert!(wide.columns.len() <= 2 && wide.ticks.len() <= 11);
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

    /// 13 минут строк по 0,25 с: ядро 0 занято на ×44…×48, его температура растёт с нагрузкой; у кластера множитель
    /// есть только первые 5 минут (дальше пусто при нагрузке 0,3 — его мощность неизвестна): модели у него нет.
    fn modelled() -> Digest {
        let mut csv = String::from("time,0.ratio,0.busy,0.temp,2–5.ratio,2–5.busy,2–5.temp\n");
        for k in 0..3200u32 {
            let ms = k * 250;
            let ratio = 44 + (k / 37) % 5;
            let busy = 0.6 + 0.4 * f64::from((k / 11) % 3) / 2.0;
            let die = 50 + (k / 101) % 5;
            let p = busy * (f64::from(ratio) / 45.0).powi(3);
            let temp = 20.0 + 0.5 * f64::from(die) + 12.0 * p;
            let cluster = if k < 1200 { "40" } else { "" };
            let _ = writeln!(
                csv,
                "2026-10-15 10:{:02}:{:02}.{:03},{ratio},{busy:.2},{temp:.3},{cluster},0.30,{die}",
                ms / 60_000,
                ms / 1000 % 60,
                ms % 1000
            );
        }
        let log = crate::model::log_from_csv(&csv);
        let die = crate::model::Die {
            names: vec!["0".into(), "2–5".into()],
            e: vec![false, true],
            neighbours: vec![Vec::new(), Vec::new()],
        };
        let (header, body) = csv.split_once('\n').expect("заголовок");
        let columns = Columns::parse(header).expect("заголовок");
        let mut tally = Tally::default();
        Scratch::default().count(&columns, body, &mut None, &mut tally);
        Digest {
            tally,
            model: Some(std::sync::Arc::new(crate::model::fit(&[&log], &die))),
            ..Digest::default()
        }
    }

    #[test]
    fn the_model_curve_is_solid_over_observed_multipliers_and_dashed_beyond() {
        let d = modelled();
        let model = d.model.as_deref().expect("модель");
        assert!(
            model.blocks[0].drawn(),
            "у ядра 0 больше 10 мин: {}",
            model.blocks[0].minutes
        );
        assert!(
            !model.blocks[1].drawn(),
            "у кластера меньше: {}",
            model.blocks[1].minutes
        );
        let fused = [50i64, 46, 46, 46, 46];
        let s = stats_with(&d, &[0, 1], None, Some(&fused), Scale::AboveDie, false);
        let core = &s.series[0];
        assert!(!core.model.is_empty() && !core.model_dash.is_empty(), "{core:?}");
        assert!(s.series[1].model.is_empty() && s.series[1].model_dash.is_empty());
        // Сплошная — от ×44 до ×48 (наблюдённые под нагрузкой): её x — в столбцах 44…48 оси.
        let frame_x = |r: f64| s.argument_range.expect("множители").normal(r).expect("доля");
        let xs: Vec<f64> = core
            .model
            .split_whitespace()
            .filter_map(|t| t.parse::<f64>().ok())
            .step_by(2)
            .collect();
        let (lo, hi) = (
            xs.iter().copied().fold(f64::MAX, f64::min),
            xs.iter().copied().fold(f64::MIN, f64::max),
        );
        assert!(
            (lo / 1000.0 - frame_x(44.0)).abs() < 0.01 && (hi / 1000.0 - frame_x(48.0)).abs() < 0.01,
            "{lo} {hi}"
        );
        // Подсказки: у ядра — что такое модель, условия кривой и ошибка, без ватт и «°C на ступень» (бриф 16.10
        // «эффективность»); у кластера — сколько данных и что нужно 10 мин.
        let core = &s.legend[0].tip;
        assert!(
            core.contains("Модель нагрева: регрессия")
                && core.contains("условия кривой")
                && core.contains("ошибка на отложенных данных")
                && !core.contains("ватт")
                && !core.contains("°C на ступень"),
            "{core}"
        );
        assert!(!s.legend[0].tip.contains("по первому ядру"));
        let e = &s.legend[1].tip;
        assert!(
            e.contains("Модели нагрева нет")
                && e.contains("нужно хотя бы 10 мин")
                && e.contains("по первому ядру, за четыре ядра"),
            "{e}"
        );
        // Выключатель «модель» прячет кривые.
        let off = stats_panel(&StatsInput {
            blocks: &blocks(),
            compared: &[0, 1],
            highlight: None,
            cutoff: None,
            digest: Some(&d),
            scale: Scale::AboveDie,
            curves: Curves::Both,
            model: false,
            swap: false,
            ..StatsInput::default()
        });
        assert!(off.series.iter().all(|l| l.model.is_empty() && l.model_dash.is_empty()));
        // У ядра 0 пять соседних множителей — его линия ø осталась (у кластера один множитель — линии и не было).
        assert!(!off.series[0].mean.is_empty(), "линия ø осталась");
    }

    /// Строки «макс»: ядро 0 и его сосед 1 и несоседний 2 (по 0,25 с, 10:00:00 … 10:00:09.750); у ядра 0 наибольшая —
    /// 88 °C в 10:00:05.250 на ×45.3 при потолке 46 (в остальных строках 70 °C, потолок 47); видеокарта — в 10:00:03.750
    /// (65 °C) и в 10:00:05.750 (66 °C, ближе — на 0,5 с), вентиляторы — в 10:00:04.250 (4100 / 4300, ближе — на 1 с)
    /// и в 10:00:07.000 (5000 / 5200).
    fn max_rows() -> Vec<String> {
        (0..40)
            .map(|k: u32| {
                let ms = k * 250;
                let time = format!("2026-10-15 10:00:{:02}.{:03}", ms / 1000, ms % 1000);
                let (temp, ceiling) = if ms == 5250 { (88, 46) } else { (70, 47) };
                let fans = match ms {
                    4250 => "4100,4300",
                    7000 => "5000,5200",
                    _ => ",",
                };
                let gpu = match ms {
                    3750 => "65,110.5,0.80",
                    5750 => "66,120.4,0.90",
                    _ => ",,",
                };
                format!("{time},60,{fans},{ceiling},45.3,0.92,{temp},47,44,0.5,63,47,40,0.2,55,{gpu}")
            })
            .collect()
    }

    const MAX_HEADER: &str = "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp,1.ceiling,1.ratio,1.busy,1.temp,2.ceiling,2.ratio,2.busy,2.temp,temp_gpu,power_gpu,busy_gpu";

    /// Сводка строк `max_rows`, прочитанная порциями по `chunk` строк (как файл, который дописывается).
    fn max_digest(chunk: usize) -> Digest {
        let mut columns = Columns::parse(MAX_HEADER).expect("заголовок");
        columns.link(
            &["0".to_string(), "1".to_string(), "2".to_string()],
            &[vec![1], vec![0, 2], vec![1]],
        );
        let mut tally = Tally::default();
        let mut carry = samples::Carry::default();
        let mut scratch = Scratch::default();
        for part in max_rows().chunks(chunk) {
            scratch.count_keeping(&columns, &part.join("\n"), &mut carry, &mut tally, None);
        }
        Digest {
            tally,
            ..Digest::default()
        }
    }

    #[test]
    fn the_max_tooltip_names_the_row_of_the_maximum() {
        // Бриф W: у «макс» подсказка — условия строки наибольшей: дата и время, множитель и потолок, нагрузка, уровень
        // кристалла (средняя остальных блоков), температуры соседей, видеокарта и вентиляторы — ближайшее чтение не
        // дальше 2 с. Ловит: условия другой строки (последней, первой), чтение видеокарты «последнее до» вместо
        // ближайшего, не-соседа в списке соседей, потерю чтения после наибольшей, когда оно в следующей порции файла.
        let blocks = vec![block(0, "0", vec![0]), block(1, "1", vec![1]), block(2, "2", vec![2])];
        for chunk in [40, 22, 23] {
            let d = max_digest(chunk);
            let peak = d.tally.blocks["0"].bins[&45].peak;
            assert_eq!(peak.at % 86_400_000, 36_005_250, "порции по {chunk}: время строки");
            let w = peak.when;
            assert_eq!((w.ceiling, w.near[0], w.near[1]), (Some(46.0), Some((1, 63.0)), None));
            assert!(w.level.is_some_and(|l| (l - 59.0).abs() < 1e-9), "{:?}", w.level);
            assert_eq!(
                (w.gpu.temp, w.fans.cpu, w.fans.gpu),
                (Some(66.0), Some(4100.0), Some(4300.0)),
                "порции по {chunk}"
            );
            let s = stats_panel(&StatsInput {
                blocks: &blocks,
                compared: &[0],
                digest: Some(&d),
                scale: Scale::Absolute,
                curves: Curves::Both,
                max: true,
                ..StatsInput::default()
            });
            let tip = s
                .columns
                .iter()
                .find(|c| c.ratio == 45)
                .map(|c| c.tip.replace('\u{a0}', " "))
                .unwrap_or_default();
            for part in [
                "▲ макс 88 °C",
                "15.10 10:00:05",
                "×45.3 / потолок ×46",
                "нагрузка 92 %",
                "кристалл 59.0 °C",
                "соседи 1 63°",
                "видеокарта 66 °C, 120 Вт, 90 %",
                "вент. 4 100 / 4 300",
            ] {
                assert!(tip.contains(part), "нет «{part}»: {tip}");
            }
            assert!(!tip.contains("2 55°"), "не сосед в соседях: {tip}");
            let point = &s.series[0].points[0];
            assert!(point.my > point.y, "квадрат наибольшей выше точки средней: {point:?}");
        }
        // Без «макс» — прежняя короткая строка наибольшей, линии нет.
        let d = max_digest(40);
        let plain = stats_panel(&StatsInput {
            blocks: &blocks,
            compared: &[0],
            digest: Some(&d),
            scale: Scale::Absolute,
            curves: Curves::Both,
            ..StatsInput::default()
        });
        assert_eq!(plain.series[0].max, Vec::<(f32, String)>::new());
        assert!(!plain.columns.iter().any(|c| c.tip.contains("потолок")));
    }
}
