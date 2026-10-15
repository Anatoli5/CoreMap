// Канал окна у программы входа (DECISIONS #32, шаг 2): именованный канал Windows, по которому окно CoreMapWindow.exe
// без прав администратора получает состояние и шлёт команды. Сообщение — четыре байта длины (младший первым) и столько
// же байт UTF-8 с одним объектом JSON (src/Wire.cs C#-части; окно этот формат уже читает). Что в сообщениях — решает
// app/serve.rs; здесь только перевозка.
//
// Доступ — как у канала движка на C# (src/Access.cs, проба 13.10, outputs\bridge-probe-2026-10-13\report.md):
// • канал создаётся вызовом Windows с признаками «первый и единственный экземпляр» (имя заранее не займёт чужая
//   программа) и «никого по сети» (PIPE_REJECT_REMOTE_CLIENTS: список доступа из .NET пропускал того же пользователя
//   через сетевой путь машины, закрыл это только этот признак);
// • пускается пользователь сеанса Windows (тот, кто за этим рабочим столом), а не учётная запись процесса: при входе
//   с правами по чужому паролю это разные люди; ему — чтение, запись, ожидание, без права создавать экземпляры;
// • метка целостности «средняя» явно: окно без прав пишет в канал программы с правами.
//
// Ввод-вывод перекрываемый, в потоке значка: ожидание клиента, чтение и запись — операции, чьи события цикл программы
// ждёт вместе с сообщениями окна (shell::run, Handler::waits). Поток не блокируется ни на чём: клиент, который перестал
// читать, через `STALL` отключается, а не останавливает значок и автомодуль.
#![expect(
    unsafe_code,
    reason = "named-pipe server with overlapped I/O, its security descriptor, the session user: Win32 calls"
)]

use std::collections::VecDeque;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{LookupAccountNameW, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SID_NAME_USE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSDomainName, WTSFreeMemory,
    WTSQuerySessionInformationW, WTSUserName,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};

/// Сообщение длиннее — нарушение протокола (Wire.Cap): соединение рвётся.
pub const CAP: usize = 1 << 20;
/// Запись не кончилась за столько — клиент не читает и отключается (Bridge.StallMs).
const STALL: Duration = Duration::from_secs(2);
/// Столько байт читается за раз; сообщения окна — команды в сотню байт.
const CHUNK: usize = 16 * 1024;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value.
    unsafe { GetLastError() }
}

/// Строка UTF-16 до нуля по указателю Windows.
///
/// # Safety
/// `text` is null or points at a NUL-terminated UTF-16 string that stays valid during the call.
unsafe fn read_wide(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    loop {
        // SAFETY: the caller guarantees a NUL-terminated string: every unit up to the terminator is in it.
        let unit = unsafe { text.add(len) };
        // SAFETY: `unit` is within the string (see above), so it is readable.
        if unsafe { *unit } == 0 {
            break;
        }
        len += 1;
    }
    // SAFETY: `len` units were just read one by one, so the slice is within the string.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
}

/// Канал окна в этом сеансе Windows: `base.<номер сеанса>` (имена каналов общие на машину, а программ входа может
/// быть по одной в каждом сеансе — как Engine.PipeName в C#).
#[must_use]
pub fn session_pipe(base: &str) -> String {
    let mut session = 0u32;
    // SAFETY: `session` is writable for the call; the process id is our own.
    unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session) };
    format!("{base}.{session}")
}

/// Одно поле сведений о сеансе Windows (имя пользователя, домен).
fn session_field(class: i32) -> Result<String, String> {
    let mut buffer: *mut u16 = null_mut();
    let mut bytes = 0u32;
    // SAFETY: the current server and session pseudo-values; Windows allocates the buffer and returns it in `buffer`.
    let ok = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            class,
            &raw mut buffer,
            &raw mut bytes,
        )
    };
    if ok == 0 {
        return Err(format!("сведения о сеансе Windows не получены, код {}", last_error()));
    }
    // SAFETY: on success `buffer` is a NUL-terminated string allocated by Windows.
    let text = unsafe { read_wide(buffer) };
    // SAFETY: the buffer came from WTSQuerySessionInformationW and is freed once, here.
    unsafe { WTSFreeMemory(buffer.cast()) };
    Ok(text)
}

/// Пользователь сеанса Windows — строкой SID («S-1-5-21-…»), для списка доступа канала (Access.SessionUser в C#).
pub fn session_user() -> Result<String, String> {
    let account = format!("{}\\{}", session_field(WTSDomainName)?, session_field(WTSUserName)?);
    let name = wide(&account);
    let mut sid = vec![0u8; 256];
    let mut sid_size = u32::try_from(sid.len()).unwrap_or(0);
    let mut domain = vec![0u16; 256];
    let mut domain_size = u32::try_from(domain.len()).unwrap_or(0);
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: the name is NUL-terminated; both buffers are writable for the sizes passed, which Windows updates.
    let ok = unsafe {
        LookupAccountNameW(
            null(),
            name.as_ptr(),
            sid.as_mut_ptr().cast(),
            &raw mut sid_size,
            domain.as_mut_ptr(),
            &raw mut domain_size,
            &raw mut kind,
        )
    };
    if ok == 0 {
        return Err(format!("учётная запись {account} не найдена, код {}", last_error()));
    }
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` holds the SID Windows just wrote; the string is allocated by Windows into `text`.
    if unsafe { ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &raw mut text) } == 0 {
        return Err(format!("SID пользователя не переведён в строку, код {}", last_error()));
    }
    // SAFETY: on success `text` is a NUL-terminated string allocated with LocalAlloc.
    let sid = unsafe { read_wide(text) };
    // SAFETY: frees the string allocated by ConvertSidToStringSidW, once.
    unsafe { LocalFree(text as HLOCAL) };
    Ok(sid)
}

/// Событие Windows (ручной сброс) для одной перекрываемой операции; закрывается при уходе.
#[derive(Debug)]
struct Event(HANDLE);

impl Event {
    fn new() -> Result<Self, String> {
        // SAFETY: null attributes and name are allowed; manual reset, not signalled.
        let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
        if handle.is_null() {
            return Err(format!("событие канала не создано, код {}", last_error()));
        }
        Ok(Self(handle))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// Место одной перекрываемой операции. `OVERLAPPED` — в куче: его адрес не меняется, пока операция идёт, даже если
/// сам `Server` переедет.
struct Slot {
    overlapped: Box<OVERLAPPED>,
    event: Event,
    pending: bool,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Slot {
    fn new() -> Result<Self, String> {
        Ok(Self {
            overlapped: Box::new(OVERLAPPED::default()),
            event: Event::new()?,
            pending: false,
        })
    }

    /// `OVERLAPPED` для новой операции: обнулён, с событием этого места.
    fn arm(&mut self) -> *mut OVERLAPPED {
        *self.overlapped = OVERLAPPED {
            hEvent: self.event.0,
            ..OVERLAPPED::default()
        };
        &raw mut *self.overlapped
    }
}

/// Чем кончилась операция.
enum Outcome {
    Waiting,
    Done(usize),
    Failed(u32),
}

/// Что случилось на канале.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Happened {
    /// Подключился клиент (номер его процесса; 0 — Windows не сказала).
    Connected(u32),
    /// Клиент ушёл — как.
    Gone(String),
}

/// Канал окна: один экземпляр, один клиент в каждый момент.
#[derive(Debug)]
pub struct Server {
    pipe: HANDLE,
    /// Ожидание клиента, потом чтения — по очереди, поэтому одно место.
    input: Slot,
    output: Slot,
    connected: bool,
    /// Клиент подключился раньше, чем канал начал его ждать (`ERROR_PIPE_CONNECTED`): операции нет, событие взведено
    /// вручную, чтобы цикл программы проснулся.
    ready: bool,
    /// Куда читается; пуст, пока клиента нет (память программы без окна — та же, что до канала).
    buffer: Vec<u8>,
    /// Прочитанное, но ещё не разобранное на сообщения.
    inbox: Vec<u8>,
    outbox: VecDeque<Vec<u8>>,
    /// Сообщение, которое пишется сейчас, и с какого времени.
    writing: Option<(Vec<u8>, Instant)>,
}

impl Server {
    /// Создаёт канал `name` с доступом для пользователя `admit` (строка SID) и начинает ждать клиента. Имя занято —
    /// ошибка: на чужом канале программа не работает.
    pub fn create(name: &str, admit: &str) -> Result<Self, String> {
        let sddl = wide(&format!(
            "D:P(D;;FA;;;NU)(A;;0x12019b;;;{admit})(A;;FA;;;SY)(A;;FA;;;BA)S:(ML;;NW;;;ME)"
        ));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: the SDDL string is NUL-terminated; Windows allocates the descriptor into `descriptor`.
        let built = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if built == 0 {
            return Err(format!("доступ к каналу не описан, код {}", last_error()));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let path = wide(&format!(r"\\.\pipe\{name}"));
        // SAFETY: the name is NUL-terminated; `attributes` and the descriptor it points at outlive the call.
        let pipe = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65_536,
                4_096,
                0,
                &raw const attributes,
            )
        };
        let error = last_error();
        // SAFETY: frees the descriptor allocated above, once; the pipe keeps its own copy.
        unsafe { LocalFree(descriptor as HLOCAL) };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(match error {
                5 | 231 => format!("канал {name} уже занят другой программой"),
                _ => format!("канал {name} не создан, код {error}"),
            });
        }
        let mut server = Self {
            pipe,
            input: Slot::new()?,
            output: Slot::new()?,
            connected: false,
            ready: false,
            buffer: Vec::new(),
            inbox: Vec::new(),
            outbox: VecDeque::new(),
            writing: None,
        };
        server.listen();
        Ok(server)
    }

    /// Ждать клиента.
    fn listen(&mut self) {
        self.connected = false;
        self.ready = false;
        let overlapped = self.input.arm();
        // SAFETY: the pipe handle is ours; the OVERLAPPED lives in a Box that stays until the operation ends (Drop and
        // `disconnect` cancel and wait first).
        let connected = unsafe { ConnectNamedPipe(self.pipe, overlapped) };
        let error = last_error();
        if connected != 0 || error == ERROR_IO_PENDING {
            self.input.pending = true;
        } else if error == ERROR_PIPE_CONNECTED {
            self.ready = true;
            // SAFETY: our own event; wakes the loop, which then takes the client in `pump`.
            unsafe { SetEvent(self.input.event.0) };
        } else if error == ERROR_NO_DATA {
            // Клиент подключился и уже ушёл: освободить экземпляр и ждать следующего (один раз, без петли).
            // SAFETY: the pipe handle is ours.
            unsafe { DisconnectNamedPipe(self.pipe) };
            let overlapped = self.input.arm();
            // SAFETY: as above.
            if unsafe { ConnectNamedPipe(self.pipe, overlapped) } != 0 || last_error() == ERROR_IO_PENDING {
                self.input.pending = true;
            }
        }
        // Иначе канал негоден: клиента он не дождётся, событий нет — программа живёт без окна (видно в журнале по
        // отказу окна подключиться).
    }

    /// Чем кончилась операция места `slot` (не ждёт).
    fn outcome(pipe: HANDLE, slot: &Slot) -> Outcome {
        let mut done = 0u32;
        // SAFETY: the same handle and OVERLAPPED as the operation started on this slot; bWait = FALSE only asks.
        let ok = unsafe { GetOverlappedResult(pipe, &raw const *slot.overlapped, &raw mut done, 0) };
        if ok != 0 {
            return Outcome::Done(done as usize);
        }
        match last_error() {
            ERROR_IO_INCOMPLETE => Outcome::Waiting,
            code => Outcome::Failed(code),
        }
    }

    /// Подключился клиент: начать читать.
    fn take_client(&mut self) -> Vec<Happened> {
        self.connected = true;
        self.buffer = vec![0; CHUNK];
        self.inbox.clear();
        let mut pid = 0u32;
        // SAFETY: the pipe handle is ours and connected; `pid` is writable.
        unsafe { GetNamedPipeClientProcessId(self.pipe, &raw mut pid) };
        let mut happened = vec![Happened::Connected(pid)];
        if let Err(code) = self.read() {
            happened.push(self.lose(&format!("чтение не началось, код {code}")));
        }
        happened
    }

    fn read(&mut self) -> Result<(), u32> {
        let overlapped = self.input.arm();
        let length = u32::try_from(self.buffer.len()).unwrap_or(0);
        // SAFETY: `buffer` is writable for `length` bytes and is not touched or reallocated until this read ends (it
        // is replaced only in `disconnect`, after cancelling and waiting); the OVERLAPPED is boxed.
        let started = unsafe { ReadFile(self.pipe, self.buffer.as_mut_ptr(), length, null_mut(), overlapped) };
        if started != 0 || last_error() == ERROR_IO_PENDING {
            self.input.pending = true;
            return Ok(());
        }
        Err(last_error())
    }

    /// Что произошло с прошлого раза: зовётся, когда сработало одно из событий `handles`.
    pub fn pump(&mut self) -> Vec<Happened> {
        let mut happened = Vec::new();
        if self.ready {
            self.ready = false;
            // SAFETY: our own event, set by `listen`.
            unsafe { ResetEvent(self.input.event.0) };
            happened.extend(self.take_client());
        }
        if self.input.pending {
            match Self::outcome(self.pipe, &self.input) {
                Outcome::Waiting => {}
                Outcome::Done(count) => {
                    self.input.pending = false;
                    if self.connected {
                        self.inbox
                            .extend_from_slice(&self.buffer[..count.min(self.buffer.len())]);
                        if let Err(code) = self.read() {
                            happened.push(self.lose(&gone(code)));
                        }
                    } else {
                        happened.extend(self.take_client());
                    }
                }
                Outcome::Failed(code) => {
                    self.input.pending = false;
                    if self.connected {
                        happened.push(self.lose(&gone(code)));
                    } else {
                        self.disconnect();
                    }
                }
            }
        }
        if self.output.pending {
            match Self::outcome(self.pipe, &self.output) {
                Outcome::Waiting => {}
                Outcome::Done(_) => {
                    self.output.pending = false;
                    self.writing = None;
                    if let Err(code) = self.write_next() {
                        happened.push(self.lose(&gone(code)));
                    }
                }
                Outcome::Failed(code) => {
                    self.output.pending = false;
                    happened.push(self.lose(&gone(code)));
                }
            }
        }
        happened
    }

    /// Следующее целое сообщение из прочитанного; Ok(None) — его ещё нет. Err — нарушение протокола.
    pub fn message(&mut self) -> Result<Option<Vec<u8>>, String> {
        let Some(head) = self.inbox.first_chunk::<4>() else {
            return Ok(None);
        };
        let size = u32::from_le_bytes(*head) as usize;
        if size == 0 || size > CAP {
            return Err(format!("длина сообщения {size} вне 1..{CAP}"));
        }
        if self.inbox.len() < 4 + size {
            return Ok(None);
        }
        let body = self.inbox[4..4 + size].to_vec();
        self.inbox.drain(..4 + size);
        Ok(Some(body))
    }

    /// Ставит сообщение в очередь клиенту. Err — запись не началась (клиент ушёл): отключить его.
    pub fn send(&mut self, text: &str) -> Result<(), String> {
        if !self.connected {
            return Ok(());
        }
        let length = u32::try_from(text.len())
            .ok()
            .filter(|&n| n as usize <= CAP)
            .ok_or_else(|| format!("сообщение длиной {} не уходит в канал", text.len()))?;
        let mut frame = Vec::with_capacity(4 + text.len());
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(text.as_bytes());
        self.outbox.push_back(frame);
        if self.output.pending {
            return Ok(());
        }
        self.write_next().map_err(gone)
    }

    fn write_next(&mut self) -> Result<(), u32> {
        let Some(frame) = self.outbox.pop_front() else {
            return Ok(());
        };
        let length = u32::try_from(frame.len()).unwrap_or(0);
        let pointer = frame.as_ptr();
        // Буфер переезжает в `writing` до начала записи: перенос Vec не двигает его байты в куче.
        self.writing = Some((frame, Instant::now()));
        let overlapped = self.output.arm();
        // SAFETY: `pointer` addresses the heap bytes of the frame now held in `writing`, which is kept untouched until
        // the write ends (`pump` or `disconnect` clear it only after completion); the OVERLAPPED is boxed.
        let started = unsafe { WriteFile(self.pipe, pointer, length, null_mut(), overlapped) };
        if started != 0 || last_error() == ERROR_IO_PENDING {
            self.output.pending = true;
            return Ok(());
        }
        self.writing = None;
        Err(last_error())
    }

    /// Пишется ли что-то сейчас (тогда очередное состояние можно пропустить: следующее всё равно придёт).
    #[must_use]
    pub fn writing(&self) -> bool {
        self.output.pending || !self.outbox.is_empty()
    }

    /// Запись висит дольше `STALL`: клиент не читает.
    #[must_use]
    pub fn stalled(&self) -> bool {
        self.writing.as_ref().is_some_and(|(_, since)| since.elapsed() >= STALL)
    }

    /// События, которых ждать циклу программы: только у идущих операций (у кончившейся событие осталось бы взведённым,
    /// и цикл крутился бы вхолостую).
    #[must_use]
    pub fn handles(&self) -> Vec<HANDLE> {
        let mut handles = Vec::with_capacity(2);
        if self.input.pending || self.ready {
            handles.push(self.input.event.0);
        }
        if self.output.pending {
            handles.push(self.output.event.0);
        }
        handles
    }

    /// Отключить клиента и ждать следующего; возвращает, что сказать о его уходе.
    pub fn lose(&mut self, why: &str) -> Happened {
        self.disconnect();
        Happened::Gone(why.to_string())
    }

    /// Обрывает идущие операции и дожидается их конца (до этого буферы и OVERLAPPED трогать нельзя).
    fn cancel(&mut self) {
        for slot in [&mut self.input, &mut self.output] {
            if !slot.pending {
                continue;
            }
            // SAFETY: cancels the operation started with this very OVERLAPPED on our handle.
            unsafe { CancelIoEx(self.pipe, &raw const *slot.overlapped) };
            let mut done = 0u32;
            // SAFETY: bWait = TRUE returns once the cancelled operation has completed, so the buffer and the
            // OVERLAPPED are free afterwards.
            unsafe { GetOverlappedResult(self.pipe, &raw const *slot.overlapped, &raw mut done, 1) };
            slot.pending = false;
        }
    }

    fn disconnect(&mut self) {
        self.cancel();
        // SAFETY: the pipe handle is ours; nothing is pending on it any more.
        unsafe { DisconnectNamedPipe(self.pipe) };
        self.buffer = Vec::new();
        self.inbox = Vec::new();
        self.outbox.clear();
        self.writing = None;
        self.listen();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel();
        // SAFETY: the handle came from CreateNamedPipeW and is closed once, here, after every operation ended.
        unsafe { CloseHandle(self.pipe) };
    }
}

/// Как ушёл клиент — по коду Windows.
fn gone(code: u32) -> String {
    match code {
        109 => "окно закрыло канал".into(),   // ERROR_BROKEN_PIPE
        232 => "окно закрывает канал".into(), // ERROR_NO_DATA
        995 => "канал закрыт".into(),         // ERROR_OPERATION_ABORTED
        code => format!("обрыв канала, код {code}"),
    }
}
