// История профилей: data\profile-log.jsonl — каждая постановка и смена потолков владельцем или по его профилю, строкой
// JSON (владелец 15.10: «нет ни истории, как я менял профиль»). Пишет только программа входа: она единственная, кто
// пишет профили по командам окна и меню. Файл только дописывается; окну уходят последние `SHOWN` строк.
//
// Строка: время («o», как в файлах профилей) и такты, что сделано (`act`), название набора (null — потолки не из
// набора, "" — заводские), потолки по ядрам до и после; у ручной правки — блок; у неполной постановки — причина.
// Что сделано: logon — профиль поставлен при входе в Windows; menu — набор выбран в меню значка; open, save, accept,
// revert, delete — команды окна; limit — потолок блока поставлен вручную; wake — после сна потолки поставлены заново;
// foreign — другая программа изменила потолки, изменение принято.

use std::collections::VecDeque;
use std::path::PathBuf;

use machine::clock::Stamp;
use machine::files;
use serde_json::{Map, Value, json};

/// Столько последних строк получает окно.
pub const SHOWN: usize = 100;

#[derive(Debug)]
pub struct History {
    path: PathBuf,
    /// Последние строки; None — файл ещё не читался (читается, когда окну впервые нужна история).
    last: Option<VecDeque<Value>>,
    /// Сколько строк дописано за эту работу программы — по нему окну шлётся новая история.
    pub written: u64,
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
        Self {
            path,
            last: None,
            written: 0,
        }
    }

    /// Дописывает строку. Err — файл не дописан (текст для журнала программы).
    pub fn add(&mut self, change: &Change<'_>, now: Stamp) -> Result<(), String> {
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
        let line = Value::Object(line);
        files::append_line(&self.path, &line.to_string()).map_err(|e| format!("история профилей не дописана: {e}"))?;
        self.written += 1;
        if let Some(last) = &mut self.last {
            last.push_back(line);
            while last.len() > SHOWN {
                last.pop_front();
            }
        }
        Ok(())
    }

    /// Последние строки: читаются из файла один раз, дальше держатся в памяти. Испорченные строки пропускаются.
    pub fn last(&mut self) -> &VecDeque<Value> {
        let path = &self.path;
        self.last.get_or_insert_with(|| {
            let text = std::fs::read_to_string(path).unwrap_or_default(); // нет файла — истории ещё нет
            let mut last: VecDeque<Value> = text
                .lines()
                .rev()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok().filter(Value::is_object))
                .take(SHOWN)
                .collect();
            last.make_contiguous().reverse();
            last
        })
    }

    /// Сообщение окну: {"t":"history","entries":[… от старых к новым]}.
    pub fn topic(&mut self) -> String {
        json!({ "t": "history", "entries": self.last() }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_append_and_the_window_gets_the_last_ones() {
        let dir = std::env::temp_dir().join(format!("coremap-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // leftovers of an earlier run with the same id
        let path = dir.join("profile-log.jsonl");
        let mut history = History::new(path.clone());
        let before = [50, 46];
        for k in 0..(SHOWN + 5) {
            let after = [i32::try_from(k).unwrap_or(0), 46];
            let change = Change {
                act: "limit",
                name: None,
                before: &before,
                after: &after,
                block: Some("0"),
                note: None,
            };
            assert!(history.add(&change, Stamp(1)).is_ok());
        }
        assert_eq!(history.written, u64::try_from(SHOWN + 5).unwrap_or(0));
        // A fresh reader (the next run of the program) sees the same last lines as the one that wrote them.
        let mut again = History::new(path);
        let last = again.last().clone();
        assert_eq!(last.len(), SHOWN);
        assert_eq!(last.back().map(|v| v["after"][0].clone()), Some(json!(SHOWN + 4)));
        assert_eq!(last.front().map(|v| v["after"][0].clone()), Some(json!(5)));
        assert_eq!(last[0]["name"], Value::Null, "ceilings set by hand belong to no set");
        let _ = std::fs::remove_dir_all(&dir); // test folder; a leftover in %TEMP% does no harm
    }
}
