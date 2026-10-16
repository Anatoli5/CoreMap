// Статистика читается в своём потоке, не в потоке окна (rust\AGENTS.md: поток окна не ждёт файлов).
//
// Поток держит сводку по каждому файлу срезов за последние 30 дней и сколько от файла прочитано; раз в SCAN он
// смотрит размеры и времена записи (одна папка: файл на день и его продолжения «-N», и журнал автомодуля) и дочитывает только
// дописанное — прошлые дни не перечитываются, сегодняшний — только с прочитанного места. Ничего не изменилось — окну
// ничего не идёт. Изменилось — окно получает новую сводку целиком (Digest) и перерисовывает то, что из неё показывает.
//
// Модель нагрева (model.rs, DECISIONS #39) — здесь же, после прохода: строки по 0,25 с каждого файла оставляет тот же
// разбор (`Scratch::count_keeping`), второго чтения файлов нет; в модель идут только последние 8 ч строк
// (`model::KEEP_MS`, через все файлы). Соседей блоков поток знает из знакомства окна (`Order::Die`); пока их нет — модели
// нет. Подгонка — заново по всем оставленным строкам: сразу после знакомства (или смены соседей), а пока срезы
// дописываются — не чаще раза в `REFIT`. Окно её не ждёт: она приходит в той же сводке.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::model::{self, Die, Log, Model};
use crate::samples::{self, AutoEntry, Columns, Fresh, Scratch, Tail, Tally};

/// Как часто смотреть, не дописаны ли журналы. Программа входа пишет срез раз в 0,25 с (до 0.5.2 — раз в 2 с); график
/// в окне отстаёт от неё не больше чем на 5 с, а проверка — несколько вызовов Windows и разбор только дописанного.
pub const SCAN: Duration = Duration::from_secs(5);
/// Срезы за сколько дней (программа входа хранит 30 и удаляет старые сама).
pub const DAYS: i64 = 30;
/// Подгонка модели, пока срезы дописываются, — не чаще раза в минуту: на 8 ч строк она стоит ~0,2 с, а минута новых
/// строк — 1/480 данных модели, кривые от неё почти не меняются. Последние строки входят не позже чем через минуту.
pub const REFIT: Duration = Duration::from_secs(60);

/// Что окно получает из потока статистики.
#[derive(Debug, Default)]
pub struct Digest {
    pub tally: Tally,
    pub files: usize,
    pub auto: Vec<AutoEntry>,
    /// Сколько байт срезов прочитано и разобрано этим проходом и за сколько: цена статистики.
    pub parsed_bytes: u64,
    pub parsed_rows: u64,
    pub parse_time: Duration,
    /// Что не прочиталось (строка для владельца); None — всё прочиталось.
    pub trouble: Option<String>,
    /// Модель нагрева по всем строкам срезов; None — соседи блоков ещё не известны (знакомства не было).
    pub model: Option<Arc<Model>>,
    /// Сколько памяти держат строки для модели, байт.
    pub kept_bytes: usize,
}

/// Что окно говорит потоку статистики.
#[derive(Debug)]
pub enum Order {
    /// Проход сейчас, не дожидаясь `SCAN`.
    Scan,
    /// Блоки и их соседи из знакомства: модель считается по ним.
    Die(Arc<Die>),
}

#[derive(Debug, Default)]
struct SampleFile {
    tail: Tail,
    /// Первая строка файла прочитана: заголовок разобран (`columns`) или это не заголовок срезов (None).
    headed: bool,
    columns: Option<Columns>,
    tally: Tally,
    /// Время последней посчитанной строки (мс, `samples::row_time`): следующая порция считает промежуток от неё.
    previous: Option<i64>,
    /// Строки по 0,25 с — для модели.
    log: Log,
}

#[derive(Debug, Default)]
struct Worker {
    dir: PathBuf,
    /// Папка срезов: у окна — samples в папке данных, у отчёта — та, что ему дали.
    samples: PathBuf,
    /// Срезы за сколько дней; None — все файлы папки (скрытый отчёт модели).
    days: Option<i64>,
    files: BTreeMap<String, SampleFile>,
    scratch: Scratch,
    auto_tail: Tail,
    auto: Vec<AutoEntry>,
    die: Option<Arc<Die>>,
    /// Соседи сменились — модель пересчитать при следующем проходе сразу, даже если файлы не менялись.
    refit: bool,
    /// Строки дописаны после последней подгонки — пересчитать, когда пройдёт `REFIT`.
    stale: bool,
    /// Когда была последняя подгонка.
    fitted: Option<Instant>,
    model: Option<Arc<Model>>,
}

/// Запускает поток статистики над папкой данных `dir`. `deliver` получает сводку, когда она изменилась (первую —
/// сразу после первого прохода). Отправитель — чтобы попросить проход сейчас или дать блоки и соседей для модели;
/// когда все отправители брошены, поток кончается.
pub fn start(dir: PathBuf, deliver: impl Fn(Arc<Digest>) + Send + 'static) -> mpsc::Sender<Order> {
    let (wake, woken) = mpsc::channel::<Order>();
    let mut worker = Worker {
        samples: dir.join("samples"),
        dir,
        days: Some(DAYS),
        ..Worker::default()
    };
    thread::Builder::new()
        .name("статистика".into())
        .spawn(move || {
            let mut first = true;
            loop {
                if let Some(digest) = worker.scan(first) {
                    deliver(Arc::new(digest));
                }
                first = false;
                match woken.recv_timeout(SCAN) {
                    Ok(Order::Scan) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(Order::Die(die)) => {
                        worker.refit = worker.die.as_deref() != Some(&*die);
                        worker.die = Some(die);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("поток статистики");
    wake
}

impl Worker {
    /// Пора ли подгонять модель в момент `now`: соседи известны и либо сменились (первая подгонка после знакомства —
    /// сразу), либо строки дописаны и с прошлой подгонки прошло не меньше `REFIT` (или её не было).
    fn due(&self, now: Instant) -> bool {
        self.die.is_some()
            && (self.refit || (self.stale && self.fitted.is_none_or(|at| now.saturating_duration_since(at) >= REFIT)))
    }

    /// Один проход; Some — что-то изменилось (или это первый проход).
    fn scan(&mut self, first: bool) -> Option<Digest> {
        let started = Instant::now();
        let mut changed = first;
        let mut trouble = None;
        let mut parsed_bytes = 0;
        let mut parsed_rows = 0;

        // Срезы: файлы дня (`day_of`) не старше DAYS дней (у отчёта — все).
        let cutoff = self.days.map(|days| samples::date_before(SystemTime::now(), days));
        let names = match std::fs::read_dir(&self.samples) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| day_of(n).is_some_and(|d| cutoff.as_deref().is_none_or(|c| d >= c)))
                .collect::<Vec<_>>(),
            // Папки срезов нет — программа входа ещё ничего не писала; это не ошибка.
            Err(_) => Vec::new(),
        };
        let before = self.files.len();
        self.files.retain(|name, _| names.contains(name));
        let mut fresh = self.files.len() != before;
        // Новые файлы — первыми: граница 8 ч (`model::KEEP_MS`) известна по самому новому, и разбор старых дней не берёт
        // строк, которые выбросит подгонка (иначе первый проход держал бы строки всех 30 дней).
        let mut names = names;
        names.sort_by(|a, b| order_of(b).cmp(&order_of(a)));
        let mut newest = self.files.values().filter_map(|f| f.log.newest()).max();
        for name in names {
            let path = self.samples.join(&name);
            let file = self.files.entry(name).or_default();
            let from = newest.map(|n| n - model::KEEP_MS);
            let read = read_samples(&path, file, &mut self.scratch, from);
            newest = newest.max(file.log.newest());
            match read {
                Ok(Some((bytes, rows))) => {
                    fresh = true;
                    parsed_bytes += bytes;
                    parsed_rows += rows;
                }
                Ok(None) => {}
                Err(e) => trouble = Some(format!("срезы {} не прочитаны: {e}", path.display())),
            }
        }
        changed |= fresh;
        self.stale |= fresh;
        // Модель — по последним 8 ч строк заново (цена — `Model::time`, в отчёте модели).
        if self.due(Instant::now())
            && let Some(die) = &self.die
        {
            let mut logs: Vec<&mut Log> = self.files.values_mut().map(|f| &mut f.log).collect();
            model::trim(&mut logs);
            let logs: Vec<&Log> = logs.into_iter().map(|l| &*l).collect();
            self.model = Some(Arc::new(model::fit(&logs, die)));
            self.fitted = Some(Instant::now());
            self.refit = false;
            self.stale = false;
            changed = true;
        }

        // Журнал автомодуля: только дописывается.
        match samples::read_new(&self.dir.join("auto-log.jsonl"), &mut self.auto_tail) {
            Ok(Fresh::Same) => {}
            Ok(Fresh::More(text)) => {
                samples::auto_entries(&text, &mut self.auto);
                changed = true;
            }
            Ok(Fresh::Again(text)) => {
                self.auto.clear();
                samples::auto_entries(&text, &mut self.auto);
                changed = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => trouble = Some(format!("журнал автомодуля не прочитан: {e}")),
        }

        if !changed {
            return None;
        }
        let mut tally = Tally::default();
        for file in self.files.values() {
            tally.merge(&file.tally);
        }
        Some(Digest {
            tally,
            model: self.model.as_ref().map(Arc::clone),
            kept_bytes: self.files.values().map(|f| f.log.bytes()).sum(),
            files: self.files.len(),
            // Копия журнала — на каждое изменение, не на каждый проход: действий автомодуля единицы в час.
            auto: self.auto.clone(),
            parsed_bytes,
            parsed_rows,
            parse_time: started.elapsed(),
            trouble,
        })
    }
}

/// Все файлы срезов папки `samples` (без границы в 30 дней) — для скрытого отчёта модели: тот же разбор, что у окна, и
/// те же последние 8 ч строк (`model::trim`), что идут в модель окна. Строки для модели по файлам и что не прочиталось.
/// Журнала автомодуля рядом нет — он не нужен.
pub fn read_all(samples: PathBuf) -> (Vec<Log>, Option<String>) {
    let mut worker = Worker {
        dir: samples.clone(), // one path, once per report
        samples,
        days: None,
        ..Worker::default()
    };
    let trouble = worker.scan(true).and_then(|d| d.trouble);
    let mut logs: Vec<Log> = std::mem::take(&mut worker.files).into_values().map(|f| f.log).collect();
    model::trim(&mut logs.iter_mut().collect::<Vec<_>>());
    (logs, trouble)
}

/// День файла срезов по его имени: «ГГГГ-ММ-ДД.csv» — файл дня; «ГГГГ-ММ-ДД-N.csv» (N — цифры) — продолжение того же
/// дня: программа входа начинает его, когда у файла дня заголовок прежнего вида (добавились столбцы). Каждый файл
/// читается сам по себе — со своим заголовком (столбцы — по названиям, лишние пропускаются) и своей цепочкой времени
/// строк. Другое имя — не срезы (None).
fn day_of(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".csv")?;
    let day = stem.get(..10)?;
    let b = day.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let dated = digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10);
    let rest = &stem[10..];
    let part = rest.is_empty()
        || rest
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
    (dated && part).then_some(day)
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

/// Дочитывает файл срезов. Some((байт, строк)) — прочитано новое; None — файл не менялся. `from` — строки раньше в
/// модель не берутся (граница 8 ч от самой новой строки уже прочитанных файлов).
fn read_samples(
    path: &Path,
    file: &mut SampleFile,
    scratch: &mut Scratch,
    from: Option<i64>,
) -> std::io::Result<Option<(u64, u64)>> {
    let text = match samples::read_new(path, &mut file.tail)? {
        Fresh::Same => return Ok(None),
        Fresh::More(text) => text,
        Fresh::Again(text) => {
            file.headed = false;
            file.columns = None;
            file.tally = Tally::default();
            file.previous = None;
            file.log = Log::default();
            text
        }
    };
    let mut body = text.as_str();
    if !file.headed {
        // Первая полная строка файла — заголовок (read_new отдаёт только полные строки).
        let Some((header, rest)) = body.split_once('\n') else {
            return Ok(Some((text.len() as u64, 0)));
        };
        file.headed = true;
        file.columns = Columns::parse(header);
        if let Some(columns) = &file.columns {
            file.log = Log::new(columns.names().map(str::to_string).collect());
        }
        body = rest;
    }
    let rows_before = file.tally.rows;
    if let Some(at) = from {
        file.log.admit_from(at);
    }
    if let Some(columns) = &file.columns {
        scratch.count_keeping(columns, body, &mut file.previous, &mut file.tally, Some(&mut file.log));
    }
    Ok(Some((text.len() as u64, file.tally.rows - rows_before)))
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
    fn the_model_keeps_only_rows_within_8_hours_of_the_newest_across_files() {
        // Вчера 20:00–20:01 (за 10 ч до самой новой) и 23:00–23:01 (за 7 ч) в одном файле, сегодня 06:00–06:01 — в
        // файле дня и его продолжении. Ловит: границу по своему файлу, а не по самому новому всех файлов; старые дни,
        // прочитанные целиком (порядок по имени ставит «-2» раньше файла дня, а вчера — раньше сегодня).
        let (dir, samples) = temp_samples("keep");
        let mut old = quarter_rows("2026-10-14", 20 * 3600, 20 * 3600 + 60);
        old.push_str(
            quarter_rows("2026-10-14", 23 * 3600, 23 * 3600 + 60)
                .split_once('\n')
                .map_or("", |x| x.1),
        );
        std::fs::write(samples.join("2026-10-14.csv"), old).expect("вчера");
        std::fs::write(
            samples.join("2026-10-15.csv"),
            quarter_rows("2026-10-15", 6 * 3600 - 60, 6 * 3600),
        )
        .expect("сегодня");
        std::fs::write(
            samples.join("2026-10-15-2.csv"),
            quarter_rows("2026-10-15", 6 * 3600, 6 * 3600 + 60),
        )
        .expect("продолжение");
        let mut worker = Worker {
            dir: dir.clone(),
            samples: samples.clone(),
            days: None,
            ..Worker::default()
        };
        // Без знакомства подгонки нет: граница держится уже разбором (новые файлы первыми).
        worker.scan(true).expect("первый проход");
        let rows = |w: &Worker, name: &str| w.files.get(name).map_or(0, |f| f.log.rows());
        assert_eq!(rows(&worker, "2026-10-14.csv"), 240, "вчера — только 23:00–23:01");
        assert_eq!(rows(&worker, "2026-10-15.csv") + rows(&worker, "2026-10-15-2.csv"), 480);
        let newest = worker
            .files
            .values()
            .filter_map(|f| f.log.newest())
            .max()
            .expect("строки есть");
        // Подгонка: та же граница; отчёт (read_all) — тоже.
        worker.die = Some(one_block());
        worker.refit = true;
        worker.scan(false).expect("подгонка");
        assert!(
            worker.model.as_ref().is_some_and(|m| m.rows == 720),
            "в модели 720 строк"
        );
        let (logs, trouble) = read_all(samples);
        let _ = std::fs::remove_dir_all(&dir); // временная папка теста; не удалилась — не беда
        assert!(trouble.is_none(), "{trouble:?}");
        assert_eq!(logs.iter().map(Log::rows).sum::<usize>(), 720, "отчёт — те же 8 ч");
        assert!(
            logs.iter()
                .all(|l| l.rows() == 0 || l.newest().is_some_and(|n| n >= newest - model::KEEP_MS))
        );
    }

    #[test]
    fn rows_aged_out_while_logging_are_dropped_before_the_next_fit() {
        // Файл пишется: самая новая строка ушла на 8 ч вперёд — ранние строки того же файла выбрасываются перед
        // подгонкой. Ловит: границу только при разборе (взятые строки остались бы навсегда).
        let mut log = model::Log::new(vec!["0".into()]);
        let row = |log: &mut model::Log, at: i64| {
            log.push(at, [[Some(45.0), Some(0.8), Some(70.0), None]].into_iter(), [None; 3]);
        };
        for k in 0..10 {
            row(&mut log, k * 250);
        }
        row(&mut log, model::KEEP_MS + 1000);
        assert_eq!(model::trim(&mut [&mut log]), Some(1000));
        assert_eq!(
            log.rows(),
            7,
            "строки 0–750 мс ушли, 1000 мс (ровно на границе) и дальше — остались"
        );
        row(&mut log, 500);
        assert_eq!(log.rows(), 7, "строка раньше границы после неё не берётся");
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
