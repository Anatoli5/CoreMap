// История профилей: data\profile-log.jsonl — каждая постановка и смена потолков владельцем или по его профилю, строкой
// JSON (владелец 15.10: «нет ни истории, как я менял профиль»). Пишет только программа входа: она единственная, кто
// пишет профили по командам окна и меню. Файл только дописывается. Окно его больше не показывает (владелец 15.10:
// «История какая-то, а нахуй мне история там?», DECISIONS #38) — файл остаётся для разбора и для других частей.
//
// Строка: время («o», как в файлах профилей) и такты, что сделано (`act`), название набора (null — потолки не из
// набора, "" — заводские), потолки по ядрам до и после; у ручной правки — блок; у неполной постановки — причина.
// Что сделано: logon — профиль поставлен при входе в Windows; menu — набор выбран в меню значка; open — набор
// применён в окне; save, delete — команды окна; limit — потолок блока поставлен вручную; wake — после сна потолки
// поставлены заново; foreign — другая программа изменила потолки, изменение принято. Строки accept и revert писали
// сборки до 0.5.4 (команды «принять» и «откатить»).

use std::path::PathBuf;

use machine::clock::Stamp;
use machine::files;
use serde_json::{Map, Value, json};

#[derive(Debug)]
pub struct History {
    path: PathBuf,
}

/// Одна смена потолков.
#[derive(Debug, Clone)]
pub struct Change<'a> {
    pub act: &'a str,
    pub name: Option<&'a str>,
    pub before: &'a [i32],
    pub after: &'a [i32],
    pub block: Option<&'a str>,
    pub note: Option<&'a str>,
}

impl History {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Дописывает строку. Err — файл не дописан (текст для журнала программы).
    pub fn add(&self, change: &Change<'_>, now: Stamp) -> Result<(), String> {
        let mut line = Map::new();
        line.insert("time".into(), json!(now.roundtrip()));
        line.insert("at".into(), json!(now.0));
        line.insert("act".into(), json!(change.act));
        line.insert("name".into(), json!(change.name));
        if let Some(block) = change.block {
            line.insert("block".into(), json!(block));
        }
        line.insert("before".into(), json!(change.before));
        line.insert("after".into(), json!(change.after));
        if let Some(note) = change.note {
            line.insert("note".into(), json!(note));
        }
        files::append_line(&self.path, &Value::Object(line).to_string())
            .map_err(|e| format!("история профилей не дописана: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_appended_in_the_shape_of_earlier_builds() {
        let dir = std::env::temp_dir().join(format!("coremap-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // leftovers of an earlier run with the same id
        let path = dir.join("profile-log.jsonl");
        let history = History::new(path.clone());
        let (before, after) = ([50, 46], [49, 46]);
        let change = Change {
            act: "limit",
            name: None,
            before: &before,
            after: &after,
            block: Some("0"),
            note: None,
        };
        assert!(history.add(&change, Stamp(1)).is_ok());
        let change = Change {
            act: "open",
            name: Some("тихий"),
            block: None,
            note: Some("блок 2–5 не ответил"),
            ..change
        };
        assert!(history.add(&change, Stamp(2)).is_ok());
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir); // test folder; a leftover in %TEMP% does no harm
        let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        assert_eq!(lines.len(), 2, "{text}");
        // The fields and their order as builds up to 0.5.3 wrote them (the file is read by other parts).
        let keys: Vec<&str> = lines[0]
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(keys, ["act", "after", "at", "before", "block", "name", "time"]);
        assert_eq!(lines[0]["name"], Value::Null, "ceilings set by hand belong to no set");
        assert_eq!(lines[0]["after"], json!([49, 46]));
        assert_eq!(lines[1]["act"], "open");
        assert_eq!(lines[1]["note"], "блок 2–5 не ответил");
        assert!(lines[1].get("block").is_none());
    }
}
