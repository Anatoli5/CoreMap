// Скрытая проверка канала окна (часть src/check.rs; DECISIONS #32, шаг 2). Клиент в соседнем потоке открывает канал
// программы входа так же, как окно CoreMapWindow.exe (src/pipe.rs окна: именованный канал, четыре байта длины и JSON),
// знакомится и шлёт команды; поток программы тем временем крутит её настоящий цикл — канал, таймеры и события те же,
// что на машине владельца. Что окно на Rust читает из сообщений (rust/window/src/wire.rs), проверяется здесь по полям и
// их видам: окно этой проверкой не запускается.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use machine::named::Names;
use serde_json::{Value, json};

use crate::app::{App, Confirm, WINDOW_ID};
use crate::check::{Report, ceilings_now, hold, logged, pump, pump_until};
use crate::shell::{self, Shell};

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
pub fn run(r: &mut Report, app: &mut App, shell: &mut Shell, names: &Names, data: &Path) {
    let pipe = app.serving().pipe.clone();
    let cores = app.topology().cores.len();
    let blocks = app.topology().blocks.len();
    let reads_before = app.reads();
    pump(shell, app, Duration::from_millis(1100));
    r.check(
        app.reads() == reads_before,
        "без окна и автомодуля датчики не читаются (канал ждёт окно, таймера нет)",
    );

    // 12a. Знакомство: ответ, hello, limits, profiles, history, потом состояния.
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
            && limits["was"].as_array().map(Vec::len) == Some(cores),
        "limits: заводские, «было», допустимые значения по блокам",
    );
    r.check(
        client
            .last("profiles")
            .is_some_and(|p| p["startup"].is_object() && p["sets"].is_array())
            && client.last("history").is_some_and(|h| h["entries"].is_array()),
        "profiles и history пришли при знакомстве",
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
    let history_before = client
        .last("history")
        .and_then(|h| h["entries"].as_array().map(Vec::len))
        .unwrap_or(0);
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
    wait(shell, app, &mut client, Duration::from_secs(2), |c| {
        c.last("history").and_then(|h| h["entries"].as_array().map(Vec::len)) > Some(history_before)
    });
    let entry = client
        .last("history")
        .and_then(|h| h["entries"].as_array().and_then(|e| e.last()).cloned())
        .unwrap_or(Value::Null);
    r.check(
        entry["act"] == "limit"
            && entry["block"] == json!(block.name)
            && entry["after"][at] == json!(was - 1)
            && entry["before"][at] == json!(was),
        &format!("история: ручной потолок записан ({entry})"),
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

    // 12f. Профили по кругу: сохранить, открыть другой, откатить, принять заводские и свой, удалить — файлы такие же,
    //      как пишет C#-часть.
    // Название по алфавиту между наборами владельца (цифры, латиница): порядок в файле виден.
    let name = "b-окно";
    let saved_ratios = ceilings_now(app);
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
    let startup = app.store().read_profile().ok().flatten();
    r.check(
        startup.as_ref().is_some_and(|p| {
            p.name == name && p.ratios == saved_ratios && p.state.as_deref() == Some("trusted") && p.boots == 0
        }),
        "и он профиль запуска (profile.json: то, что стоит, «trusted», 0 перезапусков)",
    );
    wait(shell, app, &mut client, Duration::from_secs(2), |c| {
        c.last("limits")
            .is_some_and(|l| cores_of(&l["was"]) == saved_ratios.iter().map(|&v| i64::from(v)).collect::<Vec<_>>())
    });
    let profiles = client.last("profiles").cloned().unwrap_or(Value::Null);
    r.check(
        client
            .last("limits")
            .is_some_and(|l| cores_of(&l["was"]) == saved_ratios.iter().map(|&v| i64::from(v)).collect::<Vec<_>>())
            && profiles["standing"] == name
            && profiles["wasNamed"] == name
            && profiles["canAccept"] == false,
        &format!(
            "«было» — сохранённое; стоит «{}», было «{}», принимать нечего ({})",
            profiles["standing"].as_str().unwrap_or("?"),
            profiles["wasNamed"].as_str().unwrap_or("?"),
            profiles["canAccept"]
        ),
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
    let profiles = client.last("profiles").cloned().unwrap_or(Value::Null);
    r.check(
        ok && ceilings_now(app) == wanted && profiles["wasNamed"] == name && profiles["canAccept"] == true,
        &format!(
            "profile.open «{}»: стоит он, «было» прежнее, можно принять {why}",
            if opened.is_empty() {
                "заводские"
            } else {
                &opened
            }
        ),
    );
    let (_, ok, why) = order(shell, app, &mut client, "profile.revert", &json!({}));
    r.check(
        ok && ceilings_now(app) == saved_ratios,
        &format!("profile.revert: вернулось «было» {why}"),
    );
    let (_, ok_open, _) = order(shell, app, &mut client, "profile.open", &json!({ "name": "" }));
    let (_, ok_accept, why) = order(shell, app, &mut client, "profile.accept", &json!({}));
    r.check(
        ok_open
            && ok_accept
            && app.store().read_profile().ok().flatten().is_none()
            && !data.join("profile.json").exists(),
        &format!("заводские приняты — профиля запуска больше нет {why}"),
    );
    let (_, ok_open, _) = order(shell, app, &mut client, "profile.open", &json!({ "name": name }));
    let (_, ok_accept, why) = order(shell, app, &mut client, "profile.accept", &json!({}));
    r.check(
        ok_open
            && ok_accept
            && app
                .store()
                .read_profile()
                .ok()
                .flatten()
                .is_some_and(|p| p.name == name),
        &format!("свой набор принят — снова профиль запуска {why}"),
    );
    let (_, ok, why) = order(shell, app, &mut client, "profile.delete", &json!({ "name": name }));
    r.check(
        ok && !app
            .store()
            .read_sets()
            .unwrap_or_default()
            .iter()
            .any(|s| s.name == name)
            && app.store().read_profile().ok().flatten().is_none()
            && ceilings_now(app) == saved_ratios,
        &format!("profile.delete: набора нет, профиля запуска нет, потолки остались {why}"),
    );
    let (_, ok, why) = order(shell, app, &mut client, "profile.open", &json!({ "name": name }));
    r.check(!ok && why.contains("нет"), &format!("удалённый не открывается: {why}"));
    let lines: Vec<Value> = fs::read_to_string(data.join("profile-log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let acts: Vec<&str> = lines.iter().filter_map(|l| l["act"].as_str()).collect();
    let complete = lines.iter().all(|l| {
        l["time"].is_string()
            && l["at"].is_i64()
            && array_of(&l["before"], cores, Value::is_i64)
            && array_of(&l["after"], cores, Value::is_i64)
    });
    let wanted_acts = ["limit", "save", "open", "revert", "accept", "delete"];
    r.check(
        complete && wanted_acts.iter().all(|a| acts.contains(a)),
        &format!(
            "profile-log.jsonl: {} строк, все с временем и потолками до и после ({acts:?})",
            lines.len()
        ),
    );
    let shown = client
        .last("history")
        .and_then(|h| h["entries"].as_array().map(Vec::len))
        .unwrap_or(0);
    r.check(
        shown == lines.len().min(100),
        &format!("окну ушла история: {shown} строк"),
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
    let from = shell::usage();
    pump(shell, app, Duration::from_secs(3));
    let to = shell::usage();
    r.note(&format!(
        "после окна: память выделенная {:.1} МБ (с сообщениями клиента проверки в том же процессе), процессор в покое {} мс за 3 с",
        f64::from(u32::try_from(to.private / 1024).unwrap_or(u32::MAX)) / 1024.0,
        to.cpu.saturating_sub(from.cpu) / 10_000
    ));
    r.check(
        to.cpu.saturating_sub(from.cpu) < 200_000,
        "окно ушло — в покое процессор не тратится (меньше 20 мс за 3 с)",
    );
}
