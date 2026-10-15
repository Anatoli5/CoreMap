// Метка оборванного замера data\limits-held.json (как LimitHold.Recover в C#).
//
// Измерение C#-части пишет метку с потолками, стоявшими до него, перед первой записью в процессор и удаляет её
// после возврата. Если метка осталась — процесс убили или он упал посреди измерения, и у блока мог остаться потолок
// ступени: потолки возвращаются по метке. Выше заводского предела — не поднимаются: метка могла остаться и после
// зависания машины, а потолки выше заводских после сбоя проверяет профиль (profile.rs). Метка удаляется всегда —
// и после отказа записи (повторять отказанное бессмысленно), и от чужого процессора; так и в C#, иначе две части
// обходились бы с одной меткой по-разному.

use std::fmt::Write as _;
use std::path::Path;

use crate::Lp;
use crate::files::{self, read_ints};
use crate::limits::Limits;
use crate::mailbox::Cpu;
use crate::topology::Topology;

/// Что вышло из возврата по метке: что сказать владельцу и что записать в журнал программы.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    pub said: Option<String>,
    pub logged: Option<String>,
}

const INTRO: &str = "Прошлый запуск оборвался посреди измерения. ";

/// Возвращает потолки по метке `path`, если она есть, и удаляет её.
pub fn recover(
    path: &Path,
    cpu_name: &str,
    lps: &[Lp],
    limits: &mut Limits,
    cpu: &mut dyn Cpu,
    topology: &Topology,
) -> Recovery {
    if !path.exists() {
        return Recovery::default();
    }
    let mut recovery = restore(path, cpu_name, lps, limits, cpu, topology).unwrap_or_else(|cause| Recovery {
        said: Some(format!(
            "{INTRO}Файл с потолками, стоявшими до него, не прочитан — проверь потолки блоков."
        )),
        logged: Some(format!("метка исходных потолков не прочитана: {cause}")),
    });
    if let Err(e) = files::delete_with_backup(path) {
        let line = format!("метка исходных потолков не удалена: {e}");
        recovery.logged = Some(match recovery.logged.take() {
            Some(before) => format!("{before}\n{line}"),
            None => line,
        });
    }
    recovery
}

fn restore(
    path: &Path,
    cpu_name: &str,
    lps: &[Lp],
    limits: &mut Limits,
    cpu: &mut dyn Cpu,
    topology: &Topology,
) -> Result<Recovery, String> {
    let saved = files::read_object(path).map_err(|e| e.0)?.ok_or("файла нет")?;
    let marked_lps = read_ints(saved.get("lps")).ok_or("нет поля lps")?;
    let ours: Vec<i64> = lps.iter().map(|lp| i64::from(lp.0)).collect();
    if saved.get("cpu").and_then(|v| v.as_str()) != Some(cpu_name) || marked_lps != ours {
        return Ok(Recovery {
            said: None,
            logged: Some("метка исходных потолков от другого процессора — удалена без возврата".into()),
        });
    }
    let before = read_ints(saved.get("limits")).ok_or("нет поля limits")?;
    let mut back = Vec::new();
    let mut above = Vec::new();
    let mut refusal = None;
    for block in &topology.blocks {
        let index = block.first;
        let wanted = before.get(index).copied().ok_or("в метке меньше потолков, чем ядер")?;
        let Ok(wanted) = i32::try_from(wanted) else { continue };
        // Совпадает — только если ядро ответило при последнем чтении: иначе `current` — память программы, а не процессор
        // (ревизия, outputs/review-write-path-2026-10-14.md п. 2), и блок мог остаться на ступени замера.
        if !limits.known(index) || wanted <= 0 || (limits.answered(index) && limits.current()[index] == wanted) {
            continue;
        }
        if wanted > limits.fused()[index] {
            above.push(format!("{} ×{wanted}", block.name));
            continue;
        }
        match limits.set(cpu, topology, index, wanted) {
            None => back.push(format!("{} ×{wanted}", block.name)),
            Some(refused) => refusal = Some(refused),
        }
    }
    if back.is_empty() && above.is_empty() && refusal.is_none() {
        return Ok(Recovery::default());
    }
    let mut text = String::from(INTRO);
    if !back.is_empty() {
        let _ = write!(
            text,
            "Блокам возвращены потолки, стоявшие до него: {}. ",
            back.join(", ")
        ); // into a String: cannot fail
    }
    if !above.is_empty() {
        let _ = write!(
            text,
            "Не возвращены потолки выше заводского предела — поставь их сам: {}. ",
            above.join(", ")
        ); // into a String: cannot fail
    }
    if let Some(refused) = refusal {
        let _ = write!(text, "Отказ: {refused}."); // into a String: cannot fail
    }
    Ok(Recovery {
        said: Some(text.trim_end().to_string()),
        logged: None,
    })
}
