// Автомодуль в программе входа (DECISIONS #31, «Auto module» и его подзапись; #32): раз в 2 с — температура корпуса;
// близко к пределу, пока какой-то блок не на принятом или пока процессор нагружен (доля работы по Windows) —
// показания всех блоков и решение регулятора (machine::auto). Выключен — таймера нет, ничего не читается, драйвер
// закрыт между действиями.
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
/// Пункты подменю: включено; подтягивание вверх; отсечка и глубина — открыть окно (там они настраиваются).
pub const AUTO_ON: u32 = 10;
pub const STEP_UP_ID: u32 = 11;
pub const AUTO_WINDOW_ID: u32 = 12;
/// Отсечка, °C, — ползунок в окне с шагом 1 °C (план DECISIONS #32, шаг 2; вместо пресетов 75–95 в меню).
pub const LIMIT_RANGE: RangeInclusive<i32> = 60..=95;
/// Глубина, ступеней вниз от фактического множителя: ступень — 2–5 °C у занятого блока (замер 14.10), 8 ступеней —
/// 16–40 °C, больше защите не нужно. Пресеты меню были 2/4/6 — все внутри.
pub const DEPTH_RANGE: RangeInclusive<i32> = 1..=8;
/// Как часто писать в журнал цену опроса — так она измеряется на машине владельца.
pub(super) const COST_EVERY: Duration = Duration::from_secs(60);
/// Столько чтений подряд потолок блока должен расходиться с ожидаемым, чтобы считаться чужой правкой: одно чтение
/// бывает и запоздалым ответом на собственную запись, и чужим запросом в ящике (`ThrottleStop` не берёт наш замок).
const FOREIGN_READS: u8 = 2;
/// С такой доли работы всего процессора (по Windows) блоки читаются, даже если корпус далеко от предела: для журнала
/// срезов и для подтягивания нужны блоки под нагрузкой. Простой Windows — 1–5 %; число — выбор агента, не мерено.
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
    pub control: Timing,
    pub bans: Spans,
}

impl AutoTiming {
    pub const REAL: Self = Self {
        tick: Duration::from_secs(2),
        sample: Duration::from_millis(250),
        fans: Duration::from_secs(2),
        control: Timing::REAL,
        bans: Spans::REAL,
    };
}

/// Выбор владельца, как он лежит в data\tray.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub on: bool,
    pub limit: i32,
    pub depth: i32,
    /// Подтягивать прохладные блоки выше принятого, до заводского (DECISIONS #32; по умолчанию выключено).
    pub step_up: bool,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            on: false,
            limit: 85,
            depth: 4,
            step_up: false,
        }
    }
}

/// Целое из JSON в допустимом промежутке: 85.0, «85» и 96 — не годятся (шаг — 1 °C, ступень — целая).
fn whole(value: &Value, range: &RangeInclusive<i32>) -> Option<i32> {
    value
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .filter(|v| range.contains(v))
}

impl Choice {
    /// Выбор из data\tray.json. Каждое поле — само по себе: негодное или пропавшее берёт значение по умолчанию, а
    /// годные соседние остаются (файл прежней сборки с пресетами 75–95 и 2/4/6 читается как есть).
    #[must_use]
    pub fn read(map: &Map<String, Value>) -> Self {
        let default = Self::default();
        Self {
            on: map.get("auto").and_then(Value::as_bool).unwrap_or(default.on),
            limit: map
                .get("limit")
                .and_then(|v| whole(v, &LIMIT_RANGE))
                .unwrap_or(default.limit),
            depth: map
                .get("depth")
                .and_then(|v| whole(v, &DEPTH_RANGE))
                .unwrap_or(default.depth),
            step_up: map.get("stepUp").and_then(Value::as_bool).unwrap_or(default.step_up),
        }
    }

    /// Команда окна «auto.set»: какие поля пришли — те и меняются, остальные прежние. Отказ, если хоть одно поле
    /// негодно или незнакомо (опечатка в имени не должна молча ничего не сделать), или настроек в команде нет.
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
                "depth" => {
                    next.depth = whole(value, &DEPTH_RANGE).ok_or_else(|| {
                        format!(
                            "глубина — целое число ступеней от {} до {}",
                            DEPTH_RANGE.start(),
                            DEPTH_RANGE.end()
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
        json!({ "auto": self.on, "limit": self.limit, "depth": self.depth, "stepUp": self.step_up }).to_string()
    }

    /// Тема окну «settings»: выбор и допустимые промежутки — их держит эта программа, окно по ним строит ползунки.
    #[must_use]
    pub fn topic(self) -> String {
        json!({
            "t": "settings",
            "auto": {
                "on": self.on, "limit": self.limit, "depth": self.depth, "stepUp": self.step_up,
                "limitRange": [LIMIT_RANGE.start(), LIMIT_RANGE.end()],
                "depthRange": [DEPTH_RANGE.start(), DEPTH_RANGE.end()],
            },
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
                depth: c.depth,
                step_up: c.step_up,
            },
            self.config.auto.control,
            self.topology.blocks.len(),
        )
    }

    /// Запуск программы: метка автомодуля от прошлого запуска — вернуть принятые опущенным блокам; включён — опрос.
    pub(super) fn start_auto(&mut self, shell: &Shell) {
        self.load_choice();
        self.replay_memory();
        if self.marker_path().exists() {
            let said = self.with_ceilings(App::recover_auto);
            if let Ok(Some(said)) = said {
                self.queue(&said);
            }
        }
        if self.auto.choice.on {
            self.auto.controller = Some(self.controller_for());
            self.meter.forget();
            shell.set_timer(AUTO, self.config.auto.tick);
            self.journal.note(&format!(
                "автомодуль включён: до {} °C, глубина {}{}",
                self.auto.choice.limit,
                self.auto.choice.depth,
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
        // Отсечка и глубина — ползунками в окне (владелец 15.10: пресеты «место занимают только в меню»); здесь —
        // что стоит, и пункт, который открывает окно. Включение и подтягивание остаются: ими владелец пользуется.
        let children = vec![
            item(AUTO_ON, "Включено".into(), c.on),
            item(STEP_UP_ID, "Подтягивать вверх, до заводского".into(), c.step_up),
            super::separator(),
            item(
                AUTO_WINDOW_ID,
                format!("Отсечка {} °C, глубина {} — настроить в окне", c.limit, c.depth),
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

    /// Пункт подменю. true — пункт автомодуля (разобран здесь).
    pub(super) fn auto_command(&mut self, shell: &mut Shell, id: u32) -> bool {
        let mut next = self.auto.choice;
        match id {
            STEP_UP_ID => next.step_up = !next.step_up,
            AUTO_ON => next.on = !next.on,
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
        match self.auto.choice.changed(fields) {
            Err(why) => Some(why),
            Ok(next) => {
                if next != self.auto.choice {
                    self.apply_choice(shell, next, "окно");
                }
                None
            }
        }
    }

    /// Новый выбор владельца — единственный путь его смены (меню и окно): включение и выключение опроса, возврат
    /// опущенных блоков, новый регулятор, data\tray.json, строка журнала. Окно узнаёт о нём темой «settings»
    /// (app/serve.rs, по изменению) и полем autoLimit состояния.
    fn apply_choice(&mut self, shell: &Shell, next: Choice, by: &str) {
        let was = std::mem::replace(&mut self.auto.choice, next);
        if next.on && !was.on {
            // Суммы показаний, набранные окном до включения, — не для первого решения регулятора.
            self.meter.forget();
            shell.set_timer(AUTO, self.config.auto.tick);
        } else if !next.on && was.on {
            shell.kill_timer(AUTO);
            self.stop_logging(shell);
            self.put_back("автомодуль выключен");
            self.auto.controller = None;
            // Драйвер нужен ещё окну, если оно открыто (его опрос раз в полсекунду).
            if !self.serving.attached()
                && let Some(device) = &mut self.device
            {
                device.cpu().release();
            }
        }
        if next.on {
            // Новый выбор — опущенные возвращаются, регулятор начинает заново (прежние окна — при другой отсечке).
            // Сохранить надстройку через смену отсечки мог бы только сам регулятор (machine::auto) — не здесь.
            self.put_back("новые настройки автомодуля");
            self.auto.controller = Some(self.controller_for());
            self.auto.aside.fill(false);
        }
        self.save_choice();
        self.journal.note(&format!(
            "автомодуль ({by}): {}, до {} °C, глубина {}, подтягивание {}",
            if next.on { "включён" } else { "выключен" },
            next.limit,
            next.depth,
            if next.step_up { "вкл" } else { "выкл" }
        ));
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

    /// Опрос по таймеру.
    pub(super) fn auto_tick(&mut self, shell: &mut Shell) {
        if !self.auto.choice.on {
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
        let mut reach = Reach::Package;
        let polled = self.logging.on;
        let outcome = self.with_ceilings(|app| app.poll(&mut reach));
        // Журнал срезов пишется, пока блоки судятся и потолки прочитаны (то же условие, что и раньше для строки раз в
        // 2 с): опрос раз в 0,25 с — с этого решения до того, которое скажет «не нужно». Потолки не прочитаны — журнал
        // стоит: его опрос открывал бы драйвер, который здесь после ошибки отпускается.
        if reach == Reach::Ceilings {
            self.start_logging(shell);
        } else {
            self.stop_logging(shell);
        }
        let said = match outcome {
            Ok(Polled::Quiet) => None,
            Ok(Polled::Said(said)) => Some(said),
            Ok(Polled::Reset) => {
                // Сброс сном, пробуждения программа не заметила (бывает при современном сне): сверка — сейчас.
                self.auto.wake_pending = true;
                shell.set_timer(super::WAKE, self.config.wake_delay);
                None
            }
            Err(reason) => Some(format!("Автомодуль ждёт: {reason}")),
        };
        if let Some(said) = said {
            // Одна и та же беда — одно сообщение, а не каждые 2 с (ревизия M5); драйвер — переоткрыть.
            if self.auto.last_said.as_deref() != Some(said.as_str()) {
                self.notify(shell, &said);
                self.auto.last_said = Some(said);
            }
            if let Some(device) = &mut self.device {
                device.cpu().release();
            }
        } else {
            self.auto.last_said = None;
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
                "автомодуль: опрос {} мкс{}{fans}",
                started.elapsed().as_micros(),
                match (reach, polled) {
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

    /// Один опрос под арендой: показания, сверка потолков, решение регулятора, запись.
    fn poll(&mut self, reach: &mut Reach) -> Polled {
        let (package, readings) = match self.sample() {
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
        // Обороты для условий решения в журнале действий: пока пишется журнал срезов, их читает его опрос.
        if !self.logging.on {
            self.read_fans();
        }
        let mut said = (!foreign.is_empty()).then(|| {
            format!(
                "Потолки блоков {} изменила другая программа — автомодуль их больше не трогает",
                foreign.join(", ")
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

    /// Доля работы всего процессора с прошлого опроса по Windows; первый опрос — 0.
    fn system_share(&mut self) -> f64 {
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

    /// Показания: корпус и, если нужны, блоки — корпус близко к пределу, надстройка есть или процессор нагружен.
    /// Ok(None) — блоки не нужны.
    ///
    /// Пока ядра читает кто-то другой — опрос срезов раз в 0,25 с или окно раз в полсекунды, — у ядра счётчики одни:
    /// показание блока — сумма их чтений с прошлого решения (app/meter.rs), своего чтения нет; корпус — их последнее
    /// чтение, если оно свежее половины опроса. Никто не читает — корпус и блоки читаются здесь, как раньше.
    fn sample(&mut self) -> Result<Option<Sampled>, String> {
        let share = self.system_share();
        let Some(device) = self.device.as_mut() else {
            return Ok(None);
        };
        let package = match self.meter.package_within(self.config.auto.tick / 2) {
            Some(read) => read.value,
            None => device
                .thermal()
                .package()
                .map_err(|e| format!("Автомодуль: температура не прочитана — {e}"))?,
        };
        let overlay = self.auto.active();
        let Some(controller) = self.auto.controller.as_ref() else {
            return Ok(None);
        };
        if !controller.watch_blocks(package, overlay) && share < LOADED {
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
        let topology = &self.topology;
        let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) else {
            return Ok(Some(Vec::new()));
        };
        if let Some(unread) = limits.refresh_blocks(cpu, topology) {
            return Err(format!("Автомодуль: потолки не прочитаны — {unread}"));
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
    /// действий с условиями. Возвращает отказ, если он был.
    fn act(&mut self, readings: &[Option<Reading>], package: Option<f64>) -> Option<String> {
        let topology = &self.topology;
        let limits = self.limits.as_ref()?;
        let intended = self.intended.as_deref()?;
        let ceilings: Vec<i32> = topology.blocks.iter().map(|b| limits.current()[b.first]).collect();
        let fused: Vec<i32> = topology.blocks.iter().map(|b| limits.fused()[b.first]).collect();
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
    // Настройки автомодуля по плану (DECISIONS #32, шаг 2): отсечка 60–95 °C с шагом 1 °C, глубина — целые ступени,
    // подтягивание по умолчанию выключено; негодное в файле — значение по умолчанию у этого поля, в команде — отказ.
    use super::*;

    fn map(text: &str) -> Map<String, Value> {
        serde_json::from_str(text).unwrap_or_default()
    }

    #[test]
    fn file_of_the_previous_build_is_read_as_is() {
        // Файл, записанный сборкой с пресетами меню: все значения внутри новых промежутков.
        let c = Choice::read(&map(r#"{"auto":true,"limit":75,"depth":6,"stepUp":true}"#));
        assert_eq!(
            c,
            Choice {
                on: true,
                limit: 75,
                depth: 6,
                step_up: true
            }
        );
    }

    #[test]
    fn a_bad_field_in_the_file_falls_back_alone() {
        let d = Choice::default();
        for (text, limit, depth) in [
            (r#"{"auto":true,"limit":59,"depth":4}"#, d.limit, 4),
            (r#"{"auto":true,"limit":96,"depth":4}"#, d.limit, 4),
            (r#"{"auto":true,"limit":85.5,"depth":4}"#, d.limit, 4),
            (r#"{"auto":true,"limit":"80","depth":4}"#, d.limit, 4),
            (r#"{"auto":true,"limit":70,"depth":0}"#, 70, d.depth),
            (r#"{"auto":true,"limit":70,"depth":99}"#, 70, d.depth),
            (r#"{"auto":true,"limit":70}"#, 70, d.depth),
        ] {
            let c = Choice::read(&map(text));
            assert!(c.on, "{text}: the good neighbour stays");
            assert_eq!((c.limit, c.depth), (limit, depth), "{text}");
        }
        // Ничего годного: всё по умолчанию, автомодуль выключен.
        assert_eq!(Choice::read(&map(r#"{"auto":"да","stepUp":1}"#)), d);
        // Края промежутков — годные.
        let edges = Choice::read(&map(r#"{"limit":60,"depth":1}"#));
        assert_eq!((edges.limit, edges.depth), (60, 1));
        let edges = Choice::read(&map(r#"{"limit":95,"depth":8}"#));
        assert_eq!((edges.limit, edges.depth), (95, 8));
    }

    #[test]
    fn a_command_changes_only_what_it_names() {
        let was = Choice {
            on: true,
            limit: 80,
            depth: 4,
            step_up: false,
        };
        let next = was.changed(&map(r#"{"id":7,"c":"auto.set","limit":82}"#));
        assert_eq!(next, Ok(Choice { limit: 82, ..was }));
        let next = was.changed(&map(r#"{"depth":8,"stepUp":true,"on":false}"#));
        assert_eq!(
            next,
            Ok(Choice {
                on: false,
                depth: 8,
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
            r#"{"depth":0}"#,
            r#"{"depth":9}"#,
            r#"{"depth":2.5}"#,
            r#"{"on":1}"#,
            r#"{"stepUp":"нет"}"#,
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
    fn the_topic_carries_the_choice_and_the_ranges() {
        let topic: Value = serde_json::from_str(
            &Choice {
                on: true,
                limit: 77,
                depth: 3,
                step_up: true,
            }
            .topic(),
        )
        .unwrap_or(Value::Null);
        assert_eq!(topic["t"], "settings");
        assert_eq!(
            topic["auto"],
            json!({ "on": true, "limit": 77, "depth": 3, "stepUp": true, "limitRange": [60, 95], "depthRange": [1, 8] })
        );
        // Файл и команда говорят одними именами: записанное читается обратно тем же.
        let back = Choice::read(&map(&Choice {
            on: true,
            limit: 77,
            depth: 3,
            step_up: true,
        }
        .file_text()));
        assert_eq!((back.on, back.limit, back.depth, back.step_up), (true, 77, 3, true));
    }
}
