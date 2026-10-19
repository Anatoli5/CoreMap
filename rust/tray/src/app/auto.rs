// Автомодуль в программе входа (DECISIONS #31, #32; две отсечки — #40, подзапись «две отсечки», бриф
// outputs/cutoffs-brief-2026-10-16.md) и опрос раз в 2 с. Два независимых выключателя (владелец 16.10, DECISIONS #32,
// подзапись 16.10: «отдельно какой-то выключатель для логирования и отдельно выключатель для автоподстройки… это
// независимый процесс»): автомодуль — меняет ли он потолки, журнал срезов — пишутся ли строки.
//
// Один путь чтения датчиков: пока включён автомодуль или журнал срезов пишется, CoreMapTray читает первое ядро каждого
// блока раз в 0,25 с — опросом срезов (app/sense.rs). Регулятор (machine::auto) получает каждое показание этого опроса
// и сам датчиков не читает: аварийной отсечке нужно каждое показание. Опрос раз в 2 с (здесь) датчиков не читает: он
// решает, нужен ли опрос раз в 0,25 с (автомодуль включён — всегда; иначе — журнал срезов включён и процессор занят
// ≥ 10 % по Windows), пишутся ли строки (журнал включён и процессор занят), и сверяет потолки (чужая правка). Оба
// выключены — опроса нет, драйвер закрыт между действиями.
//
// Физические наблюдения в журнале отделены от контекста выставленных потолков. Возврат проверяет фактическую
// частоту и собственную/общую/относительную активность; совпадение команд потолков не объявляет физическое состояние
// повторённым. Старый журнал срезов читается отдельно для совместимости, неизвестные условия не достраиваются.
//
// Блоки не на профиле — «надстройка» над принятыми потолками (`intended`): принятые не меняются, надстройка помнит,
// какой блок куда опущен. Пока надстройка есть, программа держит аренду «CoreMap.Ceilings»: прежнее окно или движок,
// стартуя, ждут её, а эта программа, увидев их, возвращает принятые потолки и только тогда отпускает аренду — настройка
// всегда начинается с принятых, а не со сниженных. Метка data\auto-held.json (принятые потолки и опущенные блоки)
// переписывается при каждом изменении надстройки и убирается, когда всё вернулось.
//
// Исправлено по ревизии (outputs/review-auto-2026-10-14.md): метка поднимает при запуске только блоки, которые всё
// ещё стоят опущенными (после перезагрузки процессор сбросил потолки — метка ничего не значит, а поднять по ней можно
// было бы и отключённый после сбоя профиль, H1), и не выше заводского предела; несостоявшийся возврат оставляет блок
// опущенным, метку и аренду (M1); чужая правка — только по двум чтениям подряд, говорится о ней один раз, и этот блок
// автомодуль больше не трогает до смены набора (M3); сброс сном опрос не принимает за чужую правку — его ставит на
// место сверка после пробуждения (H3); одна и та же ошибка не повторяется каждые 2 с, а отказавший драйвер
// переоткрывается (M5).

use std::fmt::Write as _;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::time::{Duration, Instant};

use machine::auto::{Cause, Controller, Decision, Frame, LoadFrame, Protection, Settings, Timing};
use machine::clock::Stamp;
use machine::files;
use machine::memory::Memory;
use machine::thermal::{Reading, Thermal};
use serde_json::{Map, Value, json};

use super::auto_state;
use super::configurations::{Kind as PhysicalKind, Ledger, Rule, Trigger};
use super::{App, Device, Item, Other, Shell, at_boot, parts};
use crate::fans::{Fans, HpFans, SimFans};

/// Таймер опроса.
pub const AUTO: usize = 4;
/// Пункты подменю: включено; отсечки — открыть окно (там они настраиваются). Подтягивания выше профиля больше нет
/// (бриф T: его заменит оптимизатор; владелец его правила отверг, по умолчанию оно было выключено).
pub const AUTO_ON: u32 = 10;
pub const AUTO_WINDOW_ID: u32 = 12;
/// Пункт меню верхнего уровня «Журнал срезов» — свой выключатель, не в подменю автомодуля (владелец 16.10).
pub const LOG_ID: u32 = 13;
/// Целевая отсечка, °C, — ползунок в окне с шагом 1 °C (план DECISIONS #32, шаг 2; вместо пресетов 75–95 в меню).
pub const LIMIT_RANGE: RangeInclusive<i32> = 60..=95;
/// Аварийная отсечка по умолчанию, °C: на 5 °C ниже 100 °C, где его `ThrottleStop` сбрасывает частоту (`TjMax` 105,
/// смещение 5; KNOWLEDGE «Датчики»).
pub const EMERGENCY_DEFAULT: i32 = 95;
/// Аварийная отсечка — от целевой + 1 до этого, °C.
pub const EMERGENCY_TOP: i32 = 100;
/// Как часто писать в журнал цену опроса — так она измеряется на машине владельца.
pub(super) const COST_EVERY: Duration = Duration::from_secs(60);
/// Столько чтений подряд потолок блока должен расходиться с ожидаемым, чтобы считаться чужой правкой: одно чтение
/// бывает и запоздалым ответом на собственную запись, и чужим запросом в ящике (`ThrottleStop` не берёт наш замок).
const FOREIGN_READS: u8 = 2;
/// С такой доли работы всего процессора (по Windows) пишется журнал срезов (DECISIONS #32, подзапись 16.10). Простой
/// Windows — 1–5 %; число — выбор агента, не мерено. Автомодуля оно не касается: его опрос идёт всегда, пока он включён
/// (бриф T: условий нагрузки нет нигде).
const LOADED: f64 = 0.10;
/// Обороты для окна, пока журнал срезов не пишется, и после отказа прошивки — не чаще: каждый раз — вызов прошивки
/// (около 30 мс, замер на машине владельца 15.10), а отказавший может держать цикл программы до 4 с (fans.rs,
/// `PATIENCE_MS`): опрос журнала и состояния окна в это время стоят (ревизия 15.10, п. 4).
const FANS_EVERY: Duration = Duration::from_secs(10);
/// Прошивка не открылась — следующая попытка через столько.
const FANS_RETRY: Duration = Duration::from_secs(60);

/// Сроки автомодуля: опрос и сроки регулятора, опрос срезов и обороты при нём. Скрытая проверка их укорачивает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoTiming {
    /// Опрос раз в 2 с: нужен ли опрос срезов, пишутся ли строки, сверка потолков.
    pub tick: Duration,
    /// Опрос срезов — одно чтение датчиков для регулятора и строк журнала (выбор владельца 15.10, вариант А, — 0,25 с).
    pub sample: Duration,
    /// Обороты, пока пишется журнал: в строку того опроса, где прочитаны.
    pub fans: Duration,
    /// Видеокарта, пока пишется журнал: в строку того опроса, где прочитана (app/sense.rs, DECISIONS #37).
    pub gpu: Duration,
    pub control: Timing,
}

impl AutoTiming {
    pub const REAL: Self = Self {
        tick: Duration::from_secs(2),
        sample: Duration::from_millis(250),
        fans: Duration::from_secs(2),
        // 2 с — по замеру 15.10 во время его игры (DECISIONS #37): температура видеокарты обновляется раз в секунду и
        // за 2 с меняется на 1 °C (медиана; 90 % — до 2 °C, наибольшее 4); мощность и занятость — средние за
        // промежуток при любом шаге; путь её тепла к ядрам — общий охладитель, отклик 15–30 с (KNOWLEDGE): 2 с дают
        // на нём 8–15 точек.
        gpu: Duration::from_secs(2),
        control: Timing::REAL,
    };
}

/// Выбор владельца, как он лежит в data\tray.json. Глубины больше нет (владелец 15.10, DECISIONS #38), подтягивания
/// выше профиля — тоже (бриф T): ключи «depth» и «stepUp» файла прежней сборки читаются молча и больше не пишутся. Два
/// выключателя независимы (владелец 16.10: «отдельно какой-то выключатель для логирования и отдельно выключатель для
/// автоподстройки… это независимый процесс»): `on` — меняет ли автомодуль потолки, `log` — пишется ли журнал срезов.
/// Аварийная отсечка всегда выше целевой (бриф T: от целевой + 1 до 100 °C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// Автомодуль меняет потолки (по умолчанию выключен).
    pub on: bool,
    /// Целевая отсечка, °C.
    pub limit: i32,
    /// Аварийная отсечка, °C.
    pub emergency: i32,
    /// Журнал срезов data\samples (по умолчанию включён: файл прежней сборки без ключа «log» — тоже включён).
    pub log: bool,
    pub window_secs: i32,
    pub settle_secs: i32,
    pub return_gap: i32,
    pub return_secs: i32,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            on: false,
            limit: 85,
            emergency: EMERGENCY_DEFAULT,
            log: true,
            window_secs: 2,
            settle_secs: 2,
            return_gap: 0,
            return_secs: 120,
        }
    }
}

/// Целое из JSON в допустимом промежутке: 85.0, «85» и 96 — не годятся (шаг — 1 °C).
fn whole(value: &Value, range: &RangeInclusive<i32>) -> Option<i32> {
    value
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .filter(|v| range.contains(v))
}

/// Допустимая аварийная отсечка при целевой `limit`.
fn emergency_range(limit: i32) -> RangeInclusive<i32> {
    (limit + 1)..=EMERGENCY_TOP
}

impl Choice {
    /// Выбор из data\tray.json. Каждое поле — само по себе: негодное или пропавшее берёт значение по умолчанию, а
    /// годные соседние остаются (файл прежней сборки читается как есть, его «depth» и «stepUp» не замечаются).
    /// Аварийной нет или она не выше целевой — 95 °C, а если целевая 95 — на градус выше неё.
    #[must_use]
    pub fn read(map: &Map<String, Value>) -> Self {
        let default = Self::default();
        let limit = map
            .get("limit")
            .and_then(|v| whole(v, &LIMIT_RANGE))
            .unwrap_or(default.limit);
        Self {
            on: map.get("auto").and_then(Value::as_bool).unwrap_or(default.on),
            limit,
            emergency: map
                .get("emergency")
                .and_then(|v| whole(v, &emergency_range(limit)))
                .unwrap_or(EMERGENCY_DEFAULT.max(limit + 1)),
            log: map.get("log").and_then(Value::as_bool).unwrap_or(default.log),
            window_secs: map
                .get("windowSecs")
                .and_then(|v| whole(v, &(1..=60)))
                .unwrap_or(default.window_secs),
            settle_secs: map
                .get("settleSecs")
                .and_then(|v| whole(v, &(0..=120)))
                .unwrap_or(default.settle_secs),
            return_gap: map
                .get("returnGap")
                .and_then(|v| whole(v, &(0..=30)))
                .unwrap_or(default.return_gap),
            return_secs: map
                .get("returnSecs")
                .and_then(|v| whole(v, &(1..=600)))
                .unwrap_or(default.return_secs),
        }
    }

    /// Те же настройки автомодуля (выключатель и обе отсечки): смена журнала срезов их не касается.
    #[must_use]
    pub fn same_auto(self, other: Self) -> bool {
        Self { log: other.log, ..self } == other
    }

    fn physical_rule(self) -> Rule {
        Rule {
            limit: self.limit,
            emergency: self.emergency,
            window_secs: self.window_secs,
        }
    }

    fn timing(self, base: Timing) -> Timing {
        // Проверка ускоряет каждый срок относительно его прежнего реального значения.
        Timing {
            window: base.window.mul_f64(f64::from(self.window_secs) / 2.0),
            settle: base.settle.mul_f64(f64::from(self.settle_secs) / 2.0),
            pause: base.pause.mul_f64(f64::from(self.return_secs) / 120.0),
            ..base
        }
    }

    /// Команда окна «log.set» {on}: журнал срезов включён или нет. Отказ, если «on» нет, оно не да/нет или есть
    /// незнакомое поле (настройки автомодуля этой командой не меняются — у них своя, «auto.set»).
    pub fn log_changed(self, fields: &Map<String, Value>) -> Result<Self, String> {
        let mut next = None;
        for (key, value) in fields {
            match key.as_str() {
                "id" | "c" => {}
                "on" => next = Some(value.as_bool().ok_or("«on» — да или нет")?),
                other => return Err(format!("незнакомая настройка журнала срезов «{other}»")),
            }
        }
        next.map(|log| Self { log, ..self })
            .ok_or_else(|| "в команде нет «on»".into())
    }

    /// Команда окна «auto.set»: какие поля пришли — те и меняются, остальные прежние. Отказ, если хоть одно поле
    /// негодно или незнакомо (опечатка в имени не должна молча ничего не сделать; «depth» и «stepUp» прежнего окна —
    /// тоже), или настроек в команде нет. Аварийная проверяется по новой целевой. Пришла только целевая, и она не ниже
    /// аварийной — аварийная поднимается на градус выше неё: последнее слово владельца — целевая, а аварийная по правилу
    /// всегда выше (окно показывает новую темой settings).
    /// `id` и `c` — номер и имя самой команды, не настройки.
    pub fn changed(self, fields: &Map<String, Value>) -> Result<Self, String> {
        let mut next = self;
        let mut any = false;
        let mut emergency = None;
        for (key, value) in fields {
            match key.as_str() {
                "id" | "c" => continue,
                "on" => next.on = value.as_bool().ok_or("«on» — да или нет")?,
                "limit" => {
                    next.limit = whole(value, &LIMIT_RANGE).ok_or_else(|| {
                        format!(
                            "целевая отсечка — целое число °C от {} до {}",
                            LIMIT_RANGE.start(),
                            LIMIT_RANGE.end()
                        )
                    })?;
                }
                "emergency" => emergency = Some(value),
                "windowSecs" => {
                    next.window_secs =
                        whole(value, &(1..=60)).ok_or("окно среднего — целое число секунд от 1 до 60")?;
                }
                "settleSecs" => {
                    next.settle_secs =
                        whole(value, &(0..=120)).ok_or("пауза установления — целое число секунд от 0 до 120")?;
                }
                "returnGap" => {
                    next.return_gap = whole(value, &(0..=30)).ok_or("запас возврата — целое число °C от 0 до 30")?;
                }
                "returnSecs" => {
                    next.return_secs =
                        whole(value, &(1..=600)).ok_or("проверка возврата — целое число секунд от 1 до 600")?;
                }
                other => return Err(format!("незнакомая настройка автомодуля «{other}»")),
            }
            any = true;
        }
        if !any {
            return Err("в команде нет настроек автомодуля".into());
        }
        let range = emergency_range(next.limit);
        match emergency {
            Some(value) => {
                next.emergency = whole(value, &range).ok_or_else(|| {
                    format!(
                        "аварийная отсечка — целое число °C от {} (целевая + 1) до {}",
                        range.start(),
                        range.end()
                    )
                })?;
            }
            None if !range.contains(&next.emergency) => next.emergency = next.limit + 1,
            None => {}
        }
        Ok(next)
    }

    /// Как выбор лежит в data\tray.json.
    #[must_use]
    pub fn file_text(self) -> String {
        json!({ "auto": self.on, "limit": self.limit, "emergency": self.emergency, "log": self.log,
            "windowSecs": self.window_secs, "settleSecs": self.settle_secs,
            "returnGap": self.return_gap, "returnSecs": self.return_secs })
        .to_string()
    }

    /// Тема окну «settings»: выбор и допустимые отсечки — их держит эта программа, окно по ним строит ползунки;
    /// журнал срезов — отдельным полем, со своей командой «log.set».
    #[must_use]
    pub fn topic(self) -> String {
        let range = emergency_range(self.limit);
        json!({
            "t": "settings",
            "auto": {
                "on": self.on, "limit": self.limit, "emergency": self.emergency,
                "limitRange": [LIMIT_RANGE.start(), LIMIT_RANGE.end()],
                "emergencyRange": [range.start(), range.end()],
                "windowSecs": self.window_secs, "settleSecs": self.settle_secs,
                "returnGap": self.return_gap, "returnSecs": self.return_secs,
                "windowSecsRange": [1, 60], "settleSecsRange": [0, 120],
                "returnGapRange": [0, 30], "returnSecsRange": [1, 600],
            },
            "log": { "on": self.log },
        })
        .to_string()
    }
}

/// Состояние автомодуля.
#[derive(Debug)]
pub struct Autopilot {
    pub choice: Choice,
    pub(super) physical: Ledger,
    pub(super) physical_poll: Vec<Option<Reading>>,
    pub(super) physical_relative: Vec<Option<f64>>,
    physical_ceilings: Vec<Option<i32>>,
    physical_poll_snapshot: Option<Rc<Value>>,
    physical_window: Vec<Option<Reading>>,
    physical_window_relative: Vec<Option<f64>>,
    physical_window_snapshot: Option<Rc<Value>>,
    physical_span: Option<(Duration, Duration)>,
    pub(super) stats_saved: Option<Instant>,
    pub(super) stats_dirty: bool,
    pub(super) controller: Option<Controller>,
    /// По блокам: потолок, поставленный автомодулем (ниже принятого); None — блок на принятом. Окно видит его в
    /// состоянии (app/serve.rs, поле auto).
    pub(super) overlay: Vec<Option<i32>>,
    /// Память срезов.
    memory: Memory,
    /// Время процессоров по Windows на прошлом опросе: (простой, всего).
    system: Option<(u64, u64)>,
    /// Последнее действительное измерение Windows; первое и неудавшееся — неизвестны.
    pub(super) global_busy: Option<f64>,
    legacy: Vec<Option<auto_state::LegacyCut>>,
    fans: Option<Box<dyn Fans>>,
    /// Когда обороты читались (или открывалась прошивка) последний раз; следующий раз — через срок, который зависит
    /// от того, пишутся ли строки журнала срезов и чем кончился прошлый раз.
    fans_last: Option<Instant>,
    /// Чем кончился прошлый раз; None — обороты прочитаны (или ещё не спрашивались).
    fans_fault: Option<FanFault>,
    /// Последние прочитанные обороты; None — не известны. Их же видит окно.
    pub(super) rpm: Option<Vec<u32>>,
    fans_said: Option<String>,
    /// Скрытая проверка: сколько раз обороты спрошены.
    pub(super) fan_calls: u64,
    /// Скрытая проверка: сколько сообщений таймера опроса пришло.
    pub(super) ticks: u64,
    /// По блокам: чужая правка принята — автомодуль этот блок не трогает до смены набора.
    aside: Vec<bool>,
    /// По блокам: сколько чтений подряд потолок расходится с ожидаемым.
    mismatch: Vec<u8>,
    /// Защита прочитана, но процессор ещё не подтвердил восстановление потолка.
    recover_pending: Vec<bool>,
    /// Аренда держится этой программой, пока есть опущенный блок.
    holding: bool,
    /// Пробуждение отмечено, сверка после него ещё впереди: опрос ждёт её (ревизия H3).
    pub wake_pending: bool,
    /// Буферы одного решения регулятора — на все опросы срезов (4 раза в секунду): показания и потолки по блокам,
    /// решения.
    frame: FrameBuffers,
    decisions: Vec<Decision>,
    origin: Instant,
    last_tick: Option<Instant>,
    last_cost: Option<Instant>,
    pub(super) last_said: Option<String>,
    last_tip: String,
}

/// Буферы кадра регулятора (`Frame`): заполняются заново в каждом опросе срезов.
#[derive(Debug, Default)]
struct FrameBuffers {
    readings: Vec<Option<Reading>>,
    context: Vec<Option<Reading>>,
    ceilings: Vec<i32>,
    accepted: Vec<i32>,
    floor: Vec<i32>,
    relative: Vec<Option<f64>>,
    prior: Vec<Option<Protection>>,
}

impl Autopilot {
    #[must_use]
    pub fn new(blocks: usize) -> Self {
        Self {
            choice: Choice::default(),
            physical: Ledger::default(),
            physical_poll: Vec::with_capacity(blocks),
            physical_relative: Vec::with_capacity(blocks),
            physical_ceilings: Vec::with_capacity(blocks),
            physical_poll_snapshot: None,
            physical_window: Vec::with_capacity(blocks),
            physical_window_relative: Vec::with_capacity(blocks),
            physical_window_snapshot: None,
            physical_span: None,
            stats_saved: None,
            stats_dirty: false,
            controller: None,
            overlay: vec![None; blocks],
            memory: Memory::default(),
            system: None,
            global_busy: None,
            legacy: vec![None; blocks],
            fans: None,
            fans_last: None,
            fans_fault: None,
            rpm: None,
            fans_said: None,
            fan_calls: 0,
            ticks: 0,
            aside: vec![false; blocks],
            mismatch: vec![0; blocks],
            recover_pending: vec![false; blocks],
            holding: false,
            wake_pending: false,
            frame: FrameBuffers {
                readings: Vec::with_capacity(blocks),
                context: Vec::with_capacity(blocks),
                ceilings: Vec::with_capacity(blocks),
                accepted: Vec::with_capacity(blocks),
                floor: Vec::with_capacity(blocks),
                relative: Vec::with_capacity(blocks),
                prior: Vec::with_capacity(blocks),
            },
            decisions: Vec::with_capacity(blocks),
            origin: Instant::now(),
            last_tick: None,
            last_cost: None,
            last_said: None,
            last_tip: String::new(),
        }
    }

    #[must_use]
    pub fn active(&self) -> bool {
        self.overlay.iter().any(Option::is_some)
            || self
                .controller
                .as_ref()
                .is_some_and(|c| (0..self.overlay.len()).any(|b| c.protection(b).is_some_and(|p| p.trial.is_some())))
    }
}

impl Device {
    pub(super) fn thermal(&mut self) -> &mut dyn Thermal {
        match self {
            Self::Real(cpu) => cpu,
            Self::Sim(cpu) => cpu,
        }
    }
}

fn profile_snapshot(
    readings: &[Option<Reading>],
    relative: &[Option<f64>],
    global_busy: Option<f64>,
    ceilings: &[Option<i32>],
    span: Option<(Duration, Duration)>,
    at: Duration,
) -> Value {
    let recorded = Stamp::now();
    let mut physical = auto_state::physical_value(readings, relative, global_busy);
    if let Some((start, end)) = span {
        physical["sampling"] = json!("completed-controller-window");
        physical["ratioBasis"] = json!("busy-weighted-window-average");
        physical["tempBasis"] = json!("arithmetic-mean-valid-polls");
        physical["busyBasis"] = json!("arithmetic-mean-known-polls");
        physical["startControllerSecs"] = json!(start.as_secs_f64());
        physical["endControllerSecs"] = json!(end.as_secs_f64());
    }
    json!({ "physical": physical, "ceilingsBefore": ceilings, "recordedControllerSecs": at.as_secs_f64(),
        "recordedAt": recorded.roundtrip(), "recordedTicks": recorded.0 })
}

/// Что сказал опрос раз в 2 с.
enum Polled {
    Quiet,
    Said(String),
    /// Потолки сброшены (сон): поставить их на место — дело сверки после пробуждения.
    Reset,
}

/// Отказ оборотов: от него — когда спрашивать снова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanFault {
    /// Прошивка открыта, но ответила ошибкой или не ответила: снова через `FANS_EVERY`.
    Read,
    /// Прошивка не открылась: снова через `FANS_RETRY`.
    Open,
}

impl App {
    pub(super) fn observe_physical_poll(&mut self) {
        let auto = &mut self.auto;
        auto.physical_poll.clear();
        auto.physical_poll
            .extend(self.topology.blocks.iter().map(|b| Some(self.meter.latest(b.first))));
        auto.physical_ceilings.clear();
        auto.physical_ceilings.extend(self.topology.blocks.iter().map(|b| {
            self.limits
                .as_ref()
                .filter(|limits| limits.known(b.first) && limits.answered(b.first))
                .map(|limits| limits.current()[b.first])
        }));
        auto_state::relative_activity(&self.topology.blocks, &auto.physical_poll, &mut auto.physical_relative);
        auto.physical_poll_snapshot = auto.physical.observe(PhysicalKind::Instant, &auto.physical_poll, || {
            profile_snapshot(
                &auto.physical_poll,
                &auto.physical_relative,
                auto.global_busy,
                &auto.physical_ceilings,
                None,
                auto.origin.elapsed(),
            )
        });
    }

    fn observe_physical_window(&mut self) {
        let auto = &mut self.auto;
        let Some(controller) = &auto.controller else { return };
        let span = controller.window_span();
        if span.is_none() || span == auto.physical_span {
            return;
        }
        auto.physical_span = span;
        auto.physical.set_window_secs(auto.choice.window_secs);
        auto.physical_window.clear();
        auto.physical_window.extend(
            controller
                .window_readings()
                .map(|r| r.temp.filter(|t| t.is_finite()).map(|_| r)),
        );
        auto_state::relative_activity(
            &self.topology.blocks,
            &auto.physical_window,
            &mut auto.physical_window_relative,
        );
        auto.physical_window_snapshot = auto.physical.observe(PhysicalKind::Window, &auto.physical_window, || {
            profile_snapshot(
                &auto.physical_window,
                &auto.physical_window_relative,
                None,
                &auto.physical_ceilings,
                span,
                auto.origin.elapsed(),
            )
        });
    }

    fn prepare_physical_failure_sources(&mut self) {
        let auto = &mut self.auto;
        if auto.decisions.iter().any(|d| d.cause == Cause::Emergency) && auto.physical_poll_snapshot.is_none() {
            auto.physical_poll_snapshot = Some(Rc::new(profile_snapshot(
                &auto.physical_poll,
                &auto.physical_relative,
                auto.global_busy,
                &auto.physical_ceilings,
                None,
                auto.origin.elapsed(),
            )));
        }
        if auto.decisions.iter().any(|d| d.cause == Cause::Target)
            && auto.physical_window_snapshot.is_none()
            && auto.physical_span.is_some()
            && auto.physical_span == auto.controller.as_ref().and_then(Controller::window_span)
        {
            auto.physical_window_snapshot = Some(Rc::new(profile_snapshot(
                &auto.physical_window,
                &auto.physical_window_relative,
                None,
                &auto.physical_ceilings,
                auto.physical_span,
                auto.origin.elapsed(),
            )));
        }
    }

    fn remember_physical_failure(&mut self, decision: &Decision) {
        let kind = match decision.cause {
            Cause::Emergency => PhysicalKind::Instant,
            Cause::Target => PhysicalKind::Window,
            _ => return,
        };
        let auto = &mut self.auto;
        let (readings, cached) = match kind {
            PhysicalKind::Instant => (&auto.physical_poll, &auto.physical_poll_snapshot),
            PhysicalKind::Window => {
                if auto.physical_span != auto.controller.as_ref().and_then(Controller::window_span) {
                    return;
                }
                (&auto.physical_window, &auto.physical_window_snapshot)
            }
        };
        let Some(snapshot) = cached.as_ref().map(Rc::clone) else {
            auto.physical.blocked = true;
            self.journal
                .error("тепловая коррекция без исходного физического снимка — автовозврат приостановлен");
            self.serving
                .notice("Автовозврат приостановлен: исходный физический снимок коррекции отсутствует");
            return;
        };
        auto.physical.failure(
            Trigger {
                block: decision.block,
                cause: decision.cause,
                kind,
                ratio: decision.ratio,
            },
            readings,
            auto.choice.physical_rule(),
            snapshot,
        );
    }

    fn settings_path(&self) -> std::path::PathBuf {
        self.config.data.join("tray.json")
    }

    fn marker_path(&self) -> std::path::PathBuf {
        self.config.data.join("auto-held.json")
    }

    fn log_path(&self) -> std::path::PathBuf {
        self.config.data.join("auto-log.jsonl")
    }

    /// Выбор владельца из data\tray.json; нет файла или он не читается — выключено (по умолчанию).
    pub(super) fn load_choice(&mut self) {
        match files::read_object(&self.settings_path()) {
            Ok(Some(map)) => self.auto.choice = Choice::read(&map),
            Ok(None) => {}
            Err(spoiled) => self
                .journal
                .error(&format!("настройки автомодуля не прочитаны: {spoiled}")),
        }
    }

    fn save_choice(&self) {
        if let Err(e) = files::write_replacing(&self.settings_path(), &self.auto.choice.file_text()) {
            self.journal.error(&format!("настройки автомодуля не записаны: {e}"));
        }
    }

    pub(super) fn controller_for(&self) -> Controller {
        let c = self.auto.choice;
        let mut controller = Controller::new(
            Settings {
                limit: f64::from(c.limit),
                emergency: f64::from(c.emergency),
                return_gap: f64::from(c.return_gap),
            },
            c.timing(self.config.auto.control),
            self.topology.blocks.len(),
        );
        if let Some(previous) = &self.auto.controller {
            for b in 0..self.topology.blocks.len() {
                if let Some(protection) = previous.protection(b) {
                    controller.seed_protection(b, protection);
                }
            }
        }
        controller
    }

    /// Запуск программы: метка автомодуля от прошлого запуска — вернуть принятые опущенным блокам; опрос раз в 2 с —
    /// если включён автомодуль или журнал срезов (выключатели независимы), регулятор — если включён автомодуль.
    pub(super) fn start_auto(&mut self, shell: &Shell) {
        self.load_choice();
        self.replay_memory();
        if self.auto.choice.on {
            self.auto.controller = Some(self.controller_for());
        }
        if self.marker_path().exists() || self.auto.choice.on {
            let said = self.with_ceilings(App::recover_auto);
            if let Ok(Some(said)) = said {
                self.queue(&said);
            }
        }
        let choice = self.auto.choice;
        if choice.on || choice.log {
            shell.set_timer(AUTO, self.config.auto.tick);
        }
        if !choice.log {
            self.journal.note("журнал срезов выключен");
        }
        if choice.on {
            self.journal.note(&format!(
                "автомодуль включён: до {} °C, аварийная {} °C",
                choice.limit, choice.emergency
            ));
        }
    }

    /// Память срезов из журнала действий: записанные срезы повторяются по порядку. Испорченные строки пропускаются —
    /// говорится об этом одной строкой журнала программы. Срезы прежних сборок («lower», до двух отсечек) уровня
    /// кристалла не знают: их запись пускает возврат при любом уровне (бриф T).
    pub(crate) fn replay_memory(&mut self) {
        let mut memory = Memory::default();
        let text = match std::fs::read_to_string(self.log_path()) {
            Ok(text) => text,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    self.journal.error(&format!("журнал автомодуля не прочитан: {e}"));
                }
                self.auto.memory = memory;
                return;
            }
        };
        self.auto.legacy = auto_state::legacy_cuts(&text, &self.topology.blocks);
        let mut bad = 0_usize;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(Value::Object(map)) = serde_json::from_str::<Value>(line) else {
                bad += 1;
                continue;
            };
            // Какое поле — температура среза: аварийная — показание, целевая — медиана, прежняя — средняя за окно.
            let temp_key = match map.get("act").and_then(Value::as_str) {
                Some("emergency") => "reading",
                Some("target") if map.get("policy").and_then(Value::as_str) == Some("manual-mean-config-v1") => "mean",
                Some("target") => "median",
                Some("lower") => "mean",
                _ => continue,
            };
            let first = map
                .get("first")
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok());
            let from = map
                .get("from")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            let temp = map.get(temp_key).and_then(Value::as_f64);
            let die = map.get("die").and_then(Value::as_f64);
            match (first, from, temp) {
                (Some(first), Some(from), Some(temp)) => memory.cut(first, from, temp, die),
                _ => bad += 1,
            }
        }
        if bad > 0 {
            self.journal.error(&format!(
                "журнал автомодуля: {bad} строк не разобрано, память срезов без них"
            ));
        }
        self.auto.memory = memory;
    }

    /// Текст метки: процессор, раскладка, принятые потолки и надстройка (первое ядро блока → потолок автомодуля).
    fn marker_text(&self) -> String {
        let lps: Vec<u32> = self.store.lps().iter().map(|lp| lp.0).collect();
        let accepted = self.intended.clone().unwrap_or_default(); // a few dozen numbers per overlay change
        let overlay: Vec<i32> = self
            .topology
            .cores
            .iter()
            .enumerate()
            .map(|(i, _)| {
                self.topology
                    .blocks
                    .iter()
                    .position(|b| b.first == i)
                    .and_then(|b| self.auto.overlay[b])
                    .unwrap_or(0)
            })
            .collect();
        let protection: Vec<Value> = (0..self.topology.blocks.len())
            .map(|b| auto_state::protection_value(self.auto.controller.as_ref().and_then(|c| c.protection(b))))
            .collect();
        json!({"cpu": self.store.cpu(), "lps": lps, "accepted": accepted,
            "auto": overlay, "returnPolicy": 1, "protection": protection})
        .to_string()
    }

    /// Переписать метку по надстройке; надстройки нет — убрать метку и отпустить аренду.
    pub(super) fn mark(&mut self) -> Result<(), String> {
        if !self.auto.active() {
            if self.auto.choice.on {
                files::write_replacing(&self.marker_path(), &self.marker_text()).map_err(|e| e.to_string())?;
            } else if let Err(e) = files::delete_with_backup(&self.marker_path()) {
                self.journal.error(&format!("метка автомодуля не удалена: {e}"));
            }
            self.release_auto_lease();
            return Ok(());
        }
        files::write_replacing(&self.marker_path(), &self.marker_text()).map_err(|e| e.to_string())?;
        if !self.auto.holding && self.lease.acquire(0) == machine::named::Wait::Held {
            self.auto.holding = true;
        }
        Ok(())
    }

    /// Метка осталась — программа упала или её закрыли, пока надстройка была. Принятые возвращаются только блокам,
    /// которые всё ещё стоят на значении автомодуля (после перезагрузки процессор сбросил потолки, и метка ничего не
    /// значит), и не выше заводского предела. Метка прежней сборки звала надстройку «lowered».
    pub(super) fn recover_auto(&mut self) -> Option<String> {
        self.recover_auto_mode(true)
    }

    pub(super) fn recover_auto_mode(&mut self, allow_reset: bool) -> Option<String> {
        let path = self.marker_path();
        let read = files::read_object(&path).ok().flatten().filter(|map| {
            map.get("cpu").and_then(serde_json::Value::as_str) == Some(self.store.cpu())
                && files::read_ints(map.get("lps")) == Some(self.store.lps().iter().map(|lp| i64::from(lp.0)).collect())
        });
        if self.auto.choice.on {
            return self.recover_held(read.as_ref(), allow_reset);
        }
        let accepted = read.as_ref().and_then(|map| files::read_ints(map.get("accepted")));
        let lowered = read
            .as_ref()
            .and_then(|map| files::read_ints(map.get("auto").or_else(|| map.get("lowered"))));
        let mut back = Vec::new();
        if let (Some(accepted), Some(lowered)) = (accepted, lowered) {
            let topology = &self.topology;
            if let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
                for block in &topology.blocks {
                    let i = block.first;
                    let value = |list: &[i64]| list.get(i).and_then(|&v| i32::try_from(v).ok()).unwrap_or(0);
                    let (want, low) = (value(&accepted), value(&lowered));
                    let still_ours = low > 0 && limits.answered(i) && limits.current()[i] == low;
                    let want = want.min(limits.fused()[i]);
                    if still_ours && want != low && limits.set(cpu, topology, i, want).is_none() {
                        back.push(format!("{} ×{want}", block.name));
                    }
                }
            }
        }
        if let Err(e) = files::delete_with_backup(&path) {
            self.journal.error(&format!("метка автомодуля не удалена: {e}"));
        }
        self.intended = self.limits.as_ref().map(|l| l.current().to_vec());
        (!back.is_empty()).then(|| {
            let text = format!(
                "Автомодуль в прошлый раз не успел вернуть потолки — возвращены: {}",
                back.join(", ")
            );
            self.journal.note(&text);
            text
        })
    }

    pub(super) fn retained_auto(&self) -> bool {
        self.auto.choice.on
            && files::read_object(&self.marker_path()).ok().flatten().is_some_and(|m| {
                m.get("cpu").and_then(Value::as_str) == Some(self.store.cpu())
                    && files::read_ints(m.get("lps"))
                        == Some(self.store.lps().iter().map(|lp| i64::from(lp.0)).collect())
                    && (files::read_ints(m.get("auto").or_else(|| m.get("lowered")))
                        .is_some_and(|v| v.iter().any(|v| *v > 0))
                        || m.get("protection")
                            .and_then(Value::as_array)
                            .is_some_and(|v| v.iter().any(|v| auto_state::protection_read(v).is_some())))
            })
    }

    /// При включённом авто перезапуск не отменяет тепловое ограничение. Непроверенный шаг откатывается.
    fn recover_held(&mut self, saved: Option<&Map<String, Value>>, allow_reset: bool) -> Option<String> {
        let mut base = self.intended.clone().unwrap_or_default(); // once at recovery, not per sample
        let saved_base = saved.and_then(|m| files::read_ints(m.get("accepted")));
        let saved_auto = saved.and_then(|m| files::read_ints(m.get("auto").or_else(|| m.get("lowered"))));
        let policy = saved.and_then(|m| m.get("returnPolicy")).and_then(Value::as_u64) == Some(1);
        let profile = std::fs::read_to_string(self.config.data.join("profile-log.jsonl")).ok();
        let mut restored = Vec::new();
        self.auto.overlay.fill(None);
        self.auto.recover_pending.fill(false);
        for b in 0..self.topology.blocks.len() {
            if let Some(controller) = &mut self.auto.controller {
                controller.clear_protection(b);
            }
            let block = &self.topology.blocks[b];
            let i = block.first;
            let Some(limits) = self.limits.as_ref() else { continue };
            let readable = limits.known(i) && limits.answered(i);
            let current = limits.current()[i];
            let old_auto = auto_state::list_value(saved_auto.as_deref(), i).filter(|v| *v > 0);
            let accepted = auto_state::list_value(saved_base.as_deref(), i).unwrap_or(current);
            let accepted = if limits.known(i) {
                accepted.min(limits.fused()[i])
            } else {
                accepted
            };
            let stored = saved
                .and_then(|m| m.get("protection"))
                .and_then(Value::as_array)
                .and_then(|v| v.get(b))
                .and_then(auto_state::protection_read);
            let legacy = self.auto.legacy[b].filter(|cut| {
                !policy
                    && cut.accepted == accepted
                    && profile
                        .as_ref()
                        .is_some_and(|text| auto_state::no_owner_change(text, i, *cut))
            });
            let protected = stored
                .or_else(|| {
                    old_auto.map(|held| Protection {
                        held,
                        reference: None,
                        trial: None,
                    })
                })
                .or_else(|| {
                    legacy.map(|cut| Protection {
                        held: cut.to,
                        reference: None,
                        trial: None,
                    })
                });
            let Some(mut protection) = protected else { continue };
            if accepted < limits.floor(i) {
                continue;
            }
            if limits.known(i) {
                protection.held = protection.held.clamp(limits.floor(i), accepted);
            }
            if protection.held >= accepted {
                continue;
            }
            let ours = !readable
                || old_auto == Some(current)
                || (allow_reset && (current == accepted || at_boot(limits, &self.boot, i)));
            if !ours {
                continue;
            } // foreign/manual hardware value wins
            protection.trial = None;
            let held = protection.held;
            for &m in &block.members {
                if let Some(value) = base.get_mut(m) {
                    *value = accepted;
                }
            }
            self.auto.overlay[b] = Some(held);
            self.auto.recover_pending[b] = true;
            if let Some(controller) = &mut self.auto.controller {
                controller.seed_protection(b, protection);
            }
            let topology = &self.topology;
            if readable && let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
                if let Some(refusal) = limits.set(cpu, topology, i, held) {
                    self.journal.error(&format!(
                        "удержание потолка {} ×{held}: {refusal}; повторю сверку",
                        block.name
                    ));
                } else {
                    self.auto.recover_pending[b] = false;
                }
            }
            restored.push(format!("{} ×{held}", block.name));
        }
        self.intended = Some(base);
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        (!restored.is_empty()).then(|| format!("Тепловые ограничения сохранены: {}", restored.join(", ")))
    }

    pub(super) fn release_auto_lease(&mut self) {
        if self.auto.holding {
            self.auto.holding = false;
            self.lease.release();
        }
    }

    /// Автоматическая пауза не является проверкой работы и не возвращает исходные потолки.
    pub(super) fn preserve_auto(&mut self, why: &str) {
        let mut rollbacks = Vec::new();
        if let Some(controller) = &self.auto.controller {
            for b in 0..self.topology.blocks.len() {
                if let Some(mut protection) = controller.protection(b)
                    && protection.trial.is_some()
                {
                    protection.trial = None;
                    rollbacks.push((b, protection));
                }
            }
        }
        for (b, protection) in rollbacks {
            let topology = &self.topology;
            let block = &topology.blocks[b];
            if let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
                if let Some(refusal) = limits.set(cpu, topology, block.first, protection.held) {
                    self.journal
                        .error(&format!("непроверенный шаг {} не отменён: {refusal}", block.name));
                } else {
                    self.auto.overlay[b] = Some(protection.held);
                    if let Some(controller) = &mut self.auto.controller {
                        controller.seed_protection(b, protection);
                    }
                }
            }
        }
        if let Some(controller) = &mut self.auto.controller {
            controller.forget();
        }
        self.auto.system = None;
        self.auto.global_busy = None;
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        self.journal
            .note(&format!("автомодуль: {why} — тепловые ограничения сохранены"));
    }

    /// Подменю «Автоограничение нагрева».
    pub(super) fn auto_menu(&self) -> Item {
        let c = self.auto.choice;
        let item = |id: u32, text: String, checked: bool| Item {
            id,
            text: Some(text),
            checked,
            enabled: true,
            default: false,
            children: Vec::new(),
        };
        // Отсечки — ползунками в окне (владелец 15.10: пресеты «место занимают только в меню»); здесь — что стоит, и
        // пункт, который открывает окно. Включение остаётся: им владелец пользуется. Подтягивания больше нет (бриф T).
        let children = vec![
            item(AUTO_ON, "Включено".into(), c.on),
            super::separator(),
            item(
                AUTO_WINDOW_ID,
                format!(
                    "Отсечка {} °C, аварийная {} °C — настроить в окне",
                    c.limit, c.emergency
                ),
                false,
            ),
        ];
        let state = if c.on {
            format!("вкл, до {} °C", c.limit)
        } else {
            "выкл".into()
        };
        Item {
            id: 0,
            text: Some(format!("Автоограничение нагрева · {state}")),
            checked: false,
            enabled: true,
            default: false,
            children,
        }
    }

    /// Пункт меню верхнего уровня «Журнал срезов» с отметкой: свой выключатель, не в подменю автомодуля (владелец 16.10:
    /// «это разные процессы»).
    pub(super) fn log_menu(&self) -> Item {
        Item {
            id: LOG_ID,
            text: Some("Журнал срезов".into()),
            checked: self.auto.choice.log,
            enabled: true,
            default: false,
            children: Vec::new(),
        }
    }

    /// Пункт подменю автомодуля или «Журнал срезов». true — пункт разобран здесь.
    pub(super) fn auto_command(&mut self, shell: &mut Shell, id: u32) -> bool {
        let mut next = self.auto.choice;
        match id {
            AUTO_ON => next.on = !next.on,
            LOG_ID => next.log = !next.log,
            AUTO_WINDOW_ID => {
                self.open_window(shell);
                return true;
            }
            _ => return false,
        }
        self.apply_choice(shell, next, "меню");
        true
    }

    /// Выбор владельца, как он сейчас (скрытая проверка).
    #[must_use]
    pub fn auto_choice(&self) -> Choice {
        self.auto.choice
    }

    /// Команда окна «auto.set» (app/serve.rs) — тот же путь, что пункты меню: проверка, применение, файл, журнал.
    /// Возвращает отказ; None — принято (или ничего не изменилось — тогда ни записи, ни возврата потолков).
    pub fn set_auto(&mut self, shell: &Shell, fields: &Map<String, Value>) -> Option<String> {
        let next = self.auto.choice.changed(fields);
        self.set_choice(shell, next)
    }

    /// Команда окна «log.set» {on} (app/serve.rs, выключатель «запись» в окне) — тот же путь, что пункт меню.
    pub fn set_log(&mut self, shell: &Shell, fields: &Map<String, Value>) -> Option<String> {
        let next = self.auto.choice.log_changed(fields);
        self.set_choice(shell, next)
    }

    fn set_choice(&mut self, shell: &Shell, next: Result<Choice, String>) -> Option<String> {
        match next {
            Err(why) => Some(why),
            Ok(next) => {
                if next != self.auto.choice {
                    self.apply_choice(shell, next, "окно");
                }
                None
            }
        }
    }

    /// Новый выбор владельца — единственный путь его смены (меню и окно): регулятор есть или нет, возврат опущенных
    /// блоков, журнал срезов, опрос раз в 2 с, data\tray.json, строка журнала. Выключатели независимы (владелец 16.10):
    /// автомодуль — только про изменение множителей, журнал срезов — только про запись; смена одного другой не трогает.
    /// Окно узнаёт о выборе темой «settings» (app/serve.rs, по изменению) и полем autoLimit состояния.
    fn apply_choice(&mut self, shell: &Shell, next: Choice, by: &str) {
        let was = std::mem::replace(&mut self.auto.choice, next);
        if !next.on && was.on {
            self.put_back("автомодуль выключен", Some("auto-off"));
            self.auto.controller = None;
            if let Err(e) = self.mark() {
                self.journal.error(&format!("метка автомодуля: {e}"));
            }
        }
        if next.on && !next.same_auto(was) {
            // Новый выбор — опущенные возвращаются, регулятор начинает заново (прежние окна — при другой отсечке).
            // Сохранить надстройку через смену отсечки мог бы только сам регулятор (machine::auto) — не здесь.
            self.preserve_auto("новые настройки автомодуля");
            self.auto.controller = Some(self.controller_for());
            self.auto.aside.fill(false);
        }
        if !next.log {
            // Журнал выключен — строки и файл дня кончаются сразу, а не со следующим опросом раз в 2 с.
            self.set_rows(false);
        }
        if !next.on && !next.log {
            // Ни регулятору, ни журналу опрос раз в 0,25 с не нужен.
            self.stop_sensing(shell);
        }
        // Опрос раз в 2 с нужен регулятору или журналу срезов; ни тому ни другому — его нет.
        let polled = |c: Choice| c.on || c.log;
        if polled(next) && !polled(was) {
            shell.set_timer(AUTO, self.config.auto.tick);
        } else if !polled(next) && polled(was) {
            shell.kill_timer(AUTO);
        }
        // Драйвер нужен ещё журналу срезов и окну, если они идут; иначе — отпустить.
        self.release_idle();
        self.save_choice();
        if !next.same_auto(was) {
            self.journal.note(&format!(
                "автомодуль ({by}): {}, до {} °C, аварийная {} °C",
                if next.on { "включён" } else { "выключен" },
                next.limit,
                next.emergency
            ));
        }
        if next.log != was.log {
            self.journal.note(&format!(
                "журнал срезов ({by}): {}",
                if next.log { "включён" } else { "выключен" }
            ));
        }
    }

    /// Владелец поставил блоку потолок сам (окно): регулятор судит этот блок заново, отставленный из-за чужой правки —
    /// снова под присмотром.
    pub(super) fn auto_block_fresh(&mut self, b: usize) {
        if let Some(pending) = self.auto.recover_pending.get_mut(b) {
            *pending = false;
        }
        if let Some(aside) = self.auto.aside.get_mut(b) {
            *aside = false;
        }
        if let Some(mismatch) = self.auto.mismatch.get_mut(b) {
            *mismatch = 0;
        }
        if let Some(controller) = &mut self.auto.controller {
            controller.forget_block(b);
        }
    }

    /// Смена набора: принятые потолки — новые, блоки, отставленные из-за чужой правки, снова под присмотром.
    pub(super) fn auto_new_set(&mut self) {
        self.auto.aside.fill(false);
        self.auto.mismatch.fill(0);
        self.auto.recover_pending.fill(false);
        if let Some(controller) = &mut self.auto.controller {
            for b in 0..self.topology.blocks.len() {
                controller.forget_block(b);
            }
        }
        if let Err(e) = self.mark() {
            self.journal
                .error(&format!("новый профиль: защита в метке не обновлена: {e}"));
        }
    }

    /// Сколько срезов памяти действует сейчас при нынешнем пределе (скрытая проверка).
    pub(crate) fn cuts_now(&self) -> usize {
        self.auto.physical.failures(self.auto.choice.physical_rule())
    }

    /// Скрытая имитационная проверка: убрать память и прочитать физические записи с диска БЕЗ новой записи перед этим.
    /// Так проверяется сохранение успешных тепловых отсечек, а не сохранение оставшегося объекта в памяти.
    pub(crate) fn restart_physical_memory(&mut self) {
        if !self.config.simulate {
            return;
        }
        self.auto.physical = Ledger::default();
        self.load_maxima();
    }

    /// Строка подсказки: что автомодуль сейчас делает.
    pub(super) fn auto_tip(&self) -> Option<String> {
        if !self.auto.choice.on && !self.auto.active() {
            return None;
        }
        let mut text = format!(
            "авто: до {} °C, аварийная {} °C",
            self.auto.choice.limit, self.auto.choice.emergency
        );
        let intended = self.intended.as_deref().unwrap_or_default();
        for (block, overlay) in self.topology.blocks.iter().zip(&self.auto.overlay) {
            if let (Some(to), Some(&base)) = (overlay, intended.get(block.first)) {
                let shift = if *to < base {
                    format!("−{}", base - to)
                } else {
                    format!("+{}", to - base)
                };
                let _ = write!(text, " · {} {shift}", block.name); // into a String: cannot fail
            }
        }
        Some(text)
    }

    /// Опрос раз в 2 с. Таймер есть, пока включён автомодуль или журнал срезов (`apply_choice`). Датчиков он не читает
    /// (бриф T: один путь чтения — опрос срезов, app/sense.rs), он решает:
    /// - нужен ли опрос срезов раз в 0,25 с: автомодулю — всегда, пока он включён (условий нагрузки нет); журналу
    ///   срезов — пока процессор занят (≥ `LOADED` по Windows);
    /// - пишутся ли строки журнала: журнал включён и процессор занят (DECISIONS #32, подзапись 16.10);
    /// - сверка потолков под арендой: чужая правка принимается (автомодуль этот блок больше не трогает), сброс сном —
    ///   дело сверки после пробуждения; потолки не прочитаны — опрос срезов стоит, владельцу — одно сообщение.
    pub(super) fn auto_tick(&mut self, shell: &mut Shell) {
        self.auto.ticks += 1;
        let choice = self.auto.choice;
        if !choice.on && !choice.log {
            shell.kill_timer(AUTO); // сообщение таймера, пришедшее после его снятия
            return;
        }
        // Сообщения таймера, накопившиеся, пока было открыто меню, приходят разом: один опрос, а не несколько в одну
        // и ту же секунду (ревизия L5).
        let half = self.config.auto.tick / 2;
        if self.auto.last_tick.is_some_and(|t| t.elapsed() < half) {
            return;
        }
        self.auto.last_tick = Some(Instant::now());
        if let Some(other) = self.other() {
            // C#-часть стартует (или уже работает): вернуть принятые, пока она ждёт аренду, и отпустить её.
            self.stop_sensing(shell);
            self.yield_to(other);
            return;
        }
        if self.take_back_pending || self.auto.wake_pending {
            self.stop_sensing(shell);
            return; // потолки ставит на место возврат права или сверка после пробуждения
        }
        let started = Instant::now();
        let busy = self.system_share().is_some_and(|v| v >= LOADED);
        let wanted = choice.on || (choice.log && busy);
        // Опрос срезов начинается под той же арендой: иначе `with_ceilings` отпустил бы драйвер, который через 0,25 с
        // снова нужен. Потолки не прочитаны — опроса нет: строка журнала без них была бы не прочитанной, а регулятор
        // судил бы по последним известным (ревизия 15.10, п. 3).
        let outcome = wanted.then(|| {
            self.with_ceilings(|app| {
                let checked = app.check_ceilings();
                if matches!(checked, Ok(Polled::Quiet | Polled::Said(_))) {
                    app.start_sensing(shell);
                }
                checked
            })
        });
        if matches!(outcome, Some(Ok(Ok(Polled::Quiet | Polled::Said(_))))) {
            self.set_rows(choice.log && busy);
        } else {
            self.stop_sensing(shell);
        }
        let said = match outcome {
            None => None,
            Some(Ok(Ok(Polled::Quiet))) => Some(None),
            Some(Ok(Ok(Polled::Said(said)) | Err(said))) => Some(Some(said)),
            Some(Ok(Ok(Polled::Reset))) => {
                // Сброс сном, пробуждения программа не заметила (бывает при современном сне): сверка — сейчас.
                self.auto.wake_pending = true;
                shell.set_timer(super::WAKE, self.config.wake_delay);
                Some(None)
            }
            Some(Err(reason)) => Some(Some(format!("{} ждёт: {reason}", self.poller()))),
        };
        // Потолков не читали (автомодуль выключен, процессор не занят) — сказанное прежде остаётся сказанным: иначе одна
        // и та же беда повторялась бы с каждым новым «процессор занят».
        match said {
            Some(Some(said)) => {
                // Одна и та же беда — одно сообщение, а не каждые 2 с (ревизия M5); драйвер — переоткрыть.
                if self.auto.last_said.as_deref() != Some(said.as_str()) {
                    self.notify(shell, &said);
                    self.auto.last_said = Some(said);
                }
                if let Some(device) = &mut self.device {
                    device.cpu().release();
                }
            }
            Some(None) => self.auto.last_said = None,
            None => {}
        }
        if self.auto.last_cost.is_none_or(|t| t.elapsed() >= COST_EVERY) {
            self.auto.last_cost = Some(Instant::now());
            self.journal.note(&format!(
                "{}: опрос раз в 2 с {} мкс{}",
                match (choice.on, choice.log) {
                    (true, true) => "автомодуль",
                    (true, false) => "автомодуль (журнал срезов выключен)",
                    (false, _) => "автомодуль выключен, журнал срезов",
                },
                started.elapsed().as_micros(),
                match (wanted, self.logging.rows) {
                    (false, _) => ", процессор не занят — датчики и потолки не читаются",
                    (true, true) => ", сверка потолков; датчики — опрос срезов, строки пишутся",
                    (true, false) => ", сверка потолков; датчики — опрос срезов, строк нет",
                }
            ));
        }
        let tip = self.tip();
        if tip != self.auto.last_tip {
            shell.set_tip(&tip);
            self.auto.last_tip = tip;
        }
    }

    /// Кто опрашивает — для сообщений владельцу: включённый автомодуль или, когда он выключен, журнал срезов.
    fn poller(&self) -> &'static str {
        if self.auto.choice.on {
            "Автомодуль"
        } else {
            "Журнал срезов"
        }
    }

    /// Сверка потолков под арендой (раз в 2 с): чужая правка принимается, о ней — одно сообщение; метка и аренда — по
    /// надстройке (чужая правка могла снять её последний блок, ревизия M2). Err — что сказать владельцу.
    fn check_ceilings(&mut self) -> Result<Polled, String> {
        let Some(foreign) = self.reconcile()? else {
            return Ok(Polled::Reset);
        };
        let said = (!foreign.is_empty()).then(|| {
            format!(
                "Потолки блоков {} изменила другая программа — {}",
                foreign.join(", "),
                if self.auto.choice.on {
                    "автомодуль их больше не трогает"
                } else {
                    "оставляю их как есть"
                }
            )
        });
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        Ok(said.map_or(Polled::Quiet, Polled::Said))
    }

    /// Доля работы всего процессора с прошлого опроса по Windows; первый опрос — неизвестно. На имитации — доля работы её ядер
    /// (их нагрузку задаёт скрытая проверка), а не Windows компьютера, где идёт проверка: шаги проверки не должны
    /// зависеть от того, чем он занят (игра владельца, соседние проверки).
    pub(super) fn system_share(&mut self) -> Option<f64> {
        if self.config.simulate {
            let share = match &self.device {
                Some(Device::Sim(sim)) if !sim.load.is_empty() => {
                    let cores = f64::from(u32::try_from(sim.load.len()).unwrap_or(u32::MAX));
                    Some((sim.load.iter().sum::<f64>() / cores).clamp(0.0, 1.0))
                }
                _ => None,
            };
            self.auto.global_busy = share;
            return share;
        }
        let now = crate::shell::system_times();
        let share = match (self.auto.system, now) {
            (Some((idle0, total0)), Some((idle1, total1)))
                if total1 > total0 && idle1 >= idle0 && idle1 - idle0 <= total1 - total0 =>
            {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a share of two tick counts; the error is far below a percent"
                )]
                let idle = idle1.saturating_sub(idle0) as f64 / (total1 - total0) as f64;
                Some((1.0 - idle).clamp(0.0, 1.0))
            }
            _ => None,
        };
        self.auto.system = now;
        self.auto.global_busy = share;
        share
    }

    /// Обороты вентиляторов: пока пишутся строки журнала срезов — раз в 2 с (`AutoTiming::fans`), иначе и после отказа —
    /// не чаще раза в 10 с (каждый раз — вызов прошивки; для условий в журнале действий автомодуля этого хватает),
    /// прошивка не открылась — раз в минуту. На имитации прошивка не трогается. Зовут опрос срезов и опрос окна. true —
    /// обороты прочитаны сейчас (они идут в строку среза только тогда).
    pub(super) fn read_fans(&mut self) -> bool {
        let now = Instant::now();
        let every = match self.auto.fans_fault {
            Some(FanFault::Open) => FANS_RETRY,
            None if self.logging.rows => self.config.auto.fans,
            Some(FanFault::Read) | None => FANS_EVERY,
        };
        // Пол-опроса срезов — допуск на дрожание таймера: обороты в каждой восьмой строке, а не то в восьмой, то в
        // девятой.
        let slack = self.config.auto.sample / 2;
        if self
            .auto
            .fans_last
            .is_some_and(|last| now.saturating_duration_since(last) + slack < every)
        {
            return false;
        }
        self.auto.fans_last = Some(now);
        if self.auto.fans.is_none() {
            let opened: Result<Box<dyn Fans>, String> = match &self.device {
                Some(Device::Sim(_)) => Ok(Box::new(sim_fans(false))),
                Some(Device::Real(_)) => HpFans::open()
                    .map(|fans| Box::new(fans) as Box<dyn Fans>)
                    .map_err(|e| e.0),
                None => return false,
            };
            match opened {
                Ok(fans) => self.auto.fans = Some(fans),
                Err(text) => {
                    self.auto.rpm = None;
                    self.auto.fans_fault = Some(FanFault::Open);
                    self.fans_trouble(&text);
                    return false;
                }
            }
        }
        let Some(fans) = self.auto.fans.as_mut() else {
            return false;
        };
        let read = fans.rpm();
        self.auto.fan_calls += 1;
        match read {
            Ok(rpm) => {
                self.auto.rpm = Some(rpm);
                self.auto.fans_said = None;
                self.auto.fans_fault = None;
                true
            }
            Err(e) => {
                self.auto.rpm = None;
                self.auto.fans_fault = Some(FanFault::Read);
                self.fans_trouble(&e.0);
                false
            }
        }
    }

    /// Скрытая проверка: обороты имитации отказывают (`fail`) или снова отвечают. Только на имитации.
    pub fn sim_fans(&mut self, fail: bool) {
        if matches!(self.device, Some(Device::Sim(_))) {
            self.auto.fans = Some(Box::new(sim_fans(fail)));
        }
    }

    /// Беда с оборотами — одна строка журнала на одну и ту же беду, а не каждые 10 с.
    fn fans_trouble(&mut self, text: &str) {
        if self.auto.fans_said.as_deref() != Some(text) {
            self.journal
                .error(&format!("автомодуль: обороты вентиляторов не прочитаны — {text}"));
            self.auto.fans_said = Some(text.to_owned());
        }
    }

    /// Сверка: стоит ли у каждого блока то, что должно (опущенное или принятое). Возвращает блоки с принятой чужой
    /// правкой; Ok(None) — потолки сброшены сном (их ставит сверка после пробуждения, ревизия H3).
    fn reconcile(&mut self) -> Result<Option<Vec<String>>, String> {
        let poller = self.poller();
        let topology = &self.topology;
        let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) else {
            return Ok(Some(Vec::new()));
        };
        if let Some(unread) = limits.refresh_blocks(cpu, topology) {
            return Err(format!("{poller}: потолки не прочитаны — {unread}"));
        }
        let intended = self.intended.get_or_insert_with(|| limits.current().to_vec());
        let mut foreign = Vec::new();
        // Принятые до чужой правки — для истории профилей; копия только в опрос, где правка принята (редко).
        let mut before: Option<Vec<i32>> = None;
        let mut adopted = Vec::new();
        for (b, block) in topology.blocks.iter().enumerate() {
            let i = block.first;
            let expected = self.auto.overlay[b].unwrap_or(intended[i]);
            if self.auto.recover_pending[b] {
                if let Some(refusal) = limits.set(cpu, topology, i, expected) {
                    return Err(format!(
                        "удержание теплового потолка {} ×{expected}: {refusal}",
                        block.name
                    ));
                }
                self.auto.recover_pending[b] = false;
            }
            if !limits.known(i) || !limits.answered(i) || limits.current()[i] == expected {
                self.auto.mismatch[b] = 0;
                continue;
            }
            if at_boot(limits, &self.boot, i) {
                return Ok(None);
            }
            self.auto.mismatch[b] = self.auto.mismatch[b].saturating_add(1);
            if self.auto.mismatch[b] < FOREIGN_READS {
                continue;
            }
            before.get_or_insert_with(|| intended.clone());
            adopted.push(block.name.clone());
            // Потолок у блока общий, а перечитано только первое ядро (refresh_blocks): его значение — всем ядрам блока.
            for &m in &block.members {
                intended[m] = limits.current()[i];
            }
            self.auto.overlay[b] = None;
            self.auto.mismatch[b] = 0;
            if !self.auto.aside[b] {
                self.auto.aside[b] = true;
                foreign.push(block.name.clone());
            }
            if let Some(controller) = &mut self.auto.controller {
                controller.forget_block(b);
            }
        }
        if let Some(before) = before {
            self.log_change("foreign", None, &before, Some(&adopted.join(", ")), None);
            self.version += 1;
        }
        Ok(Some(foreign))
    }

    /// Решения регулятора по этому опросу срезов — под арендой, сразу после чтения датчиков (app/sense.rs): каждое
    /// показание первого ядра блока идёт в регулятор, аварийная отсечка — по нему же. Автомодуль выключен — регулятора
    /// нет, решать нечего (владелец 16.10: выключатель — про изменение множителей). `package` — корпус этого опроса,
    /// для журнала действий. Возвращает отказ, если он был.
    pub(super) fn react(&mut self, package: Option<f64>) -> Option<String> {
        self.auto.controller.as_ref()?;
        let limits = self.limits.as_ref()?;
        let intended = self.intended.as_deref()?;
        let topology = &self.topology;
        let Autopilot {
            frame,
            aside,
            mismatch,
            overlay,
            recover_pending,
            ..
        } = &mut self.auto;
        frame.readings.clear();
        frame.context.clear();
        frame.ceilings.clear();
        frame.accepted.clear();
        frame.floor.clear();
        for (b, block) in topology.blocks.iter().enumerate() {
            let ceiling = limits.current()[block.first];
            let base = intended[block.first];
            frame.ceilings.push(ceiling);
            // Возвращать регулятор может только то, что сдвинул сам: блок ниже принятого, но не в надстройке, опустил
            // кто-то другой — для регулятора его профиль — то, что стоит. Иначе прохладный блок поднимался бы поверх
            // чужой правки (найдено скрытой проверкой 14.10).
            frame
                .accepted
                .push(if overlay[b].is_some() { base } else { ceiling.min(base) });
            // Ниже этого регулятор блок не опускает: та же граница, что у ручного потолка (`Limits::set` ниже не пишет).
            frame.floor.push(limits.floor(block.first));
            // Отставленный после чужой правки блок и блок, у которого потолок расходится с ожидаемым и ещё не признан
            // чужой правкой, не судятся.
            let judged = !aside[b] && mismatch[b] == 0 && !recover_pending[b];
            let reading = self.meter.latest(block.first);
            frame.context.push(Some(reading));
            frame.readings.push(judged.then_some(reading));
        }
        let now = self.auto.origin.elapsed();
        let global_busy = self.auto.global_busy;
        let rule = self.auto.choice.physical_rule();
        let Autopilot {
            controller,
            physical,
            frame,
            decisions,
            ..
        } = &mut self.auto;
        let allowed = |b: usize, to: i32, _: Option<f64>, window: &[Reading]| {
            physical.allows_closed(b, to, &frame.context, window, rule)
        };
        let view = Frame {
            readings: &frame.readings,
            ceilings: &frame.ceilings,
            accepted: &frame.accepted,
            floor: &frame.floor,
        };
        auto_state::relative_activity(&topology.blocks, &frame.context, &mut frame.relative);
        frame.prior.clear();
        frame
            .prior
            .extend((0..topology.blocks.len()).map(|b| controller.as_ref().and_then(|c| c.protection(b))));
        let load = LoadFrame {
            global_busy,
            relative: &frame.relative,
        };
        controller
            .as_mut()?
            .step_loaded_physical(now, &view, &load, &allowed, decisions);
        self.observe_physical_window();
        let Autopilot {
            controller,
            frame,
            decisions,
            ..
        } = &mut self.auto;
        if decisions.is_empty() {
            let changed = frame
                .prior
                .iter()
                .enumerate()
                .any(|(b, prior)| controller.as_ref().and_then(|c| c.protection(b)) != *prior);
            if changed && let Err(e) = self.mark() {
                self.journal.error(&format!("подтверждение потолка не сохранено: {e}"));
            }
            return None;
        }
        self.apply_decisions(package)
    }

    fn apply_decisions(&mut self, package: Option<f64>) -> Option<String> {
        self.prepare_physical_failure_sources();
        let decisions = std::mem::take(&mut self.auto.decisions);
        let mut refused = None;
        for decision in &decisions {
            if let Some(refusal) = self.apply(decision, package) {
                if let Some(controller) = &mut self.auto.controller {
                    match self.auto.frame.prior[decision.block] {
                        Some(p) => controller.seed_protection(decision.block, p),
                        None => controller.clear_protection(decision.block),
                    }
                }
                refused = Some(refusal);
            }
        }
        if self.auto.physical.failure_changed {
            self.save_maxima();
        }
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        self.auto.decisions = decisions;
        refused
    }

    /// Одно решение регулятора: надстройка и метка — до записи в процессор, потолок, затем память срезов и журналы.
    /// Возвращает отказ.
    fn apply(&mut self, d: &Decision, package: Option<f64>) -> Option<String> {
        let (name, first) = {
            let block = &self.topology.blocks[d.block];
            (block.name.clone(), block.first)
        };
        let base = self.auto.frame.accepted[d.block];
        let was = self.auto.overlay[d.block];
        // Метка — до записи в процессор: с новой надстройкой.
        self.auto.overlay[d.block] = (d.to != base).then_some(d.to);
        if let Err(e) = self.mark() {
            self.auto.overlay[d.block] = was;
            return Some(format!("Автомодуль: метка не записана, потолок не тронут — {e}"));
        }
        let topology = &self.topology;
        let (limits, cpu) = parts(&mut self.limits, &mut self.device)?;
        if let Some(refusal) = limits.set(cpu, topology, first, d.to) {
            self.auto.overlay[d.block] = was;
            return Some(format!("Автомодуль: потолок блока {name} не изменён — {refusal}"));
        }
        self.record(d, &name, first, base, package);
        None
    }

    /// Успешное решение: тепловой отказ — измеренному вектору; в журналы — средние и исходный вектор опроса.
    fn record(&mut self, d: &Decision, name: &str, first: usize, base: i32, package: Option<f64>) {
        let stamp = Stamp::now();
        self.remember_physical_failure(d);
        if matches!(d.cause, Cause::Emergency | Cause::Target) {
            self.auto.memory.cut(first, d.from, d.temp, d.die);
        }
        let die = d.die.map_or_else(|| "?".into(), |v| format!("{v:.0}"));
        let (from, to, temp) = (d.from, d.to, d.temp);
        let text = match d.cause {
            Cause::Emergency => format!(
                "автомодуль: блок {name} — аварийная отсечка ×{from} → ×{to} (показание {temp:.0} °C, аварийная {} °C; кристалл {die} °C)",
                self.auto.choice.emergency
            ),
            Cause::Target => format!(
                "автомодуль: блок {name} опущен ×{from} → ×{to} (среднее за {} с {temp:.1} °C выше {} °C; кристалл {die} °C)",
                self.auto.choice.window_secs, self.auto.choice.limit
            ),
            Cause::Revert => format!("автомодуль: блок {name} ×{from} → ×{to} — повышение не подтверждено работой"),
            Cause::Return => format!(
                "автомодуль: блок {name} пробует ×{from} → ×{to} после проверки работы (целевая {} °C; кристалл {die} °C)",
                self.auto.choice.limit
            ),
        };
        self.journal.note(&text);
        let means: Vec<Option<f64>> = self
            .auto
            .controller
            .as_ref()
            .map(|c| c.means().map(|m| m.map(|v| round(v, 1))).collect())
            .unwrap_or_default();
        let ceilings: Vec<Option<i32>> = self
            .topology
            .blocks
            .iter()
            .map(|b| self.limits.as_ref().map(|l| l.current()[b.first]))
            .collect();
        let gpu = self.last_gpu().map(|g| {
            json!({
                "temp": g.temp.map(|v| round(v, 0)),
                "power": g.power.map(|v| round(v, 1)),
                "busy": g.busy.map(|v| round(v, 2)),
            })
        });
        let mut line = Map::new();
        line.insert("time".into(), json!(stamp.roundtrip()));
        line.insert("at".into(), json!(stamp.0));
        line.insert("act".into(), json!(d.cause.act()));
        line.insert("block".into(), json!(name));
        line.insert("first".into(), json!(first));
        line.insert("from".into(), json!(from));
        line.insert("to".into(), json!(to));
        line.insert("accepted".into(), json!(base));
        line.insert("limit".into(), json!(self.auto.choice.limit));
        line.insert("emergency".into(), json!(self.auto.choice.emergency));
        line.insert("policy".into(), json!("manual-mean-config-v1"));
        line.insert("windowSecs".into(), json!(self.auto.choice.window_secs));
        // Аварийная — одно показание; целевая и возврат — среднее за выбранное окно.
        let trigger = if d.cause == Cause::Emergency { "reading" } else { "mean" };
        // Температура среза и уровень кристалла — без округления: из них при запуске восстанавливается память срезов, а
        // она сравнивает уровень кристалла строго («ниже, чем при срезе»). Округлённый вверх, он пустил бы возврат при
        // том же кристалле (найдено скрытой проверкой: 47,46 °C записалось бы как 47,5).
        line.insert(trigger.into(), json!(temp));
        line.insert("die".into(), json!(d.die));
        line.insert("means".into(), json!(means));
        line.insert("ratio".into(), json!(d.ratio));
        line.insert("busy".into(), json!(d.busy));
        line.insert("package".into(), json!(package.map(|p| round(p, 0))));
        line.insert("fans".into(), json!(self.auto.rpm));
        line.insert("gpu".into(), json!(gpu));
        line.insert("ceilings".into(), json!(ceilings));
        self.return_evidence(d.block, &mut line);
        // Журнал событий окна (app/timeline.rs): то же решение с итогом блока на покидаемом потолке; неизвестное — не пишется.
        let mut conditions = Map::new();
        for name in [
            trigger,
            "die",
            "means",
            "policy",
            "windowSecs",
            "ceilingsBefore",
            "physical",
            "physicalWindow",
            "ratio",
            "busy",
            "globalBusy",
            "relativeRepresentative",
            "relativeBusy",
            "protection",
            "evidencePhase",
        ] {
            // Неизвестное не пишется: ни null, ни список из одних null (медиан ещё нет — первое окно не закрылось).
            let known = |v: &&Value| !v.is_null() && v.as_array().is_none_or(|a| a.iter().any(|x| !x.is_null()));
            if let Some(value) = line.get(name).filter(known) {
                conditions.insert(name.into(), value.clone()); // a few numbers per decision
            }
        }
        self.known_surroundings(&mut conditions);
        self.log_line(&Value::Object(line));
        self.log_event(d.cause.act(), Some(d.block), Some((from, to)), None, conditions);
    }

    fn return_evidence(&self, block: usize, line: &mut Map<String, Value>) {
        // Только при записи действия: точные измерения, без округления для интерфейса и без подмены потолками.
        line.insert(
            "physical".into(),
            auto_state::physical_value(
                &self.auto.frame.context,
                &self.auto.frame.relative,
                self.auto.global_busy,
            ),
        );
        if let Some(controller) = &self.auto.controller
            && let Some((start, end)) = controller.window_span()
        {
            // Сводка сохранённого окна отдельно от последнего опроса; глобальная нагрузка окна не измерена здесь.
            let readings: Vec<Option<Reading>> = controller.window_readings().map(Some).collect();
            let mut window = auto_state::physical_value(&readings, &[], None);
            window["sampling"] = json!("completed-controller-window");
            window["ratioBasis"] = json!("busy-weighted-window-average");
            window["tempBasis"] = json!("arithmetic-mean-valid-polls");
            window["busyBasis"] = json!("arithmetic-mean-known-polls");
            window["startControllerSecs"] = json!(start.as_secs_f64());
            window["endControllerSecs"] = json!(end.as_secs_f64());
            window["recordedControllerSecs"] = json!(self.auto.origin.elapsed().as_secs_f64());
            line.insert("physicalWindow".into(), window);
        }
        line.insert("ceilingsBefore".into(), json!(self.auto.frame.ceilings));
        line.insert("globalBusy".into(), json!(self.auto.global_busy));
        line.insert(
            "representativeBusy".into(),
            json!(
                self.auto
                    .frame
                    .context
                    .iter()
                    .map(|r| r.and_then(|r| r.busy))
                    .collect::<Vec<_>>()
            ),
        );
        line.insert(
            "representativeRatio".into(),
            json!(
                self.auto
                    .frame
                    .context
                    .iter()
                    .map(|r| r.and_then(|r| r.ratio))
                    .collect::<Vec<_>>()
            ),
        );
        line.insert("relativeRepresentative".into(), json!(self.auto.frame.relative));
        line.insert("relativeBusy".into(), json!(self.auto.frame.relative[block]));
        line.insert("evidencePhase".into(), json!("before-action"));
        line.insert(
            "protection".into(),
            auto_state::protection_value(self.auto.controller.as_ref().and_then(|c| c.protection(block))),
        );
    }

    pub(super) fn log_line(&mut self, line: &Value) {
        if let Err(e) = files::append_line(&self.log_path(), &line.to_string()) {
            self.journal.error(&format!("журнал автомодуля не дописан: {e}"));
        }
    }

    /// Вернуть принятые потолки блокам надстройки (C#-часть стартует, сон, выход, смена набора, новые настройки).
    /// Пишет без проверки присутствия: стартующая C#-часть ждёт аренду, которую держит эта программа, и ещё ничего
    /// не прочитала. Блок, которому вернуть не удалось, остаётся опущенным — с меткой и арендой (ревизия M1).
    /// `reason` — для журнала событий окна (app/timeline.rs, kind «putback»): auto-off, settings, sleep, exit, csharp;
    /// None — смена набора: её строки «profile» и так идут от того потолка, что стоял, с поправкой.
    pub(super) fn put_back(&mut self, why: &str, reason: Option<&'static str>) {
        if self.auto.choice.on && reason.is_some_and(|r| r != "auto-off") {
            self.preserve_auto(why);
            return;
        }
        if !self.auto.active() {
            return;
        }
        let topology = &self.topology;
        let intended = self.intended.clone().unwrap_or_default(); // a few dozen numbers, once per put-back
        let mut back = Vec::new();
        // Блоки, которым вернулся принятый: (блок, с какого, на какой) — строки журнала событий после записи.
        let mut moved = Vec::new();
        if let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
            for (b, block) in topology.blocks.iter().enumerate() {
                if self.auto.overlay[b].is_none() {
                    continue;
                }
                let Some(&want) = intended.get(block.first) else {
                    continue;
                };
                let from = limits.current()[block.first];
                match limits.set(cpu, topology, block.first, want) {
                    None => {
                        self.auto.overlay[b] = None;
                        back.push(format!("{} ×{want}", block.name));
                        if from != want {
                            moved.push((b, from, want));
                        }
                    }
                    Some(refusal) => self.journal.error(&format!(
                        "автомодуль: блоку {} не вернулся ×{want}: {refusal}",
                        block.name
                    )),
                }
            }
        }
        if let Some(controller) = &mut self.auto.controller {
            controller.forget();
        }
        self.journal.note(&format!(
            "автомодуль: {why} — принятые потолки возвращены: {}",
            back.join(", ")
        ));
        let stamp = Stamp::now();
        let line = json!({ "time": stamp.roundtrip(), "at": stamp.0, "act": "back", "why": why, "blocks": back });
        self.log_line(&line);
        if let Some(reason) = reason {
            for (b, from, to) in moved {
                let conditions = self.own_conditions(b);
                self.log_event("putback", Some(b), Some((from, to)), Some(reason), conditions);
            }
        }
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
    }

    /// C#-часть пришла: вернуть принятые и отпустить аренду.
    pub(super) fn yield_to(&mut self, other: Other) {
        if self.auto.active() {
            let why = match other {
                Other::OldWindow => "открывается прежнее окно",
                _ => "запускается настройка",
            };
            self.put_back(why, Some("csharp"));
            self.release_auto_lease();
        }
        if let Some(controller) = &mut self.auto.controller {
            controller.forget();
        }
    }
}

/// Обороты имитации: те же числа во всей программе (скрытая проверка ждёт их в состоянии окна).
pub(super) fn sim_fans(fail: bool) -> SimFans {
    SimFans {
        rpm: vec![3200, 3000],
        fail,
    }
}

/// Число для журнала: `digits` знаков после запятой.
fn round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    // Настройки автомодуля по плану (DECISIONS #32, шаг 2; #38; бриф T): целевая отсечка 60–95 °C с шагом 1 °C,
    // аварийная — от целевой + 1 до 100 °C, по умолчанию 95; глубины и подтягивания нет; негодное в файле — значение по
    // умолчанию у этого поля, в команде — отказ.
    use super::*;

    fn map(text: &str) -> Map<String, Value> {
        serde_json::from_str(text).unwrap_or_default()
    }

    #[test]
    fn manual_controls_roundtrip_and_change_only_named_fields() {
        let original = Choice::default();
        let selected = original
            .changed(&map(
                r#"{"windowSecs":9,"settleSecs":0,"returnGap":7,"returnSecs":350}"#,
            ))
            .expect("valid manual settings");
        assert_eq!(
            (
                selected.window_secs,
                selected.settle_secs,
                selected.return_gap,
                selected.return_secs
            ),
            (9, 0, 7, 350)
        );
        assert_eq!(Choice::read(&map(&selected.file_text())), selected);
        let next = selected.changed(&map(r#"{"limit":80}"#)).expect("valid target");
        assert_eq!(
            Choice {
                limit: original.limit,
                ..next
            },
            selected
        );
        assert_eq!(
            selected.physical_rule(),
            Choice {
                settle_secs: 20,
                return_gap: 0,
                return_secs: 2,
                ..selected
            }
            .physical_rule()
        );
        assert!(!selected.same_auto(original));
    }

    #[test]
    fn invalid_manual_controls_never_partially_apply() {
        let original = Choice::default();
        for text in [
            r#"{"windowSecs":0,"on":true}"#,
            r#"{"windowSecs":61}"#,
            r#"{"settleSecs":-1}"#,
            r#"{"settleSecs":121}"#,
            r#"{"returnGap":-1}"#,
            r#"{"returnGap":31}"#,
            r#"{"returnSecs":0}"#,
            r#"{"returnSecs":601}"#,
            r#"{"windowSecs":2.0}"#,
            r#"{"returnSecs":"120"}"#,
        ] {
            assert!(original.changed(&map(text)).is_err(), "{text}");
        }
        let migrated = Choice::read(&map(
            r#"{"auto":true,"windowSecs":0,"settleSecs":4,"returnGap":-1,"returnSecs":600}"#,
        ));
        assert_eq!(
            (
                migrated.window_secs,
                migrated.settle_secs,
                migrated.return_gap,
                migrated.return_secs
            ),
            (2, 4, 0, 600)
        );
        assert!(migrated.on);
    }

    #[test]
    fn accelerated_checks_scale_every_manual_duration() {
        let timing = Timing {
            window: Duration::from_millis(20),
            recheck: Duration::from_millis(10),
            settle: Duration::from_millis(20),
            pause: Duration::from_millis(1200),
        };
        let configured = Choice {
            window_secs: 4,
            settle_secs: 3,
            return_secs: 60,
            ..Choice::default()
        }
        .timing(timing);
        assert_eq!(configured.window, Duration::from_millis(40));
        assert_eq!(configured.settle, Duration::from_millis(30));
        assert_eq!(configured.pause, Duration::from_millis(600));
        assert_eq!(configured.recheck, timing.recheck);
    }

    #[test]
    fn file_of_the_previous_build_is_read_as_is_its_depth_and_step_up_ignored() {
        // Файлы, записанные сборками с глубиной (0.5.3 и раньше) и с подтягиванием (0.5.5 и раньше): лишние ключи любого
        // вида не мешают остальному; аварийной в них нет — 95 °C.
        for text in [
            r#"{"auto":true,"limit":75,"depth":6,"stepUp":true}"#,
            r#"{"auto":true,"limit":75,"depth":99,"stepUp":false}"#,
            r#"{"auto":true,"limit":75,"depth":"глубоко","stepUp":"да"}"#,
        ] {
            assert_eq!(
                Choice::read(&map(text)),
                Choice {
                    on: true,
                    limit: 75,
                    emergency: 95,
                    log: true,
                    ..Choice::default()
                },
                "{text}: a file without «log» (0.5.4 and earlier) keeps the samples log on"
            );
        }
    }

    #[test]
    fn the_file_is_written_without_depth_and_step_up() {
        let text = Choice {
            on: true,
            limit: 77,
            emergency: 93,
            log: false,
            ..Choice::default()
        }
        .file_text();
        assert!(!text.contains("depth") && !text.contains("stepUp"), "{text}");
        assert_eq!(
            Choice::read(&map(&text)),
            Choice {
                on: true,
                limit: 77,
                emergency: 93,
                log: false,
                ..Choice::default()
            }
        );
    }

    #[test]
    fn the_emergency_limit_defaults_to_95_and_stays_above_the_target_in_the_file() {
        assert_eq!(Choice::default().emergency, 95);
        for (text, emergency) in [
            (r#"{"limit":80,"emergency":90}"#, 90),
            (r#"{"limit":80,"emergency":100}"#, 100),
            (r#"{"limit":80,"emergency":81}"#, 81),
            // негодное — по умолчанию
            (r#"{"limit":80,"emergency":80}"#, 95),
            (r#"{"limit":80,"emergency":101}"#, 95),
            (r#"{"limit":80,"emergency":92.5}"#, 95),
            (r#"{"limit":80,"emergency":"90"}"#, 95),
            // целевая 95: по умолчанию 95 не выше неё — на градус выше
            (r#"{"limit":95}"#, 96),
            (r#"{"limit":95,"emergency":95}"#, 96),
        ] {
            assert_eq!(Choice::read(&map(text)).emergency, emergency, "{text}");
        }
    }

    // Два независимых выключателя (владелец 16.10): журнал срезов и автомодуль. Файл без «log» — журнал включён; каждая
    // команда меняет только своё.
    #[test]
    fn the_log_switch_reads_on_by_default_and_round_trips() {
        assert!(Choice::default().log, "a fresh install logs");
        assert!(
            Choice::read(&map(r#"{"auto":false,"limit":80}"#)).log,
            "old file without «log»"
        );
        assert!(!Choice::read(&map(r#"{"auto":true,"log":false}"#)).log);
        // негодное значение — по умолчанию, соседи остаются
        let c = Choice::read(&map(r#"{"auto":true,"log":"нет"}"#));
        assert!(c.log && c.on, "{c:?}");
        for log in [true, false] {
            let c = Choice {
                log,
                ..Choice::default()
            };
            assert_eq!(Choice::read(&map(&c.file_text())), c);
        }
    }

    #[test]
    fn log_set_changes_only_the_log_and_auto_set_never_the_log() {
        let was = Choice {
            on: true,
            limit: 81,
            emergency: 92,
            log: true,
            ..Choice::default()
        };
        let next = was.log_changed(&map(r#"{"id":4,"c":"log.set","on":false}"#));
        assert_eq!(next, Ok(Choice { log: false, ..was }));
        assert!(
            next.is_ok_and(|n| n.same_auto(was)),
            "log.set leaves the auto settings alone"
        );
        for bad in [
            r#"{"on":"нет"}"#,
            r#"{"on":1}"#,
            r"{}",
            r#"{"id":1,"c":"log.set"}"#,
            r#"{"on":true,"limit":80}"#,
        ] {
            assert!(was.log_changed(&map(bad)).is_err(), "{bad} must be refused");
        }
        // auto.set не знает «log»: опечатка или чужое поле не меняют журнал молча.
        assert!(was.changed(&map(r#"{"log":false}"#)).is_err());
        let next = was.changed(&map(r#"{"on":false}"#));
        assert_eq!(
            next,
            Ok(Choice { on: false, ..was }),
            "auto off keeps the log as it was"
        );
    }

    #[test]
    fn a_bad_field_in_the_file_falls_back_alone() {
        let d = Choice::default();
        for (text, limit) in [
            (r#"{"auto":true,"limit":59}"#, d.limit),
            (r#"{"auto":true,"limit":96}"#, d.limit),
            (r#"{"auto":true,"limit":85.5}"#, d.limit),
            (r#"{"auto":true,"limit":"80"}"#, d.limit),
            (r#"{"auto":true,"limit":70}"#, 70),
        ] {
            let c = Choice::read(&map(text));
            assert!(c.on, "{text}: the good neighbour stays");
            assert_eq!(c.limit, limit, "{text}");
        }
        // Ничего годного: всё по умолчанию, автомодуль выключен.
        assert_eq!(Choice::read(&map(r#"{"auto":"да","emergency":1}"#)), d);
        // Края промежутка — годные.
        assert_eq!(Choice::read(&map(r#"{"limit":60}"#)).limit, 60);
        assert_eq!(Choice::read(&map(r#"{"limit":95}"#)).limit, 95);
    }

    #[test]
    fn a_command_changes_only_what_it_names() {
        let was = Choice {
            on: true,
            limit: 80,
            emergency: 95,
            log: true,
            ..Choice::default()
        };
        let next = was.changed(&map(r#"{"id":7,"c":"auto.set","limit":82}"#));
        assert_eq!(next, Ok(Choice { limit: 82, ..was }));
        let next = was.changed(&map(r#"{"emergency":90,"on":false}"#));
        assert_eq!(
            next,
            Ok(Choice {
                on: false,
                emergency: 90,
                ..was
            })
        );
        // Аварийная проверяется по новой целевой, пришедшей в той же команде.
        let next = was.changed(&map(r#"{"limit":92,"emergency":93}"#));
        assert_eq!(
            next,
            Ok(Choice {
                limit: 92,
                emergency: 93,
                ..was
            })
        );
    }

    #[test]
    fn a_target_raised_to_the_emergency_limit_lifts_the_emergency_limit_above_it() {
        // Последнее слово владельца — целевая; аварийная по правилу брифа всегда выше неё (целевая + 1 … 100).
        let was = Choice {
            on: true,
            limit: 80,
            emergency: 90,
            log: true,
            ..Choice::default()
        };
        assert_eq!(was.changed(&map(r#"{"limit":90}"#)).map(|c| c.emergency), Ok(91));
        assert_eq!(was.changed(&map(r#"{"limit":95}"#)).map(|c| c.emergency), Ok(96));
        assert_eq!(
            was.changed(&map(r#"{"limit":89}"#)).map(|c| c.emergency),
            Ok(90),
            "still above: unchanged"
        );
    }

    #[test]
    fn a_bad_or_unknown_value_refuses_the_whole_command() {
        let was = Choice::default();
        for text in [
            r#"{"limit":59}"#,
            r#"{"limit":96}"#,
            r#"{"limit":85.5}"#,
            r#"{"limit":"85"}"#,
            r#"{"limit":null}"#,
            r#"{"on":1}"#,
            // аварийная — от целевой + 1 до 100, целым числом
            r#"{"emergency":85}"#,
            r#"{"emergency":84}"#,
            r#"{"emergency":101}"#,
            r#"{"emergency":95.5}"#,
            r#"{"emergency":"95"}"#,
            r#"{"limit":90,"emergency":90}"#,
            // подтягивания и глубины больше нет: окно прежней сборки получает отказ, а не молчаливое «принято»
            r#"{"stepUp":true}"#,
            r#"{"depth":4}"#,
            // годное рядом с негодным не применяется частично
            r#"{"on":true,"limit":200}"#,
            r#"{"on":true,"emergency":200}"#,
            // опечатка в имени не должна молча ничего не сделать
            r#"{"limt":80}"#,
            // и рядом с годной: годная не применяется, а опечатка не теряется молча
            r#"{"limit":80,"emergancy":96}"#,
            // ни одной настройки
            r#"{"id":3,"c":"auto.set"}"#,
            r"{}",
        ] {
            assert!(was.changed(&map(text)).is_err(), "{text} must be refused");
        }
    }

    #[test]
    fn the_topic_carries_the_choice_and_both_ranges() {
        let choice = Choice {
            on: true,
            limit: 77,
            emergency: 93,
            log: false,
            ..Choice::default()
        };
        let topic: Value = serde_json::from_str(&choice.topic()).unwrap_or(Value::Null);
        assert_eq!(topic["t"], "settings");
        assert_eq!(
            topic["auto"],
            json!({
                "on": true, "limit": 77, "emergency": 93,
                "limitRange": [60, 95], "emergencyRange": [78, 100], "windowSecs": 2, "settleSecs": 2, "returnGap": 0, "returnSecs": 120, "windowSecsRange": [1, 60], "settleSecsRange": [0, 120], "returnGapRange": [0, 30], "returnSecsRange": [1, 600],
            })
        );
        assert_eq!(topic["log"], json!({ "on": false }), "the log switch has its own field");
        // Файл и команда говорят одними именами: записанное читается обратно тем же.
        assert_eq!(Choice::read(&map(&choice.file_text())), choice);
    }
}
