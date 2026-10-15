// Автомодуль в программе входа (DECISIONS #31, «Auto module» и его подзапись): раз в 2 с — температура корпуса; близко
// к пределу или пока какой-то блок опущен — показания всех блоков и решение регулятора (machine::auto). Выключен —
// таймера нет, ничего не читается, драйвер закрыт между действиями.
//
// Опущенные блоки — «надстройка» над принятыми потолками (`intended`): принятые не меняются, надстройка помнит, какой
// блок на сколько опущен. Пока надстройка есть, программа держит аренду «CoreMap.Ceilings»: прежнее окно или движок,
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

use machine::auto::{Action, Controller, Settings, Timing};
use machine::files;
use machine::thermal::{Reading, Thermal};
use serde_json::json;

use super::{App, Device, Item, Other, Shell, at_boot, parts};

/// Таймер опроса.
pub const AUTO: usize = 4;
/// Пункты подменю: включено; предел (20 + номер в `LIMITS`); глубина (30 + номер в `DEPTHS`).
pub const AUTO_ON: u32 = 10;
pub const LIMIT_ID: u32 = 20;
pub const DEPTH_ID: u32 = 30;
pub const LIMITS: [i32; 5] = [75, 80, 85, 90, 95];
pub const DEPTHS: [i32; 3] = [2, 4, 6];
/// Как часто писать в журнал цену опроса — так она измеряется на машине владельца.
const COST_EVERY: Duration = Duration::from_secs(60);
/// Столько чтений подряд потолок блока должен расходиться с ожидаемым, чтобы считаться чужой правкой: одно чтение
/// бывает и запоздалым ответом на собственную запись, и чужим запросом в ящике (`ThrottleStop` не берёт наш замок).
const FOREIGN_READS: u8 = 2;

/// Сроки автомодуля: опрос и сроки регулятора. Скрытая проверка их укорачивает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoTiming {
    pub tick: Duration,
    pub control: Timing,
}

impl AutoTiming {
    pub const REAL: Self = Self {
        tick: Duration::from_secs(2),
        control: Timing::REAL,
    };
}

/// Выбор владельца, как он лежит в data\tray.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub on: bool,
    pub limit: i32,
    pub depth: i32,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            on: false,
            limit: 85,
            depth: 4,
        }
    }
}

/// Состояние автомодуля.
#[derive(Debug)]
pub struct Autopilot {
    pub choice: Choice,
    controller: Option<Controller>,
    /// По блокам: опущенный потолок; None — блок на принятом.
    lowered: Vec<Option<i32>>,
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
            lowered: vec![None; blocks],
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
        self.lowered.iter().any(Option::is_some)
    }
}

impl Device {
    fn thermal(&mut self) -> &mut dyn Thermal {
        match self {
            Self::Real(cpu) => cpu,
            Self::Sim(cpu) => cpu,
        }
    }
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
        };
    }

    fn save_choice(&self) {
        let c = self.auto.choice;
        let text = json!({ "auto": c.on, "limit": c.limit, "depth": c.depth }).to_string();
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
            },
            self.config.auto.control,
            self.topology.blocks.len(),
        )
    }

    /// Запуск программы: метка автомодуля от прошлого запуска — вернуть принятые опущенным блокам; включён — опрос.
    pub(super) fn start_auto(&mut self, shell: &Shell) {
        self.load_choice();
        if self.marker_path().exists() {
            let said = self.with_ceilings(App::recover_auto);
            if let Ok(Some(said)) = said {
                self.queue(&said);
            }
        }
        if self.auto.choice.on {
            self.auto.controller = Some(self.controller_for());
            shell.set_timer(AUTO, self.config.auto.tick);
            self.journal.note(&format!(
                "автомодуль включён: до {} °C, глубина {}",
                self.auto.choice.limit, self.auto.choice.depth
            ));
        }
    }

    /// Текст метки: процессор, раскладка, принятые потолки и опущенные блоки (первое ядро блока → потолок).
    fn marker_text(&self) -> String {
        let lps: Vec<u32> = self.store.lps().iter().map(|lp| lp.0).collect();
        let accepted = self.intended.clone().unwrap_or_default(); // a few dozen numbers per overlay change
        let lowered: Vec<i32> = self
            .topology
            .cores
            .iter()
            .enumerate()
            .map(|(i, _)| {
                self.topology
                    .blocks
                    .iter()
                    .position(|b| b.first == i)
                    .and_then(|b| self.auto.lowered[b])
                    .unwrap_or(0)
            })
            .collect();
        format!(
            "{{\"cpu\": {}, \"lps\": {}, \"accepted\": {}, \"lowered\": {}}}",
            files::quote(self.store.cpu()),
            files::ints(&lps),
            files::ints(&accepted),
            files::ints(&lowered)
        )
    }

    /// Переписать метку по надстройке; надстройки нет — убрать метку и отпустить аренду.
    fn mark(&mut self) -> Result<(), String> {
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

    /// Метка осталась — программа упала или её закрыли, пока блоки были опущены. Поднимаются только блоки, которые
    /// всё ещё стоят на опущенном значении (после перезагрузки процессор сбросил потолки, и метка ничего не значит),
    /// и не выше заводского предела.
    fn recover_auto(&mut self) -> Option<String> {
        let path = self.marker_path();
        let read = files::read_object(&path).ok().flatten().filter(|map| {
            map.get("cpu").and_then(serde_json::Value::as_str) == Some(self.store.cpu())
                && files::read_ints(map.get("lps")) == Some(self.store.lps().iter().map(|lp| i64::from(lp.0)).collect())
        });
        let accepted = read.as_ref().and_then(|map| files::read_ints(map.get("accepted")));
        let lowered = read.as_ref().and_then(|map| files::read_ints(map.get("lowered")));
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
                    if still_ours && want > low && limits.set(cpu, topology, i, want).is_none() {
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
        let mut children = vec![item(AUTO_ON, "Включено".into(), c.on), super::separator()];
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
            (AUTO_ON, ..) => {
                self.auto.choice.on = !self.auto.choice.on;
                if self.auto.choice.on {
                    shell.set_timer(AUTO, self.config.auto.tick);
                } else {
                    shell.kill_timer(AUTO);
                    self.put_back("автомодуль выключен");
                    self.auto.controller = None;
                    if let Some(device) = &mut self.device {
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
            "автомодуль: {}, до {} °C, глубина {}",
            if c.on { "включён" } else { "выключен" },
            c.limit,
            c.depth
        ));
        true
    }

    /// Смена набора: принятые потолки — новые, блоки, отставленные из-за чужой правки, снова под присмотром.
    pub(super) fn auto_new_set(&mut self) {
        self.auto.aside.fill(false);
        self.auto.mismatch.fill(0);
        if let Some(controller) = &mut self.auto.controller {
            controller.forget();
        }
    }

    /// Строка подсказки: что автомодуль сейчас делает.
    pub(super) fn auto_tip(&self) -> Option<String> {
        if !self.auto.choice.on && !self.auto.active() {
            return None;
        }
        let mut text = format!("авто: до {} °C", self.auto.choice.limit);
        let intended = self.intended.as_deref().unwrap_or_default();
        for (block, lowered) in self.topology.blocks.iter().zip(&self.auto.lowered) {
            if let (Some(to), Some(&base)) = (lowered, intended.get(block.first)) {
                let _ = write!(text, " · {} −{}", block.name, base - to); // into a String: cannot fail
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
            self.journal.note(&format!(
                "автомодуль: опрос {} мкс{}",
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

    /// Один опрос под арендой: показания, сверка потолков, решение регулятора, запись.
    fn poll(&mut self, watched_blocks: &mut bool) -> Polled {
        let readings = match self.sample() {
            Ok(Some(readings)) => readings,
            Ok(None) => return Polled::Quiet,
            Err(said) => return Polled::Said(said),
        };
        *watched_blocks = true;
        let foreign = match self.reconcile() {
            Ok(Some(foreign)) => foreign,
            Ok(None) => return Polled::Reset,
            Err(said) => return Polled::Said(said),
        };
        let mut said = (!foreign.is_empty()).then(|| {
            format!(
                "Потолки блоков {} изменила другая программа — автомодуль их больше не трогает",
                foreign.join(", ")
            )
        });
        if let Some(refused) = self.act(&readings) {
            said = Some(refused);
        }
        // Чужая правка могла снять последний опущенный блок: метка и аренда — по надстройке (ревизия M2).
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        said.map_or(Polled::Quiet, Polled::Said)
    }

    /// Показания блоков, если они нужны: корпус близко к пределу или какой-то блок опущен. Ok(None) — не нужны.
    fn sample(&mut self) -> Result<Option<Vec<Option<Reading>>>, String> {
        let Some(device) = self.device.as_mut() else {
            return Ok(None);
        };
        let package = device
            .thermal()
            .package()
            .map_err(|e| format!("Автомодуль: температура не прочитана — {e}"))?;
        let lowered_any = self.auto.active();
        let Some(controller) = self.auto.controller.as_ref() else {
            return Ok(None);
        };
        if !controller.watch_blocks(package, lowered_any) {
            return Ok(None);
        }
        let mut readings = Vec::with_capacity(self.topology.blocks.len());
        for (b, block) in self.topology.blocks.iter().enumerate() {
            // Отставленный после чужой правки блок не судится: показаний для регулятора нет.
            let reading = (!self.auto.aside[b])
                .then(|| device.thermal().block(block.first).ok())
                .flatten();
            readings.push(reading);
        }
        Ok(Some(readings))
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
        for (b, block) in topology.blocks.iter().enumerate() {
            let i = block.first;
            let expected = self.auto.lowered[b].unwrap_or(intended[i]);
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
            for &m in &block.members {
                intended[m] = limits.current()[m];
            }
            self.auto.lowered[b] = None;
            self.auto.mismatch[b] = 0;
            if !self.auto.aside[b] {
                self.auto.aside[b] = true;
                foreign.push(block.name.clone());
            }
            if let Some(controller) = &mut self.auto.controller {
                controller.forget_block(b);
            }
        }
        Ok(Some(foreign))
    }

    /// Решение регулятора и запись. Возвращает отказ, если он был.
    fn act(&mut self, readings: &[Option<Reading>]) -> Option<String> {
        let topology = &self.topology;
        let limits = self.limits.as_ref()?;
        let intended = self.intended.as_deref()?;
        let ceilings: Vec<i32> = topology.blocks.iter().map(|b| limits.current()[b.first]).collect();
        // Возвращать регулятор может только то, что опустил сам: блок ниже принятого, но не опущенный автомодулем,
        // опустил кто-то другой — для регулятора его «принятый» — то, что стоит. Иначе прохладный блок поднимался бы
        // поверх чужой правки (найдено скрытой проверкой 14.10).
        let accepted: Vec<i32> = topology
            .blocks
            .iter()
            .enumerate()
            .map(|(b, block)| {
                let base = intended[block.first];
                if self.auto.lowered[b].is_some() {
                    base
                } else {
                    ceilings[b].min(base)
                }
            })
            .collect();
        // Блок, у которого потолок расходится с ожидаемым и ещё не подтверждён как чужая правка, не судится.
        let readings: Vec<Option<Reading>> = readings
            .iter()
            .zip(&self.auto.mismatch)
            .map(|(reading, &mismatch)| if mismatch > 0 { None } else { *reading })
            .collect();
        let now = self.auto.origin.elapsed();
        let action = self
            .auto
            .controller
            .as_mut()?
            .step(now, &readings, &ceilings, &accepted)?;
        let (b, to) = match action {
            Action::Lower { block, to } | Action::Raise { block, to } => (block, to),
        };
        let name = topology.blocks[b].name.clone();
        let first = topology.blocks[b].first;
        let mean = readings[b]
            .and_then(|r| r.temp)
            .map_or_else(|| "?".into(), |t| format!("{t:.0}"));
        let before = ceilings[b];
        let was = self.auto.lowered[b];
        // Метка — до записи в процессор: с новым опущенным блоком.
        self.auto.lowered[b] = (to < accepted[b]).then_some(to);
        if let Err(e) = self.mark() {
            self.auto.lowered[b] = was;
            return Some(format!("Автомодуль: метка не записана, потолок не тронут — {e}"));
        }
        let topology = &self.topology;
        let (limits, cpu) = parts(&mut self.limits, &mut self.device)?;
        match limits.set(cpu, topology, first, to) {
            None => {
                self.journal.note(&format!(
                    "автомодуль: блок {name} {} ×{before} → ×{to} ({mean} °C)",
                    if to < before {
                        "опущен"
                    } else {
                        "возвращён"
                    }
                ));
                None
            }
            Some(refusal) => {
                self.auto.lowered[b] = was;
                Some(format!("Автомодуль: потолок блока {name} не изменён — {refusal}"))
            }
        }
    }

    /// Вернуть принятые потолки опущенным блокам (C#-часть стартует, сон, выход, смена набора, новые настройки).
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
                if self.auto.lowered[b].is_none() {
                    continue;
                }
                let Some(&want) = intended.get(block.first) else {
                    continue;
                };
                match limits.set(cpu, topology, block.first, want) {
                    None => {
                        self.auto.lowered[b] = None;
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
