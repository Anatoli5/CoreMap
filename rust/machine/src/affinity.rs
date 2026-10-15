// Привязка потока к логическому процессору на время обращения к его регистрам.
//
// Модуль IntelMSR драйвера PawnIO читает и пишет регистр на том процессоре, на котором сейчас идёт вызывающий
// поток (исходники PawnIO: natives_impl_windows.cpp; KNOWLEDGE.md). Поэтому поток привязывается к ядру на всё
// обращение к почтовому ящику — от записи запроса до последнего чтения ответа — и потом возвращается к прежней маске.
#![expect(unsafe_code, reason = "thread affinity: Win32 call the standard library lacks")]

use windows_sys::Win32::System::Threading::{
    GetCurrentThread, GetThreadPriority, SetThreadAffinityMask, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};

use crate::Lp;

/// Поток привязан к одному логическому процессору; при выходе из области видимости прежняя маска возвращается.
#[derive(Debug)]
pub(crate) struct Pinned {
    previous: usize,
    priority: i32,
    // Привязка — свойство потока: вернуть маску должен тот же поток, поэтому `Pinned` в другой поток не передаётся.
    _thread_bound: std::marker::PhantomData<*const ()>,
}

/// Привязывает текущий поток к процессору. None — Windows отказала (процессор вне маски процесса) или номер
/// больше 63: группы процессоров не поддерживаются (как в C#-части, Topology.cs).
pub(crate) fn pin(lp: Lp) -> Option<Pinned> {
    let mask = 1usize.checked_shl(lp.0)?;
    // SAFETY: GetCurrentThread has no preconditions; it returns a pseudo-handle valid for the calling thread.
    let thread = unsafe { GetCurrentThread() };
    // На время обращения — высокий приоритет, как у потока датчиков в C# (Sensors.cs), и до привязки: переход на
    // нагруженное ядро и чтения на нём иначе ждали бы своей очереди за нагрузкой, которую автомодуль и регулирует
    // (ревизия, outputs/review-auto-2026-10-14.md M4).
    // SAFETY: the pseudo-handle of this thread; no other preconditions.
    let priority = unsafe { GetThreadPriority(thread) };
    // SAFETY: as above.
    unsafe { SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST) };
    // SAFETY: `thread` is the calling thread's pseudo-handle; the mask is a plain value.
    let previous = unsafe { SetThreadAffinityMask(thread, mask) };
    if previous == 0 {
        // SAFETY: as above; the priority read just before.
        unsafe { SetThreadPriority(thread, priority) };
        return None;
    }
    Some(Pinned {
        previous,
        priority,
        _thread_bound: std::marker::PhantomData,
    })
}

impl Drop for Pinned {
    fn drop(&mut self) {
        // SAFETY: GetCurrentThread has no preconditions.
        let thread = unsafe { GetCurrentThread() };
        // SAFETY: as in `pin`; `previous` is the mask Windows returned for this very thread.
        unsafe { SetThreadAffinityMask(thread, self.previous) };
        // SAFETY: as above; the priority read in `pin` for this very thread.
        unsafe { SetThreadPriority(thread, self.priority) };
    }
}
