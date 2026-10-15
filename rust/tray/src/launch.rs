// Окно CoreMapWindow.exe глазами программы входа: запустить его без прав администратора, найти уже открытое и
// вывести вперёд.
//
// Запуск без прав. Программа входа работает с правами администратора, а окну они не нужны и не должны достаться
// (DECISIONS #24, #29). Прямого вызова «запусти без прав» в Windows нет; копию просят запустить оболочку рабочего
// стола (Проводник) — она получает его права. Способ — из блога разработчика Windows (Raymond Chen, The Old New Thing,
// 18.11.2013), тот же, что Launch.Unelevated в C#-части: окно рабочего стола (IShellWindows::FindWindowSW) → его
// обозреватель → вид папки → объект Shell.Application внутри Проводника → ShellExecute. Из процесса с правами на
// машине владельца этот путь прошёл его проверкой «мост» 14.10 (из C#; здесь — тот же порядок вызовов COM).
//
// Одна копия окна: перед запуском ищется процесс CoreMapWindow.exe из папки программы; найден — его окно выводится
// вперёд (у программы входа есть право на это сразу после щелчка по её значку: так Windows отдаёт его владельцу
// значка).
#![expect(
    unsafe_code,
    reason = "COM of the desktop shell, process snapshot and window enumeration are Win32 calls"
)]

use std::path::Path;
use std::ptr::null_mut;

use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IDispatch, IServiceProvider};
use windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows, SID_STopLevelBrowser, SVGIO_BACKGROUND,
    SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::core::{BSTR, Interface};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};

use crate::fans::{Apartment, Var, why};

/// Ошибка шага запуска — текстом для владельца.
fn fail(step: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |e| format!("{step}: {}", why(&e))
}

/// Запускает `file` с аргументами `args` в папке `dir` руками Проводника — без прав администратора.
pub fn unelevated(file: &Path, args: &str, dir: &Path) -> Result<(), String> {
    let _com = Apartment::enter().map_err(|e| e.0)?;
    // SAFETY: a COM class id and an interface type; the apartment is open on this thread for the call.
    let windows: IShellWindows =
        unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }.map_err(fail("окна оболочки недоступны"))?;
    let desktop = Var::int(0); // CSIDL_DESKTOP
    let root = Var::empty();
    let mut window = 0i32;
    // SAFETY: both VARIANTs live through the call; `window` is writable; the result is an owned interface.
    let found = unsafe {
        windows.FindWindowSW(
            &raw const desktop.0,
            &raw const root.0,
            SWC_DESKTOP,
            &raw mut window,
            SWFO_NEEDDISPATCH,
        )
    }
    .map_err(fail("окно рабочего стола не найдено"))?;
    let provider: IServiceProvider = found.cast().map_err(fail("рабочий стол без услуг"))?;
    // SAFETY: SID_STopLevelBrowser is a static GUID; the result is an owned interface.
    let browser: IShellBrowser =
        unsafe { provider.QueryService(&SID_STopLevelBrowser) }.map_err(fail("обозреватель рабочего стола"))?;
    // SAFETY: a live interface; the result is owned.
    let view = unsafe { browser.QueryActiveShellView() }.map_err(fail("вид рабочего стола"))?;
    // SAFETY: as above.
    let background: IDispatch =
        unsafe { view.GetItemObject(SVGIO_BACKGROUND) }.map_err(fail("объект вида рабочего стола"))?;
    let folder: IShellFolderViewDual = background.cast().map_err(fail("вид папки рабочего стола"))?;
    // SAFETY: as above.
    let application = unsafe { folder.Application() }.map_err(fail("объект оболочки"))?;
    let shell: IShellDispatch2 = application.cast().map_err(fail("ShellExecute оболочки"))?;
    let (arguments, directory, verb, show) = (
        Var::string(args),
        Var::string(&dir.to_string_lossy()),
        Var::string("open"),
        Var::int(1), // SW_SHOWNORMAL
    );
    // SAFETY: every argument is a live BSTR or VARIANT for the call's duration.
    unsafe {
        shell.ShellExecute(
            &BSTR::from(file.to_string_lossy().as_ref()),
            &arguments.0,
            &directory.0,
            &verb.0,
            &show.0,
        )
    }
    .map_err(fail("Проводник не запустил окно"))
}

/// Полный путь exe процесса; None — процесс не открылся даже для такого вопроса.
#[must_use]
pub fn process_path(pid: u32) -> Option<String> {
    // SAFETY: asks only for limited query rights; a failure returns null.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut size = u32::try_from(buffer.len()).unwrap_or(0);
    // SAFETY: the handle was just opened; `buffer` is writable for `size` units, which Windows updates.
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &raw mut size) };
    // SAFETY: the handle is closed once, here.
    unsafe { CloseHandle(process) };
    (ok != 0).then(|| String::from_utf16_lossy(&buffer[..(size as usize).min(buffer.len())]))
}

/// Номер процесса, запущенного из файла `exe`; None — такого нет.
#[must_use]
pub fn find_process(exe: &Path) -> Option<u32> {
    let name = exe.file_name()?.to_string_lossy().to_lowercase();
    let full = exe.to_string_lossy().to_lowercase();
    // SAFETY: a snapshot of all processes; a failure returns INVALID_HANDLE_VALUE.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        ..PROCESSENTRY32W::default()
    };
    let mut found = None;
    // SAFETY: the snapshot handle is valid; `entry` is writable with its size set as required.
    let mut more = unsafe { Process32FirstW(snapshot, &raw mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase() == name
            && process_path(entry.th32ProcessID).is_some_and(|path| path.to_lowercase() == full)
        {
            found = Some(entry.th32ProcessID);
            break;
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &raw mut entry) } != 0;
    }
    // SAFETY: the snapshot handle is closed once, here.
    unsafe { CloseHandle(snapshot) };
    found
}

/// Что ищется перебором окон: видимое окно верхнего уровня без владельца у процесса.
struct Search {
    pid: u32,
    window: HWND,
}

unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: `data` is the address of the `Search` that `raise` keeps alive during EnumWindows.
    let search = unsafe { &mut *std::ptr::with_exposed_provenance_mut::<Search>(data.cast_unsigned()) };
    let mut pid = 0u32;
    // SAFETY: `window` comes from EnumWindows; `pid` is writable.
    unsafe { GetWindowThreadProcessId(window, &raw mut pid) };
    // SAFETY: plain queries about a window handle from EnumWindows.
    let visible = unsafe { IsWindowVisible(window) } != 0;
    // SAFETY: as above.
    let owned = !unsafe { GetWindow(window, GW_OWNER) }.is_null();
    if pid == search.pid && visible && !owned {
        search.window = window;
        return 0; // нашлось — перебор окончен
    }
    1
}

/// Выводит вперёд окно процесса `pid` (свёрнутое — разворачивает). false — видимого окна у него нет или Windows не
/// отдала ему передний план.
pub fn raise(pid: u32) -> bool {
    let mut search = Search {
        pid,
        window: null_mut(),
    };
    // SAFETY: `visit` only reads and writes `search`, which outlives the synchronous enumeration.
    unsafe { EnumWindows(Some(visit), (&raw mut search).expose_provenance().cast_signed()) };
    if search.window.is_null() {
        return false;
    }
    // SAFETY: a window handle found just now; both calls only ask Windows to show and activate it.
    if unsafe { IsIconic(search.window) } != 0 {
        // SAFETY: as above.
        unsafe { ShowWindow(search.window, SW_RESTORE) };
    }
    // SAFETY: as above.
    unsafe { SetForegroundWindow(search.window) != 0 }
}
