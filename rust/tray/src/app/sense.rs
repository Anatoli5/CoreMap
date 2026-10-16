// Чтение датчиков — один путь на всех, и журнал срезов (выбор владельца 15.10, вариант А: строка раз в 0,25 с).
//
// Пока пишется журнал срезов — автомодуль включён и его условие выполнено (корпус близко к пределу, блок не на
// принятом или процессор занят ≥ 10 %, потолки прочитаны; решает опрос автомодуля раз в 2 с, app/auto.rs), — опрос
// идёт раз в 0,25 с (`AutoTiming::sample`): корпус и первые ядра блоков. Пока открыто окно, каждый второй опрос —
// опрос окна (дважды в секунду, как таймер состояния окна): он читает все ядра и сразу отдаёт окну средние под
// нагрузкой и состояние (app/serve.rs). В промежуточных опросах остальные ядра никому не нужны: строке и автомодулю
// нужны первые ядра блоков, а у кластера E-ядер датчик температуры общий (ревизия 15.10, п. 5). Каждое чтение ядра
// (`record`) идёт в сумму автомодуля и последнее показание (app/meter.rs) и в сумму окна. Строка среза — показания
// этого опроса: температуры — в момент чтения, доля работы и фактический множитель — за промежуток с прошлого опроса
// (по счётчикам ядра), время — с миллисекундами (таймеры Windows дрожат на 16 мс). Автомодуль раз в 2 с и окно раз в
// полсекунды берут каждый свою сумму: сами датчики в это время не читают.
//
// Журнал не пишется — опроса раз в 0,25 с нет, всё как до него: окно само читает все ядра раз в полсекунды,
// автомодуль раз в 2 с — корпус (и блоки, когда условие выполнено и журнал вот-вот начнётся).
//
// Обороты, пока пишется журнал, — раз в 2 с (вызов прошивки — около 30 мс, замер на машине владельца), в строке —
// только той, где прочитаны; в остальных пусто (окно считает пустое неизвестным). Цена опроса — строкой в tray.log
// раз в минуту: так она меряется на машине владельца.
//
// Входы общего теплового пакета (DECISIONS #37; владелец 15.10: «общий тепловой пакет… данные которые релевантны и
// которым у нас есть доступ нужно все этого добавить»): мощность корпуса — в каждой строке, средняя за промежуток с
// прошлой строки (счётчик энергии 0x611, то же чтение под арендой, что и ядра); видеокарта (температура, мощность и
// занятость, NVML, crate::gpu) — раз в 2 с, как обороты, только в строке, где прочитана. Не прочиталось — пусто и одна
// строка в error.log на одну и ту же беду; журнал идёт дальше.
//
// Файл дня открыт, пока пишется журнал. Не открылся или не принял строку — одна строка ошибки и следующая попытка
// через 10 с, а не в каждом опросе (ревизия 15.10, п. 7). Раз в 2 с — тот ли это файл, что лежит под своим именем:
// удалённый или перемещённый открытый файл Windows принимает строки дальше без ошибки (проверено на этой машине
// 15.10), и они ушли бы в никуда или под чужое имя (п. 8). Строки этой сборки идут только под свой заголовок: в файле
// дня с другим заголовком (его начала прежняя сборка с другими столбцами) — продолжение дня `<день>-2.csv` (`day_file`).

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use machine::HwError;
use machine::clock::{Pattern, Stamp};
use machine::thermal::Reading;
use machine::topology::Block;

use super::auto::COST_EVERY;
use super::meter::{Meter, Watch};
use super::serve::STATE_EVERY;
use super::{App, Device};
use crate::gpu::{Gpu, GpuReading, Nvml, SimGpu};
use crate::shell::Shell;

/// Таймер опроса срезов.
pub const SENSE: usize = 7;
/// Опрос опоздал (меню значка держало цикл программы): следующий — не раньше этого, а не пачкой подряд.
const SOONEST: Duration = Duration::from_millis(10);
/// Срезы хранятся столько дней (это журнал программы, не файлы владельца; срок — его выбор 15.10).
const KEEP_DAYS: i64 = 30;
/// Первая строка цены — после стольких опросов (2 с журнала), не через минуту.
const FIRST_COST: usize = 8;
/// Файл дня не открылся или не принял строку (диск полон, папка заперта) — следующая попытка не раньше: опрос 4 раза в
/// секунду получал бы ту же ошибку.
const OPEN_RETRY: Duration = Duration::from_secs(10);
/// Мощность корпуса не прочиталась — следующая попытка не раньше: строки идут с пустым столбцом.
const POWER_RETRY: Duration = Duration::from_secs(10);
/// Видеокарта не открылась (нет драйвера NVIDIA, NVML не открылась) — следующая попытка не раньше: каждая — загрузка
/// библиотеки и nvmlInit (23 мс, замер 15.10).
const GPU_REOPEN: Duration = Duration::from_secs(60);
/// Видеокарта не прочиталась (драйвер перезапущен, видеокарта пропала) — читатель закрыт, новый — не раньше.
const GPU_RETRY: Duration = Duration::from_secs(10);
/// Сколько файлов может быть у одного дня (`<день>.csv`, `-2` … `-9`): столько смен столбцов за день.
const DAY_PARTS: u32 = 9;
/// Столбцы общего теплового пакета — в конце строки (DECISIONS #37): мощность корпуса, Вт; видеокарта — °C, Вт, доля
/// занятости 0…1 (как `busy` блоков).
const PACKAGE_COLUMNS: &str = ",power_cpu,temp_gpu,power_gpu,busy_gpu";

/// Журнал срезов: пишется ли, когда следующий опрос, файл дня, цена за минуту.
#[derive(Debug, Default)]
pub struct Logging {
    /// Пишется: идёт опрос раз в `AutoTiming::sample`.
    pub on: bool,
    /// Когда должен был прийти этот опрос. Следующий — через шаг от него, а не от срабатывания таймера: Windows будит
    /// таймер с опозданием до 16 мс, и от срабатывания шаг расползался бы в среднем больше 0,25 с.
    due: Option<Instant>,
    /// Файл дня, открытый для дописывания (день, путь, файл): строка — одна запись, без открытия файла 4 раза в
    /// секунду. Путь — `<день>.csv` или продолжение дня `<день>-N.csv` (`day_file`).
    file: Option<(String, PathBuf, File)>,
    /// Мощность корпуса не прочиталась: раньше этого не пробовать; последняя её ошибка (одна строка на одну и ту же).
    power_retry: Option<Instant>,
    power_said: Option<String>,
    /// Видеокарта.
    gpu: GpuSide,
    /// Файл дня не открылся: раньше этого не пробовать.
    retry: Option<Instant>,
    /// Когда открытый файл дня последний раз сверялся со своим именем.
    verified: Option<Instant>,
    /// День, до которого срезы уже почищены.
    pruned: String,
    /// Буферы строки и дня — одни на все опросы.
    text: String,
    day: String,
    /// Последняя ошибка записи: одна строка журнала программы на одну и ту же.
    said: Option<String>,
    /// Цена с начала минуты: мкс на опрос (чтение и строка), мс между опросами, мкс на чтение оборотов, мкс на чтение
    /// мощности корпуса (входит в опрос), мкс на чтение видеокарты.
    costs: Vec<u32>,
    steps: Vec<u32>,
    fans: Vec<u32>,
    powers: Vec<u32>,
    gpus: Vec<u32>,
    /// Сколько опросов этой минуты были опросами окна (все ядра).
    windowed: u32,
    since: Option<Instant>,
    previous: Option<Instant>,
    /// Сколько строк цены уже написано: первая — после первых `FIRST_COST` опросов, чтобы цена была видна сразу,
    /// дальше — раз в минуту.
    lines: u32,
    /// Скрытая проверка: опросов всего; из них опросов окна (все ядра); сколько раз журнал начинался; сколько раз
    /// открывался файл дня.
    pub polls: u64,
    pub window_polls: u64,
    pub starts: u64,
    pub opens: u64,
}

/// Видеокарта для журнала срезов: читатель (открывается при первом чтении и остаётся открытым), когда читалась, чем
/// кончилось, последнее показание.
#[derive(Debug, Default)]
struct GpuSide {
    reader: Option<Box<dyn Gpu>>,
    /// Когда читалась или открывалась последний раз; следующий раз — через срок по тому, чем кончилось.
    last: Option<Instant>,
    fault: Option<GpuFault>,
    /// Показание последнего чтения; в строку — только той, где прочитано.
    reading: Option<GpuReading>,
    said: Option<String>,
    /// Скрытая проверка: попыток чтения (открытие не в счёт); имитация отказывает.
    calls: u64,
    sim_fail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuFault {
    Open,
    Read,
}

/// Скрытая проверка: что делал журнал срезов (счёт с запуска программы).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogCount {
    /// Пишется сейчас.
    pub on: bool,
    pub polls: u64,
    pub window_polls: u64,
    pub starts: u64,
    pub opens: u64,
}

/// Одно чтение ядра — всем, кто его берёт: сумме автомодуля и последнему показанию (`Meter`) и окну, если оно открыто
/// (`Watch`). Так кладут чтение и опрос срезов, и опрос окна, и автомодуль, когда читает сам.
pub(super) fn record(meter: &mut Meter, watch: Option<&mut Watch>, core: usize, reading: Reading) {
    let seconds = meter.record(core, reading, Instant::now());
    if let Some(watch) = watch {
        watch.core(core, reading, seconds);
    }
}

impl App {
    /// Одно чтение датчиков: корпус и ядра — все (`window`, опрос окна) или первые ядра блоков. Каждое ядро — через
    /// `record`; раз в 10 с, пока открыто окно, — потолки заново. Под арендой (`with_ceilings`). Ok — температура
    /// корпуса.
    pub(super) fn read_sensors(&mut self, window: bool) -> Result<Option<f64>, String> {
        let Some(device) = self.device.as_mut() else {
            return Err("нет доступа к процессору".into());
        };
        let package = device
            .thermal()
            .package()
            .map_err(|e| format!("температура не прочитана: {e}"))?;
        self.meter.record_package(package, Instant::now());
        let mut watch = self.serving.watch_mut();
        let count = if window {
            self.topology.cores.len()
        } else {
            self.topology.blocks.len()
        };
        for k in 0..count {
            let core = if window { k } else { self.topology.blocks[k].first };
            let reading = device
                .thermal()
                .block(core)
                .map_err(|e| format!("датчик ядра не прочитан: {e}"))?;
            record(&mut self.meter, watch.as_deref_mut(), core, reading);
        }
        if let Some(watch) = watch {
            watch.package(package);
        }
        self.refresh_for_window();
        Ok(package)
    }

    /// Журнал начинается: опрос раз в `AutoTiming::sample`.
    pub(super) fn start_logging(&mut self, shell: &Shell) {
        if self.logging.on {
            return;
        }
        let step = self.config.auto.sample;
        self.logging.on = true;
        self.logging.starts += 1;
        self.logging.due = Some(Instant::now() + step);
        shell.set_timer(SENSE, step);
    }

    /// Цена — заново (скрытая проверка сменила сроки опроса): первая строка снова после первых опросов.
    pub(super) fn restart_costs(&mut self) {
        let l = &mut self.logging;
        l.costs.clear();
        l.steps.clear();
        l.fans.clear();
        l.powers.clear();
        l.gpus.clear();
        l.windowed = 0;
        l.since = None;
        l.previous = None;
        l.lines = 0;
    }

    /// Журнал останавливается: опроса нет, файл закрыт — без журнала программа ничего не держит открытым. Срок
    /// следующей попытки открыть файл (`retry`) остаётся: ошибка от остановки не прошла.
    pub(super) fn stop_logging(&mut self, shell: &Shell) {
        if !self.logging.on {
            return;
        }
        self.logging.on = false;
        self.logging.due = None;
        self.logging.previous = None;
        self.logging.file = None;
        shell.kill_timer(SENSE);
    }

    /// Опрос срезов по таймеру: показания (каждый второй, пока открыто окно, — опрос окна), обороты, когда пора,
    /// строка среза.
    pub(super) fn sense_tick(&mut self, shell: &Shell) {
        if !self.logging.on {
            shell.kill_timer(SENSE);
            return;
        }
        let now = Instant::now();
        let step = self.config.auto.sample;
        let next = self.logging.due.map_or(now + step, |due| due + step);
        let next = if next <= now { now + step } else { next };
        self.logging.due = Some(next);
        shell.set_timer(SENSE, next.saturating_duration_since(now).max(SOONEST));
        // Потолки ставит на место возврат права или сверка после пробуждения: журнал стоит, опрос автомодуля начнёт его
        // снова, когда можно.
        if self.take_back_pending || self.auto.wake_pending {
            self.stop_logging(shell);
            return;
        }
        let started = Instant::now();
        let stamp = Stamp::now();
        // Опрос окна — когда с прошлого прошло больше трёх четвертей его полсекунды (при шаге 0,25 с — каждый второй):
        // окно получает состояние дважды в секунду, как от своего таймера, и показания за полсекунды.
        let window = self.serving.window_due(started, STATE_EVERY.saturating_sub(step / 2));
        // Пришла ли C#-часть, смотрит `with_ceilings` (до аренды и после неё). Мощность корпуса — в том же чтении: тот же
        // открытый драйвер, промежуток — тот же, что у доли работы ядер.
        let read = self.with_ceilings(|app| {
            let package = app.read_sensors(window)?;
            Ok::<_, String>((package, app.read_power()))
        });
        let (package, (power, power_cost)) = match read {
            Ok(Ok(read)) => read,
            Ok(Err(e)) | Err(e) => {
                // Датчик не прочитался или потолки держит другая часть: журнал стоит; опрос автомодуля раз в 2 с
                // скажет владельцу и попробует снова, драйвер — заново (как ревизия M5). Стартующей C#-части —
                // принятые потолки сразу, не через 2 с.
                if let Some(device) = &mut self.device {
                    device.cpu().release();
                }
                self.serving_trouble(Some(e));
                self.stop_logging(shell);
                if let Some(other) = self.other() {
                    self.yield_to(other);
                }
                return;
            }
        };
        self.serving_trouble(None);
        let read = started.elapsed();
        if window {
            self.window_polled(shell);
        }
        // Обороты — вне аренды: вызов прошивки бывает долгим (до 4 с), стартующая C#-часть его не ждёт. Видеокарта — тоже
        // вне аренды: она к потолкам отношения не имеет.
        let fans_started = Instant::now();
        let fresh = self.read_fans();
        let fans = fresh.then(|| fans_started.elapsed());
        let gpu_started = Instant::now();
        let gpu = self.read_gpu();
        let gpu_cost = gpu.then(|| gpu_started.elapsed());
        let written = Instant::now();
        self.write_sample(stamp, package, fresh, power, gpu);
        self.account(
            read + written.elapsed(),
            Costs {
                fans,
                power: power_cost,
                gpu: gpu_cost,
            },
            window,
        );
    }

    /// Мощность корпуса за промежуток с прошлого чтения (под арендой, в опросе срезов). Не прочиталась — пусто, одна
    /// строка в error.log на одну и ту же беду, следующая попытка через `POWER_RETRY`: журнал идёт и без неё. Вторым —
    /// сколько стоило чтение (None — не читалась).
    fn read_power(&mut self) -> (Option<f64>, Option<Duration>) {
        let started = Instant::now();
        if self.logging.power_retry.is_some_and(|at| started < at) {
            return (None, None);
        }
        let Some(device) = self.device.as_mut() else {
            return (None, None);
        };
        let result = device.thermal().package_power();
        let cost = Some(started.elapsed());
        match result {
            Ok(watts) => {
                self.logging.power_retry = None;
                self.logging.power_said = None;
                (watts, cost)
            }
            Err(e) => {
                self.logging.power_retry = Some(started + POWER_RETRY);
                let text = format!("мощность корпуса не прочитана: {e}");
                if self.logging.power_said.as_deref() != Some(text.as_str()) {
                    self.journal.error(&format!("автомодуль: {text}"));
                    self.logging.power_said = Some(text);
                }
                (None, cost)
            }
        }
    }

    /// Видеокарта: пока пишется журнал — раз в `AutoTiming::gpu` (2 с); не открылась — следующая попытка через минуту,
    /// не прочиталась — читатель закрыт, новый через 10 с. На имитации — имитация. true — прочитана сейчас (в строку
    /// среза она идёт только тогда).
    fn read_gpu(&mut self) -> bool {
        let now = Instant::now();
        let every = match self.logging.gpu.fault {
            Some(GpuFault::Open) => GPU_REOPEN,
            Some(GpuFault::Read) => GPU_RETRY,
            None => self.config.auto.gpu,
        };
        // Пол-опроса срезов — допуск на дрожание таймера: видеокарта в каждой восьмой строке, как обороты.
        let slack = self.config.auto.sample / 2;
        if self
            .logging
            .gpu
            .last
            .is_some_and(|last| now.saturating_duration_since(last) + slack < every)
        {
            return false;
        }
        self.logging.gpu.last = Some(now);
        if self.logging.gpu.reader.is_none() {
            let opened: Result<Box<dyn Gpu>, HwError> = match &self.device {
                Some(Device::Sim(_)) => Ok(Box::new(sim_gpu(self.logging.gpu.sim_fail))),
                Some(Device::Real(_)) => Nvml::open().map(|gpu| Box::new(gpu) as Box<dyn Gpu>),
                None => return false,
            };
            match opened {
                Ok(reader) => {
                    self.journal.note(&format!("видеокарта: {}", reader.describe()));
                    self.logging.gpu.reader = Some(reader);
                }
                Err(e) => {
                    self.logging.gpu.fault = Some(GpuFault::Open);
                    self.logging.gpu.reading = None;
                    self.gpu_trouble(&e.0);
                    return false;
                }
            }
        }
        let Some(reader) = self.logging.gpu.reader.as_mut() else {
            return false;
        };
        self.logging.gpu.calls += 1;
        match reader.read() {
            Ok(reading) => {
                let gpu = &mut self.logging.gpu;
                gpu.reading = Some(reading);
                gpu.fault = None;
                gpu.said = None;
                true
            }
            Err(e) => {
                // Читатель после ошибки не годится (видеокарта пропала, драйвер перезапущен): закрыть, открыть новый.
                let gpu = &mut self.logging.gpu;
                gpu.reader = None;
                gpu.reading = None;
                gpu.fault = Some(GpuFault::Read);
                self.gpu_trouble(&e.0);
                false
            }
        }
    }

    /// Беда с видеокартой — одна строка error.log на одну и ту же, а не каждые 2 с.
    fn gpu_trouble(&mut self, text: &str) {
        if self.logging.gpu.said.as_deref() != Some(text) {
            self.journal.error(&format!(
                "автомодуль: видеокарта не прочитана — {text}; её столбцы пустые"
            ));
            self.logging.gpu.said = Some(text.to_owned());
        }
    }

    /// Скрытая проверка: видеокарта имитации отказывает (`fail`) или снова отвечает. Только на имитации.
    pub fn sim_gpu(&mut self, fail: bool) {
        self.logging.gpu.sim_fail = fail;
        if matches!(self.device, Some(Device::Sim(_))) && self.logging.gpu.reader.is_some() {
            self.logging.gpu.reader = Some(Box::new(sim_gpu(fail)));
        }
    }

    /// Скрытая проверка: сколько раз видеокарта читалась (попытки чтения, открытие не в счёт).
    #[must_use]
    pub fn gpu_calls(&self) -> u64 {
        self.logging.gpu.calls
    }

    fn samples_dir(&self) -> PathBuf {
        self.config.data.join("samples")
    }

    /// Строка в data\samples\<дата>.csv: время, корпус, обороты (только если прочитаны в этом опросе), по блокам
    /// потолок, фактический множитель, доля работы, температура; в конце — мощность корпуса за промежуток и видеокарта
    /// (только если прочитана в этом опросе). Заголовок — в новый файл; новый день — новый файл и чистка старых. Файла
    /// нет и открыть его пока нельзя (`retry`) — строка не пишется: об ошибке уже сказано.
    fn write_sample(&mut self, stamp: Stamp, package: Option<f64>, fans: bool, power: Option<f64>, gpu: bool) {
        let now = Instant::now();
        let mut day = std::mem::take(&mut self.logging.day);
        day.clear();
        stamp.write_local(Pattern::Date, &mut day);
        let mut text = std::mem::take(&mut self.logging.text);
        text.clear();
        if self.logging.file.as_ref().is_some_and(|(open, _, _)| *open != day) {
            self.logging.file = None; // новый день — новый файл
        }
        self.verify_file(now);
        if self.logging.file.is_none() && self.logging.retry.is_none_or(|at| now >= at) {
            self.open_day(&day, stamp, now, &mut text);
        }
        if self.logging.file.is_some()
            && let Some(limits) = self.limits.as_ref()
        {
            stamp.write_local(Pattern::Millis, &mut text);
            text.push(',');
            put(&mut text, package, 0);
            let rpm = if fans { self.auto.rpm.as_deref() } else { None };
            for k in 0..2 {
                text.push(',');
                if let Some(r) = rpm.and_then(|r| r.get(k)) {
                    let _ = write!(text, "{r}"); // into a String
                }
            }
            for block in &self.topology.blocks {
                let r = self.meter.latest(block.first);
                let _ = write!(text, ",{},", limits.current()[block.first]); // into a String
                put(&mut text, r.ratio, 2);
                text.push(',');
                put(&mut text, r.busy, 2);
                text.push(',');
                put(&mut text, r.temp, 0);
            }
            // Порядок — как в `PACKAGE_COLUMNS`.
            text.push(',');
            put(&mut text, power, 1);
            let card = if gpu { self.logging.gpu.reading } else { None };
            text.push(',');
            put(&mut text, card.and_then(|g| g.temp), 0);
            text.push(',');
            put(&mut text, card.and_then(|g| g.power), 1);
            text.push(',');
            put(&mut text, card.and_then(|g| g.busy), 2);
            text.push('\n');
        }
        let written = match self.logging.file.as_mut() {
            Some((_, _, file)) if !text.is_empty() => Some(file.write_all(text.as_bytes())),
            _ => None,
        };
        match written {
            Some(Ok(())) => self.logging.said = None,
            Some(Err(e)) => {
                // Не записалось (диск полон, том пропал): файл — заново, но не раньше `OPEN_RETRY`.
                self.logging.file = None;
                self.logging.retry = Some(now + OPEN_RETRY);
                self.samples_trouble(&format!("срез не записан: {e}"));
            }
            None => {}
        }
        self.logging.text = text;
        self.logging.day = day;
    }

    /// Открыть файл дня для дописывания (`day_file`: свой заголовок — в пустой; файл дня с другим заголовком —
    /// продолжение дня); новый день — заодно чистка старых. Не открылся — одна строка ошибки, следующая попытка не
    /// раньше `OPEN_RETRY`.
    fn open_day(&mut self, day: &str, stamp: Stamp, now: Instant, text: &mut String) {
        let dir = self.samples_dir();
        self.logging.opens += 1;
        let head = header(&self.topology.blocks); // once a day, or after the file was lost
        let opened = fs::create_dir_all(&dir).and_then(|()| day_file(&dir, day, &head));
        match opened {
            Ok((path, file, empty)) => {
                if empty {
                    text.push_str(&head);
                    text.push('\n');
                }
                if path.file_name() != Some(std::ffi::OsStr::new(&format!("{day}.csv"))) {
                    self.journal.note(&format!(
                        "срезы: в {day}.csv строки с другими столбцами (прежняя сборка) — пишу в {}",
                        path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                    ));
                }
                self.logging.file = Some((day.to_owned(), path, file));
                self.logging.retry = None;
                self.logging.verified = Some(now);
            }
            Err(e) => {
                self.logging.retry = Some(now + OPEN_RETRY);
                self.samples_trouble(&format!("срез не записан: {e}"));
            }
        }
        if day != self.logging.pruned {
            self.logging.pruned.clear();
            self.logging.pruned.push_str(day);
            prune(self, &dir, stamp);
        }
    }

    /// Раз в шаг оборотов журнала (2 с): тот ли открытый файл дня, что лежит под своим именем. Удалённый или
    /// перемещённый файл Windows принимает строки дальше без ошибки — тогда файл открывается заново. Цена — два запроса
    /// к файловой системе раз в 2 с.
    fn verify_file(&mut self, now: Instant) {
        let every = self.config.auto.fans;
        let Some((_, path, file)) = self.logging.file.as_ref() else {
            return;
        };
        if self
            .logging
            .verified
            .is_some_and(|at| now.saturating_duration_since(at) < every)
        {
            return;
        }
        self.logging.verified = Some(now);
        if same_file(file, path) {
            return;
        }
        let note = format!(
            "срезы: файл {} удалён или перемещён — пишу в новый",
            path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
        );
        self.logging.file = None;
        self.journal.note(&note);
    }

    /// Ошибка записи срезов — одна строка журнала программы на одну и ту же ошибку, а не 4 раза в секунду.
    fn samples_trouble(&mut self, text: &str) {
        if self.logging.said.as_deref() != Some(text) {
            self.journal.error(&format!("автомодуль: {text}"));
            self.logging.said = Some(text.to_owned());
        }
    }

    /// Цена опроса: раз в минуту строкой в tray.log — медиана и наибольший, шаг между опросами, обороты, мощность
    /// корпуса, видеокарта.
    fn account(&mut self, cost: Duration, parts: Costs, window: bool) {
        let now = Instant::now();
        let l = &mut self.logging;
        l.polls += 1;
        l.window_polls += u64::from(window);
        if let Some(previous) = l.previous.replace(now) {
            l.steps.push(millis(now.saturating_duration_since(previous)));
        }
        l.costs.push(micros(cost));
        for (spent, into) in [
            (parts.fans, &mut l.fans),
            (parts.power, &mut l.powers),
            (parts.gpu, &mut l.gpus),
        ] {
            if let Some(spent) = spent {
                into.push(micros(spent));
            }
        }
        l.windowed += u32::from(window);
        let since = *l.since.get_or_insert(now);
        let first = l.lines == 0 && l.costs.len() >= FIRST_COST;
        if !first && now.saturating_duration_since(since) < COST_EVERY {
            return;
        }
        l.lines += 1;
        let polls = l.costs.len();
        let (cost_median, cost_max) = spread(&mut l.costs);
        let (step_median, step_max) = spread(&mut l.steps);
        let step_min = l.steps.first().copied().unwrap_or(0);
        let (fans_median, _) = spread(&mut l.fans);
        let (power_median, power_max) = spread(&mut l.powers);
        let (gpu_median, gpu_max) = spread(&mut l.gpus);
        let line = format!(
            "срезы: опросов {polls} за {} с, из них опросов окна (все ядра) {}; опрос — медиана {cost_median} мкс, \
             наибольший {cost_max} мкс, в нём мощность корпуса — {} раз, медиана {power_median} мкс, наибольший \
             {power_max} мкс; шаг — медиана {step_median} мс, от {step_min} до {step_max} мс; обороты — {} раз, медиана \
             {fans_median} мкс; видеокарта — {} раз, медиана {gpu_median} мкс, наибольший {gpu_max} мкс",
            now.saturating_duration_since(since).as_secs(),
            l.windowed,
            l.powers.len(),
            l.fans.len(),
            l.gpus.len()
        );
        l.costs.clear();
        l.steps.clear();
        l.fans.clear();
        l.powers.clear();
        l.gpus.clear();
        l.windowed = 0;
        l.since = Some(now);
        self.journal.note(&line);
    }
}

/// Цена частей опроса, кроме самого чтения ядер: None — эта часть в опросе не читалась.
#[derive(Debug, Clone, Copy)]
struct Costs {
    fans: Option<Duration>,
    power: Option<Duration>,
    gpu: Option<Duration>,
}

/// Видеокарта имитации: те же числа во всей программе (скрытая проверка ждёт их в строках срезов).
fn sim_gpu(fail: bool) -> SimGpu {
    SimGpu {
        reading: GpuReading {
            temp: Some(61.0),
            power: Some(95.5),
            busy: Some(0.72),
        },
        fail,
    }
}

/// Заголовок файла срезов этой сборки. Новые столбцы — только в конец: старые стоят на прежних местах.
fn header(blocks: &[Block]) -> String {
    let mut text = String::from("time,package,fan_cpu,fan_gpu");
    for block in blocks {
        let n = &block.name;
        let _ = write!(text, ",{n}.ceiling,{n}.ratio,{n}.busy,{n}.temp"); // into a String: cannot fail
    }
    text.push_str(PACKAGE_COLUMNS);
    text
}

/// Имя части дня: первая — `<день>.csv`, дальше — `<день>-N.csv`.
fn part_name(day: &str, part: u32) -> String {
    if part <= 1 {
        format!("{day}.csv")
    } else {
        format!("{day}-{part}.csv")
    }
}

/// Файл дня для строк с заголовком `header`, открытый для дописывания, и пуст ли он (тогда заголовок пишет
/// вызывающий): `<день>.csv`, а если в нём строки под другим заголовком (их писала сборка с другими столбцами) —
/// `<день>-2.csv`, `-3`… — первый, которого нет, пустой или с тем же заголовком. Строки с другими столбцами под чужим
/// заголовком читатель разобрал бы не по тем столбцам; переписывать старый файл (дописать пустые столбцы) — значит
/// трогать данные владельца и сбить окно, которое дочитывает его с прочитанного места.
fn day_file(dir: &Path, day: &str, header: &str) -> io::Result<(PathBuf, File, bool)> {
    for part in 1..=DAY_PARTS {
        let path = dir.join(part_name(day, part));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        if file.metadata()?.len() == 0 {
            return Ok((path, file, true));
        }
        if first_line(&path)? == header {
            return Ok((path, file, false));
        }
    }
    Err(io::Error::other(format!(
        "у дня {day} уже {DAY_PARTS} файлов срезов с другими столбцами"
    )))
}

/// Первая строка файла без конца строки (не больше 16 КБ — заголовок короче).
fn first_line(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::with_capacity(1024);
    File::open(path)?.take(16 * 1024).read_to_end(&mut bytes)?;
    let end = bytes.iter().position(|&b| b == b'\n').unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[..end]).trim_end_matches('\r').to_owned())
}

/// День файла срезов по имени: «ГГГГ-ММ-ДД.csv» или его продолжение «ГГГГ-ММ-ДД-N.csv»; другое — None.
fn day_of(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".csv")?;
    let day = stem.get(..10)?;
    let rest = &stem[10..];
    let part = rest.is_empty()
        || rest
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    (part && day.as_bytes()[4] == b'-' && day.as_bytes()[7] == b'-').then_some(day)
}

/// Открытый файл — тот же, что лежит под именем `path`: имя есть, это файл той же длины. Пишет в файл дня только эта
/// программа, так что другой файл под тем же именем совпал бы с ним длиной разве что случайно.
fn same_file(file: &File, path: &Path) -> bool {
    match (file.metadata(), fs::metadata(path)) {
        (Ok(open), Ok(named)) => named.is_file() && open.len() == named.len(),
        _ => false,
    }
}

/// Число в строку среза: пусто, если не известно.
fn put(text: &mut String, value: Option<f64>, digits: usize) {
    if let Some(v) = value {
        let _ = write!(text, "{v:.digits$}"); // into a String: cannot fail
    }
}

/// Срезы старше 30 дней удаляются (и продолжения дня `<день>-N.csv`).
fn prune(app: &App, dir: &Path, now: Stamp) {
    let cutoff = Stamp(now.0 - KEEP_DAYS * 24 * 3600 * 10_000_000).local(Pattern::Date);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if day_of(&name).is_some_and(|day| day < cutoff.as_str())
            && let Err(e) = fs::remove_file(entry.path())
        {
            app.journal
                .error(&format!("автомодуль: старый срез {name} не удалён: {e}"));
        }
    }
}

/// Медиана и наибольшее (список сортируется на месте: он всё равно очищается после строки цены).
fn spread(values: &mut [u32]) -> (u32, u32) {
    values.sort_unstable();
    (
        values.get(values.len() / 2).copied().unwrap_or(0),
        values.last().copied().unwrap_or(0),
    )
}

fn micros(d: Duration) -> u32 {
    u32::try_from(d.as_micros()).unwrap_or(u32::MAX)
}

fn millis(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_is_median_and_largest() {
        let mut values = [250, 266, 249, 251, 1000];
        assert_eq!(spread(&mut values), (251, 1000));
        assert_eq!(spread(&mut []), (0, 0));
    }

    // Правило файла (DECISIONS #37): строки этой сборки — только под свой заголовок; файл дня, начатый сборкой с другими
    // столбцами, не трогается, день продолжается в «<день>-2.csv»; окно и чистка узнают оба имени.
    const DAY: &str = "2026-10-15";
    const OLD: &str = "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp";

    fn blocks() -> Vec<Block> {
        vec![Block {
            members: vec![0],
            first: 0,
            lp: machine::Lp(0),
            kind: machine::topology::Kind::P,
            name: "0".into(),
        }]
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coremap-sense-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir); // leftovers of an earlier run with the same process number
        fs::create_dir_all(&dir).expect("scratch folder");
        dir
    }

    #[test]
    fn new_columns_go_after_the_old_ones_and_are_not_read_as_blocks() {
        let head = header(&blocks());
        assert!(
            head.starts_with(&format!("{OLD},")),
            "old columns keep their places: {head}"
        );
        let added: Vec<&str> = head[OLD.len() + 1..].split(',').collect();
        assert_eq!(added, ["power_cpu", "temp_gpu", "power_gpu", "busy_gpu"]);
        // The window takes «X.Y» as column Y of block X (rust/window/src/samples.rs, Columns::parse).
        assert!(added.iter().all(|c| !c.contains('.')), "{added:?}");
    }

    #[test]
    fn a_new_day_file_gets_the_header() {
        let dir = scratch("new");
        let (path, _, empty) = day_file(&dir, DAY, &header(&blocks())).expect("opened");
        assert_eq!(path, dir.join("2026-10-15.csv"));
        assert!(empty, "the caller writes the header into an empty file");
        let _ = fs::remove_dir_all(&dir); // scratch
    }

    #[test]
    fn a_day_file_with_this_header_is_continued() {
        let dir = scratch("same");
        let head = header(&blocks());
        fs::write(dir.join("2026-10-15.csv"), format!("{head}\r\n1,2\n")).expect("file");
        let (path, _, empty) = day_file(&dir, DAY, &head).expect("opened");
        assert_eq!((path, empty), (dir.join("2026-10-15.csv"), false));
        let _ = fs::remove_dir_all(&dir); // scratch
    }

    #[test]
    fn a_day_file_with_other_columns_is_left_alone_and_the_day_goes_on_in_part_2() {
        let dir = scratch("old");
        let old = format!("{OLD}\n2026-10-15 10:00:00.000,70,,,53,45.00,0.50,70\n");
        fs::write(dir.join("2026-10-15.csv"), &old).expect("file");
        let head = header(&blocks());
        let (path, mut file, empty) = day_file(&dir, DAY, &head).expect("opened");
        assert_eq!((path.clone(), empty), (dir.join("2026-10-15-2.csv"), true));
        file.write_all(format!("{head}\n").as_bytes()).expect("write");
        drop(file);
        assert_eq!(
            fs::read_to_string(dir.join("2026-10-15.csv")).expect("old"),
            old,
            "untouched"
        );
        // The next start of the same day finds part 2 by its header.
        let (again, _, empty) = day_file(&dir, DAY, &head).expect("opened");
        assert_eq!((again, empty), (path, false));
        let _ = fs::remove_dir_all(&dir); // scratch
    }

    #[test]
    fn day_parts_are_known_by_name() {
        assert_eq!(day_of("2026-10-15.csv"), Some("2026-10-15"));
        assert_eq!(day_of("2026-10-15-2.csv"), Some("2026-10-15"));
        assert_eq!(day_of("2026-10-15-12.csv"), Some("2026-10-15"));
        for other in [
            "moved.csv",
            "2026-10-15-.csv",
            "2026-10-15-x.csv",
            "2026-10-15.txt",
            "20261015ab.csv",
        ] {
            assert_eq!(day_of(other), None, "{other}");
        }
    }

    #[test]
    fn unknown_values_leave_the_field_empty() {
        let mut text = String::from("x,");
        put(&mut text, Some(52.346), 2);
        text.push(',');
        put(&mut text, None, 2);
        text.push(',');
        put(&mut text, Some(71.6), 0);
        assert_eq!(text, "x,52.35,,72");
    }
}
