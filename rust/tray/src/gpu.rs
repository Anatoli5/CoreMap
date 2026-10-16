// Видеокарта — для журнала срезов: её тепло идёт в общий с процессором охладитель (владелец 15.10: «у нас же еще
// видеокарта есть и на нее тоже нагрузка меняется… надо… в уравнение еще как-то включить видеокарту»; DECISIONS #37).
//
// Источник — NVML, библиотека NVIDIA, которую ставит её драйвер (C:\Windows\System32\nvml.dll): температура, мощность и
// занятость, без прав администратора. Почему NVML, а не интерфейс Диспетчера задач (D3DKMT) и не что-то ещё, с
// замерами на машине владельца, — DECISIONS #37. Что пишется:
// - температура — °C в момент чтения (датчик NVML обновляется раз в секунду, замер 15.10);
// - мощность — средняя за промежуток с прошлого чтения, по счётчику энергии видеокарты (мДж с загрузки драйвера), как
//   мощность корпуса процессора; счётчика нет (видеокарты старше Volta) — мгновенная мощность драйвера;
// - занятость — доля 0…1, средняя за тот же промежуток по выборкам драйвера (он копит их раз в 0,2 с, около 14 с
//   назад; замер 15.10); выборок нет — мгновенная.
// Первое чтение и чтение после перерыва дольше 10 с промежутка не имеют: мощность и занятость — пусто (как доля работы
// ядра в machine::thermal).
//
// Библиотека грузится только из System32 (`LOAD_LIBRARY_SEARCH_SYSTEM32`): программа входа работает с правами
// администратора, а её папку может переписать любая программа владельца — подложенная nvml.dll рядом с exe не
// подхватится. Видеокарта — первая, которую видит NVML (на ноутбуке владельца она одна).
#![expect(
    unsafe_code,
    reason = "NVML is a C library loaded at run time: loading it and calling its functions is FFI"
)]

use std::ffi::{CStr, c_char, c_void};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use machine::HwError;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};

/// Показание видеокарты. None — не известно (первое чтение, нет такого датчика).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GpuReading {
    /// °C в момент чтения.
    pub temp: Option<f64>,
    /// Вт, средняя за промежуток с прошлого чтения (или мгновенная, если счётчика энергии нет).
    pub power: Option<f64>,
    /// Доля времени занятости 0…1 за тот же промежуток.
    pub busy: Option<f64>,
}

/// Видеокарта: настоящая (`Nvml`) или имитация (`SimGpu`).
pub trait Gpu: std::fmt::Debug {
    /// Одно чтение. Ошибка — текст для журнала; читатель после неё не годится (программа откроет новый).
    fn read(&mut self) -> Result<GpuReading, HwError>;
    /// Что читается и как — одной строкой для журнала программы при открытии.
    fn describe(&self) -> String;
}

/// Имитация для скрытой проверки: те же числа на каждое чтение или отказ.
#[derive(Debug, Clone)]
pub struct SimGpu {
    pub reading: GpuReading,
    pub fail: bool,
}

impl Gpu for SimGpu {
    fn read(&mut self) -> Result<GpuReading, HwError> {
        if self.fail {
            return Err(HwError("видеокарта не прочитана (проверка)".into()));
        }
        Ok(self.reading)
    }

    fn describe(&self) -> String {
        "имитация".into()
    }
}

/// Перерыв дольше — у чтения нет промежутка (как `GAP` в `machine::thermal`).
const GAP: Duration = Duration::from_secs(10);
/// Больше этого видеокарта ноутбука не берёт (у RTX 5080 Laptop предел 175 Вт — NVML, 15.10): больше — сброс счётчика
/// энергии (перезапуск драйвера), а не мощность.
const MOST_WATTS: f64 = 2000.0;

type Ret = i32; // nvmlReturn_t
type Device = *mut c_void; // nvmlDevice_t
const SUCCESS: Ret = 0;
const NOT_SUPPORTED: Ret = 3;
const NOT_FOUND: Ret = 6;
const INSUFFICIENT_SIZE: Ret = 7;
/// `NVML_TEMPERATURE_GPU`.
const TEMPERATURE_GPU: i32 = 0;
/// `NVML_GPU_UTILIZATION_SAMPLES`.
const UTILIZATION_SAMPLES: i32 = 1;

/// `nvmlSample_t`: время выборки (мкс) и значение — объединение `nvmlValue_t` из 8 байт, его тип сообщает вызов.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct Sample {
    stamp: u64,
    value: u64,
}

type Init = unsafe extern "C" fn() -> Ret;
type Shutdown = unsafe extern "C" fn() -> Ret;
type ErrorString = unsafe extern "C" fn(Ret) -> *const c_char;
type Count = unsafe extern "C" fn(*mut u32) -> Ret;
type Handle = unsafe extern "C" fn(u32, *mut Device) -> Ret;
type Name = unsafe extern "C" fn(Device, *mut c_char, u32) -> Ret;
type Temperature = unsafe extern "C" fn(Device, i32, *mut u32) -> Ret;
type Energy = unsafe extern "C" fn(Device, *mut u64) -> Ret;
type PowerUsage = unsafe extern "C" fn(Device, *mut u32) -> Ret;
type Samples = unsafe extern "C" fn(Device, i32, u64, *mut i32, *mut u32, *mut Sample) -> Ret;
type Utilization = unsafe extern "C" fn(Device, *mut [u32; 2]) -> Ret;

/// Функции NVML, которые берёт программа (подписи — nvml.h).
#[derive(Debug, Clone, Copy)]
struct Calls {
    shutdown: Shutdown,
    error_string: ErrorString,
    temperature: Temperature,
    energy: Energy,
    power_usage: PowerUsage,
    samples: Samples,
    utilization: Utilization,
}

/// Как читается мощность: по счётчику энергии (мДж) или мгновенная.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerFrom {
    Energy,
    Instant,
}

/// Видеокарта через NVML. NVML открыт (`nvmlInit`), пока жив этот объект; `Drop` его закрывает.
#[derive(Debug)]
pub struct Nvml {
    calls: Calls,
    device: Device,
    name: String,
    power_from: PowerFrom,
    /// Выборки занятости есть (иначе — мгновенная занятость).
    sampled: bool,
    /// Буфер выборок — один на все чтения (вызов кладёт в него до его длины).
    buffer: Vec<Sample>,
    /// Прошлое чтение: когда, счётчик энергии (мДж), время последней выборки занятости (мкс драйвера).
    last: Option<Instant>,
    energy: Option<u64>,
    seen: u64,
}

impl Nvml {
    /// Грузит nvml.dll из System32, открывает NVML и первую видеокарту; узнаёт, есть ли у неё счётчик энергии и выборки
    /// занятости.
    pub fn open() -> Result<Self, HwError> {
        let wide: Vec<u16> = "nvml.dll".encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 name that outlives the call; the flag limits the search to
        // System32 (the elevated logon program must not load a DLL planted next to its exe).
        let dll = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if dll.is_null() {
            return Err(HwError(
                "нет nvml.dll в System32 — драйвера NVIDIA нет, столбцы видеокарты пустые".into(),
            ));
        }
        // The library stays loaded for the life of the process: the function pointers below point into it, and
        // NVML may keep its own threads.
        let init: Init = function(dll, c"nvmlInit_v2")?;
        let count: Count = function(dll, c"nvmlDeviceGetCount_v2")?;
        let handle: Handle = function(dll, c"nvmlDeviceGetHandleByIndex_v2")?;
        let name: Name = function(dll, c"nvmlDeviceGetName")?;
        let calls = Calls {
            shutdown: function(dll, c"nvmlShutdown")?,
            error_string: function(dll, c"nvmlErrorString")?,
            temperature: function(dll, c"nvmlDeviceGetTemperature")?,
            energy: function(dll, c"nvmlDeviceGetTotalEnergyConsumption")?,
            power_usage: function(dll, c"nvmlDeviceGetPowerUsage")?,
            samples: function(dll, c"nvmlDeviceGetSamples")?,
            utilization: function(dll, c"nvmlDeviceGetUtilizationRates")?,
        };
        // SAFETY: nvmlInit_v2(void) from the DLL just loaded; it may be called again after nvmlShutdown.
        let ret = unsafe { init() };
        if ret != SUCCESS {
            return Err(HwError(format!(
                "NVML не открылась: {}",
                describe_error(calls.error_string, ret)
            )));
        }
        // From here on NVML is open: the object owns that and closes it in Drop, also on the error paths below.
        let mut nvml = Self {
            calls,
            device: std::ptr::null_mut(),
            name: String::new(),
            power_from: PowerFrom::Energy,
            sampled: true,
            buffer: Vec::new(),
            last: None,
            energy: None,
            seen: 0,
        };
        let mut devices = 0_u32;
        // SAFETY: nvmlDeviceGetCount_v2(unsigned int*): `devices` is writable for the call.
        nvml.check(unsafe { count(&raw mut devices) }, "число видеокарт")?;
        if devices == 0 {
            return Err(HwError("NVML не видит ни одной видеокарты NVIDIA".into()));
        }
        let mut device: Device = std::ptr::null_mut();
        // SAFETY: nvmlDeviceGetHandleByIndex_v2(unsigned int, nvmlDevice_t*): index 0 < count; the handle is
        // written into `device`.
        nvml.check(unsafe { handle(0, &raw mut device) }, "видеокарта 0")?;
        nvml.device = device;
        let mut text = [0_u8; 96]; // NVML_DEVICE_NAME_V2_BUFFER_SIZE
        // SAFETY: nvmlDeviceGetName(nvmlDevice_t, char*, unsigned int): the buffer is writable for its stated length
        // and NVML NUL-terminates what it writes.
        let ret = unsafe { name(nvml.device, text.as_mut_ptr().cast(), 96) };
        nvml.name = if ret == SUCCESS {
            CStr::from_bytes_until_nul(&text)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            "видеокарта NVIDIA".into()
        };
        // Есть ли счётчик энергии (Volta и новее) и выборки занятости: проба одним вызовом каждого.
        let mut energy = 0_u64;
        // SAFETY: nvmlDeviceGetTotalEnergyConsumption(nvmlDevice_t, unsigned long long*): `energy` is writable.
        match unsafe { (nvml.calls.energy)(nvml.device, &raw mut energy) } {
            SUCCESS => {}
            NOT_SUPPORTED => nvml.power_from = PowerFrom::Instant,
            ret => nvml.check(ret, "счётчик энергии")?,
        }
        let (mut kind, mut room) = (0_i32, 0_u32);
        // SAFETY: nvmlDeviceGetSamples with a null buffer only reports how many samples it holds into `room`;
        // `kind` and `room` are writable.
        match unsafe {
            (nvml.calls.samples)(
                nvml.device,
                UTILIZATION_SAMPLES,
                0,
                &raw mut kind,
                &raw mut room,
                std::ptr::null_mut(),
            )
        } {
            SUCCESS => nvml.buffer = vec![Sample::default(); usize::try_from(room).unwrap_or(0).max(16) + 16],
            NOT_SUPPORTED | NOT_FOUND => nvml.sampled = false,
            ret => nvml.check(ret, "выборки занятости")?,
        }
        Ok(nvml)
    }

    /// Ошибка вызова — текстом NVML.
    fn check(&self, ret: Ret, what: &str) -> Result<(), HwError> {
        if ret == SUCCESS {
            Ok(())
        } else {
            Err(self.error(ret, what))
        }
    }

    fn error(&self, ret: Ret, what: &str) -> HwError {
        HwError(format!(
            "NVML, {what}: {}",
            describe_error(self.calls.error_string, ret)
        ))
    }

    /// Мощность: средняя по счётчику энергии с прошлого чтения (`seconds` — промежуток; None — промежутка нет) или
    /// мгновенная.
    fn power(&mut self, seconds: Option<f64>) -> Result<Option<f64>, HwError> {
        if self.power_from == PowerFrom::Instant {
            let mut milliwatts = 0_u32;
            // SAFETY: nvmlDeviceGetPowerUsage(nvmlDevice_t, unsigned int*): `milliwatts` is writable.
            let ret = unsafe { (self.calls.power_usage)(self.device, &raw mut milliwatts) };
            self.check(ret, "мощность")?;
            return Ok(Some(f64::from(milliwatts) / 1000.0));
        }
        let mut energy = 0_u64;
        // SAFETY: nvmlDeviceGetTotalEnergyConsumption(nvmlDevice_t, unsigned long long*): `energy` is writable.
        let ret = unsafe { (self.calls.energy)(self.device, &raw mut energy) };
        self.check(ret, "счётчик энергии")?;
        let previous = self.energy.replace(energy);
        Ok(previous
            .zip(seconds)
            .and_then(|(was, seconds)| mean_watts(was, energy, seconds)))
    }

    /// Занятость: средняя по выборкам драйвера новее прошлого чтения (`interval` — есть ли промежуток) или мгновенная.
    fn busy(&mut self, interval: bool) -> Result<Option<f64>, HwError> {
        if !self.sampled {
            let mut rates = [0_u32; 2]; // nvmlUtilization_t: gpu, memory (%)
            // SAFETY: nvmlDeviceGetUtilizationRates(nvmlDevice_t, nvmlUtilization_t*): two unsigned ints, writable.
            let ret = unsafe { (self.calls.utilization)(self.device, &raw mut rates) };
            self.check(ret, "занятость")?;
            return Ok(Some(f64::from(rates[0].min(100)) / 100.0));
        }
        let (mut kind, mut got) = (0_i32, u32::try_from(self.buffer.len()).unwrap_or(u32::MAX));
        // SAFETY: nvmlDeviceGetSamples(device, type, lastSeen, nvmlValueType_t*, unsigned int* count, nvmlSample_t*):
        // `got` holds the buffer's length on entry and the number written on return; the buffer outlives the call.
        let ret = unsafe {
            (self.calls.samples)(
                self.device,
                UTILIZATION_SAMPLES,
                self.seen,
                &raw mut kind,
                &raw mut got,
                self.buffer.as_mut_ptr(),
            )
        };
        match ret {
            SUCCESS => {}
            // Новых выборок нет — занятость за промежуток не известна.
            NOT_FOUND => return Ok(None),
            // Драйвер стал хранить больше выборок: буфер — больше; эти — в следующем чтении.
            INSUFFICIENT_SIZE => {
                self.buffer
                    .resize(usize::try_from(got).unwrap_or(0) + 16, Sample::default());
                return Ok(None);
            }
            ret => return Err(self.error(ret, "выборки занятости")),
        }
        let fresh = &self.buffer[..usize::try_from(got).unwrap_or(0).min(self.buffer.len())];
        let busy = mean_busy(fresh, kind, self.seen);
        if let Some(newest) = fresh.iter().map(|s| s.stamp).max() {
            self.seen = self.seen.max(newest);
        }
        Ok(busy.filter(|_| interval))
    }
}

impl Gpu for Nvml {
    fn read(&mut self) -> Result<GpuReading, HwError> {
        let now = Instant::now();
        let seconds = self
            .last
            .map(|at| now.duration_since(at))
            .filter(|gap| *gap <= GAP)
            .map(|gap| gap.as_secs_f64());
        self.last = Some(now);
        let mut celsius = 0_u32;
        // SAFETY: nvmlDeviceGetTemperature(nvmlDevice_t, nvmlTemperatureSensors_t, unsigned int*): `celsius` is
        // writable.
        let ret = unsafe { (self.calls.temperature)(self.device, TEMPERATURE_GPU, &raw mut celsius) };
        self.check(ret, "температура")?;
        let power = self.power(seconds)?;
        let busy = self.busy(seconds.is_some())?;
        Ok(GpuReading {
            temp: Some(f64::from(celsius)),
            power,
            busy,
        })
    }

    fn describe(&self) -> String {
        let mut text = format!("{} через NVML; мощность ", self.name);
        text.push_str(match self.power_from {
            PowerFrom::Energy => "— средняя по счётчику энергии",
            PowerFrom::Instant => "— мгновенная (счётчика энергии у этой видеокарты нет)",
        });
        let _ = write!(
            text,
            "; занятость — {}",
            if self.sampled {
                "средняя по выборкам драйвера"
            } else {
                "мгновенная (выборок у этой видеокарты нет)"
            }
        ); // into a String: cannot fail
        text
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // Код возврата не нужен: закрытие не удалось — сказать некому (объект уходит), а следующий `open` начнёт
        // заново со своего nvmlInit.
        // SAFETY: nvmlShutdown(void) pairs the nvmlInit_v2 that `open` made before creating this object.
        unsafe { (self.calls.shutdown)() };
    }
}

/// Текст ошибки NVML: «код N: текст NVML».
fn describe_error(error_string: ErrorString, ret: Ret) -> String {
    // SAFETY: nvmlErrorString(nvmlReturn_t) returns a pointer to a static NUL-terminated string (or null).
    let text = unsafe { error_string(ret) };
    if text.is_null() {
        return format!("код {ret}");
    }
    // SAFETY: non-null, NUL-terminated and static per nvml.h; read once here.
    let text = unsafe { CStr::from_ptr(text) };
    format!("код {ret}: {}", text.to_string_lossy())
}

/// Функция библиотеки по имени; нет такой — ошибка с её именем.
fn function<T: Copy>(dll: HMODULE, name: &CStr) -> Result<T, HwError> {
    // SAFETY: `dll` is a loaded module handle and `name` a NUL-terminated ASCII name.
    let found = unsafe { GetProcAddress(dll, name.as_ptr().cast()) };
    let Some(found) = found else {
        return Err(HwError(format!(
            "в nvml.dll нет функции {} — драйвер NVIDIA слишком старый",
            name.to_string_lossy()
        )));
    };
    assert_eq!(
        size_of::<T>(),
        size_of::<unsafe extern "system" fn() -> isize>(),
        "only function pointer types are read from the DLL"
    );
    // SAFETY: T is one of the function pointer types above, the same size as FARPROC; its signature is the one
    // nvml.h declares for that name (x64 Windows has one calling convention).
    Ok(unsafe { std::mem::transmute_copy::<unsafe extern "system" fn() -> isize, T>(&found) })
}

/// Средняя мощность, Вт, между двумя показаниями счётчика энергии (мДж) `seconds` секунд друг от друга. None —
/// промежутка нет, счётчик не сдвинулся (видеокарта не бывает при нуле ватт — счётчик стоит) или пошёл назад
/// (драйвер перезапущен), вышло больше `MOST_WATTS`.
fn mean_watts(previous: u64, now: u64, seconds: f64) -> Option<f64> {
    if seconds.is_nan() || seconds <= 0.0 || now <= previous {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "millijoules over seconds: far below 2^52")]
    let watts = (now - previous) as f64 / 1000.0 / seconds;
    (watts <= MOST_WATTS).then_some(watts)
}

/// Значение выборки по его типу (`nvmlValueType_t`; объединение `nvmlValue_t` — младшие байты 8-байтового поля).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "reinterpreting the 8-byte union by its declared type: the cast is the union read"
)]
fn sample_value(kind: i32, bits: u64) -> Option<f64> {
    match kind {
        0 => Some(f64::from_bits(bits)),          // double
        1 | 2 => Some(f64::from(bits as u32)),    // unsigned int, unsigned long (32 bits on Windows)
        3 => Some(bits as f64),                   // unsigned long long
        4 => Some(bits as i64 as f64),            // signed long long
        5 => Some(f64::from(bits as u32 as i32)), // signed int
        6 => Some(f64::from(bits as u16)),        // unsigned short
        _ => None,
    }
}

/// Средняя занятость (доля 0…1) по выборкам новее `after`; None — таких нет или тип значения не известен.
fn mean_busy(samples: &[Sample], kind: i32, after: u64) -> Option<f64> {
    let (sum, n) = samples
        .iter()
        .filter(|s| s.stamp > after)
        .filter_map(|s| sample_value(kind, s.value))
        .filter(|v| v.is_finite())
        .fold((0.0, 0_u32), |(sum, n), v| (sum + v.clamp(0.0, 100.0), n + 1));
    (n > 0).then(|| sum / f64::from(n) / 100.0)
}

#[cfg(test)]
mod tests {
    // По nvml.h: счётчик энергии — мДж с загрузки драйвера (не переполняется); выборки — время в мкс и значение
    // объединением, тип которого сообщает вызов; занятость — проценты. Ожидаемые числа посчитаны отдельно, не формулой.
    use super::*;

    fn sample(stamp: u64, percent: u32) -> Sample {
        Sample {
            stamp,
            value: u64::from(percent),
        }
    }

    #[test]
    fn power_is_energy_over_the_interval() {
        // 200 J in 2 s = 100 W.
        let w = mean_watts(4_935_675_158, 4_935_875_158, 2.0).expect("an interval");
        assert!((w - 100.0).abs() < 1e-9, "{w}");
    }

    #[test]
    fn no_interval_a_still_or_reset_counter_give_no_power() {
        assert_eq!(mean_watts(1000, 3000, 0.0), None);
        assert_eq!(
            mean_watts(5000, 5000, 2.0),
            None,
            "a counter that did not move is not 0 W"
        );
        assert_eq!(
            mean_watts(5_000_000, 1000, 2.0),
            None,
            "driver restarted: counter went back"
        );
        assert_eq!(mean_watts(0, 10_000_000, 2.0), None, "5000 W is not a laptop GPU");
    }

    #[test]
    fn busy_is_the_mean_of_the_samples_after_the_last_read() {
        // Shape of the measured buffer (15.10): unsigned int percent every 0.2 s.
        let samples = [sample(100, 90), sample(300, 60), sample(500, 70), sample(700, 50)];
        let busy = mean_busy(&samples, 1, 300).expect("two fresh samples");
        assert!((busy - 0.60).abs() < 1e-12, "(70 + 50) / 2 % = 0.60, got {busy}");
        assert_eq!(mean_busy(&samples, 1, 700), None, "nothing newer than the last read");
        assert_eq!(mean_busy(&samples, 99, 0), None, "unknown value type");
    }

    #[test]
    fn sample_values_are_read_by_their_declared_type() {
        assert_eq!(sample_value(1, 0xFFFF_FFFF_0000_0041), Some(65.0), "uint: low 32 bits");
        assert_eq!(sample_value(0, 65.5_f64.to_bits()), Some(65.5), "double");
        assert_eq!(
            sample_value(5, u64::from((-3_i32).cast_unsigned())),
            Some(-3.0),
            "signed int"
        );
    }

    /// На настоящей видеокарте (только чтение, без прав администратора): `cargo test --release -p coremap-tray --
    /// --ignored real_nvml --nocapture`. Не в обычном прогоне: на машине без NVIDIA он упал бы, а не проверил.
    #[test]
    #[ignore = "reads the real GPU through NVML; run by hand on a machine with an NVIDIA driver"]
    fn real_nvml_reads_temperature_then_interval_means() {
        let mut gpu = Nvml::open().expect("NVML opens");
        println!("{}", gpu.describe());
        let first = gpu.read().expect("first read");
        println!("first: {first:?}");
        assert!(first.temp.is_some_and(|t| (10.0..=110.0).contains(&t)));
        assert_eq!((first.power, first.busy), (None, None), "no interval yet");
        std::thread::sleep(Duration::from_secs(2));
        let second = gpu.read().expect("second read");
        println!("second: {second:?}");
        assert!(second.power.is_some_and(|w| w > 0.0 && w < MOST_WATTS), "{second:?}");
        assert!(second.busy.is_some_and(|b| (0.0..=1.0).contains(&b)), "{second:?}");
        std::thread::sleep(Duration::from_secs(2));
        let started = Instant::now();
        let third = gpu.read().expect("third read");
        println!("third: {third:?}; read took {:?}", started.elapsed());
    }

    #[test]
    fn a_failing_reader_says_so() {
        let mut gpu = SimGpu {
            reading: GpuReading::default(),
            fail: true,
        };
        assert!(gpu.read().is_err());
    }
}
