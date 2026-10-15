// Вызовы Windows программы входа: скрытое окно, значок у часов, меню, всплывающие сообщения, таймеры, сон и
// пробуждение, запуск других частей CoreMap, права администратора, замер памяти. Логики здесь нет: окно только
// складывает события в очередь, а решает `app.rs`.
//
// Окно — обычное верхнего уровня, но никогда не показывается (не «только для сообщений»): сообщения, которые Windows
// рассылает всем окнам — «панель задач создана заново» (перезапуск Проводника) и сон/пробуждение, — до окон
// «только для сообщений» не доходят (документация Microsoft; DECISIONS #31).
//
// Почему очередь, а не вызов логики из процедуры окна: меню значка (TrackPopupMenu) крутит свой цикл сообщений и
// во время него снова вызывает процедуру окна. Логика в этот момент занята выбором пункта — второй вход в неё
// запрещён правилами заимствования. Процедура окна только записывает событие; цикл программы разбирает очередь.
#![expect(
    unsafe_code,
    reason = "the tray icon, menus, window messages and process start are Win32 calls"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, WAIT_ABANDONED_0, WAIT_FAILED,
    WAIT_OBJECT_0, WPARAM,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, GetCurrentProcess, GetProcessTimes, GetSystemTimes, OpenProcessToken,
    PROCESS_INFORMATION, STARTUPINFOW,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NIN_BALLOONHIDE, NIN_BALLOONTIMEOUT, NIN_BALLOONUSERCLICK, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, Shell_NotifyIconW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreatePopupMenu, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE,
    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, FindWindowW, GetCursorPos, GetMenuItemCount,
    GetMenuItemInfoW, GetSystemMetrics, HMENU, IDI_APPLICATION, IMAGE_ICON, KillTimer, LR_DEFAULTCOLOR, LR_SHARED,
    LoadIconW, LoadImageW, MENUITEMINFOW, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MFS_CHECKED,
    MFS_DEFAULT, MFS_DISABLED, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, MSG, MSGFLT_ALLOW,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    PBT_POWERSETTINGCHANGE, PM_REMOVE, PeekMessageW, PostMessageW, PostQuitMessage, QS_ALLINPUT, RegisterClassW,
    RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL, SendMessageW, SetForegroundWindow,
    SetMenuDefaultItem, SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
    WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_MOUSEMOVE, WM_NULL, WM_POWERBROADCAST,
    WM_QUERYENDSESSION, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW,
};

/// Сообщение значка (как `IconMessage` в Tray.cs).
const ICON_MESSAGE: u32 = 0x8001;
/// Прежнее окно сообщает о себе номером процесса (Tray.WindowOpened; шлёт Tray.Announce в C#).
pub const WINDOW_OPENED: u32 = 0x8002;
/// Просьба к движку открыть новое окно (Tray.OpenNew в C#).
pub const OPEN_NEW: u32 = 0x8004;

/// Что произошло — для логики программы (app.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Правый щелчок по значку или клавиша меню: показать меню в точке.
    Menu(i32, i32),
    /// Двойной щелчок по значку.
    Open,
    /// Курсор над значком: подсказку пора обновить.
    Hover,
    /// Всплывающее сообщение ушло (закрыто, истекло, по нему щёлкнули): можно показать следующее.
    BalloonDone,
    /// Прежнее окно запустилось (номер его процесса).
    Announced(u32),
    Timer(usize),
    Suspend,
    /// Пробуждение или включение выключенного экрана: потолки пора сверить.
    Wake,
    /// Windows завершает сеанс.
    EndSession,
    /// Просьба закрыться (`WM_CLOSE`: выключатель автозапуска в прежнем окне — `Tray.Dismiss` в C#).
    Close,
    /// Проводник перезапущен: значок пропал и добавлен заново.
    IconRestored,
}

#[derive(Debug, Default)]
struct Shared {
    events: VecDeque<Event>,
    taskbar_created: u32,
    /// Последнее известное состояние экрана (0 — выключен, 1 — включён, 2 — притушен); None — ещё не сообщалось.
    display: Option<u8>,
}

thread_local! {
    // Очередь событий от процедуры окна к циклу программы; одна на поток окна.
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}

fn push(event: Event) {
    SHARED.with(|s| s.borrow_mut().events.push_back(event));
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn wide_os(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// Копирует текст в поле фиксированной длины структуры значка, обрезая его.
fn fill(field: &mut [u16], text: &str) {
    let limit = field.len() - 1;
    let mut n = 0;
    for unit in text.encode_utf16().take(limit) {
        field[n] = unit;
        n += 1;
    }
    field[n] = 0;
}

fn same_guid(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

fn low_word(value: usize) -> u32 {
    u32::try_from(value & 0xFFFF).unwrap_or(0)
}

fn signed_word(value: usize) -> i32 {
    i32::from(i16::from_le_bytes([value.to_le_bytes()[0], value.to_le_bytes()[1]]))
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar = SHARED.with(|s| s.borrow().taskbar_created);
    match message {
        ICON_MESSAGE => {
            // Значок версии 4: младшее слово lparam — событие мыши, wparam — точка экрана (x, y).
            match low_word(lparam.cast_unsigned()) {
                WM_CONTEXTMENU => push(Event::Menu(signed_word(wparam), signed_word(wparam >> 16))),
                WM_LBUTTONDBLCLK => push(Event::Open),
                WM_MOUSEMOVE => push(Event::Hover),
                NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT | NIN_BALLOONUSERCLICK => push(Event::BalloonDone),
                _ => {}
            }
            0
        }
        WINDOW_OPENED => {
            push(Event::Announced(u32::try_from(wparam).unwrap_or(0)));
            0
        }
        WM_TIMER => {
            push(Event::Timer(wparam));
            0
        }
        WM_POWERBROADCAST => {
            let kind = u32::try_from(wparam).unwrap_or(0);
            if kind == PBT_APMSUSPEND {
                push(Event::Suspend);
            } else if kind == PBT_APMRESUMEAUTOMATIC || kind == PBT_APMRESUMESUSPEND {
                push(Event::Wake);
            } else if kind == PBT_POWERSETTINGCHANGE && lparam != 0 {
                // SAFETY: for PBT_POWERSETTINGCHANGE lparam points at a POWERBROADCAST_SETTING valid during the call.
                let setting = unsafe { &*(lparam as *const POWERBROADCAST_SETTING) };
                // GUID_CONSOLE_DISPLAY_STATE: 0 — экран выключен, 1 — включён, 2 — притушен. Сразу после подписки Windows
                // присылает текущее состояние — это не пробуждение; пробуждение — переход в «включён» из другого.
                if same_guid(&setting.PowerSetting, &GUID_CONSOLE_DISPLAY_STATE) && setting.DataLength >= 1 {
                    let state = setting.Data[0];
                    let before = SHARED.with(|s| s.borrow_mut().display.replace(state));
                    // Пробуждение — только из «выключен»: из «притушен» экран выходит без сна (ревизия M4).
                    if before == Some(0) && state == 1 {
                        push(Event::Wake);
                    }
                }
            }
            1
        }
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                push(Event::EndSession);
            }
            0
        }
        WM_CLOSE => {
            push(Event::Close);
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop; no preconditions.
            unsafe { PostQuitMessage(0) };
            0
        }
        _ if taskbar != 0 && message == taskbar => {
            push(Event::IconRestored);
            0
        }
        // SAFETY: default handling of every other message for this window, with the arguments Windows passed in.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Скрытое окно программы и значок при нём.
#[derive(Debug)]
pub struct Shell {
    window: HWND,
    icon_shown: bool,
    with_icon: bool,
}

impl Shell {
    /// Создаёт скрытое окно класса `class`. `with_icon` = false — скрытая проверка: значка на экране нет.
    pub fn new(class: &str, with_icon: bool) -> Result<Self, String> {
        let class_name = wide(class);
        // SAFETY: null = the module of this exe.
        let instance = unsafe { GetModuleHandleW(null()) };
        let description = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..WNDCLASSW::default()
        };
        // SAFETY: the class description and its name outlive the call.
        if unsafe { RegisterClassW(&raw const description) } == 0 {
            return Err(format!("класс окна не зарегистрирован, код {}", last_error()));
        }
        let title = wide("CoreMap");
        // SAFETY: the class was registered above; no parent, no menu, never shown (no WS_VISIBLE).
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class_name.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if window.is_null() {
            return Err(format!("окно программы не создано, код {}", last_error()));
        }
        let taskbar_name = wide("TaskbarCreated");
        // SAFETY: the name is NUL-terminated.
        let taskbar = unsafe { RegisterWindowMessageW(taskbar_name.as_ptr()) };
        SHARED.with(|s| s.borrow_mut().taskbar_created = taskbar);
        // Процесс с правами администратора: сообщение от Проводника (обычные права) иначе отсекается (UIPI).
        // SAFETY: the window belongs to this thread; no change-filter structure is asked for.
        unsafe { ChangeWindowMessageFilterEx(window, taskbar, MSGFLT_ALLOW, null_mut()) };
        let display = GUID_CONSOLE_DISPLAY_STATE;
        // SAFETY: the window belongs to this process; the GUID outlives the call (Windows copies it).
        unsafe { RegisterPowerSettingNotification(window, &raw const display, DEVICE_NOTIFY_WINDOW_HANDLE) };
        Ok(Self {
            window,
            icon_shown: false,
            with_icon,
        })
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
            hWnd: self.window,
            uID: 1,
            ..NOTIFYICONDATAW::default()
        }
    }

    /// Добавляет значок. false — Проводник ещё не готов (при входе в Windows бывает): повторить позже.
    pub fn add_icon(&mut self, tip: &str) -> bool {
        if !self.with_icon {
            return true;
        }
        let mut data = self.data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = ICON_MESSAGE;
        data.hIcon = icon();
        fill(&mut data.szTip, tip);
        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW for this window.
        self.icon_shown = unsafe { Shell_NotifyIconW(NIM_ADD, &raw const data) } != 0;
        if self.icon_shown {
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // SAFETY: as above; asks for the version-4 callback layout (wparam = point, lparam = event).
            unsafe { Shell_NotifyIconW(NIM_SETVERSION, &raw const data) };
        }
        self.icon_shown
    }

    /// Проводник перезапущен: прежний значок пропал.
    pub fn forget_icon(&mut self) {
        self.icon_shown = false;
    }

    #[must_use]
    pub fn icon_shown(&self) -> bool {
        self.icon_shown || !self.with_icon
    }

    pub fn set_tip(&self, tip: &str) {
        if !self.icon_shown {
            return;
        }
        let mut data = self.data();
        data.uFlags = NIF_TIP | NIF_SHOWTIP;
        fill(&mut data.szTip, tip);
        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW for the icon added earlier.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const data) };
    }

    /// Всплывающее сообщение у значка. false — значка нет, показать негде.
    pub fn balloon(&self, text: &str) -> bool {
        if !self.icon_shown {
            return false;
        }
        let mut data = self.data();
        data.uFlags = NIF_INFO;
        fill(&mut data.szInfo, text);
        fill(&mut data.szInfoTitle, "CoreMap");
        data.dwInfoFlags = NIIF_WARNING;
        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW for the icon added earlier.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const data) != 0 }
    }

    pub fn remove_icon(&mut self) {
        if !self.icon_shown {
            return;
        }
        let data = self.data();
        // SAFETY: removes the icon this window added.
        unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const data) };
        self.icon_shown = false;
    }

    pub fn set_timer(&self, id: usize, after: Duration) {
        let ms = u32::try_from(after.as_millis()).unwrap_or(u32::MAX).max(1);
        // SAFETY: the window belongs to this thread; WM_TIMER goes to its procedure (no callback).
        unsafe { SetTimer(self.window, id, ms, None) };
    }

    pub fn kill_timer(&self, id: usize) {
        // SAFETY: killing a timer that does not exist fails harmlessly.
        unsafe { KillTimer(self.window, id) };
    }

    /// Показывает меню в точке и ждёт выбора. 0 — ничего не выбрано.
    #[must_use]
    pub fn track(&self, menu: &Menu, x: i32, y: i32) -> u32 {
        let (x, y) = if x == 0 && y == 0 { cursor() } else { (x, y) };
        // Без этого меню не закрывается щелчком мимо него (известное поведение меню значка, документация Microsoft).
        // SAFETY: the window belongs to this thread.
        unsafe { SetForegroundWindow(self.window) };
        // SAFETY: the menu handle is owned by `menu`; TPM_RETURNCMD returns the chosen id instead of posting it.
        let chosen = unsafe {
            TrackPopupMenu(
                menu.0,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                x,
                y,
                0,
                self.window,
                null(),
            )
        };
        // SAFETY: a no-op message that lets the menu close properly next time (same documented pattern).
        unsafe { PostMessageW(self.window, WM_NULL, 0, 0) };
        u32::try_from(chosen).unwrap_or(0)
    }

    pub fn destroy(&self) {
        // SAFETY: the window belongs to this thread; WM_DESTROY ends the loop.
        unsafe { DestroyWindow(self.window) };
    }

    /// Шлёт сообщение своему окну (скрытая проверка — так же, как его прислала бы Windows или C#-часть).
    pub fn post(&self, message: u32, wparam: usize, lparam: isize) {
        // SAFETY: the window belongs to this thread; the message is queued, not sent. A pointer in lparam is the
        // caller's to keep alive until the message is handled (the check pumps the loop before dropping it).
        unsafe { PostMessageW(self.window, message, wparam, lparam) };
    }
    /// Передаёт сообщение своему окну сразу (как Windows шлёт сообщение с указателем в lparam: в очередь такие
    /// сообщения не ставятся — `PostMessage` их отвергает). Скрытая проверка: «экран выключен/включён».
    pub fn send(&self, message: u32, wparam: usize, lparam: isize) {
        // SAFETY: the window belongs to this thread, so the procedure runs synchronously; a pointer in lparam is
        // valid for the duration of the call (the caller owns it).
        unsafe { SendMessageW(self.window, message, wparam, lparam) };
    }
}

/// Кто разбирает события цикла программы (app.rs).
pub trait Handler {
    /// Дескрипторы, которых ждать вместе с сообщениями окна.
    fn waits(&self) -> Vec<HANDLE>;
    fn on(&mut self, shell: &mut Shell, what: Loop);
}

/// Цикл программы: сообщения окна и ожидание дескрипторов. Кончается с окном или к сроку `until` (скрытая проверка
/// прокручивает его кусками).
pub fn run(shell: &mut Shell, handler: &mut dyn Handler, until: Option<Instant>) {
    loop {
        if !drain_messages() {
            return;
        }
        while let Some(event) = SHARED.with(|s| s.borrow_mut().events.pop_front()) {
            handler.on(shell, Loop::Event(event));
        }
        let waiting = handler.waits();
        let timeout = match until {
            None => u32::MAX,
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                u32::try_from(left.as_millis()).unwrap_or(u32::MAX - 1).max(1)
            }
        };
        let count = u32::try_from(waiting.len()).unwrap_or(0);
        // SAFETY: `waiting` holds `count` valid waitable handles kept alive by their owner during the wait.
        let woke =
            unsafe { MsgWaitForMultipleObjectsEx(count, waiting.as_ptr(), timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if woke < WAIT_OBJECT_0 + count {
            handler.on(
                shell,
                Loop::Ended(usize::try_from(woke - WAIT_OBJECT_0).unwrap_or(usize::MAX)),
            );
        } else if woke >= WAIT_ABANDONED_0 && woke < WAIT_ABANDONED_0 + count {
            handler.on(
                shell,
                Loop::Ended(usize::try_from(woke - WAIT_ABANDONED_0).unwrap_or(usize::MAX)),
            );
        } else if woke == WAIT_FAILED {
            std::thread::sleep(Duration::from_millis(200)); // не крутить процессор, если дескриптор испорчен
        }
    }
}

/// Что разбирает цикл программы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loop {
    Event(Event),
    /// Сработал дескриптор с этим номером в списке ожидания.
    Ended(usize),
}

/// Выбирает все сообщения окна. false — пришёл `WM_QUIT`.
fn drain_messages() -> bool {
    let mut msg = MSG::default();
    // SAFETY: `msg` is writable; null window = every window of this thread.
    while unsafe { PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        if msg.message == windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT {
            return false;
        }
        // SAFETY: the message was just taken from this thread's queue.
        unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: as above.
        unsafe { DispatchMessageW(&raw const msg) };
    }
    true
}

fn cursor() -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is writable.
    unsafe { GetCursorPos(&raw mut point) };
    (point.x, point.y)
}

/// Значок программы из её же ресурсов (номер 1, tray.rc); нет — стандартный.
fn icon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    // SAFETY: no preconditions.
    let cx = unsafe { GetSystemMetrics(SM_CXSMICON) };
    // SAFETY: no preconditions.
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) };
    // SAFETY: null = this exe's module.
    let instance = unsafe { GetModuleHandleW(null()) };
    // SAFETY: resource 1 is the icon compiled from tray.rc; a shared icon is never destroyed by us.
    let loaded = unsafe {
        LoadImageW(
            instance,
            std::ptr::without_provenance(1),
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR | LR_SHARED,
        )
    };
    if loaded.is_null() {
        // SAFETY: a system icon, no module needed.
        unsafe { LoadIconW(null_mut(), IDI_APPLICATION) }
    } else {
        loaded
    }
}

/// Строка меню значка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// 0 — пояснение (серое), разделитель или подменю.
    pub id: u32,
    /// None — разделитель.
    pub text: Option<String>,
    pub checked: bool,
    pub enabled: bool,
    pub default: bool,
    /// Непусто — это подменю с такими строками.
    pub children: Vec<Item>,
}

/// Меню Windows; уничтожается при выходе из области видимости (вместе с подменю).
#[derive(Debug)]
pub struct Menu(HMENU);

/// Добавляет строки в меню; подменю создаются и прикрепляются к нему (уничтожаются вместе с ним).
fn append(menu: HMENU, items: &[Item]) -> Result<(), String> {
    for item in items {
        let ok = match &item.text {
            // SAFETY: the menu handle is valid; a separator has no text.
            None => unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, null()) },
            Some(text) => {
                // «&» в меню Windows — подчёркнутая буква; в названии профиля это просто знак.
                let text = wide(&text.replace('&', "&&").replace('\t', " "));
                let mut flags = MF_STRING;
                if item.checked {
                    flags |= MF_CHECKED;
                }
                if !item.enabled || (item.id == 0 && item.children.is_empty()) {
                    flags |= MF_GRAYED;
                }
                if item.children.is_empty() {
                    // SAFETY: the menu handle is valid; the text is NUL-terminated and copied by Windows.
                    unsafe { AppendMenuW(menu, flags, item.id as usize, text.as_ptr()) }
                } else {
                    // SAFETY: no preconditions.
                    let sub = unsafe { CreatePopupMenu() };
                    if sub.is_null() {
                        return Err(format!("подменю не создано, код {}", last_error()));
                    }
                    if let Err(e) = append(sub, &item.children) {
                        // SAFETY: the submenu is not attached yet and is destroyed once here.
                        unsafe { DestroyMenu(sub) };
                        return Err(e);
                    }
                    // SAFETY: both handles are valid; once appended, the submenu belongs to `menu`.
                    let attached = unsafe { AppendMenuW(menu, flags | MF_POPUP, sub as usize, text.as_ptr()) };
                    if attached == 0 {
                        // SAFETY: not attached: destroyed once here.
                        unsafe { DestroyMenu(sub) };
                    }
                    attached
                }
            }
        };
        if ok == 0 {
            return Err(format!("строка меню не добавлена, код {}", last_error()));
        }
        if item.default {
            // SAFETY: the item with this id was just added.
            unsafe { SetMenuDefaultItem(menu, item.id, 0) };
        }
    }
    Ok(())
}

/// Строки меню, как их держит Windows (с подменю).
fn read(menu: HMENU) -> Vec<Item> {
    // SAFETY: the menu handle is valid.
    let count = u32::try_from(unsafe { GetMenuItemCount(menu) }).unwrap_or(0);
    let mut items = Vec::new();
    for position in 0..count {
        let mut text = [0u16; 256];
        let mut info = MENUITEMINFOW {
            cbSize: u32::try_from(size_of::<MENUITEMINFOW>()).unwrap_or(0),
            fMask: MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_STRING | MIIM_SUBMENU,
            dwTypeData: text.as_mut_ptr(),
            cch: u32::try_from(text.len() - 1).unwrap_or(0),
            ..MENUITEMINFOW::default()
        };
        // SAFETY: `info` describes a writable text buffer of `cch` + 1 units that outlives the call.
        if unsafe { GetMenuItemInfoW(menu, position, 1, &raw mut info) } == 0 {
            continue;
        }
        let separator = info.fType & MF_SEPARATOR != 0;
        let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        let popup = !info.hSubMenu.is_null();
        items.push(Item {
            id: if popup { 0 } else { info.wID },
            text: (!separator).then(|| String::from_utf16_lossy(&text[..len]).replace("&&", "&")),
            checked: info.fState & MFS_CHECKED != 0,
            enabled: info.fState & MFS_DISABLED == 0,
            default: info.fState & MFS_DEFAULT != 0,
            children: if popup { read(info.hSubMenu) } else { Vec::new() },
        });
    }
    items
}

impl Menu {
    pub fn build(items: &[Item]) -> Result<Self, String> {
        // SAFETY: no preconditions.
        let handle = unsafe { CreatePopupMenu() };
        if handle.is_null() {
            return Err(format!("меню не создано, код {}", last_error()));
        }
        let menu = Self(handle);
        append(menu.0, items)?;
        Ok(menu)
    }

    /// Что Windows держит в меню: для скрытой проверки — меню собрано на самом деле, а не только в памяти программы.
    #[must_use]
    pub fn inspect(&self) -> Vec<Item> {
        read(self.0)
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreatePopupMenu and is destroyed exactly once, here.
        unsafe { DestroyMenu(self.0) };
    }
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value.
    unsafe { GetLastError() }
}

/// Запущенный процесс; дескриптор закрывается при выходе из области видимости.
#[derive(Debug)]
pub struct Child(HANDLE);

// SAFETY: a process handle is not tied to the thread that opened it.
unsafe impl Send for Child {}

impl Child {
    #[must_use]
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateProcessW and is closed exactly once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// Запускает программу с правами этого процесса (запроса Windows нет): `exe` с аргументами `args`, папка — `dir`.
pub fn launch(exe: &Path, args: &str, dir: &Path) -> Result<Child, String> {
    let application = wide_os(exe.as_os_str());
    // Командная строка: путь в кавычках и аргументы; CreateProcessW может её менять, поэтому — свой буфер.
    let mut command: Vec<u16> = wide(&format!(
        "\"{}\"{}{}",
        exe.display(),
        if args.is_empty() { "" } else { " " },
        args
    ));
    let directory = wide_os(dir.as_os_str());
    let startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(0),
        ..STARTUPINFOW::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every string is NUL-terminated and outlives the call; the command line buffer is writable; the
    // structures are initialised; handles are not inherited.
    let ok = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            0,
            CREATE_NO_WINDOW,
            null(),
            directory.as_ptr(),
            &raw const startup,
            &raw mut info,
        )
    };
    if ok == 0 {
        return Err(format!("{} не запустился, код {}", exe.display(), last_error()));
    }
    // SAFETY: the thread handle is not needed; it is closed once here.
    unsafe { CloseHandle(info.hThread) };
    Ok(Child(info.hProcess))
}

/// Шлёт сообщение окну C#-части по имени его класса. false — такого окна нет.
pub fn post_to_class(class: &str, message: u32, wparam: usize) -> bool {
    let name = wide(class);
    // SAFETY: the class name is NUL-terminated; no title filter.
    let window = unsafe { FindWindowW(name.as_ptr(), null()) };
    if window.is_null() {
        return false;
    }
    // SAFETY: posting to another process's window; Windows checks the window is still valid.
    unsafe { PostMessageW(window, message, wparam, 0) != 0 }
}

/// Процесс запущен с правами администратора.
#[must_use]
pub fn elevated() -> bool {
    let mut token: HANDLE = null_mut();
    // SAFETY: no preconditions; returns the current-process pseudo-handle.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: the pseudo-handle is always valid; `token` is writable.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = 0u32;
    // SAFETY: `elevation` is writable for its size; the token was just opened with TOKEN_QUERY.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &raw mut size,
        )
    };
    // SAFETY: the token handle is closed once here.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Перезапускает программу с правами администратора: Windows покажет свой запрос. Ok(false) — владелец отказал.
pub fn elevate(exe: &Path, args: &str) -> Result<bool, String> {
    let verb = wide("runas");
    let file = wide_os(exe.as_os_str());
    let parameters = wide(args);
    let directory = exe.parent().map(|d| wide_os(d.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0),
        fMask: SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ref().map_or(null(), Vec::as_ptr),
        nShow: SW_SHOWNORMAL,
        ..SHELLEXECUTEINFOW::default()
    };
    // SAFETY: every string is NUL-terminated and outlives the call; `info` is initialised.
    if unsafe { ShellExecuteExW(&raw mut info) } != 0 {
        return Ok(true);
    }
    match last_error() {
        1223 => Ok(false), // ERROR_CANCELLED: владелец нажал «Нет»
        code => Err(format!("перезапуск с правами администратора не удался, код {code}")),
    }
}

/// Память и время процессора этого процесса — для отчёта скрытой проверки (как у резидента C#).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// Выделенная память (`PrivateUsage`), байты.
    pub private: usize,
    /// Рабочий набор, байты.
    pub working: usize,
    /// Время процессора (ядро + пользователь), сотни наносекунд.
    pub cpu: u64,
}

#[must_use]
pub fn usage() -> Usage {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(0),
        ..PROCESS_MEMORY_COUNTERS_EX::default()
    };
    // SAFETY: no preconditions; returns the current-process pseudo-handle.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: the pseudo-handle is valid; `counters` is writable for `cb` bytes (the _EX form is accepted).
    unsafe { K32GetProcessMemoryInfo(process, (&raw mut counters).cast(), counters.cb) };
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo-handle is valid; all four FILETIMEs are writable.
    unsafe {
        GetProcessTimes(
            process,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    let ticks = |ft: FILETIME| u64::from(ft.dwHighDateTime) << 32 | u64::from(ft.dwLowDateTime);
    Usage {
        private: counters.PrivateUsage,
        working: counters.WorkingSetSize,
        cpu: ticks(kernel) + ticks(user),
    }
}

/// Время процессоров с запуска Windows, сотни наносекунд: (простой, всего). Доля работы между двумя чтениями —
/// 1 − Δпростой/Δвсего; Windows считает её сама, так что ни одно спящее ядро ради неё не будится.
#[must_use]
pub fn system_times() -> Option<(u64, u64)> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: all three FILETIMEs are writable for the call.
    if unsafe { GetSystemTimes(&raw mut idle, &raw mut kernel, &raw mut user) } == 0 {
        return None;
    }
    let ticks = |ft: FILETIME| u64::from(ft.dwHighDateTime) << 32 | u64::from(ft.dwLowDateTime);
    // Время ядра Windows включает простой: всего = ядро + пользователь.
    Some((ticks(idle), ticks(kernel) + ticks(user)))
}
