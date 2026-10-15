// Драйвер PawnIO и его модуль IntelMSR: чтение и запись регистров процессора (как класс PawnIo в Sensors.cs).
//
// Почему через библиотеку автора PawnIOLib.dll, а не прямыми запросами к драйверу (так делает LibreHardwareMonitor):
// раскладку данных запроса на запись тогда собирали бы мы сами, и ошибка в ней — неверное значение в регистре
// процессора (DECISIONS #31). Библиотека грузится по полному пути из папки Program Files, которую называет реестр
// машины (HKLM, писать туда может только администратор): не из переменной окружения и не из папки поиска — процесс
// с правами администратора не должен подхватить подложенную библиотеку.
//
// Модуль пускает запись только в свой короткий список регистров (0x150 среди них); что именно пишется, решают
// mailbox.rs и limits.rs.
#![expect(
    unsafe_code,
    reason = "PawnIOLib.dll is a C library: loading it and calling its functions is FFI"
)]

use std::ffi::{CStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};

use crate::HwError;

const ACCESS_DENIED: i32 = 0x8007_0005_u32.cast_signed();

type Open = unsafe extern "system" fn(*mut HANDLE) -> i32;
type Load = unsafe extern "system" fn(HANDLE, *const u8, usize) -> i32;
type Execute = unsafe extern "system" fn(HANDLE, *const u8, *const u64, usize, *mut u64, usize, *mut usize) -> i32;
type Close = unsafe extern "system" fn(HANDLE) -> i32;

/// Регистр процессора: прочитать и записать на том ядре, к которому привязан вызывающий поток.
/// Настоящий — `PawnIo`; в проверках почтового ящика — поддельный.
pub trait Msr {
    fn read(&mut self, msr: u32) -> Result<u64, HwError>;
    fn write(&mut self, msr: u32, value: u64) -> Result<(), HwError>;
    /// Отпустить драйвер до следующего обращения; поддельному регистру отпускать нечего.
    fn close(&mut self) {}
}

/// Открытый драйвер с загруженным модулем `IntelMSR`. Дескриптор можно закрыть между действиями (`close`):
/// следующее обращение откроет его заново — так делает резидент, который трогает процессор только при входе в
/// Windows, по щелчку в меню и после сна.
#[derive(Debug)]
pub struct PawnIo {
    open: Open,
    load: Load,
    execute: Execute,
    close: Close,
    module: PathBuf,
    handle: HANDLE, // null — закрыт
}

impl PawnIo {
    /// Грузит PawnIOLib.dll и открывает драйвер с модулем `module` (modules\IntelMSR.bin рядом с программой).
    pub fn new(module: &Path) -> Result<Self, HwError> {
        let lib = program_files()?.join("PawnIO").join("PawnIOLib.dll");
        if !lib.is_file() {
            return Err(HwError(format!("драйвер PawnIO не установлен: нет {}", lib.display())));
        }
        if !module.is_file() {
            return Err(HwError(format!("нет файла модуля {}", module.display())));
        }
        let wide: Vec<u16> = lib.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call; the flags restrict the search for
        // the DLL's own dependencies to its folder and System32.
        let dll = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if dll.is_null() {
            return Err(HwError(format!("не загрузилась библиотека {}", lib.display())));
        }
        // The library stays loaded for the life of the process: the function pointers below point into it.
        let mut io = Self {
            open: function(dll, c"pawnio_open", &lib)?,
            load: function(dll, c"pawnio_load", &lib)?,
            execute: function(dll, c"pawnio_execute", &lib)?,
            close: function(dll, c"pawnio_close", &lib)?,
            module: module.to_path_buf(),
            handle: null_mut(),
        };
        io.connect()?;
        Ok(io)
    }

    /// Открывает драйвер и загружает в него модуль: модуль живёт вместе с дескриптором.
    fn connect(&mut self) -> Result<(), HwError> {
        // Модуль читается до открытия драйвера: не прочитался — открытый дескриптор не повиснет (ревизия, п. 8).
        let blob = std::fs::read(&self.module)
            .map_err(|e| HwError(format!("модуль {} не прочитан: {e}", self.module.display())))?;
        let mut handle: HANDLE = null_mut();
        // SAFETY: `open` is pawnio_open from PawnIOLib.dll (HRESULT pawnio_open(PHANDLE)); `handle` is writable.
        let hr = unsafe { (self.open)(&raw mut handle) };
        if hr == ACCESS_DENIED {
            return Err(HwError("программа запущена без прав администратора".into()));
        }
        if hr != 0 {
            return Err(HwError(format!("PawnIO не открылся, код 0x{hr:08X}")));
        }
        // SAFETY: pawnio_load(HANDLE, const UCHAR*, SIZE_T): `handle` was just opened, `blob` outlives the call and
        // its length is passed with it.
        let hr = unsafe { (self.load)(handle, blob.as_ptr(), blob.len()) };
        if hr != 0 {
            // SAFETY: the handle came from pawnio_open above and is closed exactly once here.
            unsafe { (self.close)(handle) };
            return Err(HwError(format!("модуль IntelMSR не загрузился, код 0x{hr:08X}")));
        }
        self.handle = handle;
        Ok(())
    }

    fn ready(&mut self) -> Result<HANDLE, HwError> {
        if self.handle.is_null() {
            self.connect()?;
        }
        Ok(self.handle)
    }
}

impl Msr for PawnIo {
    fn read(&mut self, msr: u32) -> Result<u64, HwError> {
        let handle = self.ready()?;
        let input = [u64::from(msr)];
        let mut output = [0u64; 1];
        let mut got = 0usize;
        // SAFETY: pawnio_execute(HANDLE, PCSTR, const ULONG64*, SIZE_T, PULONG64, SIZE_T, PSIZE_T): the name is a
        // NUL-terminated C string, sizes are counts of 64-bit words matching the arrays, all outlive the call.
        let hr = unsafe {
            (self.execute)(
                handle,
                c"ioctl_read_msr".as_ptr().cast(),
                input.as_ptr(),
                input.len(),
                output.as_mut_ptr(),
                output.len(),
                &raw mut got,
            )
        };
        if hr != 0 {
            return Err(HwError(format!("чтение регистра 0x{msr:X} не удалось, код 0x{hr:08X}")));
        }
        // Драйвер сообщает, сколько слов ответа записал; ни одного — в ответе пусто, и показанием регистра он не
        // станет (то же правило в Sensors.cs).
        if got == 0 {
            return Err(HwError(format!(
                "чтение регистра 0x{msr:X}: драйвер вернул пустой ответ"
            )));
        }
        Ok(output[0])
    }

    fn write(&mut self, msr: u32, value: u64) -> Result<(), HwError> {
        // Модуль IntelMSR пустил бы запись и в регистры мощности и напряжения (0x601, 0x607, 0x608, 0x610, 0x1A4);
        // этой библиотеке нужен только почтовый ящик потолков. Другой регистр — отказ здесь, а не соглашение
        // вызывающих (ревизия, outputs/review-write-path-2026-10-14.md п. 1).
        if msr != crate::mailbox::REGISTER {
            return Err(HwError(format!(
                "запись в регистр 0x{msr:X} этой программе не разрешена"
            )));
        }
        let handle = self.ready()?;
        let input = [u64::from(msr), value];
        let mut got = 0usize;
        // SAFETY: as in `read`; no output words are asked for, so a null output pointer with size 0 is passed.
        let hr = unsafe {
            (self.execute)(
                handle,
                c"ioctl_write_msr".as_ptr().cast(),
                input.as_ptr(),
                input.len(),
                null_mut(),
                0,
                &raw mut got,
            )
        };
        if hr != 0 {
            return Err(HwError(format!("запись регистра 0x{msr:X} не удалась, код 0x{hr:08X}")));
        }
        Ok(())
    }

    /// Закрывает дескриптор до следующего обращения к регистрам.
    fn close(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: the handle came from pawnio_open and is set to null right after, so it is closed once.
        unsafe { (self.close)(self.handle) };
        self.handle = null_mut();
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        Msr::close(self);
    }
}

/// Функция библиотеки по имени.
fn function<T: Copy>(dll: HMODULE, name: &CStr, lib: &Path) -> Result<T, HwError> {
    // SAFETY: `dll` is a loaded module handle and `name` a NUL-terminated ASCII name.
    let found = unsafe { GetProcAddress(dll, name.as_ptr().cast()) };
    let Some(found) = found else {
        return Err(HwError(format!(
            "в {} нет функции {}",
            lib.display(),
            name.to_string_lossy()
        )));
    };
    assert_eq!(
        size_of::<T>(),
        size_of::<unsafe extern "system" fn() -> isize>(),
        "only function pointer types are read from the DLL"
    );
    // SAFETY: T is one of the function pointer types above, the same size as FARPROC; the signature is the one
    // declared in PawnIOLib.h for that name.
    Ok(unsafe { std::mem::transmute_copy::<unsafe extern "system" fn() -> isize, T>(&found) })
}

/// Папка Program Files этой машины — из реестра HKLM, а не из переменной окружения (её может задать пользователь).
fn program_files() -> Result<PathBuf, HwError> {
    let key: Vec<u16> = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion"
        .encode_utf16()
        .chain([0])
        .collect();
    let value: Vec<u16> = "ProgramFilesDir".encode_utf16().chain([0]).collect();
    let mut buffer = [0u16; 260];
    let mut bytes = u32::try_from(size_of_val(&buffer)).unwrap_or(u32::MAX);
    // SAFETY: key and value names are NUL-terminated and outlive the call; `buffer` is writable for `bytes` bytes;
    // RRF_RT_REG_SZ guarantees a NUL-terminated string on success.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &raw mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(HwError(format!(
            "папка Program Files не прочитана из реестра, код {status}"
        )));
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(PathBuf::from(OsString::from_wide(&buffer[..len])))
}
