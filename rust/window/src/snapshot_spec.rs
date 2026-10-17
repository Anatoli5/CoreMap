// Независимые проверки по заданию владельца: готовые экспериментальные кривые в окне,
// переключение осей и честное объяснение полосы; без ожидания новой подгонки.
// Подключить как тестовый модуль crate; здесь только публичный контракт, без чтения реализации.
use std::sync::Arc;

use crate::chart::{self, Curves, Scale, StatsInput, StatsShown};
use crate::conditional_snapshot::Snapshot;
use crate::store::Digest;
use crate::wire::BlockInfo;

const FIXTURE: &str = r#"{
  "version": 1,
  "source": "independent synthetic fixture",
  "conditions": "same specified conditions; two fits",
  "blocks": [
    {"name":"0","temperature_test_mae":0.4,"points":[
      {"ratio":20.0,"heat":10.0,"alternate_heat":2.0},
      {"ratio":30.0,"heat":15.0,"alternate_heat":2.0},
      {"ratio":40.0,"heat":20.0,"alternate_heat":2.0}
    ]},
    {"name":"13","temperature_test_mae":0.8,"points":[
      {"ratio":25.0,"heat":25.0,"alternate_heat":60.0},
      {"ratio":45.0,"heat":45.0,"alternate_heat":80.0}
    ]}
  ]
}"#;

fn snapshot() -> Snapshot {
    serde_json::from_str(FIXTURE).expect("корректный независимый JSON")
}

fn blocks() -> Vec<BlockInfo> {
    ["0", "13", "22"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| BlockInfo {
            index,
            members: vec![index],
            first: index,
            lp: 0,
            kind: "P".into(),
            name: name.into(),
        })
        .collect()
}

fn shown(scale: Scale, swap: bool, model: bool, compared: &[usize]) -> StatsShown {
    let digest = Digest {
        conditional: Some(Arc::new(snapshot())),
        ..Digest::default()
    };
    chart::stats_panel(&StatsInput {
        blocks: &blocks(),
        compared,
        digest: Some(&digest),
        scale,
        curves: Curves::Both,
        model,
        swap,
        ..StatsInput::default()
    })
}

fn close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.000_02, "{actual} != {expected}");
}

// Не привязываемся к плотности отрисовки или числу отрезков SVG.
fn central(shown: &StatsShown, block: usize) -> Vec<(f32, f32)> {
    shown
        .series
        .iter()
        .find(|s| s.block == block)
        .expect("ряд блока")
        .model_line
        .iter()
        .flatten()
        .copied()
        .collect()
}

fn path_coordinates(path: &str) -> Vec<f32> {
    path.split(|c: char| c.is_ascii_alphabetic() || c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<f32>().expect("числовая координата SVG"))
        .collect()
}

#[test]
fn snapshot_accepts_distinct_block_grids_and_zero_mae() {
    let mut s = snapshot();
    s.blocks[0].temperature_test_mae = 0.0;
    assert!(s.validate().is_ok());
}

#[test]
fn snapshot_rejects_bad_version_empty_sets_duplicate_names_and_ratios() {
    for case in 0..7 {
        let mut s = snapshot();
        match case {
            0 => s.version = 0,
            1 => s.version = 2,
            2 => s.blocks.clear(),
            3 => s.blocks[0].points.clear(),
            4 => s.blocks[1].name = "0".into(),
            5 => s.blocks[0].points[1].ratio = 20.0,
            6 => s.blocks[0].points[1].ratio = 19.0,
            _ => unreachable!(),
        }
        assert!(s.validate().is_err(), "некорректный случай {case}");
    }
}

#[test]
fn snapshot_rejects_nonpositive_or_nonfinite_curve_values_and_bad_mae() {
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for field in 0..3 {
            let mut s = snapshot();
            let p = &mut s.blocks[0].points[0];
            match field {
                0 => p.ratio = bad,
                1 => p.heat = bad,
                2 => p.alternate_heat = bad,
                _ => unreachable!(),
            }
            assert!(s.validate().is_err(), "поле {field}, значение {bad}");
        }
    }
    for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut s = snapshot();
        s.blocks[0].temperature_test_mae = bad;
        assert!(s.validate().is_err(), "MAE {bad}");
    }
}

#[test]
fn snapshot_rejects_malformed_json_without_panicking() {
    for bad in ["", "{", "null", "[]", "{}", r#"{"version":"1"}"#] {
        assert!(serde_json::from_str::<Snapshot>(bad).is_err());
    }
    let missing_heat = FIXTURE.replacen("\"heat\":10.0,", "", 1);
    assert!(serde_json::from_str::<Snapshot>(&missing_heat).is_err());
}

#[test]
fn snapshot_scales_have_stable_distinct_indices() {
    assert_eq!(Scale::ConditionalEfficiency.index(), 3);
    assert_eq!(Scale::ConditionalHeat.index(), 4);
    assert_eq!(Scale::of(3), Scale::ConditionalEfficiency);
    assert_eq!(Scale::of(4), Scale::ConditionalHeat);
}

#[test]
fn snapshot_efficiency_is_ratio_over_heat_and_heat_stays_heat() {
    let efficiency = shown(Scale::ConditionalEfficiency, false, true, &[0, 1]);
    let heat = shown(Scale::ConditionalHeat, false, true, &[0, 1]);
    for block in [0, 1] {
        let eta = central(&efficiency, block);
        let thermal = central(&heat, block);
        assert!(eta.len() >= 2 && thermal.len() >= 2);
        // r/H постоянно (2 у первого блока, 1 у второго), а H растёт.
        for p in &eta {
            close(p.1, eta[0].1);
        }
        assert!(thermal.last().expect("последняя точка").1 > thermal[0].1);
    }
    // Производительность на градус выше у первого блока; обратная метрика провалит проверку.
    assert!(central(&efficiency, 0)[0].1 > central(&efficiency, 1)[0].1);
}

#[test]
fn snapshot_swap_transposes_both_modes_without_changing_data() {
    for scale in [Scale::ConditionalEfficiency, Scale::ConditionalHeat] {
        let normal = shown(scale, false, true, &[0, 1]);
        let swapped = shown(scale, true, true, &[0, 1]);
        for block in [0, 1] {
            let a = central(&normal, block);
            let b = central(&swapped, block);
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(&b) {
                close(a.0, b.1);
                close(a.1, b.0);
            }
        }
    }
}

#[test]
fn snapshot_draws_both_fits_inside_frame_and_never_extrapolates_support() {
    for scale in [Scale::ConditionalEfficiency, Scale::ConditionalHeat] {
        let s = shown(scale, false, true, &[0, 1]);
        for (block, low, high) in [(0, 20, 40), (1, 25, 45)] {
            let series = s.series.iter().find(|line| line.block == block).expect("ряд");
            assert!(!series.model.is_empty() && !series.model_dash.is_empty());
            for path in [&series.model, &series.model_dash] {
                let xy = path_coordinates(path);
                assert!(!xy.is_empty() && xy.len().is_multiple_of(2));
                assert!(xy.iter().all(|v| v.is_finite() && *v >= -0.02 && *v <= 1000.02));
            }
            let points = central(&s, block);
            let left = s.columns.iter().find(|c| c.ratio == low).expect("начало диапазона").at;
            let right = s.columns.iter().find(|c| c.ratio == high).expect("конец диапазона").at;
            close(points.first().expect("начальная точка").0, left);
            close(points.last().expect("конечная точка").0, right);
            assert!(points.iter().all(|p| p.0 >= left - 0.000_02 && p.0 <= right + 0.000_02));
        }
    }
}

#[test]
fn snapshot_missing_block_has_no_borrowed_curve_and_has_explanation() {
    let s = shown(Scale::ConditionalEfficiency, false, true, &[2]);
    assert!(
        s.series
            .iter()
            .all(|line| line.model.is_empty() && line.model_dash.is_empty() && line.model_line.is_empty())
    );
    let text = format!(
        "{} {} {}",
        s.note,
        s.empty,
        s.legend.iter().map(|l| l.tip.as_str()).collect::<Vec<_>>().join(" ")
    );
    assert!(text.contains("нет") || text.contains("Нет"), "{text}");
}

#[test]
fn snapshot_model_switch_hides_both_fits_and_hover_geometry() {
    let s = shown(Scale::ConditionalEfficiency, false, false, &[0, 1]);
    assert!(
        s.series
            .iter()
            .all(|line| line.model.is_empty() && line.model_dash.is_empty() && line.model_line.is_empty())
    );
}

#[test]
fn snapshot_hover_and_legend_explain_saved_experiment_and_sensitivity() {
    let s = shown(Scale::ConditionalEfficiency, false, true, &[0, 1]);
    assert!(s.columns.iter().any(|column| !column.tip.is_empty()));
    let text = format!(
        "{} {}",
        s.note,
        s.legend.iter().map(|l| l.tip.as_str()).collect::<Vec<_>>().join(" ")
    )
    .to_lowercase();
    assert!(text.contains("эксперимент"), "{text}");
    assert!(text.contains("подгон") || text.contains("чувствитель"), "{text}");
    assert!(
        text.contains("не доверитель") || text.contains("не является доверитель"),
        "{text}"
    );
}
