// Срезы одной модели: записанная история нагрузки либо заданная постоянная занятость.
// Это условные запросы при сохранённом окружении, не прогноз перераспределения работы Windows.
use serde::{Deserialize, Serialize};

use super::{CurveLoad, Error, Surface, count, design_with, dimensions, dot, lagged, ramp};
use crate::conditional_data::Prepared;

#[derive(Debug, Deserialize)]
pub struct CurveQuery {
    pub selected: Vec<bool>,
    pub ratios: Vec<f64>,
    pub load: CurveLoad,
}

#[derive(Debug, Serialize)]
pub struct CurveEstimate {
    pub rows: usize,
    pub mean_busy: f64,
    pub points: Vec<CurveEstimatePoint>,
}

#[derive(Debug, Serialize)]
pub struct CurveEstimatePoint {
    pub ratio: f64,
    pub own_heat: f64,
    // Множитель × занятость — прокси выполненной работы, не измеренная скорость приложения.
    pub performance: f64,
    pub efficiency: Option<f64>,
    // Попадание в узлы базиса не доказывает совместную обеспеченность нагрузкой и окружением.
    pub within_knots: bool,
}

impl CurveLoad {
    fn sustained(self) -> Result<Option<f64>, Error> {
        match self {
            Self::ObservedHistory => Ok(None),
            Self::SustainedFull => Ok(Some(1.0)),
            Self::Sustained(busy) if busy.is_finite() && (0.0..=1.0).contains(&busy) => Ok(Some(busy)),
            Self::Sustained(_) => Err(Error::InvalidQuery),
        }
    }
}

fn retained_busy(data: &Prepared, surface: &Surface, fixed: Option<f64>) -> Result<Vec<f64>, Error> {
    if let Some(busy) = fixed {
        return Ok(vec![busy * surface.weights.iter().sum::<f64>(); data.times.len()]);
    }
    let mut retained = vec![0.0; data.times.len()];
    for (band, weight) in std::iter::once(None)
        .chain(surface.metadata.own_bands.iter().copied().map(Some))
        .zip(&surface.weights)
    {
        for (total, busy) in retained.iter_mut().zip(lagged(data, &data.busy, band)?) {
            *total += busy * weight;
        }
    }
    Ok(retained)
}

impl Surface {
    /// Множитель заменяется во всей собственной истории; контексты и исходная пригодность строк сохраняются.
    /// Усредняются совместные произведения нагрузки и окружения, а не отдельно усреднённые факторы.
    pub fn curve_query(
        &self,
        data: &Prepared,
        selected: &[bool],
        ratios: &[f64],
        load: CurveLoad,
    ) -> Result<CurveEstimate, Error> {
        if selected.len() != data.times.len() {
            return Err(Error::Dimensions);
        }
        if self.fit_status.is_some_and(|status| !status.converged) {
            return Err(Error::NotConverged);
        }
        if ratios.iter().any(|r| !r.is_finite() || *r < 0.0) {
            return Err(Error::InvalidQuery);
        }
        let fixed = load.sustained()?;
        // Проверка размерностей базиса предшествует его использованию, включая публичные JSON-модели.
        super::validate(&self.metadata, data)?;
        dimensions(self)?;
        let design = design_with(data, self)?;
        let ids: Vec<usize> = selected
            .iter()
            .zip(&design.valid)
            .enumerate()
            .filter_map(|(i, (&yes, &valid))| (yes && valid).then_some(i))
            .collect();
        if ids.is_empty() {
            return Err(Error::InsufficientRows);
        }
        let rows = ids.len();
        let inverse = 1.0 / count(rows);
        let mean_busy = fixed.unwrap_or_else(|| ids.iter().map(|&i| data.busy[i]).sum::<f64>() * inverse);
        let retained = retained_busy(data, self, fixed)?;
        // Один проход по донорам для каждого участка кривой, без копии данных на каждый множитель.
        let mean_gains: Vec<f64> = self
            .context_coefficients
            .iter()
            .map(|coefficients| {
                ids.iter()
                    .map(|&i| retained[i] * dot(&design.environment[i], coefficients))
                    .sum::<f64>()
                    * inverse
            })
            .collect();
        let knots = &self.metadata.knots;
        let points = ratios
            .iter()
            .map(|&ratio| {
                let own_heat = knots
                    .windows(2)
                    .zip(&mean_gains)
                    .map(|(knot, gain)| ramp(ratio, knot[0], knot[1]) * gain)
                    .sum::<f64>();
                let performance = ratio * mean_busy;
                let efficiency = (own_heat > 0.0)
                    .then(|| performance / own_heat)
                    .filter(|v| v.is_finite());
                CurveEstimatePoint {
                    ratio,
                    own_heat,
                    performance,
                    efficiency,
                    within_knots: (knots[0]..=knots[knots.len() - 1]).contains(&ratio),
                }
            })
            .collect::<Vec<_>>();
        if !mean_busy.is_finite()
            || points
                .iter()
                .any(|p| !p.own_heat.is_finite() || !p.performance.is_finite())
        {
            return Err(Error::Numerical(super::gam_solver::FitError::NumericalFailure));
        }
        Ok(CurveEstimate {
            rows,
            mean_busy,
            points,
        })
    }
}
