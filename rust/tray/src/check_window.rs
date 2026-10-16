// Скрытая проверка канала окна (часть src/check.rs; DECISIONS #32, шаг 2). Клиент в соседнем потоке открывает канал
// программы входа так же, как окно CoreMapWindow.exe (src/pipe.rs окна: именованный канал, четыре байта длины и JSON),
// знакомится и шлёт команды; поток программы тем временем крутит её настоящий цикл — канал, таймеры и события те же,
// что на машине владельца. Что окно на Rust читает из сообщений (rust/window/src/wire.rs), проверяется здесь по полям и
// их видам: окно этой проверкой не запускается.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use machine::named::Names;
use serde_json::{Value, json};
use windows_sys::Win32::UI::WindowsAndMessaging::{WM_CLOSE, WM_ENDSESSION};

use crate::app::{AUTO_ON, App, AutoTiming, Choice, Config, Confirm, EXIT_ID, LEAVE_WITHIN, WINDOW_EXE, WINDOW_ID};
use crate::check::{QuietLog, Report, ceilings_now, hold, logged, pump, pump_until};
use crate::shell::{self, Event, Loop, Shell};

/// Клиент канала, как окно: свой поток читает сообщения и отдаёт их сюда; команды уходят через него же.
struct Client {
    to: mpsc::Sender<Option<String>>,
    from: mpsc::Receiver<Value>,
    got: Vec<Value>,
    next: i64,
}

fn frame(text: &str) -> Vec<u8> {
    let mut bytes = u32::try_from(text.len()).unwrap_or(0).to_le_bytes().to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

impl Client {
    /// Подключается к каналу `pipe` (ждёт до 2 с) и говорит `first` первым сообщением (None — молчит).
    fn connect(pipe: &str, first: Option<&str>) -> Self {
        let (to, outgoing) = mpsc::channel::<Option<String>>();
        let (incoming, from) = mpsc::channel::<Value>();
        let path = format!(r"\\.\pipe\{pipe}");
        let first = first.map(str::to_string);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut file = loop {
                match fs::OpenOptions::new().read(true).write(true).open(&path) {
                    Ok(file) => break file,
                    Err(e) if Instant::now() >= deadline => {
                        let _ = incoming.send(json!({ "t": "closed", "why": e.to_string() })); // проверка увидит
                        return;
                    }
                    Err(_) => thread::sleep(Duration::from_millis(50)),
                }
            };
            if let Some(first) = first
                && file.write_all(&frame(&first)).is_err()
            {
                let _ = incoming.send(json!({ "t": "closed", "why": "hello не записан" }));
                return;
            }
            loop {
                loop {
                    match outgoing.try_recv() {
                        Ok(Some(text)) => {
                            if file.write_all(&frame(&text)).is_err() {
                                let _ = incoming.send(json!({ "t": "closed", "why": "команда не записана" }));
                                return;
                            }
                        }
                        // Ушёл: канал закрывается вместе с файлом.
                        Ok(None) | Err(mpsc::TryRecvError::Disconnected) => return,
                        Err(mpsc::TryRecvError::Empty) => break,
                    }
                }
                let mut head = [0u8; 4];
                if file.read_exact(&mut head).is_err() {
                    let _ = incoming.send(json!({ "t": "closed", "why": "канал закрыт" }));
                    return;
                }
                let mut body = vec![0u8; u32::from_le_bytes(head) as usize];
                if file.read_exact(&mut body).is_err() {
                    let _ = incoming.send(json!({ "t": "closed", "why": "сообщение оборвано" }));
                    return;
                }
                let value = serde_json::from_slice::<Value>(&body)
                    .unwrap_or_else(|e| json!({ "t": "unparsed", "why": e.to_string() }));
                if incoming.send(value).is_err() {
                    return;
                }
            }
        });
        Self {
            to,
            from,
            got: Vec::new(),
            next: 1,
        }
    }

    fn hello(pipe: &str) -> Self {
        Self::connect(pipe, Some(r#"{"id":0,"c":"hello","proto":1}"#))
    }

    /// Команда; номер для ответа.
    fn ask(&mut self, command: &str, args: &Value) -> i64 {
        let id = self.next;
        self.next += 1;
        let mut message = args.as_object().cloned().unwrap_or_default();
        message.insert("id".into(), json!(id));
        message.insert("c".into(), json!(command));
        let _ = self.to.send(Some(Value::Object(message).to_string())); // поток ушёл — ответа не будет, проверка увидит
        id
    }

    /// Сырое сообщение (нарушение протокола).
    fn raw(&self, text: &str) {
        let _ = self.to.send(Some(text.to_string())); // as above
    }

    fn close(&self) {
        let _ = self.to.send(None); // as above
    }

    fn collect(&mut self) {
        while let Ok(value) = self.from.try_recv() {
            self.got.push(value);
        }
    }

    fn of(&self, topic: &str) -> Vec<&Value> {
        self.got.iter().filter(|v| v["t"] == topic).collect()
    }

    fn last(&self, topic: &str) -> Option<&Value> {
        self.got.iter().rev().find(|v| v["t"] == topic)
    }

    fn reply(&self, id: i64) -> Option<&Value> {
        self.got.iter().find(|v| v["t"] == "reply" && v["re"] == id)
    }

    /// Состояние, пришедшее после ответа на команду `id`.
    fn state_after(&self, id: i64) -> Option<&Value> {
        let at = self.got.iter().position(|v| v["t"] == "reply" && v["re"] == id)?;
        self.got[at..].iter().find(|v| v["t"] == "state")
    }

    fn closed(&self) -> bool {
        self.got.iter().any(|v| v["t"] == "closed")
    }
}

/// Крутит цикл программы, пока у клиента не выполнится условие (не дольше `limit`). true — выполнилось.
fn wait(
    shell: &mut Shell,
    app: &mut App,
    client: &mut Client,
    limit: Duration,
    done: impl Fn(&Client) -> bool,
) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        client.collect();
        if done(client) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        pump(shell, app, Duration::from_millis(30));
    }
}

/// Команда и ответ на неё (с состоянием следом); ответ — (ok, why).
fn order(shell: &mut Shell, app: &mut App, client: &mut Client, command: &str, args: &Value) -> (i64, bool, String) {
    let id = client.ask(command, args);
    wait(shell, app, client, Duration::from_secs(5), |c| {
        c.state_after(id).is_some()
    });
    let reply = client.reply(id);
    (
        id,
        reply.is_some_and(|r| r["ok"] == true),
        reply.and_then(|r| r["why"].as_str()).unwrap_or_default().to_string(),
    )
}

fn array_of(value: &Value, len: usize, item: impl Fn(&Value) -> bool) -> bool {
    value.as_array().is_some_and(|a| a.len() == len && a.iter().all(item))
}

fn number_or_null(v: &Value) -> bool {
    v.is_number() || v.is_null()
}

/// Читает ли окно на Rust это состояние: поля и их виды — как в rust/window/src/wire.rs (State, Fans). Без поля
/// окно не разберёт сообщение и покажет «сообщение движка не разобралось».
fn window_reads_state(state: &Value, cores: usize) -> Result<(), String> {
    let need = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
    need(number_or_null(&state["pkg"]), "pkg")?;
    need(state.get("watts").is_none_or(number_or_null), "watts")?;
    need(state["busy"].is_boolean(), "busy")?;
    for row in ["cur", "max", "ratio", "avg"] {
        need(array_of(&state[row], cores, number_or_null), row)?;
    }
    for flags in ["thr", "load"] {
        need(array_of(&state[flags], cores, |v| v == 0 || v == 1), flags)?;
    }
    need(
        state["limit"].is_null() || array_of(&state["limit"], cores, Value::is_i64),
        "limit",
    )?;
    let rpm = &state["fans"]["rpm"];
    need(
        state["fans"].is_object() && (rpm.is_null() || rpm.as_array().is_some_and(|a| a.iter().all(number_or_null))),
        "fans.rpm",
    )?;
    need(state["trip"].is_null() || state["trip"].is_string(), "trip")?;
    need(
        state["sensorError"].is_null() || state["sensorError"].is_string(),
        "sensorError",
    )
}

/// Читает ли окно знакомство (`Hello`, `Layout`, `Tile`, `CoreInfo`, `BlockInfo` в wire.rs).
fn window_reads_hello(hello: &Value) -> Result<(), String> {
    let need = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
    need(
        hello["cpu"].is_string() && hello["simulated"].is_boolean(),
        "cpu, simulated",
    )?;
    let all = |key: &str, test: &dyn Fn(&Value) -> bool| hello[key].as_array().is_some_and(|a| a.iter().all(test));
    need(all("cores", &|c| c["lp"].is_i64() && c["kind"].is_string()), "cores")?;
    need(
        all("blocks", &|b| {
            b["index"].is_u64()
                && b["members"].is_array()
                && b["first"].is_u64()
                && b["lp"].is_i64()
                && b["kind"].is_string()
                && b["name"].is_string()
        }),
        "blocks",
    )?;
    let layout = &hello["layout"];
    need(
        (layout["note"].is_null() || layout["note"].is_string())
            && layout["confirmed"].is_boolean()
            && layout["cols"].is_i64()
            && layout["rows"].is_i64()
            && layout["tiles"].as_array().is_some_and(|t| {
                t.iter().all(|t| {
                    t["block"].is_u64()
                        && t["col"].as_u64().is_some_and(|c| u16::try_from(c).is_ok())
                        && t["row"].is_u64()
                        && t["places"].is_array()
                })
            }),
        "layout",
    )
}

/// 12m (см. run). Горячий блок — с самым высоким потолком: корпус имитации близко к пределу 75 °C, условие журнала
/// выполнено без доли работы по Windows. Регулятор на настоящих сроках за несколько секунд ничего не решает.
fn logging_with_window(r: &mut Report, app: &mut App, shell: &mut Shell, client: &mut Client) {
    let cores = u64::try_from(app.topology().cores.len()).unwrap_or(0);
    let now = ceilings_now(app);
    let Some(hot) = app
        .topology()
        .blocks
        .iter()
        .max_by_key(|b| now.get(b.first).copied().unwrap_or(0))
        .cloned()
    else {
        r.check(false, "журнал срезов с окном: блоков нет");
        return;
    };
    let was = app.retime(shell, AutoTiming::REAL);
    if let Some(sim) = app.sim() {
        for &m in &hot.members {
            sim.load[m] = 1.0;
        }
    }
    app.command(shell, AUTO_ON);
    wait(shell, app, client, Duration::from_secs(5), |_| false);
    let started = app.logging().on;
    client.collect();
    let (before, reads, fed, from) = (
        app.logging(),
        app.reads(),
        app.window_fed().unwrap_or(0),
        client.got.len(),
    );
    wait(shell, app, client, Duration::from_secs(4), |_| false);
    let (after, reads_after, fed_after) = (app.logging(), app.reads(), app.window_fed().unwrap_or(0));
    let fresh: Vec<Value> = client.got[from..]
        .iter()
        .filter(|v| v["t"] == "state")
        .cloned()
        .collect();
    let cost = crate::check::cost_line(app);
    app.command(shell, AUTO_ON); // выключить
    if let Some(sim) = app.sim() {
        for &m in &hot.members {
            sim.load[m] = 0.0;
        }
    }
    app.retime(shell, was);
    r.note(&format!(
        "цена опроса срезов при открытом окне (имитация): {}",
        cost.unwrap_or_default()
    ));
    let blocks = u64::try_from(app.topology().blocks.len()).unwrap_or(0);
    let (polled, windowed, read) = (
        after.polls - before.polls,
        after.window_polls - before.window_polls,
        reads_after - reads,
    );
    // Ревизия 15.10, п. 5: все ядра — только в опросе окна, каждом втором; между ними — первые ядра блоков (журналу и
    // автомодулю другие не нужны). Чтений ровно столько, ни одного сверх: окно и автомодуль сами не читают.
    r.check(
        started
            && after.on
            && after.starts == before.starts
            && polled > 0
            && (2 * windowed).abs_diff(polled) <= 1
            && read == windowed * cores + (polled - windowed) * blocks,
        &format!(
            "журнал срезов при открытом окне: {polled} опросов, из них {windowed} опросов окна; {read} чтений ядер — все {cores} в опросе окна, {blocks} первых в остальных, ни одного сверх"
        ),
    );
    // Ревизия 15.10, п. 1: средние под нагрузкой — по опросам окна, каждый опрос окна — один раз, и только он.
    let fed = fed_after - fed;
    r.check(
        fed == windowed,
        &format!("средние под нагрузкой: {fed} опросов окна дошло до средних из {windowed} — каждое чтение один раз"),
    );
    let hot_first = hot.first;
    r.check(
        (7..=10).contains(&fresh.len())
            && fresh.iter().all(|s| {
                s["sensorError"].is_null() && s["cur"].as_array().is_some_and(|a| a.iter().all(Value::is_number))
            })
            && fresh.iter().any(|s| s["load"][hot_first] == 1),
        &format!(
            "состояний окну за 4 с: {} (дважды в секунду), все с температурами ядер, горячий блок {} — нагружен",
            fresh.len(),
            hot.name
        ),
    );
}

/// 12b. Настройки автомодуля по каналу (DECISIONS #36): тема settings при знакомстве (выбор и промежутки), auto.set
/// меняет выбор — data\tray.json, тема, autoLimit в состоянии (линия отсечки окна); негодное — отказ с причиной без
/// записи. Включается на одно состояние и выключается: регулятору нужны минуты, ничего не успеет сдвинуться.
fn auto_settings(r: &mut Report, app: &mut App, shell: &mut Shell, client: &mut Client, data: &Path, state: &Value) {
    let settings = client.last("settings").cloned().unwrap_or(Value::Null);
    let auto = &settings["auto"];
    r.check(
        auto["on"] == false
            && auto["limit"].is_i64()
            && auto["stepUp"].is_boolean()
            && auto["limitRange"] == json!([60, 95])
            && auto.get("depth").is_none()
            && auto.get("depthRange").is_none(),
        &format!("settings при знакомстве (глубины нет): {auto}"),
    );
    r.check(
        settings["log"] == json!({ "on": true }),
        &format!(
            "settings при знакомстве: журнал срезов — своим полем, включён ({})",
            settings["log"]
        ),
    );
    let file = data.join("tray.json");
    let before = fs::read_to_string(&file).unwrap_or_default();
    let mut refused = Vec::new();
    for bad in [
        json!({ "limit": 96 }),
        json!({ "limit": 85.5 }),
        json!({ "depth": 4 }),
        json!({ "on": true, "limit": 59 }),
        json!({ "limt": 80 }),
        json!({}),
    ] {
        let (_, ok, why) = order(shell, app, client, "auto.set", &bad);
        refused.push((bad, ok, why));
    }
    let untouched = fs::read_to_string(&file).unwrap_or_default() == before;
    r.check(
        refused.iter().all(|(_, ok, why)| !ok && !why.is_empty()) && untouched && app.auto_choice().limit != 59,
        &format!(
            "auto.set с негодным — отказ с причиной, tray.json не тронут: {}",
            refused
                .iter()
                .map(|(bad, _, why)| format!("{bad} — {why}"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );
    let off = state["autoLimit"].is_null();
    let (id, ok, why) = order(shell, app, client, "auto.set", &json!({ "on": true, "limit": 77 }));
    let on = client.state_after(id).cloned().unwrap_or(Value::Null);
    let topic = client.last("settings").cloned().unwrap_or(Value::Null);
    let written = fs::read_to_string(&file).unwrap_or_default();
    r.check(
        ok && on["autoLimit"] == json!(77)
            && topic["auto"]["on"] == true
            && topic["auto"]["limit"] == json!(77)
            && written.contains("\"auto\":true")
            && written.contains("\"limit\":77"),
        &format!(
            "auto.set {{on, 77}}: принято {why}, тема settings {}, autoLimit в состоянии {}, tray.json {written}",
            topic["auto"], on["autoLimit"]
        ),
    );
    let (id, ok, _) = order(shell, app, client, "auto.set", &json!({ "on": false }));
    let off_again = client.state_after(id).is_some_and(|s| s["autoLimit"].is_null());
    r.check(
        off && ok && off_again,
        "отсечка автомодуля в состоянии: выключен — null, включён на 77 °C — 77, выключен снова — null",
    );
    // То же значение ещё раз — ничего не меняется: ни строки журнала, ни записи файла.
    let lines = logged(app, "автомодуль (окно)");
    let stamp = fs::metadata(&file).and_then(|m| m.modified()).ok();
    let (_, ok, _) = order(shell, app, client, "auto.set", &json!({ "on": false }));
    r.check(
        ok && logged(app, "автомодуль (окно)") == lines && fs::metadata(&file).and_then(|m| m.modified()).ok() == stamp,
        "auto.set без изменения — принято, но ни записи tray.json, ни строки журнала",
    );
}

/// 12b'. Журнал срезов по каналу — свой выключатель и своя команда log.set (владелец 16.10: два независимых
/// выключателя): меняет только себя, тема settings и tray.json следом; негодное — отказ без записи.
fn log_settings(r: &mut Report, app: &mut App, shell: &mut Shell, client: &mut Client, data: &Path) {
    let file = data.join("tray.json");
    // Журнал срезов — свой выключатель и своя команда log.set (владелец 16.10: два независимых выключателя): меняет
    // только себя; негодное — отказ без записи.
    let auto = app.auto_choice();
    let (id, ok, why) = order(shell, app, client, "log.set", &json!({ "on": false }));
    let topic = client.last("settings").cloned().unwrap_or(Value::Null);
    let written = fs::read_to_string(&file).unwrap_or_default();
    let after = app.auto_choice();
    r.check(
        ok && client.state_after(id).is_some()
            && topic["log"] == json!({ "on": false })
            && !after.log
            && after.same_auto(auto)
            && written.contains("\"log\":false"),
        &format!(
            "log.set {{on: false}}: принято {why}, тема settings {}, автомодуль не тронут, tray.json {written}",
            topic["log"]
        ),
    );
    let mut refused = Vec::new();
    for bad in [json!({ "on": "нет" }), json!({ "limit": 80 }), json!({})] {
        let (_, ok, why) = order(shell, app, client, "log.set", &bad);
        refused.push((bad, ok, why));
    }
    r.check(
        refused.iter().all(|(_, ok, why)| !ok && !why.is_empty()) && !app.auto_choice().log,
        &format!(
            "log.set с негодным — отказ с причиной: {}",
            refused
                .iter()
                .map(|(bad, _, why)| format!("{bad} — {why}"))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );
    let (_, ok, _) = order(shell, app, client, "log.set", &json!({ "on": true }));
    r.check(
        ok && app.auto_choice().log && app.auto_choice().same_auto(auto) && logged(app, "журнал срезов (окно)") >= 2,
        "log.set {on: true}: журнал снова включён, в tray.log — по строке на смену",
    );
}

/// Что настоящее окно делает в своей проверке настроек (rust/window/src/check.rs, `settings`): включает автомодуль,
/// тянет ползунок отсечки до 88 °C, включает подтягивание, выключает автомодуль — четыре жеста (глубины нет, #38).
const WINDOW_LIMIT: i32 = 88;
const WINDOW_GESTURES: usize = 4;
/// Щелчков по «запись» в той же проверке окна: выключить и включить журнал срезов.
const WINDOW_LOG_GESTURES: usize = 2;
/// Название профиля, который окно в той же проверке сохраняет, применяет, перезаписывает и удаляет щелчками.
const WINDOW_PROFILE: &str = "проверка окна";

/// Отчёт окна проверки — рядом с его картинкой.
fn report_of(picture: &Path) -> PathBuf {
    PathBuf::from(format!("{}.txt", picture.display()))
}

/// Запускает CoreMapWindow.exe с ключами `pass` против канала программы входа. None — не запустился (сказано в отчёте).
fn start_window(r: &mut Report, app: &App, exe: &Path, picture: &Path, pass: &[&str]) -> Option<Child> {
    let _ = fs::remove_file(report_of(picture)); // отчёта прошлой проверки нет — тем лучше; не удалился — его выдаст код выхода
    let child = Command::new(exe)
        .arg("--pipe")
        .arg(&app.serving().pipe)
        .arg("--snapshot")
        .arg(picture)
        .args(pass)
        .spawn();
    match child {
        Ok(child) => Some(child),
        Err(e) => {
            r.check(false, &format!("{WINDOW_EXE} {} не запустился: {e}", pass.join(" ")));
            None
        }
    }
}

/// Крутит цикл программы, пока окно не выйдет, не дольше `limit`. Код выхода окна; None — не уложилось (окно убито).
fn wait_exit(shell: &mut Shell, app: &mut App, child: &mut Child, limit: Duration) -> Option<i32> {
    let deadline = Instant::now() + limit;
    loop {
        pump(shell, app, Duration::from_millis(50));
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) if Instant::now() < deadline => {}
            Ok(None) | Err(_) => {
                let _ = child.kill(); // не уложилось — код None, проверка это скажет
                let _ = child.wait(); // убитый процесс забран; не вышло — его код и так None
                return None;
            }
        }
    }
}

/// Окно проверки: CoreMapWindow.exe с ключами `pass` против канала программы входа; её цикл крутится, пока окно не
/// выйдет (не дольше 90 с). Отчёт окна — в этот отчёт строками; пройдена — код 0 и отчёт «ok».
fn window_pass(r: &mut Report, app: &mut App, shell: &mut Shell, exe: &Path, picture: &Path, pass: &[&str]) {
    let started = Instant::now();
    let Some(mut child) = start_window(r, app, exe, picture, pass) else {
        return;
    };
    let code = wait_exit(shell, app, &mut child, Duration::from_secs(90));
    window_report(r, picture, pass, code, started);
}

/// Отчёт окна проверки — в этот отчёт строками; пройдена — код 0 и отчёт «ok».
fn window_report(r: &mut Report, picture: &Path, pass: &[&str], code: Option<i32>, started: Instant) {
    let text = fs::read_to_string(report_of(picture)).unwrap_or_default();
    r.note(&format!(
        "--- окно {WINDOW_EXE} {} против программы входа, картинка {} ---",
        pass.join(" "),
        picture.display()
    ));
    for line in text.lines() {
        r.note(line);
    }
    r.check(
        code == Some(0) && text.starts_with("ok"),
        &format!(
            "окно {} прошло свою проверку против программы входа за {:.1} с; код {code:?}",
            pass.join(" "),
            started.elapsed().as_secs_f64()
        ),
    );
}

/// 12n. Настоящее окно CoreMapWindow.exe против программы входа (DECISIONS #36, #38), два запуска с одной папкой окна.
/// Первый (--settings) щёлкает выключатели и тянет ползунок настоящими событиями мыши и смотрит, что вернулось в окно;
/// здесь — что дошло до программы входа: выбор, data\tray.json (только настройки автомодуля), по строке журнала на
/// жест, а не на каждый шаг ползунка. Затем щелчками — профили: выбор строки ничего не ставит, «сохранить как»,
/// «применить», «сохранить», «удалить»; здесь — что стоит, profiles.json, профиль запуска и история. Затем он выбирает
/// блоки, подсветку, размер и закрывается — это окно пишет в свой файл, программа входа о нём не знает. Второй
/// (--reopened) — новое окно, новое знакомство: открылось ли оно таким, каким его закрыли. Выбор автомодуля после шага
/// возвращается прежним: дальше шаги на нём.
fn real_window(r: &mut Report, app: &mut App, shell: &mut Shell, root: &Path, out: &Path, data: &Path) {
    let exe = root.join(WINDOW_EXE);
    if !exe.exists() {
        r.check(
            false,
            &format!("нет {WINDOW_EXE} рядом с программой входа (его кладёт tools\\rust.ps1)"),
        );
        return;
    }
    // Папка окна в проверке — своя, внутри временной папки программы входа: окно пишет туда придуманные срезы для
    // графика и свой файл; срезы программы входа (шаг 13 после этого) им не мешают. Картинки — logon-*.png, не
    // window*.png: проверка движка C# (Tray.cs, EngineCheck) стирает window*.png перед своим окном.
    let folder = data.join("window-check");
    let folder_arg = folder.display().to_string();
    let before = app.auto_choice();
    let lines = logged(app, "автомодуль (окно)");
    let log_lines = logged(app, "журнал срезов (окно)");
    let ceilings = ceilings_now(app);
    let history = profile_log(data).len();
    let logon = app.store().read_profile().ok().flatten().map(|p| p.name);
    let (marked, unmarked) = (
        logged(app, "при старте Windows загружается"),
        logged(app, "больше не загружается при старте Windows"),
    );
    window_pass(
        r,
        app,
        shell,
        &exe,
        &out.with_file_name("logon-auto.png"),
        &["--settings", "--data", &folder_arg],
    );
    let after = app.auto_choice();
    let written = fs::read_to_string(data.join("tray.json")).unwrap_or_default();
    let want = Choice {
        on: false,
        limit: WINDOW_LIMIT,
        step_up: true,
        log: true,
    };
    r.check(
        after == want && written == want.file_text(),
        &format!("выбор из окна дошёл до программы входа: {after:?}; tray.json {written}"),
    );
    let gestures = logged(app, "автомодуль (окно)") - lines;
    r.check(
        gestures == WINDOW_GESTURES,
        &format!(
            "строк журнала «автомодуль (окно)»: {gestures} — по одной на жест ({WINDOW_GESTURES}), не на каждый шаг ползунка"
        ),
    );
    // «запись» в шапке статистики — журнал срезов, свой выключатель (владелец 16.10): окно выключает и включает его
    // щелчками, команда log.set на каждый; выключатель автомодуля от этого не меняется (он проверен выше).
    let log_gestures = logged(app, "журнал срезов (окно)") - log_lines;
    r.check(
        log_gestures == WINDOW_LOG_GESTURES && app.auto_choice().log,
        &format!(
            "строк журнала «журнал срезов (окно)»: {log_gestures} — по одной на щелчок по «запись» ({WINDOW_LOG_GESTURES}); журнал снова включён"
        ),
    );
    // Профили щелчками окна: что окно оставило программе входа.
    let acts: Vec<(String, Value)> = profile_log(data)
        .into_iter()
        .skip(history)
        .filter(|l| l["name"] == WINDOW_PROFILE)
        .map(|l| (l["act"].as_str().unwrap_or("").to_string(), l))
        .collect();
    let names: Vec<&str> = acts.iter().map(|(a, _)| a.as_str()).collect();
    let left = app.store().read_sets().unwrap_or_default();
    let logon_after = app.store().read_profile().ok().flatten().map(|p| p.name);
    r.check(
        names == ["save", "open", "save", "delete"]
            && !left.iter().any(|s| s.name == WINDOW_PROFILE)
            && logon_after == logon
            && ceilings_now(app) == ceilings,
        &format!(
            "профили из окна дошли до программы входа: история {names:?} (сохранить как, применить, сохранить, удалить), набора больше нет, профиль запуска как до окна ({logon_after:?}), потолки как до окна"
        ),
    );
    let marks = logged(app, "при старте Windows загружается") - marked;
    let unmarks = logged(app, "больше не загружается при старте Windows") - unmarked;
    r.check(
        marks >= 1 && unmarks >= 1,
        &format!("отметка «загружать при старте Windows» щелчками окна: поставлена {marks} раз, снята {unmarks} раз"),
    );
    // Второй запуск — и после неудачи первого: отчёт покажет, что из запомненного дошло.
    window_pass(
        r,
        app,
        shell,
        &exe,
        &out.with_file_name("logon-reopen.png"),
        &["--reopened", "--data", &folder_arg],
    );
    // Прежний выбор: следующие шаги проверки рассчитаны на него.
    let mut back = serde_json::Map::new();
    back.insert("limit".into(), json!(before.limit));
    back.insert("stepUp".into(), json!(before.step_up));
    back.insert("on".into(), json!(before.on));
    let refused = app.set_auto(shell, &back);
    r.check(
        refused.is_none(),
        &format!("прежний выбор автомодуля возвращён {refused:?}"),
    );
}

/// Строки data\profile-log.jsonl по порядку (испорченные пропускаются).
fn profile_log(data: &Path) -> Vec<Value> {
    fs::read_to_string(data.join("profile-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn cores_of(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0)).collect())
        .unwrap_or_default()
}

#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
pub fn run(r: &mut Report, app: &mut App, shell: &mut Shell, names: &Names, places: (&Path, &Path, &Path)) {
    let (root, out, data) = places;
    let pipe = app.serving().pipe.clone();
    let cores = app.topology().cores.len();
    let blocks = app.topology().blocks.len();
    let reads_before = app.reads();
    pump(shell, app, Duration::from_millis(1100));
    r.check(
        app.reads() == reads_before,
        "без окна датчики не читаются: автомодуль выключен, процессор не занят — опрос раз в 2 с их не трогает (канал ждёт окно)",
    );

    // 12a. Знакомство: ответ, hello, limits, profiles, потом состояния.
    let mut client = Client::hello(&pipe);
    let greeted = wait(shell, app, &mut client, Duration::from_secs(5), |c| {
        !c.of("state").is_empty() || c.closed()
    });
    let reply = client.reply(0).cloned();
    r.check(
        greeted && reply.as_ref().is_some_and(|v| v["ok"] == true),
        "окно назвало себя — принято",
    );
    let hello = client.last("hello").cloned().unwrap_or(Value::Null);
    r.check(
        hello["blocks"].as_array().map(Vec::len) == Some(blocks)
            && hello["cores"].as_array().map(Vec::len) == Some(cores)
            && hello["layout"]["tiles"].as_array().map(Vec::len) == Some(blocks)
            && hello["engine"] == "logon",
        &format!(
            "hello: {} блоков, {} ядер, схема «{}», плиток {}",
            blocks,
            cores,
            hello["layout"]["name"].as_str().unwrap_or("?"),
            hello["layout"]["tiles"].as_array().map_or(0, Vec::len)
        ),
    );
    match window_reads_hello(&hello) {
        Ok(()) => r.check(true, "hello — в том виде, что читает окно на Rust (wire.rs)"),
        Err(field) => r.check(false, &format!("hello: окно не прочтёт поле {field}")),
    }
    let limits = client.last("limits").cloned().unwrap_or(Value::Null);
    r.check(
        limits["choices"].as_array().map(Vec::len) == Some(blocks)
            && limits["fused"].as_array().map(Vec::len) == Some(cores)
            && limits.get("was").is_none(),
        "limits: заводские и допустимые значения по блокам; «было» больше нет (DECISIONS #38)",
    );
    let profiles = client.last("profiles").cloned().unwrap_or(Value::Null);
    r.check(
        profiles["startup"].is_object()
            && profiles["sets"].is_array()
            && profiles.get("wasNamed").is_none()
            && profiles.get("canAccept").is_none()
            && client.last("history").is_none(),
        "profiles пришли при знакомстве (без «было» и «принять»); истории окну больше не шлётся",
    );

    // 12b. Состояние дважды в секунду; в нём всё, что читает окно, и новое: доля работы, автомодуль, потолки владельца.
    let from = client.of("state").len();
    let started = Instant::now();
    wait(shell, app, &mut client, Duration::from_millis(2100), |_| false);
    let count = client.of("state").len() - from;
    let seconds = started.elapsed().as_secs_f64();
    r.check(
        (3..=6).contains(&count),
        &format!("состояние: {count} за {seconds:.1} с (дважды в секунду)"),
    );
    let state = client.last("state").cloned().unwrap_or(Value::Null);
    match window_reads_state(&state, cores) {
        Ok(()) => r.check(true, "состояние — в том виде, что читает окно на Rust (wire.rs)"),
        Err(field) => r.check(false, &format!("состояние: окно не прочтёт поле {field}")),
    }
    r.check(
        array_of(&state["busyShare"], cores, number_or_null)
            && array_of(&state["auto"], blocks, |v| v.is_null() || v.is_i64())
            && array_of(&state["intended"], cores, Value::is_i64)
            && state["cur"].as_array().is_some_and(|a| a.iter().all(Value::is_number)),
        "в состоянии: температуры всех ядер, доля работы, потолок автомодуля по блокам, потолки владельца",
    );
    r.check(
        state["fans"]["rpm"] == json!([3200, 3000]),
        &format!("обороты вентиляторов в состоянии ({})", state["fans"]["rpm"]),
    );
    auto_settings(r, app, shell, &mut client, data, &state);
    log_settings(r, app, shell, &mut client, data);

    // 12c. Средняя под нагрузкой: нагруженный блок получает среднюю после 2 с прогрева, ненагруженный — нет;
    //      «сбросить максимумы» обнуляет и наибольшие, и средние.
    let loaded = app.topology().blocks[0].clone();
    let idle = app.topology().blocks[1].first;
    if let Some(sim) = app.sim() {
        for &m in &loaded.members {
            sim.load[m] = 1.0;
        }
    }
    let first = loaded.first;
    let averaged = wait(shell, app, &mut client, Duration::from_secs(5), |c| {
        c.last("state").is_some_and(|s| s["avg"][first].is_number())
    });
    let state = client.last("state").cloned().unwrap_or(Value::Null);
    let ceiling = ceilings_now(app).get(first).copied().unwrap_or(0);
    r.check(
        averaged
            && state["avgRatio"][first] == json!(ceiling)
            && state["avg"][idle].is_null()
            && state["load"][first] == 1
            && state["load"][idle] == 0,
        &format!(
            "средняя под нагрузкой: блок {} — {} °C при ×{}, ненагруженный — {}; признак нагрузки {} / {}",
            loaded.name,
            state["avg"][first],
            state["avgRatio"][first],
            state["avg"][idle],
            state["load"][first],
            state["load"][idle]
        ),
    );
    let (id, ok, _) = order(shell, app, &mut client, "max.reset", &json!({}));
    let after = client.state_after(id).cloned().unwrap_or(Value::Null);
    r.check(
        ok && after["avg"].as_array().is_some_and(|a| a.iter().all(Value::is_null))
            && after["avgRatio"]
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_null))
            && after["max"] == after["cur"],
        "max.reset: средние сброшены, наибольшие — с нынешних",
    );
    if let Some(sim) = app.sim() {
        for &m in &loaded.members {
            sim.load[m] = 0.0;
        }
    }

    // 12m. Журнал срезов на настоящих сроках, пока окно открыто: опрос срезов читает все ядра (окну они нужны) — и это
    //      единственное чтение: чтений ядер ровно «опросов × ядер», окно раз в полсекунды и автомодуль раз в 2 с сами
    //      ничего не читают; состояния окну идут дважды в секунду с показаниями этого опроса.
    logging_with_window(r, app, shell, &mut client);

    // 12d. Нагрузка и замеры — отказ одной фразой; неизвестная команда и негодные аргументы — с причиной.
    let (_, ok, why) = order(shell, app, &mut client, "load.toggle", &json!({ "block": 0 }));
    r.check(
        !ok && why == "в этой версии окна нет нагрузки и замеров",
        &format!("load.toggle — отказ: {why}"),
    );
    let (_, ok, why) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 99, "ratio": 40 }),
    );
    r.check(
        !ok && why.contains("негодные аргументы"),
        &format!("limit.set блоку 99 — {why}"),
    );

    // 12e. Потолок вручную: ставится; вне допустимого — отказ без вопроса; выше заводского — только после «Да».
    let block = app.topology().blocks[4].clone();
    let at = block.first;
    let was = ceilings_now(app).get(at).copied().unwrap_or(0);
    let fused = cores_of(&limits["fused"]).get(at).copied().unwrap_or(0);
    let history_before = profile_log(data).len();
    let writes = app.sim().map_or(0, |s| s.writes);
    let (id, ok, why) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": was - 1 }),
    );
    let state = client.state_after(id).cloned().unwrap_or(Value::Null);
    r.check(
        ok && ceilings_now(app).get(at) == Some(&(was - 1))
            && state["limit"][at] == json!(was - 1)
            && state["intended"][at] == json!(was - 1)
            && app.sim().map_or(0, |s| s.writes) > writes,
        &format!(
            "limit.set блоку {} ×{was} → ×{}: поставлен, в состоянии виден {why}",
            block.name,
            was - 1
        ),
    );
    let log = profile_log(data);
    let entry = log
        .last()
        .filter(|_| log.len() > history_before)
        .cloned()
        .unwrap_or(Value::Null);
    r.check(
        entry["act"] == "limit"
            && entry["block"] == json!(block.name)
            && entry["after"][at] == json!(was - 1)
            && entry["before"][at] == json!(was),
        &format!("profile-log.jsonl: ручной потолок записан ({entry})"),
    );
    let asked = app.serving().asked.len();
    let (_, ok, why) = order(shell, app, &mut client, "limit.set", &json!({ "block": 4, "ratio": 5 }));
    r.check(
        !ok && why.contains("вне допустимого") && app.serving().asked.len() == asked,
        &format!("×5 — отказ без вопроса владельцу: {why}"),
    );
    let (_, ok, why) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": fused + 1 }),
    );
    r.check(
        !ok && why.contains("не подтверждён")
            && app.serving().asked.len() == asked + 1
            && ceilings_now(app).get(at) == Some(&(was - 1)),
        &format!(
            "×{} (выше заводского) без «Да» — отказ: {why}; вопрос «{}»",
            fused + 1,
            app.serving().asked.last().map_or("", String::as_str)
        ),
    );
    app.serving_mut().confirm = Confirm::Answer(true);
    let (_, ok, _) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": fused + 1 }),
    );
    r.check(
        ok && ceilings_now(app).get(at) == Some(&i32::try_from(fused + 1).unwrap_or(0)),
        "после «Да» — поставлен",
    );
    app.serving_mut().confirm = Confirm::Answer(false);
    let (_, ok, _) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": was }),
    );
    r.check(ok && ceilings_now(app).get(at) == Some(&was), "и возвращён");

    // 12f. Профили по кругу (DECISIONS #38): сохранить, отметить «при старте Windows», применить другой, применить свой,
    //      сохранить в отмеченный, перенести и снять отметку, применить заводские, удалить. Применение и сохранение
    //      профиль запуска не меняют; «принять» и «откатить» больше нет; набор выше заводского применяется только после
    //      «Да»; файлы такие же, как пишет C#-часть.
    // Название по алфавиту между наборами владельца (цифры, латиница): порядок в файле виден.
    let name = "b-окно";
    let saved_ratios = ceilings_now(app);
    let logon_before = fs::read_to_string(data.join("profile.json")).ok();
    let (_, ok, why) = order(shell, app, &mut client, "profile.save", &json!({ "name": name }));
    let sets = app.store().read_sets().unwrap_or_default();
    let text = fs::read_to_string(data.join("profiles.json")).unwrap_or_default();
    let ordered = sets
        .windows(2)
        .all(|w| w[0].name.to_lowercase() <= w[1].name.to_lowercase());
    r.check(
        ok && sets.iter().any(|s| s.name == name && s.ratios == saved_ratios)
            && text == app.store().sets_text(&sets)
            && text.starts_with("{\"cpu\": ")
            && ordered,
        &format!("profile.save «{name}»: набор в profiles.json, порядок по алфавиту, вид файла как у C# {why}"),
    );
    r.check(
        fs::read_to_string(data.join("profile.json")).ok() == logon_before
            && client.last("profiles").is_some_and(|p| p["standing"] == name),
        "сохранение профиль запуска не меняет (profile.json тот же); в профилях совпадает с текущим — он",
    );
    let (_, ok, why) = order(
        shell,
        app,
        &mut client,
        "profile.startup",
        &json!({ "name": name, "on": true }),
    );
    let startup = app.store().read_profile().ok().flatten();
    r.check(
        ok && startup.as_ref().is_some_and(|p| {
            p.name == name && p.ratios == saved_ratios && p.state.as_deref() == Some("trusted") && p.boots == 0
        }) && client.last("profiles").is_some_and(|p| p["startup"]["name"] == name),
        &format!("profile.startup «{name}» вкл: он профиль запуска (profile.json: его потолки, «trusted», 0 перезапусков) {why}"),
    );
    let (_, ok, why) = order(
        shell,
        app,
        &mut client,
        "profile.startup",
        &json!({ "name": "", "on": true }),
    );
    r.check(
        !ok && !why.is_empty()
            && app
                .store()
                .read_profile()
                .ok()
                .flatten()
                .is_some_and(|p| p.name == name),
        &format!("«заводские» при старте не загружаются — отказ: {why}"),
    );
    let other_name = sets
        .iter()
        .find(|s| s.name != name && s.ratios != saved_ratios)
        .map(|s| s.name.clone());
    let opened = other_name.clone().unwrap_or_default();
    let wanted: Vec<i32> = match &other_name {
        Some(n) => sets
            .iter()
            .find(|s| &s.name == n)
            .map(|s| s.ratios.clone())
            .unwrap_or_default(),
        None => cores_of(&limits["fused"])
            .iter()
            .map(|&v| i32::try_from(v).unwrap_or(0))
            .collect(),
    };
    let (_, ok, why) = order(shell, app, &mut client, "profile.open", &json!({ "name": opened }));
    let logon = app.store().read_profile().ok().flatten().map(|p| p.name);
    r.check(
        ok && ceilings_now(app) == wanted
            && logon.as_deref() == Some(name)
            && client.last("profiles").is_some_and(|p| p["standing"] == json!(opened)),
        &format!(
            "profile.open «{}» (применить): стоит он, профиль запуска прежний ({logon:?}) {why}",
            if opened.is_empty() {
                "заводские"
            } else {
                &opened
            }
        ),
    );
    for gone in ["profile.accept", "profile.revert"] {
        let (_, ok, why) = order(shell, app, &mut client, gone, &json!({}));
        r.check(
            !ok && why.contains("неизвестная команда") && ceilings_now(app) == wanted,
            &format!("{gone} больше нет — {why}"),
        );
    }
    let (_, ok, why) = order(shell, app, &mut client, "profile.open", &json!({ "name": name }));
    r.check(
        ok && ceilings_now(app) == saved_ratios,
        &format!("profile.open «{name}»: снова стоит {why}"),
    );
    // Сохранение в отмеченный профиль: при входе будут его новые потолки.
    let lowered = was - 1;
    let (_, ok_set, _) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": lowered }),
    );
    let (_, ok, why) = order(shell, app, &mut client, "profile.save", &json!({ "name": name }));
    let marked = app.store().read_profile().ok().flatten();
    r.check(
        ok_set
            && ok
            && marked
                .as_ref()
                .is_some_and(|p| p.name == name && p.ratios.get(at) == Some(&lowered)),
        &format!(
            "сохранение в отмеченный «{name}»: в profile.json его новые потолки (×{lowered} у блока {}) {why}",
            block.name
        ),
    );
    let (_, ok_back, _) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": was }),
    );
    let (_, ok_saved, _) = order(shell, app, &mut client, "profile.save", &json!({ "name": name }));
    r.check(ok_back && ok_saved && ceilings_now(app) == saved_ratios, "и обратно");
    // Отметка переходит на другой профиль (отмечен только один) и снимается.
    if let Some(other) = &other_name {
        let (_, ok, why) = order(
            shell,
            app,
            &mut client,
            "profile.startup",
            &json!({ "name": other, "on": true }),
        );
        let moved = app.store().read_profile().ok().flatten();
        r.check(
            ok && moved.as_ref().is_some_and(|p| &p.name == other && p.ratios == wanted)
                && ceilings_now(app) == saved_ratios,
            &format!("отметка перешла на «{other}» (его потолки при входе), в процессоре ничего не изменилось {why}"),
        );
        let (_, ok, why) = order(
            shell,
            app,
            &mut client,
            "profile.startup",
            &json!({ "name": name, "on": false }),
        );
        r.check(
            !ok && app
                .store()
                .read_profile()
                .ok()
                .flatten()
                .is_some_and(|p| &p.name == other),
            &format!("снять отметку с неотмеченного — отказ: {why}"),
        );
        let (_, ok, why) = order(
            shell,
            app,
            &mut client,
            "profile.startup",
            &json!({ "name": other, "on": false }),
        );
        r.check(
            ok && app.store().read_profile().ok().flatten().is_none() && !data.join("profile.json").exists(),
            &format!("отметка снята — при входе ничего не ставится {why}"),
        );
    } else {
        r.note("других сохранённых наборов нет — перенос отметки не проверен");
    }
    let (_, ok, why) = order(shell, app, &mut client, "profile.open", &json!({ "name": "" }));
    r.check(
        ok && cores_of(&limits["fused"]) == ceilings_now(app).iter().map(|&v| i64::from(v)).collect::<Vec<_>>(),
        &format!("заводские применены — стоят заводские {why}"),
    );
    // Набор с потолком выше заводского: «применить» спрашивает владельца; «нет» — ничего не ставится.
    let (_, ok_back, _) = order(shell, app, &mut client, "profile.open", &json!({ "name": name }));
    app.serving_mut().confirm = Confirm::Answer(true);
    let (_, ok_up, _) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": fused + 1 }),
    );
    app.serving_mut().confirm = Confirm::Answer(false);
    let above = "b-выше";
    let (_, ok_saved, _) = order(shell, app, &mut client, "profile.save", &json!({ "name": above }));
    let (_, ok_open, _) = order(shell, app, &mut client, "profile.open", &json!({ "name": name }));
    let asked = app.serving().asked.len();
    let (_, ok, why) = order(shell, app, &mut client, "profile.open", &json!({ "name": above }));
    r.check(
        ok_back
            && ok_up
            && ok_saved
            && ok_open
            && !ok
            && why.contains("не подтверждён")
            && app.serving().asked.len() == asked + 1
            && ceilings_now(app) == saved_ratios,
        &format!(
            "применить набор выше заводского без «Да» — отказ: {why}; вопрос «{}»",
            app.serving().asked.last().map_or("", String::as_str)
        ),
    );
    // Отмеченный профиль удаляется — при входе ничего не ставится.
    let (_, ok_mark, _) = order(
        shell,
        app,
        &mut client,
        "profile.startup",
        &json!({ "name": name, "on": true }),
    );
    let (_, ok_gone, _) = order(shell, app, &mut client, "profile.delete", &json!({ "name": above }));
    let (_, ok, why) = order(shell, app, &mut client, "profile.delete", &json!({ "name": name }));
    r.check(
        ok_mark
            && ok_gone
            && ok
            && !app
                .store()
                .read_sets()
                .unwrap_or_default()
                .iter()
                .any(|s| s.name == name || s.name == above)
            && app.store().read_profile().ok().flatten().is_none()
            && ceilings_now(app) == saved_ratios,
        &format!("profile.delete отмеченного: наборов нет, профиля запуска нет, потолки остались {why}"),
    );
    let (_, ok, why) = order(shell, app, &mut client, "profile.open", &json!({ "name": name }));
    r.check(!ok && why.contains("нет"), &format!("удалённый не применяется: {why}"));
    let lines = profile_log(data);
    let acts: Vec<&str> = lines.iter().filter_map(|l| l["act"].as_str()).collect();
    let complete = lines.iter().all(|l| {
        l["time"].is_string()
            && l["at"].is_i64()
            && array_of(&l["before"], cores, Value::is_i64)
            && array_of(&l["after"], cores, Value::is_i64)
    });
    let wanted_acts = ["limit", "save", "open", "delete"];
    r.check(
        complete
            && wanted_acts.iter().all(|a| acts.contains(a))
            && !acts.contains(&"accept")
            && !acts.contains(&"revert"),
        &format!(
            "profile-log.jsonl: {} строк, все с временем и потолками до и после ({acts:?})",
            lines.len()
        ),
    );

    // 12g. Сообщение владельцу уходит и окну.
    let said = app.said.len();
    app.notify(shell, "проверка сообщения окну");
    let told = wait(shell, app, &mut client, Duration::from_secs(2), |c| {
        c.of("notice").iter().any(|n| n["text"] == "Проверка сообщения окну")
    });
    r.check(
        told && app.said.len() > said,
        "сообщение владельцу пришло окну (notice)",
    );

    // 12h. Открыто прежнее окно: потолки и профили — отказ с причиной, в процессор ничего, датчики не читаются, но
    //      состояние идёт и говорит почему.
    let window = hold(&names.old_window);
    shell.post(shell::WINDOW_OPENED, 4242, 0);
    pump(shell, app, Duration::from_millis(100));
    let writes = app.sim().map_or(0, |s| s.writes);
    let asks = app.sim().map_or(0, |s| s.reads);
    let reads = app.reads();
    let (id, ok, why) = order(
        shell,
        app,
        &mut client,
        "limit.set",
        &json!({ "block": 4, "ratio": was - 1 }),
    );
    let (_, ok_save, why_save) = order(shell, app, &mut client, "profile.save", &json!({ "name": "нельзя" }));
    wait(shell, app, &mut client, Duration::from_millis(1100), |_| false);
    let state = client.last("state").cloned().unwrap_or(Value::Null);
    r.check(
        !ok && why.contains("открыто прежнее окно CoreMap")
            && !ok_save
            && why_save.contains("открыто прежнее окно CoreMap"),
        &format!("при прежнем окне: limit.set — {why}; profile.save — {why_save}"),
    );
    r.check(
        app.sim().map_or(0, |s| s.writes) == writes && app.sim().map_or(0, |s| s.reads) == asks && app.reads() == reads,
        "при прежнем окне процессор не тронут: ни записи, ни чтения потолков, ни датчиков",
    );
    r.check(
        client.state_after(id).is_some()
            && state["sensorError"]
                .as_str()
                .is_some_and(|t| t.contains("открыто прежнее окно CoreMap"))
            && state["limit"].is_null()
            && state["cur"].as_array().is_some_and(|a| a.iter().all(Value::is_null)),
        &format!(
            "состояние идёт без показаний и говорит почему: {}",
            state["sensorError"]
        ),
    );
    let taken_back = logged(app, "потолки снова мои");
    let _ = window.send(());
    pump_until(shell, app, Duration::from_secs(5), |a| {
        logged(a, "потолки снова мои") > taken_back
    });
    let back = wait(shell, app, &mut client, Duration::from_secs(2), |c| {
        c.last("state")
            .is_some_and(|s| s["sensorError"].is_null() && s["cur"][0].is_number())
    });
    r.check(back, "прежнее окно закрылось — показания снова идут");

    // 12i. Окно уже открыто: «Окно» выводит его вперёд, второго не запускает.
    let launched = app.launched.len();
    app.command(shell, WINDOW_ID);
    let pid = std::process::id();
    r.check(
        app.launched.len() == launched + 1 && app.launched.last() == Some(&format!("вперёд: процесс {pid}")),
        &format!(
            "«Окно», когда окно подключено, — вперёд, второе не запускается ({:?})",
            app.launched.last()
        ),
    );

    // 12j. Окно ушло: состояние и чтение датчиков прекращаются.
    client.close();
    let gone = logged(app, "окно отключилось");
    pump_until(shell, app, Duration::from_secs(3), |a| {
        logged(a, "окно отключилось") > gone
    });
    let (states, reads) = (app.serving().states, app.reads());
    pump(shell, app, Duration::from_millis(1500));
    r.check(
        logged(app, "окно отключилось") > gone && app.serving().states == states && app.reads() == reads,
        "окно ушло — состояний нет, датчики не читаются",
    );

    // 12k. Канал снова свободен: второе окно подключается; молчащее отключается через 2 с; не JSON — разрыв.
    let mut again = Client::hello(&pipe);
    let ok = wait(shell, app, &mut again, Duration::from_secs(4), |c| {
        c.reply(0).is_some_and(|v| v["ok"] == true) && !c.of("state").is_empty()
    });
    again.raw("это не JSON");
    let dropped = wait(shell, app, &mut again, Duration::from_secs(4), Client::closed);
    r.check(
        ok && dropped,
        "второе подключение принято; сообщение не JSON — разрыв (нарушение протокола)",
    );
    let silent = Client::connect(&pipe, None);
    let mut silent = silent;
    let cut = wait(shell, app, &mut silent, Duration::from_secs(5), Client::closed);
    r.check(
        cut && logged(app, "окно молчит") > 0,
        "подключилось и молчит — отключено через 2 с",
    );
    r.check(
        logged(app, "окно: опрос") > 0,
        "цена опроса окна записана в tray.log (раз в минуту, как у автомодуля)",
    );
    r.note(&format!(
        "канал: подключений {}, состояний отправлено {}",
        app.serving().sessions,
        app.serving().states
    ));

    // 12l. Окна нет: таймеров канала не осталось — процессор в покое не тратится.
    drop(client);
    drop(again);
    drop(silent);
    pump(shell, app, Duration::from_millis(300));
    // Покой — на настоящих сроках опроса раз в 2 с: у проверки он раз в 50 мс (шаг 10 в check.rs).
    let was = app.retime(shell, AutoTiming::REAL);
    let from = shell::usage();
    pump(shell, app, Duration::from_secs(3));
    let to = shell::usage();
    app.retime(shell, was);
    r.note(&format!(
        "после окна: память выделенная {:.1} МБ (с сообщениями клиента проверки в том же процессе), процессор в покое {} мс за 3 с",
        f64::from(u32::try_from(to.private / 1024).unwrap_or(u32::MAX)) / 1024.0,
        to.cpu.saturating_sub(from.cpu) / 10_000
    ));
    r.check(
        to.cpu.saturating_sub(from.cpu) < 200_000,
        "окно ушло — в покое процессор не тратится (меньше 20 мс за 3 с)",
    );

    // 12n. Настоящее окно: настройки автомодуля щелчками и ползунками.
    real_window(r, app, shell, root, out, data);
}

/// Размер окна в его проходе --bye (rust/window/src/check.rs, `LEFT_SIZE`): в файле окна его нет, записать его туда
/// может только «bye».
const WINDOW_LEFT_SIZE: [u32; 2] = [1400, 860];

/// Вторая программа входа в этом же процессе — для выхода, который кончает её цикл: своя папка данных и свой канал,
/// сроки и имена меток — как у проверяемой. Только когда проверяемая уже вышла: события окон одного потока идут через
/// одну очередь (shell.rs), и таймеры живой первой достались бы второй.
fn another(r: &mut Report, first: &App, data: &Path, suffix: &str) -> Option<(App, Shell)> {
    let config = Config {
        data: data.join(suffix),
        pipe: format!("{}.{suffix}", first.config().pipe),
        ..first.config().clone()
    };
    if let Err(e) = fs::create_dir_all(&config.data) {
        r.check(false, &format!("папка второй программы {}: {e}", config.data.display()));
        return None;
    }
    let class = config.pipe.clone(); // имя класса окна — то же, что у канала: в процессе оно одно
    let made = App::new(config, Box::new(QuietLog)).and_then(|app| Shell::new(&class, false).map(|shell| (app, shell)));
    match made {
        Ok((mut app, mut shell)) => {
            app.start(&mut shell);
            Some((app, shell))
        }
        Err(e) => {
            r.check(false, &format!("вторая программа не запустилась: {e}"));
            None
        }
    }
}

/// Размер из файла окна (data\window.json): [ширина, высота]; None — файла или поля нет. Окно пишет его числами
/// с дробной частью («1500.0», rust/window/src/saved.rs): читать как целые нельзя — так первая версия шага читала None.
fn window_size(file: &Path) -> Option<Vec<f64>> {
    let value: Value = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
    value["size"].as_array()?.iter().map(Value::as_f64).collect()
}

/// Шаг 14 — выход программы с открытым окном (владелец 15.10: «если мы закрываем движок, а интерфейс остается, то
/// есть схуяли… чтобы трей сначала закрывал окно… а потом закрывался сам»). Последний шаг проверки: программа выходит.
//  14a. Настоящее окно (--lost); канал пропадает без «bye», как при падении программы: окно остаётся и одной строкой
//       говорит, что CoreMapTray не отвечает. Канал снова есть.
//  14b. Настоящее окно (--bye) и «Выход» в меню: окно получает «bye», записывает себя в файл окна и закрывается в
//       пределах `LEAVE_WITHIN`; программа выходит после него.
//  14c. Вторая программа, окна нет: конец сеанса Windows — выход сразу, без ожидания.
//  14d. Третья программа, окно (клиент проверки) «bye» получает, но не закрывается: щелчок по значку окно заново не
//       открывает; программа выходит через `LEAVE_WITHIN` без него.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
pub fn leaving(r: &mut Report, app: &mut App, shell: &mut Shell, places: (&Path, &Path, &Path)) {
    let (root, out, data) = places;
    let exe = root.join(WINDOW_EXE);
    if !exe.exists() {
        r.check(false, &format!("нет {WINDOW_EXE} рядом с программой входа"));
        return;
    }
    // Папка окна — та же, что у шага 12n: в его файле окна — то, что записало закрытие прохода --settings.
    let folder = data.join("window-check");
    let folder_arg = folder.display().to_string();
    let file = folder.join("window.json");

    // 14a. Канал пропал без «bye».
    let pass = ["--lost", "--data", folder_arg.as_str()];
    let picture = out.with_file_name("logon-lost.png");
    let started = Instant::now();
    if let Some(mut child) = start_window(r, app, &exe, &picture, &pass) {
        pump_until(shell, app, Duration::from_secs(15), |a| a.serving().attached());
        let attached = app.serving().attached();
        // Канал рвётся не раньше чем через 2 с после подключения: тогда окно пробует снова через секунду, а не через
        // пять (pipe.rs окна) — проверка только короче, итог тот же.
        pump(shell, app, Duration::from_millis(2200));
        app.cut_pipe(shell);
        let code = wait_exit(shell, app, &mut child, Duration::from_secs(30));
        window_report(r, &picture, &pass, code, started);
        r.check(attached, "окно подключилось до того, как канал пропал");
    }
    app.open_server();
    r.check(
        !app.serving().handles().is_empty(),
        "канал окна снова есть (как после нового запуска программы)",
    );

    // 14b. «Выход» с открытым окном.
    let pass = ["--bye", "--data", folder_arg.as_str()];
    let picture = out.with_file_name("logon-bye.png");
    let started = Instant::now();
    let Some(mut child) = start_window(r, app, &exe, &picture, &pass) else {
        return;
    };
    pump_until(shell, app, Duration::from_secs(15), |a| a.serving().attached());
    let attached = app.serving().attached();
    let before = window_size(&file);
    let asked = Instant::now();
    app.command(shell, EXIT_ID);
    let code = wait_exit(shell, app, &mut child, LEAVE_WITHIN + Duration::from_secs(5));
    let closed_in = asked.elapsed();
    pump_until(shell, app, Duration::from_secs(1), |a| a.serving().left.is_some());
    let after = window_size(&file);
    window_report(r, &picture, &pass, code, started);
    r.check(
        attached && code == Some(0) && closed_in <= LEAVE_WITHIN,
        &format!(
            "«Выход» с открытым окном: окно закрылось через {} мс (срок {} с)",
            closed_in.as_millis(),
            LEAVE_WITHIN.as_secs()
        ),
    );
    let left = app.serving().left.map(|t| t.duration_since(asked).as_millis());
    r.check(
        left.is_some() && logged(app, "выход: окно закрылось через") == 1 && logged(app, "окно не закрылось") == 0,
        &format!("CoreMapTray вышел после окна, через {left:?} мс после «Выход»"),
    );
    let want = WINDOW_LEFT_SIZE.map(f64::from).to_vec();
    r.check(
        before.as_ref().is_some_and(|b| *b != want) && after.as_ref() == Some(&want),
        &format!("файл окна после «bye»: размер {before:?} → {after:?}"),
    );
    // Что осталось от вышедшей программы в общей очереди событий — ей же (её таймеров больше нет).
    pump(shell, app, Duration::from_millis(50));

    // 14c. Окна нет, конец сеанса Windows: выход сразу.
    if let Some((mut second, mut shell2)) = another(r, app, data, "s2") {
        let asked = Instant::now();
        shell2.post(WM_ENDSESSION, 1, 0);
        pump_until(&mut shell2, &mut second, Duration::from_secs(2), |a| {
            a.serving().left.is_some()
        });
        let left = second.serving().left.map(|t| t.duration_since(asked));
        r.check(
            left.is_some_and(|t| t < Duration::from_millis(500))
                && logged(&second, "Windows завершает сеанс") == 1
                && logged(&second, "сказано закрыться") == 0,
            &format!("окна нет, конец сеанса Windows — выход сразу ({left:?})"),
        );
        pump(&mut shell2, &mut second, Duration::from_millis(50));
    }

    // 14d. Окно «bye» получило и не закрывается.
    if let Some((mut third, mut shell3)) = another(r, app, data, "s3") {
        let pipe = third.serving().pipe.clone();
        let mut client = Client::hello(&pipe);
        let greeted = wait(&mut shell3, &mut third, &mut client, Duration::from_secs(5), |c| {
            !c.of("state").is_empty() || c.closed()
        });
        let asked = Instant::now();
        shell3.post(WM_CLOSE, 0, 0);
        wait(&mut shell3, &mut third, &mut client, Duration::from_secs(2), |c| {
            !c.of("bye").is_empty()
        });
        // Щелчок по значку — так, как его разбирает цикл программы. (Меню здесь не зовётся: без защиты оно показалось бы
        // на экране и ждало бы выбора; защита у них одна — `event` в app.rs.)
        let launched = third.launched.len();
        third.on(&mut shell3, Loop::Event(Event::Open));
        let reopened = third.launched.len() - launched;
        pump_until(&mut shell3, &mut third, LEAVE_WITHIN + Duration::from_secs(2), |a| {
            a.serving().left.is_some()
        });
        let left = third.serving().left.map(|t| t.duration_since(asked));
        client.collect();
        let bye = client.got.iter().position(|v| v["t"] == "bye");
        let quiet = bye.is_some_and(|at| client.got[at + 1..].iter().all(|v| v["t"] == "closed"));
        r.check(
            greeted && bye.is_some() && quiet && reopened == 0,
            &format!(
                "окну сказано «bye», после него ему ничего не шлётся; щелчок по значку, пока программа ждёт окно, его заново не открывает и вперёд не выводит ({reopened})"
            ),
        );
        r.check(
            // Срок таймера Windows отсчитывается тиками около 16 мс: допуск на один.
            left.is_some_and(|t| {
                t + Duration::from_millis(20) >= LEAVE_WITHIN && t < LEAVE_WITHIN + Duration::from_secs(1)
            }) && logged(&third, "окно не закрылось за") == 1,
            &format!("окно не закрылось — программа вышла без него через {left:?}"),
        );
        client.close();
    }
}
