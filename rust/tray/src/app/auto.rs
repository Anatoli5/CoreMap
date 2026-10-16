// Опрос раз в 2 с и автомодуль в программе входа (DECISIONS #31, «Auto module» и его подзапись; #32). Два независимых
// выключателя (владелец 16.10, DECISIONS #32, подзапись 16.10: «отдельно какой-то выключатель для логирования и отдельно
// выключатель для автоподстройки… это независимый процесс»): автомодуль — меняет ли он потолки, журнал срезов —
// пишется ли он. Опрос идёт, пока включён хоть один из них и потолки у этой программы (не у C#-части, не в ожидании
// возврата права или сверки после сна): процессор нагружен (доля работы по Windows) — показания блоков, сверка потолков
// и (если включён) журнал срезов; включённый автомодуль читает ещё корпус в каждом опросе и, когда корпус близко к
// пределу или блок не на принятом, тоже судит блоки — решение регулятора (machine::auto). Автомодуль выключен —
// регулятора нет, потолков он не меняет; выключен и процессор не занят — ничего не читается. Оба выключены — опроса нет,
// драйвер закрыт между действиями.
//
// Память перегрева (machine::memory) не пускает блок обратно на ступень, где он перегрелся, пока не выйдет запрет; она
// восстанавливается при запуске из журнала действий data\auto-log.jsonl — там каждое решение с условиями: средняя и
// наибольшая температура блока, фактический множитель, загрузка, температуры всех блоков (соседи), корпус,
// вентиляторы. Пока блоки судятся (то же условие), идёт опрос раз в 0,25 с и пишется журнал срезов
// data\samples\<дата>.csv — «матрица» владельца для будущих кривых (app/sense.rs); решает регулятор по-прежнему раз
// в 2 с — по сумме показаний этого опроса за 2 с (app/meter.rs), сам ядра тогда не читает.
//
// Блоки не на принятом — «надстройка» над принятыми потолками (`intended`): принятые не меняются, надстройка помнит,
// какой блок куда сдвинут (вниз — снижение, вверх — подтягивание до заводского). Пока надстройка есть, программа держит аренду «CoreMap.Ceilings»: прежнее окно или движок,
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
use std::time::{Duration, Instant};

use machine::auto::{Action, Controller, Frame, Settings, Timing};
use machine::clock::{Pattern, Stamp};
use machine::files;
use machine::memory::{Memory, Spans};
use machine::thermal::{Reading, Thermal};
use serde_json::{Map, Value, json};

use super::{App, Device, Item, Other, Shell, at_boot, parts};
use crate::fans::{Fans, HpFans, SimFans};

/// Таймер опроса.
pub const AUTO: usize = 4;
/// Пункты подменю: включено; подтягивание вверх; отсечка — открыть окно (там она настраивается).
pub const AUTO_ON: u32 = 10;
pub const STEP_UP_ID: u32 = 11;
pub const AUTO_WINDOW_ID: u32 = 12;
/// Пункт меню верхнего уровня «Журнал срезов» — свой выключатель, не в подменю автомодуля (владелец 16.10).
pub const LOG_ID: u32 = 13;
/// Отсечка, °C, — ползунок в окне с шагом 1 °C (план DECISIONS #32, шаг 2; вместо пресетов 75–95 в меню).
pub const LIMIT_RANGE: RangeInclusive<i32> = 60..=95;
/// Как часто писать в журнал цену опроса — так она измеряется на машине владельца.
pub(super) const COST_EVERY: Duration = Duration::from_secs(60);
/// Столько чтений подряд потолок блока должен расходиться с ожидаемым, чтобы считаться чужой правкой: одно чтение
/// бывает и запоздалым ответом на собственную запись, и чужим запросом в ящике (`ThrottleStop` не берёт наш замок).
const FOREIGN_READS: u8 = 2;
/// С такой доли работы всего процессора (по Windows) блоки читаются и пишется журнал срезов, даже если корпус далеко от
/// предела и автомодуль выключен: для журнала срезов и для подтягивания нужны блоки под нагрузкой. Простой Windows —
/// 1–5 %; число — выбор агента, не мерено.
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
    /// Опрос автомодуля: решение регулятора, а без журнала срезов — и само чтение корпуса (и блоков).
    pub tick: Duration,
    /// Опрос срезов, пока пишется журнал: строка на каждый (выбор владельца 15.10, вариант А, — 0,25 с).
    pub sample: Duration,
    /// Обороты, пока пишется журнал: в строку того опроса, где прочитаны.
    pub fans: Duration,
    /// Видеокарта, пока пишется журнал: в строку того опроса, где прочитана (app/sense.rs, DECISIONS #37).
    pub gpu: Duration,
    pub control: Timing,
    pub bans: Spans,
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
        bans: Spans::REAL,
    };
}

/// Выбор владельца, как он лежит в data\tray.json. Глубины больше нет (владелец 15.10, DECISIONS #38): регулятор
/// опускает блок сколько нужно, но не ниже нижней границы допустимого потолка; ключ «depth» файла прежней сборки
/// читается молча и больше не пишется. Два выключателя независимы (владелец 16.10: «отдельно какой-то выключатель для
/// логирования и отдельно выключатель для автоподстройки… это независимый процесс»): `on` — меняет ли автомодуль
/// потолки, `log` — пишется ли журнал срезов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// Автомодуль меняет потолки (по умолчанию выключен).
    pub on: bool,
    pub limit: i32,
    /// Подтягивать прохладные блоки выше принятого, до заводского (DECISIONS #32; по умолчанию выключено).
    pub step_up: bool,
    /// Журнал срезов data\samples (по умолчанию включён: файл прежней сборки без ключа «log» — тоже включён).
    pub log: bool,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            on: false,
            limit: 85,
            step_up: false,
            log: true,
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

impl Choice {
    /// Выбор из data\tray.json. Каждое поле — само по себе: негодное или пропавшее берёт значение по умолчанию, а
    /// годные соседние остаются (файл прежней сборки с пресетами 75–95 читается как есть, его «depth» не замечается).
    #[must_use]
    pub fn read(map: &Map<String, Value>) -> Self {
        let default = Self::default();
        Self {
            on: map.get("auto").and_then(Value::as_bool).unwrap_or(default.on),
            limit: map
                .get("limit")
                .and_then(|v| whole(v, &LIMIT_RANGE))
                .unwrap_or(default.limit),
            step_up: map.get("stepUp").and_then(Value::as_bool).unwrap_or(default.step_up),
            log: map.get("log").and_then(Value::as_bool).unwrap_or(default.log),
        }
    }

    /// Те же настройки автомодуля (выключатель, отсечка, подтягивание): смена журнала срезов их не касается.
    #[must_use]
    pub fn same_auto(self, other: Self) -> bool {
        (self.on, self.limit, self.step_up) == (other.on, other.limit, other.step_up)
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
    /// негодно или незнакомо (опечатка в имени не должна молча ничего не сделать; «depth» прежнего окна — тоже), или
    /// настроек в команде нет.
    /// `id` и `c` — номер и имя самой команды, не настройки.
    pub fn changed(self, fields: &Map<String, Value>) -> Result<Self, String> {
        let mut next = self;
        let mut any = false;
        for (key, value) in fields {
            match key.as_str() {
                "id" | "c" => continue,
                "on" => next.on = value.as_bool().ok_or("«on» — да или нет")?,
                "stepUp" => next.step_up = value.as_bool().ok_or("«stepUp» — да или нет")?,
                "limit" => {
                    next.limit = whole(value, &LIMIT_RANGE).ok_or_else(|| {
                        format!(
                            "отсечка — целое число °C от {} до {}",
                            LIMIT_RANGE.start(),
                            LIMIT_RANGE.end()
                        )
                    })?;
                }
                other => return Err(format!("незнакомая настройка автомодуля «{other}»")),
            }
            any = true;
        }
        if any {
            Ok(next)
        } else {
            Err("в команде нет настроек автомодуля".into())
        }
    }

    /// Как выбор лежит в data\tray.json.
    #[must_use]
    pub fn file_text(self) -> String {
        json!({ "auto": self.on, "limit": self.limit, "stepUp": self.step_up, "log": self.log }).to_string()
    }

    /// Тема окну «settings»: выбор и допустимая отсечка — её держит эта программа, окно по ней строит ползунок;
    /// журнал срезов — отдельным полем, со своей командой «log.set».
    #[must_use]
    pub fn topic(self) -> String {
        json!({
            "t": "settings",
            "auto": {
                "on": self.on, "limit": self.limit, "stepUp": self.step_up,
                "limitRange": [LIMIT_RANGE.start(), LIMIT_RANGE.end()],
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
    pub(super) controller: Option<Controller>,
    /// По блокам: потолок, поставленный автомодулем (ниже или выше принятого); None — блок на принятом. Окно видит его
    /// в состоянии (app/serve.rs, поле auto).
    pub(super) overlay: Vec<Option<i32>>,
    /// Память перегрева.
    memory: Memory,
    /// Время процессоров по Windows на прошлом опросе: (простой, всего).
    system: Option<(u64, u64)>,
    fans: Option<Box<dyn Fans>>,
    /// Когда обороты читались (или открывалась прошивка) последний раз; следующий раз — через срок, который зависит
    /// от того, пишется ли журнал срезов и чем кончился прошлый раз.
    fans_last: Option<Instant>,
    /// Чем кончился прошлый раз; None — обороты прочитаны (или ещё не спрашивались).
    fans_fault: Option<FanFault>,
    /// Последние прочитанные обороты; None — не известны. Их же видит окно.
    pub(super) rpm: Option<Vec<u32>>,
    fans_said: Option<String>,
    fans_cost: Option<Duration>,
    /// Скрытая проверка: сколько раз обороты спрошены.
    pub(super) fan_calls: u64,
    /// Скрытая проверка: сколько сообщений таймера опроса пришло.
    pub(super) ticks: u64,
    /// По блокам: чужая правка принята — автомодуль этот блок не трогает до смены набора.
    aside: Vec<bool>,
    /// По блокам: сколько чтений подряд потолок расходится с ожидаемым.
    mismatch: Vec<u8>,
    /// Аренда держится этой программой, пока есть опущенный блок.
    holding: bool,
    /// Пробуждение отмечено, сверка после него ещё впереди: опрос ждёт её (ревизия H3).
    pub wake_pending: bool,
    origin: Instant,
    last_tick: Option<Instant>,
    last_cost: Option<Instant>,
    last_said: Option<String>,
    last_tip: String,
}

impl Autopilot {
    #[must_use]
    pub fn new(blocks: usize) -> Self {
        Self {
            choice: Choice::default(),
            controller: None,
            overlay: vec![None; blocks],
            memory: Memory::new(Spans::REAL),
            system: None,
            fans: None,
            fans_last: None,
            fans_fault: None,
            rpm: None,
            fans_said: None,
            fans_cost: None,
            fan_calls: 0,
            ticks: 0,
            aside: vec![false; blocks],
            mismatch: vec![0; blocks],
            holding: false,
            wake_pending: false,
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

/// Показания опроса: корпус и блоки (None — блок не читался).
type Sampled = (Option<f64>, Vec<Option<Reading>>);

/// Изменение потолка блока, сделанное автомодулем, — для журналов.
struct Change {
    name: String,
    first: usize,
    before: i32,
    to: i32,
    base: i32,
}

/// Что сказал опрос.
enum Polled {
    Quiet,
    Said(String),
    /// Потолки сброшены (сон): поставить их на место — дело сверки после пробуждения.
    Reset,
}

/// Докуда дошёл опрос: от этого — пишется ли журнал срезов и что сказать в строке цены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Ничего: автомодуль выключен и процессор не занят — датчики не нужны ни регулятору, ни журналу срезов.
    Nothing,
    /// Только корпус: блоки не нужны, не прочитаны или опроса не было.
    Package,
    /// Блоки прочитаны, потолки — нет.
    Blocks,
    /// Блоки и потолки прочитаны: журнал срезов пишется — в строке потолки прочитанные, а не последние известные
    /// (ревизия 15.10, п. 3; так было и до журнала раз в 0,25 с).
    Ceilings,
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
        Controller::new(
            Settings {
                limit: f64::from(c.limit),
                step_up: c.step_up,
            },
            self.config.auto.control,
            self.topology.blocks.len(),
        )
    }

    /// Запуск программы: метка автомодуля от прошлого запуска — вернуть принятые опущенным блокам; опрос раз в 2 с —
    /// если включён автомодуль или журнал срезов (выключатели независимы), регулятор — если включён автомодуль.
    pub(super) fn start_auto(&mut self, shell: &Shell) {
        self.load_choice();
        self.replay_memory();
        if self.marker_path().exists() {
            let said = self.with_ceilings(App::recover_auto);
            if let Ok(Some(said)) = said {
                self.queue(&said);
            }
        }
        let choice = self.auto.choice;
        if choice.on || choice.log {
            self.meter.forget();
            shell.set_timer(AUTO, self.config.auto.tick);
        }
        if !choice.log {
            self.journal.note("журнал срезов выключен");
        }
        if choice.on {
            self.auto.controller = Some(self.controller_for());
            self.journal.note(&format!(
                "автомодуль включён: до {} °C{}",
                self.auto.choice.limit,
                if self.auto.choice.step_up {
                    ", с подтягиванием"
                } else {
                    ""
                }
            ));
        }
    }

    /// Память перегрева из журнала действий: записанные снижения повторяются по порядку. Испорченные строки
    /// пропускаются — говорится об этом одной строкой журнала программы.
    pub(crate) fn replay_memory(&mut self) {
        // Журнала нет или он не читается — память пустая, но со сроками из настроек: прежде здесь оставалась память из
        // `Autopilot::new` с настоящими сроками, и скрытая проверка с короткими сроками сравнивала запреты на 30 мин с
        // запретами на 2 с (её шаг «из журнала действий восстанавливаются те же» то проходил, то нет).
        let mut memory = Memory::new(self.config.auto.bans);
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
        let mut bad = 0_usize;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(Value::Object(map)) = serde_json::from_str::<Value>(line) else {
                bad += 1;
                continue;
            };
            if map.get("act").and_then(Value::as_str) != Some("lower") {
                continue;
            }
            let first = map
                .get("first")
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok());
            let ban = map
                .get("ban")
                .and_then(Value::as_i64)
                .and_then(|v| i32::try_from(v).ok());
            let mean = map.get("mean").and_then(Value::as_f64);
            let at = map.get("at").and_then(Value::as_i64);
            match (first, ban, mean, at) {
                (Some(first), Some(ban), Some(mean), Some(at)) => {
                    memory.overheated(first, ban, mean, Stamp(at));
                }
                _ => bad += 1,
            }
        }
        if bad > 0 {
            self.journal.error(&format!(
                "журнал автомодуля: {bad} строк не разобрано, память перегрева без них"
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
        format!(
            "{{\"cpu\": {}, \"lps\": {}, \"accepted\": {}, \"auto\": {}}}",
            files::quote(self.store.cpu()),
            files::ints(&lps),
            files::ints(&accepted),
            files::ints(&overlay)
        )
    }

    /// Переписать метку по надстройке; надстройки нет — убрать метку и отпустить аренду.
    pub(super) fn mark(&mut self) -> Result<(), String> {
        if !self.auto.active() {
            if let Err(e) = files::delete_with_backup(&self.marker_path()) {
                self.journal.error(&format!("метка автомодуля не удалена: {e}"));
            }
            if self.auto.holding {
                self.auto.holding = false;
                self.lease.release();
            }
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
    fn recover_auto(&mut self) -> Option<String> {
        let path = self.marker_path();
        let read = files::read_object(&path).ok().flatten().filter(|map| {
            map.get("cpu").and_then(serde_json::Value::as_str) == Some(self.store.cpu())
                && files::read_ints(map.get("lps")) == Some(self.store.lps().iter().map(|lp| i64::from(lp.0)).collect())
        });
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
        // Отсечка — ползунком в окне (владелец 15.10: пресеты «место занимают только в меню»); здесь — что стоит, и
        // пункт, который открывает окно. Включение и подтягивание остаются: ими владелец пользуется.
        let children = vec![
            item(AUTO_ON, "Включено".into(), c.on),
            item(STEP_UP_ID, "Подтягивать вверх, до заводского".into(), c.step_up),
            super::separator(),
            item(
                AUTO_WINDOW_ID,
                format!("Отсечка {} °C — настроить в окне", c.limit),
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
            STEP_UP_ID => next.step_up = !next.step_up,
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
        if next.on && !was.on {
            // Суммы показаний, набранные журналом или окном до включения, — не для первого решения регулятора.
            self.meter.forget();
        } else if !next.on && was.on {
            self.put_back("автомодуль выключен");
            self.auto.controller = None;
        }
        if next.on && !next.same_auto(was) {
            // Новый выбор — опущенные возвращаются, регулятор начинает заново (прежние окна — при другой отсечке).
            // Сохранить надстройку через смену отсечки мог бы только сам регулятор (machine::auto) — не здесь.
            self.put_back("новые настройки автомодуля");
            self.auto.controller = Some(self.controller_for());
            self.auto.aside.fill(false);
        }
        if !next.log {
            // Журнал выключен — опрос раз в 0,25 с и файл дня кончаются сразу, а не со следующим опросом раз в 2 с.
            self.stop_logging(shell);
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
                "автомодуль ({by}): {}, до {} °C, подтягивание {}",
                if next.on { "включён" } else { "выключен" },
                next.limit,
                if next.step_up { "вкл" } else { "выкл" }
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
        if let Some(controller) = &mut self.auto.controller {
            controller.forget();
        }
    }

    /// Сколько запретов памяти перегрева действует сейчас при нынешнем пределе (скрытая проверка).
    pub(crate) fn bans_now(&self) -> usize {
        let limit = f64::from(self.auto.choice.limit);
        let now = Stamp::now();
        self.topology
            .blocks
            .iter()
            .map(|b| self.auto.memory.active(b.first, limit, now).len())
            .sum()
    }

    /// Строка подсказки: что автомодуль сейчас делает.
    pub(super) fn auto_tip(&self) -> Option<String> {
        if !self.auto.choice.on && !self.auto.active() {
            return None;
        }
        let mut text = format!("авто: до {} °C", self.auto.choice.limit);
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

    /// Опрос по таймеру. Таймер есть, пока включён автомодуль или журнал срезов (`apply_choice`):
    /// - автомодуль включён — как всегда: корпус, при нужде блоки, сверка, решение регулятора; журнал срезов, если он
    ///   включён и блоки с потолками прочитаны;
    /// - автомодуль выключен, журнал включён — процессор занят (≥ `LOADED`): блоки, сверка потолков (чужая правка
    ///   принимается) и журнал; регулятора нет, потолки не меняются; не занят — ничего не читается, журнал стоит.
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
            self.stop_logging(shell);
            self.yield_to(other);
            return;
        }
        if self.take_back_pending || self.auto.wake_pending {
            self.stop_logging(shell);
            return; // потолки ставит на место возврат права или сверка после пробуждения
        }
        let started = Instant::now();
        let share = self.system_share();
        let mut reach = Reach::Nothing;
        let polled = self.logging.on;
        // Журнал срезов пишется, пока он включён, а блоки и потолки прочитаны (то же условие, что и раньше для строки
        // раз в 2 с): опрос раз в 0,25 с — с этого решения до того, которое скажет «не нужно». Потолки не прочитаны —
        // журнал стоит: его опрос открывал бы драйвер, который здесь после ошибки отпускается. Начинается журнал под
        // той же арендой: иначе `with_ceilings` отпустил бы драйвер, который через 0,25 с снова нужен журналу.
        let outcome = if choice.on || share >= LOADED {
            reach = Reach::Package;
            Some(self.with_ceilings(|app| {
                let polled = app.poll(share, &mut reach);
                if reach == Reach::Ceilings && choice.log {
                    app.start_logging(shell);
                }
                polled
            }))
        } else {
            // Автомодуль выключен, процессор не занят: ни регулятору, ни журналу датчики не нужны — драйвер не
            // трогается. Набранное окном — не для следующего раза (как в `sample`, когда блоки не судятся).
            self.meter.forget();
            None
        };
        if !(reach == Reach::Ceilings && choice.log) {
            self.stop_logging(shell);
        }
        let said = match outcome {
            None => None,
            Some(Ok(Polled::Quiet)) => Some(None),
            Some(Ok(Polled::Said(said))) => Some(Some(said)),
            Some(Ok(Polled::Reset)) => {
                // Сброс сном, пробуждения программа не заметила (бывает при современном сне): сверка — сейчас.
                self.auto.wake_pending = true;
                shell.set_timer(super::WAKE, self.config.wake_delay);
                Some(None)
            }
            Some(Err(reason)) => Some(Some(format!("{} ждёт: {reason}", self.poller()))),
        };
        // Опроса датчиков не было (автомодуль выключен, процессор не занят) — сказанное прежде остаётся сказанным:
        // иначе одна и та же беда повторялась бы с каждым новым «процессор занят».
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
            let fans = self
                .auto
                .fans_cost
                .take()
                .map(|cost| format!(", из них обороты {} мкс", cost.as_micros()))
                .unwrap_or_default();
            self.journal.note(&format!(
                "{}: опрос {} мкс{}{fans}",
                match (choice.on, choice.log) {
                    (true, true) => "автомодуль",
                    (true, false) => "автомодуль (журнал срезов выключен)",
                    (false, _) => "автомодуль выключен, журнал срезов",
                },
                started.elapsed().as_micros(),
                match (reach, polled) {
                    (Reach::Nothing, _) => ", процессор не занят — датчики не читаются",
                    (Reach::Package, _) => ", только корпус",
                    (_, true) => ", с блоками (показания опроса срезов)",
                    (_, false) => ", с блоками",
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

    /// Один опрос под арендой: показания, сверка потолков, решение регулятора (если он есть), запись.
    fn poll(&mut self, share: f64, reach: &mut Reach) -> Polled {
        let (package, readings) = match self.sample(share) {
            Ok(Some(sampled)) => sampled,
            Ok(None) => {
                // Блоки не читались: прежние показания больше не окно подряд.
                if let Some(controller) = &mut self.auto.controller {
                    controller.forget();
                }
                return Polled::Quiet;
            }
            Err(said) => return Polled::Said(said),
        };
        *reach = Reach::Blocks;
        let foreign = match self.reconcile() {
            Ok(Some(foreign)) => foreign,
            Ok(None) => return Polled::Reset,
            Err(said) => return Polled::Said(said),
        };
        *reach = Reach::Ceilings;
        // Обороты для условий решения в журнале действий: пока пишется журнал срезов, их читает его опрос; выключенному
        // автомодулю решать нечего — и читать их незачем (вызов прошивки — около 30 мс).
        if self.auto.choice.on && !self.logging.on {
            self.read_fans();
        }
        let mut said = (!foreign.is_empty()).then(|| {
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
        if let Some(refused) = self.act(&readings, package) {
            said = Some(refused);
        }
        // Чужая правка могла снять последний блок надстройки: метка и аренда — по надстройке (ревизия M2).
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        said.map_or(Polled::Quiet, Polled::Said)
    }

    /// Доля работы всего процессора с прошлого опроса по Windows; первый опрос — 0. На имитации — доля работы её ядер
    /// (их нагрузку задаёт скрытая проверка), а не Windows компьютера, где идёт проверка: шаги проверки не должны
    /// зависеть от того, чем он занят (игра владельца, соседние проверки).
    fn system_share(&mut self) -> f64 {
        if self.config.simulate {
            return match &self.device {
                Some(Device::Sim(sim)) if !sim.load.is_empty() => {
                    let cores = f64::from(u32::try_from(sim.load.len()).unwrap_or(u32::MAX));
                    (sim.load.iter().sum::<f64>() / cores).clamp(0.0, 1.0)
                }
                _ => 0.0,
            };
        }
        let now = crate::shell::system_times();
        let share = match (self.auto.system, now) {
            (Some((idle0, total0)), Some((idle1, total1))) if total1 > total0 => {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a share of two tick counts; the error is far below a percent"
                )]
                let idle = idle1.saturating_sub(idle0) as f64 / (total1 - total0) as f64;
                (1.0 - idle).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        self.auto.system = now;
        share
    }

    /// Показания: корпус и, если нужны, блоки — включённому автомодулю, когда корпус близко к пределу или надстройка
    /// есть; ему и журналу срезов — когда процессор нагружен (`share`). Ok(None) — блоки не нужны.
    ///
    /// Пока ядра читает кто-то другой — опрос срезов раз в 0,25 с или окно раз в полсекунды, — у ядра счётчики одни:
    /// показание блока — сумма их чтений с прошлого решения (app/meter.rs), своего чтения нет; корпус — их последнее
    /// чтение, если оно свежее половины опроса. Никто не читает — корпус и блоки читаются здесь, как раньше.
    fn sample(&mut self, share: f64) -> Result<Option<Sampled>, String> {
        let poller = self.poller();
        let Some(device) = self.device.as_mut() else {
            return Ok(None);
        };
        let package = match self.meter.package_within(self.config.auto.tick / 2) {
            Some(read) => read.value,
            None => device
                .thermal()
                .package()
                .map_err(|e| format!("{poller}: температура не прочитана — {e}"))?,
        };
        let overlay = self.auto.active();
        // Выключенный автомодуль регулятора не держит: блоки — только по занятости процессора (журналу срезов).
        let watch = self
            .auto
            .controller
            .as_ref()
            .is_some_and(|c| c.watch_blocks(package, overlay));
        if !watch && share < LOADED {
            // Блоки не судятся: набранное окном — не для следующего решения (оно должно быть по свежему времени).
            self.meter.forget();
            return Ok(None);
        }
        let sensing = self.serving.attached() || self.logging.on;
        let mut readings = Vec::with_capacity(self.topology.blocks.len());
        for (b, block) in self.topology.blocks.iter().enumerate() {
            // Отставленный после чужой правки блок не судится: показаний для регулятора нет. Набранное опросом срезов
            // или окном — прочь, чтобы, когда блок снова под присмотром, первое решение было по свежему времени.
            if self.auto.aside[b] {
                self.meter.drain(block.first);
                readings.push(None);
                continue;
            }
            if !(sensing && self.meter.has(block.first)) {
                if let Ok(reading) = device.thermal().block(block.first) {
                    // Тем же путём, что опрос срезов и окна: открытое окно складывает и это чтение.
                    super::sense::record(&mut self.meter, self.serving.watch_mut(), block.first, reading);
                } else {
                    // Блок не прочитался: в этом опросе его нет (как и раньше); сумма — заново.
                    self.meter.drain(block.first);
                    readings.push(None);
                    continue;
                }
            }
            readings.push(Some(self.meter.drain(block.first)));
        }
        Ok(Some((package, readings)))
    }

    /// Обороты вентиляторов: пока пишется журнал срезов — раз в 2 с (`AutoTiming::fans`), иначе и после отказа — не
    /// чаще раза в 10 с (каждый раз — вызов прошивки), прошивка не открылась — раз в минуту. На имитации прошивка не
    /// трогается. Зовут опрос срезов, опрос автомодуля (пока журнал не пишется) и опрос окна. true — обороты прочитаны
    /// сейчас (они идут в строку среза только тогда).
    pub(super) fn read_fans(&mut self) -> bool {
        let now = Instant::now();
        let every = match self.auto.fans_fault {
            Some(FanFault::Open) => FANS_RETRY,
            None if self.logging.on => self.config.auto.fans,
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
        let started = Instant::now();
        let read = fans.rpm();
        self.auto.fans_cost = Some(started.elapsed());
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

    /// Решение регулятора и запись; снижение ставит запрет в памяти перегрева. Каждое решение — строка журнала
    /// действий с условиями. Возвращает отказ, если он был. Автомодуль выключен — регулятора нет: решать нечего, потолки
    /// не меняются (владелец 16.10: выключатель — про изменение множителей).
    fn act(&mut self, readings: &[Option<Reading>], package: Option<f64>) -> Option<String> {
        self.auto.controller.as_ref()?;
        let topology = &self.topology;
        let limits = self.limits.as_ref()?;
        let intended = self.intended.as_deref()?;
        let ceilings: Vec<i32> = topology.blocks.iter().map(|b| limits.current()[b.first]).collect();
        let fused: Vec<i32> = topology.blocks.iter().map(|b| limits.fused()[b.first]).collect();
        // Ниже этого регулятор блок не опускает: та же граница, что у ручного потолка (`Limits::set` ниже не пишет).
        let floor: Vec<i32> = topology.blocks.iter().map(|b| limits.floor(b.first)).collect();
        // Возвращать регулятор может только то, что сдвинул сам: блок ниже принятого, но не в надстройке, опустил
        // кто-то другой — для регулятора его «принятый» — то, что стоит. Иначе прохладный блок поднимался бы поверх
        // чужой правки (найдено скрытой проверкой 14.10).
        let accepted: Vec<i32> = topology
            .blocks
            .iter()
            .enumerate()
            .map(|(b, block)| {
                let base = intended[block.first];
                if self.auto.overlay[b].is_some() {
                    base
                } else {
                    ceilings[b].min(base)
                }
            })
            .collect();
        // Блок, у которого потолок расходится с ожидаемым и ещё не подтверждён как чужая правка, не судится.
        let judged: Vec<Option<Reading>> = readings
            .iter()
            .zip(&self.auto.mismatch)
            .map(|(reading, &mismatch)| if mismatch > 0 { None } else { *reading })
            .collect();
        let now = self.auto.origin.elapsed();
        let stamp = Stamp::now();
        let limit = f64::from(self.auto.choice.limit);
        let frame = Frame {
            readings: &judged,
            ceilings: &ceilings,
            accepted: &accepted,
            fused: &fused,
            floor: &floor,
        };
        let Autopilot { controller, memory, .. } = &mut self.auto;
        let banned = |b: usize, to: i32| {
            topology
                .blocks
                .get(b)
                .is_some_and(|block| memory.blocking(block.first, to, limit, stamp).is_some())
        };
        let decision = controller.as_mut()?.step(now, &frame, &banned)?;
        let (b, to) = match decision.action {
            Action::Lower { block, to } | Action::Raise { block, to } => (block, to),
        };
        let name = topology.blocks[b].name.clone();
        let first = topology.blocks[b].first;
        let before = ceilings[b];
        let base = accepted[b];
        let was = self.auto.overlay[b];
        // Метка — до записи в процессор: с новой надстройкой.
        self.auto.overlay[b] = (to != base).then_some(to);
        if let Err(e) = self.mark() {
            self.auto.overlay[b] = was;
            return Some(format!("Автомодуль: метка не записана, потолок не тронут — {e}"));
        }
        let topology = &self.topology;
        let (limits, cpu) = parts(&mut self.limits, &mut self.device)?;
        if let Some(refusal) = limits.set(cpu, topology, first, to) {
            self.auto.overlay[b] = was;
            return Some(format!("Автомодуль: потолок блока {name} не изменён — {refusal}"));
        }
        let change = Change {
            name,
            first,
            before,
            to,
            base,
        };
        self.record(&decision, &change, stamp, readings, package);
        None
    }

    /// Сделанное решение: запрет в памяти перегрева (снижение), строка журнала программы и строка журнала действий.
    fn record(
        &mut self,
        decision: &machine::auto::Decision,
        change: &Change,
        stamp: Stamp,
        readings: &[Option<Reading>],
        package: Option<f64>,
    ) {
        let Change {
            name,
            first,
            before,
            to,
            base,
        } = change;
        let window = decision.window;
        let mean = window.map_or_else(|| "?".into(), |w| format!("{:.0}", w.mean));
        let ban = decision
            .ban
            .zip(window)
            .map(|(at, w)| self.auto.memory.overheated(*first, at, w.mean, stamp));
        let (act, text) = match decision.action {
            Action::Lower { .. } => (
                "lower",
                format!(
                    "автомодуль: блок {name} опущен ×{before} → ×{to} ({mean} °C, наибольшая {}){}",
                    window.map_or_else(|| "?".into(), |w| format!("{:.0}", w.max)),
                    ban.map(|ban| format!(
                        "; ×{} и выше — не раньше {} ({}-й перегрев)",
                        ban.at,
                        ban.until.local(Pattern::Time),
                        ban.strikes
                    ))
                    .unwrap_or_default()
                ),
            ),
            Action::Raise { .. } if to <= base => (
                "raise",
                format!(
                    "автомодуль: блок {name} возвращён ×{before} → ×{to} (прохладен под нагрузкой {} с)",
                    decision.cool.as_secs()
                ),
            ),
            Action::Raise { .. } => (
                "raise",
                format!(
                    "автомодуль: блок {name} поднят выше принятого ×{before} → ×{to} (прохладен под нагрузкой {} с, \
                     ×{base} принятый)",
                    decision.cool.as_secs()
                ),
            ),
        };
        self.journal.note(&text);
        let mut line = self.conditions(stamp, act, readings, package);
        line.insert("block".into(), json!(name));
        line.insert("first".into(), json!(first));
        line.insert("from".into(), json!(before));
        line.insert("to".into(), json!(to));
        line.insert("accepted".into(), json!(base));
        line.insert("limit".into(), json!(self.auto.choice.limit));
        if let Some(w) = window {
            line.insert("mean".into(), json!(round(w.mean, 1)));
            line.insert("max".into(), json!(round(w.max, 1)));
            line.insert("busy".into(), json!(round(w.busy, 2)));
            line.insert("ratio".into(), json!(w.ratio.map(|r| round(r, 2))));
        }
        line.insert("cool".into(), json!(decision.cool.as_secs()));
        if let Some(ban) = ban {
            line.insert("ban".into(), json!(ban.at));
            line.insert("until".into(), json!(ban.until.roundtrip()));
            line.insert("strikes".into(), json!(ban.strikes));
        }
        self.log_line(&Value::Object(line));
    }

    /// Условия для строки журнала действий: время, что сделано, корпус, вентиляторы, по блокам потолок, фактический
    /// множитель, доля работы, температура (соседи читаются отсюда).
    fn conditions(
        &self,
        stamp: Stamp,
        act: &str,
        readings: &[Option<Reading>],
        package: Option<f64>,
    ) -> serde_json::Map<String, Value> {
        let column = |f: fn(&Reading) -> Option<f64>, digits: i32| -> Vec<Option<f64>> {
            readings
                .iter()
                .map(|r| r.as_ref().and_then(f).map(|v| round(v, digits)))
                .collect()
        };
        let ceilings: Vec<Option<i32>> = self
            .topology
            .blocks
            .iter()
            .map(|b| self.limits.as_ref().map(|l| l.current()[b.first]))
            .collect();
        let mut map = serde_json::Map::new();
        map.insert("time".into(), json!(stamp.roundtrip()));
        map.insert("at".into(), json!(stamp.0));
        map.insert("act".into(), json!(act));
        map.insert("package".into(), json!(package.map(|p| round(p, 0))));
        map.insert("fans".into(), json!(self.auto.rpm));
        map.insert("ceilings".into(), json!(ceilings));
        map.insert("temps".into(), json!(column(|r| r.temp, 0)));
        map.insert("ratios".into(), json!(column(|r| r.ratio, 2)));
        map.insert("busys".into(), json!(column(|r| r.busy, 2)));
        map
    }

    pub(super) fn log_line(&mut self, line: &Value) {
        if let Err(e) = files::append_line(&self.log_path(), &line.to_string()) {
            self.journal.error(&format!("журнал автомодуля не дописан: {e}"));
        }
    }

    /// Вернуть принятые потолки блокам надстройки (C#-часть стартует, сон, выход, смена набора, новые настройки).
    /// Пишет без проверки присутствия: стартующая C#-часть ждёт аренду, которую держит эта программа, и ещё ничего
    /// не прочитала. Блок, которому вернуть не удалось, остаётся опущенным — с меткой и арендой (ревизия M1).
    pub(super) fn put_back(&mut self, why: &str) {
        if !self.auto.active() {
            return;
        }
        let topology = &self.topology;
        let intended = self.intended.clone().unwrap_or_default(); // a few dozen numbers, once per put-back
        let mut back = Vec::new();
        if let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
            for (b, block) in topology.blocks.iter().enumerate() {
                if self.auto.overlay[b].is_none() {
                    continue;
                }
                let Some(&want) = intended.get(block.first) else {
                    continue;
                };
                match limits.set(cpu, topology, block.first, want) {
                    None => {
                        self.auto.overlay[b] = None;
                        back.push(format!("{} ×{want}", block.name));
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
            self.put_back(why);
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
    // Настройки автомодуля по плану (DECISIONS #32, шаг 2; #38): отсечка 60–95 °C с шагом 1 °C, подтягивание по
    // умолчанию выключено, глубины нет; негодное в файле — значение по умолчанию у этого поля, в команде — отказ.
    use super::*;

    fn map(text: &str) -> Map<String, Value> {
        serde_json::from_str(text).unwrap_or_default()
    }

    #[test]
    fn file_of_the_previous_build_is_read_as_is_and_its_depth_ignored() {
        // Файл, записанный сборкой с глубиной (0.5.3 и раньше): «depth» любого вида не мешает остальному.
        for text in [
            r#"{"auto":true,"limit":75,"depth":6,"stepUp":true}"#,
            r#"{"auto":true,"limit":75,"depth":99,"stepUp":true}"#,
            r#"{"auto":true,"limit":75,"depth":"глубоко","stepUp":true}"#,
        ] {
            assert_eq!(
                Choice::read(&map(text)),
                Choice {
                    on: true,
                    limit: 75,
                    step_up: true,
                    log: true,
                },
                "{text}: a file without «log» (0.5.4 and earlier) keeps the samples log on"
            );
        }
    }

    #[test]
    fn the_file_is_written_without_depth() {
        let text = Choice {
            on: true,
            limit: 77,
            step_up: false,
            log: false,
        }
        .file_text();
        assert!(!text.contains("depth"), "{text}");
        assert_eq!(text, r#"{"auto":true,"limit":77,"log":false,"stepUp":false}"#);
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
            step_up: true,
            log: true,
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
        assert_eq!(Choice::read(&map(r#"{"auto":"да","stepUp":1}"#)), d);
        // Края промежутка — годные.
        assert_eq!(Choice::read(&map(r#"{"limit":60}"#)).limit, 60);
        assert_eq!(Choice::read(&map(r#"{"limit":95}"#)).limit, 95);
    }

    #[test]
    fn a_command_changes_only_what_it_names() {
        let was = Choice {
            on: true,
            limit: 80,
            step_up: false,
            log: true,
        };
        let next = was.changed(&map(r#"{"id":7,"c":"auto.set","limit":82}"#));
        assert_eq!(next, Ok(Choice { limit: 82, ..was }));
        let next = was.changed(&map(r#"{"stepUp":true,"on":false}"#));
        assert_eq!(
            next,
            Ok(Choice {
                on: false,
                step_up: true,
                ..was
            })
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
            r#"{"stepUp":"нет"}"#,
            // глубины больше нет: окно прежней сборки получает отказ, а не молчаливое «принято»
            r#"{"depth":4}"#,
            // годное рядом с негодным не применяется частично
            r#"{"on":true,"limit":200}"#,
            // опечатка в имени не должна молча ничего не сделать
            r#"{"limt":80}"#,
            // и рядом с годной: годная не применяется, а опечатка не теряется молча
            r#"{"limit":80,"stepUP":true}"#,
            // ни одной настройки
            r#"{"id":3,"c":"auto.set"}"#,
            r"{}",
        ] {
            assert!(was.changed(&map(text)).is_err(), "{text} must be refused");
        }
    }

    #[test]
    fn the_topic_carries_the_choice_and_the_range() {
        let choice = Choice {
            on: true,
            limit: 77,
            step_up: true,
            log: false,
        };
        let topic: Value = serde_json::from_str(&choice.topic()).unwrap_or(Value::Null);
        assert_eq!(topic["t"], "settings");
        assert_eq!(
            topic["auto"],
            json!({ "on": true, "limit": 77, "stepUp": true, "limitRange": [60, 95] })
        );
        assert_eq!(topic["log"], json!({ "on": false }), "the log switch has its own field");
        // Файл и команда говорят одними именами: записанное читается обратно тем же.
        assert_eq!(Choice::read(&map(&choice.file_text())), choice);
    }
}
