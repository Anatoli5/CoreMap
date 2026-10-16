// Программа входа CoreMap: CoreMapTray.exe, значок у часов (DECISIONS #30, #31, #32). При входе в Windows ставит
// профиль потолков по правилам защиты, из меню значка переключает профили, после сна сверяет потолки, по выбору
// владельца ограничивает нагрев (автомодуль). Окно CoreMapWindow.exe открывает она же («Окно» в меню, щелчок по значку)
// и обслуживает его по каналу: показания, потолки, профили (app/serve.rs). Замеров и кривых в ней нет; прежнее окно на
// C# — отдельная программа, которую владелец запускает сам.
//
//   CoreMapTray.exe                        значок; без прав администратора — спрашивает их у Windows (запрос на экране)
//   CoreMapTray.exe --simulate --snapshot F [--saved]
//                                          скрытая проверка (src/check.rs): без значка и прав, процессор — имитация,
//                                          данные — во временной папке (--saved — копия профилей из data\); отчёт в F
//   CoreMapTray.exe --probe                ничего не делает и выходит: пустил ли Windows свежую сборку (tools\rust.ps1)
//
// --simulate без --snapshot не запускается: имитация писала бы в настоящие файлы профилей.
#![windows_subsystem = "windows"]

mod app;
mod check;
mod check_window;
mod fans;
mod gpu;
mod journal;
mod launch;
mod layout;
mod pipe;
mod shell;

use std::path::PathBuf;
use std::time::Duration;

use machine::named::{self, Names};

use crate::app::{App, Config, Confirm};
use crate::shell::{Handler, Loop, Shell};

/// Номер версии (файл VERSION, tools\rust.ps1) — его владелец видит в подсказке значка и называет.
pub const VERSION: &str = env!("COREMAP_VERSION");
/// Время сборки (tools\rust.ps1); после «|» — метка, которая только делает файл другим при пересборке.
pub const BUILT: &str = env!("COREMAP_BUILT");
/// Канал окна: к имени добавляется номер сеанса Windows (`pipe::session_pipe`). Своё имя, не канал движка на C#
/// («CoreMap.Bridge.N»): движок, запущенный кнопкой прежнего окна, не должен столкнуться с программой входа.
const PIPE: &str = "CoreMap.Logon";

impl Handler for App {
    fn waits(&self) -> Vec<windows_sys::Win32::Foundation::HANDLE> {
        App::waits(self)
    }

    fn on(&mut self, shell: &mut Shell, what: Loop) {
        App::on(self, shell, what);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |key: &str| args.iter().any(|a| a == key);
    // Пробный запуск после сборки: только узнать, пустил ли Windows файл. Раньше всего, в том числе прав.
    if has("--probe") {
        return;
    }
    let Some(root) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    else {
        std::process::exit(1);
    };
    let snapshot = args
        .iter()
        .position(|a| a == "--snapshot")
        .and_then(|i| args.get(i + 1));
    if has("--simulate") {
        let Some(report) = snapshot else { std::process::exit(1) };
        std::process::exit(check::run(&root, &PathBuf::from(report), has("--saved")));
    }
    if !shell::elevated() {
        if has("--elevated") {
            // Уже перезапущен с правами, а их нет: второй раз не спрашивать, только записать.
            journal::Journal::new(&app::data_dir(&root)).error("программа входа: права администратора не получены");
            std::process::exit(1);
        }
        let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).chain(["--elevated"]).collect();
        let exe = std::env::current_exe().unwrap_or_else(|_| root.join("CoreMapTray.exe"));
        if let Err(e) = shell::elevate(&exe, &rest.join(" ")) {
            journal::Journal::new(&app::data_dir(&root)).error(&e);
        }
        return;
    }
    let names = Names::real();
    // Одна копия: вторая молча выходит (задание планировщика тоже не запускает вторую — IgnoreNew).
    let Ok((_single, existed)) = named::create(&names.logon, true) else {
        std::process::exit(1)
    };
    if existed {
        return;
    }
    let config = Config {
        data: app::data_dir(&root),
        root,
        names,
        simulate: false,
        launch: true,
        pipe: pipe::session_pipe(PIPE),
        confirm: Confirm::Ask,
        grace: Duration::from_secs(2),
        wake_delay: Duration::from_secs(5),
        lease_wait: 10_000,
        wake_gap: Duration::from_secs(30),
        auto: app::AutoTiming::REAL,
    };
    let journal = journal::Journal::new(&config.data);
    let mut app = match App::with_system_log(config) {
        Ok(app) => app,
        Err(e) => {
            journal.error(&format!("программа входа не запустилась: {e}"));
            std::process::exit(1);
        }
    };
    let mut shell = match Shell::new("CoreMap.Logon", true) {
        Ok(shell) => shell,
        Err(e) => {
            journal.error(&format!("программа входа не запустилась: {e}"));
            std::process::exit(1);
        }
    };
    journal.note(&format!(
        "запуск, версия {VERSION} (собрана {})",
        BUILT.split('|').next().unwrap_or_default()
    ));
    app.start(&mut shell);
    shell::run(&mut shell, &mut app, None);
    journal.note("выход");
}
