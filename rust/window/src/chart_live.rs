// Основной график получает только результаты живой условной модели.
use super::{
    ColumnShown, Frame, LegendShown, Scale, SeriesShown, StatsInput, StatsShown, clipped, color_of, dashed_path,
    fill_ticks, resolve_frame, solid_path,
};
use crate::conditional::{CurveEstimate, CurveEstimatePoint};
use crate::conditional_runtime::LiveBlock;
use crate::panels::f;
use crate::samples;
use crate::wire::BlockInfo;
use std::fmt::Write as _;

type Curve<'a> = (&'a BlockInfo, &'a LiveBlock);

fn value(point: &CurveEstimatePoint) -> Option<f64> {
    point
        .efficiency
        .filter(|v| point.within_knots && v.is_finite() && *v >= 0.0)
}

fn visible_points(curve: &CurveEstimate) -> impl Iterator<Item = &CurveEstimatePoint> {
    curve.points.iter().filter(|p| value(p).is_some())
}

pub(super) fn panel(input: &StatsInput, compared: &[&BlockInfo], shown: &mut StatsShown) {
    shown.value_title = "множитель × занятость / собственный нагрев, 1/°C".into();
    let trouble = input.digest.and_then(|d| d.trouble.as_deref());
    let Some(live) = input.digest.and_then(|d| d.live.as_deref()) else {
        shown.empty = trouble.map_or_else(
            || "Условная модель ещё рассчитывается по журналу измерений…".into(),
            |reason| format!("Условная модель недоступна: {reason}"),
        );
        return;
    };
    let latest = live.blocks.iter().filter_map(|b| b.last_ms).max();
    shown.note = format!(
        "Живой расчёт · журнал до {} · {} · сплошная: записанная история нагрузки; «100 %» добавляет пунктир полной нагрузки · это оценки модели, не измеренная производительность",
        latest.map_or_else(|| "неизвестно".into(), samples::day_time),
        if live.pending {
            "пересчёт продолжается"
        } else {
            "пересчёт завершён"
        }
    );
    if let Some(reason) = trouble {
        let _ = write!(shown.note, " · ошибка: {reason}"); // Запись в String не отказывает.
    }
    let mut curves = Vec::new();
    for block in compared {
        let result = live.blocks.iter().find(|r| r.name == block.name);
        shown.legend.push(LegendShown {
            block: block.index,
            name: block.name.clone(),
            color: color_of(input.blocks, block.index),
            tip: result.map_or_else(|| "Ожидает расчёта".into(), |r| details(r)),
        });
        if let Some(result) = result {
            curves.push((*block, &**result));
        }
    }
    if curves.is_empty() {
        shown.empty = if compared.is_empty() {
            "Выбери ядра для сравнения условных кривых".into()
        } else {
            "Выбранные ядра ожидают расчёта условной модели".into()
        };
        return;
    }
    let points = curves.iter().flat_map(|(_, r)| {
        r.observed
            .iter()
            .chain(input.model.then_some(&r.sustained).into_iter().flatten())
    });
    let (mut ratios, mut values) = (Vec::new(), Vec::new());
    for estimate in points {
        for point in visible_points(estimate) {
            if (0.0..=255.0).contains(&point.ratio) {
                ratios.push(point.ratio);
                values.push(value(point).expect("отобрана известная эффективность"));
            }
        }
    }
    let Some(frame) = resolve_frame(input, &ratios, &values, shown) else {
        shown.empty = empty_reason(&curves);
        return;
    };
    fill_ticks(&frame, Scale::Efficiency, shown);
    curves.sort_by_key(|(b, _)| input.highlight == Some(b.index));
    for (block, result) in &curves {
        let mut series = SeriesShown {
            block: block.index,
            color: color_of(input.blocks, block.index),
            faded: input.highlight.is_some_and(|b| b != block.index),
            ..SeriesShown::default()
        };
        if let Some(estimate) = &result.observed {
            for run in runs(estimate, &frame) {
                let mut path = String::new();
                solid_path(&run, &frame, &mut path);
                series.mean.push((1.0, path));
                series.model_line.push(
                    run.iter()
                        .map(|&(x, y)| {
                            let (x, y) = frame.turn(x, y);
                            (f(x), f(y))
                        })
                        .collect(),
                );
            }
        }
        if input.model
            && let Some(estimate) = &result.sustained
        {
            for run in runs(estimate, &frame) {
                dashed_path(&run, &frame, &mut series.model_dash);
            }
        }
        cuts(input, block, result, &frame, &mut series);
        shown.series.push(series);
    }
    columns(&frame, &curves, shown);
}

fn empty_reason(curves: &[Curve<'_>]) -> String {
    curves
        .iter()
        .map(|(b, r)| {
            let reason = if r.observed.is_some() {
                "эффективность неизвестна: собственный нагрев не положителен либо множитель вне базиса"
            } else {
                &r.status
            };
            format!("{}: {reason}", b.name)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn cuts(input: &StatsInput, block: &BlockInfo, result: &LiveBlock, frame: &Frame, series: &mut SeriesShown) {
    let Some(digest) = input.digest else { return };
    let Some(curve) = &result.observed else { return };
    for event in digest
        .events
        .iter()
        .filter(|e| e.kind.cuts() && e.block.as_deref() == Some(block.name.as_str()))
    {
        let Some(ratio) = event.ratio else { continue };
        let Some((efficiency, _)) = at(curve, ratio) else {
            continue;
        };
        let (x, y) = frame.turn(frame.along_at(ratio), frame.value(efficiency));
        if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
            series.cuts.push(super::CutShown {
                x: f(x),
                y: f(y),
                kind: if event.kind == super::Kind::Target { 2 } else { 1 },
            });
        }
    }
}

fn runs(estimate: &CurveEstimate, frame: &Frame) -> Vec<Vec<(f64, f64)>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    for point in &estimate.points {
        if let Some(v) = value(point) {
            current.push((frame.along_at(point.ratio), frame.value(v)));
        } else {
            result.extend(clipped(&current));
            current.clear();
        }
    }
    result.extend(clipped(&current));
    result
}

fn details(block: &LiveBlock) -> String {
    let unknown = if block.omitted_contexts.is_empty() {
        "все доступные изменявшиеся факторы включены".into()
    } else {
        format!("не определены: {}", block.omitted_contexts.join("; "))
    };
    let coverage = block
        .coverage
        .iter()
        .map(|c| format!("{}: {}/{}", c.name, c.known_rows, c.total_rows))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "{}\n{}\n• исходных секунд {}, пригодных {}, обучающих {}\n• история {} с — предварительный кандидат, не измеренная инерция ноутбука\n• исходный период {} — {}; неполные состояния не участвуют в расчёте\n• {}\n• известные показания: {}\n• нулевой собственный нагрев означает неизвестную эффективность; вне узлов базиса линия не продолжается",
        block.name,
        block.status,
        block.prepared_rows,
        block.eligible_rows,
        block.training_rows,
        block.options.horizon_seconds,
        block.first_ms.map_or_else(|| "неизвестно".into(), samples::day_time),
        block.last_ms.map_or_else(|| "неизвестно".into(), samples::day_time),
        unknown,
        coverage
    )
}

fn at(estimate: &CurveEstimate, ratio: f64) -> Option<(f64, f64)> {
    if let Some(p) = estimate.points.iter().find(|p| (p.ratio - ratio).abs() < 1e-9) {
        return Some((value(p)?, p.own_heat));
    }
    let pair = estimate
        .points
        .windows(2)
        .find(|p| p[0].ratio < ratio && ratio < p[1].ratio)?;
    let (a, b) = (&pair[0], &pair[1]);
    let k = (ratio - a.ratio) / (b.ratio - a.ratio);
    Some((
        value(a)? + k * (value(b)? - value(a)?),
        a.own_heat + k * (b.own_heat - a.own_heat),
    ))
}

fn columns(frame: &Frame, curves: &[Curve<'_>], shown: &mut StatsShown) {
    for ratio in (frame.lo..=frame.hi).filter(|r| (0.0..=1.0).contains(&frame.along(*r))) {
        let mut lines = vec![format!("×{ratio} · условная модель по журналу")];
        for (block, result) in curves {
            for (label, estimate) in [
                ("записанная нагрузка", &result.observed),
                ("постоянные 100 %", &result.sustained),
            ] {
                let text = estimate.as_ref().and_then(|e| at(e, f64::from(ratio))).map_or_else(
                    || "неизвестно / вне области базиса".into(),
                    |(eff, heat)| format!("η {eff:.2} 1/°C; свой нагрев {heat:.2} °C"),
                );
                lines.push(format!("Ядро {} · {label}: {text}", block.name));
            }
        }
        shown.columns.push(ColumnShown {
            ratio,
            at: f(frame.along(ratio)),
            tip: lines.join("\n"),
        });
    }
}
