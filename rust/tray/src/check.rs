// Скрытая проверка программы входа: CoreMapTray.exe --simulate --snapshot <отчёт> [--saved]. Значка на экране нет,
// прав администратора не нужно, процессор — имитация, данные — во временной папке, все метки Windows — с приставкой
// «.Check.<номер процесса>»: настоящие части CoreMap проверку не видят и она их не видит. Окно, меню, сообщения,
// таймеры и метки — настоящие, Windows. Проходит каждую строку правила «кто когда пишет потолки» (DECISIONS #31) и
// канал окна (DECISIONS #32, шаг 2): клиент в соседнем потоке подключается к нему, как окно, — check_window.rs.
// Первая строка отчёта — «ok» или «FAILED»; tools\check.ps1 смотрит на неё.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use machine::clock::{Pattern, Stamp};
use machine::events::{Event, EventLog};
use machine::named::{self, Names};
use machine::thermal::Thermal;

use crate::app::{AUTO_ON, AUTO_WINDOW_ID, App, AutoTiming, Config, Confirm, EXIT_ID, FACTORY_ID, LOG_ID, WINDOW_ID};
use crate::shell::{self, Item, Menu, Shell};

/// Журнал без сбоев: проверка не зависит от истории машины.
#[derive(Debug)]
pub struct QuietLog;

impl EventLog for QuietLog {
    fn first(&self, _since: Stamp, _condition: &str) -> Result<Option<Event>, String> {
        Ok(None)
    }
}

pub struct Report {
    lines: Vec<String>,
    failed: bool,
}

impl Report {
    pub fn check(&mut self, ok: bool, what: &str) {
        self.lines
            .push(format!("{} {what}", if ok { "ok    " } else { "FAILED" }));
        self.failed |= !ok;
    }

    pub fn note(&mut self, text: &str) {
        self.lines.push(format!("       {text}"));
    }
}

pub fn pump(shell: &mut Shell, app: &mut App, for_: Duration) {
    shell::run(shell, app, Some(Instant::now() + for_));
}

/// Прокручивает цикл, пока не выполнится условие, но не дольше `limit`. Фиксированная пауза под нагрузкой полной
/// проверки (дюжина процессов рядом) оказалась мала: конец «окна» доходил позже 0,9 с (outputs\s5u, 14.10).
pub fn pump_until(shell: &mut Shell, app: &mut App, limit: Duration, done: impl Fn(&mut App) -> bool) {
    let deadline = Instant::now() + limit;
    while !done(app) && Instant::now() < deadline {
        pump(shell, app, Duration::from_millis(50));
    }
}

/// Сколько раз строка встречается в tray.log: шаг ждёт свою запись, а не только снятие метки — иначе действие,
/// отложенное на паузу (возврат права, сверка после пробуждения), срабатывает уже в следующем шаге и путает его.
pub fn logged(app: &App, needle: &str) -> usize {
    fs::read_to_string(app.journal().tray_path())
        .unwrap_or_default()
        .matches(needle)
        .count()
}

pub fn ceilings_now(app: &mut App) -> Vec<i32> {
    app.sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default()
}

/// Держит мьютекс в своём потоке, пока не скажут; потом поток кончается, не отпустив его, — как закрытая C#-часть.
pub fn hold(name: &str) -> mpsc::Sender<()> {
    let (stop, wait) = mpsc::channel::<()>();
    let (ready, started) = mpsc::channel::<()>();
    let name = name.to_string();
    thread::spawn(move || {
        let held = named::create(&name, true);
        // Сказать, что метка поднята; если проверка уже не ждёт — неважно.
        let _ = ready.send(());
        // Ждать сигнала (или конца проверки).
        let _ = wait.recv();
        drop(held); // дескриптор закрыт, мьютекс не отпущен: владелец «кончился»
    });
    // Ждём, пока метка появится; поток не ответил за секунду — проверка покажет это сама.
    let _ = started.recv_timeout(Duration::from_secs(1));
    stop
}

/// Держит мьютекс, не владея им (так C#-часть создаёт метку живого Core движка — Engine.cs), пока не скажут.
fn exist(name: &str) -> mpsc::Sender<()> {
    let (stop, wait) = mpsc::channel::<()>();
    let held = named::create(name, false);
    thread::spawn(move || {
        let _ = wait.recv(); // ждать сигнала или конца проверки
        drop(held);
    });
    stop
}

/// Сообщение Windows «состояние экрана изменилось» (0 — выключен, 1 — включён), как его шлёт система.
#[repr(C)]
struct DisplayState {
    guid: windows_sys::core::GUID,
    length: u32,
    state: u32,
}

/// Передаёт своему окну смену состояния экрана — сразу, как Windows (сообщение с указателем в очередь не ставится:
/// `PostMessage` его отвергает; так первая версия этого шага молча ничего не проверяла), потом прокручивает цикл.
fn display(shell: &mut Shell, app: &mut App, state: u32, for_: Duration) {
    let message = DisplayState {
        guid: windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE,
        length: 4,
        state,
    };
    // Указатель уходит в lparam как число: адрес открыт для обратного перевода в указатель (expose_provenance).
    let pointer = (&raw const message).expose_provenance().cast_signed();
    // PBT_POWERSETTINGCHANGE; процедура окна разбирает его до возврата из send.
    shell.send(
        windows_sys::Win32::UI::WindowsAndMessaging::WM_POWERBROADCAST,
        0x8013,
        pointer,
    );
    std::hint::black_box(&message);
    pump(shell, app, for_);
}

/// Свободна ли аренда для другого потока (своя аренда в своём потоке берётся всегда: мьютекс повторно входимый).
fn lease_free(name: &str) -> bool {
    let name = name.to_string();
    thread::spawn(move || {
        let Ok((mutex, _)) = named::create(&name, false) else {
            return false;
        };
        let free = mutex.acquire(0) == named::Wait::Held;
        if free {
            mutex.release();
        }
        free
    })
    .join()
    .unwrap_or(false)
}

/// Прокручивает цикл, пока потолок ядра не перестанет меняться (1,5 с без изменений, не дольше 20 с); его значение.
/// 20 с, а не 10: без глубины (DECISIONS #38) горячий блок имитации опускается до остывания — с ×54 до ×43 это
/// 11 ступеней по 0,7 с (окно 0,4 с и пауза 0,3 с проверки).
fn settle(shell: &mut Shell, app: &mut App, core: usize) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = ceilings_now(app).get(core).copied().unwrap_or(0);
    let mut since = Instant::now();
    while Instant::now() < deadline && since.elapsed() < Duration::from_millis(1500) {
        pump(shell, app, Duration::from_millis(50));
        let now = ceilings_now(app).get(core).copied().unwrap_or(0);
        if now != last {
            last = now;
            since = Instant::now();
        }
    }
    last
}

/// Нижняя граница допустимого потолка ядра — последний (самый низкий) пункт того, что можно поставить.
fn choices_floor(app: &App, core: usize) -> i32 {
    app.limits_choices(core).last().copied().unwrap_or(0)
}

fn sets(items: &[Item]) -> Vec<&Item> {
    items.iter().filter(|i| i.id >= FACTORY_ID).collect()
}

pub fn run(root: &Path, out: &Path, saved: bool) -> i32 {
    let mut r = Report {
        lines: Vec::new(),
        failed: false,
    };
    let pid = std::process::id();
    let data = std::env::temp_dir().join(format!("coremap-tray-{pid}"));
    let _ = fs::remove_dir_all(&data); // остатки прошлой проверки с тем же номером процесса — не наши данные
    if let Err(e) = fs::create_dir_all(&data) {
        r.check(false, &format!("временная папка {}: {e}", data.display()));
    }
    if saved {
        for name in ["profile.json", "profiles.json"] {
            let from = root.join("data").join(name);
            if from.exists() && fs::copy(&from, data.join(name)).is_err() {
                r.check(false, &format!("копия {name}"));
            }
        }
    }
    let config = Config {
        root: root.to_path_buf(),
        data: data.clone(),
        names: Names::check(pid),
        simulate: true,
        launch: false,
        pipe: format!("CoreMap.Logon.Check.{pid}"),
        confirm: Confirm::Answer(false),
        grace: Duration::from_millis(300),
        wake_delay: Duration::from_millis(300),
        lease_wait: 500,
        wake_gap: Duration::from_millis(500),
        // Сроки в проверке укорочены в пять раз против настоящих: опрос срезов 50 мс (0,25 с), окно медианы 0,4 с
        // (2 с) — те же 8 показаний; повтор аварийной проверки 0,2 с (1 с); пауза возврата 1 с (настоящая — 2 мин; в
        // проверке короче, чтобы она шла секунды). Опрос раз в 2 с — 50 мс; обороты при строках — каждый восьмой опрос
        // (как 2 с при 0,25 с), видеокарта — так же; настоящий шаг журнала — отдельным шагом проверки (12m).
        auto: crate::app::AutoTiming {
            tick: Duration::from_millis(50),
            sample: Duration::from_millis(50),
            fans: Duration::from_millis(400),
            gpu: Duration::from_millis(400),
            control: machine::auto::Timing {
                window: Duration::from_millis(400),
                recheck: Duration::from_millis(200),
                settle: Duration::from_millis(400),
                pause: Duration::from_secs(1),
            },
        },
    };
    let names = config.names.clone();
    let result = App::new(config, Box::new(QuietLog))
        .and_then(|app| Shell::new(&format!("CoreMap.Logon.Check.{pid}"), false).map(|shell| (app, shell)));
    match result {
        Ok((mut app, mut shell)) => scenario(&mut r, &mut app, &mut shell, &names, (root, out, &data)),
        Err(e) => r.check(false, &format!("запуск: {e}")),
    }
    let mut text = String::from(if r.failed { "FAILED\n" } else { "ok\n" });
    for line in &r.lines {
        let _ = writeln!(text, "{line}"); // into a String: cannot fail
    }
    let _ = fs::remove_dir_all(&data); // временная папка проверки; не удалилась — останется в %TEMP%, вреда нет
    if let Some(dir) = out.parent() {
        let _ = fs::create_dir_all(dir); // нет папки — запись ниже скажет об этом кодом выхода
    }
    if fs::write(out, text).is_err() {
        return 3;
    }
    i32::from(r.failed)
}

#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
fn scenario(r: &mut Report, app: &mut App, shell: &mut Shell, names: &Names, places: (&Path, &Path, &Path)) {
    let data = places.2;
    // 0. Метка автомодуля от прежней загрузки (процессор с тех пор сброшен): по ней ничего не поднимается, даже
    //    если в ней записано выше заводского (ревизия H1).
    let lps: Vec<u32> = app.topology().lps().iter().map(|lp| lp.0).collect();
    let boot_caps = ceilings_now(app);
    let expected_startup = app
        .store()
        .read_profile()
        .ok()
        .flatten()
        .map_or_else(|| boot_caps.clone(), |profile| profile.ratios);
    // Метка должна быть устаревшей и относительно заводских, и относительно сохранённых потолков.
    let stale_low = (1..=255)
        .find(|value| !boot_caps.contains(value) && !expected_startup.contains(value))
        .expect("24 simulated cores cannot occupy all 255 marker values");
    let stale = format!(
        "{{\"cpu\": {}, \"lps\": {}, \"accepted\": {}, \"lowered\": {}}}",
        machine::files::quote(app.store().cpu()),
        machine::files::ints(&lps),
        machine::files::ints(&vec![60; lps.len()]),
        machine::files::ints(&vec![stale_low; lps.len()])
    );
    let _ = fs::write(data.join("auto-held.json"), stale); // не записалась — проверка ниже это покажет
    // data	ray.json, как его пишет 0.5.4: ключа «log» нет — журнал срезов включён (владелец 16.10: два выключателя,
    // журнал по умолчанию включён).
    let _ = fs::write(data.join("tray.json"), r#"{"auto":false,"limit":85,"stepUp":false}"#); // как выше
    let start = Instant::now();
    let before = shell::usage();
    app.start(shell);
    let startup_caps = ceilings_now(app);
    let startup_messages = app.said.clone(); // one diagnostic snapshot at startup

    r.check(
        startup_caps == expected_startup && !data.join("auto-held.json").exists(),
        "метка автомодуля от прежней загрузки ничего не подняла и снята",
    );
    let started = shell::usage();
    r.note(&format!("start {} ms", start.elapsed().as_millis()));
    let ticks = app.auto_ticks();
    pump(shell, app, Duration::from_millis(300));
    let choice = app.auto_choice();
    r.check(
        choice.log && !choice.on && choice.emergency == 95 && app.auto_ticks() > ticks,
        &format!(
            "tray.json прежней сборки без «log» и «emergency», со «stepUp»: журнал срезов включён, автомодуль выключен, аварийная отсечка 95 °C, опрос раз в 2 с идёт ({} сообщений таймера за 0,3 с при шаге проверки 50 мс)",
            app.auto_ticks() - ticks
        ),
    );

    // 1. Запуск: потолки прочитаны, профиль запуска (если есть) поставлен.
    r.check(app.available(), "потолки прочитаны из процессора (имитация)");
    let profile = app.store().read_profile();
    match &profile {
        Ok(Some(p)) => {
            let current: Vec<i32> = app
                .sim()
                .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
                .unwrap_or_default();
            r.check(
                current == p.ratios,
                &format!("профиль запуска «{}» поставлен при входе; сразу {:?}, после опроса {:?}, ожидается {:?}, сообщения {:?}", p.name, startup_caps, current, p.ratios, startup_messages),
            );
        }
        Ok(None) => r.note("профиля запуска нет — стоят заводские"),
        Err(e) => r.check(false, &format!("профиль не прочитан: {e}")),
    }
    r.check(app.said.is_empty(), "при штатном запуске сообщений нет");

    // 2. Меню собрано Windows так, как задумано.
    let items = app.menu();
    match Menu::build(&items) {
        Ok(menu) => {
            let seen = menu.inspect();
            r.check(
                seen == items,
                &format!("меню Windows совпадает с задуманным ({} строк)", seen.len()),
            );
            for (made, shown) in items.iter().zip(&seen) {
                if made != shown {
                    r.note(&format!("задумано {made:?}"));
                    r.note(&format!("в Windows {shown:?}"));
                }
            }
            let checked = sets(&seen).iter().filter(|i| i.checked).count();
            r.check(checked == 1, &format!("отмечен один набор (отмечено {checked})"));
            r.check(
                seen.iter()
                    .any(|i| i.id == WINDOW_ID && i.default && i.text.as_deref() == Some("Окно")),
                "по умолчанию — «Окно» (щелчок по значку открывает новое окно)",
            );
            let old = seen.iter().filter(|i| {
                i.text
                    .as_deref()
                    .is_some_and(|t| t.contains("Новое окно") || t.contains("прежнее окно"))
            });
            r.check(old.count() == 0, "в меню нет «Новое окно» и «Открыть прежнее окно»");
            // Отсечки — в окне (DECISIONS #36), глубины нет (#38), подтягивания нет (бриф T): в подменю автомодуля
            // только включение и пункт, который открывает окно; пресетов нет, о глубине и подтягивании ни слова.
            let auto: Vec<u32> = seen
                .iter()
                .find(|i| {
                    i.text
                        .as_deref()
                        .is_some_and(|t| t.starts_with("Автоограничение нагрева"))
                })
                .map(|i| i.children.iter().filter(|c| c.text.is_some()).map(|c| c.id).collect())
                .unwrap_or_default();
            let texts: Vec<String> = seen
                .iter()
                .flat_map(|i| std::iter::once(i).chain(i.children.iter()))
                .filter_map(|i| i.text.clone())
                .collect();
            r.check(
                auto == [AUTO_ON, AUTO_WINDOW_ID]
                    && !texts.iter().any(|t| t.contains("глубин") || t.contains("одтягив"))
                    && texts.iter().any(|t| t.contains("аварийная 95 °C")),
                &format!("подменю автомодуля: включение и «отсечка …, аварийная 95 °C — настроить в окне» — без пресетов, глубины и подтягивания ({auto:?})"),
            );
            r.check(
                seen.iter().any(|i| {
                    i.id == LOG_ID
                        && i.checked
                        && i.enabled
                        && i.children.is_empty()
                        && i.text.as_deref() == Some("Журнал срезов")
                }),
                "«Журнал срезов» — свой пункт меню верхнего уровня, с отметкой (включён), не в подменю автомодуля",
            );
            r.check(seen.iter().any(|i| i.id == EXIT_ID && i.enabled), "«Выход» доступен");
        }
        Err(e) => r.check(false, &e),
    }

    // 3. Щелчок по другому набору: он ставится; профиль запуска не меняется — его выбирают отдельно, отметкой
    //    «загружать при старте Windows» в окне (DECISIONS #38).
    let target = sets(&items)
        .into_iter()
        .find(|i| !i.checked && i.id > FACTORY_ID)
        .map_or(FACTORY_ID, |i| i.id);
    let writes = app.sim().map_or(0, |s| s.writes);
    let logon = fs::read_to_string(data.join("profile.json")).ok();
    let events = data
        .join("events")
        .join(format!("{}.jsonl", Stamp::now().local(Pattern::Date)));
    let (events_from, ceilings_from) = (lines_in(&events), ceilings_now(app));
    app.command(shell, target);
    let ceilings_to = ceilings_now(app);
    let changed: Vec<String> = app
        .topology()
        .blocks
        .iter()
        .filter(|b| ceilings_from.get(b.first) != ceilings_to.get(b.first))
        .map(|b| b.name.clone())
        .collect();
    let lines: Vec<serde_json::Value> = lines_after(&events, events_from)
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let listed: Vec<String> = lines
        .iter()
        .filter_map(|v| v["block"].as_str().map(str::to_string))
        .collect();
    r.check(
        !changed.is_empty()
            && listed == changed
            && lines.iter().all(|v| v["kind"] == "profile" && v["period"]["ceiling"] == v["from"] && v["to"].is_i64()),
        &format!(
            "журнал событий: набор из меню — по строке profile на каждый блок, у которого потолок сменился ({} из {}): {listed:?}",
            lines.len(),
            changed.len()
        ),
    );
    let after = app.menu();
    let now_checked = sets(&after).into_iter().find(|i| i.checked).map(|i| i.id);
    r.check(
        now_checked == Some(target),
        &format!("после щелчка отмечен выбранный набор ({target})"),
    );
    let wrote = app.sim().map_or(0, |s| s.writes) > writes;
    r.check(wrote, "потолки записаны в процессор");
    r.check(
        fs::read_to_string(data.join("profile.json")).ok() == logon,
        "профиль запуска тот же: profile.json не тронут",
    );
    let back = sets(&items).into_iter().find(|i| i.checked).map(|i| i.id);

    // 4. Открыто прежнее окно: процессор не трогаем, наборы серые; закрылось — потолки снова наши, метка замера
    //    возвращена.
    let window = hold(&names.old_window);
    shell.post(shell::WINDOW_OPENED, 4242, 0);
    pump(shell, app, Duration::from_millis(100));
    let grey = app.menu();
    r.check(
        sets(&grey).iter().all(|i| !i.enabled && !i.checked),
        "пока открыто окно, наборы серые и без отметки",
    );
    r.check(
        grey.first().and_then(|i| i.text.as_deref())
            == Some("открыто прежнее окно CoreMap — профили переключаются в нём"),
        "пояснение в меню: окно открыто",
    );
    let writes = app.sim().map_or(0, |s| s.writes);
    let reads = app.sim().map_or(0, |s| s.reads);
    let refused = app.switch("");
    r.check(
        refused
            .as_deref()
            .is_some_and(|t| t.contains("открыто прежнее окно CoreMap")),
        "щелчок по набору при открытом окне — отказ",
    );
    r.check(
        app.sim().map_or(0, |s| s.writes) == writes,
        "при открытом окне в процессор ничего не записано",
    );
    r.check(
        app.sim().map_or(0, |s| s.reads) == reads,
        "при открытом окне процессор даже не читается",
    );
    // «Окно» ведёт замер: блок 0 стоит на ступени, метка хранит потолки до замера.
    let intended = app.intended().map(<[i32]>::to_vec).unwrap_or_default();
    let block0 = app.topology().blocks[0].first;
    let step = u8::try_from(intended.get(block0).copied().unwrap_or(40) - 3).unwrap_or(40);
    if let Some(sim) = app.sim() {
        sim.foreign_write(block0, step);
    }
    let lps: Vec<u32> = app.topology().lps().iter().map(|lp| lp.0).collect();
    let marker = format!(
        "{{\"cpu\": {}, \"time\": \"2026-10-14 20:00:00\", \"lps\": {}, \"limits\": {}}}",
        machine::files::quote(app.store().cpu()),
        machine::files::ints(&lps),
        machine::files::ints(&intended)
    );
    let _ = fs::write(data.join("limits-held.json"), marker); // не записалась — проверка ниже это покажет
    let said = app.said.len();
    let _ = window.send(()); // поток «окна» кончается, не отпустив метку
    let marker = data.join("limits-held.json");
    pump_until(shell, app, Duration::from_secs(5), |_| !marker.exists());
    let current: Vec<i32> = app
        .sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default();
    r.check(
        current.get(block0) == intended.get(block0),
        "окно закрылось — потолок блока 0 возвращён по метке замера",
    );
    r.check(
        app.said[said..]
            .iter()
            .any(|s| s.starts_with("Прошлый запуск оборвался посреди измерения")),
        "сообщение о возврате по метке",
    );
    r.check(!data.join("limits-held.json").exists(), "метка замера удалена");
    r.check(
        sets(&app.menu()).iter().all(|i| i.enabled),
        "окно закрылось — наборы снова доступны",
    );

    // 5. Движок с живым Core: то же, что окно.
    let engine = hold(&names.engine);
    let core = exist(&names.engine_core);
    pump(shell, app, Duration::from_millis(50));
    let _ = app.menu(); // программа замечает движок (как при взгляде на значок)
    r.check(
        app.tip() == format!("CoreMap {} — идёт настройка", crate::VERSION),
        "подсказка: идёт настройка",
    );
    let taken_back = logged(app, "потолки снова мои");
    let _ = core.send(());
    let _ = engine.send(());
    pump_until(shell, app, Duration::from_secs(5), |a| {
        logged(a, "потолки снова мои") > taken_back
    });

    // 6. Аренда занята другой частью CoreMap: действие ждёт, потом отказывается; освободилась — проходит.
    let lease = names.lease.clone();
    let (free, freed) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        if let Ok((mutex, _)) = named::create(&lease, false) {
            let held = mutex.acquire(0);
            let _ = freed.recv(); // держит аренду, пока проверка не скажет
            if held == named::Wait::Held {
                mutex.release();
            }
        }
    });
    thread::sleep(Duration::from_millis(50));
    let refused = app.switch("");
    r.check(
        refused
            .as_deref()
            .is_some_and(|t| t.contains("меняет другая часть CoreMap")),
        "аренда занята — отказ после ожидания",
    );
    let _ = free.send(());
    let _ = holder.join();
    if let Some(back) = back {
        app.command(shell, back);
    }
    let restored = app.menu();
    r.check(
        sets(&restored).into_iter().find(|i| i.checked).map(|i| i.id) == back,
        "аренда свободна — набор переключается",
    );

    // 7. Сон сбросил потолки к заводским: после пробуждения они поставлены заново.
    let intended = app.intended().map(<[i32]>::to_vec).unwrap_or_default();
    if let Some(sim) = app.sim() {
        sim.reset_to_factory();
    }
    let woke = logged(app, "после пробуждения");
    shell.post(windows_sys::Win32::UI::WindowsAndMessaging::WM_POWERBROADCAST, 0x12, 0); // PBT_APMRESUMEAUTOMATIC
    pump_until(shell, app, Duration::from_secs(5), |a| {
        logged(a, "после пробуждения") > woke
    });
    let current: Vec<i32> = app
        .sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default();
    r.check(
        current == intended,
        "после пробуждения сброшенные потолки поставлены заново",
    );
    if current != intended {
        r.note(&format!("стоит {current:?}"));
        r.note(&format!("должно {intended:?}"));
    }
    let log = fs::read_to_string(app.journal().tray_path()).unwrap_or_default();
    r.check(
        log.contains("после пробуждения"),
        "сверка после пробуждения записана в tray.log",
    );

    // 8. Чужая программа изменила потолок: принять, сказать один раз, не писать.
    // Значение, которого программа сама не ставила: на пять ступеней ниже того, что должно стоять.
    let target = app.topology().blocks[1].first;
    let value = u8::try_from(intended.get(target).copied().unwrap_or(40) - 5).unwrap_or(30);
    if let Some(sim) = app.sim() {
        sim.foreign_write(target, value);
    }
    let writes = app.sim().map_or(0, |s| s.writes);
    let said = app.said.len();
    app.wake_repair(shell);
    r.check(
        app.sim().map_or(0, |s| s.writes) == writes,
        "чужую правку не перебивает",
    );
    r.check(
        app.said[said..].iter().any(|s| s.contains("изменила другая программа")),
        "о чужой правке сказано",
    );
    r.check(
        app.intended().and_then(|i| i.get(target)).copied() == Some(i32::from(value)),
        "чужая правка принята",
    );
    let said = app.said.len();
    app.wake_repair(shell);
    r.check(app.said.len() == said, "второй раз о той же правке не говорит");

    // 9. «Окно» запускает окно на Rust с каналом программы входа (в проверке только записывается), щелчок по
    //    значку — то же; второе, пока первое ещё открывается, не запускается. C#-частей программа не запускает.
    let pipe = app.serving().pipe.clone();
    app.command(shell, WINDOW_ID);
    r.check(
        app.launched == [format!("CoreMapWindow.exe --pipe {pipe}")],
        &format!("«Окно» запускает CoreMapWindow.exe --pipe {pipe}"),
    );
    let said = app.said.len();
    // Щелчок по значку, как его шлёт Проводник значку версии 4 (младшее слово — WM_LBUTTONUP, затем NIN_SELECT):
    // окно ещё открывается после «Окна» — второе не запускается, владелец видит почему; второе сообщение того же
    // щелчка и двойной щелчок — не повторяют ни запуск, ни сообщение.
    shell.post(0x8001, 0, 0x202);
    shell.post(0x8001, 0, 0x400);
    pump(shell, app, Duration::from_millis(100));
    let told = app.said[said..]
        .iter()
        .filter(|s| s.contains("ещё открывается"))
        .count();
    r.check(
        app.launched.len() == 1 && told == 1,
        &format!("щелчок, пока окно открывается, — второе не запускается, сказано {told} раз"),
    );
    let said = app.said.len();
    shell.post(0x8001, 0, 0x400);
    pump(shell, app, Duration::from_millis(100));
    r.check(
        app.launched.len() == 1 && app.said.len() == said,
        "второй щелчок двойного — ни запуска, ни сообщения",
    );
    r.note(&format!("запуски: {:?}", app.launched));
    r.check(
        !app.launched.iter().any(|l| l.contains("CoreMap.exe")),
        "части на C# программа входа не запускает",
    );

    let log = fs::read_to_string(app.journal().tray_path()).unwrap_or_default();
    for line in log.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev() {
        r.note(&format!("tray.log: {line}"));
    }

    let resume_log = app.auto_choice().log;
    if resume_log {
        app.command(shell, LOG_ID);
    }
    // 9a. Модерн-стэндбай: пробуждение приходит как «экран выключен → включён». Выход из «притушен» — не сон.
    let intended = app.intended().map(<[i32]>::to_vec).unwrap_or_default();
    let wake_from = app.journal().tray_path().metadata().map_or(0, |m| m.len());
    if let Some(sim) = app.sim() {
        sim.reset_to_factory();
    }
    display(shell, app, 2, Duration::from_millis(50));
    display(shell, app, 1, Duration::from_millis(600));
    let current: Vec<i32> = app
        .sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default();
    r.check(
        current != intended,
        "экран из «притушен» во «включён» — это не пробуждение, сверки нет",
    );
    display(shell, app, 0, Duration::from_millis(50));
    // Сверка после пробуждения не чаще раза за промежуток (в проверке он короткий, wake_gap).
    display(shell, app, 1, Duration::from_millis(50));
    pump_until(shell, app, Duration::from_secs(5), |a| ceilings_now(a) == intended);
    let current: Vec<i32> = app
        .sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default();
    r.check(
        current == intended,
        "экран «выключен → включён»: сброшенные потолки поставлены заново",
    );
    let grown = app.journal().tray_path().metadata().map_or(0, |m| m.len()) > wake_from;
    r.check(grown, "и это записано в tray.log");
    if !grown || current != intended {
        let log = fs::read_to_string(app.journal().tray_path()).unwrap_or_default();
        for line in log.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev() {
            r.note(&format!("tray.log после экрана: {line}"));
        }
    }

    if resume_log {
        app.command(shell, LOG_ID);
    }
    // 9b. Сон, пока открыто окно: после его закрытия профиль ставится заново, а не принимается то, что оставил сон.
    let profile = app.store().read_profile().ok().flatten();
    let window = hold(&names.old_window);
    pump(shell, app, Duration::from_millis(50));
    let _ = app.menu(); // программа замечает окно (как при взгляде на значок)
    shell.post(windows_sys::Win32::UI::WindowsAndMessaging::WM_POWERBROADCAST, 4, 0); // PBT_APMSUSPEND
    pump(shell, app, Duration::from_millis(50));
    if let Some(sim) = app.sim() {
        sim.reset_to_factory();
    }
    let _ = window.send(());
    let wanted = profile.as_ref().map(|p| p.ratios.clone()).unwrap_or_default();
    pump_until(shell, app, Duration::from_secs(5), |a| ceilings_now(a) == wanted);
    let current: Vec<i32> = app
        .sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default();
    match profile {
        Some(p) => r.check(
            current == p.ratios,
            "сон во время работы окна: после него профиль запуска поставлен заново",
        ),
        None => r.note("сон во время работы окна: профиля запуска нет — проверять нечего"),
    }

    // 11. Автомодуль (DECISIONS #31, #32, #38; две отсечки — #40, бриф T): нагруженный горячий блок опускается целевой
    //     отсечкой, пока его 2-с медиана выше предела и не падает, — не ниже нижней границы допустимого потолка, держа
    //     аренду и метку; каждый срез — в память срезов с уровнем кристалла; стартующему прежнему окну — принятые
    //     потолки и аренда. Остывший блок не возвращается, пока кристалл не холоднее, чем при срезе, и возвращается по
    //     ступени за паузу, когда холоднее. Остальные ядра заняты на 0,15: процессор занят ≥ 10 %, строки журнала идут.
    let accepted = app.intended().map(<[i32]>::to_vec).unwrap_or_default();
    // Горячий блок — с самым высоким принятым потолком (кроме кластера 2–5: ему ниже чужая правка): нагрев имитации
    // 40 + (потолок − 20) · 1,5 выше 75 °C с ×44. Профиль — копия файлов владельца, номер блока заранее не известен
    // (15.10 его csmart2 поставил ядру 1 ×43 — 74,5 °C, и проверка с жёстко взятым блоком 1 перестала греть).
    let block = app
        .topology()
        .blocks
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != 2)
        .max_by_key(|&(_, b)| accepted.get(b.first).copied().unwrap_or(0))
        .map_or_else(|| app.topology().blocks[1].clone(), |(_, b)| b.clone());
    let returned = format!("автомодуль: блок {} пробует", block.name);
    let first = block.first;
    let marker = data.join("auto-held.json");
    let set_load = |app: &mut App, hot: f64, rest: f64| {
        if let Some(sim) = app.sim() {
            for (core, load) in sim.load.iter_mut().enumerate() {
                *load = if block.members.contains(&core) { hot } else { rest };
            }
        }
    };
    app.command(shell, AUTO_ON);
    // Отсечка — тем же путём, что команда окна «auto.set» (пункта меню для неё больше нет).
    let refused = app.set_auto(shell, &serde_json::from_str(r#"{"limit":75}"#).unwrap_or_default());
    r.check(
        refused.is_none(),
        &format!("автомодуль: отсечка 75 °C принята {refused:?}"),
    );
    let settings = fs::read_to_string(data.join("tray.json")).unwrap_or_default();
    r.check(
        settings.contains("\"auto\":true")
            && settings.contains("\"limit\":75")
            && settings.contains("\"emergency\":95")
            && !settings.contains("depth")
            && !settings.contains("stepUp"),
        &format!(
            "автомодуль: выбор записан в tray.json, с аварийной отсечкой, без глубины и подтягивания ({settings})"
        ),
    );
    let base = accepted.get(first).copied().unwrap_or(0);
    // Нижняя граница допустимого потолка блока — та же, что у ручного (меню потолка окна): её последний пункт.
    let floor = choices_floor(app, first);
    set_load(app, 1.0, 0.15);
    let lowest = settle(shell, app, first);
    let heat = app
        .sim()
        .and_then(|s| s.block(first).ok())
        .and_then(|r| r.temp)
        .unwrap_or(f64::NAN);
    r.check(
        lowest < base && lowest >= floor && (heat <= 75.0 || lowest == floor),
        &format!(
            "автомодуль: горячий блок {} опущен целевой отсечкой ×{base} → ×{lowest}, пока не остыл ({heat:.1} °C при пределе 75; ниже ×{floor} нельзя)",
            block.name
        ),
    );
    let now = ceilings_now(app);
    r.check(
        now.iter()
            .zip(&accepted)
            .enumerate()
            .all(|(i, (c, a))| block.members.contains(&i) || c == a),
        "автомодуль: остальные блоки (заняты на 0,15, ниже предела) не тронуты",
    );
    r.check(marker.exists(), "автомодуль: метка с принятыми потолками записана");
    r.check(
        !lease_free(&names.lease),
        "автомодуль: пока блок опущен, аренда держится",
    );
    let tip = app.tip();
    r.check(
        tip.contains(&format!(
            "авто: до 75 °C, аварийная 95 °C · {} −{}",
            block.name,
            base - lowest
        )),
        &format!("автомодуль: подсказка ({})", tip.replace('\n', " / ")),
    );
    let window = hold(&names.old_window);
    pump_until(shell, app, Duration::from_secs(3), |_| lease_free(&names.lease));
    r.check(
        ceilings_now(app).get(first) == Some(&lowest),
        "автомодуль: передача прежнему окну сохраняет тепловой потолок",
    );
    r.check(
        marker.exists() && lease_free(&names.lease),
        "автомодуль: сохранена метка защиты, аренда отпущена",
    );
    let taken_back = logged(app, "потолки снова мои");
    let _ = window.send(());
    pump_until(shell, app, Duration::from_secs(5), |a| {
        logged(a, "потолки снова мои") > taken_back
    });
    let again = settle(shell, app, first);
    r.check(
        again < base,
        &format!("автомодуль: окно закрылось — горячий блок снова опущен (×{again})"),
    );
    // Память физических состояний: отбросить оперативную копию и прочитать уже записанное с диска.
    let cuts = app.cuts_now();
    app.restart_physical_memory();
    r.check(
        cuts > 0 && app.cuts_now() == cuts && logged(app, "среднее за") > 0,
        &format!("автомодуль: память физических состояний ({cuts} записей) восстановлена с диска после очистки оперативной копии"),
    );
    check_logs(r, app, data, block.first);
    // Остывание своего блока в простое не даёт доказательства для возврата, даже при работе остальных.
    set_load(app, 0.0, 0.15);
    let raised = logged(app, &returned);
    pump(shell, app, Duration::from_secs(3));
    r.check(
        ceilings_now(app).get(first) == Some(&again) && logged(app, &returned) == raised,
        &format!(
            "автомодуль: остывший простаивающий блок не вернулся за 3 с при паузе 1 с (стоит {:?})",
            ceilings_now(app).get(first)
        ),
    );
    // Остывание всего процессора в простое не испытывает потолок и не разрешает возврат.
    let log_from = lines_in(&data.join("auto-log.jsonl"));
    set_load(app, 0.0, 0.0);
    pump(shell, app, Duration::from_secs(3));
    let back: Vec<serde_json::Value> = lines_after(&data.join("auto-log.jsonl"), log_from)
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["act"] == "return" && v["first"] == first)
        .collect();
    r.check(
        ceilings_now(app).get(first) == Some(&again) && back.is_empty(),
        "возврат: простой всех ядер и холодный кристалл не поднимают потолок",
    );
    r.check(
        marker.exists() && !lease_free(&names.lease),
        "возврат: тепловая защита остаётся в метке и под арендой",
    );
    let refused = app.set_auto(shell, &serde_json::from_str(r#"{"limit":76}"#).unwrap_or_default());
    r.check(
        refused.is_none() && ceilings_now(app).get(first) == Some(&again),
        "смена целевой отсечки сохраняет снижение и сбрасывает только окно проверки",
    );
    app.wake_repair(shell);
    r.check(
        ceilings_now(app).get(first) == Some(&again),
        "пробуждение без сброса процессора сохраняет тепловой потолок",
    );
    if let Some(sim) = app.sim() {
        sim.reset_to_factory();
    }
    app.wake_repair(shell);
    r.check(
        ceilings_now(app).get(first) == Some(&again),
        "сброс процессора сном восстанавливает сниженный, а не исходный потолок",
    );
    // Вернуть целевую для следующих сценариев; само по себе это не отменяет защиту.
    app.set_auto(shell, &serde_json::from_str(r#"{"limit":75}"#).unwrap_or_default());
    // Чужая правка у блока, который автомодуль не трогал: принять, сказать один раз, больше этот блок не трогать.
    let other = app.topology().blocks[2].first; // кластер E-ядер 2–5
    let foreign = u8::try_from(accepted.get(other).copied().unwrap_or(40) - 2).unwrap_or(30);
    let said = app.said.len();
    if let Some(sim) = app.sim() {
        sim.foreign_write(other, foreign);
    }
    pump_until(shell, app, Duration::from_secs(3), |a| {
        a.intended().and_then(|i| i.get(other)) == Some(&i32::from(foreign))
    });
    pump(shell, app, Duration::from_millis(300));
    r.note(&format!(
        "чужая правка: блок {other} ×{foreign}, стоит {:?}, принятые {:?}",
        ceilings_now(app).get(other),
        app.intended().and_then(|i| i.get(other))
    ));
    let told = app.said[said..]
        .iter()
        .filter(|s| s.contains("автомодуль их больше не трогает"))
        .count();
    r.check(
        app.intended().and_then(|i| i.get(other)) == Some(&i32::from(foreign)) && told == 1,
        &format!("автомодуль: чужая правка принята, сказано один раз ({told})"),
    );
    app.command(shell, AUTO_ON); // выключить

    // 11a. Две отсечки на заданных температурах (бриф T): аварийная — по одному показанию и через паузу снова; целевая —
    //      по медиане, ждёт, пока падает; простой блок срезается; возврат — после полной паузы и не раньше, если в ней
    //      была медиана выше предела.
    cutoffs(r, app, shell, data, &block);

    // 11b. Журнал срезов на настоящих сроках (опрос 0,25 с, обороты раз в 2 с, решение раз в 2 с), окна нет.
    logging_real(r, app, shell, data, &block);
    filtered_capture(r, app, shell, data);

    // 11c. Два выключателя — журнал срезов и автомодуль — независимы (владелец 16.10): все четыре сочетания.
    switches(r, app, shell, data, &block);

    // 10. Цена: память и процессор в покое, окна нет. Автомодуль выключен, журнал включён: опрос раз в 2 с идёт и при
    //     выключенном автомодуле (владелец 16.10), но процессор не занят — датчики он не читает. Покой меряется на
    //     настоящих сроках: у проверки опрос раз в 50 мс, в 40 раз чаще, — на них мерилась бы проверка, а не программа
    //     (s24a: 31 мс за 3 с на сроках проверки). Цена одного опроса на сроках проверки — строкой отчёта.
    let (fast_from, ticks) = (shell::usage(), app.auto_ticks());
    pump(shell, app, Duration::from_secs(1));
    let (fast_to, polled) = (shell::usage(), app.auto_ticks() - ticks);
    r.note(&format!(
        "опрос раз в 2 с при выключенном автомодуле и незанятом процессоре, сроки проверки: {polled} сообщений таймера за 1 с, процессора {} мкс",
        fast_to.cpu.saturating_sub(fast_from.cpu) / 10
    ));
    let was = app.retime(shell, AutoTiming::REAL);
    let idle_from = shell::usage();
    pump(shell, app, Duration::from_secs(3));
    let idle_to = shell::usage();
    app.retime(shell, was);
    let mb = |b: usize| f64::from(u32::try_from(b / 1024).unwrap_or(u32::MAX)) / 1024.0;
    let ms = |ticks: u64| ticks / 10_000;
    r.note(&format!(
        "память выделенная: до запуска {:.1} МБ, после запуска {:.1} МБ, после проверки {:.1} МБ; рабочий набор {:.1} МБ",
        mb(before.private),
        mb(started.private),
        mb(idle_to.private),
        mb(idle_to.working)
    ));
    r.note(&format!(
        "процессор в покое: {} мс за 3 с",
        ms(idle_to.cpu.saturating_sub(idle_from.cpu))
    ));
    r.check(
        idle_to.cpu.saturating_sub(idle_from.cpu) < 200_000,
        "в покое процессор не тратится (меньше 20 мс за 3 с; опрос — на настоящих сроках, раз в 2 с)",
    );

    // 12. Окно по каналу (DECISIONS #32, шаг 2) — после замера цены: его клиент держит в этом же процессе все
    //     полученные сообщения, и память после него — уже не память программы входа.
    crate::check_window::run(r, app, shell, names, places);

    // 12b. Входы общего теплового пакета (DECISIONS #37): файл дня, начатый сборкой с другими столбцами, не трогается —
    //      день продолжается в «<день>-2.csv» со своим заголовком; видеокарта или мощность корпуса не прочитались —
    //      пусто, одна строка в error.log, журнал идёт, повтор — не в каждом опросе.
    package_inputs(r, app, shell, data);

    // 13. Журнал срезов, когда что-то не так: файл дня удалён, перемещён, не открывается; обороты отказывают; потолки
    //     не читаются. Файл дня после этого шага может не открываться ещё 10 с — дальше он не нужен.
    logging_failures(r, app, shell, data);

    // 14. Выход с открытым окном: сначала окно, потом программа. Последним шагом: после него цикл программы кончен.
    crate::check_window::leaving(r, app, shell, places);
}

/// 11a. Две отсечки на заданных температурах (бриф T; нагрев имитации мгновенный и стоит на месте — температуры задаёт
/// проверка, `SimCpu::forced`). Все ядра простаивают (доля работы 0): условий нагрузки нет нигде. Сроки проверки: опрос
/// 50 мс, окно медианы 0,4 с, повтор аварийной проверки 0,2 с, пауза возврата 1 с. Предел 75 °C (шаг 11), аварийный —
/// 90 °C, поставлен командой, как из окна.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
fn cutoffs(r: &mut Report, app: &mut App, shell: &mut Shell, data: &Path, hot: &machine::topology::Block) {
    let first = hot.first;
    let blocks = app.topology().blocks.len();
    let log = data.join("auto-log.jsonl");
    // Горячий блок — `hot_temp`, остальные — `rest`; доля работы у всех 0.
    let force = |app: &mut App, hot_temp: f64, rest: f64| {
        if let Some(sim) = app.sim() {
            for (core, t) in sim.forced.iter_mut().enumerate() {
                *t = Some(if hot.members.contains(&core) { hot_temp } else { rest });
            }
            sim.load.fill(0.0);
        }
    };
    // Строки журнала действий этого блока с этим действием, дописанные после первых `from` строк.
    let acts = |from: usize, act: &str| -> Vec<serde_json::Value> {
        lines_after(&log, from)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["act"] == act && v["first"] == first)
            .collect()
    };
    let at = |v: &serde_json::Value| v["at"].as_i64().unwrap_or(0) / 10_000; // мс
    let gaps = |list: &[serde_json::Value]| -> Vec<i64> { list.windows(2).map(|w| at(&w[1]) - at(&w[0])).collect() };
    let idle = |list: &[serde_json::Value]| list.iter().all(|v| v["busy"] == 0.0);

    force(app, 40.0, 40.0);
    app.command(shell, AUTO_ON);
    let mut emergency = serde_json::Map::new();
    emergency.insert("emergency".into(), 90.into());
    let refused = app.set_auto(shell, &emergency);
    pump(shell, app, Duration::from_secs(1)); // 2-с медианы всех блоков — 40 °C
    let settings = fs::read_to_string(data.join("tray.json")).unwrap_or_default();
    r.check(
        refused.is_none() && app.auto_choice().emergency == 90 && settings.contains("\"emergency\":90"),
        &format!("аварийная отсечка 90 °C принята командой auto.set, записана в tray.json ({settings})"),
    );

    // а) Одно показание не ниже аварийного предела — ступень сразу, одна; медиану одно показание не сдвигает.
    let from = lines_in(&log);
    force(app, 95.0, 40.0);
    pump_until(shell, app, Duration::from_secs(2), |_| {
        !acts(from, "emergency").is_empty()
    });
    force(app, 40.0, 40.0);
    pump(shell, app, Duration::from_millis(1200));
    let once = acts(from, "emergency");
    let none = acts(from, "target");
    r.check(
        once.len() == 1
            && none.is_empty()
            && once[0]["reading"] == 95.0
            && once[0]["die"] == 40.0
            && once[0]["to"].as_i64() == once[0]["from"].as_i64().map(|f| f - 1)
            && once[0]["means"].as_array().is_some_and(|m| m.len() == blocks)
            && once[0]["fans"] == serde_json::json!([3200, 3000])
            && once[0].get("gpu").is_some()
            && idle(&once),
        &format!(
            "аварийная отсечка: одно показание 95 °C у простаивающего блока — {} ступень сразу, целевых — {} (медиана 40 °C); строка журнала: {}",
            once.len(),
            none.len(),
            once.first().map(ToString::to_string).unwrap_or_default()
        ),
    );

    // б) Всё ещё горячо: следующая аварийная ступень — не раньше паузы 0,2 с (1 с у настоящих сроков), и она есть.
    let from = lines_in(&log);
    force(app, 95.0, 40.0);
    pump(shell, app, Duration::from_millis(1100));
    force(app, 40.0, 40.0);
    let again = acts(from, "emergency");
    let spaced = gaps(&again);
    r.check(
        again.len() >= 3 && spaced.iter().all(|&g| g >= 195) && idle(&again),
        &format!(
            "аварийная отсечка: 95 °C 1,1 с — {} ступеней, между ними не меньше паузы 0,2 с (мс: {spaced:?})",
            again.len()
        ),
    );

    // в) Целевая: 2-с медиана выше предела и не падает — ступень раз в окно; выше, но падает — ждать. Сначала секунда при
    //    40 °C: прошлая медиана — не 95 °C из шага б (иначе первые окна при 88 °C «падали» бы).
    pump(shell, app, Duration::from_secs(1));
    force(app, 88.0, 40.0);
    let from = lines_in(&log);
    // Ждём два действия, а не две удачно попавшие в короткий стеновой срок серии Windows-таймеров.
    // Минимальный интервал проверяется отдельно; при пропуске опросов контроллер правильно начинает окно заново.
    let flat_started = Instant::now();
    pump_until(shell, app, Duration::from_secs(3), |_| acts(from, "target").len() >= 2);
    let flat_ms = flat_started.elapsed().as_millis();
    let flat = acts(from, "target");
    let flat_gaps = gaps(&flat);
    // Падает 88 → 76 °C (8 °C/с): медиана каждого окна ниже прошлой, все выше 75. Первые 0,45 с не в счёт — в первом окне
    // спада ещё показания ровной части.
    let start = Instant::now();
    let mut from = None;
    while start.elapsed() < Duration::from_millis(1500) {
        if from.is_none() && start.elapsed() >= Duration::from_millis(450) {
            from = Some(lines_in(&log));
        }
        force(app, 87.5 - 8.0 * start.elapsed().as_secs_f64(), 40.0);
        pump(shell, app, Duration::from_millis(25));
    }
    let falling = acts(from.unwrap_or(usize::MAX), "target");
    r.check(
        flat.len() >= 2
            && flat_gaps.iter().all(|&g| g >= 780)
            && flat.iter().all(|v| v["mean"].as_f64().is_some_and(|m| m > 75.0))
            && idle(&flat)
            && !falling.is_empty(),
        &format!(
            "целевая отсечка: 88 °C {flat_ms} мс — {} ступеней раз в окно (мс: {flat_gaps:?}); 88 → 76 °C спадом — {} ступеней",
            flat.len(),
            falling.len()
        ),
    );

    // г) Нулевая занятость при холодных датчиках не даёт данных для возврата.
    let from = lines_in(&log);
    let held = ceilings_now(app).get(first).copied();
    force(app, 50.0, 35.0);
    pump(shell, app, Duration::from_secs(3));
    r.check(
        acts(from, "return").is_empty() && ceilings_now(app).get(first).copied() == held,
        "возврат: после аварийной и целевой отсечек остывание в простое не разрешает повышение",
    );
    // Заданные температуры — прочь, аварийная — как была, автомодуль выключен (опущенные возвращаются).
    if let Some(sim) = app.sim() {
        sim.forced.fill(None);
    }
    let mut back = serde_json::Map::new();
    back.insert("emergency".into(), 95.into());
    let refused = app.set_auto(shell, &back);
    app.command(shell, AUTO_ON); // выключить
    r.check(
        refused.is_none() && !app.auto_choice().on && app.auto_choice().emergency == 95,
        "после шага: аварийная 95 °C, автомодуль выключен",
    );
}

/// Строк в error.log с этим текстом.
fn errors(data: &Path, needle: &str) -> usize {
    fs::read_to_string(data.join("error.log"))
        .unwrap_or_default()
        .matches(needle)
        .count()
}

/// Строки файла после первых `skip` (дописанные после того, как в нём было `skip` строк).
fn lines_after(path: &Path, skip: usize) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .skip(skip)
        .map(str::to_string)
        .collect()
}

/// Шаг 12b — входы общего теплового пакета (DECISIONS #37). Сроки — как у шага 13: опрос срезов 50 мс, видеокарта и
/// обороты — раз в 0,4 с; окна нет.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
fn package_inputs(r: &mut Report, app: &mut App, shell: &mut Shell, data: &Path) {
    // Предыдущий сценарий проверял сохранение ручек 3/4/2/60 через окно. Этот сценарий отдельно задаёт свои сроки.
    let defaults = serde_json::json!({"windowSecs": 2, "settleSecs": 2, "returnGap": 0, "returnSecs": 120});
    let refusal = app.set_auto(shell, defaults.as_object().expect("настройки стенда — объект"));
    r.check(
        refusal.is_none(),
        "входы теплового пакета: исходные ручные сроки сценария заданы",
    );
    let was = app.retime(
        shell,
        AutoTiming {
            tick: Duration::from_millis(200),
            sample: Duration::from_millis(50),
            fans: Duration::from_millis(400),
            gpu: Duration::from_millis(400),
            control: machine::auto::Timing::REAL,
        },
    );
    let blocks = app.topology().blocks.clone(); // a dozen names, once
    let now = ceilings_now(app);
    let Some(hot) = blocks
        .iter()
        .max_by_key(|b| now.get(b.first).copied().unwrap_or(0))
        .cloned()
    else {
        r.check(false, "входы теплового пакета: блоков нет");
        return;
    };
    // Горячий блок — `value`; пока он занят, остальные ядра — на 0,15: процессор занят ≥ 10 %, строки журнала пишутся
    // (DECISIONS #32, подзапись 16.10; автомодуль строк не пишет — бриф T).
    let load = |app: &mut App, value: f64| {
        if let Some(sim) = app.sim() {
            let rest = if value > 0.0 { 0.15 } else { 0.0 };
            for (core, load) in sim.load.iter_mut().enumerate() {
                *load = if hot.members.contains(&core) { value } else { rest };
            }
        }
    };
    let file = today(data);
    let day = Stamp::now().local(Pattern::Date);
    let part2 = file.with_file_name(format!("{day}-2.csv"));

    // Файл дня, начатый 0.5.3: её заголовок (без столбцов теплового пакета) и одна строка. Журнал стоит (автомодуль
    // выключен после шага 12 и процессор не занят), файл закрыт.
    let mut old = String::from("time,package,fan_cpu,fan_gpu");
    for b in &blocks {
        let n = &b.name;
        let _ = write!(old, ",{n}.ceiling,{n}.ratio,{n}.busy,{n}.temp"); // into a String: cannot fail
    }
    let old_header = old.clone(); // compared with the new header below
    old.push_str("\n2026-10-15 10:00:00.000,70,,");
    for _ in &blocks {
        old.push_str(",53,45.00,0.50,70");
    }
    old.push('\n');
    let idle = !app.logging().rows;
    let _ = fs::remove_file(&part2); // the scratch folder may hold none; a leftover would be ours anyway
    let planted = fs::write(&file, &old).is_ok();
    load(app, 1.0);
    app.command(shell, AUTO_ON);
    pump_until(shell, app, Duration::from_secs(3), |_| lines_in(&part2) >= 4);
    let kept = fs::read_to_string(&file).unwrap_or_default() == old;
    let text = fs::read_to_string(&part2).unwrap_or_default();
    let first = text.lines().next().unwrap_or_default();
    let width = first.split(',').count();
    let told = logged(app, &format!("пишу в {day}-2.csv"));
    r.check(
        idle && planted
            && kept
            && first.starts_with(&format!("{old_header},"))
            && first.contains(",power_cpu,temp_gpu,power_gpu,busy_gpu,")
            && first.ends_with(",busy_gpu,power_cores,filtered_from")
            && text.lines().count() >= 4
            && text.lines().skip(1).all(|l| l.split(',').count() == width)
            && told == 1,
        &format!(
            "файл дня со столбцами прежней сборки не тронут ({}), день продолжен в {day}-2.csv со своим заголовком ({width} столбцов, строк {}), в tray.log — {told} строка",
            if kept { "как был" } else { "ИЗМЕНЁН" },
            text.lines().count().saturating_sub(1)
        ),
    );

    // Видеокарта не прочиталась: в этой и следующих строках её столбцы пусты, одна строка в error.log, новый читатель —
    // через 10 с (не в каждом опросе), журнал идёт.
    app.sim_gpu(true);
    let calls = app.gpu_calls();
    pump_until(shell, app, Duration::from_secs(2), |a| a.gpu_calls() > calls);
    let failed = app.gpu_calls();
    let at = lines_in(&part2);
    pump(shell, app, Duration::from_secs(2));
    let again = app.gpu_calls() - failed;
    let after = lines_after(&part2, at);
    let blank = after.iter().all(|l| card_of(l) == ",,");
    let going = app.logging().rows;
    let gpu_told = errors(data, "видеокарта не прочитана (проверка)");
    app.sim_gpu(false);
    r.check(
        failed == calls + 1 && again == 0 && !after.is_empty() && blank && going && gpu_told == 1,
        &format!(
            "видеокарта не прочиталась — её столбцы пусты в {} строках после, попыток за 2 с {again} (шаг 0,4 с; повтор — через 10 с), строк в error.log {gpu_told}, журнал идёт",
            after.len()
        ),
    );

    // Мощность корпуса не прочиталась: пусто в строках, одна строка в error.log, журнал идёт.
    if let Some(sim) = app.sim() {
        sim.power_refused = true;
    }
    let at = lines_in(&part2);
    pump(shell, app, Duration::from_secs(1));
    let after = lines_after(&part2, at);
    let power_at = first
        .split(',')
        .position(|name| name == "power_cpu")
        .expect("столбец мощности");
    let no_power = after.iter().all(|l| field(l, power_at).is_empty());
    let going = app.logging().rows;
    let power_told = errors(data, "мощность корпуса не прочитана");
    if let Some(sim) = app.sim() {
        sim.power_refused = false;
    }
    r.check(
        !after.is_empty() && no_power && going && power_told == 1,
        &format!(
            "мощность корпуса не прочиталась — пусто в {} строках, строк в error.log {power_told}, журнал идёт",
            after.len()
        ),
    );

    app.command(shell, AUTO_ON); // выключить; журнал (он от автомодуля не зависит) останавливает retime ниже
    load(app, 0.0);
    app.retime(shell, was);
    // Шаг 13 начинает файл дня заново: оба файла — прочь (временная папка проверки; не удалились — удалятся с ней).
    let _ = fs::remove_file(&file);
    let _ = fs::remove_file(&part2);
}

/// Журналы автомодуля после срезов: строки действий со всеми условиями (бриф T), срезы с заголовком по всем блокам.
fn check_logs(r: &mut Report, app: &App, data: &Path, first: usize) {
    let blocks = app.topology().blocks.len();
    let log = fs::read_to_string(data.join("auto-log.jsonl")).unwrap_or_default();
    let cuts: Vec<serde_json::Value> = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["act"] == "target" && v["first"] == first)
        .collect();
    // Время, блок, с какого на какой потолок; медиана, по которой срезан; уровень кристалла; последние 2-с медианы всех
    // блоков; фактический множитель и загрузка блока; вентиляторы и видеокарта (строки журнала срезов пишутся —
    // видеокарта прочитана).
    let complete = cuts.iter().all(|v| {
        v["time"].is_string()
            && v["at"].is_i64()
            && v["block"].is_string()
            && v["from"].is_i64()
            && v["to"].as_i64() == v["from"].as_i64().map(|f| f - 1)
            && v["mean"].as_f64().is_some_and(|m| m > 75.0)
            && v["die"].is_f64()
            && v["means"].as_array().is_some_and(|a| a.len() == blocks)
            && v["ratio"].is_f64()
            && v["busy"] == 1.0
            && v["fans"] == serde_json::json!([3200, 3000])
            && v["gpu"]["temp"] == 61.0
            && v["limit"] == 75
            && v["emergency"] == 95
    });
    r.check(
        !cuts.is_empty() && complete,
        &format!(
            "журнал действий: {} целевых срезов блока со всеми условиями (медиана, кристалл, медианы {blocks} блоков, множитель и загрузка, вентиляторы, видеокарта); первая: {}",
            cuts.len(),
            cuts.first().map(ToString::to_string).unwrap_or_default()
        ),
    );
    let samples = fs::read_to_string(today(data)).unwrap_or_default();
    let mut lines = samples.lines();
    // Время, корпус, два вентилятора; по 4 на блок; мощность корпуса и видеокарта (DECISIONS #37); в конце — мощность
    // ядер. Напряжения блоков нет (владелец 16.10: его задаёт процессор от множителя и условий; в 0.5.4 оно было).
    let pack = 4 + 4 * blocks;
    let width = pack + 4 + 2;
    let head = lines.next().unwrap_or_default();
    let header = head.split(',').count();
    let rows: Vec<&str> = lines.collect();
    r.check(
        header == width
            && field(head, pack) == "power_cpu"
            && field(head, pack + 3) == "busy_gpu"
            && field(head, pack + 4) == "power_cores"
            && field(head, pack + 5) == "filtered_from"
            && !head.contains("volts")
            && !rows.is_empty()
            && rows.iter().all(|l| l.split(',').count() == width),
        &format!(
            "срезы: заголовок {header} столбцов из {width}, кончается «busy_gpu,power_cores,filtered_from», напряжения нет; строк {}",
            rows.len()
        ),
    );
    let timed = rows.iter().filter(|l| ms_of_day(field(l, 0)).is_some()).count();
    let fans = rows.iter().filter(|l| !field(l, 2).is_empty()).count();
    r.check(
        timed == rows.len() && fans > 0 && fans < rows.len(),
        &format!(
            "срезы: время с миллисекундами у {timed} строк из {}; обороты — в {fans} строках (где прочитаны), в остальных пусто",
            rows.len()
        ),
    );
    // Мощность корпуса — в каждой строке (у имитации она есть всегда); видеокарта — только где прочитана, и тогда все
    // три числа имитации.
    let power = rows.iter().filter(|l| !field(l, pack).is_empty()).count();
    let cards: Vec<String> = rows
        .iter()
        .map(|l| l.split(',').skip(pack + 1).take(3).collect::<Vec<_>>().join(","))
        .collect();
    let read = cards.iter().filter(|c| *c == SIM_GPU).count();
    let empty = cards.iter().filter(|c| *c == ",,").count();
    r.check(
        power == rows.len() && read > 0 && read < rows.len() && read + empty == rows.len(),
        &format!(
            "срезы: мощность корпуса — в {power} строках из {}; видеокарта ({SIM_GPU}) — в {read} (где прочитана), пусто — в {empty}",
            rows.len()
        ),
    );
    // Мощность ядер — в последнем столбце каждой строки; у имитации это мощность корпуса без 8 Вт его покоя
    // (machine sim.rs), а не повтор её.
    let cores_ok = rows
        .iter()
        .filter(|l| {
            let package = field(l, pack).parse::<f64>().ok();
            let cores = field(l, pack + 4).parse::<f64>().ok();
            matches!((package, cores), (Some(p), Some(c)) if (p - c - 8.0).abs() < 0.15)
        })
        .count();
    r.check(
        cores_ok == rows.len(),
        &format!(
            "срезы: мощность ядер — в {cores_ok} строках из {}, на 8 Вт меньше мощности корпуса",
            rows.len()
        ),
    );
}

/// Видеокарта имитации в строке среза: температура, мощность, занятость (app/sense.rs, `sim_gpu`).
const SIM_GPU: &str = "61,95.5,0.72";

/// Три поля видеокарты строки среза (температура, мощность, занятость) через запятую. Строка: время, корпус, два
/// вентилятора, по 4 на блок, мощность корпуса и видеокарта, мощность ядер — 9 + 4 × блоков полей.
fn card_of(line: &str) -> String {
    let fields: Vec<&str> = line.split(',').collect();
    let at = 4 + 4 * (fields.len().saturating_sub(9) / 4) + 1;
    fields.get(at..at + 3).map(|f| f.join(",")).unwrap_or_default()
}

/// Поле строки среза по номеру ("" — нет такого).
fn field(line: &str, k: usize) -> &str {
    line.split(',').nth(k).unwrap_or("")
}

/// Миллисекунды от полуночи по времени строки среза «гггг-мм-дд чч:мм:сс.ммм»; None — время не такого вида.
fn ms_of_day(time: &str) -> Option<i64> {
    if time.len() != 23 || time.as_bytes().get(19) != Some(&b'.') {
        return None;
    }
    let n = |at: std::ops::Range<usize>| time.get(at)?.parse::<i64>().ok();
    Some(((n(11..13)? * 60 + n(14..16)?) * 60 + n(17..19)?) * 1000 + n(20..23)?)
}

/// Строки сегодняшних срезов со временем позже `from` (оно — в том же виде, что у строк).
pub fn rows_since(data: &Path, from: &str) -> Vec<String> {
    fs::read_to_string(today(data))
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter(|l| field(l, 0) > from)
        .map(str::to_string)
        .collect()
}

/// Последняя строка цены опроса срезов в tray.log.
pub fn cost_line(app: &App) -> Option<String> {
    fs::read_to_string(app.journal().tray_path())
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|l| l.contains("срезы: опросов"))
        .map(str::to_string)
}

/// 11b. Журнал срезов на настоящих сроках (опрос 0,25 с, обороты раз в 2 с, решение раз в 2 с), окна нет: строки раз
/// в 0,25 с со временем до миллисекунд, обороты — в каждой восьмой строке, в остальных пусто; ядра читает только опрос
/// срезов — первое ядро каждого блока на опрос, автомодуль раз в 2 с сам не читает. Регулятор на этих сроках за
/// несколько секунд ничего не решает (окно 20 с). Только имитация: настоящие таймеры, но не настоящий процессор.
fn logging_real(r: &mut Report, app: &mut App, shell: &mut Shell, data: &Path, hot: &machine::topology::Block) {
    let blocks = u64::try_from(app.topology().cores.len()).unwrap_or(0);
    let was = app.retime(shell, AutoTiming::REAL);
    // Горячий блок занят целиком, остальные ядра — на 0,15: процессор занят ≥ 10 %, строки журнала пишутся.
    let set_load = |app: &mut App, hot_load: f64, rest: f64| {
        if let Some(sim) = app.sim() {
            for (core, load) in sim.load.iter_mut().enumerate() {
                *load = if hot.members.contains(&core) { hot_load } else { rest };
            }
        }
    };
    set_load(app, 1.0, 0.15);
    let from = Stamp::now().local(Pattern::Millis);
    app.command(shell, AUTO_ON);
    pump_until(shell, app, Duration::from_secs(5), |a| a.logging().rows);
    let (before, reads) = (app.logging(), app.reads());
    pump(shell, app, Duration::from_secs(6));
    let (after, reads_after) = (app.logging(), app.reads());
    // Выключатель автомодуля журнал не останавливает (владелец 16.10: два независимых выключателя): процессор занят —
    // строки идут дальше; перестал быть занят — их останавливает следующий опрос раз в 2 с, и опрос срезов (автомодуль
    // выключен) кончается вместе с ними.
    app.command(shell, AUTO_ON); // выключить
    let right_after = app.logging().rows;
    set_load(app, 0.0, 0.0);
    pump_until(shell, app, Duration::from_secs(5), |a| !a.logging().rows);
    let stopped = app.logging().on && !app.logging().rows;
    let cost = cost_line(app);
    app.retime(shell, was);
    r.check(
        before.on && before.rows && after.rows && after.starts == before.starts && right_after && stopped,
        "журнал срезов: строки пошли при занятом процессоре, шли без перерыва 6 с; выключение автомодуля их не остановило, отфильтрованы строки при низкой загрузке, опрос ядер продолжается",
    );
    let (polled, read) = (after.polls - before.polls, reads_after - reads);
    r.check(
        polled > 0 && read == polled * blocks,
        &format!(
            "журнал срезов без окна: {read} чтений ядер за {polled} опросов — по каждому из {blocks} ядер; автомодуль за это время сам не читал"
        ),
    );
    let rows = rows_since(data, &from);
    let times: Vec<i64> = rows.iter().filter_map(|l| ms_of_day(field(l, 0))).collect();
    let mut steps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
    steps.sort_unstable();
    let median = steps.get(steps.len() / 2).copied().unwrap_or(0);
    let (low, high) = (steps.first().copied().unwrap_or(0), steps.last().copied().unwrap_or(0));
    r.check(
        times.len() == rows.len() && rows.len() >= 20 && (240..=265).contains(&median) && high < 400,
        &format!(
            "строки срезов на настоящих сроках: {} строк, время с миллисекундами у {}; шаг — медиана {median} мс, от {low} до {high} мс",
            rows.len(),
            times.len()
        ),
    );
    let fan_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, l)| !field(l, 2).is_empty())
        .map(|(i, _)| i)
        .collect();
    let gaps: Vec<usize> = fan_rows.windows(2).map(|w| w[1] - w[0]).collect();
    // Первая строка с оборотами — одна из первых восьми: какая именно, зависит от того, когда обороты читались до
    // начала журнала (опрос автомодуля без журнала читает их раз в 10 с), а не от журнала (ревизия 15.10, п. 9).
    r.check(
        fan_rows.first().is_some_and(|&first| first < 8) && fan_rows.len() >= 2 && gaps.iter().all(|&g| g == 8),
        &format!(
            "обороты — в одной из первых восьми строк и дальше в каждой восьмой (строки {fan_rows:?} из {}), в остальных пусто",
            rows.len()
        ),
    );
    // Видеокарта — раз в 2 с, как обороты: в одной из первых восьми строк и дальше в каждой восьмой.
    let card_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, l)| card_of(l) == SIM_GPU)
        .map(|(i, _)| i)
        .collect();
    let card_gaps: Vec<usize> = card_rows.windows(2).map(|w| w[1] - w[0]).collect();
    r.check(
        card_rows.first().is_some_and(|&first| first < 8)
            && card_rows.len() >= 2
            && card_gaps.iter().all(|&g| g == 8)
            && rows.iter().all(|l| {
                let card = card_of(l);
                card == SIM_GPU || card == ",,"
            }),
        &format!(
            "видеокарта — в одной из первых восьми строк и дальше в каждой восьмой (строки {card_rows:?} из {}), в остальных пусто",
            rows.len()
        ),
    );
    let line = cost.unwrap_or_default();
    let parts = |what: &str| {
        line.split(what)
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
    };
    r.check(
        parts("мощность корпуса — ") > 0 && parts("видеокарта — ") > 0,
        &format!("цена опроса срезов в tray.log (имитация), с мощностью корпуса и видеокартой: {line}"),
    );
}

/// Доля работы всех ядер имитации — так её видит опрос раз в 2 с на имитации вместо Windows (app/auto.rs,
/// `system_share`).
fn sim_share(app: &mut App) -> f64 {
    app.sim().map_or(0.0, |s| {
        let cores = f64::from(u32::try_from(s.load.len().max(1)).unwrap_or(u32::MAX));
        s.load.iter().sum::<f64>() / cores
    })
}

/// 11c. Два выключателя — журнал срезов и автомодуль — независимы (владелец 16.10: «отдельно какой-то выключатель для
/// логирования и отдельно выключатель для автоподстройки… это независимый процесс»). Все четыре сочетания; смена
/// одного выключателя не меняет другой. Сроки проверки (опрос и срезы — 50 мс, окно регулятора 0,4 с); окна нет.
/// Горячий блок — тот же, что в шаге 11 (под нагрузкой выше отсечки 75 °C); «процессор занят» у имитации — доля
/// работы её ядер: остальным ядрам — 0,15.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
fn switches(r: &mut Report, app: &mut App, shell: &mut Shell, data: &Path, hot: &machine::topology::Block) {
    let first = hot.first;
    let base = app.intended().and_then(|i| i.get(first)).copied().unwrap_or(0);
    let marker = data.join("auto-held.json");
    let settings = data.join("tray.json");
    let file = today(data);
    let actions = || lines_in(&data.join("auto-log.jsonl"));
    let no_fields = serde_json::Map::new;
    let set_load = |app: &mut App, hot_load: f64, rest: f64| {
        if let Some(sim) = app.sim() {
            for (core, load) in sim.load.iter_mut().enumerate() {
                *load = if hot.members.contains(&core) { hot_load } else { rest };
            }
        }
    };
    let ceiling = |app: &mut App| ceilings_now(app).get(first).copied().unwrap_or(0);
    let asks = |app: &mut App| app.sim().map_or(0, |s| s.reads);
    let writes = |app: &mut App| app.sim().map_or(0, |s| s.writes);
    let was = app.auto_choice();
    r.check(
        !was.on && was.log && app.logging().on && !app.logging().rows,
        &format!("перед шагом: автомодуль выключен, журнал включён и стоит ({was:?})"),
    );

    // А. Журнал включён, автомодуль выключен, процессор занят, блок выше отсечки: журнал пишется, потолки не меняются.
    set_load(app, 1.0, 0.15);
    let busy = sim_share(app) * 100.0;
    let heat = app
        .sim()
        .and_then(|s| s.block(first).ok())
        .and_then(|reading| reading.temp)
        .unwrap_or(f64::NAN);
    let (wrote, ceilings, acts, rows) = (writes(app), ceilings_now(app), actions(), lines_in(&file));
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().rows);
    let starts = app.logging().starts;
    pump(shell, app, Duration::from_millis(1500));
    let written = lines_in(&file).saturating_sub(rows);
    r.check(
        app.logging().rows && app.logging().starts == starts && written >= 10,
        &format!(
            "журнал включён, автомодуль выключен, процессор занят (имитация: {busy:.0} %): журнал срезов пишется — {written} строк за 1,5 с"
        ),
    );
    r.check(
        heat > 75.0 && writes(app) == wrote && ceilings_now(app) == ceilings && !marker.exists() && actions() == acts,
        &format!(
            "автомодуль выключен: блок {} при {heat:.1} °C (отсечка 75) не опущен — ни записи в процессор, ни метки, ни строки в журнале действий",
            hot.name
        ),
    );

    // Б. Оба включены, автомодуль — вкл → выкл → вкл, процессор занят: регулятор опускает блок (те же условия, решает
    //    выключатель), выключение возвращает его; журнал не останавливается, его выключатель не меняется.
    app.command(shell, AUTO_ON);
    pump_until(shell, app, Duration::from_secs(5), |a| {
        ceilings_now(a).get(first).is_some_and(|&c| c < base)
    });
    let lowered = ceiling(app);
    let mut going = app.logging().on;
    let events = data
        .join("events")
        .join(format!("{}.jsonl", Stamp::now().local(Pattern::Date)));
    let events_from = lines_in(&events);
    app.command(shell, AUTO_ON);
    let back = ceiling(app);
    let putbacks: Vec<serde_json::Value> = lines_after(&events, events_from)
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["kind"] == "putback" && v["block"] == hot.name.as_str())
        .collect();
    r.check(
        putbacks.len() == 1
            && putbacks[0]["reason"] == "auto-off"
            && putbacks[0]["from"] == lowered
            && putbacks[0]["to"] == base
            && putbacks[0]["period"]["ceiling"] == lowered,
        &format!(
            "журнал событий: выключение автомодуля — строка putback (reason auto-off) с итогом на покидаемом ×{lowered}: {}",
            putbacks.first().map(ToString::to_string).unwrap_or_default()
        ),
    );
    going &= app.logging().on;
    pump(shell, app, Duration::from_millis(500));
    going &= app.logging().on;
    let log_kept = app.auto_choice().log;
    app.command(shell, AUTO_ON);
    pump(shell, app, Duration::from_millis(500));
    going &= app.logging().on;
    r.check(
        lowered < base && back == base,
        &format!(
            "оба включены: тот же блок опущен ×{base} → ×{lowered} (условия те же — решает выключатель автомодуля); выключили — вернулся ×{back}"
        ),
    );
    r.check(
        going && app.logging().starts == starts && log_kept && app.auto_choice().log && app.auto_choice().on,
        &format!(
            "автомодуль вкл → выкл → вкл при занятом процессоре: журнал срезов не останавливался (начинался {} раз за шаг), выключатель журнала не тронут",
            app.logging().starts - starts + 1
        ),
    );

    // В. Журнал выключен, автомодуль включён: строки и файл дня кончаются сразу; опрос срезов идёт дальше — он нужен
    //    регулятору (бриф T: один путь чтения), и регулятор опускает блок по его показаниям, хотя процессор не занят.
    //    Отсечка — 60 °C: у профиля владельца самый высокий потолок бывает ×44 (76 °C под нагрузкой у имитации), и до
    //    75 °C ему одна ступень — её регулятор сделал в шаге Б (s24a); при 60 °C ему есть куда опускать. Новая отсечка
    //    возвращает блок к принятому, регулятор начинает заново.
    set_load(app, 1.0, 0.0);
    let (polls, rows) = (app.logging().polls, lines_in(&file));
    app.command(shell, LOG_ID);
    let stopped = !app.logging().rows && app.logging().on;
    let choice = app.auto_choice();
    let mut low = no_fields();
    low.insert("limit".into(), 60.into());
    let refused = app.set_auto(shell, &low);
    let (reads, acts) = (app.reads(), actions());
    pump_until(shell, app, Duration::from_secs(5), |_| actions() > acts);
    pump(shell, app, Duration::from_millis(300));
    let file_text = fs::read_to_string(&settings).unwrap_or_default();
    r.check(
        refused.is_none()
            && stopped
            && choice.on
            && !choice.log
            && file_text.contains("\"auto\":true")
            && file_text.contains("\"log\":false"),
        &format!("журнал выключен — строки и файл дня кончились сразу, опрос срезов идёт (он нужен автомодулю); автомодуль остался включён (tray.json {file_text})"),
    );
    r.check(
        actions() > acts
            && ceiling(app) < base
            && app.reads() > reads
            && app.logging().on
            && !app.logging().rows
            && app.logging().polls > polls
            && lines_in(&file) == rows,
        &format!(
            "журнал выключен, автомодуль включён, процессор не занят: регулятор опускает блок (×{}) по показаниям опроса срезов ({} опросов, {} чтений ядер), строк нет",
            ceiling(app),
            app.logging().polls - polls,
            app.reads() - reads
        ),
    );

    // Г. Оба выключены: опроса нет вовсе — даже при занятом процессоре; драйвер отпущен; опущенный блок вернулся.
    set_load(app, 1.0, 0.15);
    let released = app.releases();
    app.command(shell, AUTO_ON);
    let back = ceiling(app);
    let choice = app.auto_choice();
    // Сообщение таймера, уже стоявшее в очереди, ещё может прийти: счёт — после него.
    pump(shell, app, Duration::from_millis(100));
    let (ticks, reads, asked, wrote) = (app.auto_ticks(), app.reads(), asks(app), writes(app));
    pump(shell, app, Duration::from_secs(1));
    r.check(
        back == base
            && !choice.on
            && !choice.log
            && app.releases() > released
            && app.auto_ticks() == ticks
            && app.reads() == reads
            && asks(app) == asked
            && writes(app) == wrote
            && !app.logging().on,
        &format!(
            "оба выключены: процессор занят ({busy:.0} %), а опроса нет — за 1 с ни сообщения таймера, ни чтения датчиков и потолков, ни записи; драйвер отпущен; блок вернулся к ×{back}, журнал остался выключен"
        ),
    );

    // Д. Журнал включён, автомодуль выключен: процессор занят — журнал пишется; стал не занят — журнал стоит, драйвер
    //    отпущен, опрос идёт, но датчиков и потолков не читает.
    app.command(shell, LOG_ID);
    let choice = app.auto_choice();
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().on);
    let restarted = app.logging().on;
    set_load(app, 0.0, 0.0);
    pump_until(shell, app, Duration::from_secs(2), |a| !a.logging().rows);
    let stopped = app.logging().on && !app.logging().rows;
    let (ticks, reads, asked, released, polls) = (
        app.auto_ticks(),
        app.reads(),
        asks(app),
        app.releases(),
        app.logging().polls,
    );
    pump(shell, app, Duration::from_secs(1));
    let polled = app.auto_ticks() - ticks;
    r.check(
        !choice.on
            && choice.log
            && restarted
            && stopped
            && polled > 0
            && app.reads() > reads
            && asks(app) >= asked
            && app.releases() == released
            && app.logging().polls > polls,
        &format!(
            "журнал включён, автомодуль выключен: прохладный простой отфильтрован; опрос всех ядер продолжается ({polled} контрольных сообщений за 1 с)"
        ),
    );
    // Отсечка — как была до шага В.
    let mut back = no_fields();
    back.insert("limit".into(), was.limit.into());
    let refused = app.set_auto(shell, &back);
    r.check(
        refused.is_none() && app.auto_choice() == was,
        &format!("выбор после шага — как до него: {:?}", app.auto_choice()),
    );
}

/// Файл сегодняшних срезов.
fn today(data: &Path) -> std::path::PathBuf {
    data.join("samples")
        .join(format!("{}.csv", Stamp::now().local(Pattern::Date)))
}

/// Строк в файле (0 — файла нет или он не читается).
fn lines_in(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |t| t.lines().count())
}

/// Шаг 13 — журнал срезов, когда что-то не так (ревизия outputs/review-sampling-2026-10-15.md, пункты 3, 4, 7, 8):
/// файл дня удалён и перемещён посреди записи, обороты отказывают, потолки не читаются, файл дня не открывается.
/// Сроки: опрос срезов 50 мс, обороты и сверка файла дня — раз в 0,4 с, опрос автомодуля 0,2 с; регулятор — на
/// настоящих сроках и за эти секунды ничего не решает. Окна нет.
#[expect(
    clippy::too_many_lines,
    reason = "one scenario, step after step, reads best as one piece"
)]
fn logging_failures(r: &mut Report, app: &mut App, shell: &mut Shell, data: &Path) {
    let was = app.retime(
        shell,
        AutoTiming {
            tick: Duration::from_millis(200),
            sample: Duration::from_millis(50),
            fans: Duration::from_millis(400),
            gpu: Duration::from_millis(400),
            control: machine::auto::Timing::REAL,
        },
    );
    let now = ceilings_now(app);
    let Some(hot) = app
        .topology()
        .blocks
        .iter()
        .max_by_key(|b| now.get(b.first).copied().unwrap_or(0))
        .cloned()
    else {
        r.check(false, "журнал срезов, отказы: блоков нет");
        return;
    };
    // Горячий блок — `value`; пока он занят, остальные ядра — на 0,15: процессор занят ≥ 10 %, строки журнала пишутся
    // (DECISIONS #32, подзапись 16.10; автомодуль строк не пишет — бриф T).
    let load = |app: &mut App, value: f64| {
        if let Some(sim) = app.sim() {
            let rest = if value > 0.0 { 0.15 } else { 0.0 };
            for (core, load) in sim.load.iter_mut().enumerate() {
                *load = if hot.members.contains(&core) { value } else { rest };
            }
        }
    };
    let file = today(data);
    load(app, 1.0);
    app.command(shell, AUTO_ON);
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().rows);
    // Файл открыт журналом, только когда в него легла строка этого журнала: удалить его раньше — значит проверить
    // открытие нового, а не удаление открытого (так первая версия шага прошла на коде до исправления).
    let size = fs::metadata(&file).map_or(0, |m| m.len());
    pump_until(shell, app, Duration::from_secs(1), |_| {
        fs::metadata(&file).is_ok_and(|m| m.len() > size)
    });

    // п. 8: файл дня удалён — строки не уходят в удалённый файл, начинается новый с заголовком.
    let removed = fs::remove_file(&file).is_ok();
    pump_until(shell, app, Duration::from_secs(2), |_| lines_in(&file) >= 3);
    let text = fs::read_to_string(&file).unwrap_or_default();
    r.check(
        removed && text.starts_with("time,") && text.lines().count() >= 3,
        &format!(
            "файл срезов удалён посреди записи — начат новый, с заголовком ({} строк)",
            text.lines().count()
        ),
    );
    // п. 8: файл дня перемещён — в перемещённый больше не пишется, строки идут в новый файл дня.
    let moved = file.with_file_name("moved.csv");
    let renamed = fs::rename(&file, &moved).is_ok();
    pump_until(shell, app, Duration::from_secs(2), |_| lines_in(&file) >= 2);
    let left = fs::metadata(&moved).map_or(0, |m| m.len());
    pump(shell, app, Duration::from_millis(500));
    let still = fs::metadata(&moved).map_or(0, |m| m.len());
    r.check(
        renamed && lines_in(&file) >= 2 && still == left,
        &format!(
            "файл срезов перемещён — строки идут в новый файл дня, в перемещённый больше нет ({left} → {still} байт)"
        ),
    );
    let _ = fs::remove_file(&moved); // временная папка проверки: не удалился — удалится с ней

    // п. 4: обороты отказали — следующая попытка через 10 с (как до 0.5.2), а не в шаге журнала: каждая попытка у
    // настоящей прошивки может держать цикл программы до 4 с.
    app.sim_fans(true);
    let calls = app.fan_calls();
    pump_until(shell, app, Duration::from_secs(2), |a| a.fan_calls() > calls);
    let failed = app.fan_calls();
    pump(shell, app, Duration::from_secs(2));
    let again = app.fan_calls() - failed;
    let going = app.logging().rows;
    app.sim_fans(false);
    r.check(
        failed == calls + 1 && again == 0 && going,
        &format!(
            "обороты отказали посреди журнала — за 2 с после отказа попыток {again} (шаг оборотов 0,4 с; повтор — через 10 с), журнал идёт"
        ),
    );

    // п. 3: потолки не читаются — строк нет (столбец потолков был бы не прочитан), опроса срезов нет (он открывал бы
    // драйвер, который опрос автомодуля после ошибки отпускает); прочитались снова — журнал пишется.
    let deaf = app
        .topology()
        .blocks
        .iter()
        .find(|b| b.first != hot.first)
        .map_or(0, |b| b.first);
    if let Some(sim) = app.sim() {
        sim.set_deaf(deaf, true);
    }
    pump_until(shell, app, Duration::from_secs(2), |a| !a.logging().on);
    let (stopped, polls, rows) = (!app.logging().on, app.logging().polls, lines_in(&file));
    pump(shell, app, Duration::from_secs(1));
    let quiet = !app.logging().on && app.logging().polls == polls && lines_in(&file) == rows;
    if let Some(sim) = app.sim() {
        sim.set_deaf(deaf, false);
    }
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().rows);
    r.check(
        stopped && quiet && app.logging().rows,
        "потолки не читаются — журнал срезов стоит (ни опроса, ни строк), прочитались снова — пишется",
    );

    // п. 7: файл дня не открывается — одна попытка и одна строка в error.log, следующая попытка через 10 с, а не в
    // каждом опросе. Строки стоят и файл закрыт — выключателем журнала (автомодуль его не останавливает, владелец
    // 16.10); автомодуль остаётся включённым — опрос срезов идёт, строки начнёт снова опрос раз в 2 с.
    app.command(shell, LOG_ID); // выключить журнал: строки и файл дня кончаются сразу
    let _ = fs::remove_file(&file); // файла может и не быть: на его месте нужна папка
    let blocked = fs::create_dir(&file).is_ok();
    let opens = app.logging().opens;
    app.command(shell, LOG_ID);
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().rows);
    let polls = app.logging().polls;
    pump(shell, app, Duration::from_millis(1500));
    let (tries, polled) = (app.logging().opens - opens, app.logging().polls - polls);
    app.command(shell, AUTO_ON); // выключить
    let told = fs::read_to_string(data.join("error.log"))
        .unwrap_or_default()
        .matches("срез не записан")
        .count();
    let _ = fs::remove_dir(&file); // временная папка проверки, как выше
    r.check(
        blocked && polled >= 10 && tries == 1 && told == 1,
        &format!(
            "файл дня не открывается — попыток {tries} за {polled} опросов, строк в error.log {told}; следующая — через 10 с"
        ),
    );
    load(app, 0.0);
    app.retime(shell, was);
}

/// Один занятый LP внутри E-кластера включает запись; фильтрация связывает точные крайние строки.
fn filtered_capture(r: &mut Report, app: &mut App, shell: &mut Shell, data: &Path) {
    let Some(core) = app
        .topology()
        .blocks
        .iter()
        .find(|b| b.members.len() > 1)
        .and_then(|b| b.members.get(1))
        .copied()
    else {
        r.check(false, "проверке отбора нужен E-кластер");
        return;
    };
    let forced = app.sim().map(|sim| sim.forced.clone()).unwrap_or_default(); // copy once, restore scenario input
    let choose = |app: &mut App, busy: f64| {
        if let Some(sim) = app.sim() {
            sim.forced.fill(Some(45.0));
            sim.load.fill(0.0);
            sim.load[core] = busy;
        }
    };
    choose(app, 0.6);
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().rows);
    pump(shell, app, Duration::from_millis(200));
    let file = today(data);
    let old = fs::read_to_string(&file).unwrap_or_default();
    let anchor = old.lines().last().map(|l| field(l, 0).to_owned()).unwrap_or_default();
    choose(app, 0.1);
    pump_until(shell, app, Duration::from_secs(2), |a| !a.logging().rows);
    let count = lines_in(&file);
    pump(shell, app, Duration::from_millis(300));
    let held = count == lines_in(&file) && app.logging().on;
    choose(app, 0.6);
    pump_until(shell, app, Duration::from_secs(2), |a| a.logging().rows);
    let text = fs::read_to_string(&file).unwrap_or_default();
    let proof = text.lines().skip(count).find(|l| field(l, 0) != anchor);
    r.check(held && proof.is_some_and(|l| l.split(',').next_back() == Some(anchor.as_str())),
        "один непервый LP E-кластера включает запись; низкая нагрузка фильтруется, после неё точная ссылка на предыдущую строку");
    choose(app, 0.0);
    if let Some(sim) = app.sim() {
        sim.forced = forced;
    }
    pump_until(shell, app, Duration::from_secs(2), |a| !a.logging().rows);
}
