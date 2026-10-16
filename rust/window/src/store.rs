// Статистика читается в своём потоке, не в потоке окна (rust\AGENTS.md: поток окна не ждёт файлов).
//
// Поток держит сводку по каждому файлу срезов за последние 30 дней и сколько от файла прочитано; раз в SCAN он
// смотрит размеры и времена записи (одна папка: файл на день и его продолжения «-N», и два журнала) и дочитывает только
// дописанное — прошлые дни не перечитываются, сегодняшний — только с прочитанного места. Ничего не изменилось — окну
// ничего не идёт. Изменилось — окно получает новую сводку целиком (Digest) и перерисовывает то, что из неё показывает.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};

use crate::samples::{self, AutoEntry, Columns, Fresh, Scratch, Tail, Tally};

/// Как часто смотреть, не дописаны ли журналы. Программа входа пишет срез раз в 0,25 с (до 0.5.2 — раз в 2 с); график
/// в окне отстаёт от неё не больше чем на 5 с, а проверка — несколько вызовов Windows и разбор только дописанного.
pub const SCAN: Duration = Duration::from_secs(5);
/// Срезы за сколько дней (программа входа хранит 30 и удаляет старые сама).
pub const DAYS: i64 = 30;

/// Что окно получает из потока статистики.
#[derive(Debug, Default)]
pub struct Digest {
    pub tally: Tally,
    pub files: usize,
    pub auto: Vec<AutoEntry>,
    /// Журнал смен профиля из data\profile-log.jsonl; None — файла нет.
    pub history: Option<Vec<Map<String, Value>>>,
    /// Сколько байт срезов прочитано и разобрано этим проходом и за сколько: цена статистики.
    pub parsed_bytes: u64,
    pub parsed_rows: u64,
    pub parse_time: Duration,
    /// Что не прочиталось (строка для владельца); None — всё прочиталось.
    pub trouble: Option<String>,
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
}

#[derive(Debug, Default)]
struct Worker {
    dir: PathBuf,
    files: BTreeMap<String, SampleFile>,
    scratch: Scratch,
    auto_tail: Tail,
    auto: Vec<AutoEntry>,
    history_tail: Tail,
    history: Option<Vec<Map<String, Value>>>,
}

/// Запускает поток статистики над папкой данных `dir`. `deliver` получает сводку, когда она изменилась (первую —
/// сразу после первого прохода). Отправитель — чтобы попросить проход сейчас; когда он брошен, поток кончается.
pub fn start(dir: PathBuf, deliver: impl Fn(Arc<Digest>) + Send + 'static) -> mpsc::Sender<()> {
    let (wake, woken) = mpsc::channel::<()>();
    let mut worker = Worker {
        dir,
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
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("поток статистики");
    wake
}

impl Worker {
    /// Один проход; Some — что-то изменилось (или это первый проход).
    fn scan(&mut self, first: bool) -> Option<Digest> {
        let started = Instant::now();
        let mut changed = first;
        let mut trouble = None;
        let mut parsed_bytes = 0;
        let mut parsed_rows = 0;

        // Срезы: файлы дня (`day_of`) не старше DAYS дней.
        let cutoff = samples::date_before(SystemTime::now(), DAYS);
        let names = match std::fs::read_dir(self.dir.join("samples")) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| day_of(n).is_some_and(|d| d >= cutoff.as_str()))
                .collect::<Vec<_>>(),
            // Папки срезов нет — программа входа ещё ничего не писала; это не ошибка.
            Err(_) => Vec::new(),
        };
        let before = self.files.len();
        self.files.retain(|name, _| names.contains(name));
        changed |= self.files.len() != before;
        for name in names {
            let path = self.dir.join("samples").join(&name);
            let file = self.files.entry(name).or_default();
            match read_samples(&path, file, &mut self.scratch) {
                Ok(Some((bytes, rows))) => {
                    changed = true;
                    parsed_bytes += bytes;
                    parsed_rows += rows;
                }
                Ok(None) => {}
                Err(e) => trouble = Some(format!("срезы {} не прочитаны: {e}", path.display())),
            }
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

        // Журнал смен профиля (пишет программа входа); нет файла — нет истории.
        match samples::read_new(&self.dir.join("profile-log.jsonl"), &mut self.history_tail) {
            Ok(Fresh::Same) => {}
            Ok(Fresh::More(text)) => {
                samples::history_entries(&text, self.history.get_or_insert_with(Vec::new));
                changed = true;
            }
            Ok(Fresh::Again(text)) => {
                let history = self.history.get_or_insert_with(Vec::new);
                history.clear();
                samples::history_entries(&text, history);
                changed = true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                changed |= self.history.take().is_some();
            }
            Err(e) => trouble = Some(format!("журнал профилей не прочитан: {e}")),
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
            files: self.files.len(),
            // Копия журналов — на каждое изменение, не на каждый проход: действий автомодуля единицы в час.
            auto: self.auto.clone(),
            history: self.history.clone(),
            parsed_bytes,
            parsed_rows,
            parse_time: started.elapsed(),
            trouble,
        })
    }
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

/// Дочитывает файл срезов. Some((байт, строк)) — прочитано новое; None — файл не менялся.
fn read_samples(path: &Path, file: &mut SampleFile, scratch: &mut Scratch) -> std::io::Result<Option<(u64, u64)>> {
    let text = match samples::read_new(path, &mut file.tail)? {
        Fresh::Same => return Ok(None),
        Fresh::More(text) => text,
        Fresh::Again(text) => {
            file.headed = false;
            file.columns = None;
            file.tally = Tally::default();
            file.previous = None;
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
        body = rest;
    }
    let rows_before = file.tally.rows;
    if let Some(columns) = &file.columns {
        scratch.count(columns, body, &mut file.previous, &mut file.tally);
    }
    Ok(Some((text.len() as u64, file.tally.rows - rows_before)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
