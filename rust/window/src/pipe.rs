// Конец моста у окна: канал движка (именованный канал Windows), сообщения — четыре байта длины и JSON (src/Wire.cs).
//
// Канал открывается с перекрываемым вводом-выводом: чтение ждёт в своём потоке, а команда от владельца пишется
// сразу. С обычным (синхронным) вводом-выводом Windows выстраивает чтение и запись одного канала в очередь, и щелчок
// ждал бы, пока придёт следующее состояние, — до полсекунды.
// Права на канале — «узнать, кто я» (SECURITY_IDENTIFICATION), как у стороны окна в C#: движок может спросить
// Windows, кто клиент, но действовать от его имени не может.
//
// Это единственный модуль окна с unsafe: перекрываемого ввода-вывода для каналов и номера сеанса Windows в
// стандартной библиотеке нет, остаются вызовы Windows напрямую. Снаружи модуль безопасен: Link и engine_pipe.
#![expect(
    unsafe_code,
    reason = "Win32 calls the standard library lacks: overlapped named-pipe I/O, the session id"
)]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    WriteFile,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::CreateEventW;

use crate::wire::{Message, PROTOCOL};

const CAP: usize = 1 << 20; // сообщение длиннее — нарушение протокола (Wire.Cap)

/// Канал движка этого сеанса Windows: «CoreMap.Bridge.<номер сеанса>» (Engine.PipeName в C#-части).
pub fn engine_pipe() -> String {
    let mut session = 0u32;
    // SAFETY: `session` is a valid, writable u32 for the duration of the call; the process id is our own.
    unsafe { ProcessIdToSessionId(std::process::id(), &raw mut session) };
    format!("CoreMap.Bridge.{session}")
}

/// Ручка Windows, закрываемая ровно один раз.
#[derive(Debug)]
struct Handle(HANDLE);

// SAFETY: a kernel handle is not tied to the thread that opened it; Pipe gives the read side to one thread and
// serialises writes under its lock, so concurrent use is limited to one read and one write, which overlapped I/O allows.
unsafe impl Send for Handle {}
// SAFETY: as above — shared references only start overlapped operations, each with its own OVERLAPPED and event.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW or CreateEventW, was checked valid, and is closed only here.
        unsafe { CloseHandle(self.0) };
    }
}

fn event() -> io::Result<Handle> {
    // SAFETY: null attributes and a null name are allowed; a manual-reset event, not signalled.
    let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(handle))
    }
}

/// Открытый канал. Чтение — из одного потока, запись — под замком из любого.
#[derive(Debug)]
struct Pipe {
    handle: Handle,
    read_event: Handle,
    write_event: Handle,
    // Порядок кадров: две команды из разных мест не должны перемешаться байтами. Больше ничего не охраняет.
    writing: Mutex<()>,
}

impl Pipe {
    fn open(name: &str) -> io::Result<Pipe> {
        let path: Vec<u16> = OsStr::new(&format!(r"\\.\pipe\{name}"))
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: `path` is NUL-terminated UTF-16 and outlives the call; null security attributes and a null
        // template handle are allowed for OPEN_EXISTING.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = Handle(handle);
        Ok(Pipe {
            handle,
            read_event: event()?,
            write_event: event()?,
            writing: Mutex::new(()),
        })
    }

    /// Одна перекрываемая операция до конца. Буфер живёт и не двигается, пока она не кончилась: функция ждёт её
    /// завершения (`GetOverlappedResult` с ожиданием) прежде, чем вернуть управление и отпустить буфер и `OVERLAPPED`.
    fn transfer(&self, event: &Handle, buffer: &mut [u8], write: bool) -> io::Result<usize> {
        let length = u32::try_from(buffer.len()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: OVERLAPPED is plain data; all-zero with an event is its documented initial state.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.0;
        let started = if write {
            // SAFETY: `buffer` holds `length` readable bytes and `overlapped` lives on this frame; both stay in place
            // until GetOverlappedResult below has waited for the operation to finish.
            unsafe { WriteFile(self.handle.0, buffer.as_ptr(), length, null_mut(), &raw mut overlapped) }
        } else {
            // SAFETY: as for WriteFile, with `buffer` writable for `length` bytes.
            unsafe {
                ReadFile(
                    self.handle.0,
                    buffer.as_mut_ptr(),
                    length,
                    null_mut(),
                    &raw mut overlapped,
                )
            }
        };
        if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
                return Err(error);
            }
        }
        let mut done = 0u32;
        // SAFETY: the same handle and OVERLAPPED as the operation just started; bWait = TRUE returns only after the
        // operation is complete, successfully or not.
        if unsafe { GetOverlappedResult(self.handle.0, &raw const overlapped, &raw mut done, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(done as usize)
    }

    fn read_exact(&self, buffer: &mut [u8]) -> io::Result<()> {
        let mut at = 0;
        while at < buffer.len() {
            let got = self.transfer(&self.read_event, &mut buffer[at..], false)?;
            if got == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            at += got;
        }
        Ok(())
    }

    fn send(&self, text: &str) -> io::Result<()> {
        let length = u32::try_from(text.len()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let mut frame = Vec::with_capacity(4 + text.len());
        frame.extend_from_slice(&length.to_le_bytes());
        frame.extend_from_slice(text.as_bytes());
        let _turn = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut at = 0;
        while at < frame.len() {
            at += self.transfer(&self.write_event, &mut frame[at..], true)?;
        }
        Ok(())
    }

    /// Следующее сообщение в `body` (буфер переиспользуется от сообщения к сообщению).
    fn receive(&self, body: &mut Vec<u8>) -> io::Result<()> {
        let mut head = [0u8; 4];
        self.read_exact(&mut head)?;
        let size = u32::from_le_bytes(head) as usize;
        if size == 0 || size > CAP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("длина сообщения {size}"),
            ));
        }
        body.resize(size, 0);
        self.read_exact(body)
    }
}

/// Что окно узнаёт о канале.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a few events a second, each moved once to the window thread; a box would only add an allocation"
)]
pub enum Event {
    Up,
    Down(String),
    Message(Message),
}

/// Связь с движком: свой поток держит канал, переподключается и отдаёт события в deliver.
#[derive(Debug)]
pub struct Link {
    // Канал, пока он открыт: поток моста кладёт и убирает, команды окна берут копию указателя и сразу отпускают.
    pipe: Mutex<Option<Arc<Pipe>>>,
    next: AtomicI64,
}

impl Link {
    pub fn start(name: String, deliver: impl Fn(Event) + Send + 'static) -> Arc<Link> {
        let link = Arc::new(Link {
            pipe: Mutex::new(None),
            next: AtomicI64::new(1),
        });
        let me = Arc::clone(&link);
        thread::Builder::new()
            .name("мост-окно".into())
            .spawn(move || me.run(&name, deliver))
            .expect("поток моста");
        link
    }

    fn run(&self, name: &str, deliver: impl Fn(Event)) {
        let mut told = String::new();
        let down = |why: &str, told: &mut String| {
            if why != told {
                *told = why.to_string();
                deliver(Event::Down(why.to_string()));
            }
        };
        loop {
            let pipe = match Pipe::open(name) {
                Ok(pipe) => Arc::new(pipe),
                Err(e) => {
                    let why = match e.raw_os_error().map(i32::cast_unsigned) {
                        Some(ERROR_FILE_NOT_FOUND) => "движок не запущен".to_string(),
                        Some(ERROR_PIPE_BUSY) => "канал занят другим окном".to_string(),
                        _ => format!("канал не открылся: {e}"),
                    };
                    down(&why, &mut told);
                    // Ждать движок: раз в секунду один вызов Windows, пока канала нет.
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            *self.pipe.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&pipe));
            told.clear();
            let since = Instant::now();
            deliver(Event::Up);
            // Окно говорит первым: знакомство — одно на соединение.
            let hello = format!(r#"{{"id":0,"c":"hello","proto":{PROTOCOL}}}"#);
            let mut why = String::from("связь с движком прервалась");
            if pipe.send(&hello).is_ok() {
                let mut body = Vec::new();
                // Ошибка чтения — канал закрыт или оборван: для окна это одно, «связь прервалась».
                while pipe.receive(&mut body).is_ok() {
                    match serde_json::from_slice::<Message>(&body) {
                        Ok(message) => deliver(Event::Message(message)),
                        Err(e) => {
                            why = format!("сообщение движка не разобралось: {e}");
                            break;
                        }
                    }
                }
            }
            *self.pipe.lock().unwrap_or_else(PoisonError::into_inner) = None;
            down(&why, &mut told);
            // Канал, закрытый сразу после знакомства, — отказ (например, открыто прежнее окно): тот же отказ
            // каждую секунду ничего не даст, следующая попытка позже.
            thread::sleep(if since.elapsed() < Duration::from_secs(2) {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(1)
            });
        }
    }

    /// Команда движку; номер для ответа или None — канала нет.
    pub fn ask(&self, command: &str, mut args: serde_json::Map<String, serde_json::Value>) -> Option<i64> {
        let pipe = self.pipe.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        args.insert("id".into(), id.into());
        args.insert("c".into(), command.into());
        // Не записалось — канал оборван; поток моста это увидит и скажет окну, команда считается не отправленной.
        pipe.send(&serde_json::Value::Object(args).to_string())
            .ok()
            .map(|()| id)
    }
}
