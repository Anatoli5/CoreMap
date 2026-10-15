// Скрытая проверка программы входа: CoreMapTray.exe --simulate --snapshot <отчёт> [--saved]. Значка на экране нет,
// прав администратора не нужно, процессор — имитация, данные — во временной папке, все метки Windows — с приставкой
// «.Check.<номер процесса>»: настоящие части CoreMap проверку не видят и она их не видит. Окно, меню, сообщения,
// таймеры и метки — настоящие, Windows. Проходит каждую строку правила «кто когда пишет потолки» (DECISIONS #31).
// Первая строка отчёта — «ok» или «FAILED»; tools\check.ps1 смотрит на неё.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use machine::clock::Stamp;
use machine::events::{Event, EventLog};
use machine::named::{self, Names};
use machine::thermal::Thermal;

use crate::app::{AUTO_ON, App, Config, DEPTH_ID, EXIT_ID, FACTORY_ID, LIMIT_ID, NEW_ID, OPEN_ID};
use crate::shell::{self, Item, Menu, Shell};

/// Журнал без сбоев: проверка не зависит от истории машины.
#[derive(Debug)]
struct QuietLog;

impl EventLog for QuietLog {
    fn first(&self, _since: Stamp, _condition: &str) -> Result<Option<Event>, String> {
        Ok(None)
    }
}

struct Report {
    lines: Vec<String>,
    failed: bool,
}

impl Report {
    fn check(&mut self, ok: bool, what: &str) {
        self.lines
            .push(format!("{} {what}", if ok { "ok    " } else { "FAILED" }));
        self.failed |= !ok;
    }

    fn note(&mut self, text: &str) {
        self.lines.push(format!("       {text}"));
    }
}

fn pump(shell: &mut Shell, app: &mut App, for_: Duration) {
    shell::run(shell, app, Some(Instant::now() + for_));
}

/// Прокручивает цикл, пока не выполнится условие, но не дольше `limit`. Фиксированная пауза под нагрузкой полной
/// проверки (дюжина процессов рядом) оказалась мала: конец «окна» доходил позже 0,9 с (outputs\s5u, 14.10).
fn pump_until(shell: &mut Shell, app: &mut App, limit: Duration, done: impl Fn(&mut App) -> bool) {
    let deadline = Instant::now() + limit;
    while !done(app) && Instant::now() < deadline {
        pump(shell, app, Duration::from_millis(50));
    }
}

/// Сколько раз строка встречается в tray.log: шаг ждёт свою запись, а не только снятие метки — иначе действие,
/// отложенное на паузу (возврат права, сверка после пробуждения), срабатывает уже в следующем шаге и путает его.
fn logged(app: &App, needle: &str) -> usize {
    fs::read_to_string(app.journal().tray_path())
        .unwrap_or_default()
        .matches(needle)
        .count()
}

fn ceilings_now(app: &mut App) -> Vec<i32> {
    app.sim()
        .map(|s| s.current().iter().map(|&c| i32::from(c)).collect())
        .unwrap_or_default()
}

/// Держит мьютекс в своём потоке, пока не скажут; потом поток кончается, не отпустив его, — как закрытая C#-часть.
fn hold(name: &str) -> mpsc::Sender<()> {
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

/// Прокручивает цикл, пока потолок ядра не перестанет меняться (1,5 с без изменений, не дольше 10 с); его значение.
fn settle(shell: &mut Shell, app: &mut App, core: usize) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(10);
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
        grace: Duration::from_millis(300),
        wake_delay: Duration::from_millis(300),
        lease_wait: 500,
        wake_gap: Duration::from_millis(500),
        // Сроки автомодуля в проверке укорочены: опрос 50 мс, окно 0,4 с, пауза 0,3 с, прохлада 1 с.
        auto: crate::app::AutoTiming {
            tick: Duration::from_millis(50),
            control: machine::auto::Timing {
                window: Duration::from_millis(400),
                settle: Duration::from_millis(300),
                release: Duration::from_secs(1),
            },
        },
    };
    let names = config.names.clone();
    let result = App::new(config, Box::new(QuietLog))
        .and_then(|app| Shell::new(&format!("CoreMap.Logon.Check.{pid}"), false).map(|shell| (app, shell)));
    match result {
        Ok((mut app, mut shell)) => scenario(&mut r, &mut app, &mut shell, &names, &data),
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
fn scenario(r: &mut Report, app: &mut App, shell: &mut Shell, names: &Names, data: &Path) {
    // 0. Метка автомодуля от прежней загрузки (процессор с тех пор сброшен): по ней ничего не поднимается, даже
    //    если в ней записано выше заводского (ревизия H1).
    let lps: Vec<u32> = app.topology().lps().iter().map(|lp| lp.0).collect();
    let stale = format!(
        "{{\"cpu\": {}, \"lps\": {}, \"accepted\": {}, \"lowered\": {}}}",
        machine::files::quote(app.store().cpu()),
        machine::files::ints(&lps),
        machine::files::ints(&vec![60; lps.len()]),
        machine::files::ints(&vec![40; lps.len()])
    );
    let _ = fs::write(data.join("auto-held.json"), stale); // не записалась — проверка ниже это покажет
    let start = Instant::now();
    let before = shell::usage();
    app.start(shell);
    let raised = ceilings_now(app).contains(&60);
    r.check(
        !raised && !data.join("auto-held.json").exists(),
        "метка автомодуля от прежней загрузки ничего не подняла и снята",
    );
    let started = shell::usage();
    r.note(&format!("start {} ms", start.elapsed().as_millis()));

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
                &format!("профиль запуска «{}» поставлен при входе", p.name),
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
                seen.iter().any(|i| i.id == OPEN_ID && i.default),
                "по умолчанию — «Открыть прежнее окно»",
            );
            r.check(seen.iter().any(|i| i.id == EXIT_ID && i.enabled), "«Выход» доступен");
        }
        Err(e) => r.check(false, &e),
    }

    // 3. Щелчок по другому набору: он ставится и становится профилем запуска.
    let target = sets(&items)
        .into_iter()
        .find(|i| !i.checked && i.id > FACTORY_ID)
        .map_or(FACTORY_ID, |i| i.id);
    let writes = app.sim().map_or(0, |s| s.writes);
    app.command(shell, target);
    let after = app.menu();
    let now_checked = sets(&after).into_iter().find(|i| i.checked).map(|i| i.id);
    r.check(
        now_checked == Some(target),
        &format!("после щелчка отмечен выбранный набор ({target})"),
    );
    let wrote = app.sim().map_or(0, |s| s.writes) > writes;
    r.check(wrote, "потолки записаны в процессор");
    match app.store().read_profile() {
        Ok(Some(p)) if target > FACTORY_ID => {
            let text = after
                .iter()
                .find(|i| i.id == target)
                .and_then(|i| i.text.clone())
                .unwrap_or_default();
            r.check(
                text.starts_with(&p.name),
                &format!("набор «{}» стал профилем запуска", p.name),
            );
        }
        Ok(None) if target == FACTORY_ID => r.check(true, "«заводские»: профиля запуска больше нет"),
        other => r.check(false, &format!("профиль запуска после щелчка: {other:?}")),
    }
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
        grey.first().and_then(|i| i.text.as_deref()) == Some("окно CoreMap открыто — профили переключаются в нём"),
        "пояснение в меню: окно открыто",
    );
    let writes = app.sim().map_or(0, |s| s.writes);
    let reads = app.sim().map_or(0, |s| s.reads);
    let refused = app.switch("");
    r.check(
        refused.as_deref().is_some_and(|t| t.contains("окно CoreMap открыто")),
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
    let launched = app.launched.len();
    app.command(shell, NEW_ID);
    r.check(
        app.launched.len() == launched && app.said.iter().any(|s| s.contains("Сначала закрой прежнее окно")),
        "«Новое окно» при открытом прежнем — отказ",
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

    // 5. Движок с живым Core: то же, что окно; «Прежнее окно» — отказ.
    let engine = hold(&names.engine);
    let core = exist(&names.engine_core);
    pump(shell, app, Duration::from_millis(50));
    let launched = app.launched.len();
    app.command(shell, OPEN_ID);
    r.check(
        app.launched.len() == launched,
        "«Прежнее окно» при работающей настройке — не запускается",
    );
    r.check(
        app.said.iter().any(|s| s.contains("Прежнее окно сейчас не открыть")),
        "и говорит почему",
    );
    r.check(app.tip() == "CoreMap — идёт настройка", "подсказка: идёт настройка");
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

    // 9. Запуск C#-частей (в проверке только записывается).
    app.command(shell, OPEN_ID);
    app.command(shell, NEW_ID);
    r.note(&format!("запуски: {:?}", app.launched));
    r.check(
        app.launched.iter().any(|l| l == "CoreMap.exe --simulate"),
        "«Открыть прежнее окно» запускает CoreMap.exe",
    );
    r.check(
        app.launched
            .iter()
            .any(|l| l == "CoreMap.exe --simulate --tray --bridge --open"),
        "«Новое окно» запускает движок на время настройки",
    );

    let log = fs::read_to_string(app.journal().tray_path()).unwrap_or_default();
    for line in log.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev() {
        r.note(&format!("tray.log: {line}"));
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

    // 11. Автомодуль (DECISIONS #31): нагруженный горячий блок опускается, пока не остынет до предела, но не глубже
    //     выбранного, держа аренду и метку; стартующему прежнему окну возвращает принятые и отпускает аренду; остыв —
    //     возвращается по одной ступени.
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
    let lowered = format!("автомодуль: блок {} возвращён", block.name);
    let first = block.first;
    let marker = data.join("auto-held.json");
    app.command(shell, AUTO_ON);
    app.command(shell, LIMIT_ID); // 75 °C
    app.command(shell, DEPTH_ID + 1); // 4 ступени
    let settings = fs::read_to_string(data.join("tray.json")).unwrap_or_default();
    r.check(
        settings.contains("\"auto\":true") && settings.contains("\"limit\":75") && settings.contains("\"depth\":4"),
        "автомодуль: выбор записан в tray.json",
    );
    let base = accepted.get(first).copied().unwrap_or(0);
    let floor = base - 4;
    if let Some(sim) = app.sim() {
        for &m in &block.members {
            sim.load[m] = 1.0;
        }
    }
    let lowest = settle(shell, app, first);
    let heat = app
        .sim()
        .and_then(|s| s.block(first).ok())
        .and_then(|r| r.temp)
        .unwrap_or(f64::NAN);
    r.check(
        lowest < base && lowest >= floor && (heat <= 75.0 || lowest == floor),
        &format!(
            "автомодуль: горячий блок {} опущен ×{base} → ×{lowest} ({heat:.1} °C при пределе 75, не глубже ×{floor})",
            block.name
        ),
    );
    let now = ceilings_now(app);
    r.check(
        now.iter()
            .zip(&accepted)
            .enumerate()
            .all(|(i, (c, a))| block.members.contains(&i) || c == a),
        "автомодуль: остальные блоки не тронуты",
    );
    r.check(marker.exists(), "автомодуль: метка с принятыми потолками записана");
    r.check(
        !lease_free(&names.lease),
        "автомодуль: пока блок опущен, аренда держится",
    );
    let tip = app.tip();
    r.check(
        tip.contains(&format!("авто: до 75 °C · {} −{}", block.name, base - lowest)),
        &format!("автомодуль: подсказка ({})", tip.replace('\n', " / ")),
    );
    let window = hold(&names.old_window);
    pump_until(shell, app, Duration::from_secs(3), |a| {
        ceilings_now(a).get(first) == Some(&base)
    });
    r.check(
        ceilings_now(app).get(first) == Some(&base),
        "автомодуль: прежнему окну — принятые потолки",
    );
    r.check(
        !marker.exists() && lease_free(&names.lease),
        "автомодуль: метка снята, аренда отпущена",
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
    if let Some(sim) = app.sim() {
        for &m in &block.members {
            sim.load[m] = 0.0;
        }
    }
    let raised = logged(app, &lowered);
    pump_until(shell, app, Duration::from_secs(12), |a| {
        ceilings_now(a).get(first) == Some(&base)
    });
    r.check(
        ceilings_now(app).get(first) == Some(&base),
        "автомодуль: остывший блок вернулся к принятому",
    );
    let steps = logged(app, &lowered) - raised;
    r.check(
        usize::try_from(base - again).is_ok_and(|n| n == steps),
        &format!("автомодуль: по одной ступени ({steps} из ×{again} в ×{base})"),
    );
    r.check(
        !marker.exists() && lease_free(&names.lease),
        "автомодуль: всё вернулось — метки нет, аренда свободна",
    );
    app.command(shell, AUTO_ON); // выключить: дальше меряется покой без опроса

    // 10. Цена: память и процессор в покое.
    let idle_from = shell::usage();
    pump(shell, app, Duration::from_secs(3));
    let idle_to = shell::usage();
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
        "в покое процессор не тратится (меньше 20 мс за 3 с)",
    );
}
