// Живой расчёт условных кривых из исходных журналов. История не проходит отбор прежней модели.
// 32 с и ridge 0,002 — воспроизводимый численный кандидат, а не установленная постоянная ноутбука.
use std::collections::BTreeMap;

use crate::conditional::{self, CurveEstimate, CurveLoad, Error, Metadata, Surface};
use crate::conditional_data::{self, Prepared};
use crate::model::{Die, Log};

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub horizon_seconds: i64,
    pub ridge: f64,
    pub max_training_rows: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            horizon_seconds: 32,
            ridge: 0.002,
            // Ограничение стоимости подгонки, не порог достоверности и не отбор «хороших» температур.
            max_training_rows: 4096,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Coverage {
    pub name: String,
    pub known_rows: usize,
    pub total_rows: usize,
}

#[derive(Clone, Debug)]
pub struct BlockFit {
    pub name: String,
    pub prepared_rows: usize,
    pub training_rows: usize,
    pub eligible_rows: usize,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub coverage: Vec<Coverage>,
    pub omitted_contexts: Vec<String>,
    pub status: String,
    pub options: Options,
    pub model: Option<Fitted>,
}

#[derive(Clone, Debug)]
pub struct Fitted {
    pub surface: Surface,
    pub data: Prepared,
}

impl Fitted {
    pub fn curve_query(&self, load: CurveLoad) -> Result<CurveEstimate, Error> {
        self.query(load)
    }

    pub fn query(&self, load: CurveLoad) -> Result<CurveEstimate, Error> {
        let extent = self
            .data
            .ratio
            .iter()
            .copied()
            .filter(|r| r.is_finite() && *r >= 0.0)
            .map(|ratio| (ratio, ratio))
            .reduce(|(lo, hi), (ratio, _)| (lo.min(ratio), hi.max(ratio)));
        let Some((lo, hi)) = extent else {
            return Err(Error::InsufficientRows);
        };
        // Равномерная сетка — только разрешение линии; численная модель не округляет измеренный множитель.
        let ratios: Vec<f64> = (0..=64).map(|i| lo + (hi - lo) * f64::from(i) / 64.0).collect();
        self.query_ratios(&ratios, load)
    }

    pub fn query_ratios(&self, ratios: &[f64], load: CurveLoad) -> Result<CurveEstimate, Error> {
        self.surface
            .curve_query(&self.data, &vec![true; self.data.times.len()], ratios, load)
    }
}

fn coverage(data: &Prepared) -> Vec<Coverage> {
    [
        ("ratio", &data.ratio),
        ("busy", &data.busy),
        ("temperature", &data.target),
    ]
    .into_iter()
    .chain(data.contexts.iter().map(|(name, column)| (name.as_str(), column)))
    .map(|(name, column)| Coverage {
        name: name.to_string(),
        known_rows: column.iter().filter(|v| v.is_finite()).count(),
        total_rows: data.times.len(),
    })
    .collect()
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "quantile index is bounded by the allocated vector length; interpolation uses in-memory row counts"
)]
fn quantile(sorted: &[f64], fraction: f64) -> f64 {
    let position = fraction * (sorted.len() - 1) as f64;
    let lo = position.floor() as usize;
    let hi = position.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (position - lo as f64)
}

fn finite_sorted(values: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut values: Vec<f64> = values.filter(|v| v.is_finite()).collect();
    values.sort_by(f64::total_cmp);
    values
}

fn own_bands(horizon: i64) -> Vec<[i64; 2]> {
    (0..horizon.min(16))
        .map(|lo| [lo, lo + 1])
        .chain(
            [[16, 32], [32, 64], [64, 128], [128, 256], [256, 512], [512, 1024]]
                .into_iter()
                .filter(|b| b[1] <= horizon),
        )
        .collect()
}

fn environment_bands(horizon: i64) -> Vec<[i64; 2]> {
    [
        [0, 2],
        [2, 8],
        [8, 16],
        [16, 32],
        [32, 64],
        [64, 128],
        [128, 256],
        [256, 512],
        [512, 1024],
    ]
    .into_iter()
    .filter(|b| b[1] <= horizon)
    .collect()
}

fn metadata(data: &Prepared, horizon: i64, omitted: &mut Vec<String>) -> Result<Metadata, Error> {
    let mut ratios = finite_sorted(
        data.ratio
            .iter()
            .zip(&data.busy)
            .filter_map(|(&r, &b)| (b > 0.5).then_some(r)),
    );
    if ratios.len() < 30 {
        ratios = finite_sorted(data.ratio.iter().copied());
    }
    if ratios.len() < 30 {
        return Err(Error::InsufficientRows);
    }
    let mut knots = vec![0.0];
    knots.extend([0.05, 0.33, 0.66, 0.95].map(|q| quantile(&ratios, q)));
    knots.retain(|r| *r >= 0.0);
    knots.sort_by(f64::total_cmp);
    knots.dedup();
    if knots.len() < 2 {
        return Err(Error::InsufficientRows);
    }
    let mut contexts = BTreeMap::new();
    for (name, column) in &data.contexts {
        if name == "package_lag_only" {
            continue;
        }
        let values = finite_sorted(column.iter().copied());
        if values.len() < 30 {
            omitted.push(format!("{name}: недостаточно измерений"));
            continue;
        }
        let scale = [quantile(&values, 0.05), quantile(&values, 0.95)];
        if scale[1] - scale[0] < 1e-8 {
            omitted.push(format!("{name}: влияние не определено, параметр почти не менялся"));
        } else {
            contexts.insert(name.clone(), scale); // Имена принадлежат метаданным; исходные столбцы сохраняются.
        }
    }
    let package_known = data
        .contexts
        .get("package_lag_only")
        .is_some_and(|v| v.iter().filter(|x| x.is_finite()).count() >= 30);
    if !package_known {
        omitted.push("package_lag_only: недостаточно измерений".into());
    }
    Ok(Metadata {
        knots,
        contexts,
        own_bands: own_bands(horizon),
        shape_history_contexts: Some(
            ["fan_cpu", "fan_gpu", "temp_gpu", "power_gpu", "busy_gpu"]
                .map(str::to_string)
                .to_vec(),
        ),
        use_package_history: package_known && horizon > 0,
    })
}

fn training_mask(valid: &[bool], ratios: &[f64], limit: usize) -> Vec<bool> {
    let ids: Vec<usize> = valid
        .iter()
        .enumerate()
        .filter_map(|(i, &yes)| yes.then_some(i))
        .collect();
    let mut mask = vec![false; valid.len()];
    if ids.len() <= limit {
        for i in ids {
            mask[i] = true;
        }
    } else if limit > 1 {
        // Крайние измеренные множители не теряются из-за ограничения стоимости обучения.
        let low = ids.iter().copied().min_by(|&a, &b| ratios[a].total_cmp(&ratios[b]));
        let high = ids.iter().copied().max_by(|&a, &b| ratios[a].total_cmp(&ratios[b]));
        for i in low.into_iter().chain(high) {
            mask[i] = true;
        }
        let remaining: Vec<usize> = ids.into_iter().filter(|&i| !mask[i]).collect();
        let slots = limit - mask.iter().filter(|&&yes| yes).count();
        if slots == 1 {
            mask[remaining[(remaining.len() - 1) / 2]] = true;
        } else if slots > 1 {
            for i in 0..slots {
                mask[remaining[i * (remaining.len() - 1) / (slots - 1)]] = true;
            }
        }
    }
    mask
}

pub fn fit_block(logs: &[&Log], die: &Die, block: usize, options: Options) -> BlockFit {
    let data = conditional_data::prepare(logs, die, block);
    let mut result = BlockFit {
        name: die.names.get(block).cloned().unwrap_or_default(),
        prepared_rows: data.times.len(),
        training_rows: 0,
        eligible_rows: 0,
        first_ms: data.times.iter().copied().min(),
        last_ms: data.times.iter().copied().max(),
        coverage: coverage(&data),
        omitted_contexts: Vec::new(),
        status: String::new(),
        options,
        model: None,
    };
    let fitted = fit_prepared(data, options, &mut result);
    match fitted {
        Ok(model) => {
            result.status = "Условная модель пересчитана; горизонт истории — предварительный кандидат".into();
            result.model = Some(model);
        }
        Err(error) => result.status = error_text(error).into(),
    }
    result
}

fn fit_prepared(data: Prepared, options: Options, result: &mut BlockFit) -> Result<Fitted, Error> {
    if ![0, 2, 8, 16, 32, 64, 128, 256, 512, 1024].contains(&options.horizon_seconds)
        || !options.ridge.is_finite()
        || options.ridge < 0.0
        || options.max_training_rows < 20
    {
        return Err(Error::InvalidMetadata);
    }
    let mut meta = metadata(&data, options.horizon_seconds, &mut result.omitted_contexts)?;
    let bands = environment_bands(options.horizon_seconds);
    let candidate = Surface::template(meta.clone(), bands.clone());
    let mut valid = conditional::design_with(&data, &candidate)?.valid;
    for (yes, &target) in valid.iter_mut().zip(&data.target) {
        *yes &= target.is_finite();
    }
    let maximum = data
        .ratio
        .iter()
        .zip(&valid)
        .filter_map(|(&ratio, &yes)| yes.then_some(ratio))
        .max_by(f64::total_cmp)
        .ok_or(Error::InsufficientRows)?;
    // Внутренние узлы остаются квантильными; верхний узел покрывает реальные полные состояния,
    // а не обрезает измеренный хвост по 95-му процентилю пилотного шаблона.
    meta.knots.truncate(meta.knots.len().saturating_sub(1));
    meta.knots.retain(|ratio| *ratio < maximum);
    meta.knots.push(maximum);
    if meta.knots.len() < 2 {
        return Err(Error::InsufficientRows);
    }
    let template = Surface::template(meta, bands);
    valid = conditional::design_with(&data, &template)?.valid;
    for (yes, &target) in valid.iter_mut().zip(&data.target) {
        *yes &= target.is_finite();
    }
    result.eligible_rows = valid.iter().filter(|&&yes| yes).count();
    let train = training_mask(&valid, &data.ratio, options.max_training_rows);
    result.training_rows = train.iter().filter(|&&yes| yes).count();
    let surface = conditional::fit(&data, template, &train, options.ridge)?;
    Ok(Fitted { surface, data })
}

fn error_text(error: Error) -> &'static str {
    match error {
        Error::InsufficientRows => "Недостаточно полных состояний с известной историей для условной модели",
        Error::NotConverged => "Подгонка условной модели не сошлась; прежняя кривая не подставляется",
        Error::Numerical(_) => "Численная ошибка условной модели; прежняя кривая не подставляется",
        Error::MissingContext => "Недостаточно показаний внешнего параметра для условной модели",
        Error::History(_) => "Непригодная временная история условной модели",
        Error::Dimensions | Error::InvalidMetadata | Error::InvalidQuery => "Неверные входные данные условной модели",
    }
}

#[cfg(test)]
mod benchmark {
    use super::{Options, fit_block};
    use crate::conditional::{CurveEstimatePoint, CurveLoad};

    fn extent(values: impl Iterator<Item = f64>) -> Option<[f64; 2]> {
        values
            .filter(|value| value.is_finite())
            .map(|value| [value, value])
            .reduce(|[lo, hi], [value, _]| [lo.min(value), hi.max(value)])
    }

    fn curve_details(points: &[CurveEstimatePoint]) -> (serde_json::Value, usize) {
        let drawable = points
            .iter()
            .filter(|point| {
                point.within_knots
                    && point.own_heat > 0.0
                    && point.efficiency.is_some_and(|value| value.is_finite() && value > 0.0)
            })
            .count();
        (
            serde_json::json!({
                "drawable_efficiency_points": drawable,
                "within_knots_points": points.iter().filter(|point| point.within_knots).count(),
                "ratio_extent": extent(points.iter().map(|point| point.ratio)),
                "own_heat_extent": extent(points.iter().map(|point| point.own_heat)),
                "efficiency_extent": extent(points.iter().filter_map(|point| point.efficiency)),
                "points": points,
            }),
            drawable,
        )
    }

    // Запускается отдельно на папке журнала; обычные проверки не читают данные владельца.
    #[test]
    #[ignore = "explicit bounded real-data benchmark; requires samples and output paths"]
    fn one_real_block_native_adapter() {
        let samples = std::env::var_os("COREMAP_LIVE_BENCH_SAMPLES")
            .expect("COREMAP_LIVE_BENCH_SAMPLES points to the explicit sample folder");
        let output = std::env::var_os("COREMAP_LIVE_BENCH_OUTPUT")
            .expect("COREMAP_LIVE_BENCH_OUTPUT points to the private report");
        let started = std::time::Instant::now();
        let (logs, trouble) = crate::store::read_all(samples.into());
        assert!(trouble.is_none(), "input parsing must succeed: {trouble:?}");
        let names = logs.iter().find(|log| log.rows() > 0).expect("source rows").names();
        let die = crate::model_report::die_for(names).expect("actual layout for recorded block names");
        let block = die
            .names
            .iter()
            .position(|name| name == "23")
            .expect("recorded block23");
        let source_rows: usize = logs.iter().map(crate::model::Log::rows).sum();
        let read_seconds = started.elapsed().as_secs_f64();
        let refs: Vec<_> = logs.iter().collect();
        let fit_started = std::time::Instant::now();
        let result = fit_block(&refs, &die, block, Options::default());
        let fit_seconds = fit_started.elapsed().as_secs_f64();
        let curve = result
            .model
            .as_ref()
            .map(|model| model.query(CurveLoad::ObservedHistory));
        let query_seconds = fit_started.elapsed().as_secs_f64() - fit_seconds;
        let (curve_rows, curve_points, query_error) = match &curve {
            Some(Ok(curve)) => (curve.rows, curve.points.len(), None),
            Some(Err(error)) => (0, 0, Some(format!("{error:?}"))),
            None => (0, 0, None),
        };
        let points = match &curve {
            Some(Ok(curve)) => curve.points.as_slice(),
            _ => &[],
        };
        let (details, drawable_points) = curve_details(points);
        let report = serde_json::json!({
            "block": result.name,
            "source_rows": source_rows,
            "read_seconds": read_seconds,
            "fit_seconds": fit_seconds,
            "query_seconds": query_seconds,
            "prepared_rows": result.prepared_rows,
            "eligible_rows": result.eligible_rows,
            "training_rows": result.training_rows,
            "first_ms": result.first_ms,
            "last_ms": result.last_ms,
            "omitted_contexts": result.omitted_contexts,
            "status": result.status,
            "curve_rows": curve_rows,
            "curve_points": curve_points,
            "curve_details": details,
            "knots": result.model.as_ref().map(|model| &model.surface.metadata.knots),
            "query_error": query_error,
            "model_fitted": result.model.is_some(),
            "horizon_seconds": result.options.horizon_seconds,
            "ridge": result.options.ridge,
            "max_training_rows": result.options.max_training_rows,
            "basis_status": "existing numerical candidate; no laptop horizon or attribution validation"
        });
        std::fs::write(output, serde_json::to_vec_pretty(&report).expect("report JSON"))
            .expect("write private benchmark report");
        assert!(
            result.model.is_some(),
            "real adapter fitting must succeed: {}",
            result.status
        );
        assert!(query_error.is_none(), "real curve query must succeed");
        assert!(curve_points > 0, "real adapter must supply curve points");
        assert!(
            drawable_points > 1,
            "real adapter must supply a drawable efficiency line"
        );
    }
}
