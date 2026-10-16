// Экраны Windows для места окна (saved.rs): на каком экране полоса заголовка и какова его рабочая область (без панели
// задач). В стандартной библиотеке и в Slint 1.18 этого нет (список экранов есть только у winit, а он закрыт за
// нестабильным признаком Slint) — остаются два вызова Windows, отсюда unsafe.
#![expect(
    unsafe_code,
    reason = "Win32 calls the standard library lacks: MonitorFromRect and GetMonitorInfoW"
)]

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromRect};

use crate::saved::Rect;

/// Рабочая область экрана, на котором больше всего прямоугольника `on`; None — он ни на одном экране.
pub fn work_area(on: Rect) -> Option<Rect> {
    let rect = RECT {
        left: on.left,
        top: on.top,
        right: on.right,
        bottom: on.bottom,
    };
    // SAFETY: `rect` is a valid RECT on this frame, read during the call only.
    let monitor = unsafe { MonitorFromRect(&raw const rect, MONITOR_DEFAULTTONULL) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(0),
        rcMonitor: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        rcWork: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        dwFlags: 0,
    };
    // SAFETY: `monitor` came from MonitorFromRect just now and is not null; `info` is writable, cbSize set as required.
    if unsafe { GetMonitorInfoW(monitor, &raw mut info) } == 0 {
        return None;
    }
    let w = info.rcWork;
    Some(Rect {
        left: w.left,
        top: w.top,
        right: w.right,
        bottom: w.bottom,
    })
}
