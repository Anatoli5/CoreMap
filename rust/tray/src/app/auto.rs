// Автомодуль в программе входа (DECISIONS #31, «Auto module» и его подзапись; #32): раз в 2 с — температура корпуса;
// близко к пределу, пока какой-то блок не на принятом или пока процессор нагружен (доля работы по Windows) —
// показания всех блоков и решение регулятора (machine::auto). Выключен — таймера нет, ничего не читается, драйвер
// закрыт между действиями.
//
// Память перегрева (machine::memory) не пускает блок обратно на ступень, где он перегрелся, пока не выйдет запрет; она
// восстанавливается при запуске из журнала действий data\auto-log.jsonl — там каждое решение с условиями: средняя и
// наибольшая температура блока, фактический множитель, загрузка, температуры всех блоков (соседи), корпус,
// вентиляторы. Пока блоки читаются, каждые 2 с пишется срез в data\samples\<дата>.csv — «матрица» владельца для
// будущих кривых; срезы старше 30 дней удаляются.
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
use std::time::{Duration, Instant};

use machine::auto::{Action, Controller, Frame, Settings, Timing};
use machine::clock::{Pattern, Stamp};
use machine::files;
use machine::memory::{Memory, Spans};
use machine::thermal::{Reading, Thermal};
use serde_json::{Value, json};

use super::{App, Device, Item, Other, Shell, at_boot, parts};
use crate::fans::{Fans, HpFans, SimFans};

/// Таймер опроса.
pub const AUTO: usize = 4;
/// Пункты подменю: включено; подтягивание вверх; предел (20 + номер в `LIMITS`); глубина (30 + номер в `DEPTHS`).
pub const AUTO_ON: u32 = 10;
pub const STEP_UP_ID: u32 = 11;
pub const LIMIT_ID: u32 = 20;
pub const DEPTH_ID: u32 = 30;
pub const LIMITS: [i32; 5] = [75, 80, 85, 90, 95];
pub const DEPTHS: [i32; 3] = [2, 4, 6];
/// Как часто писать в журнал цену опроса — так она измеряется на машине владельца.
const COST_EVERY: Duration = Duration::from_secs(60);
/// Столько чтений подряд потолок блока должен расходиться с ожидаемым, чтобы считаться чужой правкой: одно чтение
/// бывает и запоздалым ответом на собственную запись, и чужим запросом в ящике (`ThrottleStop` не берёт наш замок).
const FOREIGN_READS: u8 = 2;
/// С такой доли работы всего процессора (по Windows) блоки читаются, даже если корпус далеко от предела: для журнала
/// срезов и для подтягивания нужны блоки под нагрузкой. Простой Windows — 1–5 %; число — выбор агента, не мерено.
const LOADED: f64 = 0.10;
/// Обороты читаются не чаще: каждый раз — вызов прошивки.
const FANS_EVERY: Duration = Duration::from_secs(10);
/// Прошивка не открылась — следующая попытка через столько.
const FANS_RETRY: Duration = Duration::from_secs(60);
/// Срезы хранятся столько дней.
const KEEP_DAYS: i64 = 30;

/// Сроки автомодуля: опрос и сроки регулятора. Скрытая проверка их укорачивает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoTiming {
    pub tick: Duration,
    pub control: Timing,
    pub bans: Spans,
}

impl AutoTiming {
    pub const REAL: Self = Self {
        tick: Duration::from_secs(2),
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
    /// Когда читать обороты (или открывать прошивку) в следующий раз.
    fans_next: Option<Instant>,
    /// Последние прочитанные обороты; None — не известны. Их же видит окно.
    pub(super) rpm: Option<Vec<u32>>,
    fans_said: Option<String>,
    /// Последняя ошибка записи журналов: одна строка журнала программы на одну и ту же.
    files_said: Option<String>,
    fans_cost: Option<Duration>,
    /// День, в чей файл пишутся срезы.
    samples_day: String,
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
            fans_next: None,
            rpm: None,
            fans_said: None,
            files_said: None,
            fans_cost: None,
            samples_day: String::new(),
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
        let read = files::read_object(&self.settings_path());
        let map = match read {
            Ok(Some(map)) => map,
            Ok(None) => return,
            Err(spoiled) => {
                self.journal
                    .error(&format!("настройки автомодуля не прочитаны: {spoiled}"));
                return;
            }
        };
        let number = |key: &str, allowed: &[i32], fallback: i32| {
            map.get(key)
                .and_then(serde_json::Value::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| allowed.contains(v))
                .unwrap_or(fallback)
        };
        let default = Choice::default();
        self.auto.choice = Choice {
            on: map.get("auto").and_then(serde_json::Value::as_bool).unwrap_or(false),
            limit: number("limit", &LIMITS, default.limit),
            depth: number("depth", &DEPTHS, default.depth),
            step_up: map.get("stepUp").and_then(serde_json::Value::as_bool).unwrap_or(false),
        };
    }

    fn save_choice(&self) {
        let c = self.auto.choice;
        let text = json!({ "auto": c.on, "limit": c.limit, "depth": c.depth, "stepUp": c.step_up }).to_string();
        if let Err(e) = files::write_replacing(&self.settings_path(), &text) {
            self.journal.error(&format!("настройки автомодуля не записаны: {e}"));
        }
    }

    fn controller_for(&self) -> Controller {
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
        let text = match std::fs::read_to_string(self.log_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.journal.error(&format!("журнал автомодуля не прочитан: {e}"));
                return;
            }
        };
        let mut memory = Memory::new(self.config.auto.bans);
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
        let mut children = vec![
            item(AUTO_ON, "Включено".into(), c.on),
            item(STEP_UP_ID, "Подтягивать вверх, до заводского".into(), c.step_up),
            super::separator(),
        ];
        for (k, &limit) in LIMITS.iter().enumerate() {
            let id = LIMIT_ID + u32::try_from(k).unwrap_or(0);
            children.push(item(id, format!("Предел {limit} °C"), limit == c.limit));
        }
        children.push(super::separator());
        for (k, &depth) in DEPTHS.iter().enumerate() {
            let id = DEPTH_ID + u32::try_from(k).unwrap_or(0);
            let steps = if depth == 2 {
                "ступени"
            } else {
                "ступеней"
            };
            children.push(item(id, format!("Глубина: до {depth} {steps} вниз"), depth == c.depth));
        }
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
        let limit = id
            .checked_sub(LIMIT_ID)
            .and_then(|k| LIMITS.get(usize::try_from(k).ok()?).copied());
        let depth = id
            .checked_sub(DEPTH_ID)
            .and_then(|k| DEPTHS.get(usize::try_from(k).ok()?).copied());
        match (id, limit, depth) {
            (STEP_UP_ID, ..) => self.auto.choice.step_up = !self.auto.choice.step_up,
            (AUTO_ON, ..) => {
                self.auto.choice.on = !self.auto.choice.on;
                if self.auto.choice.on {
                    // Суммы показаний, набранные окном до включения, — не для первого решения регулятора.
                    self.meter.forget();
                    shell.set_timer(AUTO, self.config.auto.tick);
                } else {
                    shell.kill_timer(AUTO);
                    self.put_back("автомодуль выключен");
                    self.auto.controller = None;
                    // Драйвер нужен ещё окну, если оно открыто (его опрос раз в полсекунду).
                    if !self.serving.attached()
                        && let Some(device) = &mut self.device
                    {
                        device.cpu().release();
                    }
                }
            }
            (_, Some(limit), _) if id < DEPTH_ID => self.auto.choice.limit = limit,
            (_, _, Some(depth)) => self.auto.choice.depth = depth,
            _ => return false,
        }
        if self.auto.choice.on {
            // Новый выбор — опущенные возвращаются, регулятор начинает заново (прежние окна — при другом пределе).
            self.put_back("новые настройки автомодуля");
            self.auto.controller = Some(self.controller_for());
            self.auto.aside.fill(false);
        }
        self.save_choice();
        let c = self.auto.choice;
        self.journal.note(&format!(
            "автомодуль: {}, до {} °C, глубина {}, подтягивание {}",
            if c.on { "включён" } else { "выключен" },
            c.limit,
            c.depth,
            if c.step_up { "вкл" } else { "выкл" }
        ));
        true
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
            self.yield_to(other);
            return;
        }
        if self.take_back_pending || self.auto.wake_pending {
            return; // потолки ставит на место возврат права или сверка после пробуждения
        }
        let started = Instant::now();
        let mut watched_blocks = false;
        let outcome = self.with_ceilings(|app| app.poll(&mut watched_blocks));
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
                if watched_blocks {
                    ", с блоками"
                } else {
                    ", только корпус"
                }
            ));
        }
        let tip = self.tip();
        if tip != self.auto.last_tip {
            shell.set_tip(&tip);
            self.auto.last_tip = tip;
        }
    }

    /// Один опрос под арендой: показания, сверка потолков, срез в журнал, решение регулятора, запись.
    fn poll(&mut self, watched_blocks: &mut bool) -> Polled {
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
        *watched_blocks = true;
        let foreign = match self.reconcile() {
            Ok(Some(foreign)) => foreign,
            Ok(None) => return Polled::Reset,
            Err(said) => return Polled::Said(said),
        };
        self.read_fans();
        self.write_sample(package, &readings);
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
    /// Пока открыто окно, оно читает все ядра раз в полсекунду, и у ядра счётчики одни: показание блока — сумма чтений
    /// окна с прошлого опроса (app/meter.rs), своего чтения нет. Окна нет — блок читается здесь, как раньше.
    fn sample(&mut self) -> Result<Option<Sampled>, String> {
        let share = self.system_share();
        let Some(device) = self.device.as_mut() else {
            return Ok(None);
        };
        let package = device
            .thermal()
            .package()
            .map_err(|e| format!("Автомодуль: температура не прочитана — {e}"))?;
        let overlay = self.auto.active();
        let Some(controller) = self.auto.controller.as_ref() else {
            return Ok(None);
        };
        if !controller.watch_blocks(package, overlay) && share < LOADED {
            // Блоки не судятся: набранное окном — не для следующего решения (оно должно быть по свежему времени).
            self.meter.forget();
            return Ok(None);
        }
        let sensing = self.serving.attached();
        let mut readings = Vec::with_capacity(self.topology.blocks.len());
        for (b, block) in self.topology.blocks.iter().enumerate() {
            // Отставленный после чужой правки блок не судится: показаний для регулятора нет.
            if self.auto.aside[b] {
                readings.push(None);
                continue;
            }
            if !(sensing && self.meter.has(block.first)) {
                if let Ok(reading) = device.thermal().block(block.first) {
                    self.meter.record(block.first, reading, Instant::now());
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

    /// Обороты вентиляторов — не чаще раза в 10 с (каждый раз — вызов прошивки). На имитации прошивка не трогается.
    /// Зовут опрос автомодуля и опрос окна.
    pub(super) fn read_fans(&mut self) {
        let now = Instant::now();
        if self.auto.fans_next.is_some_and(|next| now < next) {
            return;
        }
        self.auto.fans_next = Some(now + FANS_EVERY);
        if self.auto.fans.is_none() {
            let opened: Result<Box<dyn Fans>, String> = match &self.device {
                Some(Device::Sim(_)) => Ok(Box::new(SimFans {
                    rpm: vec![3200, 3000],
                    fail: false,
                })),
                Some(Device::Real(_)) => HpFans::open()
                    .map(|fans| Box::new(fans) as Box<dyn Fans>)
                    .map_err(|e| e.0),
                None => return,
            };
            match opened {
                Ok(fans) => self.auto.fans = Some(fans),
                Err(text) => {
                    self.auto.rpm = None;
                    self.auto.fans_next = Some(now + FANS_RETRY);
                    self.fans_trouble(&text);
                    return;
                }
            }
        }
        let Some(fans) = self.auto.fans.as_mut() else {
            return;
        };
        let started = Instant::now();
        let read = fans.rpm();
        self.auto.fans_cost = Some(started.elapsed());
        match read {
            Ok(rpm) => {
                self.auto.rpm = Some(rpm);
                self.auto.fans_said = None;
            }
            Err(e) => {
                self.auto.rpm = None;
                self.fans_trouble(&e.0);
            }
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

    /// Срез в data\samples\<дата>.csv: время, корпус, вентиляторы, по блокам потолок, фактический множитель, доля
    /// работы, температура. Заголовок — при создании файла; новый день — новый файл и чистка старых.
    fn write_sample(&mut self, package: Option<f64>, readings: &[Option<Reading>]) {
        let Some(limits) = self.limits.as_ref() else {
            return;
        };
        let now = Stamp::now();
        let day = now.local(Pattern::Date);
        let dir = self.config.data.join("samples");
        let path = dir.join(format!("{day}.csv"));
        let mut text = String::with_capacity(600);
        if !path.exists() {
            text.push_str("time,package,fan_cpu,fan_gpu");
            for block in &self.topology.blocks {
                let n = &block.name;
                let _ = write!(text, ",{n}.ceiling,{n}.ratio,{n}.busy,{n}.temp"); // into a String: cannot fail
            }
            text.push('\n');
        }
        let rpm = self.auto.rpm.as_deref().unwrap_or_default();
        let fan = |k: usize| rpm.get(k).map(u32::to_string).unwrap_or_default();
        let _ = write!(
            text,
            "{},{},{},{}",
            now.local(Pattern::Log),
            opt(package, 0),
            fan(0),
            fan(1)
        ); // into a String
        for (block, reading) in self.topology.blocks.iter().zip(readings) {
            let r = reading.unwrap_or_default();
            let _ = write!(
                text,
                ",{},{},{},{}",
                limits.current()[block.first],
                opt(r.ratio, 2),
                opt(r.busy, 2),
                opt(r.temp, 0)
            ); // into a String: cannot fail
        }
        let line = text.strip_suffix('\n').unwrap_or(&text);
        if let Err(e) = files::append_line(&path, line) {
            self.files_trouble(&format!("срез не записан: {e}"));
        }
        if day != self.auto.samples_day {
            self.auto.samples_day = day;
            self.prune_samples(&dir, now);
        }
    }

    /// Ошибка записи журнала — одна строка журнала программы на одну и ту же ошибку.
    fn files_trouble(&mut self, text: &str) {
        if self.auto.files_said.as_deref() != Some(text) {
            self.journal.error(&format!("автомодуль: {text}"));
            self.auto.files_said = Some(text.to_owned());
        }
    }

    /// Срезы старше 30 дней удаляются (это журнал программы, не файлы владельца; срок — его выбор 15.10).
    fn prune_samples(&self, dir: &std::path::Path, now: Stamp) {
        let cutoff = Stamp(now.0 - KEEP_DAYS * 24 * 3600 * 10_000_000).local(Pattern::Date);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(date) = name.strip_suffix(".csv") else {
                continue;
            };
            if date.len() == 10
                && date < cutoff.as_str()
                && let Err(e) = std::fs::remove_file(entry.path())
            {
                self.journal
                    .error(&format!("автомодуль: старый срез {name} не удалён: {e}"));
            }
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

/// Число для журнала: `digits` знаков после запятой.
fn round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round() / scale
}

/// Число для среза: пусто, если не известно.
fn opt(value: Option<f64>, digits: usize) -> String {
    value.map(|v| format!("{v:.digits$}")).unwrap_or_default()
}
