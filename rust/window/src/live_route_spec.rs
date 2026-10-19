// Независимые проверки пути «живая модель → основной график». Случаи заданы до чтения новой отрисовки.
use std::sync::Arc;
use std::time::Duration;

use crate::chart::{Scale, StatsInput, StatsShown, stats_panel};
use crate::conditional::{CurveEstimate, CurveEstimatePoint};
use crate::conditional_live::Options;
use crate::conditional_runtime::{LiveBlock, LiveCurves};
use crate::conditional_snapshot::{BlockCurve, CurvePoint, Snapshot};
use crate::model::{BlockModel, Coef, Model, OwnBin, Rank, Reference, Steps};
use crate::plot::Range;
use crate::store::Digest;
use crate::wire::BlockInfo;

fn blocks() -> Vec<BlockInfo> {
    (0..2)
        .map(|index| BlockInfo {
            index,
            members: vec![index],
            first: index,
            lp: i64::try_from(index).expect("малый номер ядра"),
            kind: "P".into(),
            name: format!("P{index}"),
        })
        .collect()
}

fn estimate(values: &[(f64, Option<f64>, bool)]) -> CurveEstimate {
    CurveEstimate {
        rows: 120,
        mean_busy: 0.6,
        points: values
            .iter()
            .map(|&(ratio, efficiency, within_knots)| CurveEstimatePoint {
                ratio,
                own_heat: efficiency.map_or(0.0, |value| ratio * 0.6 / value),
                performance: ratio * 0.6,
                efficiency,
                within_knots,
            })
            .collect(),
    }
}

fn live_block(name: &str) -> LiveBlock {
    LiveBlock {
        name: name.into(),
        observed: Some(estimate(&[(20.0, Some(3.0), true), (30.0, Some(5.0), true)])),
        sustained: Some(estimate(&[(20.0, Some(7.0), true), (30.0, Some(9.0), true)])),
        status: "Проверка источника: RAW-LIVE-ONLY".into(),
        prepared_rows: 150,
        training_rows: 140,
        eligible_rows: 120,
        first_ms: Some(10_000),
        last_ms: Some(160_000),
        coverage: Vec::new(),
        omitted_contexts: vec!["gpu_temp".into()],
        options: Options::default(),
    }
}

fn live(ready: Vec<LiveBlock>, pending: bool) -> Arc<LiveCurves> {
    Arc::new(LiveCurves {
        generation: 17,
        source_revision: 23,
        pending,
        blocks: ready.into_iter().map(Arc::new).collect(),
    })
}

fn legacy_poison() -> Model {
    let coefficient = Coef {
        a: 30.0,
        beta: 0.0,
        own: [0.0; 7],
        neighbours: Vec::new(),
        shape: Steps {
            bins: vec![20, 30],
            weights: vec![0.2, 0.1],
        },
        rank: [0.0; 2],
    };
    let block = BlockModel {
        name: "P0".into(),
        e: false,
        usable: 10_000,
        minutes: 30.0,
        coef: None,
        shaped: Some(coefficient.clone()),
        folds: vec![coefficient; 4],
        reference: Some(Reference {
            die: 30.0,
            neighbours: Vec::new(),
            lo: 20.0,
            hi: 30.0,
            median: 25.0,
            rows: 10_000,
        }),
        errors: None,
        halves: None,
        diagnosis: None,
        own_bins: [20, 30]
            .into_iter()
            .map(|ratio| OwnBin {
                ratio,
                secs: 1000.0,
                load: 1.0,
                value: 0.25,
            })
            .collect(),
        influence: None,
    };
    assert_eq!(
        block.efficiency().len(),
        2,
        "старый источник действительно умеет дать кривую"
    );
    Model {
        blocks: vec![block],
        rows: 10_000,
        rank: Rank::default(),
        time: Duration::ZERO,
    }
}

fn poisoned() -> Digest {
    Digest {
        model: Some(Arc::new(legacy_poison())),
        conditional: Some(Arc::new(Snapshot {
            version: 1,
            source: "SAVED-POISON".into(),
            conditions: "Старый снимок, не текущие условия".into(),
            blocks: vec![BlockCurve {
                name: "P0".into(),
                points: vec![
                    CurvePoint {
                        ratio: 20.0,
                        heat: 0.1,
                        alternate_heat: 0.1,
                    },
                    CurvePoint {
                        ratio: 30.0,
                        heat: 0.15,
                        alternate_heat: 0.15,
                    },
                ],
                temperature_test_mae: 0.0,
            }],
        })),
        ..Digest::default()
    }
}

fn shown(digest: &Digest, swap: bool) -> StatsShown {
    stats_panel(&StatsInput {
        blocks: &blocks(),
        compared: &[0],
        digest: Some(digest),
        scale: Scale::Efficiency,
        model: true,
        swap,
        argument_range: Range::new(10.0, 40.0),
        value_range: Range::new(0.0, 10.0),
        ..StatsInput::default()
    })
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 0.002, "{actual} != {expected}");
}

fn all_explanations(result: &StatsShown) -> String {
    format!("{} {} {:?}", result.note, result.empty, result.legend)
}

fn path_coordinates(path: &str) -> Vec<(f64, f64)> {
    let numbers: Vec<f64> = path
        .split(|character: char| character.is_ascii_alphabetic() || character.is_whitespace() || character == ',')
        .filter_map(|number| number.parse().ok())
        .collect();
    assert_eq!(numbers.len() % 2, 0, "путь состоит из пар координат: {path}");
    numbers
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

fn painted(series: &crate::chart::SeriesShown) -> bool {
    series.mean.iter().any(|(_, path)| !path.is_empty())
        || !series.model.is_empty()
        || !series.model_dash.is_empty()
        || !series.sensitivity.is_empty()
        || series.p95.iter().any(|(_, path)| !path.is_empty())
}

#[test]
fn main_efficiency_uses_live_coordinates_with_legacy_and_saved_sources_present() {
    let mut digest = poisoned();
    digest.live = Some(live(vec![live_block("P0")], false));
    let result = shown(&digest, false);
    let series = result.series.first().expect("живая линия ядра");
    assert_ne!(series.mean, [], "наблюдённая нагрузка сплошной линией");
    assert_ne!(series.model_dash, "", "полная занятость отдельным пунктиром");
    let full_coordinates = path_coordinates(&series.model_dash);
    assert_ne!(full_coordinates, []);
    assert!(
        full_coordinates
            .iter()
            .all(|&(x, y)| (330.0..=670.0).contains(&x) && (99.0..=301.0).contains(&y)),
        "пунктир содержит значения полной занятости 7…9, а не наблюдённые 3…5: {full_coordinates:?}"
    );
    assert!(
        series.p95.is_empty() && series.sensitivity.is_empty(),
        "режим нагрузки не разброс подгонок"
    );
    let coordinates = path_coordinates(&series.mean[0].1);
    let first = coordinates.first().expect("начало сплошной линии");
    close(first.0 / 1000.0, 1.0 / 3.0);
    close(first.1 / 1000.0, 0.7);
    let last = coordinates.last().expect("конец сплошной линии");
    close(last.0 / 1000.0, 2.0 / 3.0);
    close(last.1 / 1000.0, 0.5);
    assert!(
        all_explanations(&result).contains("RAW-LIVE-ONLY"),
        "показать происхождение результата"
    );
}

#[test]
fn swapped_live_graph_preserves_logical_manual_ranges_and_transposes_only_coordinates() {
    let mut digest = poisoned();
    digest.live = Some(live(vec![live_block("P0")], false));
    let result = shown(&digest, true);
    assert_eq!(result.argument_range, Range::new(10.0, 40.0));
    assert_eq!(result.value_range, Range::new(0.0, 10.0));
    let coordinates = path_coordinates(&result.series[0].mean[0].1);
    let point = coordinates.first().expect("начало сплошной линии");
    close(point.0 / 1000.0, 0.3);
    close(point.1 / 1000.0, 2.0 / 3.0);
}

#[test]
fn absent_live_result_does_not_fall_back_to_old_ready_curves() {
    let result = shown(&poisoned(), false);
    assert!(
        result.series.is_empty(),
        "отсутствие живого результата не разрешает старый источник"
    );
    assert_ne!(result.empty.trim(), "", "объяснение ожидания обязательно");
}

#[test]
fn pending_without_a_live_fit_stays_empty_and_explicit() {
    let mut digest = poisoned();
    digest.live = Some(live(Vec::new(), true));
    let result = shown(&digest, false);
    assert_eq!(result.series, []);
    assert_ne!(result.empty.trim(), "");
}

#[test]
fn failed_current_fit_shows_its_reason_without_any_old_curve() {
    let mut digest = poisoned();
    let mut failed = live_block("P0");
    failed.observed = None;
    failed.sustained = None;
    failed.status = "LIVE-FAIL: нет измеренной загрузки".into();
    digest.live = Some(live(vec![failed], false));
    let result = shown(&digest, false);
    assert!(
        result.series.iter().all(|series| !painted(series)),
        "подмена неуспешного расчёта прежним запрещена"
    );
    assert!(all_explanations(&result).contains("LIVE-FAIL"));
}

#[test]
fn unsupported_block_does_not_hide_another_supported_block() {
    let mut digest = poisoned();
    let mut failed = live_block("P1");
    failed.observed = None;
    failed.sustained = None;
    failed.status = "ONLY-P1-FAIL".into();
    digest.live = Some(live(vec![live_block("P0"), failed], false));
    let result = stats_panel(&StatsInput {
        blocks: &blocks(),
        compared: &[0, 1],
        digest: Some(&digest),
        scale: Scale::Efficiency,
        ..StatsInput::default()
    });
    assert!(result.series.iter().any(|series| series.block == 0));
    assert!(
        result
            .series
            .iter()
            .filter(|series| series.block == 1)
            .all(|series| !painted(series))
    );
    assert!(all_explanations(&result).contains("ONLY-P1-FAIL"));
}

#[test]
fn unsupported_or_zero_heat_points_split_the_actual_path_without_a_bridge() {
    let mut digest = poisoned();
    let mut block = live_block("P0");
    block.observed = Some(estimate(&[
        (20.0, Some(3.0), true),
        (22.0, Some(4.0), true),
        (24.0, Some(5.0), false),
        (26.0, Some(6.0), true),
        (28.0, Some(7.0), true),
        (30.0, None, true),
        (32.0, Some(8.0), true),
        (34.0, Some(9.0), true),
    ]));
    block.sustained = None;
    digest.live = Some(live(vec![block], false));
    let result = shown(&digest, false);
    let series = result.series.first().expect("поддержанные отрезки остаются");
    let starts = series
        .mean
        .iter()
        .map(|(_, path)| path.matches('M').count())
        .sum::<usize>();
    assert_eq!(
        starts, 3,
        "три независимых отрезка, пропуски не соединяются: {:?}",
        series.mean
    );
}

#[test]
fn explicit_saved_snapshot_view_is_preserved_as_a_separate_source() {
    let digest = poisoned();
    let result = stats_panel(&StatsInput {
        blocks: &blocks(),
        compared: &[0],
        digest: Some(&digest),
        scale: Scale::ConditionalEfficiency,
        ..StatsInput::default()
    });
    assert!(!result.series.is_empty(), "пользовательский просмотр снимка сохранён");
    assert!(all_explanations(&result).contains("SAVED-POISON"));
}

#[test]
fn a_runtime_or_source_failure_is_not_disguised_as_a_fit_still_in_progress() {
    let mut digest = poisoned();
    digest.trouble = Some("SOURCE-ERROR: вычислительный поток завершился".into());
    let failed = shown(&digest, false);
    assert!(failed.series.iter().all(|series| !painted(series)));
    assert!(
        all_explanations(&failed).contains("SOURCE-ERROR"),
        "причина отказа видна на основном графике"
    );
    digest.live = Some(live(vec![live_block("P0")], true));
    let old_but_explicit = shown(&digest, false);
    assert!(
        all_explanations(&old_but_explicit).contains("SOURCE-ERROR"),
        "сохранённая оценка не скрывает отказ источника"
    );
}

#[test]
fn manual_axis_ranges_do_not_hide_the_reason_for_unknown_live_efficiency() {
    let mut digest = poisoned();
    let mut block = live_block("P0");
    block.observed = Some(estimate(&[(20.0, None, true), (30.0, None, true)]));
    block.sustained = None;
    digest.live = Some(live(vec![block], false));
    let result = shown(&digest, false);
    assert!(result.series.iter().all(|series| !painted(series)));
    assert_ne!(
        result.empty.trim(),
        "",
        "ручные границы не превращают неизвестный результат в молчаливый пустой график"
    );
}
