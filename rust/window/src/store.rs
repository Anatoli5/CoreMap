// Статистика читается в своём потоке, не в потоке окна (rust\AGENTS.md: поток окна не ждёт файлов).
//
// Поток держит сводку по каждому файлу срезов за последние 30 дней и сколько от файла прочитано; раз в SCAN он
// смотрит размеры и времена записи (папка срезов: файл на день и его продолжения «-N»; папка событий: файл на день; журнал
// автомодуля) и дочитывает только
// дописанное — прошлые дни не перечитываются, сегодняшний — только с прочитанного места. Ничего не изменилось — окну
// ничего не идёт. Изменилось — окно получает новую сводку целиком (Digest) и перерисовывает то, что из неё показывает.
//
// Окну поток начинает читать, когда знает блоки и их соседей (`Order::Die`, после знакомства): строка наибольшей
// температуры корзины («макс») помнит температуры соседей. Сменились блоки — файлы читаются заново.
//
// Сырые строки одного разбора сохраняются без сокращения прежней моделью. Условная модель получает неизменяемый
// снимок в отдельном вычислительном потоке: один пакет журналов, блоки последовательно, готовые запросы в Digest.live.
// Прежняя модель остаётся для диагностических видов и не меняет исходную историю условной модели.
//
// День журнала для вида «по времени» (timeline.rs) поток читает по просьбе окна (`Order::Day`) — весь день заново, не из
// сводки; пока этот день дописывается, перечитывает его не чаще раза в `DAY_REFRESH`.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::conditional_runtime::{self, LiveCurves, Runtime};
use crate::events::{self, AutoEntry, Event, EventEntry};
use crate::model::{self, Die, Log, Model};
use crate::samples::{self, Carry, Columns, Fresh, Scratch, Tail, Tally};
use crate::timeline::{self, Day, Quantity};

/// Как часто смотреть, не дописаны ли журналы. Программа входа пишет срез раз в 0,25 с (до 0.5.2 — раз в 2 с); график
/// в окне отстаёт от неё не больше чем на 5 с, а проверка — несколько вызовов Windows и разбор только дописанного.
pub const SCAN: Duration = Duration::from_secs(5);
/// Срезы за сколько дней (программа входа хранит 30 и удаляет старые сама).
pub const DAYS: i64 = 30;
/// Минимальная пауза после завершения пакета подгонки до следующего: фоновый расчёт не должен работать непрерывно,
/// пока срезы дописываются. Прежняя настройка темпа, не измеренный бюджет вычислений или правило управления.
pub const REFIT: Duration = Duration::from_secs(60);
/// Показанный день дописывается — перечитать его не чаще раза в столько (день строк по 0,25 с — десятки МБ текста).
pub const DAY_REFRESH: Duration = Duration::from_secs(30);

/// Что окно получает из потока статистики.
#[derive(Debug, Default)]
pub struct Digest {
    pub live: Option<Arc<LiveCurves>>,
    pub conditional: Option<Arc<crate::conditional_snapshot::Snapshot>>,
    pub conditional_trouble: Option<String>,
    pub tally: Tally,
    pub files: usize,
    /// События, новые первыми: файлы data\events\<день>.jsonl (с 0.5.6) и прежний журнал автомодуля
    /// (data\auto-log.jsonl) за дни до них — `events::merged`. Пересобираются, только когда журналы дописаны.
    pub events: Arc<Vec<Event>>,
    /// Дни, за которые есть срезы («ГГГГ-ММ-ДД», по возрастанию) — выбор дня вида «по времени».
    pub days: Vec<String>,
    /// Сколько байт срезов прочитано и разобрано этим проходом и за сколько: цена статистики.
    pub parsed_bytes: u64,
    pub parsed_rows: u64,
    pub parse_time: Duration,
    /// Что не прочиталось (строка для владельца); None — всё прочиталось.
    pub trouble: Option<String>,
    /// Модель нагрева по строкам срезов; None — соседи блоков ещё не известны (знакомства не было).
    pub model: Option<Arc<Model>>,
    /// Что сделало хранение строк после последней подгонки.
    pub retained: Option<model::Retained>,
    /// Сколько памяти держат строки для модели, байт.
    pub kept_bytes: usize,
}

/// Что поток статистики отдаёт окну.
#[derive(Debug)]
pub enum Delivery {
    Digest(Arc<Digest>),
    Day(Arc<Day>),
}

/// Какой день показать в виде «по времени» и чьи ряды.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayOrder {
    pub date: String,
    pub blocks: Vec<String>,
    /// Какие ряды блоков читать (раскладка «блоки» — одну величину, «параметры» — все).
    pub quantities: Vec<Quantity>,
}

/// Что окно говорит потоку статистики.
#[derive(Debug)]
pub enum Order {
    /// Проход сейчас, не дожидаясь `SCAN`.
    Scan,
    /// Блоки и их соседи из знакомства: модель считается по ним.
    Die(Arc<Die>),
    /// День для вида «по времени»; None — вид закрыт, день не нужен.
    Day(Option<DayOrder>),
}

#[derive(Debug, Default)]
struct SampleFile {
    tail: Tail,
    /// Первая строка файла прочитана: заголовок разобран (`columns`) или это не заголовок срезов (None).
    headed: bool,
    columns: Option<Columns>,
    tally: Tally,
    /// Что передаёт следующей порции файла прошлая: время последней строки, последние чтения, ждущие наибольшие.
    carry: Carry,
    /// Строки по 0,25 с — для модели.
    log: Log,
}

#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts of the reading thread (waits for blocks, refit due, rows stale, day stale), not one state machine"
)]
struct Worker {
    live: Option<Arc<LiveCurves>>,
    runtime: Option<Runtime>,
    runtime_enabled: bool,
    live_trouble: Option<String>,
    generation: u64,
    source_revision: u64,
    live_fitted: Option<Instant>,
    live_submitted_revision: Option<u64>,
    conditional: Option<Arc<crate::conditional_snapshot::Snapshot>>,
    conditional_stamp: Option<SystemTime>,
    conditional_trouble: Option<String>,
    dir: PathBuf,
    /// Папка срезов: у окна — samples в папке данных, у отчёта — та, что ему дали.
    samples: PathBuf,
    /// Срезы за сколько дней; None — все файлы папки (скрытый отчёт модели).
    days: Option<i64>,
    /// Читать, только когда известны блоки (окно); отчёт и тесты читают и без них.
    wait_die: bool,
    files: BTreeMap<String, SampleFile>,
    scratch: Scratch,
    auto_tail: Tail,
    auto: Vec<AutoEntry>,
    /// Файлы событий по дням: сколько прочитано и строки.
    event_files: BTreeMap<String, (Tail, Vec<EventEntry>)>,
    /// События для окна (`events::merged`) — пересобираются, когда журналы дописаны.
    events: Arc<Vec<Event>>,
    die: Option<Arc<Die>>,
    /// Соседи сменились — модель пересчитать при следующем проходе сразу, даже если файлы не менялись.
    refit: bool,
    /// Строки дописаны после последней подгонки — пересчитать, когда пройдёт `REFIT`.
    stale: bool,
    /// Когда была последняя подгонка.
    fitted: Option<Instant>,
    model: Option<Arc<Model>>,
    retained: Option<model::Retained>,
    /// День вида «по времени», когда он прочитан и дописан ли с тех пор.
    day: Option<DayOrder>,
    day_read: Option<Instant>,
    day_stale: bool,
}

/// Запускает поток статистики над папкой данных `dir`. `deliver` получает сводку, когда она изменилась (первую —
/// после первого прохода, когда известны блоки), и день вида «по времени», когда его попросили. Отправитель — чтобы
/// попросить проход сейчас, дать блоки и соседей, попросить день; когда все отправители брошены, поток кончается.
pub fn start(dir: PathBuf, deliver: impl Fn(Delivery) + Send + 'static) -> mpsc::Sender<Order> {
    let (wake, woken) = mpsc::channel::<Order>();
    let mut worker = Worker {
        samples: dir.join("samples"),
        dir,
        days: Some(DAYS),
        wait_die: true,
        runtime_enabled: true,
        ..Worker::default()
    };
    worker.ensure_runtime();
    thread::Builder::new()
        .name("статистика".into())
        .spawn(move || {
            let mut first = true;
            loop {
                if !(worker.wait_die && worker.die.is_none()) {
                    if let Some(digest) = worker.scan(first) {
                        deliver(Delivery::Digest(Arc::new(digest)));
                    }
                    first = false;
                    if let Some(day) = worker.day_due(Instant::now()) {
                        deliver(Delivery::Day(Arc::new(day)));
                    }
                }
                match woken.recv_timeout(SCAN) {
                    Ok(Order::Scan) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(Order::Die(die)) => worker.set_die(die),
                    Ok(Order::Day(day)) => {
                        worker.day_read = None;
                        worker.day = day;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("поток статистики");
    wake
}

impl Worker {
    /// Блоки и соседи из знакомства. Другие — модель заново, и файлы читаются заново: условия строк наибольшей
    /// температуры («макс») берут соседей из них.
    fn set_die(&mut self, die: Arc<Die>) {
        if self.die.as_deref() == Some(&*die) {
            return;
        }
        self.refit = true;
        self.files.clear();
        self.die = Some(die);
        self.reset_live();
    }

    fn reset_live(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.source_revision = self.source_revision.saturating_add(1);
        self.live = None;
        self.live_submitted_revision = None;
        self.live_fitted = None;
        if let Some(runtime) = &self.runtime {
            runtime.invalidate(self.generation);
        }
    }

    /// Пора ли подгонять модель в момент `now`: соседи известны и либо сменились (первая подгонка после знакомства —
    /// сразу), либо строки дописаны и с прошлой подгонки прошло не меньше `REFIT` (или её не было).
    fn due(&self, now: Instant) -> bool {
        self.die.is_some()
            && (self.refit || (self.stale && self.fitted.is_none_or(|at| now.saturating_duration_since(at) >= REFIT)))
    }

    /// День вида «по времени», если его пора прочитать: попросили и ещё не читали, или он дописан и прошло
    /// `DAY_REFRESH`.
    fn day_due(&mut self, now: Instant) -> Option<Day> {
        let order = self.day.as_ref()?;
        let due = self
            .day_read
            .is_none_or(|at| self.day_stale && now.saturating_duration_since(at) >= DAY_REFRESH);
        if !due {
            return None;
        }
        let mut names: Vec<String> = self
            .files
            .keys()
            .filter(|n| day_of(n) == Some(order.date.as_str()))
            .cloned()
            .collect(); // a few file names of one day
        names.sort_by(|a, b| order_of(a).cmp(&order_of(b)));
        let day = timeline::read_day(&self.samples, &order.date, &names, &order.blocks, &order.quantities);
        self.day_read = Some(now);
        self.day_stale = false;
        Some(day)
    }

    /// Один проход; Some — что-то изменилось (или это первый проход).
    fn scan(&mut self, first: bool) -> Option<Digest> {
        let started = Instant::now();
        let mut changed = first | self.read_conditional(first);
        let mut trouble = self.live_trouble.clone();
        let mut parsed_bytes = 0;
        let mut parsed_rows = 0;
        let mut source_available = true;

        // Срезы: файлы дня (`day_of`) не старше DAYS дней (у отчёта — все).
        let cutoff = self.days.map(|days| samples::date_before(SystemTime::now(), days));
        let names = match std::fs::read_dir(&self.samples) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| day_of(n).is_some_and(|d| cutoff.as_deref().is_none_or(|c| d >= c)))
                .collect::<Vec<_>>(),
            // Папки срезов нет — программа входа ещё ничего не писала; это не ошибка.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                trouble = Some(format!("папка срезов не прочитана: {error}"));
                source_available = false;
                self.files.keys().cloned().collect() // Ошибка доступа не означает удаления уже прочитанных файлов.
            }
        };
        let before = self.files.len();
        self.files.retain(|name, _| names.contains(name));
        let mut fresh = self.files.len() != before;
        let mut reset = fresh;
        let die = self.die.as_deref();
        for name in names {
            let path = self.samples.join(&name);
            let of_day = self
                .day
                .as_ref()
                .is_some_and(|d| day_of(&name) == Some(d.date.as_str()));
            let file = self.files.entry(name).or_default();
            match read_samples(&path, file, &mut self.scratch, die) {
                Ok(Some((bytes, rows, again))) => {
                    fresh = true;
                    reset |= again;
                    self.day_stale |= of_day;
                    parsed_bytes += bytes;
                    parsed_rows += rows;
                }
                Ok(None) => {}
                Err(e) => {
                    trouble = Some(format!("срезы {} не прочитаны: {e}", path.display()));
                    source_available = false;
                }
            }
        }
        changed |= fresh;
        self.stale |= fresh;
        if reset {
            self.reset_live();
        }
        if fresh {
            self.source_revision = self.source_revision.saturating_add(1);
            self.mark_live_pending();
        }
        changed |= self.update_live(source_available);
        if let Some(message) = &self.live_trouble {
            trouble = Some(message.clone()); // Пользовательская ошибка текущей сводки.
        }
        if !source_available {
            self.mark_live_pending();
            changed = true;
        }
        changed |= self.fit_legacy();

        changed |= self.read_events(cutoff.as_deref(), &mut trouble);

        if !changed {
            return None;
        }
        let mut tally = Tally::default();
        for file in self.files.values() {
            tally.merge(&file.tally);
        }
        let mut days: Vec<String> = self
            .files
            .keys()
            .filter_map(|n| day_of(n))
            .map(str::to_string)
            .collect();
        days.dedup();
        Some(Digest {
            live: self.live.as_ref().map(Arc::clone),
            conditional: self.conditional.as_ref().map(Arc::clone),
            conditional_trouble: self.conditional_trouble.clone(),
            tally,
            model: self.model.as_ref().map(Arc::clone),
            retained: self.retained,
            kept_bytes: self.files.values().map(|f| f.log.bytes()).sum(),
            files: self.files.len(),
            days,
            events: Arc::clone(&self.events),
            parsed_bytes,
            parsed_rows,
            parse_time: started.elapsed(),
            trouble,
        })
    }

    fn fit_legacy(&mut self) -> bool {
        // Прежний расчёт нужен диагностическим видам. Он не владеет хранением исходных строк новой модели.
        if !self.due(Instant::now()) {
            return false;
        }
        let Some(die) = &self.die else { return false };
        let logs: Vec<&Log> = self.files.values().map(|f| &f.log).collect();
        self.model = Some(Arc::new(model::fit(&logs, die)));
        self.retained = None;
        self.fitted = Some(Instant::now());
        self.refit = false;
        self.stale = false;
        true
    }

    fn mark_live_pending(&mut self) {
        let mut live = self.live.as_deref().cloned().unwrap_or_else(|| LiveCurves {
            generation: self.generation,
            source_revision: self.source_revision,
            ..LiveCurves::default()
        }); // Здесь копируются только Arc готовых блоков, не точки и не журналы.
        live.pending = true;
        self.live = Some(Arc::new(live));
    }

    fn update_live(&mut self, source_available: bool) -> bool {
        self.ensure_runtime();
        let Some(runtime) = &mut self.runtime else {
            return false;
        };
        let mut changed = false;
        let update = match runtime.poll() {
            Ok(update) => update,
            Err(message) => {
                self.runtime = None;
                self.live = None;
                self.live_trouble = Some(format!("условная модель не пересчитана: {message}"));
                self.live_fitted = Some(Instant::now());
                return true;
            }
        };
        if let Some(update) = update {
            // pending самого вычислителя означает незавершённый пакет. Проверяется до добавления признака более
            // свежих строк: при непрерывной записи этот второй признак остаётся true и после завершения подгонки.
            self.live_fitted = completed_at(self.live_fitted, self.generation, &update, Instant::now());
            if let Some(live) = receive_live(
                self.live.as_deref(),
                update,
                self.generation,
                self.source_revision,
                self.die.as_ref().map_or(0, |die| die.names.len()),
            ) {
                self.live = Some(Arc::new(live));
                changed = true;
            }
        }
        let due = live_due(
            Instant::now(),
            self.live_fitted,
            self.source_revision,
            self.live_submitted_revision,
        );
        if !source_available || !due || runtime.busy() {
            return changed;
        }
        let Some(die) = &self.die else {
            return changed;
        };
        let batch = conditional_runtime::Batch {
            generation: self.generation,
            source_revision: self.source_revision,
            // Одна копия сырых журналов на пакет нужна, чтобы читатель продолжал принимать новые строки во время
            // подгонки. Одновременно существует только один пакет, без очереди копий по каждому блоку/срезу.
            logs: self.files.values().map(|f| f.log.clone()).collect(),
            die: Arc::clone(die),
            options: crate::conditional_live::Options::default(),
        };
        self.live_submitted_revision = Some(self.source_revision);
        self.live_fitted = Some(Instant::now());
        match runtime.submit(batch) {
            Ok(()) => self.live_trouble = None,
            Err(error) => {
                self.live_trouble = Some(format!("условная модель не пересчитана: {error}"));
                self.live = None;
                self.runtime = None;
            }
        }
        true
    }

    fn ensure_runtime(&mut self) {
        if !self.runtime_enabled || self.runtime.is_some() || self.live_fitted.is_some_and(|at| at.elapsed() < REFIT) {
            return;
        }
        match Runtime::start() {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                self.live_submitted_revision = None;
                self.live_trouble = None;
            }
            Err(error) => {
                self.live_trouble = Some(format!("поток условной модели не запущен: {error}"));
                self.live_fitted = Some(Instant::now());
            }
        }
    }

    fn read_conditional(&mut self, first: bool) -> bool {
        let path = self.dir.join(crate::conditional_snapshot::FILE);
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok(); // Отказ чтения самого файла ниже имеет видимую причину.
        if !first && stamp == self.conditional_stamp {
            return false;
        }
        self.conditional_stamp = stamp;
        match crate::conditional_snapshot::read(&path) {
            Ok(snapshot) => {
                self.conditional = snapshot.map(Arc::new);
                self.conditional_trouble = None;
            }
            Err(message) => {
                self.conditional = None;
                self.conditional_trouble = Some(message);
            }
        }
        true
    }

    /// Журнал автомодуля и файлы событий не старше `cutoff`: только дописываются. Что-то дописано — события для окна
    /// заново (`events::merged`), true.
    fn read_events(&mut self, cutoff: Option<&str>, trouble: &mut Option<String>) -> bool {
        let mut logged = false;
        match read_log(&self.dir.join("auto-log.jsonl"), &mut self.auto_tail, &mut self.auto) {
            Ok(fresh) => logged |= fresh,
            Err(e) => *trouble = Some(format!("журнал автомодуля не прочитан: {e}")),
        }
        let folder = self.dir.join("events");
        let names = match std::fs::read_dir(&folder) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| {
                    n.strip_suffix(".jsonl")
                        .filter(|stem| stem.len() == 10)
                        .and_then(dated)
                        .is_some_and(|d| cutoff.is_none_or(|c| d >= c))
                })
                .collect::<Vec<_>>(),
            // Папки событий нет — программа входа их ещё не писала (до 0.5.6); это не ошибка.
            Err(_) => Vec::new(),
        };
        let before = self.event_files.len();
        self.event_files.retain(|name, _| names.contains(name));
        logged |= self.event_files.len() != before;
        for name in names {
            let path = folder.join(&name);
            let (tail, lines) = self.event_files.entry(name).or_default();
            match read_log(&path, tail, lines) {
                Ok(fresh) => logged |= fresh,
                Err(e) => *trouble = Some(format!("события {} не прочитаны: {e}", path.display())),
            }
        }
        if logged {
            let all: Vec<EventEntry> = self.event_files.values().flat_map(|(_, l)| l.iter().cloned()).collect(); // a few hundred lines, when a log grew
            self.events = Arc::new(events::merged(&all, &self.auto));
        }
        logged
    }
}

fn completed_at(previous: Option<Instant>, generation: u64, update: &LiveCurves, now: Instant) -> Option<Instant> {
    if update.generation == generation && !update.pending {
        Some(now)
    } else {
        previous
    }
}

fn live_due(now: Instant, completed: Option<Instant>, source_revision: u64, submitted: Option<u64>) -> bool {
    submitted != Some(source_revision) && completed.is_none_or(|at| now.saturating_duration_since(at) >= REFIT)
}

fn receive_live(
    previous: Option<&LiveCurves>,
    update: Arc<LiveCurves>,
    generation: u64,
    source_revision: u64,
    blocks: usize,
) -> Option<LiveCurves> {
    if update.generation != generation {
        return None;
    }
    let pending = update.pending;
    // При обновлении полный предыдущий пакет остаётся целиком и с прежней подписью источника. Старт без такого
    // пакета показывает блоки по мере готовности. Финальный отказ блока заменяет старую кривую, как и успех.
    let mut live = if let Some(previous) = previous
        && previous.generation == generation
        && previous.blocks.len() == blocks
        && pending
    {
        previous.clone() // Только Arc готовых блоков, без копии точек/журналов.
    } else {
        Arc::unwrap_or_clone(update)
    };
    live.pending |= pending || live.source_revision != source_revision;
    Some(live)
}

/// Все файлы срезов папки `samples` (без границы в 30 дней) — для скрытого отчёта модели: тот же разбор, что у окна.
/// Строки для модели по файлам и что не прочиталось. Журнала автомодуля рядом нет — он не нужен.
pub fn read_all(samples: PathBuf) -> (Vec<Log>, Option<String>) {
    let mut worker = Worker {
        dir: samples.clone(), // one path, once per report
        samples,
        days: None,
        ..Worker::default()
    };
    let trouble = worker.scan(true).and_then(|d| d.trouble);
    let logs: Vec<Log> = std::mem::take(&mut worker.files).into_values().map(|f| f.log).collect();
    (logs, trouble)
}

/// Дочитывает журнал строк JSON (`events::read_lines`); true — что-то прочитано. Файла нет — не ошибка: программа
/// входа его ещё не писала (файлы событий — с 0.5.6).
fn read_log<T: serde::de::DeserializeOwned>(path: &Path, tail: &mut Tail, into: &mut Vec<T>) -> std::io::Result<bool> {
    match samples::read_new(path, tail) {
        Ok(Fresh::Same) => Ok(false),
        Ok(Fresh::More(text)) => {
            events::read_lines(&text, into);
            Ok(true)
        }
        Ok(Fresh::Again(text)) => {
            into.clear();
            events::read_lines(&text, into);
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// День файла срезов по его имени: «ГГГГ-ММ-ДД.csv» — файл дня; «ГГГГ-ММ-ДД-N.csv» (N — цифры) — продолжение того же
/// дня: программа входа начинает его, когда у файла дня заголовок прежнего вида (добавились столбцы). Каждый файл
/// читается сам по себе — со своим заголовком (столбцы — по названиям, лишние пропускаются) и своей цепочкой времени
/// строк. Другое имя — не срезы (None).
fn day_of(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".csv")?;
    let day = dated(stem)?;
    let rest = &stem[10..];
    let part = rest.is_empty()
        || rest
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
    part.then_some(day)
}

/// Начало имени — дата «ГГГГ-ММ-ДД»; иначе None.
fn dated(stem: &str) -> Option<&str> {
    let day = stem.get(..10)?;
    let b = day.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    (digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10)).then_some(day)
}

/// Порядок файлов срезов по времени: день, затем номер продолжения (у файла дня — 0). По имени «-2» шло бы раньше
/// файла дня («-» меньше «.»).
fn order_of(name: &str) -> (&str, u32) {
    let day = day_of(name).unwrap_or("");
    let part = name
        .strip_suffix(".csv")
        .and_then(|stem| stem.get(10..))
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    (day, part)
}

/// Дочитывает файл срезов. Some((байт, строк)) — прочитано новое; None — файл не менялся. `die` — блоки и соседи:
/// столбцы файла узнают по ним соседей блоков (`Columns::link`).
fn read_samples(
    path: &Path,
    file: &mut SampleFile,
    scratch: &mut Scratch,
    die: Option<&Die>,
) -> std::io::Result<Option<(u64, u64, bool)>> {
    let (text, again) = match samples::read_new(path, &mut file.tail)? {
        Fresh::Same => return Ok(None),
        Fresh::More(text) => (text, false),
        Fresh::Again(text) => {
            file.headed = false;
            file.columns = None;
            file.tally = Tally::default();
            file.carry = Carry::default();
            file.log = Log::default();
            (text, true)
        }
    };
    let mut body = text.as_str();
    if !file.headed {
        // Первая полная строка файла — заголовок (read_new отдаёт только полные строки).
        let Some((header, rest)) = body.split_once('\n') else {
            return Ok(Some((text.len() as u64, 0, again)));
        };
        file.headed = true;
        file.columns = Columns::parse(header);
        if let Some(columns) = &mut file.columns {
            if let Some(die) = die {
                columns.link(&die.names, &die.neighbours);
            }
            file.log = Log::new(columns.names().map(str::to_string).collect());
        }
        body = rest;
    }
    let rows_before = file.tally.rows;
    if let Some(columns) = &file.columns {
        scratch.count_keeping(columns, body, &mut file.carry, &mut file.tally, Some(&mut file.log));
    }
    Ok(Some((text.len() as u64, file.tally.rows - rows_before, again)))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// Строки по 0,25 с одного блока «0» с `from` по `to` (секунды дня `day`), заголовок с миллисекундами.
    fn quarter_rows(day: &str, from: u32, to: u32) -> String {
        let mut text = String::from("time,package,0.ceiling,0.ratio,0.busy,0.temp\n");
        for q in from * 4..to * 4 {
            let (s, ms) = (q / 4, q % 4 * 250);
            let _ = writeln!(
                text,
                "{day} {:02}:{:02}:{:02}.{ms:03},60,53,45,0.8,70",
                s / 3600,
                s / 60 % 60,
                s % 60
            ); // into a String
        }
        text
    }

    fn one_block() -> Arc<Die> {
        Arc::new(Die {
            names: vec!["0".into()],
            e: vec![false],
            neighbours: vec![Vec::new()],
        })
    }

    fn temp_samples(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("coremap-window-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // остатки прошлого прогона с тем же номером процесса
        let samples = dir.join("samples");
        std::fs::create_dir_all(&samples).expect("папка срезов");
        (dir, samples)
    }

    #[test]
    fn canonical_raw_history_survives_a_saturated_legacy_fit() {
        let (dir, samples) = temp_samples("raw-not-legacy-retained");
        let mut text = String::from("time,package,0.ceiling,0.ratio,0.busy,0.temp,1.ceiling,1.ratio,1.busy,1.temp\n");
        for q in 0..7200u32 {
            let (s, ms) = (q / 4, q % 4 * 250);
            let (ratio, temp) = if (3000..3200).contains(&q) { (40, 62) } else { (45, 66) };
            writeln!(
                text,
                "2026-10-15 01:{:02}:{:02}.{ms:03},60,53,{ratio},0.8,{temp},53,45,0.8,58",
                s / 60,
                s % 60
            )
            .expect("строка в памяти");
        }
        std::fs::write(samples.join("2026-10-15.csv"), text).expect("сырые измерения");
        let mut worker = Worker {
            dir: dir.clone(),
            samples,
            days: None,
            die: Some(Arc::new(Die {
                names: vec!["0".into(), "1".into()],
                e: vec![false; 2],
                neighbours: vec![vec![1], vec![0]],
            })),
            refit: true,
            ..Worker::default()
        };
        let digest = worker.scan(true).expect("проход с подгонкой");
        assert_eq!(digest.tally.rows, 7200);
        assert_eq!(worker.files.values().map(|f| f.log.rows()).sum::<usize>(), 7200);
        assert!(
            digest.retained.is_none(),
            "прежняя насыщенность не сокращает историю новой модели"
        );
        let _ = std::fs::remove_dir_all(dir); // Только временный каталог данного теста.
    }

    #[test]
    fn raw_append_marks_old_curve_pending_and_rewrite_invalidates_its_generation() {
        let (dir, samples) = temp_samples("live-revision");
        let file = samples.join("2026-10-15.csv");
        std::fs::write(&file, quarter_rows("2026-10-15", 3600, 3610)).expect("первый журнал");
        let mut worker = Worker {
            dir: dir.clone(),
            samples,
            days: None,
            die: Some(one_block()),
            ..Worker::default()
        };
        worker.scan(true).expect("чтение");
        let generation = worker.generation;
        let revision = worker.source_revision;
        worker.live = Some(Arc::new(LiveCurves {
            generation,
            source_revision: revision,
            pending: false,
            blocks: Vec::new(),
        }));
        let mut text = std::fs::read_to_string(&file).expect("журнал");
        text.push_str(
            quarter_rows("2026-10-15", 3610, 3620)
                .split_once('\n')
                .expect("заголовок")
                .1,
        );
        std::fs::write(&file, text).expect("дописанные строки");
        let appended = worker.scan(false).expect("новая сводка");
        assert!(worker.source_revision > revision);
        let live = appended.live.expect("свежесть кривой");
        assert!(live.pending);
        assert_eq!(
            live.source_revision, revision,
            "старый результат не получает новую подпись источника"
        );
        assert_eq!(worker.files.values().map(|f| f.log.rows()).sum::<usize>(), 80);

        std::fs::write(&file, quarter_rows("2026-10-15", 3600, 3601)).expect("перезаписанный журнал");
        let rewritten = worker.scan(false).expect("сброс источника");
        assert!(worker.generation > generation);
        let live = rewritten.live.expect("ожидание новой модели");
        assert!(live.pending && live.blocks.is_empty());
        assert_eq!(live.generation, worker.generation);
        assert_eq!(worker.files.values().map(|f| f.log.rows()).sum::<usize>(), 4);
        let _ = std::fs::remove_dir_all(dir); // Только временный каталог данного теста.
    }

    #[test]
    fn complete_revision_stays_visible_until_refresh_finishes_including_a_failed_block() {
        let block = |name: &str, status: &str| {
            Arc::new(crate::conditional_runtime::LiveBlock {
                name: name.into(),
                observed: None,
                sustained: None,
                status: status.into(),
                prepared_rows: 0,
                training_rows: 0,
                eligible_rows: 0,
                first_ms: None,
                last_ms: None,
                coverage: Vec::new(),
                omitted_contexts: Vec::new(),
                options: crate::conditional_live::Options::default(),
            })
        };
        let previous = LiveCurves {
            generation: 3,
            source_revision: 10,
            pending: false,
            blocks: vec![block("0", "предыдущий результат"), block("1", "предыдущий результат")],
        };
        let partial = Arc::new(LiveCurves {
            generation: 3,
            source_revision: 11,
            pending: true,
            blocks: vec![block("0", "новый результат")],
        });
        let visible = receive_live(Some(&previous), Arc::clone(&partial), 3, 12, 2).expect("ожидание обновления");
        assert_eq!(visible.source_revision, 10);
        assert!(visible.pending && Arc::ptr_eq(&visible.blocks[1], &previous.blocks[1]));
        let startup = receive_live(None, partial, 3, 12, 2).expect("первый расчёт поступает по блокам");
        assert_eq!(startup.blocks.len(), 1);
        assert_eq!(startup.source_revision, 11);

        let complete = Arc::new(LiveCurves {
            generation: 3,
            source_revision: 11,
            pending: false,
            blocks: vec![block("0", "новый результат"), block("1", "новая подгонка не сошлась")],
        });
        let replaced = receive_live(Some(&previous), Arc::clone(&complete), 3, 12, 2).expect("новый полный результат");
        assert_eq!(replaced.source_revision, 11);
        assert!(replaced.pending, "появилась ещё более свежая исходная запись");
        assert_eq!(replaced.blocks[1].status, "новая подгонка не сошлась");
        assert!(!Arc::ptr_eq(&replaced.blocks[1], &previous.blocks[1]));
        assert!(
            receive_live(None, complete, 4, 12, 2).is_none(),
            "старое поколение не проходит"
        );
    }

    #[test]
    fn live_refresh_pause_starts_at_completion_even_when_a_batch_takes_longer_than_the_interval() {
        let started = Instant::now();
        let finished = started + REFIT + Duration::from_secs(10);
        let mut update = LiveCurves {
            generation: 2,
            source_revision: 8,
            pending: true,
            blocks: Vec::new(),
        };
        let before = completed_at(Some(started), 2, &update, finished);
        assert_eq!(before, Some(started), "промежуточный блок не завершает пакет");
        update.pending = false;
        let completed = completed_at(before, 2, &update, finished);
        assert_eq!(completed, Some(finished));
        assert!(
            !live_due(finished, completed, 9, Some(8)),
            "нет немедленной новой подгонки"
        );
        assert!(!live_due(
            finished + REFIT.saturating_sub(Duration::from_millis(1)),
            completed,
            9,
            Some(8)
        ));
        assert!(live_due(finished + REFIT, completed, 9, Some(8)));
        assert!(
            !live_due(finished + REFIT, completed, 8, Some(8)),
            "источник не менялся"
        );
        assert_eq!(
            completed_at(None, 3, &update, finished),
            None,
            "отменённый пакет не задерживает новое поколение"
        );
        assert!(
            live_due(finished, None, 1, None),
            "первый пакет нового поколения начинается сразу"
        );
    }

    #[test]
    fn the_model_takes_every_row_of_the_period_not_only_the_last_8_hours() {
        // Вчера 20:00–20:01 (за 10 ч до самой новой строки) и сегодня 06:00–06:01: обе минуты — в модели (границы 8 ч
        // больше нет, бриф W). Ловит: прежнюю границу.
        let (dir, samples) = temp_samples("all");
        std::fs::write(
            samples.join("2026-10-14.csv"),
            quarter_rows("2026-10-14", 20 * 3600, 20 * 3600 + 60),
        )
        .expect("вчера");
        std::fs::write(
            samples.join("2026-10-15.csv"),
            quarter_rows("2026-10-15", 6 * 3600, 6 * 3600 + 60),
        )
        .expect("сегодня");
        let mut worker = Worker {
            dir: dir.clone(),
            samples: samples.clone(),
            days: None,
            die: Some(one_block()),
            refit: true,
            ..Worker::default()
        };
        worker.scan(true).expect("проход");
        let (logs, trouble) = read_all(samples);
        let _ = std::fs::remove_dir_all(&dir); // временная папка теста; не удалилась — не беда
        assert!(
            worker.model.as_ref().is_some_and(|m| m.rows == 480),
            "в модели обе минуты"
        );
        assert!(trouble.is_none(), "{trouble:?}");
        assert_eq!(logs.iter().map(Log::rows).sum::<usize>(), 480, "отчёт — те же строки");
    }

    #[test]
    fn the_window_reads_only_after_hello_and_reads_again_for_new_blocks() {
        // Окно: без блоков прохода нет (условиям «макс» нужны соседи); блоки пришли — проход; другие блоки — файлы
        // читаются заново. Ловит: проход до знакомства, прежние сводки файлов после смены блоков.
        let (dir, samples) = temp_samples("die");
        std::fs::write(samples.join("2026-10-15.csv"), quarter_rows("2026-10-15", 3600, 3610)).expect("файл дня");
        let mut worker = Worker {
            dir: dir.clone(),
            samples,
            days: None,
            wait_die: true,
            ..Worker::default()
        };
        worker.set_die(one_block());
        assert!(worker.refit);
        let first = worker.scan(true).expect("проход");
        assert_eq!(first.tally.rows, 40);
        worker.set_die(Arc::new(Die {
            names: vec!["0".into(), "1".into()],
            e: vec![false, false],
            neighbours: vec![vec![1], vec![0]],
        }));
        assert!(worker.files.is_empty() && worker.refit, "файлы — заново");
        let again = worker.scan(false).expect("проход");
        let _ = std::fs::remove_dir_all(&dir); // временная папка теста; не удалилась — не беда
        assert_eq!(again.tally.rows, 40, "те же строки, не вдвое");
    }

    #[test]
    fn the_model_is_refitted_at_once_after_hello_then_at_most_once_a_minute() {
        // Ловит: подгонку на каждый проход (раз в 5 с), отложенную первую подгонку, потерянные последние строки (после
        // минуты подгонка идёт и без новых строк).
        let (dir, samples) = temp_samples("refit");
        let file = samples.join("2026-10-15.csv");
        std::fs::write(&file, quarter_rows("2026-10-15", 3600, 3660)).expect("файл дня");
        let mut worker = Worker {
            dir: dir.clone(),
            samples,
            days: None,
            die: Some(one_block()),
            refit: true,
            ..Worker::default()
        };
        worker.scan(true).expect("первый проход");
        let first = worker.model.clone().expect("первая подгонка — сразу");
        assert_eq!(first.rows, 240);
        // Строки дописаны через 5 с: прошлая подгонка моложе минуты — модель та же.
        let more = quarter_rows("2026-10-15", 3660, 3720);
        let mut text = std::fs::read_to_string(&file).expect("файл дня");
        text.push_str(more.split_once('\n').map_or("", |x| x.1));
        std::fs::write(&file, text).expect("дописано");
        assert!(
            worker.scan(false).is_some(),
            "строки дописаны — сводка идёт (статистика графика)"
        );
        let same = worker.model.clone().expect("модель");
        assert!(Arc::ptr_eq(&first, &same), "второй подгонки в течение минуты нет");
        assert!(worker.stale);
        // Минута прошла, новых строк нет: подгонка с дописанными строками.
        worker.fitted = Instant::now().checked_sub(REFIT + Duration::from_secs(1));
        let digest = worker.scan(false).expect("подгонка — новая сводка");
        let next = digest.model.expect("модель");
        assert!(!Arc::ptr_eq(&first, &next) && next.rows == 480, "{}", next.rows);
        assert!(!worker.stale);
        let quiet = worker.scan(false);
        let _ = std::fs::remove_dir_all(&dir); // временная папка теста; не удалилась — не беда
        assert!(quiet.is_none(), "ничего не дописано — ни подгонки, ни сводки");
    }

    #[test]
    fn a_day_is_read_on_request_from_its_files_and_again_only_after_new_rows() {
        // День 15.10 — файл дня и продолжение, 14.10 — другой день. Просьба — день читается целиком (обе части по
        // порядку); без новых строк — не перечитывается; дописан — перечитывается не раньше `DAY_REFRESH`.
        let (dir, samples) = temp_samples("day");
        std::fs::write(samples.join("2026-10-14.csv"), quarter_rows("2026-10-14", 3600, 3610)).expect("вчера");
        std::fs::write(samples.join("2026-10-15.csv"), quarter_rows("2026-10-15", 3600, 3610)).expect("файл дня");
        let part = samples.join("2026-10-15-2.csv");
        std::fs::write(&part, quarter_rows("2026-10-15", 7200, 7205)).expect("продолжение");
        let mut worker = Worker {
            dir: dir.clone(),
            samples,
            days: None,
            ..Worker::default()
        };
        let digest = worker.scan(true).expect("проход");
        assert_eq!(digest.days, ["2026-10-14", "2026-10-15"]);
        let now = Instant::now();
        assert!(worker.day_due(now).is_none(), "не просили — не читается");
        worker.day = Some(DayOrder {
            date: "2026-10-15".into(),
            blocks: vec!["0".into()],
            quantities: vec![Quantity::Temp, Quantity::Ceiling],
        });
        let day = worker.day_due(now).expect("день");
        assert_eq!(day.t.len(), 60, "обе части дня");
        assert_eq!(day.gaps().len(), 1, "между частями — час без строк, одним перерывом");
        assert!(day.blocks[0].ratio.is_empty(), "о множителе не просили");
        assert!(day.t.windows(2).all(|w| w[0] < w[1]));
        assert!((day.blocks[0].temp[0] - 70.0).abs() < 1e-6 && (day.blocks[0].ceiling[59] - 53.0).abs() < 1e-6);
        assert!(worker.day_due(now).is_none(), "уже прочитан, новых строк нет");
        let mut text = std::fs::read_to_string(&part).expect("продолжение");
        text.push_str(
            quarter_rows("2026-10-15", 7205, 7206)
                .split_once('\n')
                .map_or("", |x| x.1),
        );
        std::fs::write(&part, text).expect("дописано");
        worker.scan(false);
        assert!(worker.day_due(now).is_none(), "дописан, но `DAY_REFRESH` не прошло");
        let later = now + DAY_REFRESH;
        let again = worker.day_due(later).expect("перечитан");
        let _ = std::fs::remove_dir_all(&dir); // временная папка теста; не удалилась — не беда
        assert_eq!(again.t.len(), 64);
    }

    #[test]
    fn files_are_read_newest_first() {
        let mut names = vec![
            "2026-10-15-2.csv",
            "2026-10-14.csv",
            "2026-10-15.csv",
            "2026-10-15-10.csv",
        ];
        names.sort_by(|a, b| order_of(b).cmp(&order_of(a)));
        assert_eq!(
            names,
            [
                "2026-10-15-10.csv",
                "2026-10-15-2.csv",
                "2026-10-15.csv",
                "2026-10-14.csv"
            ]
        );
    }

    #[test]
    fn day_files_and_their_continuations_are_samples_other_names_are_not() {
        assert_eq!(day_of("2026-10-15.csv"), Some("2026-10-15"));
        assert_eq!(day_of("2026-10-15-2.csv"), Some("2026-10-15"));
        assert_eq!(day_of("2026-10-15-12.csv"), Some("2026-10-15"));
        for other in [
            "2026-10-15-x.csv",
            "2026-10-15-.csv",
            "2026-10-15-2a.csv",
            "2026-10-15_2.csv",
            "notes.csv",
            "2026-10-15.txt",
            "2026-1-15.csv",
            "ab26-10-15.csv",
        ] {
            assert_eq!(day_of(other), None, "{other}");
        }
    }

    #[test]
    fn a_continuation_file_with_new_columns_is_read_with_its_own_header() {
        let dir = std::env::temp_dir().join(format!("coremap-window-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // остатки прошлого прогона с тем же номером процесса
        let samples = dir.join("samples");
        std::fs::create_dir_all(&samples).expect("папка срезов");
        let today = samples::date_before(SystemTime::now(), 0);
        let old =
            "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp,2–5.ceiling,2–5.ratio,2–5.busy,2–5.temp";
        // Файл дня — прежний заголовок, две строки по 2 с.
        std::fs::write(
            samples.join(format!("{today}.csv")),
            format!(
                "{old}
{today} 10:00:00,60,3000,,53,50,1,80,46,44,0.5,60
{today} 10:00:02,60,,,53,50,1,82,46,44,0.5,60
"
            ),
        )
        .expect("файл дня");
        // Продолжение дня: столбцы GPU и мощности добавлены в конец, порядок блоков другой, строки по 0,25 с.
        let new = "time,package,fan_cpu,fan_gpu,2–5.ceiling,2–5.ratio,2–5.busy,2–5.temp,0.ceiling,0.ratio,0.busy,0.temp,power_cpu,temp_gpu,power_gpu,busy_gpu";
        std::fs::write(
            samples.join(format!("{today}-2.csv")),
            format!(
                "{new}
{today} 11:00:00.000,61,3100,,46,44,0.5,62,53,51,1,90,45.5,70,120.0,0.93
{today} 11:00:00.250,61,,,46,44,0.5,62,53,51,1,90,45.1,70,118.2,0.91
"
            ),
        )
        .expect("продолжение дня");
        // Не срезы: имя не по правилу.
        std::fs::write(
            samples.join(format!("{today}-x.csv")),
            format!(
                "{old}
{today} 12:00:00,60,,,53,52,1,99,46,44,0.5,60
"
            ),
        )
        .expect("чужой файл");
        std::fs::write(
            samples.join("notes.csv"),
            "time
",
        )
        .expect("чужой файл");
        let mut worker = Worker {
            dir: dir.clone(),
            samples,
            days: Some(DAYS),
            ..Worker::default()
        };
        let digest = worker.scan(true).expect("первый проход");
        let _ = std::fs::remove_dir_all(&dir); // временная папка теста; не удалилась — не беда
        assert_eq!(digest.files, 2, "файл дня и его продолжение; чужие имена не читаются");
        assert_eq!(digest.tally.rows, 4);
        let core = &digest.tally.blocks["0"];
        // Ядро 0 нашлось по названию в обоих файлах: ×50 из файла дня (2 + 2 с), ×51 из продолжения (0,25 + 0,25 с).
        assert_eq!(core.bins[&50].n, 2);
        assert_eq!(core.bins[&51].n, 2);
        assert!((core.bins[&51].secs - 0.5).abs() < 1e-9, "{}", core.bins[&51].secs);
        assert_eq!(core.bins[&51].mean(), Some(90.0));
        assert!(
            !core.bins.contains_key(&52),
            "строка из файла с чужим именем не посчитана"
        );
        // Время — по строкам каждого файла: 2 + 2 с в файле дня и 0,25 + 0,25 с в продолжении.
        assert!((digest.tally.secs - (4.0 + 0.5)).abs() < 1e-9, "{}", digest.tally.secs);
    }
}
