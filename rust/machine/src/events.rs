// Системный журнал Windows: первое событие после данного времени, подходящее под условие (как Profile.FirstEvent).
// По нему правила запуска решают, не кончилось ли прошлое применение профиля сбоем (profile.rs).
#![expect(unsafe_code, reason = "the Windows event log API (wevtapi): Win32 calls")]

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, FILETIME, GetLastError};
use windows_sys::Win32::System::EventLog::{
    EVT_HANDLE, EVT_VARIANT, EvtClose, EvtCreateRenderContext, EvtNext, EvtQuery, EvtQueryChannelPath,
    EvtQueryForwardDirection, EvtRender, EvtRenderContextSystem, EvtRenderEventValues, EvtSystemEventID,
    EvtSystemProviderName, EvtSystemTimeCreated, EvtVarTypeFileTime, EvtVarTypeString, EvtVarTypeUInt16,
};

use crate::clock::{Stamp, from_filetime};

/// Сколько ждать ответа журнала: запрос идёт под арендой потолков, бесконечное ожидание задержало бы старт окна
/// (ревизия L9).
const LOG_WAIT_MS: u32 = 5000;

/// Событие журнала: поставщик, номер, время.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub provider: String,
    pub id: u16,
    pub at: Stamp,
}

/// Журнал, в котором правила запуска ищут сбои. Настоящий — `SystemLog`; в проверках — подставной.
pub trait EventLog: std::fmt::Debug {
    /// Первое (самое раннее) событие системного журнала не раньше `since`, подходящее под условие `XPath`
    /// (часть внутри `System[...]`). Ok(None) — таких нет; Err — журнал не прочитан (текст причины).
    fn first(&self, since: Stamp, condition: &str) -> Result<Option<Event>, String>;
}

/// Системный журнал этой машины.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLog;

/// Закрывается при выходе из области видимости.
struct Evt(EVT_HANDLE);

impl Drop for Evt {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: the handle came from an Evt* call and is closed exactly once, here.
            unsafe { EvtClose(self.0) };
        }
    }
}

fn last_error() -> u32 {
    // SAFETY: reads the calling thread's last-error value.
    unsafe { GetLastError() }
}

impl EventLog for SystemLog {
    fn first(&self, since: Stamp, condition: &str) -> Result<Option<Event>, String> {
        let query = format!(
            "*[System[TimeCreated[@SystemTime>='{}'] and ({condition})]]",
            since.utc_query()
        );
        let channel: Vec<u16> = "System".encode_utf16().chain([0]).collect();
        let query: Vec<u16> = query.encode_utf16().chain([0]).collect();
        // SAFETY: local session (0); channel and query are NUL-terminated and outlive the call.
        let results = Evt(unsafe {
            EvtQuery(
                0,
                channel.as_ptr(),
                query.as_ptr(),
                EvtQueryChannelPath | EvtQueryForwardDirection,
            )
        });
        if results.0 == 0 {
            return Err(format!("запрос к журналу Windows не принят, код {}", last_error()));
        }
        let mut raw: EVT_HANDLE = 0;
        let mut returned = 0u32;
        // SAFETY: room for exactly one event handle; `returned` is writable.
        if unsafe { EvtNext(results.0, 1, &raw mut raw, LOG_WAIT_MS, 0, &raw mut returned) } == 0 {
            let error = last_error();
            return if error == ERROR_NO_MORE_ITEMS {
                Ok(None)
            } else {
                Err(format!("журнал Windows не прочитан, код {error}"))
            };
        }
        let event = Evt(raw);
        // SAFETY: no value paths are passed for a system render context.
        let context = Evt(unsafe { EvtCreateRenderContext(0, null(), EvtRenderContextSystem) });
        if context.0 == 0 {
            return Err(format!(
                "журнал Windows: не создан разбор события, код {}",
                last_error()
            ));
        }
        render(&context, &event)
            .map(Some)
            .ok_or_else(|| format!("журнал Windows: событие не разобрано, код {}", last_error()))
    }
}

/// Поставщик, номер и время события из его системных свойств.
fn render(context: &Evt, event: &Evt) -> Option<Event> {
    let mut used = 0u32;
    let mut count = 0u32;
    // SAFETY: the first call only asks for the needed size (null buffer of size 0 is allowed).
    let ok = unsafe {
        EvtRender(
            context.0,
            event.0,
            EvtRenderEventValues,
            0,
            null_mut(),
            &raw mut used,
            &raw mut count,
        )
    };
    if ok == 0 && last_error() != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    // u64 elements keep the buffer aligned for EVT_VARIANT (8 bytes).
    let mut buffer = vec![0u64; usize::try_from(used).ok()?.div_ceil(8)];
    let size = u32::try_from(buffer.len() * 8).ok()?;
    // SAFETY: `buffer` is writable for `size` bytes and outlives every read below.
    let ok = unsafe {
        EvtRender(
            context.0,
            event.0,
            EvtRenderEventValues,
            size,
            buffer.as_mut_ptr().cast(),
            &raw mut used,
            &raw mut count,
        )
    };
    if ok == 0 {
        return None;
    }
    let values = usize::try_from(count).ok()?;
    let at = |index: i32| -> Option<EVT_VARIANT> {
        let index = usize::try_from(index).ok()?;
        if index >= values || (index + 1) * size_of::<EVT_VARIANT>() > buffer.len() * 8 {
            return None;
        }
        // SAFETY: the buffer holds `count` EVT_VARIANTs from its start (EvtRender's contract for system values) and
        // the bounds were checked above; the buffer is 8-byte aligned.
        let item = buffer.as_ptr().cast::<EVT_VARIANT>().wrapping_add(index);
        // SAFETY: as above — `item` points at the index-th EVT_VARIANT inside `buffer`.
        Some(unsafe { item.read() })
    };
    let provider = at(EvtSystemProviderName)?;
    let id = at(EvtSystemEventID)?;
    let time = at(EvtSystemTimeCreated)?;
    if provider.Type != u32::try_from(EvtVarTypeString).ok()?
        || id.Type != u32::try_from(EvtVarTypeUInt16).ok()?
        || time.Type != u32::try_from(EvtVarTypeFileTime).ok()?
    {
        return None;
    }
    // SAFETY: the type tags were checked: StringVal is a NUL-terminated UTF-16 string inside `buffer`.
    let name = unsafe { provider.Anonymous.StringVal };
    let provider = wide_to_string(name)?;
    // SAFETY: type tag checked above.
    let id = unsafe { id.Anonymous.UInt16Val };
    // SAFETY: type tag checked above; FileTimeVal is a 64-bit FILETIME value.
    let ticks = unsafe { time.Anonymous.FileTimeVal };
    let ft = FILETIME {
        dwLowDateTime: u32::try_from(ticks & 0xFFFF_FFFF).ok()?,
        dwHighDateTime: u32::try_from(ticks >> 32).ok()?,
    };
    Some(Event {
        provider,
        id,
        at: Stamp(from_filetime(ft)),
    })
}

fn wide_to_string(text: *const u16) -> Option<String> {
    if text.is_null() {
        return None;
    }
    let mut len = 0usize;
    // SAFETY: the string is NUL-terminated (EvtRender's contract) and lives in the caller's buffer.
    while unsafe { *text.wrapping_add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` UTF-16 units were just read from this string.
    let units = unsafe { std::slice::from_raw_parts(text, len) };
    Some(String::from_utf16_lossy(units))
}
