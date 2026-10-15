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
// Файл дня открыт, пока пишется журнал. Не открылся или не принял строку — одна строка ошибки и следующая попытка
// через 10 с, а не в каждом опросе (ревизия 15.10, п. 7). Раз в 2 с — тот ли это файл, что лежит под своим именем:
// удалённый или перемещённый открытый файл Windows принимает строки дальше без ошибки (проверено на этой машине
// 15.10), и они ушли бы в никуда или под чужое имя (п. 8).

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use machine::clock::{Pattern, Stamp};
use machine::thermal::Reading;

use super::App;
use super::auto::COST_EVERY;
use super::meter::{Meter, Watch};
use super::serve::STATE_EVERY;
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

/// Журнал срезов: пишется ли, когда следующий опрос, файл дня, цена за минуту.
#[derive(Debug, Default)]
pub struct Logging {
    /// Пишется: идёт опрос раз в `AutoTiming::sample`.
    pub on: bool,
    /// Когда должен был прийти этот опрос. Следующий — через шаг от него, а не от срабатывания таймера: Windows будит
    /// таймер с опозданием до 16 мс, и от срабатывания шаг расползался бы в среднем больше 0,25 с.
    due: Option<Instant>,
    /// Файл дня, открытый для дописывания (день, файл): строка — одна запись, без открытия файла 4 раза в секунду.
    file: Option<(String, File)>,
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
    /// Цена с начала минуты: мкс на опрос (чтение и строка), мс между опросами, мкс на чтение оборотов.
    costs: Vec<u32>,
    steps: Vec<u32>,
    fans: Vec<u32>,
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
        // Пришла ли C#-часть, смотрит `with_ceilings` (до аренды и после неё).
        let package = match self.with_ceilings(|app| app.read_sensors(window)) {
            Ok(Ok(package)) => package,
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
        // Обороты — вне аренды: вызов прошивки бывает долгим (до 4 с), стартующая C#-часть его не ждёт.
        let fans_started = Instant::now();
        let fresh = self.read_fans();
        let fans = fresh.then(|| fans_started.elapsed());
        let written = Instant::now();
        self.write_sample(stamp, package, fresh);
        self.account(read + written.elapsed(), fans, window);
    }

    fn samples_dir(&self) -> PathBuf {
        self.config.data.join("samples")
    }

    /// Строка в data\samples\<дата>.csv: время, корпус, обороты (только если прочитаны в этом опросе), по блокам
    /// потолок, фактический множитель, доля работы, температура. Заголовок — в новый файл; новый день — новый файл и
    /// чистка старых. Файла нет и открыть его пока нельзя (`retry`) — строка не пишется: об ошибке уже сказано.
    fn write_sample(&mut self, stamp: Stamp, package: Option<f64>, fans: bool) {
        let now = Instant::now();
        let mut day = std::mem::take(&mut self.logging.day);
        day.clear();
        stamp.write_local(Pattern::Date, &mut day);
        let mut text = std::mem::take(&mut self.logging.text);
        text.clear();
        if self.logging.file.as_ref().is_some_and(|(open, _)| *open != day) {
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
            text.push('\n');
        }
        let written = match self.logging.file.as_mut() {
            Some((_, file)) if !text.is_empty() => Some(file.write_all(text.as_bytes())),
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

    /// Открыть файл дня для дописывания; заголовок — в пустой (файл дня, начатый прежней сборкой, его уже имеет); новый
    /// день — заодно чистка старых. Не открылся — одна строка ошибки, следующая попытка не раньше `OPEN_RETRY`.
    fn open_day(&mut self, day: &str, stamp: Stamp, now: Instant, text: &mut String) {
        let dir = self.samples_dir();
        let path = dir.join(format!("{day}.csv"));
        self.logging.opens += 1;
        let opened = fs::create_dir_all(&dir).and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
        match opened {
            Ok(file) => {
                if file.metadata().is_ok_and(|m| m.len() == 0) {
                    text.push_str("time,package,fan_cpu,fan_gpu");
                    for block in &self.topology.blocks {
                        let n = &block.name;
                        let _ = write!(text, ",{n}.ceiling,{n}.ratio,{n}.busy,{n}.temp"); // into a String
                    }
                    text.push('\n');
                }
                self.logging.file = Some((day.to_owned(), file)); // once a day, or after the file was lost
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
        let Some((day, file)) = self.logging.file.as_ref() else {
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
        if same_file(file, &self.samples_dir().join(format!("{day}.csv"))) {
            return;
        }
        let note = format!("срезы: файл {day}.csv удалён или перемещён — пишу в новый");
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

    /// Цена опроса: раз в минуту строкой в tray.log — медиана и наибольший, шаг между опросами, обороты.
    fn account(&mut self, cost: Duration, fans: Option<Duration>, window: bool) {
        let now = Instant::now();
        let l = &mut self.logging;
        l.polls += 1;
        l.window_polls += u64::from(window);
        if let Some(previous) = l.previous.replace(now) {
            l.steps.push(millis(now.saturating_duration_since(previous)));
        }
        l.costs.push(micros(cost));
        if let Some(fans) = fans {
            l.fans.push(micros(fans));
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
        let line = format!(
            "срезы: опросов {polls} за {} с, из них опросов окна (все ядра) {}; опрос — медиана {cost_median} мкс, \
             наибольший {cost_max} мкс; шаг — медиана {step_median} мс, от {step_min} до {step_max} мс; обороты — {} раз, \
             медиана {fans_median} мкс",
            now.saturating_duration_since(since).as_secs(),
            l.windowed,
            l.fans.len()
        );
        l.costs.clear();
        l.steps.clear();
        l.fans.clear();
        l.windowed = 0;
        l.since = Some(now);
        self.journal.note(&line);
    }
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

/// Срезы старше 30 дней удаляются.
fn prune(app: &App, dir: &Path, now: Stamp) {
    let cutoff = Stamp(now.0 - KEEP_DAYS * 24 * 3600 * 10_000_000).local(Pattern::Date);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(date) = name.strip_suffix(".csv") else {
            continue;
        };
        if date.len() == 10
            && date < cutoff.as_str()
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
