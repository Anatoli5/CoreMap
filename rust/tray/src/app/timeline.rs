// Журнал событий для окна (владелец 16.10, через координатора): data\events\<день>.jsonl — что меняло потолки блоков и
// что блок накопил на потолке, который он покидает. Отдельно от data\auto-log.jsonl: тот — память автомодуля.
//
// Строка: {"time", "kind", "block", "from", "to", "period", "conditions"}; время — как в журнале автомодуля
// (`Stamp::roundtrip`). kind:
// - emergency, target, return — срезы и возвраты автомодуля (app/auto.rs, `record`);
// - profile — поставлен набор (меню, «применить» в окне, профиль запуска при входе): строка на каждый блок, у которого
//   потолок сменился; edit — потолок плитки из окна; foreign — принятая чужая правка (все — из `log_change`, одного
//   места записи истории потолков);
// - wake — сверка после сна поставила потолки на место (тоже из `log_change`);
// - putback — автомодуль вернул опущенным блокам принятые (app/auto.rs, `put_back`), поле «reason»: auto-off,
//   settings, sleep, exit, csharp (смена набора — нет: её строки profile идут от того потолка, что стоял);
// - reset — «сбросить максимумы»: без блока и без периода.
// Так журнал — полная история потолков: окно строит по нему потолки во времени.
// period — у каждой строки со сменой потолка: итог блока на потолке, который он покидает, из хранилища плиток
// (app/meter.rs, `Kept::period`) — {"ceiling", "secs", "max", "maxTime", "mean"}. Только итог: строки на каждый новый
// максимум нет (владелец: «максимум пишется только последний для данной конфигурации»). conditions — что было у
// решения (у автомодуля — показание или медиана, кристалл, медианы, множитель, загрузка, видеокарта, вентиляторы), у
// правок владельца — последние медианы и показания, что есть. Неизвестное не пишется.
//
// Файл на день, как срезы (`<день>.jsonl` в data\events): старые дни удаляются целиком при смене дня, файл, который
// может читать окно, не переписывается. Срок — тот же, что у срезов (app/sense.rs, `KEEP_DAYS`).

use std::path::{Path, PathBuf};

use machine::clock::{Pattern, Stamp};
use machine::files;
use serde_json::{Map, Value, json};

use super::App;
use super::sense::KEEP_DAYS;

/// Журнал событий: до какого дня старые файлы уже удалены.
#[derive(Debug, Default)]
pub struct Timeline {
    pruned: String,
}

/// Число с `digits` знаками после запятой.
fn round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round() / scale
}

impl App {
    fn events_dir(&self) -> PathBuf {
        self.config.data.join("events")
    }

    /// Строка журнала событий. `block` — номер блока; `change` — (с какого, на какой потолок): с ним пишется итог блока на
    /// покидаемом потолке; `reason` — почему (у putback: auto-off, settings, sleep, exit, csharp). Не записалось — одна строка в error.log, программа идёт дальше.
    pub(super) fn log_event(
        &mut self,
        kind: &str,
        block: Option<usize>,
        change: Option<(i32, i32)>,
        reason: Option<&str>,
        conditions: Map<String, Value>,
    ) {
        let stamp = Stamp::now();
        let mut line = Map::new();
        line.insert("time".into(), json!(stamp.roundtrip()));
        line.insert("kind".into(), json!(kind));
        if let Some(reason) = reason {
            line.insert("reason".into(), json!(reason));
        }
        if let Some(b) = block
            && let Some(name) = self.topology.blocks.get(b).map(|x| x.name.clone())
        {
            line.insert("block".into(), json!(name));
            if let Some((from, to)) = change {
                line.insert("from".into(), json!(from));
                line.insert("to".into(), json!(to));
                let p = self.kept.period(b, from);
                let mut period = Map::new();
                period.insert("ceiling".into(), json!(p.ceiling));
                period.insert("secs".into(), json!(round(p.secs, 1)));
                if let Some(max) = p.max {
                    period.insert("max".into(), json!(max));
                }
                if let Some(at) = p.max_at {
                    period.insert("maxTime".into(), json!(at.roundtrip()));
                }
                if let Some(mean) = p.mean {
                    period.insert("mean".into(), json!(round(mean, 1)));
                }
                line.insert("period".into(), Value::Object(period));
            }
        }
        if !conditions.is_empty() {
            line.insert("conditions".into(), Value::Object(conditions));
        }
        let dir = self.events_dir();
        let day = stamp.local(Pattern::Date);
        if day != self.timeline.pruned {
            prune(self, &dir, stamp);
            self.timeline.pruned.clone_from(&day);
        }
        let written = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|()| {
            files::append_line(&dir.join(format!("{day}.jsonl")), &Value::Object(line).to_string())
                .map_err(|e| e.to_string())
        });
        if let Err(e) = written {
            self.journal.error(&format!("журнал событий не дописан: {e}"));
        }
    }

    /// Условия правки владельца: последние средние регулятора за выбранное окно, последние показания всех блоков и
    /// самого блока, видеокарта и вентиляторы — что известно.
    pub(super) fn own_conditions(&self, block: usize) -> Map<String, Value> {
        let mut map = Map::new();
        if let Some(controller) = self.auto.controller.as_ref() {
            let means: Vec<Option<f64>> = controller.means().map(|m| m.map(|v| round(v, 1))).collect();
            if means.iter().any(Option::is_some) {
                map.insert("means".into(), json!(means));
                map.insert("windowSecs".into(), json!(self.auto.choice.window_secs));
            }
        }
        let temps: Vec<Option<f64>> = self
            .topology
            .blocks
            .iter()
            .map(|b| self.meter.latest(b.first).temp)
            .collect();
        if temps.iter().any(Option::is_some) {
            map.insert("temps".into(), json!(temps));
        }
        if let Some(b) = self.topology.blocks.get(block) {
            let r = self.meter.latest(b.first);
            for (name, value, digits) in [("temp", r.temp, 0), ("ratio", r.ratio, 2), ("busy", r.busy, 2)] {
                if let Some(v) = value {
                    map.insert(name.into(), json!(round(v, digits)));
                }
            }
        }
        self.known_surroundings(&mut map);
        map
    }

    /// Видеокарта и вентиляторы, если известны, — в условия строки.
    pub(super) fn known_surroundings(&self, map: &mut Map<String, Value>) {
        if let Some(g) = self.last_gpu() {
            let mut gpu = Map::new();
            for (name, value, digits) in [("temp", g.temp, 0), ("power", g.power, 1), ("busy", g.busy, 2)] {
                if let Some(v) = value {
                    gpu.insert(name.into(), json!(round(v, digits)));
                }
            }
            if !gpu.is_empty() {
                map.insert("gpu".into(), Value::Object(gpu));
            }
        }
        if let Some(rpm) = &self.auto.rpm {
            map.insert("fans".into(), json!(rpm));
        }
    }

    /// Правка потолков владельцем, принятая чужая или сверка после сна (`log_change`, act истории профилей): строка на
    /// каждый блок, у которого потолок сменился.
    pub(super) fn ceiling_events(&mut self, act: &str, before: &[i32], after: &[i32]) {
        let kind = match act {
            "menu" | "open" | "logon" => "profile",
            "limit" => "edit",
            "foreign" => "foreign",
            "wake" => "wake",
            _ => return,
        };
        let changed: Vec<(usize, i32, i32)> = self
            .topology
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(b, block)| {
                let (from, to) = (*before.get(block.first)?, *after.get(block.first)?);
                (from != to).then_some((b, from, to))
            })
            .collect();
        for (b, from, to) in changed {
            let conditions = self.own_conditions(b);
            self.log_event(kind, Some(b), Some((from, to)), None, conditions);
        }
    }
}

/// Файлы событий старше `KEEP_DAYS` дней удаляются (имя — «ГГГГ-ММ-ДД.jsonl»).
fn prune(app: &App, dir: &Path, now: Stamp) {
    let cutoff = Stamp(now.0 - KEEP_DAYS * 24 * 3600 * 10_000_000).local(Pattern::Date);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let old = name
            .strip_suffix(".jsonl")
            .is_some_and(|day| day.len() == 10 && day.as_bytes()[4] == b'-' && day < cutoff.as_str());
        if old && let Err(e) = std::fs::remove_file(entry.path()) {
            app.journal.error(&format!("старый файл событий {name} не удалён: {e}"));
        }
    }
}
