// Независимые случаи подключения живой модели; реализация адаптера не читалась.
use crate::conditional::CurveLoad;
use crate::conditional_live::{Options, fit_block};
use crate::model::{Die, Log};

fn topology(count: usize) -> Die {
    Die {
        names: (0..count).map(|i| format!("P{i}")).collect(),
        e: vec![false; count],
        neighbours: vec![Vec::new(); count],
    }
}

fn options(horizon_seconds: i64) -> Options {
    Options {
        horizon_seconds,
        ridge: 0.002,
        max_training_rows: 4096,
    }
}

fn samples(die: &Die, count: usize, gain: f64, fan: bool, start: i64) -> Log {
    let mut log = Log::new(die.names.clone());
    for i in 0..count {
        let ratio = [20.0, 25.0, 30.0, 35.0][i % 4];
        let busy = [0.3, 0.6, 0.9][(i / 4) % 3];
        let rpm = [1200.0, 2400.0, 3600.0][(i / 12) % 3];
        let heat = gain * ratio * busy * (1.0 - rpm / 12_000.0);
        let mut shared = [None; 8];
        shared[4] = fan.then_some(rpm);
        let cells = (0..die.names.len()).map(|_| [Some(ratio), Some(busy), Some(30.0 + heat), Some(40.0)]);
        log.push_with_environment(start + i64::try_from(i).expect("малый пример") * 1000, cells, shared);
    }
    log
}

#[test]
fn no_source_measurements_cannot_yield_a_live_curve() {
    let die = topology(1);
    let empty = Log::new(die.names.clone());
    let result = fit_block(&[&empty], &die, 0, options(0));
    assert!(result.model.is_none(), "без наблюдений модель неизвестна");
    assert_eq!(result.prepared_rows, 0);
    assert_ne!(result.status.trim(), "", "нужна причина отсутствия");
}

#[test]
fn paired_live_query_preserves_both_separate_curves() {
    let die = topology(1);
    let log = samples(&die, 180, 0.7, true, 0);
    let fit = fit_block(&[&log], &die, 0, options(0));
    let model = fit.model.expect("модель известной зависимости");
    let pair = model.query_pair().expect("две линии живого графика");
    for (actual, load) in pair.iter().zip([CurveLoad::ObservedHistory, CurveLoad::SustainedFull]) {
        let single = model.query(load).expect("отдельная линия");
        assert_eq!(
            serde_json::to_value(actual).expect("парная линия"),
            serde_json::to_value(single).expect("отдельная линия")
        );
    }
}

#[test]
fn absent_own_busy_is_not_reconstructed_as_full_or_idle() {
    let die = topology(1);
    let mut log = Log::new(die.names.clone());
    for i in 0..180 {
        log.push_with_environment(
            i * 1000,
            [[Some(25.0), None, Some(50.0), Some(40.0)]].into_iter(),
            [None; 8],
        );
    }
    let result = fit_block(&[&log], &die, 0, options(0));
    assert!(result.model.is_none(), "неизвестная загрузка не измерение");
    assert_ne!(result.status.trim(), "");
}

#[test]
fn unavailable_gpu_and_fan_are_explicitly_omitted_not_zero_filled() {
    let die = topology(1);
    let log = samples(&die, 180, 0.7, false, 0);
    let result = fit_block(&[&log], &die, 0, options(0));
    let missing_gpu: Vec<_> = result
        .coverage
        .iter()
        .filter(|item| item.name.to_ascii_lowercase().contains("gpu"))
        .collect();
    assert!(!missing_gpu.is_empty(), "видна полнота данных видеокарты");
    assert!(missing_gpu.iter().all(|item| item.known_rows == 0));
    assert!(missing_gpu.iter().all(|item| item.total_rows > 0));
    assert!(
        result.omitted_contexts.iter().any(|name| name.contains("gpu")),
        "отсутствующий фактор нельзя выдать за включённый"
    );
}

#[test]
fn zero_gpu_power_is_distinct_from_an_absent_measurement() {
    let die = topology(1);
    let mut known = Log::new(die.names.clone());
    let mut absent = Log::new(die.names.clone());
    for i in 0..180 {
        let ratio = [20.0, 25.0, 30.0][usize::try_from(i % 3).expect("индекс")];
        let cells = [[Some(ratio), Some(0.6), Some(40.0 + ratio), Some(40.0)]];
        let mut zero = [None; 8];
        zero[1] = Some(0.0);
        known.push_with_environment(i * 1000, cells.into_iter(), zero);
        absent.push_with_environment(i * 1000, cells.into_iter(), [None; 8]);
    }
    let with_zero = fit_block(&[&known], &die, 0, options(0));
    let without = fit_block(&[&absent], &die, 0, options(0));
    assert!(
        with_zero
            .coverage
            .iter()
            .zip(&without.coverage)
            .any(|(a, b)| { a.name == b.name && a.known_rows > b.known_rows && b.known_rows == 0 }),
        "явный ноль сохраняет известность, пропуск остаётся неизвестным"
    );
}

#[test]
fn historical_rows_survive_reprocessing_with_a_newer_schema() {
    let die = topology(1);
    let old = samples(&die, 180, 0.7, false, 0);
    let new = samples(&die, 180, 0.7, true, 180_000);
    let old_only = fit_block(&[&old], &die, 0, options(0));
    let new_only = fit_block(&[&new], &die, 0, options(0));
    let combined = fit_block(&[&old, &new], &die, 0, options(0));
    assert_eq!(
        combined.prepared_rows,
        old_only.prepared_rows + new_only.prepared_rows,
        "смена схемы не удаляет исходные состояния из подготовки"
    );
    assert_eq!(combined.first_ms, old_only.first_ms);
    assert_eq!(combined.last_ms, new_only.last_ms);
}

#[test]
fn fitting_actual_changed_source_changes_the_conditional_curve() {
    let die = topology(1);
    let cool = samples(&die, 216, 0.45, true, 0);
    let hot = samples(&die, 216, 1.2, true, 216_000);
    let first = fit_block(&[&cool], &die, 0, options(0));
    let appended = fit_block(&[&cool, &hot], &die, 0, options(0));
    assert!(appended.prepared_rows > first.prepared_rows);
    let curve_first = first
        .model
        .expect("разнообразный первый режим")
        .query_ratios(&[25.0], CurveLoad::SustainedFull)
        .expect("первый запрос");
    let curve_appended = appended
        .model
        .expect("разнообразные новые измерения")
        .query_ratios(&[25.0], CurveLoad::SustainedFull)
        .expect("обновлённый запрос");
    assert!(
        (curve_first.points[0].own_heat - curve_appended.points[0].own_heat).abs() > 0.01,
        "новое независимое тепловое поведение обязано изменить расчёт"
    );
}

#[test]
fn live_query_preserves_one_donor_cohort_in_both_load_modes() {
    let die = topology(1);
    let log = samples(&die, 216, 0.7, true, 0);
    let result = fit_block(&[&log], &die, 0, options(0));
    let fitted = result.model.expect("разнообразный режим");
    let observed = fitted
        .query_ratios(&[25.0], CurveLoad::ObservedHistory)
        .expect("наблюдаемая загрузка");
    let full = fitted
        .query_ratios(&[25.0], CurveLoad::SustainedFull)
        .expect("полная загрузка");
    let quarter = fitted
        .query_ratios(&[25.0], CurveLoad::Sustained(0.25))
        .expect("выбранная загрузка");
    assert_eq!(observed.rows, full.rows);
    assert_eq!(observed.rows, quarter.rows);
    assert!(observed.mean_busy > 0.3 && observed.mean_busy < 0.9);
    assert_eq!(full.mean_busy, 1.0);
    assert_eq!(quarter.mean_busy, 0.25);
}

#[test]
fn query_without_explicit_extrapolation_stays_inside_observed_ratio_domain() {
    let die = topology(1);
    let log = samples(&die, 216, 0.7, true, 0);
    let result = fit_block(&[&log], &die, 0, options(0));
    let curve = result
        .model
        .expect("разнообразный режим")
        .query(CurveLoad::ObservedHistory)
        .expect("штатная кривая");
    assert!(!curve.points.is_empty());
    assert!(
        curve
            .points
            .iter()
            .all(|point| point.ratio >= 20.0 && point.ratio <= 35.0)
    );
}

#[test]
fn a_physical_gap_is_not_converted_to_contiguous_history() {
    let die = topology(1);
    let before = samples(&die, 108, 0.7, true, 0);
    let after = samples(&die, 108, 0.7, true, 1_000_000);
    let result = fit_block(&[&before, &after], &die, 0, options(2));
    let fitted = result.model.expect("два достаточно длинных режима");
    assert!(fitted.data.segments.windows(2).any(|pair| pair[0] != pair[1]));
    let mut saw_gap = false;
    for i in 1..fitted.data.times.len() {
        if fitted.data.times[i] - fitted.data.times[i - 1] > 100_000 {
            saw_gap = true;
            assert_ne!(fitted.data.segments[i], fitted.data.segments[i - 1]);
        }
    }
    assert!(saw_gap, "исходный разрыв виден в физическом времени");
}

#[test]
fn unsupported_block_does_not_supply_another_blocks_curve() {
    let die = topology(2);
    let mut log = Log::new(die.names.clone());
    for i in 0..216 {
        let ratio = [20.0, 25.0, 30.0, 35.0][usize::try_from(i % 4).expect("индекс")];
        let busy = [0.3, 0.6, 0.9][usize::try_from((i / 4) % 3).expect("индекс")];
        log.push_with_environment(
            i * 1000,
            [
                [Some(ratio), Some(busy), Some(30.0 + ratio * busy), Some(40.0)],
                [Some(ratio), Some(busy), None, Some(40.0)],
            ]
            .into_iter(),
            [None; 8],
        );
    }
    let good = fit_block(&[&log], &die, 0, options(0));
    let unknown = fit_block(&[&log], &die, 1, options(0));
    assert!(good.model.is_some(), "полное ядро продолжает считаться");
    assert!(unknown.model.is_none(), "чужая кривая не заменяет неизвестную");
    assert_ne!(unknown.status.trim(), "");
    assert_ne!(good.name, unknown.name);
}

// Редкий верхний режим находится внутри эпизода, а не на его концах. Его тепловой отклик заметно выше обычного.
fn rare_upper_ratio_log(die: &Die) -> Log {
    let mut log = Log::new(die.names.clone());
    for index in 0..600 {
        let rare = index == 137;
        let ratio = if rare {
            55.0
        } else {
            [20.0, 25.0, 30.0, 35.0][usize::try_from(index % 4).expect("малый индекс")]
        };
        let busy = if rare {
            0.9
        } else {
            [0.3, 0.6, 0.9][usize::try_from((index / 4) % 3).expect("малый индекс")]
        };
        let own_heat = 0.2 * ratio + if rare { 40.0 } else { 0.0 };
        log.push_with_environment(
            index * 1000,
            [[Some(ratio), Some(busy), Some(30.0 + own_heat * busy), Some(60.0)]].into_iter(),
            [None; 8],
        );
    }
    log
}

#[test]
fn a_rare_but_measured_upper_ratio_remains_inside_the_live_basis_and_curve() {
    let die = topology(1);
    let log = rare_upper_ratio_log(&die);
    let result = fit_block(&[&log], &die, 0, options(0));
    assert!(
        result.eligible_rows > 500,
        "редкая верхняя точка — меньше пяти процентов состояний"
    );
    let fitted = result
        .model
        .expect("разнообразная основа и редкое измеренное верхнее состояние");
    assert_eq!(fitted.surface.metadata.knots.last().copied(), Some(55.0));
    let curve = fitted
        .query(CurveLoad::SustainedFull)
        .expect("кривая по измеренному диапазону");
    let upper = curve.points.last().expect("конечная точка кривой");
    assert!((upper.ratio - 55.0).abs() < 1e-9);
    assert!(
        upper.within_knots && upper.efficiency.is_some(),
        "измеренный край не отбрасывается процентилем"
    );
}

#[test]
fn a_bounded_training_set_keeps_the_distinct_rare_extreme_response() {
    let die = topology(1);
    let log = rare_upper_ratio_log(&die);
    let result = fit_block(
        &[&log],
        &die,
        0,
        Options {
            max_training_rows: 20,
            ..options(0)
        },
    );
    assert_eq!(result.training_rows, 20, "ограничение стоимости продолжает действовать");
    let curve = result
        .model
        .expect("ограниченная подгонка с сохранённым крайним режимом")
        .query_ratios(&[35.0, 55.0], CurveLoad::SustainedFull)
        .expect("сравнение обычного и редкого режимов");
    assert!(
        curve.points[1].own_heat > curve.points[0].own_heat + 10.0,
        "верхний отклик получен из реальной редкой строки, а не продолжен плоским: {:?}",
        curve.points
    );
}
