// Окно CoreMapWindow.exe у программы входа (DECISIONS #32, шаг 2): канал, по которому окно получает состояние и шлёт
// команды, и то, что по нему ходит. Перевозка — pipe.rs; здесь — сообщения и команды.
//
// Сообщения — те же, что шлёт движок на C# (src/Feed.cs), чтобы окно работало с программой входа без переделки:
// • hello — процессор, ядра, блоки, схема кристалла; limits — заводские потолки и что можно поставить блоку;
//   profiles — профиль запуска, сохранённые наборы и какой из них совпадает с тем, что стоит; notice — сообщение
//   владельцу. Шлются при знакомстве и потом только когда изменились.
// • state — показания, дважды в секунду, пока окно подключено (пока пишется журнал срезов — из каждого второго его
//   опроса, app/sense.rs), и сразу после исполненной команды; окна нет — нет ни таймера, ни чтения датчиков.
// Новое, чего у движка не было: settings — выбор владельца для автомодуля и допустимая отсечка (по изменению); в state —
// доля работы ядер (busyShare), потолок автомодуля по блокам (auto), потолки владельца под ним (intended) и отсечка
// автомодуля (autoLimit); bye — программа выходит: окно запоминает себя и закрывается, программа выходит после него
// (`App::leave`; владелец 15.10: «чтобы трей сначала закрывал окно… а потом закрывался сам»).
//
// Команды — тоже как у движка (src/Commands.cs): limit.set, profile.save/open/delete, max.reset, notices.seen; новые —
// profile.startup (отметка профиля запуска) и auto.set (настройки автомодуля из окна, app/auto.rs). Что окно помнит о себе (выбранные
// блоки, подсветку, размер и место), программа входа не получает и не хранит: это настройки окна, их держит оно само
// (DECISIONS #36). Нагрузки и замеров в программе входа нет: их команды отклоняются одной фразой. Поднять потолок выше
// заводского — только после «Да» владельца в окошке самой программы входа (у неё права администратора: в него не
// щёлкнет другая программа). Пока работает прежнее окно или движок на C#, потолками владеют они: команды потолков и
// профилей отклоняются с причиной, состояние идёт без показаний датчиков (их нельзя читать — DECISIONS #31).
//
// Профили и текущее состояние (владелец 15.10, DECISIONS #38): текущее состояние — потолки, которые стоят в процессоре,
// с поправками автомодуля; профили — сохранённые наборы и «заводские». Это разные вещи: выбор набора в окне ничего не
// ставит, окно само показывает разницу выбранного с текущим. profile.open («применить» в окне) ставит набор — он
// становится текущим состоянием, как щелчок по набору в меню значка; profile.save пишет текущее состояние в набор
// (поправки автомодуля становятся потолками владельца). Профиль запуска (data\profile.json — что ставится при входе в
// Windows) выбирается отдельно — profile.startup, отметка «загружать при старте Windows» у сохранённого профиля:
// отмечен один или ни одного; ни применение, ни сохранение его не меняют, кроме того, что сохранение в отмеченный
// профиль обновляет и его потолки при входе. Несохранённые правки при входе не ставятся. Ни «было», ни «принять», ни
// «откатить» больше нет.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use machine::clock::{Pattern, Stamp};
use machine::profile::{self, Set, same_name};
use machine::topology::Kind;
use serde_json::{Map, Value, json};

use super::auto::Choice;
use super::history::Change;
use super::meter::Watch;
use super::{App, Via, parts};
use crate::layout::{self, Layout};
use crate::pipe::{Happened, Server};
use crate::shell::{self, Shell};

/// Таймер состояния: раз в `STATE_EVERY`, пока окно подключено и назвало себя.
pub const STATE: usize = 5;
/// Таймер знакомства: окно подключилось и молчит.
pub const GREET: usize = 6;
pub(super) const STATE_EVERY: Duration = Duration::from_millis(500);
const GREET_EVERY: Duration = Duration::from_millis(500);
/// Подключилось и столько молчит — отключается (Bridge.GreetWithin).
const GREET_WITHIN: Duration = Duration::from_secs(2);
/// Раз во столько потолки перечитываются из процессора, пока открыто окно: их мог сбросить сон или изменить другая
/// программа (Engine.RefreshEvery).
const REFRESH_EVERY: Duration = Duration::from_secs(10);
/// Драйвер не открылся — следующая попытка не раньше: каждая грузит модуль заново.
const CONNECT_EVERY: Duration = Duration::from_secs(10);
/// Как часто писать в журнал цену опроса окна — так она измеряется на машине владельца (как у автомодуля).
const COST_EVERY: Duration = Duration::from_secs(60);
/// Окно запущено, а его процесса ещё не видно: второй щелчок в это время ничего не запускает.
const LAUNCH_GRACE: Duration = Duration::from_secs(5);
/// Таймер выхода: окно не закрылось за `LEAVE_WITHIN` после «bye» — программа выходит без него.
pub const LEAVE: usize = 9;
/// Сколько программа ждёт, пока окно закроется после «bye». Окну для этого нужно записать свой файл (меньше килобайта)
/// и кончить свой цикл — миллисекунды (скрытая проверка их меряет). Срок нужен на окно, которое зависло: программа всё
/// равно выходит, а владелец, выбравший «Выход», не ждёт исчезновения значка дольше нескольких секунд.
pub const LEAVE_WITHIN: Duration = Duration::from_secs(3);
/// Окну: программа выходит — запомни себя и закройся (rust/window/src/wire.rs, `Message::Bye`).
const BYE: &str = r#"{"t":"bye"}"#;
const PROTOCOL: i64 = 1;
/// Название профиля длиннее (в знаках UTF-16, как Length в C#) не принимается.
const NAME_LENGTH: usize = 64;
/// Столько последних сообщений владельцу держится для окна.
const NOTICES_KEPT: usize = 20;
const NO_TUNING: &str = "в этой версии окна нет нагрузки и замеров";
const UNCONFIRMED: &str = "потолок выше заводского владельцем не подтверждён";
/// Файл окна рядом с программой.
pub const WINDOW_EXE: &str = "CoreMapWindow.exe";

/// Кто отвечает на вопрос о потолке выше заводского.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Окошко Windows на экране владельца.
    Ask,
    /// Скрытая проверка: ответ задан заранее, вопрос записывается.
    Answer(bool),
}

/// Окно, подключённое к каналу.
#[derive(Debug)]
struct Session {
    pid: u32,
    since: Instant,
    greeted: bool,
    /// Знакомство отклонено: после отправки ответа канал рвётся.
    closing: bool,
    watch: Watch,
    /// Что из сообщений по изменению уже ушло: заводские потолки и почему потолки не перечитались.
    sent_limits: Option<(Vec<i32>, Option<String>)>,
    sent_profiles: Option<String>,
    seen_current: Vec<i32>,
    seen_version: u64,
    /// Какой выбор автомодуля окно уже знает (тема settings).
    sent_settings: Option<Choice>,
    sent_notice: u64,
    /// Почему в состоянии нет показаний датчиков; None — есть.
    trouble: Option<String>,
    /// Почему потолки не перечитались; None — перечитались.
    unread: Option<String>,
    next_refresh: Instant,
    next_connect: Instant,
    /// Когда цена опроса последний раз записана в журнал.
    last_cost: Option<Instant>,
}

/// Программа выходит и ждёт, пока закроется окно (`App::leave`).
#[derive(Debug, Clone, Copy)]
struct Leaving {
    since: Instant,
    /// Окно запущено, но ещё не подключилось: его ждут, чтобы сказать «bye» и ему.
    awaiting: bool,
    /// «bye» сказан окну.
    told: bool,
}

/// Канал окна и всё, что программа о нём помнит.
#[derive(Debug)]
pub struct Serving {
    /// Имя канала; окну передаётся при запуске (--pipe).
    pub pipe: String,
    server: Option<Server>,
    /// Почему канала нет.
    trouble: Option<String>,
    session: Option<Session>,
    layout: Option<Layout>,
    notices: VecDeque<(u64, String)>,
    notice_seq: u64,
    notices_seen: u64,
    launched_at: Option<Instant>,
    /// Программа выходит и ждёт окно; None — не выходит.
    leaving: Option<Leaving>,
    /// Буфер сообщения «состояние»: оно собирается дважды в секунду, память — одна и та же.
    text: String,
    pub confirm: Confirm,
    /// Скрытая проверка: о чём спрашивали владельца.
    pub asked: Vec<String>,
    /// Скрытая проверка: сколько окон подключалось, сколько состояний ушло.
    pub sessions: u64,
    pub states: u64,
    /// Скрытая проверка: когда программа вышла (после окна или без него); None — не выходила.
    pub left: Option<Instant>,
}

impl Serving {
    #[must_use]
    pub fn new(pipe: String, confirm: Confirm) -> Self {
        Self {
            pipe,
            server: None,
            trouble: None,
            session: None,
            layout: None,
            notices: VecDeque::new(),
            notice_seq: 0,
            notices_seen: 0,
            launched_at: None,
            leaving: None,
            text: String::new(),
            confirm,
            asked: Vec::new(),
            sessions: 0,
            states: 0,
            left: None,
        }
    }

    /// К каналу подключено окно, которое назвало себя.
    #[must_use]
    pub fn attached(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.greeted)
    }

    /// Программа выходит и ждёт, пока закроется окно.
    #[must_use]
    pub fn leaving(&self) -> bool {
        self.leaving.is_some()
    }

    /// Сообщение владельцу — и окну (notice).
    pub fn notice(&mut self, text: &str) {
        self.notice_seq += 1;
        self.notices.push_back((self.notice_seq, text.to_string()));
        while self.notices.len() > NOTICES_KEPT {
            self.notices.pop_front();
        }
    }

    /// Дескрипторы канала, которых ждёт цикл программы.
    #[must_use]
    pub fn handles(&self) -> Vec<windows_sys::Win32::Foundation::HANDLE> {
        self.server.as_ref().map(Server::handles).unwrap_or_default()
    }

    /// Что видит подключённое окно; None — окна нет или оно ещё не назвало себя.
    pub(super) fn watch_mut(&mut self) -> Option<&mut Watch> {
        self.session.as_mut().filter(|s| s.greeted).map(|s| &mut s.watch)
    }

    pub(super) fn watch(&self) -> Option<&Watch> {
        self.session.as_ref().filter(|s| s.greeted).map(|s| &s.watch)
    }

    /// Пора ли опросу окна: окно подключено и прошлый его опрос был не ближе `min` (`Watch::due`).
    pub(super) fn window_due(&self, now: Instant, min: Duration) -> bool {
        self.watch().is_some_and(|w| w.due(now, min))
    }
}

/// Число с не более чем `decimals` знаками после точки, без хвостовых нулей; None — null (Out.Fix в Wire.cs).
fn fix(text: &mut String, value: Option<f64>, decimals: i32) {
    match value.filter(|v| v.is_finite()) {
        None => text.push_str("null"),
        Some(v) => {
            let scale = 10_f64.powi(decimals);
            let rounded = (v * scale).round() / scale;
            let mut number = format!("{rounded:.prec$}", prec = usize::try_from(decimals).unwrap_or(0));
            if number.contains('.') {
                number.truncate(number.trim_end_matches('0').trim_end_matches('.').len());
            }
            if number == "-0" {
                number = "0".into();
            }
            text.push_str(&number);
        }
    }
}

fn row(text: &mut String, values: &[Option<f64>], decimals: i32) {
    text.push('[');
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            text.push(',');
        }
        fix(text, *v, decimals);
    }
    text.push(']');
}

fn ints(text: &mut String, values: impl Iterator<Item = Option<i32>>) {
    text.push('[');
    for (i, v) in values.enumerate() {
        if i > 0 {
            text.push(',');
        }
        match v {
            Some(v) => {
                let _ = write!(text, "{v}"); // into a String: cannot fail
            }
            None => text.push_str("null"),
        }
    }
    text.push(']');
}

fn key(text: &mut String, name: &str) {
    if !text.ends_with('{') {
        text.push(',');
    }
    text.push('"');
    text.push_str(name);
    text.push_str("\":");
}

fn reply(id: i64, refusal: Option<&str>, note: Option<&str>) -> String {
    let mut map = Map::new();
    map.insert("t".into(), json!("reply"));
    map.insert("re".into(), json!(id));
    map.insert("ok".into(), json!(refusal.is_none()));
    if let Some(why) = refusal {
        map.insert("why".into(), json!(why));
    }
    if let Some(note) = note {
        map.insert("note".into(), json!(note));
    }
    Value::Object(map).to_string()
}

/// Число из аргументов команды: только целое JSON (как `raw is int` в Commands.cs).
fn int(f: &Map<String, Value>, name: &str) -> Option<i64> {
    f.get(name).and_then(Value::as_i64)
}

/// Название профиля; "" — заводские (где так сказано в действии).
fn name_of(f: &Map<String, Value>) -> Option<&str> {
    f.get("name")
        .and_then(Value::as_str)
        .filter(|n| n.encode_utf16().count() <= NAME_LENGTH)
}

impl App {
    // ----- канал -----

    /// Создаёт канал окна. Не вышло — программа работает без окна, причина — в журнале и в ответе на «Окно». Скрытая
    /// проверка зовёт и сама: канал снова есть после `cut_pipe`.
    pub fn open_server(&mut self) {
        let created = crate::pipe::session_user().and_then(|user| Server::create(&self.serving.pipe, &user));
        match created {
            Ok(server) => self.serving.server = Some(server),
            Err(e) => {
                self.journal.error(&format!("канал окна не создан: {e}"));
                self.serving.trouble = Some(e);
            }
        }
    }

    /// Сработало событие канала.
    pub(super) fn serve_io(&mut self, shell: &mut Shell) {
        let Some(server) = self.serving.server.as_mut() else {
            return;
        };
        for happened in server.pump() {
            match happened {
                Happened::Connected(pid) => self.client_came(shell, pid),
                Happened::Gone(why) => self.client_left(shell, &why),
            }
        }
        self.read_messages(shell);
        let done = self.serving.session.as_ref().is_some_and(|s| s.closing)
            && self.serving.server.as_ref().is_some_and(|s| !s.writing());
        if done {
            self.drop_client(shell, "отказано в знакомстве");
        }
        // Программа выходит, а окно только что отключилось: теперь выходит и она.
        self.leave_if_alone(shell);
    }

    /// Скрытая проверка: канал пропадает, как при падении программы, — без «bye», окно просто теряет связь. Снова —
    /// `open_server`.
    pub fn cut_pipe(&mut self, shell: &Shell) {
        // Канал закрывается вместе с сервером (Drop: идущие операции обрываются и дожидаются, затем CloseHandle).
        self.serving.server = None;
        self.client_left(shell, "проверка: канал пропал без «bye»");
    }

    fn client_came(&mut self, shell: &Shell, pid: u32) {
        let exe = crate::launch::process_path(pid).unwrap_or_else(|| "exe неизвестен".into());
        self.journal
            .note(&format!("к каналу подключилось окно: процесс {pid}, {exe}"));
        self.serving.sessions += 1;
        let now = Instant::now();
        self.serving.session = Some(Session {
            pid,
            since: now,
            greeted: false,
            closing: false,
            watch: Watch::new(self.topology.cores.len()),
            sent_limits: None,
            sent_profiles: None,
            seen_current: Vec::new(),
            seen_version: 0,
            sent_settings: None,
            sent_notice: 0,
            trouble: None,
            unread: None,
            next_refresh: now,
            next_connect: now,
            last_cost: None,
        });
        // Программа уже выходит (окно запустили перед самым «Выходом»): это окно сразу получает «bye».
        if let Some(leaving) = self.serving.leaving.as_mut() {
            leaving.awaiting = false;
            self.say_bye(shell);
            return;
        }
        shell.set_timer(GREET, GREET_EVERY);
    }

    fn client_left(&mut self, shell: &Shell, why: &str) {
        let Some(session) = self.serving.session.take() else {
            return;
        };
        shell.kill_timer(STATE);
        shell.kill_timer(GREET);
        self.journal.note(&format!(
            "окно отключилось ({why}), процесс {}, было подключено {} с",
            session.pid,
            session.since.elapsed().as_secs()
        ));
        // Окна нет: драйвер открыт только ради автомодуля.
        if !self.auto.choice.on
            && let Some(device) = &mut self.device
        {
            device.cpu().release();
        }
    }

    fn drop_client(&mut self, shell: &Shell, why: &str) {
        if let Some(server) = self.serving.server.as_mut() {
            server.lose(why);
        }
        self.client_left(shell, why);
    }

    fn send(&mut self, shell: &Shell, text: &str) {
        let Some(server) = self.serving.server.as_mut() else {
            return;
        };
        if let Err(e) = server.send(text) {
            self.drop_client(shell, &e);
        }
    }

    fn read_messages(&mut self, shell: &mut Shell) {
        loop {
            let Some(server) = self.serving.server.as_mut() else {
                return;
            };
            match server.message() {
                Ok(Some(body)) => {
                    if let Err(why) = self.on_message(shell, &body) {
                        self.journal.note(&format!("канал окна: нарушение протокола — {why}"));
                        self.drop_client(shell, &format!("нарушение протокола: {why}"));
                        return;
                    }
                }
                Ok(None) => return,
                Err(why) => {
                    self.journal.note(&format!("канал окна: нарушение протокола — {why}"));
                    self.drop_client(shell, &format!("нарушение протокола: {why}"));
                    return;
                }
            }
        }
    }

    /// Сообщение окна. Err — нарушение протокола (не JSON, нет номера или команды, первое — не hello): разрыв.
    fn on_message(&mut self, shell: &mut Shell, body: &[u8]) -> Result<(), String> {
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(body) else {
            return Err("не JSON-объект".into());
        };
        let id = int(&fields, "id").ok_or("в сообщении нет номера")?;
        let command = fields
            .get("c")
            .and_then(Value::as_str)
            .ok_or("в сообщении нет команды")?
            .to_string();
        let Some(session) = self.serving.session.as_mut() else {
            return Ok(());
        };
        // Окну сказано «bye»: оно закрывается, его команды уже ничего не значат.
        if session.closing || self.serving.leaving.is_some() {
            return Ok(());
        }
        if !session.greeted {
            if command != "hello" {
                return Err("первое сообщение — не hello".into());
            }
            if int(&fields, "proto") != Some(PROTOCOL) {
                session.closing = true;
                let refusal = format!("программа входа говорит на протоколе {PROTOCOL}, окно — на другом");
                self.send(shell, &reply(id, Some(&refusal), None));
                return Ok(());
            }
            self.greet(shell, id);
            return Ok(());
        }
        let (refusal, note) = self.execute(shell, &command, &fields);
        if let Some(refusal) = &refusal {
            self.journal.note(&format!("окно: «{command}» отклонено — {refusal}"));
        }
        self.send(shell, &reply(id, refusal.as_deref(), note.as_deref()));
        // Следом за ответом — что изменилось и состояние: окно видит исполненное сразу, а не через полсекунды.
        self.push_topics(shell);
        self.push_state(shell);
        Ok(())
    }

    /// Окно назвало себя: ответ, знакомство, всё, что дальше шлётся по изменению, и первое состояние.
    fn greet(&mut self, shell: &mut Shell, id: i64) {
        if let Some(session) = self.serving.session.as_mut() {
            session.greeted = true;
        }
        shell.kill_timer(GREET);
        self.send(shell, &reply(id, None, None));
        let hello = self.hello();
        self.send(shell, &hello);
        // Сообщения владельцу, которые окно не отметило прочитанными, — заново (как Feed.Greeting).
        self.serve_tick(shell);
        shell.set_timer(STATE, STATE_EVERY);
    }

    /// Таймер знакомства: подключилось и молчит дольше `GREET_WITHIN` — отключить.
    pub(super) fn greet_tick(&mut self, shell: &Shell) {
        match &self.serving.session {
            Some(s) if !s.greeted => {
                if s.since.elapsed() >= GREET_WITHIN {
                    self.drop_client(shell, &format!("окно молчит {} с", GREET_WITHIN.as_secs()));
                }
            }
            _ => shell.kill_timer(GREET),
        }
    }

    /// Таймер состояния: показания и состояние окну, по изменению — остальное.
    pub(super) fn serve_tick(&mut self, shell: &Shell) {
        if !self.serving.attached() {
            shell.kill_timer(STATE);
            return;
        }
        if self.serving.server.as_ref().is_some_and(Server::stalled) {
            self.drop_client(shell, "окно не читает канал 2 с");
            return;
        }
        // Пока пишется журнал срезов, опрос окна — каждый второй его опрос (app/sense.rs): он и шлёт состояние, и цену
        // чтения пишет он. Журнал только что кончился, а его опрос окна был меньше четверти секунды назад — это
        // состояние пропускается, следующее — через полсекунды: средние идут по промежуткам около полсекунды.
        if self.logging.on || !self.serving.window_due(Instant::now(), STATE_EVERY / 2) {
            return;
        }
        let started = Instant::now();
        self.sense();
        let cost = started.elapsed();
        if let Some(session) = self.serving.session.as_mut()
            && session.last_cost.is_none_or(|t| t.elapsed() >= COST_EVERY)
        {
            session.last_cost = Some(Instant::now());
            let line = format!(
                "окно: опрос {} мкс{}",
                cost.as_micros(),
                if session.trouble.is_some() {
                    ", без датчиков"
                } else {
                    ", все ядра"
                }
            );
            self.journal.note(&line);
        }
        // Прошлое ещё пишется: это состояние пропускается, следующее придёт через полсекунды.
        if self.serving.server.as_ref().is_some_and(Server::writing) {
            return;
        }
        self.push_topics(shell);
        self.push_state(shell);
    }

    // ----- датчики -----

    /// Опрос окна по его таймеру, пока журнал срезов не пишется: все ядра, корпус, раз в 10 с — потолки. Держит другая
    /// часть `CoreMap` — датчики не читаются.
    fn sense(&mut self) {
        let reason = if let Some(other) = self.other() {
            Some(other.menu_text().to_string())
        } else if self.limits.is_none()
            && self
                .serving
                .session
                .as_ref()
                .is_some_and(|s| Instant::now() < s.next_connect)
        {
            Some(self.unavailable.clone().map_or_else(
                || "потолки множителей недоступны".into(),
                |u| format!("датчики недоступны: {u}"),
            ))
        } else {
            match self.with_ceilings(Self::sense_all) {
                Ok(Ok(())) => {
                    // Обороты — вне аренды: вызов прошивки бывает долгим (до 4 с), стартующая C#-часть его не ждёт.
                    self.read_fans();
                    None
                }
                Ok(Err(e)) => {
                    // Драйвер отказал: при следующем опросе он откроется заново (как у автомодуля, ревизия M5).
                    if let Some(device) = &mut self.device {
                        device.cpu().release();
                    }
                    Some(e)
                }
                Err(e) => {
                    if let Some(session) = self.serving.session.as_mut() {
                        session.next_connect = Instant::now() + CONNECT_EVERY;
                    }
                    Some(e)
                }
            }
        };
        self.serving_trouble(reason);
    }

    /// Итог чтения датчиков для окна: None — показания есть; Some — почему их нет (окно покажет это строкой, а
    /// показания ядер — пустыми).
    pub(super) fn serving_trouble(&mut self, reason: Option<String>) {
        let Some(session) = self.serving.session.as_mut() else {
            return;
        };
        if reason.is_some() {
            session.watch.blank();
        }
        session.trouble = reason;
    }

    /// Под арендой: корпус и каждое ядро (как Monitor в C#) — одним путём чтения (app/sense.rs), затем средние под
    /// нагрузкой.
    fn sense_all(&mut self) -> Result<(), String> {
        self.read_sensors(true)?;
        self.feed_means();
        Ok(())
    }

    /// Опрос окна окончен: показания за полсекунды и средние под нагрузкой при нынешних потолках (`Watch::polled`).
    fn feed_means(&mut self) {
        if let (Some(session), Some(limits)) = (self.serving.session.as_mut(), &self.limits) {
            session.watch.polled(limits.current(), Instant::now());
        }
    }

    /// Опрос окна в опросе срезов (app/sense.rs) окончен: средние под нагрузкой, затем — что изменилось и состояние,
    /// как по таймеру состояния.
    pub(super) fn window_polled(&mut self, shell: &Shell) {
        self.feed_means();
        if self.serving.server.as_ref().is_some_and(Server::writing) {
            return;
        }
        self.push_topics(shell);
        self.push_state(shell);
    }

    /// Раз в 10 с, пока открыто окно, — потолки заново из процессора: их мог сбросить сон или изменить другая
    /// программа. Под арендой, в опросе датчиков.
    pub(super) fn refresh_for_window(&mut self) {
        let Some(session) = self.serving.session.as_mut().filter(|s| s.greeted) else {
            return;
        };
        if Instant::now() < session.next_refresh {
            return;
        }
        session.next_refresh = Instant::now() + REFRESH_EVERY;
        if let Some((limits, cpu)) = parts(&mut self.limits, &mut self.device) {
            session.unread = limits.refresh(cpu, &self.topology);
        }
    }

    // ----- сообщения -----

    fn hello(&mut self) -> String {
        if self.serving.layout.is_none() {
            let mut skipped = Vec::new();
            let layout = layout::pick(
                &self.topology.cores,
                &self.topology.blocks,
                &machine::topology::cpu_identifier(),
                &self.config.root.join("layouts"),
                &mut skipped,
            );
            for line in skipped {
                self.journal.note(&line);
            }
            self.serving.layout = Some(layout);
        }
        let kind = |k: Kind| if k == Kind::P { "P" } else { "E" };
        let cores: Vec<Value> = self
            .topology
            .cores
            .iter()
            .map(|c| json!({ "lp": c.lp.0, "kind": kind(c.kind), "cluster": c.cluster }))
            .collect();
        let blocks: Vec<Value> = self
            .topology
            .blocks
            .iter()
            .enumerate()
            .map(|(index, b)| {
                let title = if b.members.len() == 1 {
                    format!("ядро {}", b.name)
                } else {
                    format!("блок E-ядер {}", b.name)
                };
                json!({
                    "index": index, "members": b.members, "first": b.first, "lp": b.lp.0,
                    "kind": kind(b.kind), "name": b.name, "title": title,
                })
            })
            .collect();
        json!({
            "t": "hello", "proto": PROTOCOL, "build": crate::VERSION, "version": crate::VERSION,
            "engine": "logon", "simulated": self.config.simulate, "cpu": self.store.cpu(),
            "cores": cores, "blocks": blocks,
            "layout": self.serving.layout.as_ref().map(Layout::json),
            "sensors": true, "sensorProblem": Value::Null, "limits": self.limits.is_some(), "curves": false,
            "levels": [], "durations": [], "passes": [], "over": machine::limits::OVER,
        })
        .to_string()
    }

    /// Что изменилось с прошлого раза: потолки, профили, выбор автомодуля, сообщения владельцу.
    fn push_topics(&mut self, shell: &Shell) {
        // После «bye» окну больше ничего не шлётся: оно уже не читает канал.
        if !self.serving.attached() || self.serving.leaving.is_some() {
            return;
        }
        let mut out: Vec<String> = Vec::new();
        if let Some(limits) = &self.limits {
            let unread = self.serving.session.as_ref().and_then(|s| s.unread.clone());
            let now = (limits.fused().to_vec(), unread);
            if let Some(session) = self.serving.session.as_mut()
                && session.sent_limits.as_ref() != Some(&now)
            {
                let choices: Vec<Vec<i32>> = self.topology.blocks.iter().map(|b| limits.choices(b.first)).collect();
                out.push(json!({ "t": "limits", "fused": now.0, "unread": now.1, "choices": choices }).to_string());
                session.sent_limits = Some(now);
            }
            // Профили — только после команды, смены набора или когда потолки сдвинулись: больше им меняться не от
            // чего (как Feed.Changed).
            let stale = self.serving.session.as_ref().is_some_and(|s| {
                s.sent_profiles.is_none() || s.seen_version != self.version || s.seen_current != limits.current()
            });
            if stale {
                let profiles = profiles_topic(self, &self.listed_sets());
                if let Some(session) = self.serving.session.as_mut() {
                    limits.current().clone_into(&mut session.seen_current);
                    session.seen_version = self.version;
                    if session.sent_profiles.as_deref() != Some(profiles.as_str()) {
                        session.sent_profiles = Some(profiles.clone()); // kept to compare; sent once per change
                        out.push(profiles);
                    }
                }
            }
        }
        // Выбор автомодуля — при знакомстве и после каждой смены (окно, меню значка).
        let choice = self.auto.choice;
        if let Some(session) = self.serving.session.as_mut()
            && session.sent_settings != Some(choice)
        {
            session.sent_settings = Some(choice);
            out.push(choice.topic());
        }
        let (seen, last) = (self.serving.notices_seen, self.serving.notice_seq);
        if let Some(session) = self.serving.session.as_mut()
            && session.sent_notice < last
        {
            let from = session.sent_notice.max(seen);
            for (seq, text) in self.serving.notices.iter().filter(|(seq, _)| *seq > from) {
                out.push(json!({ "t": "notice", "seq": seq, "text": text }).to_string());
            }
            session.sent_notice = last;
        }
        for message in out {
            self.send(shell, &message);
        }
    }

    /// Сообщение «состояние» (Feed.State): по ядрам — температура, наибольшая, множитель, средние под нагрузкой,
    /// нагрузка, потолок; корпус, вентиляторы; новое — доля работы, потолок автомодуля и потолки владельца.
    fn push_state(&mut self, shell: &Shell) {
        let Some(session) = self.serving.session.as_ref() else {
            return;
        };
        if !session.greeted || self.serving.leaving.is_some() {
            return;
        }
        let mut s = std::mem::take(&mut self.serving.text);
        s.clear();
        let w = &session.watch;
        self.serving.states += 1;
        s.push_str("{\"t\":\"state\"");
        key(&mut s, "n");
        let _ = write!(s, "{}", self.serving.states); // into a String: cannot fail
        key(&mut s, "up");
        fix(&mut s, Some(session.since.elapsed().as_secs_f64()), 1);
        key(&mut s, "pkg");
        fix(&mut s, w.pkg, 0);
        key(&mut s, "pkgMax");
        fix(&mut s, w.pkg_max, 0);
        key(&mut s, "since");
        fix(&mut s, Some(w.since.elapsed().as_secs_f64()), 0);
        s.push_str(",\"busy\":false,\"level\":0");
        key(&mut s, "cur");
        row(&mut s, &w.cur, 0);
        key(&mut s, "max");
        row(&mut s, &w.max, 0);
        key(&mut s, "ratio");
        row(&mut s, &w.ratio, 1);
        key(&mut s, "avg");
        row(&mut s, w.avg(), 1);
        key(&mut s, "avgRatio");
        row(&mut s, w.avg_ratio(), 1);
        // Троттлинга программа входа не читает: признаки — нули (поле окну нужно).
        key(&mut s, "thr");
        ints(&mut s, w.cur.iter().map(|_| Some(0)));
        // «Нагружено» — доля работы ядра не меньше BUSY (нагрузку теперь даёт не программа, а игра).
        key(&mut s, "load");
        ints(
            &mut s,
            w.busy
                .iter()
                .map(|b| Some(i32::from(b.is_some_and(|b| b >= machine::auto::BUSY)))),
        );
        key(&mut s, "limit");
        match (&self.limits, &session.trouble) {
            (Some(limits), None) => ints(&mut s, limits.current().iter().map(|&c| Some(c))),
            _ => s.push_str("null"),
        }
        key(&mut s, "fans");
        s.push('{');
        key(&mut s, "rpm");
        match self.auto.rpm.as_deref() {
            Some(rpm) => ints(&mut s, rpm.iter().map(|&r| i32::try_from(r).ok())),
            None => s.push_str("null"),
        }
        s.push_str(",\"max\":false,\"spin\":false,\"mode\":0}");
        s.push_str(",\"run\":null,\"trip\":null");
        key(&mut s, "sensorError");
        match &session.trouble {
            Some(t) => s.push_str(&serde_json::Value::from(t.as_str()).to_string()),
            None => s.push_str("null"),
        }
        key(&mut s, "busyShare");
        row(&mut s, &w.busy, 2);
        key(&mut s, "auto");
        ints(&mut s, self.auto.overlay.iter().copied());
        key(&mut s, "intended");
        match &self.intended {
            Some(intended) => ints(&mut s, intended.iter().map(|&c| Some(c))),
            None => s.push_str("null"),
        }
        // Отсечка автомодуля, °C — окно рисует её линией на графике статистики; null — автомодуль выключен.
        key(&mut s, "autoLimit");
        if self.auto.choice.on {
            s.push_str(&self.auto.choice.limit.to_string());
        } else {
            s.push_str("null");
        }
        s.push('}');
        self.send(shell, &s);
        self.serving.text = s;
    }

    // ----- команды -----

    /// Команда окна. Возвращает отказ (None — сделано) и итог одной строкой, если он есть.
    fn execute(&mut self, shell: &Shell, command: &str, f: &Map<String, Value>) -> (Option<String>, Option<String>) {
        let bad = || Some(format!("негодные аргументы команды «{command}»"));
        let refusal = match command {
            "limit.set" => match (int(f, "block"), int(f, "ratio")) {
                (Some(block), Some(ratio)) => self.set_limit(block, ratio),
                _ => bad(),
            },
            "profile.save" => name_of(f).map_or_else(bad, |name| self.save_profile(name)),
            "profile.open" => name_of(f).map_or_else(bad, |name| self.apply_profile(name)),
            "profile.delete" => name_of(f).map_or_else(bad, |name| self.delete_profile(name)),
            "profile.startup" => match (name_of(f), f.get("on").and_then(Value::as_bool)) {
                (Some(name), Some(on)) => self.mark_startup(name, on),
                _ => bad(),
            },
            "max.reset" => {
                if let Some(session) = self.serving.session.as_mut() {
                    session.watch.reset();
                }
                None
            }
            "auto.set" => self.set_auto(shell, f),
            "notices.seen" => match int(f, "seq").and_then(|v| u64::try_from(v).ok()) {
                Some(seq) => {
                    self.serving.notices_seen = self.serving.notices_seen.max(seq);
                    None
                }
                None => bad(),
            },
            "load.toggle" | "load.stop" | "level.set" | "run.start" | "run.stop" | "fan.mode" | "heat.mode"
            | "fit.apply" | "fit.view" => Some(NO_TUNING.into()),
            _ => Some(format!("неизвестная команда «{command}»")),
        };
        self.version += 1;
        shell.set_tip(&self.tip());
        (refusal, None)
    }

    /// Потолки уже прочитаны или их удалось прочитать сейчас (драйвер открывается при первом действии).
    fn ready(&mut self) -> Result<(), String> {
        self.with_ceilings(|_| ())
    }

    /// Можно ли ставить такие потолки: ни один не поднимается выше заводского дальше, чем стоит сейчас, или владелец
    /// ответил «Да» (Commands.Allowed).
    fn allowed(&mut self, ratios: &[i32], what: &str) -> bool {
        let Some(limits) = &self.limits else {
            return true; // потолков нет — действие и так будет отклонено
        };
        let over = (0..ratios.len().min(limits.fused().len()))
            .filter(|&i| limits.known(i) && ratios[i] > limits.fused()[i] && ratios[i] > limits.current()[i])
            .map(|i| ratios[i] - limits.fused()[i])
            .max()
            .unwrap_or(0);
        if over == 0 {
            return true;
        }
        let question = format!("{what} — выше заводского на {over}?");
        self.serving.asked.push(question.clone());
        let yes = match self.serving.confirm {
            Confirm::Answer(answer) => answer,
            Confirm::Ask => shell::confirm(&question),
        };
        self.journal.note(&format!(
            "вопрос владельцу: {question} — {}",
            if yes { "да" } else { "нет" }
        ));
        yes
    }

    /// Пишет строку истории профилей.
    pub(super) fn log_change(
        &mut self,
        act: &str,
        name: Option<&str>,
        before: &[i32],
        block: Option<&str>,
        note: Option<&str>,
    ) {
        let after = self.limits.as_ref().map(|l| l.current().to_vec()).unwrap_or_default();
        let change = Change {
            act,
            name,
            before,
            after: &after,
            block,
            note,
        };
        if let Err(e) = self.history.add(&change, Stamp::now()) {
            self.journal.error(&e);
        }
    }

    fn current(&self) -> Vec<i32> {
        self.limits.as_ref().map(|l| l.current().to_vec()).unwrap_or_default()
    }

    /// limit.set: потолок блока вручную. Поправку автомодуля у этого блока заменяет выбор владельца.
    fn set_limit(&mut self, block: i64, ratio: i64) -> Option<String> {
        let bad = Some("негодные аргументы команды «limit.set»".to_string());
        let Some(b) = usize::try_from(block).ok().filter(|&b| b < self.topology.blocks.len()) else {
            return bad;
        };
        let Ok(ratio) = i32::try_from(ratio) else {
            return bad;
        };
        if let Some(other) = self.other() {
            return Some(other.menu_text().into());
        }
        if let Err(e) = self.ready() {
            return Some(e);
        }
        let (first, members, name, kind) = {
            let block = &self.topology.blocks[b];
            (block.first, block.members.clone(), block.name.clone(), block.kind)
        };
        let before = self.current();
        // Значение вне допустимого отклонит Limits::set — спрашивать о нём владельца незачем.
        if self.limits.as_ref().is_some_and(|l| l.choices(first).contains(&ratio)) {
            let mut wanted = before.clone();
            for &m in &members {
                wanted[m] = ratio;
            }
            let whom = if kind == Kind::P {
                format!("ядру {name}")
            } else {
                format!("блоку E-ядер {name}")
            };
            if !self.allowed(&wanted, &format!("Поставить {whom} потолок ×{ratio}")) {
                return Some(UNCONFIRMED.into());
            }
        }
        let done = self.with_ceilings(|app| {
            let topology = &app.topology;
            let (limits, cpu) = parts(&mut app.limits, &mut app.device)?;
            let refusal = limits.set(cpu, topology, first, ratio);
            // Что стоит после попытки — то и должно стоять (как после смены набора).
            let current = limits.current().to_vec();
            if let Some(intended) = app.intended.as_mut() {
                for &m in &members {
                    intended[m] = current[m];
                }
            } else {
                app.intended = Some(current);
            }
            app.auto.overlay[b] = None;
            app.auto_block_fresh(b);
            if let Err(e) = app.mark() {
                app.journal.error(&format!("метка автомодуля: {e}"));
            }
            Some(refusal)
        });
        match done {
            Ok(Some(None)) => {
                self.journal.note(&format!("из окна поставлен блоку {name} ×{ratio}"));
                self.log_change("limit", None, &before, Some(&name), None);
                None
            }
            Ok(Some(Some(refusal))) | Err(refusal) => Some(refusal),
            Ok(None) => Some("потолки множителей недоступны".into()),
        }
    }

    /// Наборы из файла заново: их могло изменить прежнее окно, пока программа входа держала старый список. Файл не
    /// читается — профили не пишутся (половина списка поверх целого хуже отказа).
    fn fresh_sets(&mut self) -> Result<Vec<Set>, String> {
        self.store
            .read_sets()
            .map_err(|spoiled| format!("список профилей не прочитан, профили не меняю: {spoiled}"))
    }

    /// profile.save: текущее состояние (с поправками автомодуля) — в набор с этим названием (есть — перезаписывается).
    /// Профиль запуска не меняется; если это он — при входе в Windows будут его новые потолки.
    fn save_profile(&mut self, name: &str) -> Option<String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Some("впиши название профиля".into());
        }
        if let Some(other) = self.other() {
            return Some(other.menu_text().into());
        }
        let before = self.current();
        let done = self.with_ceilings(|app| -> Result<(), String> {
            let mut sets = app.fresh_sets()?;
            app.reread()?;
            app.absorb("сохранён профиль");
            let limits = app.limits.as_ref().ok_or("потолки множителей недоступны")?;
            let current = limits.current().to_vec();
            profile::put_set(&mut sets, &name, &current);
            app.store
                .write_sets(&sets)
                .map_err(|e| format!("файл профилей не записан: {e}"))?;
            app.sets = sets;
            if app.at_startup(&name) {
                let saved = profile::save(&app.store, limits, &name, Stamp::now())
                    .map_err(|e| format!("файл профиля не записан: {e}"))?;
                app.profile = Some(saved);
                app.note = None;
            }
            app.intended = Some(current);
            app.preferred = Some(name.clone());
            app.auto_new_set();
            Ok(())
        });
        match done {
            Ok(Ok(())) => {
                self.journal.note(&format!("из окна сохранён профиль «{name}»"));
                self.log_change("save", Some(&name), &before, None, None);
                None
            }
            Ok(Err(refusal)) | Err(refusal) => Some(refusal),
        }
    }

    /// profile.open — «применить» в окне: ставит набор («» — заводские), он становится текущим состоянием (тем же путём,
    /// что щелчок по набору в меню значка, `App::apply_set`); профиль запуска не меняется. Набор с потолком выше
    /// заводского — только после «Да» владельца.
    fn apply_profile(&mut self, name: &str) -> Option<String> {
        let name = name.trim();
        if let Some(other) = self.other() {
            return Some(other.menu_text().into());
        }
        if let Err(e) = self.ready() {
            return Some(e);
        }
        if let Ok(sets) = self.fresh_sets() {
            self.sets = sets;
        }
        let Some(ratios) = self.set_ratios(name) else {
            return Some(format!("профиля «{name}» нет"));
        };
        let what = if name.is_empty() {
            "Применить заводские потолки".to_string()
        } else {
            format!("Применить профиль «{name}»")
        };
        if !self.allowed(&ratios, &what) {
            return Some(UNCONFIRMED.into());
        }
        self.apply_set(name, Some(ratios), Via::Window).err()
    }

    /// profile.startup: отметка «загружать при старте Windows» у сохранённого профиля (владелец 15.10: «тот профиль,
    /// который опрувнули для применения при загрузке… если профиль не сохранен… его нельзя поставить на автозагрузку»).
    /// on — он становится профилем запуска (отмечен только один: прежняя отметка уходит), с правилами защиты при входе
    /// (пробный срок для потолков выше заводского, отключение после сбоя); off у отмеченного — при входе ничего не
    /// ставится. Потолки в процессоре не трогаются.
    fn mark_startup(&mut self, name: &str, on: bool) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return Some("при старте Windows загружается только сохранённый профиль — «заводские» им не бывают".into());
        }
        if let Some(other) = self.other() {
            return Some(other.menu_text().into());
        }
        if !on {
            if !self.at_startup(name) {
                return Some(format!("«{name}» и так не загружается при старте Windows"));
            }
            if let Err(e) = self.store.forget_profile() {
                return Some(format!("файл профиля не удалён: {e}"));
            }
            self.profile = None;
            self.note = None;
            self.journal
                .note(&format!("из окна: «{name}» больше не загружается при старте Windows"));
            return None;
        }
        match self.fresh_sets() {
            Ok(sets) => self.sets = sets,
            Err(refusal) => return Some(refusal),
        }
        let Some(set) = self.listed_sets().into_iter().find(|s| same_name(&s.name, name)) else {
            return Some(format!("профиля «{name}» нет — сначала сохрани его"));
        };
        // Уже отмечен и не отключён после сбоя — заново не пишется: это обнулило бы его испытательный срок.
        if self
            .profile
            .as_ref()
            .is_some_and(|p| same_name(&p.name, name) && !p.disabled())
        {
            return None;
        }
        // Пробный срок или нет — по заводским потолкам: они известны после первого чтения.
        if let Err(e) = self.ready() {
            return Some(e);
        }
        let Some(limits) = self.limits.as_ref() else {
            return Some("потолки множителей недоступны".into());
        };
        match profile::mark(&self.store, limits, &set.name, &set.ratios, Stamp::now()) {
            Ok(marked) => {
                self.profile = Some(marked);
                self.note = None;
                self.journal
                    .note(&format!("из окна: при старте Windows загружается «{}»", set.name));
                None
            }
            Err(e) => Some(format!("файл профиля не записан: {e}")),
        }
    }

    /// profile.delete: убирает набор; если он был профилем запуска — при запуске ничего не ставится. Потолки, которые
    /// стоят, остаются.
    fn delete_profile(&mut self, name: &str) -> Option<String> {
        if let Some(other) = self.other() {
            return Some(other.menu_text().into());
        }
        let mut sets = match self.fresh_sets() {
            Ok(sets) => sets,
            Err(refusal) => return Some(refusal),
        };
        let removed = profile::remove_set(&mut sets, name);
        if removed && let Err(e) = self.store.write_sets(&sets) {
            return Some(format!("файл профилей не записан: {e}"));
        }
        self.sets = sets;
        let startup = self.at_startup(name);
        if startup {
            if let Err(e) = self.store.forget_profile() {
                return Some(format!("файл профиля не удалён: {e}"));
            }
            self.profile = None;
        }
        if !removed && !startup {
            return Some(format!("профиля «{name}» нет"));
        }
        self.journal.note(&format!("из окна удалён профиль «{}»", name.trim()));
        let current = self.current();
        self.log_change("delete", Some(name.trim()), &current, None, None);
        None
    }

    /// Перечитывает все потолки из процессора (сохранить берёт то, что стоит в процессоре, а не память программы:
    /// опрос автомодуля перечитывает только первые ядра блоков).
    fn reread(&mut self) -> Result<(), String> {
        let topology = &self.topology;
        let (limits, cpu) = parts(&mut self.limits, &mut self.device).ok_or("потолки множителей недоступны")?;
        match limits.refresh(cpu, topology) {
            None => Ok(()),
            Some(unread) => Err(format!("потолки не перечитаны из процессора: {unread}")),
        }
    }

    /// Поправки автомодуля становятся потолками владельца: то, что стоит, — новые принятые (сохранить).
    fn absorb(&mut self, why: &str) {
        if !self.auto.active() {
            return;
        }
        let names: Vec<String> = self
            .topology
            .blocks
            .iter()
            .zip(&self.auto.overlay)
            .filter_map(|(b, o)| o.map(|to| format!("{} ×{to}", b.name)))
            .collect();
        self.auto.overlay.fill(None);
        self.intended = Some(self.current());
        if let Some(controller) = &mut self.auto.controller {
            controller.forget();
        }
        if let Err(e) = self.mark() {
            self.journal.error(&format!("метка автомодуля: {e}"));
        }
        self.journal.note(&format!(
            "автомодуль: {why} — его поправки стали потолками владельца: {}",
            names.join(", ")
        ));
        let stamp = Stamp::now();
        self.log_line(
            &json!({ "time": stamp.roundtrip(), "at": stamp.0, "act": "absorb", "why": why, "blocks": names }),
        );
    }

    /// Профиль запуска с таким названием.
    fn at_startup(&self, name: &str) -> bool {
        self.profile.as_ref().is_some_and(|p| same_name(&p.name, name))
    }

    /// Каким набором являются потолки (LimitsStartup.Named с набором, открытым последним).
    pub(super) fn named(&self, ratios: &[i32]) -> Option<String> {
        let limits = self.limits.as_ref()?;
        profile::named(
            limits,
            ratios,
            self.profile.as_ref(),
            &self.listed_sets(),
            self.preferred.as_deref(),
        )
    }

    fn named_now(&self) -> Option<String> {
        let current = self.current();
        self.named(&current)
    }

    // ----- выход -----

    /// Выход программы («Выход» в меню значка, просьба C#-части закрыться, конец сеанса Windows): сначала окно — ему
    /// «bye», оно запоминает себя и закрывается; программа выходит, когда оно отключилось, или через `LEAVE_WITHIN`, если
    /// оно не закрылось (`App::quit`). Окна нет и не открывается — выход сразу. Окно, запущенное меньше `LAUNCH_GRACE`
    /// назад и ещё не подключившееся, ждётся тем же сроком: иначе оно открылось бы уже без программы.
    pub(super) fn leave(&mut self, shell: &mut Shell) {
        if self.serving.leaving.is_some() {
            return;
        }
        let awaiting = self.serving.session.is_none()
            && self.serving.server.is_some()
            && self.serving.launched_at.is_some_and(|t| t.elapsed() < LAUNCH_GRACE);
        self.serving.leaving = Some(Leaving {
            since: Instant::now(),
            awaiting,
            told: false,
        });
        shell.kill_timer(STATE);
        shell.kill_timer(GREET);
        if self.serving.session.is_some() || awaiting {
            shell.set_timer(LEAVE, LEAVE_WITHIN);
        }
        self.say_bye(shell);
        self.leave_if_alone(shell);
    }

    /// «bye» подключённому окну.
    fn say_bye(&mut self, shell: &Shell) {
        let Some(pid) = self.serving.session.as_ref().map(|s| s.pid) else {
            return;
        };
        if let Some(leaving) = self.serving.leaving.as_mut() {
            leaving.told = true;
        }
        self.journal
            .note(&format!("выход: окну (процесс {pid}) сказано закрыться"));
        self.send(shell, BYE);
    }

    /// Программа выходит и ждать больше некого (окно отключилось, запущенного нет): выход.
    fn leave_if_alone(&mut self, shell: &mut Shell) {
        let Some(leaving) = self.serving.leaving else {
            return;
        };
        if self.serving.left.is_some() || self.serving.session.is_some() || leaving.awaiting {
            return;
        }
        if leaving.told {
            self.journal.note(&format!(
                "выход: окно закрылось через {} мс",
                leaving.since.elapsed().as_millis()
            ));
        }
        self.finish_leave(shell);
    }

    /// Таймер выхода: окно не закрылось (или запущенное не подключилось) за `LEAVE_WITHIN` — выход без него.
    pub(super) fn leave_tick(&mut self, shell: &mut Shell) {
        shell.kill_timer(LEAVE);
        if self.serving.leaving.is_none() || self.serving.left.is_some() {
            return;
        }
        self.journal.note(&format!(
            "выход: окно не закрылось за {} с — выхожу без него",
            LEAVE_WITHIN.as_secs()
        ));
        self.finish_leave(shell);
    }

    fn finish_leave(&mut self, shell: &mut Shell) {
        shell.kill_timer(LEAVE);
        self.serving.left = Some(Instant::now());
        self.quit(shell);
    }

    // ----- окно на экране -----

    /// «Окно» в меню значка и щелчок по значку: открыть окно или вывести вперёд уже открытое.
    pub(super) fn open_window(&mut self, shell: &mut Shell) {
        if self.serving.server.is_none() {
            let why = self.serving.trouble.clone().unwrap_or_default();
            self.notify(shell, &format!("Окно не открыть: канал окна не создан — {why}"));
            return;
        }
        // Уже открыто: подключено к каналу — его процесс; иначе процесс окна из папки программы.
        let exe = self.config.root.join(WINDOW_EXE);
        let running = match &self.serving.session {
            Some(session) => Some(session.pid),
            None if self.config.launch => crate::launch::find_process(&exe),
            None => None,
        };
        if let Some(pid) = running {
            if self.config.launch {
                if !crate::launch::raise(pid) {
                    self.journal.note(&format!("окно (процесс {pid}) не вышло вперёд"));
                }
            } else {
                self.launched.push(format!("вперёд: процесс {pid}"));
            }
            return;
        }
        if self.serving.launched_at.is_some_and(|t| t.elapsed() < LAUNCH_GRACE) {
            self.notify(shell, "Окно CoreMap ещё открывается — подожди несколько секунд");
            return;
        }
        let args = format!("--pipe {}", self.serving.pipe);
        if !self.config.launch {
            self.launched.push(format!("{WINDOW_EXE} {args}"));
            self.serving.launched_at = Some(Instant::now());
            return;
        }
        if !exe.exists() {
            self.notify(shell, &format!("Окно не открыть: рядом с программой нет {WINDOW_EXE}"));
            return;
        }
        match crate::launch::unelevated(&exe, &args, &self.config.root) {
            Ok(()) => {
                self.serving.launched_at = Some(Instant::now());
                self.journal.note("окно запущено без прав администратора");
            }
            Err(e) => {
                self.journal.error(&format!("окно не запустилось: {e}"));
                self.notify(shell, &format!("Окно не открылось: {e}"));
            }
        }
    }
}

/// Сообщение «профили» (Feed.Profiles): профиль запуска, наборы и какой из них совпадает с тем, что стоит.
fn profiles_topic(app: &App, sets: &[Set]) -> String {
    let startup = match &app.profile {
        None => json!({
            "exists": false, "name": Value::Null, "state": Value::Null, "disabled": false,
            "describe": describe(app), "details": Value::Null, "note": app.note,
        }),
        Some(p) => json!({
            "exists": true, "name": p.name, "state": p.state, "disabled": p.disabled(),
            "describe": describe(app), "details": details(app), "note": app.note,
        }),
    };
    let listed: Vec<Value> = sets
        .iter()
        .map(|s| json!({ "name": s.name, "ratios": s.ratios }))
        .collect();
    json!({
        "t": "profiles",
        "startup": startup,
        "sets": listed,
        "standing": app.named_now(),
    })
    .to_string()
}

/// Профиль запуска одной строкой (Profile.Describe).
fn describe(app: &App) -> String {
    let note = app.note.as_deref().map(|n| format!(" · {n}")).unwrap_or_default();
    match &app.profile {
        None => format!("при запуске ничего не ставится{note}"),
        Some(p) => format!(
            "при запуске ставится «{}»{}{note}",
            p.name,
            match p.state.as_deref() {
                Some(profile::FAILED) => " — отключён после сбоя",
                Some(profile::TRIAL) => " (пробный)",
                _ => "",
            }
        ),
    }
}

/// Подробности профиля запуска (Profile.Details).
fn details(app: &App) -> Option<String> {
    let p = app.profile.as_ref()?;
    let mut text = format!(
        "Профиль запуска «{}», сохранён {}",
        p.name,
        p.saved.local(Pattern::DayTime)
    );
    match p.state.as_deref() {
        Some(profile::FAILED) => {
            let _ = write!(
                text,
                "\n• отключён и при запуске программы не ставится: {}",
                p.failure.as_deref().unwrap_or_default()
            ); // into a String
        }
        Some(profile::TRIAL) => {
            let _ = write!(
                text,
                "\n• пробный, потому что в нём есть потолки выше заводских: пережил перезапусков {}, часов {:.1}",
                p.boots, p.hours
            ); // into a String
        }
        _ => {}
    }
    if let Some(at) = &app.at_startup {
        let _ = write!(text, "\n• поставлен при запуске программы в {at}"); // into a String
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(value: Option<f64>, decimals: i32) -> String {
        let mut text = String::new();
        fix(&mut text, value, decimals);
        text
    }

    #[test]
    fn numbers_as_the_engine_writes_them() {
        // Out.Fix: rounded, trailing zeros dropped, NaN and missing as null.
        assert_eq!(fixed(Some(52.0), 1), "52");
        assert_eq!(fixed(Some(51.46), 1), "51.5");
        assert_eq!(fixed(Some(0.504), 2), "0.5");
        assert_eq!(fixed(Some(-0.01), 0), "0");
        assert_eq!(fixed(Some(f64::NAN), 1), "null");
        assert_eq!(fixed(None, 0), "null");
    }

    #[test]
    fn the_window_parses_a_state_of_this_shape() {
        // The fields and value kinds the Rust window requires (rust/window/src/wire.rs, State): an engine-shaped
        // state built by the same helpers must parse as JSON with those fields present.
        let mut s = String::from("{\"t\":\"state\"");
        key(&mut s, "pkg");
        fix(&mut s, Some(55.0), 0);
        key(&mut s, "cur");
        row(&mut s, &[Some(50.0), None], 0);
        key(&mut s, "thr");
        ints(&mut s, [Some(0), Some(0)].into_iter());
        key(&mut s, "auto");
        ints(&mut s, [None, Some(44)].into_iter());
        s.push('}');
        let parsed: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
        assert_eq!(parsed["cur"], json!([50, null]));
        assert_eq!(parsed["auto"], json!([null, 44]));
        assert_eq!(parsed["thr"], json!([0, 0]));
    }

    #[test]
    fn command_names_and_lengths() {
        let f: Map<String, Value> =
            serde_json::from_str(r#"{"name":"  жара ","block":2,"ratio":45.0}"#).unwrap_or_default();
        assert_eq!(name_of(&f), Some("  жара "));
        assert_eq!(int(&f, "block"), Some(2));
        assert_eq!(int(&f, "ratio"), None, "a fraction is not a ratio");
        let long: Map<String, Value> =
            serde_json::from_str(&format!(r#"{{"name":"{}"}}"#, "я".repeat(65))).unwrap_or_default();
        assert_eq!(name_of(&long), None);
    }
}
