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
        "files with 0.25-s rows: {}, rows kept: {rows}, memory of kept rows: {:.1} MB, reading and parsing: {:.0} ms",
        logs.len(),
        mb(bytes),
        read_time.as_secs_f64() * 1000.0
    );
    let fitted = model::fit(&logs, &die);
    let _ = writeln!(
        text,
        "fit time (L3, all blocks, 4 folds, halves, two passes): {:.0} ms",
        fitted.time.as_secs_f64() * 1000.0
    );
    let _ = writeln!(text);
    blocks(&fitted, &die, text);
    acceptance(&fitted, text);
    split_power_section(&fitted, text);
    equal_load_section(&fitted, text);
    if let Some(at) = split {
        split_section(&logs, &die, at, text);
    }
    level_section(&logs, &die, text);
    power_section(&logs, &die, &fitted, text);
    0
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
        "errors: held-out RMSE of 10-s means of T-D, 4 contiguous folds; (a) offset + beta*D, (b) + own, (c) + neighbours"
    );
    for (i, b) in fitted.blocks.iter().enumerate() {
        let near: Vec<&str> = die.neighbours[i].iter().map(|&j| die.names[j].as_str()).collect();
        let _ = writeln!(
            text,
            "\nblock {} ({}): usable rows {} ({:.1} min), cube-law fallback {:.0} %, neighbours {}",
            b.name,
            if b.e { "E-cluster" } else { "P" },
            b.usable,
            b.minutes,
            b.fallback * 100.0,
            near.join(", ")
        );
        coef_lines(b, die, text);
        physics_lines(b, die, text);
    }
}

/// Коэффициенты блока и ошибки на отложенных данных.
fn coef_lines(b: &model::BlockModel, die: &Die, text: &mut String) {
    match &b.coef {
        Some(c) => {
            let _ = writeln!(
                text,
                "  coef: a {:.2}, beta {:.3}, own [fast {:.2}; ema 0.5s {:.2}, 1s {:.2}, 2s {:.2}, 4s {:.2}, 8s {:.2}, 16s {:.2}] total {:.2}",
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
            "  held-out: (a) {:.2}  (b) {:.2}  (c) {:.2} °C over {} windows; diagnosis (d) = (c) with beta fixed at 1: {:.2}",
            e.die_only, e.own, e.full, e.windows, e.unit
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
            "  diagnosis, unconstrained (c): beta {:.3}, own total {:+.2}, neighbours fast/4-s [{}]",
            d.signed.beta,
            d.signed.own_total(),
            near(&d.signed)
        );
        let _ = writeln!(
            text,
            "  diagnosis, beta = 1 (d): own total {:.2}, neighbours fast/4-s [{}]",
            d.unit.own_total(),
            near(&d.unit)
        );
    }
}

/// Опорные условия, кривая, «°C на ступень», стабильность, напряжение.
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
            .filter_map(|&x| Some(format!("x{x:.1} {:.1}", b.curve(x)?)))
            .collect();
        let _ = writeln!(text, "  curve (absolute °C): {}", curve.join(", "));
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
            let _ = writeln!(text, "  own gain total, max/min between data halves: {h:.2}");
        }
        None => {
            let _ = writeln!(
                text,
                "  own gain total between halves: not comparable (a half has none)"
            );
        }
    }
    match &b.volts {
        Some(v) => {
            let (lo, hi) = v.range();
            let _ = writeln!(
                text,
                "  V(r): observed x{lo:.1}..x{hi:.1}: {:.3} V at lo, {:.3} V at x45, {:.3} V at hi",
                v.at(lo),
                v.at(model::REFERENCE),
                v.at(hi)
            );
        }
        None => {
            let _ = writeln!(
                text,
                "  V(r): none (no rows with volts and busy >= 0.5 over two whole multipliers)"
            );
        }
    }
    if let Some(r) = b.rail {
        let _ = writeln!(
            text,
            "  voltage vs other blocks of its kind r = {:.2}, vs own multiplier r = {:.2} ({} rows){}",
            r.with_others,
            r.with_ratio,
            r.rows,
            if r.with_others > r.with_ratio {
                "  FINDING: follows the others more than its own multiplier (shared rail?)"
            } else {
                ""
            }
        );
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
            "block {:>5}: train rows {:>6}, test rows {:>5}; observed before {}, after {}; predicted after (a) {} (b) {} (c) {}{}; diagnosis: (d) beta=1 {}, fitted beta {}, mean D train {} test {}, (beta-1)*dD {}",
            b.name,
            b.train,
            b.test,
            show(b.before),
            show(b.after),
            show(b.predicted[0]),
            show(b.predicted[1]),
            show(b.predicted[2]),
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

fn power_section(logs: &[&Log], die: &Die, fitted: &Model, text: &mut String) {
    let _ = writeln!(
        text,
        "\n== power-proxy check: power_cpu ~ c0 + cP*sum_P p + cE*4*sum_E p (held-out, 4 folds) =="
    );
    let check = model::power_check(logs, die, fitted);
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
    match &check.both {
        Some((cube, volts)) => {
            show("cube law, rows with logged voltage", cube.as_ref(), text);
            show("logged voltage, same rows", volts.as_ref(), text);
        }
        None => {
            let _ = writeln!(
                text,
                "logged voltage: no rows where every block with a multiplier has a voltage (and a V(r) curve)"
            );
        }
    }
}
