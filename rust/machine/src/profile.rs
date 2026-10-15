// Профили потолков и правила их постановки при запуске (как Profile, ProfileSets и LimitsStartup.Named в C#).
//
// Профиль запуска (data\profile.json) — тот, что сохранён или принят последним; при входе в Windows он ставится
// заново, но не раньше, чем по журналу Windows проверено, что прошлое применение не кончилось сбоем. Профиль с
// потолком выше заводского проходит испытательный срок («пробный»): столько-то перезапусков и часов без сбоя.
// Сбой после применения рискованного профиля отключает его; безопасный ставится и после сбоя, с пометкой.
// Сохранённые наборы по названиям (data\profiles.json) пишут прежнее окно на C# и программа входа (команды окна на
// Rust, DECISIONS #32 шаг 2) — в одном и том же виде (`Store::sets_text`).
//
// Отличие от C# (DECISIONS #31, выбор владельца 2): часы пробного профиля считаются до штатного завершения работы
// ИЛИ до записи Windows о быстром выключении (Kernel-Power 42) — при быстром запуске событие 6006 не пишется, и
// часы набирались только после полного выключения (измерено: 10 из 23 загрузок за 20 дней). Сбой же относится к
// профилю, как и прежде, до первого 6006: событие 42 пишет и гибернация, а сбой после пробуждения из неё должен
// идти в счёт профиля. C#-часть меняется так же, чтобы счёт не расходился.

use std::io;
use std::path::{Path, PathBuf};

use crate::Lp;
use crate::clock::{Pattern, Stamp};
use crate::events::{Event, EventLog};
use crate::files::{self, Spoiled, number, quote, read_ints};
use crate::limits::Limits;
use crate::mailbox::Cpu;
use crate::topology::Topology;

pub const TRIAL: &str = "trial";
pub const TRUSTED: &str = "trusted";
pub const FAILED: &str = "failed";
const TRIAL_BOOTS: i32 = 1;
const TRIAL_HOURS: f64 = 3.0;

/// Сбой системы: аварийное выключение или зависание, неожиданное выключение, синий экран, неисправимая аппаратная
/// ошибка и исправленная ошибка самого процессора (событие 19 — первый признак неустойчивого разгона). Событие 17
/// (исправленная ошибка шины PCI Express) не в счёт: на машине владельца оно пишется пачками при каждой загрузке.
pub const FAILURE_EVENTS: &str = "(Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=41) or \
     (Provider[@Name='EventLog'] and EventID=6008) or \
     (Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting'] and EventID=1001) or \
     (Provider[@Name='Microsoft-Windows-WHEA-Logger'] and (Level=1 or Level=2 or EventID=19))";
/// Штатное завершение работы.
pub const CLEAN_SHUTDOWN: &str = "(Provider[@Name='EventLog'] and EventID=6006)";
/// Конец отрезка, который идёт в часы пробного профиля: штатное завершение или уход в S4 (быстрое выключение,
/// гибернация).
pub const STRETCH_END: &str = "(Provider[@Name='EventLog'] and EventID=6006) or \
     (Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=42)";

/// Профиль запуска, как он лежит в data\profile.json.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub ratios: Vec<i32>,
    pub state: Option<String>,
    pub failure: Option<String>,
    pub saved: Stamp,
    pub applied: Stamp,
    pub boots: i32,
    pub hours: f64,
}

impl Profile {
    /// Отключён после сбоя: при запуске не ставится.
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.state.as_deref() == Some(FAILED)
    }
}

/// Сохранённый набор потолков.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Set {
    pub name: String,
    pub ratios: Vec<i32>,
}

/// Названия наборов сравниваются без учёта регистра и пробелов по краям (как `CurrentCultureIgnoreCase` и `Trim` в C#).
#[must_use]
pub fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Файлы профилей этой машины: папка данных, название процессора и номера логических процессоров. Файл другого
/// процессора (или другой раскладки) молча считается чужим — как в C#.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    cpu: String,
    lps: Vec<Lp>,
}

impl Store {
    #[must_use]
    pub fn new(dir: &Path, cpu: &str, lps: &[Lp]) -> Self {
        Self {
            dir: dir.to_path_buf(),
            cpu: cpu.to_string(),
            lps: lps.to_vec(),
        }
    }

    #[must_use]
    pub fn profile_path(&self) -> PathBuf {
        self.dir.join("profile.json")
    }

    #[must_use]
    pub fn sets_path(&self) -> PathBuf {
        self.dir.join("profiles.json")
    }

    #[must_use]
    pub fn marker_path(&self) -> PathBuf {
        self.dir.join("limits-held.json")
    }

    #[must_use]
    pub fn cpu(&self) -> &str {
        &self.cpu
    }

    #[must_use]
    pub fn lps(&self) -> &[Lp] {
        &self.lps
    }

    fn ours(&self, map: &serde_json::Map<String, serde_json::Value>) -> Option<bool> {
        let lps = read_ints(map.get("lps"))?;
        let ours: Vec<i64> = self.lps.iter().map(|lp| i64::from(lp.0)).collect();
        Some(map.get("cpu").and_then(|v| v.as_str()) == Some(self.cpu.as_str()) && lps == ours)
    }

    /// Профиль запуска. Ok(None) — его нет или он от другого процессора; Err — файл не читается (он сохранён .bad).
    pub fn read_profile(&self) -> Result<Option<Profile>, Spoiled> {
        let path = self.profile_path();
        let Some(map) = files::read_object(&path)? else {
            return Ok(None);
        };
        let fail = |what: &str| files::spoiled(&path, what);
        if !map.contains_key("cpu") {
            return Err(fail("нет поля cpu"));
        }
        if !self.ours(&map).ok_or_else(|| fail("нет поля lps"))? {
            return Ok(None);
        }
        let ratios: Vec<i32> = read_ints(map.get("ratios"))
            .ok_or_else(|| fail("нет поля ratios"))?
            .into_iter()
            .map(|r| i32::try_from(r).unwrap_or(0))
            .collect();
        if ratios.len() != self.lps.len() {
            return Err(fail("потолков в профиле не столько, сколько ядер"));
        }
        let stamp = |key: &str| {
            map.get(key)
                .and_then(|v| v.as_str())
                .and_then(Stamp::parse_roundtrip)
                .ok_or_else(|| fail(&format!("нет даты {key}")))
        };
        let text = |key: &str| -> Result<Option<String>, Spoiled> {
            let value = map.get(key).ok_or_else(|| fail(&format!("нет поля {key}")))?;
            Ok(value.as_str().map(str::to_string))
        };
        let boots = map.get("boots").ok_or_else(|| fail("нет поля boots"))?;
        let hours = map.get("hours").ok_or_else(|| fail("нет поля hours"))?;
        Ok(Some(Profile {
            name: match map.get("name") {
                Some(v) => v.as_str().unwrap_or_default().to_string(),
                None => "Профиль".into(),
            },
            ratios,
            state: text("state")?,
            failure: text("failure")?,
            saved: stamp("saved")?,
            applied: stamp("applied")?,
            boots: if boots.is_null() {
                0
            } else {
                boots
                    .as_i64()
                    .and_then(|b| i32::try_from(b).ok())
                    .ok_or_else(|| fail("boots не число"))?
            },
            hours: if hours.is_null() {
                f64::NAN
            } else {
                hours.as_f64().ok_or_else(|| fail("hours не число"))?
            },
        }))
    }

    /// Текст profile.json — тот же, что пишет C# (поля в том же порядке, те же разделители).
    #[must_use]
    pub fn profile_text(&self, p: &Profile) -> String {
        let lps: Vec<u32> = self.lps.iter().map(|lp| lp.0).collect();
        let opt = |v: &Option<String>| v.as_deref().map_or_else(|| "null".to_string(), quote);
        format!(
            "{{\"cpu\": {}, \"name\": {}, \"lps\": {}, \"ratios\": {}, \"state\": {}, \"saved\": {}, \"applied\": {}, \
             \"boots\": {}, \"hours\": {}, \"failure\": {}}}",
            quote(&self.cpu),
            quote(&p.name),
            files::ints(&lps),
            files::ints(&p.ratios),
            opt(&p.state),
            quote(&p.saved.roundtrip()),
            quote(&p.applied.roundtrip()),
            p.boots,
            number(p.hours),
            opt(&p.failure),
        )
    }

    pub fn write_profile(&self, p: &Profile) -> io::Result<()> {
        files::write_replacing(&self.profile_path(), &self.profile_text(p))
    }

    /// «Заводские»: профиля запуска больше нет (с ним уходит и его .bak — в C# он оставался, B16 описи).
    pub fn forget_profile(&self) -> io::Result<()> {
        files::delete_with_backup(&self.profile_path())
    }

    /// Сохранённые наборы в порядке файла (C# пишет их по алфавиту без учёта регистра). Ok(пусто) — файла нет или
    /// он от другого процессора; Err — не читается. Наборы берутся только все разом: половина файла хуже пустого.
    pub fn read_sets(&self) -> Result<Vec<Set>, Spoiled> {
        let path = self.sets_path();
        let Some(map) = files::read_object(&path)? else {
            return Ok(Vec::new());
        };
        let fail = |what: &str| files::spoiled(&path, what);
        if !map.contains_key("cpu") {
            return Err(fail("нет поля cpu"));
        }
        if !self.ours(&map).ok_or_else(|| fail("нет поля lps"))? {
            return Ok(Vec::new());
        }
        let sets = map
            .get("sets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| fail("нет поля sets"))?;
        let mut read = Vec::with_capacity(sets.len());
        for set in sets {
            let name = set
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| fail("у набора нет названия"))?;
            let ratios: Vec<i32> = read_ints(set.get("ratios"))
                .ok_or_else(|| fail("у набора нет потолков"))?
                .into_iter()
                .map(|r| i32::try_from(r).unwrap_or(0))
                .collect();
            if ratios.len() != self.lps.len() {
                return Err(fail("потолков в наборе не столько, сколько ядер"));
            }
            read.push(Set {
                name: name.to_string(),
                ratios,
            });
        }
        Ok(read)
    }
}

/// Название события для владельца (как в Profile.FirstEvent).
fn failure_name(event: &Event) -> &'static str {
    match event.provider.as_str() {
        "Microsoft-Windows-Kernel-Power" => "аварийное выключение или зависание",
        "EventLog" if event.id == 6008 => "неожиданное выключение",
        "EventLog" => "завершение работы",
        "Microsoft-Windows-WER-SystemErrorReporting" => "синий экран",
        _ if event.id == 19 => "исправленная ошибка процессора",
        _ => "неисправимая аппаратная ошибка",
    }
}

/// Итог постановки профиля при запуске.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    /// Профиль запуска после проверки (с обновлёнными счётчиками); None — его нет.
    pub profile: Option<Profile>,
    /// Что пошло не так: не применён, применён не полностью, применён после сбоя; None — всё штатно.
    pub note: Option<String>,
    /// Во сколько профиль поставлен при этом запуске («HH:mm»); None — не ставился.
    pub at_startup: Option<String>,
}

/// Запуск: читает профиль и, если потолки с прошлого раза сбросились (перезагрузка, другая программа), ставит его
/// заново — кроме случая, когда прошлое применение кончилось сбоем (Profile.Startup в C#). `now` — часы
/// (в проверках — подставные). Err — файл профиля не читается.
pub fn startup(
    store: &Store,
    limits: &mut Limits,
    cpu: &mut dyn Cpu,
    topology: &Topology,
    log: &dyn EventLog,
    now: &dyn Fn() -> Stamp,
) -> Result<Startup, Spoiled> {
    let Some(mut p) = store.read_profile()? else {
        return Ok(Startup {
            profile: None,
            note: None,
            at_startup: None,
        });
    };
    if p.disabled() || limits.same(&p.ratios) {
        return Ok(Startup {
            profile: Some(p),
            note: None,
            at_startup: None,
        });
    }
    let risky = limits.above_factory(&p.ratios);
    let mut note = None;
    let mut stretch_end = None;
    let checked = (|| -> Result<Option<Event>, String> {
        let failure = log.first(p.applied, FAILURE_EVENTS)?;
        let clean = log.first(p.applied, CLEAN_SHUTDOWN)?;
        stretch_end = log.first(p.applied, STRETCH_END)?.map(|e| e.at);
        // Сбой относится к профилю, только если случился в том же сеансе Windows, где профиль применили: до первого
        // штатного завершения работы.
        Ok(failure.filter(|f| clean.as_ref().is_none_or(|c| c.at >= f.at)))
    })();
    match checked {
        Ok(Some(failure)) => {
            let what = format!("{} {}", failure_name(&failure), failure.at.local(Pattern::DayTime));
            if risky {
                p.state = Some(FAILED.into());
                p.failure = Some(format!(
                    "{what} — после применения профиля {}",
                    p.applied.local(Pattern::DayTime)
                ));
                let note = store
                    .write_profile(&p)
                    .err()
                    .map(|e| format!("профиль отключён после сбоя, но файл профиля не записан: {e}"));
                return Ok(Startup {
                    profile: Some(p),
                    note,
                    at_startup: None,
                });
            }
            note = Some(format!(
                "после прошлого применения был сбой ({what}); потолки в профиле не выше заводских, поэтому он применён"
            ));
        }
        Ok(None) => {}
        Err(cause) => {
            if risky {
                let note = format!(
                    "профиль не применён: не удалось прочитать журнал Windows и убедиться, что сбоя не было ({cause})"
                );
                return Ok(Startup {
                    profile: Some(p),
                    note: Some(note),
                    at_startup: None,
                });
            }
        }
    }
    if p.state.as_deref() == Some(TRIAL) {
        p.boots += 1;
        if let Some(end) = stretch_end {
            p.hours += end.hours_since(p.applied).max(0.0);
        }
        if p.boots >= TRIAL_BOOTS && p.hours >= TRIAL_HOURS {
            p.state = Some(TRUSTED.into());
        }
    }
    // Отметка времени пишется до записи в процессор: если машина зависнет сразу после неё, следующий запуск отнесёт
    // сбой к этому применению, а не к прошлому. Не записалась — профиль не ставится.
    p.applied = now();
    if let Err(e) = store.write_profile(&p) {
        let note = format!("профиль не применён: не записан файл профиля ({e})");
        return Ok(Startup {
            profile: Some(p),
            note: Some(note),
            at_startup: None,
        });
    }
    let mut at_startup = None;
    match limits.apply(cpu, topology, &p.ratios) {
        Some(refusal) => note = Some(format!("профиль применён не полностью: {refusal}")),
        None => at_startup = Some(now().local(Pattern::Time)),
    }
    if let Err(e) = store.write_profile(&p) {
        note = Some(format!("профиль поставлен, но файл профиля не записан: {e}"));
    }
    Ok(Startup {
        profile: Some(p),
        note,
        at_startup,
    })
}

/// Делает то, что стоит в процессоре сейчас, профилем запуска с данным названием (Profile.Save в C#):
/// новый испытательный срок, если в нём есть потолок выше заводского.
pub fn save(store: &Store, limits: &Limits, name: &str, now: Stamp) -> io::Result<Profile> {
    let p = Profile {
        name: name.to_string(),
        ratios: limits.current().to_vec(),
        state: Some(
            if limits.above_factory(limits.current()) {
                TRIAL
            } else {
                TRUSTED
            }
            .into(),
        ),
        failure: None,
        saved: now,
        applied: now,
        boots: 0,
        hours: 0.0,
    };
    store.write_profile(&p)?;
    Ok(p)
}

/// Каким сохранённым набором являются потолки, стоящие в процессоре: Some(название), Some("") — заводские, None — ни
/// одним (ставились вручную, ползунком или измерением). См. `named`.
#[must_use]
pub fn standing(limits: &Limits, startup: Option<&Profile>, sets: &[Set]) -> Option<String> {
    named(limits, limits.current(), startup, sets, None)
}

/// Каким сохранённым набором являются потолки `ratios`: Some(название), Some("") — заводские, None — ни одним.
/// Ядра, о которых процессор не ответил, не в счёт. Первым идёт профиль запуска, затем `preferred` — набор, который
/// владелец открыл или сохранил последним (одни и те же потолки бывают записаны под несколькими названиями), затем
/// наборы по порядку (LimitsStartup.Named в C#). `sets` уже включает профиль запуска.
#[must_use]
pub fn named(
    limits: &Limits,
    ratios: &[i32],
    startup: Option<&Profile>,
    sets: &[Set],
    preferred: Option<&str>,
) -> Option<String> {
    let same = |other: &[i32]| (0..ratios.len()).all(|i| !limits.known(i) || other.get(i) == ratios.get(i));
    match startup {
        None if same(limits.fused()) => return Some(String::new()),
        Some(p) if sets.iter().any(|s| same_name(&s.name, &p.name)) && same(&p.ratios) => {
            return Some(p.name.trim().to_string());
        }
        _ => {}
    }
    if let Some(set) = preferred.and_then(|name| sets.iter().find(|s| same_name(&s.name, name)))
        && same(&set.ratios)
    {
        return Some(set.name.trim().to_string());
    }
    if let Some(set) = sets.iter().find(|s| same(&s.ratios)) {
        return Some(set.name.clone());
    }
    same(limits.fused()).then(String::new)
}

/// Кладёт набор в список (ProfileSets.Put в C#): набор с тем же названием (без учёта регистра и пробелов по краям)
/// получает новые потолки и сохраняет своё прежнее написание, как ключ в словаре C# без учёта регистра.
pub fn put_set(sets: &mut Vec<Set>, name: &str, ratios: &[i32]) {
    if let Some(set) = sets.iter_mut().find(|s| same_name(&s.name, name)) {
        set.ratios = ratios.to_vec();
    } else {
        sets.push(Set {
            name: name.trim().to_string(),
            ratios: ratios.to_vec(),
        });
    }
    sort_sets(sets);
}

/// Убирает набор с таким названием. false — такого нет.
pub fn remove_set(sets: &mut Vec<Set>, name: &str) -> bool {
    let before = sets.len();
    sets.retain(|s| !same_name(&s.name, name));
    sets.len() != before
}

/// Порядок наборов в файле — по алфавиту без учёта регистра, как их держит C# (`SortedDictionary` с
/// `CurrentCultureIgnoreCase`). Здесь сравниваются строчные буквы по номерам знаков: у латиницы, кириллицы и цифр
/// порядок тот же, расходится только у знаков препинания и буквы «ё» — это меняет лишь порядок строк в меню, C# при
/// чтении сортирует сам.
fn sort_sets(sets: &mut [Set]) {
    sets.sort_by_cached_key(|s| s.name.to_lowercase());
}

impl Store {
    /// Текст profiles.json — тот же, что пишет C# (ProfileSets.Write через Json.WriteFile): процессор, раскладка,
    /// наборы в порядке списка.
    #[must_use]
    pub fn sets_text(&self, sets: &[Set]) -> String {
        let lps: Vec<u32> = self.lps.iter().map(|lp| lp.0).collect();
        let listed: Vec<String> = sets
            .iter()
            .map(|s| {
                format!(
                    "{{\"name\": {}, \"ratios\": {}}}",
                    quote(&s.name),
                    files::ints(&s.ratios)
                )
            })
            .collect();
        format!(
            "{{\"cpu\": {}, \"lps\": {}, \"sets\": [{}]}}",
            quote(&self.cpu),
            files::ints(&lps),
            listed.join(", ")
        )
    }

    /// Записывает profiles.json (через .tmp и .bak, как C#). Пишет его программа входа по командам окна и прежнее
    /// окно на C#; одновременно они не пишут: пока работает C#-часть, программа входа команды профилей отклоняет.
    pub fn write_sets(&self, sets: &[Set]) -> io::Result<()> {
        files::write_replacing(&self.sets_path(), &self.sets_text(sets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(name: &str, ratios: &[i32]) -> Set {
        Set {
            name: name.into(),
            ratios: ratios.to_vec(),
        }
    }

    #[test]
    fn sets_text_is_the_csharp_writer_shape() {
        // Shape copied from a profiles.json written by the C# window on the owner's machine (15.10).
        let store = Store::new(Path::new("."), "Intel(R) Core(TM) Ultra 9 275HX", &[Lp(0), Lp(1)]);
        let text = store.sets_text(&[set("74optima", &[48, 48]), set("csmart", &[47, 45])]);
        assert_eq!(
            text,
            "{\"cpu\": \"Intel(R) Core(TM) Ultra 9 275HX\", \"lps\": [0, 1], \"sets\": [{\"name\": \"74optima\", \
             \"ratios\": [48, 48]}, {\"name\": \"csmart\", \"ratios\": [47, 45]}]}"
        );
        assert_eq!(
            store.sets_text(&[]),
            "{\"cpu\": \"Intel(R) Core(TM) Ultra 9 275HX\", \"lps\": [0, 1], \"sets\": []}"
        );
    }

    #[test]
    fn put_keeps_the_stored_spelling_and_the_alphabetical_order() {
        let mut sets = vec![set("csmart", &[1]), set("Жара", &[2])];
        put_set(&mut sets, "  CSMART ", &[9]);
        put_set(&mut sets, "74optima", &[3]);
        put_set(&mut sets, "bench", &[4]);
        let names: Vec<&str> = sets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["74optima", "bench", "csmart", "Жара"]);
        assert_eq!(sets[2].ratios, [9], "the same name, any case, replaces the ratios");
        assert!(remove_set(&mut sets, "жара"));
        assert!(!remove_set(&mut sets, "жара"), "nothing left to remove");
        assert_eq!(sets.len(), 3);
    }
}
