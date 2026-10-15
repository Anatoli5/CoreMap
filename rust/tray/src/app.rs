// Программа входа: что она делает с потолками и когда (DECISIONS #31). Вызовы Windows — в shell.rs, железо и
// правила защиты — в библиотеке machine; здесь — решения.
//
// При входе в Windows: возврат потолков по метке оборванного замера, затем профиль запуска по правилам защиты
// (machine::profile::startup). Дальше программа спит до действия владельца: меню значка, щелчок по набору,
// пробуждение после сна. Драйвер открыт только на время действия.
//
// Одна программа пишет потолки в каждый момент. Пока работает прежнее окно или движок настройки (части на C#), эта
// программа не трогает процессор вовсе — ни записи, ни чтения: они держат свои метки в Windows всё время работы
// (присутствие). Каждое действие с потолками идёт под общей арендой «CoreMap.Ceilings» — её же берёт C#-часть при
// старте, прежде чем прочитать потолки. Когда C#-часть закрылась, эта программа через пару секунд возвращает себе
// право: восстанавливает потолки по метке, если замер оборвали, и принимает то, что стоит (профиль не ставит).
//
// После сна потолки сверяются с тем, что должно стоять (`intended`): блок, сброшенный к заводским, получает своё
// значение заново; блок с другим значением изменила чужая программа — его новое значение принимается, с одним
// сообщением, без борьбы.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use machine::clock::Stamp;
use machine::events::{EventLog, SystemLog};
use machine::limits::Limits;
use machine::mailbox::{Cpu, MailboxCpu};
use machine::named::{self, Lease, Names};
use machine::pawnio::PawnIo;
use machine::profile::{self, Profile, Set, Store, same_name};
use machine::sim::SimCpu;
use machine::topology::Topology;
use machine::{HwError, marker};

use crate::journal::Journal;
use crate::shell::{self, Child, Event, Item, Loop, Menu, Shell};

pub const OPEN_ID: u32 = 1;
pub const EXIT_ID: u32 = 2;
pub const NEW_ID: u32 = 3;
pub const FACTORY_ID: u32 = 100; // 101 и дальше — наборы в порядке последнего меню

const RETRY: usize = 1;
const TAKE_BACK: usize = 2;
const WAKE: usize = 3;
const RETRIES: u8 = 3;
const RETRY_EVERY: Duration = Duration::from_secs(10);
const HOVER_GAP: Duration = Duration::from_secs(1);
/// Класс окна движка на C# (Tray.WindowClass): ему шлётся просьба открыть новое окно.
const ENGINE_CLASS: &str = "CoreMap.Tray";

/// Настройки запуска: где лежит программа и данные, имена меток, задержки.
#[derive(Debug, Clone)]
pub struct Config {
    /// Папка программы: CoreMap.exe, modules\IntelMSR.bin.
    pub root: PathBuf,
    pub data: PathBuf,
    pub names: Names,
    /// Процессор — имитация (скрытая проверка).
    pub simulate: bool,
    /// Запускать ли части на C#; в скрытой проверке запуск только записывается.
    pub launch: bool,
    /// Пауза после конца C#-части, прежде чем вернуть себе право: движок, запущенный кнопкой «новое окно», создаёт
    /// свою метку чуть позже, чем закрывается прежнее окно (outputs/ceiling-ownership-2026-10-14.md §2).
    pub grace: Duration,
    /// Пауза после пробуждения перед сверкой: драйвер и другие программы (`ThrottleStop`, Omen Gaming Hub) успевают
    /// сделать своё.
    pub wake_delay: Duration,
    /// Сколько ждать аренду, мс.
    pub lease_wait: u32,
    /// Сверка после пробуждения — не чаще раза за этот промежуток: одно пробуждение шлёт несколько событий.
    pub wake_gap: Duration,
}

/// Кто сейчас держит потолки вместо этой программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Other {
    OldWindow,
    /// У движка жив Core: к нему подключено окно или идёт измерение.
    Tuning,
    /// Процесс движка (или прежний резидент на C#) без живого Core.
    Engine,
    /// Эта программа только что запустила часть на C#, её метки ещё нет.
    Starting,
}

impl Other {
    fn menu_text(self) -> &'static str {
        match self {
            Self::OldWindow => "окно CoreMap открыто — профили переключаются в нём",
            Self::Tuning => "идёт настройка в окне CoreMap — профили ждут её конца",
            Self::Engine => "работает другая часть CoreMap — профили переключаются там",
            Self::Starting => "запускается окно CoreMap",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Self::OldWindow => "CoreMap — открыто окно программы",
            Self::Tuning => "CoreMap — идёт настройка",
            Self::Engine => "CoreMap — работает другая часть CoreMap",
            Self::Starting => "CoreMap — запускается окно программы",
        }
    }
}

/// Потолки в настоящем процессоре или в имитации.
#[derive(Debug)]
enum Device {
    Real(MailboxCpu),
    Sim(SimCpu),
}

impl Device {
    fn cpu(&mut self) -> &mut dyn Cpu {
        match self {
            Self::Real(cpu) => cpu,
            Self::Sim(cpu) => cpu,
        }
    }
}

/// То, чего программа ждёт вместе с сообщениями окна: метка C#-части или процесс, запущенный ею самой.
#[derive(Debug)]
enum Watched {
    Mutex(named::Mutex, Other),
    Child(Child, Other),
}

impl Watched {
    fn other(&self) -> Other {
        match self {
            Self::Mutex(_, other) | Self::Child(_, other) => *other,
        }
    }
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts (started, slept while away, take-back pending, balloon shown), not one state"
)]
pub struct App {
    config: Config,
    topology: Topology,
    store: Store,
    journal: Journal,
    events: Box<dyn EventLog>,
    device: Option<Device>,
    limits: Option<Limits>,
    unavailable: Option<String>,
    profile: Option<Profile>,
    sets: Vec<Set>,
    note: Option<String>,
    refusal: Option<String>,
    started: bool,
    intended: Option<Vec<i32>>,
    /// Потолки, которые процессор держит после загрузки, — прочитаны при запуске до профиля. Сон, который сбрасывает
    /// потолки, возвращает их к этим значениям; это не всегда заводской предел (у 275HX ядра 12 и 23 — ×53 при
    /// пределе ×54; найдено тестом по спецификации, KNOWLEDGE.md).
    boot: Vec<i32>,
    power_while_away: bool,
    /// C#-часть только что закрылась, возврат права ещё впереди (пауза `grace`).
    take_back_pending: bool,
    listed: Vec<String>,
    lease: Rc<named::Mutex>,
    watched: Vec<Watched>,
    balloons: VecDeque<String>,
    balloon_up: bool,
    retries: u8,
    last_wake: Option<Instant>,
    last_hover: Option<Instant>,
    /// Скрытая проверка: что было бы запущено и что было бы сказано всплывающим сообщением.
    pub launched: Vec<String>,
    pub said: Vec<String>,
}

impl App {
    pub fn new(config: Config, events: Box<dyn EventLog>) -> Result<Self, String> {
        let topology = Topology::read().map_err(|e| e.0)?;
        let cpu = machine::topology::cpu_name();
        let store = Store::new(&config.data, &cpu, &topology.lps());
        let journal = Journal::new(&config.data);
        let (lease, _) = named::create(&config.names.lease, false).map_err(|e| format!("аренда потолков: {e}"))?;
        Ok(Self {
            config,
            topology,
            store,
            journal,
            events,
            device: None,
            limits: None,
            unavailable: None,
            profile: None,
            sets: Vec::new(),
            note: None,
            refusal: None,
            started: false,
            intended: None,
            boot: Vec::new(),
            power_while_away: false,
            take_back_pending: false,
            listed: Vec::new(),
            lease: Rc::new(lease),
            watched: Vec::new(),
            balloons: VecDeque::new(),
            balloon_up: false,
            retries: 0,
            last_wake: None,
            last_hover: None,
            launched: Vec::new(),
            said: Vec::new(),
        })
    }

    /// Обычный запуск: системный журнал Windows.
    pub fn with_system_log(config: Config) -> Result<Self, String> {
        Self::new(config, Box::new(SystemLog))
    }

    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    #[must_use]
    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Имитация процессора (скрытая проверка): None, если процессор настоящий или ещё не открыт.
    pub fn sim(&mut self) -> Option<&mut SimCpu> {
        match &mut self.device {
            Some(Device::Sim(sim)) => Some(sim),
            _ => None,
        }
    }

    #[must_use]
    pub fn intended(&self) -> Option<&[i32]> {
        self.intended.as_deref()
    }

    // ----- кто держит потолки -----

    /// Кто сейчас держит потолки вместо этой программы; заодно начинает ждать его конца.
    pub fn other(&mut self) -> Option<Other> {
        let names = &self.config.names;
        let found = if named::exists(&names.old_window) {
            Some((Other::OldWindow, names.old_window.clone()))
        } else if named::exists(&names.engine_core) {
            Some((Other::Tuning, names.engine.clone()))
        } else if named::exists(&names.engine) {
            Some((Other::Engine, names.engine.clone()))
        } else {
            None
        };
        if let Some((other, mutex)) = found {
            // Ждать конца того, кто держит потолки: его метку (её держит его главный поток всю жизнь процесса).
            let watching = self
                .watched
                .iter()
                .any(|w| matches!(w, Watched::Mutex(_, o) if *o == other));
            if !watching && let Some(handle) = named::open(&mutex) {
                self.watched.push(Watched::Mutex(handle, other));
            }
            return Some(other);
        }
        self.watched
            .iter()
            .find(|w| matches!(w, Watched::Child(..)))
            .map(Watched::other)
    }

    /// Дескрипторы, которых ждёт цикл программы.
    #[must_use]
    pub fn waits(&self) -> Vec<windows_sys::Win32::Foundation::HANDLE> {
        self.watched
            .iter()
            .map(|w| match w {
                Watched::Mutex(m, _) => m.raw(),
                Watched::Child(c, _) => c.raw(),
            })
            .collect()
    }

    /// Действие с потолками: под арендой, только если никто другой их не держит; драйвер открыт на время действия.
    fn with_ceilings<R>(&mut self, act: impl FnOnce(&mut Self) -> R) -> Result<R, String> {
        // Присутствие — сначала: пока стартует C#-часть, не ждать её аренду 10 с с замершим значком
        // (outputs/review-ownership-2026-10-14.md L5); после аренды — ещё раз, это и закрывает гонку.
        if let Some(other) = self.other() {
            return Err(other.menu_text().into());
        }
        let lease = Rc::clone(&self.lease);
        let Some(_held) = Lease::take(&lease, self.config.lease_wait) else {
            return Err("потолки сейчас меняет другая часть CoreMap — попробуй ещё раз через несколько секунд".into());
        };
        if let Some(other) = self.other() {
            return Err(other.menu_text().into());
        }
        if self.limits.is_none()
            && let Err(e) = self.connect()
        {
            // Драйвер мог открыться до отказа: не держать его до следующего действия (ревизия, п. 8).
            if let Some(device) = &mut self.device {
                device.cpu().release();
            }
            self.unavailable = Some(e.0.clone());
            return Err(format!("потолки множителей недоступны: {}", e.0));
        }
        let result = act(self);
        if let Some(device) = &mut self.device {
            device.cpu().release();
        }
        Ok(result)
    }

    fn connect(&mut self) -> Result<(), HwError> {
        if self.device.is_none() {
            self.device = Some(if self.config.simulate {
                Device::Sim(SimCpu::new(&self.topology))
            } else {
                let io = PawnIo::new(&self.config.root.join("modules").join("IntelMSR.bin"))?;
                Device::Real(MailboxCpu::new(
                    Box::new(io),
                    &self.config.names.mailbox,
                    &self.topology,
                )?)
            });
        }
        let device = self
            .device
            .as_mut()
            .ok_or_else(|| HwError("нет доступа к процессору".into()))?;
        let (limits, unread) = Limits::read(device.cpu(), &self.topology)?;
        if let Some(unread) = unread {
            // Не только в журнал: профиль этим ядрам не ставится, а подсказка иначе показала бы его стоящим целиком.
            self.journal.note(&format!("потолки при запуске: {unread}"));
            self.refusal = Some(format!("{unread} — им потолки не ставятся"));
        }
        self.limits = Some(limits);
        self.unavailable = None;
        Ok(())
    }

    // ----- запуск -----

    /// Вход в Windows: метка оборванного замера, затем профиль запуска.
    pub fn start(&mut self, shell: &mut Shell) {
        match self.with_ceilings(Self::startup) {
            Ok(notice) => {
                self.started = true;
                if let Some(notice) = notice {
                    self.notify(shell, &notice);
                }
            }
            Err(reason) => {
                if self.other().is_some() {
                    // Профиль ставит та часть, что уже работает; эта программа включится, когда та закроется.
                    self.started = true;
                    self.read_files();
                    self.journal.note(&format!("при запуске профиль не ставлю: {reason}"));
                } else {
                    self.journal.note(&format!("запуск: {reason}"));
                    self.schedule_retries(shell);
                }
            }
        }
        if !shell.add_icon(&self.tip()) {
            self.schedule_retries(shell);
        }
        self.show_balloon(shell);
    }

    fn startup(&mut self) -> Option<String> {
        let recovery = self.recover();
        // То, что стоит до профиля, — потолки после загрузки (программа входа запускается при входе в Windows).
        self.boot = self.limits.as_ref().map(|l| l.current().to_vec()).unwrap_or_default();
        self.read_sets();
        let (topology, store, events) = (&self.topology, &self.store, self.events.as_ref());
        let limits = self.limits.as_mut()?;
        let cpu = self.device.as_mut()?.cpu();
        let outcome = profile::startup(store, limits, cpu, topology, events, &Stamp::now);
        match outcome {
            Ok(startup) => {
                if let Some(at) = &startup.at_startup {
                    let name = startup.profile.as_ref().map_or("", |p| p.name.as_str());
                    self.journal
                        .note(&format!("при входе поставлен профиль «{name}» в {at}"));
                }
                self.profile = startup.profile;
                self.note = startup.note;
            }
            Err(spoiled) => {
                self.journal.error(&format!("профиль потолков не прочитан: {spoiled}"));
                self.profile = None;
                self.note = Some(format!("профиль не прочитан: {spoiled}"));
            }
        }
        if let Some(note) = &self.note {
            self.journal.note(note);
        }
        self.intended = self.limits.as_ref().map(|l| l.current().to_vec());
        let in_effect = self
            .profile
            .as_ref()
            .is_some_and(|p| self.limits.as_ref().is_some_and(|l| l.same(&p.ratios)));
        let attention = if in_effect && self.refusal.is_none() {
            None
        } else {
            self.attention()
        };
        match (recovery, attention) {
            (Some(said), Some(more)) => Some(format!("{said} И ещё: {more}.")),
            (said, None) => said,
            (None, more) => more,
        }
    }

    /// Метка оборванного замера: потолки по ней и текст для владельца.
    fn recover(&mut self) -> Option<String> {
        let path = self.store.marker_path();
        let (store, topology) = (&self.store, &self.topology);
        let limits = self.limits.as_mut()?;
        let cpu = self.device.as_mut()?.cpu();
        let recovery = marker::recover(&path, store.cpu(), store.lps(), limits, cpu, topology);
        if let Some(logged) = &recovery.logged {
            self.journal.error(logged);
        }
        if let Some(said) = &recovery.said {
            self.journal.note(said);
        }
        recovery.said
    }

    /// Наборы и профиль запуска читаются порознь: испорченный список наборов не мешает профилю (B1 описи).
    fn read_sets(&mut self) {
        match self.store.read_sets() {
            Ok(sets) => self.sets = sets,
            Err(spoiled) => {
                self.journal.error(&format!("список профилей не прочитан: {spoiled}"));
                self.refusal = Some(format!("список профилей не прочитан: {spoiled}"));
            }
        }
    }

    fn read_files(&mut self) {
        self.read_sets();
        match self.store.read_profile() {
            Ok(profile) => self.profile = profile,
            Err(spoiled) => {
                self.profile = None;
                self.note = Some(format!("профиль не прочитан: {spoiled}"));
            }
        }
    }

    fn schedule_retries(&mut self, shell: &Shell) {
        if self.retries == 0 {
            self.retries = RETRIES;
            shell.set_timer(RETRY, RETRY_EVERY);
        }
    }

    /// Повтор запуска: значок не добавился (Проводник ещё не готов) или драйвер не открылся.
    fn retry(&mut self, shell: &mut Shell) {
        if !shell.icon_shown() {
            shell.add_icon(&self.tip());
        }
        if !self.started && self.other().is_none() {
            self.limits = None;
            if let Ok(notice) = self.with_ceilings(Self::startup) {
                self.started = true;
                if let Some(notice) = notice {
                    self.notify(shell, &notice);
                }
            }
        }
        self.retries = self.retries.saturating_sub(1);
        if (shell.icon_shown() && self.started) || self.retries == 0 {
            shell.kill_timer(RETRY);
            self.retries = 0;
            if !self.started
                && let Some(attention) = self.attention()
            {
                self.notify(shell, &attention);
            }
        }
        shell.set_tip(&self.tip());
        self.show_balloon(shell);
    }

    // ----- что видит владелец -----

    #[must_use]
    pub fn available(&self) -> bool {
        self.limits.is_some()
    }

    fn startup_name(&self) -> String {
        self.profile.as_ref().map(|p| p.name.clone()).unwrap_or_default()
    }

    /// Наборы для меню: сохранённые и профиль запуска, если его нет в списке (C# вписывал его в profiles.json;
    /// эта программа файл наборов не пишет).
    fn listed_sets(&self) -> Vec<Set> {
        let mut sets = self.sets.clone(); // a dozen short sets, built only when the menu or tip is shown
        if let Some(p) = &self.profile
            && !sets.iter().any(|s| same_name(&s.name, &p.name))
        {
            sets.push(Set {
                name: p.name.clone(),
                ratios: p.ratios.clone(),
            });
        }
        sets
    }

    /// Что не так, одной строкой; None — всё штатно (Resident.Attention в C#).
    #[must_use]
    pub fn attention(&self) -> Option<String> {
        if !self.available() {
            return Some(format!(
                "потолки множителей недоступны: {}",
                self.unavailable.as_deref().unwrap_or("профили не прочитаны")
            ));
        }
        if let Some(refusal) = &self.refusal {
            return Some(refusal.clone());
        }
        if let Some(note) = &self.note {
            return Some(note.clone());
        }
        let p = self.profile.as_ref()?;
        p.disabled()
            .then(|| format!("профиль «{}» отключён после сбоя и при запуске не ставится", p.name))
    }

    /// Какой набор стоит в процессоре: Some(название), Some("") — заводские, None — ни один.
    fn standing(&self) -> Option<String> {
        let limits = self.limits.as_ref()?;
        profile::standing(limits, self.profile.as_ref(), &self.listed_sets())
    }

    /// Подсказка значка (Resident.Tip в C#).
    #[must_use]
    pub fn tip(&mut self) -> String {
        if let Some(other) = self.other() {
            return other.tip().into();
        }
        let attention = self.attention();
        if !self.available() {
            return format!("CoreMap — {}", attention.unwrap_or_default());
        }
        let standing = self.standing();
        let text = format!(
            "CoreMap — {}",
            match standing.as_deref() {
                None => "потолки не из профиля".to_string(),
                Some("") => "заводские потолки".to_string(),
                Some(name) => format!("«{name}»"),
            }
        );
        if let Some(attention) = attention {
            return format!("{text}\n{attention}");
        }
        let at_startup = self.startup_name();
        if standing.as_deref().is_some_and(|s| same_name(s, &at_startup)) {
            return text;
        }
        let startup = if at_startup.is_empty() {
            "заводские".to_string()
        } else {
            format!("«{at_startup}»")
        };
        format!("{text}\nпри запуске — {startup}")
    }

    /// Меню значка (Resident.Menu в C#): пояснение, наборы с галочкой на стоящем, «Новое окно», «Открыть прежнее
    /// окно», «Выход».
    pub fn menu(&mut self) -> Vec<Item> {
        let other = self.other();
        let mut items = Vec::new();
        let attention = other.map(|o| o.menu_text().to_string()).or_else(|| self.attention());
        if let Some(attention) = attention {
            // Строка меню короткая; целиком причина — в подсказке и в журнале.
            let text = if attention.chars().count() <= 90 {
                attention
            } else {
                format!("{}…", attention.chars().take(89).collect::<String>())
            };
            items.push(Item {
                id: 0,
                text: Some(text),
                checked: false,
                enabled: false,
                default: false,
            });
            items.push(separator());
        }
        if self.available() {
            let standing = if other.is_some() { None } else { self.standing() };
            let sets = self.listed_sets();
            self.listed = sets.iter().map(|s| s.name.clone()).collect();
            let at_startup = self.startup_name();
            let disabled = self.profile.as_ref().is_some_and(Profile::disabled);
            let names = std::iter::once(String::new()).chain(self.listed.iter().cloned());
            for (k, name) in names.enumerate() {
                let marked = standing.as_deref().is_some_and(|s| same_name(s, &name));
                let is_startup = same_name(&name, &at_startup);
                let mut text = if name.is_empty() {
                    "заводские".to_string()
                } else {
                    name.clone()
                };
                if is_startup && disabled {
                    text.push_str("  · отключён после сбоя");
                } else if is_startup && !marked {
                    text.push_str("  · ставится при запуске");
                }
                items.push(Item {
                    id: FACTORY_ID + u32::try_from(k).unwrap_or(0),
                    text: Some(text),
                    checked: marked,
                    enabled: other.is_none(),
                    default: false,
                });
                if k == 0 && !self.listed.is_empty() {
                    items.push(separator());
                }
            }
            items.push(separator());
        }
        items.push(Item {
            id: NEW_ID,
            text: Some("Новое окно".into()),
            checked: false,
            enabled: true,
            default: false,
        });
        items.push(Item {
            id: OPEN_ID,
            text: Some("Открыть прежнее окно".into()),
            checked: false,
            enabled: true,
            default: true,
        });
        items.push(Item {
            id: EXIT_ID,
            text: Some("Выход".into()),
            checked: false,
            enabled: true,
            default: false,
        });
        items
    }

    // ----- действия владельца -----

    /// Открытие меню: перечитать потолки и файлы (их могла изменить другая часть), как Load(false) в C#.
    pub fn refresh_view(&mut self) {
        let done = self.with_ceilings(|app| {
            let topology = &app.topology;
            if let Some((limits, cpu)) = parts(&mut app.limits, &mut app.device) {
                app.refusal = limits
                    .refresh(cpu, topology)
                    .map(|u| format!("потолки не перечитаны из процессора: {u}"));
            }
            let said = app.recover();
            app.read_files();
            if app.intended.is_none() {
                app.intended = app.limits.as_ref().map(|l| l.current().to_vec());
            }
            said
        });
        match done {
            Ok(Some(said)) => self.queue(&said),
            Ok(None) => {}
            // Держит другая часть: процессор не трогаем, но файлы читать можно — меню покажет названия.
            Err(_) => self.read_files(),
        }
    }

    /// Пункт меню.
    pub fn command(&mut self, shell: &mut Shell, id: u32) {
        match id {
            OPEN_ID => self.open_old_window(shell),
            NEW_ID => self.open_new_window(shell),
            EXIT_ID => {
                shell.remove_icon();
                shell.destroy();
            }
            0 => {}
            _ => {
                let k = usize::try_from(id - FACTORY_ID).unwrap_or(usize::MAX);
                let name = if k == 0 {
                    Some(String::new())
                } else {
                    self.listed.get(k - 1).cloned()
                };
                if let Some(name) = name {
                    let said = self.switch(&name);
                    if let Some(said) = said {
                        self.notify(shell, &said);
                    }
                }
            }
        }
        shell.set_tip(&self.tip());
        self.show_balloon(shell);
    }

    /// Щелчок по набору: ставит его и делает профилем запуска; «заводские» — профиля запуска больше нет. Как у
    /// резидента C#, это «открыть и принять» сразу. None — сделано; иначе — что сказать владельцу.
    pub fn switch(&mut self, name: &str) -> Option<String> {
        let title = if name.is_empty() {
            "Заводские потолки".to_string()
        } else {
            format!("Профиль «{name}»")
        };
        let ratios = if name.is_empty() {
            self.limits.as_ref().map(|l| l.fused().to_vec())
        } else {
            self.listed_sets()
                .into_iter()
                .find(|s| same_name(&s.name, name))
                .map(|s| s.ratios)
        };
        let done = self.with_ceilings(|app| -> Result<(), String> {
            let Some(ratios) = ratios else {
                return Err(format!("профиля «{name}» нет"));
            };
            let topology = &app.topology;
            let (limits, cpu) = parts(&mut app.limits, &mut app.device).ok_or("потолки недоступны")?;
            if let Some(refused) = limits.apply(cpu, topology, &ratios) {
                return Err(format!(
                    "{title} не поставлен{}: {refused}",
                    if name.is_empty() { "ы" } else { "" }
                ));
            }
            let limits = app.limits.as_ref().ok_or("потолки недоступны")?;
            if name.is_empty() {
                app.store
                    .forget_profile()
                    .map_err(|e| format!("файл профиля не удалён: {e}"))?;
                app.profile = None;
            } else if !app
                .profile
                .as_ref()
                .is_some_and(|p| same_name(&p.name, name) && !p.disabled())
            {
                // Профиль запуска, который и так стоит, заново не записывается: это обнулило бы его испытательный срок.
                let saved = profile::save(&app.store, limits, name.trim(), Stamp::now())
                    .map_err(|e| format!("{title}: файл профиля не записан ({e})"))?;
                app.profile = Some(saved);
            }
            app.note = None;
            app.intended = Some(limits.current().to_vec());
            app.journal.note(&format!(
                "из меню поставлен {}",
                if name.is_empty() {
                    "заводские потолки"
                } else {
                    name
                }
            ));
            Ok(())
        });
        // Что стоит после попытки — то и должно стоять, удалась она целиком или нет: иначе сверка после сна
        // переписала бы блоки по старому набору или назвала бы их чужой правкой (ревизия L3).
        if let Some(limits) = &self.limits {
            self.intended = Some(limits.current().to_vec());
        }
        match done {
            Ok(Ok(())) => {
                self.refusal = None;
                None
            }
            Ok(Err(refusal)) | Err(refusal) => {
                self.refusal = Some(refusal.clone());
                Some(refusal)
            }
        }
    }

    fn csharp(&self) -> PathBuf {
        self.config.root.join("CoreMap.exe")
    }

    /// Запуск части на C# с правами этой программы (запроса Windows нет).
    fn start_csharp(&mut self, shell: &mut Shell, args: &str, what: Other) {
        let args = if self.config.simulate {
            format!("--simulate {args}").trim().to_string()
        } else {
            args.to_string()
        };
        if !self.config.launch {
            self.launched.push(format!("CoreMap.exe {args}").trim().to_string());
            return;
        }
        match shell::launch(&self.csharp(), &args, &self.config.root) {
            // Пока у запущенной части нет своей метки, присутствие — её процесс.
            Ok(child) => self.watched.push(Watched::Child(child, what)),
            Err(e) => {
                self.journal.error(&e);
                self.notify(shell, &format!("Окно CoreMap не открылось: {e}"));
            }
        }
    }

    fn open_old_window(&mut self, shell: &mut Shell) {
        match self.other() {
            Some(Other::Tuning | Other::Engine) => {
                self.notify(
                    shell,
                    "Прежнее окно сейчас не открыть: к программе подключено новое окно или идёт измерение",
                );
            }
            // Уже открыто — прежнее окно само выведет свою копию вперёд (Program.cs).
            _ => self.start_csharp(shell, "", Other::Starting),
        }
    }

    fn open_new_window(&mut self, shell: &mut Shell) {
        match self.other() {
            Some(Other::Starting) => {
                self.notify(shell, "Окно CoreMap ещё запускается — подожди несколько секунд");
            }
            Some(Other::OldWindow) => {
                self.notify(
                    shell,
                    "Сначала закрой прежнее окно CoreMap — или нажми в нём «новое окно»",
                );
            }
            Some(Other::Tuning | Other::Engine) => {
                if self.config.launch && !shell::post_to_class(ENGINE_CLASS, shell::OPEN_NEW, 0) {
                    self.notify(shell, "Новое окно не открыть: движок CoreMap не отвечает");
                }
            }
            _ => self.start_csharp(shell, "--tray --bridge --open", Other::Tuning),
        }
    }

    // ----- события -----

    /// Разбор события цикла программы.
    pub fn on(&mut self, shell: &mut Shell, what: Loop) {
        match what {
            Loop::Ended(index) => self.ended(shell, index),
            Loop::Event(event) => self.event(shell, event),
        }
    }

    fn event(&mut self, shell: &mut Shell, event: Event) {
        match event {
            Event::Menu(x, y) => {
                self.refresh_view();
                let items = self.menu();
                match Menu::build(&items) {
                    Ok(menu) => {
                        let chosen = shell.track(&menu, x, y);
                        self.command(shell, chosen);
                    }
                    Err(e) => self.journal.error(&e),
                }
            }
            Event::Open => self.command(shell, OPEN_ID),
            Event::Hover => {
                if self.last_hover.is_none_or(|t| t.elapsed() >= HOVER_GAP) {
                    self.last_hover = Some(Instant::now());
                    shell.set_tip(&self.tip());
                }
            }
            Event::BalloonDone => {
                self.balloon_up = false;
                self.show_balloon(shell);
            }
            Event::Announced(pid) => {
                self.journal.note(&format!("прежнее окно запущено (процесс {pid})"));
                self.other();
                shell.set_tip(&self.tip());
            }
            Event::Timer(RETRY) => self.retry(shell),
            Event::Timer(TAKE_BACK) => {
                shell.kill_timer(TAKE_BACK);
                self.take_back(shell);
            }
            Event::Timer(WAKE) => {
                shell.kill_timer(WAKE);
                self.wake_repair(shell);
            }
            Event::Timer(_) => {}
            Event::Suspend => {
                self.journal.note("уход в сон");
                self.note_power_near_csharp();
            }
            Event::Wake => {
                self.note_power_near_csharp();
                // Несколько событий одного пробуждения (сон, экран) — одна сверка, и не раньше паузы на
                // успокоение (ревизия L6).
                let delay = match self.last_wake {
                    Some(t) if t.elapsed() < self.config.wake_gap => self
                        .config
                        .wake_gap
                        .saturating_sub(t.elapsed())
                        .max(self.config.wake_delay),
                    _ => self.config.wake_delay,
                };
                shell.set_timer(WAKE, delay);
            }
            Event::EndSession => self.journal.note("Windows завершает сеанс"),
            Event::Close => {
                self.journal.note("выход по просьбе окна CoreMap");
                shell.remove_icon();
                shell.destroy();
            }
            Event::IconRestored => {
                shell.forget_icon();
                shell.add_icon(&self.tip());
            }
        }
    }

    /// Кончилось то, чего программа ждала: C#-часть закрылась.
    fn ended(&mut self, shell: &mut Shell, index: usize) {
        if index >= self.watched.len() {
            return;
        }
        let watched = self.watched.remove(index);
        if let Watched::Mutex(mutex, _) = &watched {
            // Ожидание отдало брошенный мьютекс этой программе: сразу отпустить, иначе C#-часть, запущенная следом,
            // решила бы, что её копия уже работает.
            mutex.release();
        }
        drop(watched);
        self.take_back_pending = true;
        shell.set_timer(TAKE_BACK, self.config.grace);
    }

    /// C#-часть закрылась: вернуть себе право писать потолки (outputs/ceiling-ownership-2026-10-14.md §3).
    pub fn take_back(&mut self, shell: &mut Shell) {
        self.take_back_pending = false;
        if self.other().is_some() {
            return; // следом запустилась другая часть (новое окно после прежнего) — ждём её конца
        }
        let done = self.with_ceilings(|app| {
            let topology = &app.topology;
            if let Some((limits, cpu)) = parts(&mut app.limits, &mut app.device) {
                app.refusal = limits
                    .refresh(cpu, topology)
                    .map(|u| format!("потолки не перечитаны из процессора: {u}"));
            }
            let said = app.recover();
            app.read_files();
            if app.power_while_away {
                app.repair_to_profile();
            }
            app.power_while_away = false;
            app.intended = app.limits.as_ref().map(|l| l.current().to_vec());
            app.journal.note("C#-часть закрылась — потолки снова мои");
            said
        });
        match done {
            Ok(Some(said)) => self.notify(shell, &said),
            Ok(None) => {}
            Err(reason) => self
                .journal
                .note(&format!("вернуть право на потолки не вышло: {reason}")),
        }
        if !self.started {
            self.started = true;
        }
        shell.set_tip(&self.tip());
        self.show_balloon(shell);
    }

    /// Пока работала C#-часть, машина спала: если всё, что отличается от профиля запуска, стоит на заводских, —
    /// это сброс сном, профиль ставится заново (без счёта испытательного срока: это не запуск).
    fn repair_to_profile(&mut self) {
        let Some(p) = self.profile.clone().filter(|p| !p.disabled()) else {
            return;
        };
        let topology = &self.topology;
        let Some(limits) = self.limits.as_ref() else { return };
        let reset = topology.blocks.iter().all(|b| {
            let i = b.first;
            !limits.known(i) || limits.current()[i] == p.ratios[i] || at_boot(limits, &self.boot, i)
        });
        if !reset || limits.same(&p.ratios) {
            return;
        }
        if let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
            let refused = limits.apply(cpu, topology, &p.ratios);
            self.journal.note(&match refused {
                None => format!("после сна во время работы окна профиль «{}» поставлен заново", p.name),
                Some(r) => format!(
                    "после сна во время работы окна профиль «{}» поставлен не полностью: {r}",
                    p.name
                ),
            });
        }
    }

    /// Сон или пробуждение, пока работает C#-часть или возврат права ещё впереди: возврат права сверит потолки с
    /// профилем (`repair_to_profile`), а не примет то, что оставил сон (ревизия M2).
    fn note_power_near_csharp(&mut self) {
        if self.take_back_pending || self.other().is_some() {
            self.power_while_away = true;
        }
    }

    /// Сверка после пробуждения: сброшенные к заводским блоки получают своё значение; чужую правку — принять.
    pub fn wake_repair(&mut self, shell: &mut Shell) {
        self.last_wake = Some(Instant::now());
        // Пока C#-часть не закрылась или право ещё не возвращено, потолки могут стоять на ступени оборванного замера:
        // сверять их с тем, что должно стоять, рано — это сделает возврат права (по метке замера и профилю).
        if self.take_back_pending || self.other().is_some() {
            self.power_while_away = true;
            self.journal
                .note("пробуждение, пока работает другая часть CoreMap — сверю потолки после неё");
            return;
        }
        let done = self.with_ceilings(|app| {
            // Метка замера могла остаться от C#-части, конца которой эта программа не видела (ревизия M3).
            if let Some(said) = app.recover() {
                app.intended = app.limits.as_ref().map(|l| l.current().to_vec());
                return Some(Ok(said));
            }
            let topology = &app.topology;
            let (limits, cpu) = parts(&mut app.limits, &mut app.device)?;
            if let Some(unread) = limits.refresh(cpu, topology) {
                return Some(Err(format!("после пробуждения потолки не прочитаны: {unread}")));
            }
            let Some(intended) = app.intended.clone() else {
                app.intended = app.limits.as_ref().map(|l| l.current().to_vec());
                return None;
            };
            let mut repaired = Vec::new();
            let mut foreign = Vec::new();
            let mut refusal = None;
            for block in &topology.blocks {
                let i = block.first;
                let Some((limits, cpu)) = parts(&mut app.limits, &mut app.device) else {
                    break;
                };
                if !limits.known(i) || limits.current()[i] == intended[i] {
                    continue;
                }
                if at_boot(limits, &app.boot, i) {
                    match limits.set(cpu, topology, i, intended[i]) {
                        None => repaired.push(block.name.clone()),
                        Some(r) => refusal = Some(r),
                    }
                } else {
                    foreign.push(block);
                }
            }
            let mut said = None;
            if !foreign.is_empty() {
                let current = app.limits.as_ref().map(|l| l.current().to_vec()).unwrap_or_default();
                if let Some(intended) = app.intended.as_mut() {
                    for block in &foreign {
                        for &m in &block.members {
                            intended[m] = current[m];
                        }
                    }
                }
                let names: Vec<&str> = foreign.iter().map(|b| b.name.as_str()).collect();
                said = Some(format!(
                    "Потолки блоков {} изменила другая программа — оставляю их как есть",
                    names.join(", ")
                ));
            }
            let line = match (repaired.is_empty(), &refusal) {
                (true, None) if foreign.is_empty() => "после пробуждения: потолки на месте".to_string(),
                (true, None) => "после пробуждения: чужая правка принята".to_string(),
                (false, None) => format!("после пробуждения переставлены блоки: {}", repaired.join(", ")),
                (_, Some(r)) => format!("после пробуждения не переставлено: {r}"),
            };
            app.journal.note(&line);
            if let Some(r) = refusal {
                return Some(Err(format!("После пробуждения потолки не восстановлены: {r}")));
            }
            said.map(Ok)
        });
        match done {
            Ok(Some(Ok(said) | Err(said))) => self.notify(shell, &said),
            Ok(None) => {}
            Err(reason) => self.journal.note(&format!("сверка после пробуждения: {reason}")),
        }
        shell.set_tip(&self.tip());
        self.show_balloon(shell);
    }

    // ----- сообщения владельцу -----

    fn queue(&mut self, text: &str) {
        self.journal.note(&format!("сообщение: {text}"));
        self.balloons.push_back(capital(text));
    }

    /// Сообщение владельцу: всплывающее у значка, по одному (Windows показывает одно за раз); каждое — в журнал.
    pub fn notify(&mut self, shell: &mut Shell, text: &str) {
        self.queue(text);
        self.show_balloon(shell);
    }

    fn show_balloon(&mut self, shell: &Shell) {
        if self.balloon_up {
            return;
        }
        if !self.config.launch {
            // Скрытая проверка: значка нет, сообщения собираются для отчёта.
            self.said.extend(self.balloons.drain(..));
            return;
        }
        if let Some(next) = self.balloons.front()
            && shell.balloon(next)
        {
            self.balloons.pop_front();
            self.balloon_up = true;
        }
    }
}

/// Блок стоит на том, к чему его возвращает сброс: на потолке после загрузки или на заводском пределе (если потолки
/// после загрузки неизвестны — программа запущена не при входе).
fn at_boot(limits: &Limits, boot: &[i32], core: usize) -> bool {
    let current = limits.current()[core];
    current == limits.fused()[core] || boot.get(core) == Some(&current)
}

/// Потолки и прибор сразу (оба нужны почти любому действию); поля по отдельности — чтобы рядом можно было брать
/// топологию и прочие поля программы.
fn parts<'a>(
    limits: &'a mut Option<Limits>,
    device: &'a mut Option<Device>,
) -> Option<(&'a mut Limits, &'a mut dyn Cpu)> {
    Some((limits.as_mut()?, device.as_mut()?.cpu()))
}

/// Разделитель; Windows держит его недоступным (так его и видит проверка меню).
fn separator() -> Item {
    Item {
        id: 0,
        text: None,
        checked: false,
        enabled: false,
        default: false,
    }
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// Папка данных рядом с программой.
#[must_use]
pub fn data_dir(root: &Path) -> PathBuf {
    root.join("data")
}
