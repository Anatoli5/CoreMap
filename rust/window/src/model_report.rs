// Скрытый отчёт модели нагрева (CoreMapWindow.exe --model-report <срезы> <отчёт> [--split "ГГГГ-ММ-ДД чч:мм:сс"]):
// без окна и без канала. Срезы читаются тем же разбором, что у окна (store::read_all → samples.rs), модель — тем же
// счётом (model.rs); соседи блоков — из схемы кристалла layouts\*.json рядом с программой (та же, что программа входа
// шлёт окну в знакомстве), по тому же правилу соседства (die::neighbours). Отчёт читают агенты — он по-английски
// (правило владельца G46); пишет коэффициенты, ошибки на отложенных данных, проверку по моменту смены потолка, кривые
// напряжения, проверку мощности, слой уровня кристалла и время подгонки.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

use crate::model::{self, Die, Log, Model};
use crate::samples;
use crate::store;
use crate::wire::{BlockInfo, Hello, Layout, Tile};

/// Отчёт; код выхода: 0 — записан, 2 — нет срезов или схемы кристалла, 3 — файл отчёта не записан.
pub fn run(dir: &Path, out: &Path, split: Option<&str>) -> i32 {
    let mut text = String::new();
    let code = report(dir, split, &mut text);
    if std::fs::write(out, &text).is_err() {
        return 3;
    }
    code
}

fn report(dir: &Path, split: Option<&str>, text: &mut String) -> i32 {
    // Папка срезов: та, что дали, или её samples.
    let folder = if dir.join("samples").is_dir() {
        dir.join("samples")
    } else {
        dir.to_path_buf()
    };
    let read_started = Instant::now();
    let (logs, trouble) = store::read_all(folder.clone());
    let read_time = read_started.elapsed();
    let logs: Vec<&Log> = logs.iter().filter(|l| l.rows() > 0).collect();
    let _ = writeln!(
        text,
        "CoreMap thermal model report (rust/window/src/model.rs, DECISIONS #39)"
    ); // into a String
    let _ = writeln!(text, "samples folder: {}", folder.display());
    let _ = writeln!(
        text,
        "own fast term: learned shape g(r) (hinge: busy*1, busy*(r-lo), busy*max(0, r-k), knots every 2 steps over the p5-p95 solid range, all weights >= 0: increasing and convex)"
    );
    let _ = writeln!(
        text,
        "history terms, neighbours, power split: p = busy*(r/45)^3 - the power shape is ASSUMED cubic, not measured; no voltage input (removed 16.10)"
    );
    let _ = writeln!(
        text,
        "known limitation: the CPU lowers its multiplier when hot (thermal velocity boost), so hot rows sit at lower r - the top end of g can be biased downward; not corrected"
    );
    if let Some(t) = trouble {
        let _ = writeln!(text, "read trouble: {t}");
    }
    let Some(names) = logs.first().map(|l| l.names().to_vec()) else {
        let _ = writeln!(text, "FAILED: no 0.25-s rows in the folder");
        return 2;
    };
    let die = match die_for(&names) {
        Ok(die) => die,
        Err(e) => {
            let _ = writeln!(text, "FAILED: {e}");
            return 2;
        }
    };
    let rows: usize = logs.iter().map(|l| l.rows()).sum();
    let bytes: usize = logs.iter().map(|l| l.bytes()).sum();
    let _ = writeln!(
        text,
        "files with 0.25-s rows: {}, rows: {rows} (every row of the folder: no 8-h cap since brief W 16.10), memory of the rows: {:.1} MB, reading and parsing: {:.0} ms",
        logs.len(),
        mb(bytes),
        read_time.as_secs_f64() * 1000.0
    );
    let fitted = model::fit(&logs, &die);
    let _ = writeln!(
        text,
        "fit time on all rows (L3 with the shape, the shared P-core rank, the proxy form, all blocks, 4 folds, ablations, halves, three passes): {:.0} ms",
        fitted.time.as_secs_f64() * 1000.0
    );
    retention_section(&logs, &die, &fitted, text);
    let _ = writeln!(text);
    blocks(&fitted, &die, text);
    acceptance(&fitted, text);
    rank_section(&fitted, text);
    influence_section(&fitted, text);
    shape_tables(&logs, &die, &fitted, text);
    split_power_section(&fitted, text);
    equal_load_section(&fitted, text);
    if let Some(at) = split {
        split_section(&logs, &die, at, text);
    }
    level_section(&logs, &die, text);
    power_section(&logs, &die, text);
    0
}

/// Хранение строк (бриф W): что оно оставило бы после этой подгонки, память и время подгонки по оставленному.
fn retention_section(logs: &[&Log], die: &Die, fitted: &Model, text: &mut String) {
    let mut kept: Vec<Log> = logs.iter().map(|&l| l.clone()).collect(); // the report's own copy of the rows, once
    let started = Instant::now();
    let r = model::retain(&mut kept.iter_mut().collect::<Vec<_>>(), die, fitted);
    let retain_time = started.elapsed();
    let refit = model::fit(&kept.iter().collect::<Vec<_>>(), die);
    let errors = |m: &Model| {
        let mut e: Vec<f64> = m.blocks.iter().filter_map(|b| b.errors.map(|e| e.full)).collect();
        e.sort_by(f64::total_cmp);
        e.get(e.len() / 2).copied()
    };
    let _ = writeln!(
        text,
        "retention (cell = block x whole multiplier; newest 10-s windows until the standard error of the cell's mean partial residual <= {} °C, at least {} windows; rows older than that dropped unless another block needs them; 16 s before each kept row and the last {} s kept): {} of {} cells saturated; rows {} -> {}, memory {:.1} MB -> {:.1} MB, retention pass {:.0} ms",
        model::SATURATED,
        model::MIN_WINDOWS,
        model::LIVE_MS / 1000,
        r.saturated,
        r.cells,
        r.before,
        r.after,
        mb(logs.iter().map(|l| l.bytes()).sum()),
        mb(r.bytes),
        retain_time.as_secs_f64() * 1000.0
    );
    let _ = writeln!(
        text,
        "fit time on the retained rows: {:.0} ms; median held-out (c) over blocks: all rows {}, retained rows {}",
        refit.time.as_secs_f64() * 1000.0,
        errors(fitted).map_or_else(|| "-".into(), |v| format!("{v:.2} °C")),
        errors(&refit).map_or_else(|| "-".into(), |v| format!("{v:.2} °C"))
    );
}

/// Общий ранг P-ядер (дополнение брифа 16.10): только здесь, не в окне.
fn rank_section(fitted: &Model, text: &mut String) {
    let _ = writeln!(text, "\n== shared P-core rank (report only) ==");
    let _ = writeln!(
        text,
        "term: busy*[rank = k]*p(r) (own fast p in rows where the core's ceiling is the highest (rank 1) or the next distinct (rank 2) among P-cores; rank 3 = base), one coefficient per rank for all P-cores, any sign; identified only by rank changes within a core"
    );
    let r = &fitted.rank;
    for k in 0..model::RANKS {
        if !r.used[k] {
            let _ = writeln!(
                text,
                "rank {}: not in the fit - no P-core has >= {} usable loaded rows both in and out of this rank",
                k + 1,
                model::MIN_ROWS
            );
            continue;
        }
        let _ = writeln!(
            text,
            "rank {}: {:+.2} °C per unit own p (folds {:+.2} .. {:+.2}) - {}",
            k + 1,
            r.value[k],
            r.lo[k],
            r.hi[k],
            if r.determined[k] {
                "determined (the fold range does not cross 0)"
            } else {
                "not determined (the fold range crosses 0 or a fold did not solve)"
            }
        );
    }
}

/// «Что влияет на температуру ядра» (бриф W): изменчивость вклада групп и ошибка без группы.
fn influence_section(fitted: &Model, text: &mut String) {
    let _ = writeln!(text, "\n== what affects the core temperature (per block) ==");
    let _ = writeln!(
        text,
        "size = time-weighted SD of the group's contribution over the fitted rows, °C (folds lo..hi; '?' = the fold range reaches 0); ablation = held-out (c) error without the group minus with it, °C"
    );
    let names = ["die", "own work", "neighbours", "history", "rank"];
    for b in &fitted.blocks {
        let Some(inf) = b.influence else {
            let _ = writeln!(text, "block {:>5}: no fit", b.name);
            continue;
        };
        let parts: Vec<String> = (0..model::GROUPS)
            .map(|g| {
                format!(
                    "{} {:.2}{} ({:.2}..{:.2}; {})",
                    names[g],
                    inf.size[g],
                    if inf.determined(g) { "" } else { "?" },
                    inf.lo[g],
                    inf.hi[g],
                    inf.ablation[g].map_or_else(|| "-".into(), |a| format!("{a:+.3}"))
                )
            })
            .collect();
        let _ = writeln!(text, "block {:>5}: {}", b.name, parts.join(" | "));
    }
}

fn mb(bytes: usize) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "megabytes of rows, far below 2^52")]
    let b = bytes as f64;
    b / 1_048_576.0
}

/// Блоки и соседи по схеме кристалла, чьи плитки — ровно блоки файла (названия: номер процессора Windows или
/// «первый–последний» у кластера, как у программы входа, machine/topology.rs).
fn die_for(names: &[String]) -> Result<Die, String> {
    let mut folders: Vec<PathBuf> = Vec::new();
    if let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        folders.push(exe.join("layouts"));
    }
    folders.push(PathBuf::from("layouts"));
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files.iter().filter(|p| p.extension().is_some_and(|x| x == "json")) {
            let Ok(doc) = std::fs::read_to_string(path).map(|t| serde_json::from_str::<Value>(&t)) else {
                continue;
            };
            let Ok(doc) = doc else { continue };
            if let Some(hello) = hello_of(&doc, names) {
                return Ok(Die::of(&hello));
            }
        }
    }
    Err(format!(
        "no layouts\\*.json next to the program or in the current folder has exactly the blocks {names:?}"
    ))
}

/// Знакомство из файла схемы, если её плитки — ровно эти блоки.
fn hello_of(doc: &Value, names: &[String]) -> Option<Hello> {
    let units = doc["units"].as_array()?;
    let mut tiles = Vec::with_capacity(units.len());
    let mut blocks = Vec::with_capacity(units.len());
    for unit in units {
        let lps: Vec<u64> = unit["lps"].as_array()?.iter().filter_map(Value::as_u64).collect();
        let (lo, hi) = (lps.iter().min()?, lps.iter().max()?);
        let name = if lps.len() == 1 {
            lo.to_string()
        } else {
            format!("{lo}–{hi}")
        };
        let index = names.iter().position(|n| *n == name)?;
        blocks.push(BlockInfo {
            index,
            members: Vec::new(),
            first: 0,
            lp: i64::try_from(*lo).ok()?,
            kind: if lps.len() > 1 { "E".into() } else { "P".into() },
            name,
        });
        tiles.push(Tile {
            block: index,
            col: u16::try_from(unit["col"].as_u64()?).ok()?,
            row: u16::try_from(unit["row"].as_u64()?).ok()?,
            places: vec![0; lps.len()],
        });
    }
    if blocks.len() != names.len() {
        return None;
    }
    blocks.sort_by_key(|b| b.index);
    if blocks.iter().enumerate().any(|(i, b)| b.index != i) {
        return None;
    }
    Some(Hello {
        cpu: String::new(),
        simulated: false,
        cores: Vec::new(),
        blocks,
        layout: Layout {
            note: None,
            confirmed: false,
            cols: i32::try_from(doc["cols"].as_i64()?).ok()?,
            rows: i32::try_from(doc["rows"].as_i64()?).ok()?,
            tiles,
        },
    })
}

fn blocks(fitted: &Model, die: &Die, text: &mut String) {
    let _ = writeln!(text, "== L3 per block ==");
    let _ = writeln!(
        text,
        "errors: held-out RMSE of 10-s means of T-D, 4 contiguous folds; (a) offset + beta*D, (b) + own, (c) + neighbours - with the learned shape; (c) old = the proxy form K*p for the own fast term"
    );
    for (i, b) in fitted.blocks.iter().enumerate() {
        let near: Vec<&str> = die.neighbours[i].iter().map(|&j| die.names[j].as_str()).collect();
        let _ = writeln!(
            text,
            "\nblock {} ({}): usable rows {} ({:.1} min), neighbours {}",
            b.name,
            if b.e { "E-cluster" } else { "P" },
            b.usable,
            b.minutes,
            near.join(", ")
        );
        coef_lines(b, die, text);
        physics_lines(b, die, text);
    }
}

/// Выученная форма: узлы, веса, излом.
fn shape_line(c: &model::Coef, text: &mut String) {
    let h = &c.shape;
    let bends: Vec<String> = h
        .knots
        .iter()
        .zip(h.weights.iter().skip(2))
        .map(|(k, w)| format!("x{k:.0} {w:.2}"))
        .collect();
    let _ = writeln!(
        text,
        "  shape g(r): lo x{:.2}, base {:.2} °C, slope {:.3} °C/step, bends [{}]; g(lo) {:.2}, g(x45) {:.2}",
        h.lo,
        h.weights.first().copied().unwrap_or(0.0),
        h.weights.get(1).copied().unwrap_or(0.0),
        bends.join(", "),
        h.at(h.lo),
        h.at(model::REFERENCE)
    );
    let _ = match h.knee() {
        Some(k) => writeln!(
            text,
            "  knee at x{:.0}: slope {:.3} -> {:.3} °C per step at full load",
            k.at, k.before, k.after
        ),
        None => writeln!(
            text,
            "  knee: none (rule: largest bend >= 0.1 °C/step and slope after >= 2x before)"
        ),
    };
}

/// Коэффициенты блока и ошибки на отложенных данных.
fn coef_lines(b: &model::BlockModel, die: &Die, text: &mut String) {
    if let Some(c) = &b.shaped {
        let _ = writeln!(
            text,
            "  shaped L3: a {:.2}, beta {:.3}, history (ema 0.5..16 s) [{:.2}, {:.2}, {:.2}, {:.2}, {:.2}, {:.2}] total {:.2}",
            c.a,
            c.beta,
            c.own[1],
            c.own[2],
            c.own[3],
            c.own[4],
            c.own[5],
            c.own[6],
            c.own_total()
        );
        shape_line(c, text);
        let near: Vec<String> = c
            .neighbours
            .iter()
            .map(|n| format!("{} {:.2}/{:.2}", die.names[n.block], n.fast, n.slow))
            .collect();
        let _ = writeln!(text, "  shaped neighbours fast/4-s: [{}]", near.join(", "));
    }
    match &b.coef {
        Some(c) => {
            let _ = writeln!(
                text,
                "  proxy-form L3 (for °C/W): a {:.2}, beta {:.3}, own [fast {:.2}; ema 0.5s {:.2}, 1s {:.2}, 2s {:.2}, 4s {:.2}, 8s {:.2}, 16s {:.2}] total K {:.2}",
                c.a,
                c.beta,
                c.own[0],
                c.own[1],
                c.own[2],
                c.own[3],
                c.own[4],
                c.own[5],
                c.own[6],
                c.own_total()
            );
            for n in &c.neighbours {
                let _ = writeln!(
                    text,
                    "  neighbour {}: fast {:.2}, 4-s average {:.2}",
                    die.names[n.block], n.fast, n.slow
                );
            }
        }
        None => {
            let _ = writeln!(text, "  coef: none (fewer than {} usable rows)", model::MIN_ROWS);
        }
    }
    if let Some(e) = b.errors {
        let _ = writeln!(
            text,
            "  held-out: (a) {:.2}  (b) {:.2}  (c) {:.2}  (c) old {:.2} °C over {} windows; diagnosis (d) = (c) with beta fixed at 1: {:.2}",
            e.die_only, e.own, e.full, e.proxy, e.windows, e.unit
        );
    }
    if let Some(d) = &b.diagnosis {
        let near = |c: &model::Coef| -> String {
            c.neighbours
                .iter()
                .map(|n| format!("{} {:+.2}/{:+.2}", die.names[n.block], n.fast, n.slow))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            text,
            "  diagnosis, unconstrained (c): beta {:.3}, history total {:+.2}, g(x45) {:+.2}, neighbours fast/4-s [{}]",
            d.signed.beta,
            d.signed.own_total(),
            d.signed.shape.at(model::REFERENCE),
            near(&d.signed)
        );
        let _ = writeln!(
            text,
            "  diagnosis, beta = 1 (d): history total {:.2}, g(x45) {:.2}, neighbours fast/4-s [{}]",
            d.unit.own_total(),
            d.unit.shape.at(model::REFERENCE),
            near(&d.unit)
        );
    }
}

/// Опорные условия, кривая, «°C на ступень», стабильность.
fn physics_lines(b: &model::BlockModel, die: &Die, text: &mut String) {
    if let Some(r) = &b.reference {
        let near: Vec<String> = r
            .neighbours
            .iter()
            .map(|&(j, p)| format!("{} {p:.3}", die.names[j]))
            .collect();
        let _ = writeln!(
            text,
            "  reference (rows with own busy >= 0.5: {}): die {:.1} °C, neighbours p [{}], actual multiplier x{:.2}..x{:.2}, median x{:.2}",
            r.rows,
            r.die,
            near.join(", "),
            r.lo,
            r.hi,
            r.median
        );
        let curve: Vec<String> = [r.lo, r.median, r.hi]
            .iter()
            .filter_map(|&x| Some(format!("x{x:.1} {:.1} (old {:.1})", b.curve(x)?, b.proxy_curve(x)?)))
            .collect();
        let _ = writeln!(
            text,
            "  curve (absolute °C), shaped (old proxy form): {}",
            curve.join(", ")
        );
    }
    // «°C на ватт» = K/c прежнего вида: K — его свой нагрев при полной нагрузке на ×45 (там p = 1); выученная форма в
    // него не входит — рядом её нагрев на ×45.
    if let (Some(old), Some(new)) = (&b.coef, &b.shaped) {
        let _ = writeln!(
            text,
            "  own heating at full load at x45: proxy form K = {:.2} °C (this K enters °C/W = K/c), learned shape g(45) + history = {:.2} °C",
            b.heat(old, model::REFERENCE),
            b.heat(new, model::REFERENCE)
        );
    }
    if let Some(s) = b.slope {
        let _ = writeln!(
            text,
            "  slope: {:.3} °C per step at x{:.1} (folds {:.3}..{:.3}), {}",
            s.value,
            s.at,
            s.lo,
            s.hi,
            if s.value > 1e-9 {
                format!("{:.2} steps per °C", 1.0 / s.value)
            } else {
                "flat".into()
            }
        );
    }
    match b.halves {
        Some(h) => {
            let _ = writeln!(text, "  proxy-form K, max/min between data halves: {h:.2}");
        }
        None => {
            let _ = writeln!(text, "  proxy-form K between halves: not comparable (a half has none)");
        }
    }
}

/// Слой разделения мощности и «°C на ватт» по блокам.
fn split_power_section(fitted: &Model, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== power split: P = c0 + sum_j c_j * p_j (E-cluster p x4), NNLS, 4 contiguous folds; degrees per watt = K_own,total / watts =="
    );
    let Some(split) = &fitted.power else {
        let _ = writeln!(
            text,
            "not computed: no rows with power_cores or power_cpu where every block's p is known (or fewer than 4 folds' worth)"
        );
        return;
    };
    let _ = writeln!(
        text,
        "column {}: rows {}, {:.1} min; c0 {:.2} W; held-out RMSE {:.2} W per row",
        split.column, split.rows, split.minutes, split.c0, split.rmse
    );
    for b in &fitted.blocks {
        let Some(pw) = b.power else { continue };
        let partner = pw.partner.map_or_else(
            || "-".into(),
            |(j, r)| format!("{} (r = {r:+.2})", fitted.blocks[j].name),
        );
        let figure = match b.resistance {
            Some(r) => format!(
                "{:.3} °C/W ({:.3}..{:.3}), same-work reference {:.2} W",
                r.value,
                r.lo,
                r.hi,
                b.same_work.unwrap_or(f64::NAN)
            ),
            None if !pw.determined => "no figure: watts not determined separately".into(),
            None => "no figure: no L3 coefficients".into(),
        };
        let _ = writeln!(
            text,
            "block {:>5}: {:.2} W per unit p (folds {:.2}..{:.2}), {}; R² on the others {:.2}, most linked {partner}; {figure}",
            b.name,
            pw.watts,
            pw.lo,
            pw.hi,
            if pw.determined { "determined" } else { "NOT determined" },
            pw.collinear
        );
    }
}

/// Тепловой порядок ядер по прогону 12.10 при одинаковой нагрузке (SHA-256, KNOWLEDGE.md): P-ядра — нагрев, °C, при
/// 9,8–12,1 Вт; E-кластеры — колонка Б (6–9, 18–21) горячее колонки А (2–5, 14–17).
const EQUAL_LOAD_P: [(&str, f64); 8] = [
    ("12", 13.4),
    ("23", 13.8),
    ("10", 15.8),
    ("22", 15.8),
    ("0", 18.4),
    ("11", 25.6),
    ("1", 26.5),
    ("13", 28.8),
];
const COLUMN_A: [&str; 2] = ["2–5", "14–17"];
const COLUMN_B: [&str; 2] = ["6–9", "18–21"];

/// Ранги (средний ранг у равных), с 1.
fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut out = vec![0.0; values.len()];
    let mut k = 0;
    while k < order.len() {
        let mut end = k;
        while end + 1 < order.len() && (values[order[end + 1]] - values[order[k]]).abs() < 1e-12 {
            end += 1;
        }
        #[expect(clippy::cast_precision_loss, reason = "a dozen ranks")]
        let rank = (k + end) as f64 / 2.0 + 1.0;
        for &i in &order[k..=end] {
            out[i] = rank;
        }
        k = end + 1;
    }
    out
}

/// Коэффициент Спирмена: Пирсон по рангам. None — меньше трёх пар или нет разброса.
fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() < 3 {
        return None;
    }
    let (ra, rb) = (ranks(a), ranks(b));
    #[expect(clippy::cast_precision_loss, reason = "a dozen ranks")]
    let n = ra.len() as f64;
    let (ma, mb) = (ra.iter().sum::<f64>() / n, rb.iter().sum::<f64>() / n);
    let cov: f64 = ra.iter().zip(&rb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let va: f64 = ra.iter().map(|x| (x - ma).powi(2)).sum();
    let vb: f64 = rb.iter().map(|y| (y - mb).powi(2)).sum();
    (va > 0.0 && vb > 0.0).then(|| cov / (va * vb).sqrt())
}

/// Сверка с прогоном 12.10 при одинаковой нагрузке: «°C на ватт» (новое число) и, для сравнения, свой коэффициент на
/// единицу занятости (отвергнутое число, DECISIONS #39).
fn equal_load_section(fitted: &Model, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== acceptance against the 12.10 equal-load run (KNOWLEDGE.md): P-core heating order 12 < 23 < 10 = 22 < 0 < 11 < 1 < 13; E column B (6-9, 18-21) hotter than A (2-5, 14-17) =="
    );
    let block = |name: &str| fitted.blocks.iter().find(|b| b.name == name);
    let compare = |what: &str, value: &dyn Fn(&model::BlockModel) -> Option<f64>, text: &mut String| {
        let pairs: Vec<(f64, f64, &str)> = EQUAL_LOAD_P
            .iter()
            .filter_map(|&(name, heat)| Some((value(block(name)?)?, heat, name)))
            .collect();
        let shown: Vec<String> = pairs.iter().map(|(v, _, n)| format!("{n} {v:.2}")).collect();
        let rho = spearman(
            &pairs.iter().map(|p| p.0).collect::<Vec<_>>(),
            &pairs.iter().map(|p| p.1).collect::<Vec<_>>(),
        );
        let _ = writeln!(
            text,
            "{what}: P-cores with a value {} of 8 [{}]; Spearman vs 12.10 {}",
            pairs.len(),
            shown.join(", "),
            rho.map_or_else(|| "not testable (fewer than 3 P-cores)".into(), |r| format!("{r:+.2}"))
        );
        let column = |names: &[&str]| -> Option<Vec<f64>> { names.iter().map(|n| value(block(n)?)).collect() };
        match (column(&COLUMN_A), column(&COLUMN_B)) {
            (Some(a), Some(b)) => {
                let split = b.iter().all(|vb| a.iter().all(|va| vb > va));
                let _ = writeln!(
                    text,
                    "{what}: E column A [{:.2}, {:.2}], column B [{:.2}, {:.2}]: B above A for every pair: {split}",
                    a[0], a[1], b[0], b[1]
                );
            }
            _ => {
                let _ = writeln!(text, "{what}: E columns not testable (an E-cluster has no value)");
            }
        }
    };
    compare(
        "degrees per watt (new figure)",
        &|b| b.resistance.map(|r| r.value),
        text,
    );
    compare(
        "K_own,total per unit busy (rejected figure, for contrast)",
        &|b| b.coef.as_ref().map(model::Coef::own_total),
        text,
    );
    if fitted.blocks.iter().all(|b| b.resistance.is_none()) {
        let _ = writeln!(
            text,
            "needed for the new figure: play rows with power_cores (CoreMapTray 0.5.4) where the P-cores' loads are not locked together; {}",
            fitted.power.as_ref().map_or_else(
                || "today: no rows with power at all".to_string(),
                |p| format!(
                    "today: {:.1} min of rows with {} (0.5.3 desktop and the analysis agents' runs, not play)",
                    p.minutes, p.column
                )
            )
        );
    }
}

fn acceptance(fitted: &Model, text: &mut String) {
    let _ = writeln!(text, "\n== acceptance (brief) ==");
    let mut full: Vec<f64> = fitted.blocks.iter().filter_map(|b| b.errors.map(|e| e.full)).collect();
    full.sort_by(f64::total_cmp);
    let median = match full.len() {
        0 => None,
        n if n % 2 == 1 => Some(full[n / 2]),
        n => Some(full[n / 2 - 1].midpoint(full[n / 2])),
    };
    let p_better = fitted
        .blocks
        .iter()
        .filter(|b| !b.e)
        .all(|b| b.errors.is_some_and(|e| e.full < e.die_only));
    let _ = writeln!(
        text,
        "{}  (c) better than (a) for every P-core",
        if p_better { "PASS" } else { "FAIL" }
    );
    match median {
        Some(m) => {
            let _ = writeln!(
                text,
                "{}  median (c) over blocks {m:.2} °C (needs <= 0.9)",
                if m <= 0.9 { "PASS" } else { "FAIL" }
            );
        }
        None => {
            let _ = writeln!(text, "FAIL  no block has held-out errors");
        }
    }
    // Прежний вид (c) — для сравнения.
    let mut old: Vec<f64> = fitted.blocks.iter().filter_map(|b| b.errors.map(|e| e.proxy)).collect();
    old.sort_by(f64::total_cmp);
    if !old.is_empty() {
        let n = old.len();
        let median = if n % 2 == 1 {
            old[n / 2]
        } else {
            old[n / 2 - 1].midpoint(old[n / 2])
        };
        let p_old = fitted
            .blocks
            .iter()
            .filter(|b| !b.e)
            .all(|b| b.errors.is_some_and(|e| e.proxy < e.die_only));
        let _ = writeln!(
            text,
            "old proxy form (c): median {median:.2} °C; better than (a) for every P-core: {p_old}"
        );
    }
    // Диагностика (не приёмка): то же при β = 1.
    let mut unit: Vec<f64> = fitted.blocks.iter().filter_map(|b| b.errors.map(|e| e.unit)).collect();
    unit.sort_by(f64::total_cmp);
    let unit_better = fitted
        .blocks
        .iter()
        .filter(|b| !b.e)
        .all(|b| b.errors.is_some_and(|e| e.unit < e.die_only));
    if !unit.is_empty() {
        let n = unit.len();
        let median = if n % 2 == 1 {
            unit[n / 2]
        } else {
            unit[n / 2 - 1].midpoint(unit[n / 2])
        };
        let _ = writeln!(
            text,
            "diagnosis (d) beta fixed at 1: median {median:.2} °C; better than (a) for every P-core: {unit_better}"
        );
    }
}

fn split_section(logs: &[&Log], die: &Die, at: &str, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== natural experiment: fit before {at}, predict after (same file) =="
    );
    let Some(t) = samples::row_time(at) else {
        let _ = writeln!(text, "FAILED: split time not understood (want YYYY-MM-DD hh:mm:ss)");
        return;
    };
    let started = Instant::now();
    let result = model::split(logs, die, t.ms);
    let _ = writeln!(
        text,
        "observed and predicted mean of T-D (block above the mean of the other blocks), °C; {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    let show = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |v| format!("{v:+.2}"));
    for b in &result {
        let _ = writeln!(
            text,
            "block {:>5}: train rows {:>6}, test rows {:>5}; observed before {}, after {}; predicted after (a) {} (b) {} (c) {} (c old {}){}; diagnosis: (d) beta=1 {}, fitted beta {}, mean D train {} test {}, (beta-1)*dD {}",
            b.name,
            b.train,
            b.test,
            show(b.before),
            show(b.after),
            show(b.predicted[0]),
            show(b.predicted[1]),
            show(b.predicted[2]),
            show(b.predicted[4]),
            match (b.after, b.predicted[2]) {
                (Some(o), Some(p)) => format!("  error (c) {:+.2}", p - o),
                _ => String::new(),
            },
            match (b.after, b.predicted[3]) {
                (Some(o), Some(p)) => format!("{p:+.2} (error {:+.2})", p - o),
                _ => "-".into(),
            },
            b.beta.map_or_else(|| "-".into(), |v| format!("{v:.3}")),
            b.level_train.map_or_else(|| "-".into(), |v| format!("{v:.2}")),
            b.level_test.map_or_else(|| "-".into(), |v| format!("{v:.2}")),
            match (b.beta, b.level_train, b.level_test) {
                (Some(beta), Some(t), Some(u)) => format!("{:+.2}", (beta - 1.0) * (u - t)),
                _ => "-".into(),
            }
        );
    }
    let _ = writeln!(
        text,
        "parts of the predicted mean after the split: [a + (beta-1)*D, own fast (shape or K*pfast), own history, neighbours, rank] for (c) and (c old); mean own busy and multiplier on the test rows; shared rank of the fit before the split: {}",
        result
            .first()
            .map_or_else(|| "-".into(), |b| format!("{:+.2}, {:+.2}", b.rank[0], b.rank[1]))
    );
    for b in &result {
        let show4 = |p: &[f64; 5]| format!("[{:+.2}, {:+.2}, {:+.2}, {:+.2}, {:+.2}]", p[0], p[1], p[2], p[3], p[4]);
        let _ = writeln!(
            text,
            "block {:>5}: (c) {} (c old) {}; busy {}, x{}",
            b.name,
            show4(&b.parts[0]),
            show4(&b.parts[1]),
            b.busy_test.map_or_else(|| "-".into(), |v| format!("{v:.2}")),
            b.ratio_test.map_or_else(|| "-".into(), |v| format!("{v:.1}"))
        );
    }
    if let Some(b) = result.iter().find(|b| b.name == "0")
        && let (Some(o), Some(p)) = (b.after, b.predicted[2])
    {
        let _ = writeln!(
            text,
            "{}  block 0: predicted {p:+.2} vs observed {o:+.2} after the split (needs within 1 °C)",
            if (p - o).abs() <= 1.0 { "PASS" } else { "FAIL" }
        );
    }
}

fn level_section(logs: &[&Log], die: &Die, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== L4 die level: mean of all blocks from moving averages of CPU (and GPU) power =="
    );
    let started = Instant::now();
    let Some(level) = model::level(logs, die) else {
        let _ = writeln!(
            text,
            "not computed: no gap-free stretch of rows with power_cpu as long as the shortest constant (4 s)"
        );
        return;
    };
    let _ = writeln!(
        text,
        "longest gap-free stretch {:.0} s; constants used {:?} s; warm-up {:.0} s; {:.0} ms",
        level.longest,
        level.taus,
        level.warm,
        started.elapsed().as_secs_f64() * 1000.0
    );
    let show = |name: &str, c: Option<&model::Cross>, text: &mut String| match c {
        Some(c) => {
            let coef: Vec<String> = c.c.iter().map(|v| format!("{v:.3}")).collect();
            let _ = writeln!(
                text,
                "{name}: rows {}, held-out RMSE {:.2} °C per row, {:.2} °C of 10-s means, variance of 10-s means explained {:.0} %; d0 {:.2}, gains [{}]",
                c.rows,
                c.rmse_rows,
                c.rmse_windows,
                c.explained * 100.0,
                c.a,
                coef.join(", ")
            );
        }
        None => {
            let _ = writeln!(text, "{name}: not enough rows after warm-up");
        }
    };
    show("CPU only", level.cpu_only.as_ref(), text);
    show("CPU + GPU", level.with_gpu.as_ref(), text);
}

fn power_section(logs: &[&Log], die: &Die, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== power-proxy check: power_cpu ~ c0 + cP*sum_P p + cE*4*sum_E p (held-out, 4 folds); p = busy*(r/45)^3, assumed, not measured =="
    );
    let check = model::power_check(logs, die);
    let show = |name: &str, c: Option<&model::Cross>, text: &mut String| match c {
        Some(c) => {
            let coef: Vec<String> = c.c.iter().map(|v| format!("{v:.2}")).collect();
            let _ = writeln!(
                text,
                "{name}: rows {}, held-out RMSE {:.2} W per row; c0 {:.2} W, [cP, cE] [{}] W",
                c.rows,
                c.rmse_rows,
                c.a,
                coef.join(", ")
            );
        }
        None => {
            let _ = writeln!(text, "{name}: too few rows");
        }
    };
    show("cube law, all rows", check.cube.as_ref(), text);
}

/// Выученная форма рядом с сырыми корзинами (T − D при занятости ≥ 0,9 по целому множителю) у блоков 0, 10, 22 и 12:
/// модель на опорном D и на D корзины — сколько сырого подъёма объясняет уровень кристалла, а сколько остаётся.
fn shape_tables(logs: &[&Log], die: &Die, fitted: &Model, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== learned shape vs raw bins (T - mean of the other blocks, rows with own busy >= 0.9) =="
    );
    let _ = writeln!(
        text,
        "columns: x | raw T-D, minutes, mean D of the bin | model T-D at the reference D (shaped / old proxy form) | model at the bin's D (shaped) | die part (beta-1)*(D_bin - D_ref) | g(r), slope of g"
    );
    for name in ["0", "10", "22", "12"] {
        let Some(index) = die.names.iter().position(|n| n == name) else {
            continue;
        };
        let b = &fitted.blocks[index];
        let (Some(c), Some(r)) = (&b.shaped, &b.reference) else {
            let _ = writeln!(text, "\nblock {name}: no shaped fit");
            continue;
        };
        let _ = writeln!(
            text,
            "\nblock {name}: beta {:.3}, reference D {:.1} °C, solid range x{:.1}..x{:.1}",
            c.beta, r.die, r.lo, r.hi
        );
        for bin in model::relative_bins(logs, die, index, 0.9) {
            #[expect(clippy::cast_precision_loss, reason = "a multiplier, 0..80")]
            let x = bin.ratio as f64;
            #[expect(clippy::cast_precision_loss, reason = "rows of 0.25 s, far below 2^52")]
            let minutes = bin.rows as f64 * 0.25 / 60.0;
            let (Some(new), Some(old)) = (b.curve(x), b.proxy_curve(x)) else {
                continue;
            };
            let die_part = (c.beta - 1.0) * (bin.level - r.die);
            let _ = writeln!(
                text,
                "  x{:>2} | {:+6.1} {:6.1} min D {:5.1} | {:+6.1} / {:+6.1} | {:+6.1} | {:+5.1} | {:6.2} {:5.2}",
                bin.ratio,
                bin.above,
                minutes,
                bin.level,
                new - r.die,
                old - r.die,
                new - r.die + die_part,
                die_part,
                c.shape.at(x),
                c.shape.slope_at(x + 0.5)
            );
        }
    }
}
