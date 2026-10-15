// Именованные метки Windows (мьютексы), по которым части CoreMap узнают друг о друге (DECISIONS #31).
//
// Присутствие: прежнее окно и движок на C# держат свои мьютексы всё время, пока работают; пока такой есть,
// программа входа процессор не трогает. Аренда (`LEASE`): её берут на всю многошаговую операцию с потолками —
// прочитать, записать по блокам, проверить, — чтобы стартующая C#-часть не прочитала наполовину переставленные
// потолки. Замок почтового ящика (`MAILBOX`): один запрос к регистру 0x150 от записи до ответа (Tuning.cs).
//
// В скрытой проверке к каждому имени добавляется «.Check.<номер процесса>» (`Names::check`): проверка не должна ни
// видеть настоящие части CoreMap, ни мешать им.
#![expect(unsafe_code, reason = "named mutexes: Win32 calls the standard library lacks")]

use std::io;
use std::ptr::null;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
};

/// Имена меток. Настоящие — `Names::real()`; в скрытой проверке — `Names::check(pid)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names {
    /// Одна копия программы входа (новое имя: прежние держит движок C#, DECISIONS #31).
    pub logon: String,
    /// Аренда потолков.
    pub lease: String,
    /// Замок почтового ящика процессора — общий с C#-частью (Tuning.cs, OcMailbox.Gate).
    pub mailbox: String,
    /// Прежнее окно (Program.WindowMutex).
    pub old_window: String,
    /// Процесс движка или прежний резидент на C# (Tray.Single).
    pub engine: String,
    /// Живой Core движка (Engine.CoreMutex): к движку подключено окно или идёт измерение.
    pub engine_core: String,
}

impl Names {
    #[must_use]
    pub fn real() -> Self {
        Self {
            logon: "Local\\CoreMap.Logon".into(),
            lease: "Local\\CoreMap.Ceilings".into(),
            mailbox: "Local\\CoreMap.Mailbox".into(),
            old_window: "Local\\CoreMap.SingleInstance".into(),
            engine: "Local\\CoreMap.Tray".into(),
            engine_core: "Local\\CoreMap.Engine.Core".into(),
        }
    }

    /// Имена скрытой проверки: те же с приставкой «.Check.<pid>».
    #[must_use]
    pub fn check(pid: u32) -> Self {
        let real = Self::real();
        let mark = |name: String| format!("{name}.Check.{pid}");
        Self {
            logon: mark(real.logon),
            lease: mark(real.lease),
            mailbox: mark(real.mailbox),
            old_window: mark(real.old_window),
            engine: mark(real.engine),
            engine_core: mark(real.engine_core),
        }
    }
}

fn wide(name: &str) -> Vec<u16> {
    name.encode_utf16().chain([0]).collect()
}

/// Есть ли сейчас мьютекс с таким именем. Отказ в доступе — тоже «есть»: его держит процесс, которому мы не ровня.
#[must_use]
pub fn exists(name: &str) -> bool {
    match open_raw(name) {
        Ok(_) => true,
        Err(error) => error == ERROR_ACCESS_DENIED,
    }
}

/// Открывает существующий мьютекс только для ожидания. None — его нет (или в доступе отказано).
#[must_use]
pub fn open(name: &str) -> Option<Mutex> {
    open_raw(name).ok()
}

/// Ошибка — код Windows, снятый сразу после вызова.
fn open_raw(name: &str) -> Result<Mutex, u32> {
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated and outlives the call; SYNCHRONIZE is enough to wait on the mutex.
    let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        // SAFETY: reads the calling thread's last-error value that OpenMutexW has just set.
        return Err(unsafe { GetLastError() });
    }
    Ok(Mutex(handle))
}

/// Создаёт мьютекс (или открывает существующий). Второе значение — он уже был до нас.
pub fn create(name: &str, owned: bool) -> io::Result<(Mutex, bool)> {
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated and outlives the call; default security (null) is the creator's.
    let handle = unsafe { CreateMutexW(null(), i32::from(owned), name.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: reads the last-error value CreateMutexW just set.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    Ok((Mutex(handle), existed))
}

/// Чем кончилось ожидание мьютекса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Мьютекс наш (в том числе брошенный: его владелец кончился, не отпустив).
    Held,
    /// Не дождались.
    Timeout,
    /// Ожидание не удалось.
    Failed,
}

/// Дескриптор мьютекса; закрывается при выходе из области видимости.
#[derive(Debug)]
pub struct Mutex(HANDLE);

// SAFETY: a kernel handle is not tied to the thread that opened it; ownership of the mutex itself is per thread and
// is managed by the caller (acquire and release on the same thread, see `Lease`).
unsafe impl Send for Mutex {}

impl Mutex {
    /// Ждёт мьютекс до `ms` миллисекунд. Брошенный (владелец кончился) считается взятым — так и в C#-части.
    #[must_use]
    pub fn acquire(&self, ms: u32) -> Wait {
        // SAFETY: the handle is a valid mutex handle owned by self.
        match unsafe { WaitForSingleObject(self.0, ms) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Wait::Held,
            WAIT_TIMEOUT => Wait::Timeout,
            _ => Wait::Failed,
        }
    }

    /// Отпускает взятый мьютекс.
    pub fn release(&self) {
        // SAFETY: the handle is valid; releasing a mutex this thread does not own fails harmlessly.
        unsafe { ReleaseMutex(self.0) };
    }

    /// Дескриптор для ожидания вместе с сообщениями окна (`MsgWaitForMultipleObjectsEx` у программы входа).
    #[must_use]
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Mutex {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW/OpenMutexW and is closed exactly once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// Взятый мьютекс; отпускается при выходе из области видимости. Брать и отпускать — в одном потоке: так требует
/// Windows, поэтому `Lease` не передаётся между потоками.
#[derive(Debug)]
pub struct Lease<'a> {
    mutex: &'a Mutex,
    _thread_bound: std::marker::PhantomData<*const ()>,
}

impl<'a> Lease<'a> {
    /// Берёт мьютекс, ожидая до `ms` миллисекунд. None — не дождались или ожидание не удалось.
    #[must_use]
    pub fn take(mutex: &'a Mutex, ms: u32) -> Option<Self> {
        (mutex.acquire(ms) == Wait::Held).then_some(Self {
            mutex,
            _thread_bound: std::marker::PhantomData,
        })
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.mutex.release();
    }
}
