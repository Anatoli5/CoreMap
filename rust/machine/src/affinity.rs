// Привязка потока к логическому процессору на время обращения к его регистрам.
//
// Модуль IntelMSR драйвера PawnIO читает и пишет регистр на том процессоре, на котором сейчас идёт вызывающий
// поток (исходники PawnIO: natives_impl_windows.cpp; KNOWLEDGE.md). Поэтому поток привязывается к ядру на всё
// обращение к почтовому ящику — от записи запроса до последнего чтения ответа — и потом возвращается к прежней маске.
#![expect(unsafe_code, reason = "thread affinity: Win32 call the standard library lacks")]

use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

use crate::Lp;

/// Поток привязан к одному логическому процессору; при выходе из области видимости прежняя маска возвращается.
#[derive(Debug)]
pub(crate) struct Pinned {
    previous: usize,
    // Привязка — свойство потока: вернуть маску должен тот же поток, поэтому `Pinned` в другой поток не передаётся.
    _thread_bound: std::marker::PhantomData<*const ()>,
}

/// Привязывает текущий поток к процессору. None — Windows отказала (процессор вне маски процесса) или номер
/// больше 63: группы процессоров не поддерживаются (как в C#-части, Topology.cs).
pub(crate) fn pin(lp: Lp) -> Option<Pinned> {
    let mask = 1usize.checked_shl(lp.0)?;
    // SAFETY: GetCurrentThread has no preconditions; it returns a pseudo-handle valid for the calling thread.
    let thread = unsafe { GetCurrentThread() };
    // SAFETY: `thread` is the calling thread's pseudo-handle; the mask is a plain value.
    let previous = unsafe { SetThreadAffinityMask(thread, mask) };
    (previous != 0).then_some(Pinned {
        previous,
        _thread_bound: std::marker::PhantomData,
    })
}

impl Drop for Pinned {
    fn drop(&mut self) {
        // SAFETY: GetCurrentThread has no preconditions.
        let thread = unsafe { GetCurrentThread() };
        // SAFETY: as in `pin`; `previous` is the mask Windows returned for this very thread.
        unsafe { SetThreadAffinityMask(thread, self.previous) };
    }
}
