// Условная кривая: монотонные по множителю базисы, обучаемая история собственной нагрузки и деформация окружением.
// Это статистическое разложение температуры, не измерение теплового сопротивления. Одинаковый временной базис
// используется для подгонки, проверки и запросов; текущая температура корпуса среди предикторов не допускается.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::conditional_data::Prepared;
use crate::gam_solver;
use crate::history::Signal;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Metadata {
    pub knots: Vec<f64>,
    pub contexts: BTreeMap<String, [f64; 2]>,
    pub own_bands: Vec<[i64; 2]>,
    #[serde(default)]
    pub shape_history_contexts: Option<Vec<String>>,
    #[serde(default)]
    pub use_package_history: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ContextBasis {
    #[serde(default)]
    pub constant: bool,
    pub context: Option<String>,
    pub side: Option<usize>,
    pub lag: Option<[i64; 2]>,
    pub joint: Option<Vec<(String, usize)>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BackgroundTerm {
    pub context: String,
    pub transform: String,
    pub lag: Option<[i64; 2]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BackgroundBasis {
    pub kind: String,
    pub name: String,
    pub group: String,
    pub penalty: f64,
    pub basis: Option<BackgroundTerm>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Surface {
    pub metadata: Metadata,
    pub bands: Vec<[i64; 2]>,
    pub weights: Vec<f64>,
    pub context_basis: Vec<ContextBasis>,
    pub background_basis: Vec<BackgroundBasis>,
    pub context_coefficients: Vec<Vec<f64>>,
    pub background_coefficients: Vec<f64>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Diagnostic {
    pub iterations: usize,
    pub projected_gradient: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Dimensions,
    InvalidMetadata,
    History(crate::history::HistoryError),
    MissingContext,
    InsufficientRows,
    Numerical(gam_solver::FitError),
    NotConverged,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Prediction {
    pub temperature: f64,
    /// Условный собственный вклад, якорь — нулевая активность при том же окружении.
    pub own_heat: f64,
}

#[derive(Debug)]
pub struct Design {
    // Строка × лаг нагрузки × интервал множителя; отдельная матрица — чтобы не создавать полный тензор всех лагов.
    drive: Vec<Vec<Vec<f64>>>,
    environment: Vec<Vec<f64>>,
    background: Vec<Vec<f64>>,
    pub valid: Vec<bool>,
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn ramp(value: f64, lo: f64, hi: f64) -> f64 {
    if value.is_finite() {
        ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
    } else {
        f64::NAN
    }
}

fn hat(value: f64, scale: [f64; 2], side: usize) -> f64 {
    let u = ramp(value, scale[0], scale[1]);
    if side == 0 { 1.0 - u } else { u }
}

fn validate(meta: &Metadata, data: &Prepared) -> Result<(), Error> {
    let n = data.times.len();
    if n == 0
        || data.segments.len() != n
        || data.ratio.len() != n
        || data.busy.len() != n
        || data.target.len() != n
        || data.contexts.values().any(|v| v.len() != n)
    {
        return Err(Error::Dimensions);
    }
    if meta.knots.len() < 2
        || meta.knots.iter().any(|v| !v.is_finite())
        || meta.knots.windows(2).any(|v| v[1] <= v[0])
        || meta
            .contexts
            .values()
            .any(|s| !s[0].is_finite() || !s[1].is_finite() || s[1] <= s[0])
        || meta.own_bands.iter().any(|&[lo, hi]| lo < 0 || hi <= lo)
    {
        return Err(Error::InvalidMetadata);
    }
    Ok(())
}

/// Интегрируется уже преобразованная величина: среднее нелинейного базиса не равно базису среднего входа.
fn lagged(data: &Prepared, values: &[f64], lag: Option<[i64; 2]>) -> Result<Vec<f64>, Error> {
    let Some([lo, hi]) = lag else {
        return Ok(values.to_vec());
    };
    if lo < 0 || hi <= lo {
        return Err(Error::InvalidMetadata);
    }
    let lo = lo.checked_mul(1000).ok_or(Error::InvalidMetadata)?;
    let hi = hi.checked_mul(1000).ok_or(Error::InvalidMetadata)?;
    let mut signal = Signal::new(5000).map_err(Error::History)?;
    let mut segment = None;
    let mut result = Vec::with_capacity(values.len());
    for ((&at, &sid), &value) in data.times.iter().zip(&data.segments).zip(values) {
        if segment != Some(sid) {
            signal = Signal::new(5000).map_err(Error::History)?;
            segment = Some(sid);
        }
        signal.push(at, finite(value)).map_err(Error::History)?;
        let window = signal.window(at, lo, hi).map_err(Error::History)?;
        result.push(if window.coverage() >= 1.0 {
            window.mean.unwrap_or(f64::NAN)
        } else {
            f64::NAN
        });
    }
    Ok(result)
}

fn context_column(data: &Prepared, meta: &Metadata, basis: &ContextBasis) -> Result<Vec<f64>, Error> {
    if basis.constant {
        return Ok(vec![1.0; data.times.len()]);
    }
    let factors = if let Some(joint) = &basis.joint {
        joint.clone() // Обычно два фактора; дескриптор нужен только во время подготовки модели.
    } else {
        vec![(
            basis.context.clone().ok_or(Error::InvalidMetadata)?,
            basis.side.ok_or(Error::InvalidMetadata)?,
        )]
    };
    let mut result = vec![1.0; data.times.len()];
    for (name, side) in factors {
        if side > 1 || (name == "package_lag_only" && basis.lag.is_none()) {
            return Err(Error::InvalidMetadata);
        }
        let scale = *meta.contexts.get(&name).ok_or(Error::MissingContext)?;
        let values = data.contexts.get(&name).ok_or(Error::MissingContext)?;
        let transformed: Vec<f64> = values.iter().map(|&v| hat(v, scale, side)).collect();
        let history = lagged(data, &transformed, basis.lag)?;
        for (product, v) in result.iter_mut().zip(history) {
            *product *= v;
        }
    }
    Ok(result)
}

fn background_column(data: &Prepared, meta: &Metadata, basis: &BackgroundBasis) -> Result<Vec<f64>, Error> {
    if basis.kind == "intercept" {
        return Ok(vec![1.0; data.times.len()]);
    }
    let term = basis.basis.as_ref().ok_or(Error::InvalidMetadata)?;
    if term.context == "package_lag_only" && term.lag.is_none() {
        return Err(Error::InvalidMetadata);
    }
    let scale = match term.transform.as_str() {
        "linear" => *meta.contexts.get(&term.context).ok_or(Error::MissingContext)?,
        "package_linear" if term.context == "package_lag_only" => [50.0, 70.0],
        _ => return Err(Error::InvalidMetadata),
    };
    let values = data.contexts.get(&term.context).ok_or(Error::MissingContext)?;
    let transformed: Vec<f64> = values.iter().map(|v| (v - scale[0]) / (scale[1] - scale[0])).collect();
    lagged(data, &transformed, term.lag)
}

fn transpose(columns: &[Vec<f64>], n: usize) -> Vec<Vec<f64>> {
    (0..n).map(|i| columns.iter().map(|c| c[i]).collect()).collect()
}

fn design_with(data: &Prepared, surface: &Surface) -> Result<Design, Error> {
    validate(&surface.metadata, data)?;
    let n = data.times.len();
    let k = surface.metadata.knots.len() - 1;
    let mut drive = vec![vec![vec![0.0; k]; surface.metadata.own_bands.len() + 1]; n];
    for (j, knot) in surface.metadata.knots.windows(2).enumerate() {
        let values: Vec<f64> = data
            .ratio
            .iter()
            .zip(&data.busy)
            .map(|(&r, &b)| ramp(r, knot[0], knot[1]) * b)
            .collect();
        for (l, lag) in std::iter::once(None)
            .chain(surface.metadata.own_bands.iter().copied().map(Some))
            .enumerate()
        {
            for (row, v) in drive.iter_mut().zip(lagged(data, &values, lag)?) {
                row[l][j] = v;
            }
        }
    }
    let env = surface
        .context_basis
        .iter()
        .map(|b| context_column(data, &surface.metadata, b))
        .collect::<Result<Vec<_>, _>>()?;
    let bg = surface
        .background_basis
        .iter()
        .map(|b| background_column(data, &surface.metadata, b))
        .collect::<Result<Vec<_>, _>>()?;
    let environment = transpose(&env, n);
    let background = transpose(&bg, n);
    let valid = drive
        .iter()
        .zip(&environment)
        .zip(&background)
        .map(|((d, e), b)| d.iter().flatten().chain(e).chain(b).all(|v| v.is_finite()))
        .collect();
    Ok(Design {
        drive,
        environment,
        background,
        valid,
    })
}

fn dimensions(surface: &Surface) -> Result<(), Error> {
    if surface.weights.len() != surface.metadata.own_bands.len() + 1
        || surface.context_coefficients.len() != surface.metadata.knots.len() - 1
        || surface
            .context_coefficients
            .iter()
            .any(|c| c.len() != surface.context_basis.len())
        || surface.background_coefficients.len() != surface.background_basis.len()
    {
        return Err(Error::Dimensions);
    }
    if surface
        .weights
        .iter()
        .chain(surface.context_coefficients.iter().flatten())
        .any(|v| !v.is_finite() || *v < 0.0)
        || surface.background_coefficients.iter().any(|v| !v.is_finite())
    {
        return Err(Error::InvalidMetadata);
    }
    Ok(())
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

impl Surface {
    pub fn template(metadata: Metadata, bands: Vec<[i64; 2]>) -> Self {
        let mut context_basis = vec![ContextBasis {
            constant: true,
            ..ContextBasis::default()
        }];
        let mut background_basis = vec![BackgroundBasis {
            kind: "intercept".into(),
            name: "intercept".into(),
            group: "background".into(),
            penalty: 0.00001,
            basis: None,
        }];
        for name in metadata.contexts.keys() {
            for lag in std::iter::once(None).chain(bands.iter().copied().map(Some)) {
                if name == "package_lag_only" && lag.is_none() {
                    continue;
                }
                if lag.is_none()
                    || metadata
                        .shape_history_contexts
                        .as_ref()
                        .is_none_or(|names| names.contains(name))
                {
                    for side in 0..2 {
                        context_basis.push(ContextBasis {
                            context: Some(name.clone()),
                            side: Some(side),
                            lag,
                            ..ContextBasis::default()
                        });
                    }
                }
                background_basis.push(BackgroundBasis {
                    kind: "background".into(),
                    name: format!("{name}{lag:?}"),
                    group: name.clone(),
                    penalty: 1.0,
                    basis: Some(BackgroundTerm {
                        context: name.clone(),
                        transform: "linear".into(),
                        lag,
                    }),
                });
            }
        }
        if metadata.contexts.contains_key("fan_cpu") && metadata.contexts.contains_key("temp_gpu") {
            for lag in std::iter::once(None).chain(bands.iter().copied().map(Some)) {
                for a in 0..2 {
                    for b in 0..2 {
                        context_basis.push(ContextBasis {
                            joint: Some(vec![("fan_cpu".into(), a), ("temp_gpu".into(), b)]),
                            lag,
                            ..ContextBasis::default()
                        });
                    }
                }
            }
        }
        if metadata.use_package_history {
            for &lag in &bands {
                background_basis.push(BackgroundBasis {
                    kind: "background".into(),
                    name: format!("package{lag:?}"),
                    group: "package_history".into(),
                    penalty: 2.0,
                    basis: Some(BackgroundTerm {
                        context: "package_lag_only".into(),
                        transform: "package_linear".into(),
                        lag: Some(lag),
                    }),
                });
            }
        }
        Self {
            metadata,
            bands,
            weights: Vec::new(),
            context_basis,
            background_basis,
            context_coefficients: Vec::new(),
            background_coefficients: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn predict(&self, data: &Prepared) -> Result<Vec<Option<Prediction>>, Error> {
        validate(&self.metadata, data)?;
        dimensions(self)?;
        let design = design_with(data, self)?;
        Ok(design
            .drive
            .iter()
            .enumerate()
            .map(|(i, d)| {
                if !design.valid[i] {
                    return None;
                }
                let own_heat = self
                    .context_coefficients
                    .iter()
                    .enumerate()
                    .map(|(k, c)| {
                        let retained: f64 = d.iter().zip(&self.weights).map(|(r, w)| r[k] * w).sum();
                        retained * dot(&design.environment[i], c)
                    })
                    .sum::<f64>();
                let temperature = own_heat + dot(&design.background[i], &self.background_coefficients);
                (temperature.is_finite() && own_heat.is_finite()).then_some(Prediction { temperature, own_heat })
            })
            .collect())
    }
}

fn centered_fit(
    rows: &[Vec<f64>],
    y: &[f64],
    signed: usize,
    penalties: &[f64],
    ridge: f64,
) -> Result<gam_solver::Fit, Error> {
    if y.len() < 20 || rows.is_empty() {
        return Err(Error::InsufficientRows);
    }
    let center = y.iter().sum::<f64>() / count(y.len());
    let targets: Vec<f64> = y.iter().map(|v| v - center).collect();
    let mut bounded = vec![true; penalties.len()];
    bounded[..signed].fill(false);
    let mut fit = gam_solver::fit(rows, &targets, &bounded, penalties, ridge).map_err(Error::Numerical)?;
    if !fit.converged {
        return Err(Error::NotConverged);
    }
    fit.coefficients[0] += center;
    if fit.predict(&rows[0]).is_none() {
        return Err(Error::Numerical(gam_solver::FitError::NumericalFailure));
    }
    Ok(fit)
}

#[expect(clippy::cast_precision_loss, reason = "in-memory row count is below 2^53")]
fn count(n: usize) -> f64 {
    n as f64
}

/// Чередование двух ограниченных МНК. Лаговый вес общий для участков кривой: это ограничение ранга, а не физический закон.
pub fn fit(data: &Prepared, template: Surface, train: &[bool], ridge: f64) -> Result<Surface, Error> {
    fit_iterations(data, template, train, ridge, 12)
}

pub fn fit_iterations(
    data: &Prepared,
    mut template: Surface,
    train: &[bool],
    ridge: f64,
    max_iterations: usize,
) -> Result<Surface, Error> {
    if train.len() != data.times.len() || !ridge.is_finite() || ridge < 0.0 {
        return Err(Error::Dimensions);
    }
    let design = design_with(data, &template)?;
    let ids: Vec<usize> = train
        .iter()
        .enumerate()
        .filter_map(|(i, &yes)| (yes && design.valid[i] && data.target[i].is_finite()).then_some(i))
        .collect();
    if ids.len() < 20 {
        return Err(Error::InsufficientRows);
    }
    let targets: Vec<f64> = ids.iter().map(|&i| data.target[i]).collect();
    let lags = template.metadata.own_bands.len() + 1;
    let ramps = template.metadata.knots.len() - 1;
    let contexts = template.context_basis.len();
    let backgrounds = template.background_basis.len();
    let mut weights = vec![0.0; lags];
    if lags == 1 {
        weights[0] = 1.0;
    } else {
        let initial = (lags - 1).min(3);
        weights[1..=initial].fill(1.0 / count(initial));
    }
    let mut penalties: Vec<f64> = template.background_basis.iter().map(|d| d.penalty).collect();
    penalties.resize(backgrounds + ramps * contexts, 1.0);
    let mut final_pass = false;
    // Последняя итерация пересчитывает кривую после нормировки окончательного ядра истории.
    for iteration in 0..=max_iterations {
        let rows: Vec<Vec<f64>> = ids
            .iter()
            .map(|&i| {
                let mut row = design.background[i].clone();
                for j in 0..ramps {
                    let retained: f64 = weights.iter().zip(&design.drive[i]).map(|(w, d)| w * d[j]).sum();
                    row.extend(design.environment[i].iter().map(|e| retained * e));
                }
                row
            })
            .collect();
        let curve = centered_fit(&rows, &targets, backgrounds, &penalties, ridge)?;
        template.diagnostics.push(Diagnostic {
            iterations: curve.iterations,
            projected_gradient: curve.projected_gradient,
        });
        template.background_coefficients = curve.coefficients[..backgrounds].to_vec();
        template.context_coefficients = curve.coefficients[backgrounds..]
            .chunks(contexts)
            .map(<[f64]>::to_vec)
            .collect();
        template.weights.clone_from(&weights);
        if iteration == max_iterations || final_pass {
            break;
        }
        let lag_rows: Vec<Vec<f64>> = ids
            .iter()
            .map(|&i| {
                let gains: Vec<f64> = template
                    .context_coefficients
                    .iter()
                    .map(|c| dot(c, &design.environment[i]))
                    .collect();
                let mut row = design.background[i].clone();
                row.extend(design.drive[i].iter().map(|d| dot(d, &gains)));
                row
            })
            .collect();
        let mut lag_penalties: Vec<f64> = template.background_basis.iter().map(|d| d.penalty).collect();
        lag_penalties.resize(backgrounds + lags, 0.001);
        let kernel = centered_fit(&lag_rows, &targets, backgrounds, &lag_penalties, ridge)?;
        template.diagnostics.push(Diagnostic {
            iterations: kernel.iterations,
            projected_gradient: kernel.projected_gradient,
        });
        let mut next = kernel.coefficients[backgrounds..].to_vec();
        let sum: f64 = next.iter().sum();
        if sum < 1e-12 {
            break;
        }
        for w in &mut next {
            *w /= sum;
        }
        let delta = next
            .iter()
            .zip(&weights)
            .map(|(a, backgrounds)| (a - backgrounds).abs())
            .fold(0.0, f64::max);
        weights = next;
        // Малое изменение ядра не означает доказанной глобальной оптимальности; следующий проход — окончательная подгонка.
        final_pass = delta < 1e-4;
    }
    dimensions(&template)?;
    Ok(template)
}
