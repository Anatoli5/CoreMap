// События (бриф W, часть 5): что случилось с потолками и максимумами — одним списком под графиком (новые первыми) и
// отметками на графиках. Источник — data\events\<ГГГГ-ММ-ДД>.jsonl, файл на день (пишет CoreMapTray с 0.5.6): отсечки
// и возвраты автомодуля, применённый профиль, потолок, изменённый на плитке или чужой программой, сброс ▲ и ø, возврат
// потолков профиля (выключен автомодуль, сменены настройки, сон, выход, прежняя программа), пробуждение. За дни до
// него — прежний журнал автомодуля data\auto-log.jsonl (lower, raise, back до 0.5.6). Файлы читаются терпимо: строка,
// которая не разобралась, пропускается, файла может не быть; событие незнакомого вида показывается своим именем.
//
// Здесь только разбор и слова; читает файлы store.rs, показывают chart.rs (отметки на линиях по множителю), timeline.rs
// (отметки по времени) и view.rs (список).
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::Value;

use crate::samples::{row_time, short_time};

/// Строка файла событий. Поля сверх общих — `period` (итог периода потолка, который блок покидает: потолок, секунды, ▲ и
/// когда, ø), `conditions` (что было в момент события; неизвестное программа не пишет) и `reason` (почему возвращены
/// потолки профиля) — читаются как есть, их нет — ничего.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct EventEntry {
    #[serde(default)]
    pub time: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
    #[serde(default)]
    pub period: Option<Value>,
    #[serde(default)]
    pub conditions: Option<Value>,
    /// Название профиля у «profile», если программа его пишет.
    #[serde(default)]
    pub name: Option<String>,
    /// Почему возвращены потолки профиля («putback»): auto-off, settings, sleep, exit, csharp.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Строка `data\auto-log.jsonl` (пишет `rust\tray\src\app\auto.rs`). Прежние действия «lower», «raise», «back» и
/// действия 0.5.6 «emergency», «target», «return» до появления events.jsonl. Поля, которых окно не показывает,
/// пропускаются.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct AutoEntry {
    #[serde(default)]
    pub time: Option<String>,
    pub act: String,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
    #[serde(default)]
    pub mean: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub cool: Option<u64>,
    #[serde(default)]
    pub ban: Option<i64>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub strikes: Option<u32>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    /// Что сработало: одно показание (аварийная) или 2-с медиана (отсечка и возврат), °C.
    #[serde(default)]
    pub reading: Option<f64>,
    #[serde(default)]
    pub median: Option<f64>,
    #[serde(default)]
    pub limit: Option<f64>,
    #[serde(default)]
    pub emergency: Option<f64>,
    #[serde(default)]
    pub die: Option<f64>,
    #[serde(default)]
    pub ratio: Option<f64>,
    #[serde(default)]
    pub busy: Option<f64>,
    #[serde(default)]
    pub gpu: Option<Value>,
    #[serde(default)]
    pub fans: Option<Value>,
}

/// Строки JSON по одной в строке — в `into`; что не разобралось, пропускается.
pub fn read_lines<T: serde::de::DeserializeOwned>(text: &str, into: &mut Vec<T>) {
    into.extend(text.lines().filter_map(|l| serde_json::from_str::<T>(l).ok()));
}

/// Вид события.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Аварийная отсечка (одно показание).
    Emergency,
    /// Отсечка по цели (2-с медиана).
    Target,
    /// Возврат к профилю.
    Return,
    /// Применён профиль.
    Profile,
    /// Потолок изменён на плитке.
    Edit,
    /// ▲ и ø сброшены.
    Reset,
    /// Потолок изменён другой программой — принят как есть.
    Foreign,
    /// Потолки профиля возвращены (`reason`).
    Putback,
    /// Пробуждение после сна.
    Wake,
    /// Вид, которого окно не знает: показывается его имя.
    Other,
    /// Прежний автомодуль (до 0.5.6): снижение, подъём, возврат принятых.
    Lower,
    Raise,
    Back,
}

impl Kind {
    fn of(word: &str) -> Option<Kind> {
        Some(match word {
            "emergency" => Kind::Emergency,
            "target" => Kind::Target,
            "return" => Kind::Return,
            "profile" => Kind::Profile,
            "edit" => Kind::Edit,
            "reset" => Kind::Reset,
            "foreign" => Kind::Foreign,
            "putback" => Kind::Putback,
            "wake" => Kind::Wake,
            "lower" => Kind::Lower,
            "raise" => Kind::Raise,
            "back" => Kind::Back,
            _ => return None,
        })
    }

    /// Опускает ли потолок автомодуль (отсечки и прежнее снижение).
    #[must_use]
    pub fn cuts(self) -> bool {
        matches!(self, Kind::Emergency | Kind::Target | Kind::Lower)
    }

    /// Цвет отметки: отсечки — красная (аварийная, прежнее снижение) и янтарная (по цели), возвраты — бирюзовая,
    /// действия владельца — светлая, чужая программа — сиреневая, остальное — серая.
    #[must_use]
    pub fn color(self) -> u32 {
        match self {
            Kind::Emergency | Kind::Lower => 0x00ff_5d5d,
            Kind::Target => 0x00ff_b454,
            Kind::Return | Kind::Raise | Kind::Back | Kind::Putback => 0x006f_d0c8,
            Kind::Profile | Kind::Edit | Kind::Reset => 0x00c8_ccd4,
            Kind::Foreign => 0x00b8_8cff,
            Kind::Wake | Kind::Other => 0x0079_808c,
        }
    }
}

/// Событие для показа.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Время, мс по местным часам (как у срезов, `samples::row_time`); None — не разобралось.
    pub at: Option<i64>,
    /// «16.10 14:03».
    pub time: String,
    /// День («ГГГГ-ММ-ДД») — щелчок по строке списка открывает его.
    pub date: Option<String>,
    pub kind: Kind,
    /// Блок (название, как в знакомстве); None — событие всех блоков (сброс).
    pub block: Option<String>,
    /// «×46 → ×45».
    pub change: String,
    /// Что это и почему — коротко.
    pub why: String,
    /// Подробности: условия момента, итог периода потолка.
    pub details: String,
    /// Множитель, на котором оно случилось: фактический из условий, иначе потолок, с которого.
    pub ratio: Option<f64>,
}

/// Время строки журнала («2026-10-15T01:25:08.1234567+03:00» — местное время с поясом) → мс по местным часам, как у
/// срезов (`row_time`); иначе None.
#[must_use]
pub fn entry_ms(text: &str) -> Option<i64> {
    let base = text.get(..19)?.replacen('T', " ", 1);
    let rest = text.get(19..).unwrap_or("");
    let fraction: String = rest
        .strip_prefix('.')
        .map(|f| f.chars().take_while(char::is_ascii_digit).take(9).collect())
        .unwrap_or_default();
    if fraction.is_empty() {
        row_time(&base)
    } else {
        row_time(&format!("{base}.{fraction}"))
    }
    .map(|t| t.ms)
}

/// Число из объекта JSON по ключу.
fn number(value: Option<&Value>, key: &str) -> Option<f64> {
    value?.get(key)?.as_f64()
}

/// Дата строки журнала («2026-10-16T14:03:07…» → «2026-10-16»); не дата — None.
fn date_of(time: &str) -> Option<String> {
    let d = time.get(..10)?;
    let b = d.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    (digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10)).then(|| d.to_string())
}

/// Температуры момента у изменения владельца и 2-с медианы: «температура 81 °C · 2-с медианы 80.1 / 79.5 °C».
fn temps(c: Option<&Value>) -> Vec<String> {
    let list = |key: &str, digits: usize| -> Option<String> {
        let Some(Value::Array(v)) = c?.get(key) else {
            return None;
        };
        let parts: Vec<String> = v
            .iter()
            .filter_map(Value::as_f64)
            .map(|t| format!("{t:.digits$}"))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" / "))
    };
    let mut out = Vec::new();
    if let Some(t) = number(c, "temp") {
        out.push(format!("температура {t:.0} °C"));
    } else if let Some(t) = list("temps", 0) {
        out.push(format!("температуры {t} °C"));
    }
    if let Some(m) = list("medians", 1) {
        out.push(format!("2-с медианы {m} °C"));
    }
    out
}

/// Почему возвращены потолки профиля — словами.
fn reason(word: Option<&str>) -> String {
    match word {
        Some("auto-off") => "автомодуль выключен".into(),
        Some("settings") => "настройки автомодуля изменены".into(),
        Some("sleep") => "сон".into(),
        Some("exit") => "выход CoreMapTray".into(),
        Some("csharp") => "запущена прежняя программа (C#)".into(),
        Some(other) => other.into(),
        None => "причина не записана".into(),
    }
}

/// «множитель ×44.6 · нагрузка 91 % · кристалл 71.4 °C · видеокарта 70 °C, 120 Вт, 93 % · вент. 4100 / 4300 об/мин».
fn conditions(
    ratio: Option<f64>,
    busy: Option<f64>,
    die: Option<f64>,
    gpu: Option<&Value>,
    fans: Option<&Value>,
) -> String {
    let mut out = Vec::new();
    if let Some(r) = ratio {
        out.push(format!("множитель ×{r:.1}"));
    }
    if let Some(b) = busy {
        out.push(format!("нагрузка {:.0} %", b * 100.0));
    }
    if let Some(d) = die {
        out.push(format!("кристалл {d:.1} °C"));
    }
    let gpu: Vec<String> = [
        number(gpu, "temp").map(|t| format!("{t:.0} °C")),
        number(gpu, "power").map(|p| format!("{p:.0} Вт")),
        number(gpu, "busy").map(|b| format!("{:.0} %", b * 100.0)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !gpu.is_empty() {
        out.push(format!("видеокарта {}", gpu.join(", ")));
    }
    if let Some(Value::Array(list)) = fans {
        let rpm: Vec<String> = list
            .iter()
            .filter_map(Value::as_f64)
            .map(|v| format!("{v:.0}"))
            .collect();
        if !rpm.is_empty() {
            out.push(format!("вент. {} об/мин", rpm.join(" / ")));
        }
    }
    out.join(" · ")
}

/// «×46 → ×45».
fn change(from: Option<i64>, to: Option<i64>) -> String {
    match (from, to) {
        (Some(f), Some(t)) => format!("×{f} → ×{t}"),
        (_, Some(t)) => format!("→ ×{t}"),
        _ => String::new(),
    }
}

#[expect(clippy::cast_precision_loss, reason = "multipliers are a few dozen")]
fn real(v: i64) -> f64 {
    v as f64
}

/// Событие из строки файла событий; вид не знаком — показывается его имя.
fn from_event(e: &EventEntry) -> Event {
    let kind = Kind::of(&e.kind).unwrap_or(Kind::Other);
    let c = e.conditions.as_ref();
    let die = number(c, "die");
    let at_die = die.map(|d| format!("; кристалл {d:.1} °C")).unwrap_or_default();
    let why = match kind {
        Kind::Emergency => format!(
            "аварийная отсечка{}{at_die}",
            number(c, "reading")
                .map(|t| format!(": показание {t:.0} °C"))
                .unwrap_or_default()
        ),
        Kind::Target => format!(
            "отсечка: 2-с медиана{} и не падает{at_die}",
            number(c, "median").map(|t| format!(" {t:.1} °C")).unwrap_or_default()
        ),
        Kind::Return => format!("возврат: паузу 2-с медианы не выше цели{at_die}"),
        Kind::Profile => format!(
            "применён профиль{}",
            e.name.as_deref().map(|n| format!(" «{n}»")).unwrap_or_default()
        ),
        Kind::Edit => "потолок изменён на плитке".to_string(),
        Kind::Reset => "▲ и ø сброшены".to_string(),
        Kind::Foreign => "потолок изменён другой программой — принят как есть".to_string(),
        Kind::Putback => format!("потолки профиля возвращены: {}", reason(e.reason.as_deref())),
        Kind::Wake => "пробуждение после сна".to_string(),
        Kind::Other | Kind::Lower | Kind::Raise | Kind::Back => e.kind.clone(), // one word per event
    };
    let mut details = temps(c);
    let rest = conditions(
        number(c, "ratio"),
        number(c, "busy"),
        None,
        c.and_then(|c| c.get("gpu")),
        c.and_then(|c| c.get("fans")),
    );
    if !rest.is_empty() {
        details.push(rest);
    }
    let mut details = details.join(" · ");
    if let Some(p) = e.period.as_ref() {
        let ceiling = number(Some(p), "ceiling");
        let mut period = ceiling.map_or_else(|| "период потолка:".to_string(), |c| format!("при ×{c:.0}:"));
        if let Some(m) = number(Some(p), "max") {
            let _ = write!(period, " ▲ {m:.0} °C"); // into a String
            if let Some(t) = p.get("maxTime").and_then(Value::as_str) {
                let _ = write!(period, " ({})", short_time(t)); // into a String
            }
        }
        if let Some(m) = number(Some(p), "mean") {
            let _ = write!(period, ", ø {m:.1} °C"); // into a String
        }
        if let Some(s) = number(Some(p), "secs") {
            let _ = write!(period, ", {:.0} мин", s / 60.0); // into a String
        }
        if !details.is_empty() {
            details.push_str(" · ");
        }
        details.push_str(&period);
    }
    Event {
        at: e.time.as_deref().and_then(entry_ms),
        time: e.time.as_deref().map(short_time).unwrap_or_default(),
        date: e.time.as_deref().and_then(date_of),
        kind,
        block: e.block.clone(), // one name per event
        change: change(e.from, e.to),
        why,
        details,
        ratio: number(c, "ratio").or_else(|| e.from.map(real)),
    }
}

/// События из строки журнала автомодуля (возврат принятых — по событию на блок).
fn from_auto(e: &AutoEntry, into: &mut Vec<Event>) {
    let Some(kind) = Kind::of(&e.act) else { return };
    let at = e.time.as_deref().and_then(entry_ms);
    let time = e.time.as_deref().map(short_time).unwrap_or_default();
    let date = e.time.as_deref().and_then(date_of);
    if kind == Kind::Back {
        for item in &e.blocks {
            let Some((name, ceiling)) = item.rsplit_once(" ×") else {
                continue;
            };
            into.push(Event {
                at,
                time: time.clone(), // a few blocks per line
                date: date.clone(),
                kind,
                block: Some(name.to_string()),
                change: format!("→ ×{ceiling}"),
                why: format!("принятый возвращён: {}", e.why.as_deref().unwrap_or("")),
                details: String::new(),
                ratio: None,
            });
        }
        return;
    }
    let die = e.die.map(|d| format!("; кристалл {d:.1} °C")).unwrap_or_default();
    let why = match kind {
        Kind::Emergency => format!(
            "аварийная отсечка: показание{}{}{die}",
            e.reading.map(|r| format!(" {r:.0} °C")).unwrap_or_default(),
            e.emergency.map(|l| format!(" ≥ {l:.0} °C")).unwrap_or_default()
        ),
        Kind::Target => format!(
            "отсечка: 2-с медиана{}{} и не падает{die}",
            e.median.map(|m| format!(" {m:.1} °C")).unwrap_or_default(),
            e.limit.map(|l| format!(" > {l:.0} °C")).unwrap_or_default()
        ),
        Kind::Return => format!(
            "возврат: паузу 2-с медианы{}{die}",
            e.limit.map(|l| format!(" не выше {l:.0} °C")).unwrap_or_default()
        ),
        Kind::Lower => {
            let mut why = match (e.mean, e.max) {
                (Some(m), Some(x)) => format!("перегрев: ø {m:.0} °C, ▲ {x:.0} °C"),
                (Some(m), None) => format!("перегрев: ø {m:.0} °C"),
                _ => "перегрев".to_string(),
            };
            if let Some(ban) = e.ban {
                let until = e.until.as_deref().map(short_time).unwrap_or_default();
                let strikes = e.strikes.map(|s| format!(", {s}-й раз")).unwrap_or_default();
                let _ = write!(why, "; ×{ban} не раньше {until}{strikes}"); // into a String
            }
            why
        }
        _ => e.cool.map_or_else(
            || "прохладен под нагрузкой".to_string(),
            |c| format!("прохладен под нагрузкой {c} с"),
        ),
    };
    into.push(Event {
        at,
        time,
        date,
        kind,
        block: e.block.clone(), // one name per line
        change: change(e.from, e.to),
        why,
        details: conditions(e.ratio, e.busy, None, e.gpu.as_ref(), e.fans.as_ref()),
        ratio: e.ratio.or_else(|| e.from.map(real)),
    });
}

/// Все события, новые первыми: файлы событий и строки журнала автомодуля раньше их первого события (за дни, когда их
/// ещё не было; файлов событий нет — весь журнал автомодуля).
#[must_use]
pub fn merged(events: &[EventEntry], auto: &[AutoEntry]) -> Vec<Event> {
    let mut out: Vec<Event> = events.iter().map(from_event).collect();
    let first = out.iter().filter_map(|e| e.at).min();
    let mut old = Vec::new();
    for e in auto {
        from_auto(e, &mut old);
    }
    out.extend(
        old.into_iter()
            .filter(|e| first.is_none_or(|f| e.at.is_some_and(|a| a < f))),
    );
    // Новые первыми; без времени — в конце, в порядке файла.
    out.sort_by(|a, b| b.at.unwrap_or(i64::MIN).cmp(&a.at.unwrap_or(i64::MIN)));
    out
}

/// Строки списка событий: номер события в `events` и чей это блок среди `names` (блоков сравнения). `all` — все
/// события; иначе — блоков сравнения и события без блока (сброс, пробуждение: они касаются всех). Блок — по названию
/// целиком («2» — не «2–5»).
#[must_use]
pub fn listed(events: &[Event], names: &[&str], all: bool) -> Vec<(usize, Option<usize>)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let which = e.block.as_deref().and_then(|b| names.iter().position(|n| *n == b));
            (all || which.is_some() || e.block.is_none()).then_some((i, which))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_old_auto_log_lines_merge_newest_first() {
        // Бриф W, часть 5: events.jsonl — общие поля, `period` и `conditions`; за дни до него — прежний журнал
        // автомодуля. Ловит: прежние строки за дни, когда events.jsonl уже пишется (дубли); строку неизвестного вида в
        // списке; неразобранный период потолка; порядок не по времени.
        let mut events = Vec::new();
        read_lines(
            &[
                r#"{"time":"2026-10-16T14:03:07.1234567+03:00","kind":"emergency","block":"0","from":46,"to":45,"period":{"ceiling":46,"secs":600,"max":96,"maxTime":"2026-10-16T14:03:07.0000000+03:00","mean":81.24},"conditions":{"reading":96.0,"die":71.4,"medians":[80.1],"ratio":44.62,"busy":0.91,"gpu":{"temp":70,"power":120.4,"busy":0.93},"fans":[4100,4300]}}"#,
                r#"{"time":"2026-10-16T14:05:00.0000000+03:00","kind":"reset"}"#,
                r#"{"time":"2026-10-16T14:06:00.0000000+03:00","kind":"profile","name":"csmart2","block":"10","from":44,"to":46}"#,
                r#"{"time":"2026-10-16T14:07:00.0000000+03:00","kind":"edit","block":"12","from":45,"to":44,"conditions":{"temp":81,"medians":[79.5,80.25]}}"#,
                r#"{"time":"2026-10-16T14:08:00.0000000+03:00","kind":"something new"}"#,
                r#"{"time":"2026-10-16T14:09:00.0000000+03:00","kind":"putback","reason":"sleep","block":"0","from":44,"to":46}"#,
                "не JSON",
            ]
            .join("\n"),
            &mut events,
        );
        assert_eq!(events.len(), 6, "строка не JSON пропущена");
        let mut auto = Vec::new();
        read_lines(
            &[
                r#"{"time":"2026-10-15T10:00:00.0000000+03:00","act":"lower","block":"0","from":50,"to":49,"mean":88.4,"max":93.0}"#,
                r#"{"time":"2026-10-15T11:00:00.0000000+03:00","act":"back","why":"выход","blocks":["0 ×50","2–5 ×46"]}"#,
                r#"{"time":"2026-10-16T14:10:00.0000000+03:00","act":"target","block":"0","from":45,"to":44,"median":86.5,"limit":85}"#,
            ]
            .join("\n"),
            &mut auto,
        );
        let all = merged(&events, &auto);
        let kinds: Vec<Kind> = all.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Putback,
                Kind::Other,
                Kind::Edit,
                Kind::Profile,
                Kind::Reset,
                Kind::Emergency,
                Kind::Back,
                Kind::Back,
                Kind::Lower
            ],
            "новые первыми; незнакомый вид — не пропущен; строка автомодуля после первого события — не дублем"
        );
        assert!(all[0].why.contains("потолки профиля возвращены: сон"), "{}", all[0].why);
        assert_eq!(all[1].why, "something new", "незнакомый вид — своим именем");
        assert!(
            all[2]
                .details
                .contains("температура 81 °C · 2-с медианы 79.5 / 80.2 °C"),
            "{}",
            all[2].details
        );
        assert_eq!(all[2].date.as_deref(), Some("2026-10-16"));
        let emergency = &all[5];
        assert_eq!((emergency.change.as_str(), emergency.ratio), ("×46 → ×45", Some(44.62)));
        assert!(
            emergency
                .why
                .contains("аварийная отсечка: показание 96 °C; кристалл 71.4 °C"),
            "{}",
            emergency.why
        );
        assert!(
            emergency.details.contains("видеокарта 70 °C, 120 Вт, 93 %")
                && emergency
                    .details
                    .contains("при ×46: ▲ 96 °C (16.10 14:03), ø 81.2 °C, 10 мин"),
            "{}",
            emergency.details
        );
        assert_eq!(emergency.at, row_time("2026-10-16 14:03:07.123").map(|t| t.ms));
        assert_eq!(all[4].block, None, "сброс — без блока");
        assert!(all[3].why.contains("«csmart2»"));
        assert_eq!((all[6].block.as_deref(), all[6].change.as_str()), (Some("0"), "→ ×50"));
        assert_eq!(all[6].date.as_deref(), Some("2026-10-15"));
        // Нет events.jsonl — весь журнал автомодуля, включая новые действия.
        let old = merged(&[], &auto);
        assert_eq!(old.len(), 4);
        assert!(
            old[0].why.contains("2-с медиана 86.5 °C > 85 °C и не падает"),
            "{}",
            old[0].why
        );
        assert!(Kind::Emergency.cuts() && Kind::Target.cuts() && !Kind::Return.cuts());
    }

    #[test]
    fn the_list_shows_every_event_or_those_of_compared_blocks() {
        // Часть 5: переключатель «все блоки» / «выбранные блоки». Выбранные — события блоков сравнения (название
        // целиком: «2» — не «2–5») и события без блока (сброс касается всех); у строки — чей это блок среди выбранных.
        let event = |block: Option<&str>| Event {
            at: None,
            time: String::new(),
            date: None,
            kind: Kind::Edit,
            block: block.map(str::to_string),
            change: String::new(),
            why: String::new(),
            details: String::new(),
            ratio: None,
        };
        let events = [event(Some("2–5")), event(Some("2")), event(None), event(Some("0"))];
        assert_eq!(
            listed(&events, &["0", "2–5"], false),
            [(0, Some(1)), (2, None), (3, Some(0))]
        );
        assert_eq!(listed(&events, &["0", "2–5"], true).len(), 4);
        assert_eq!(listed(&events, &[], false), [(2, None)]);
    }
}
