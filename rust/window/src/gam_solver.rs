// Штрафной МНК: знаковые фоновые коэффициенты и неотрицательные собственные вклады.
// Цель = ||Xβ − y||² / (2N) + ridge·Σ penalty[j]·β[j]² / 2. Центрирование и свободный член — у вызывающего кода.
// Матрица Грама выделяется один раз в фоновом расчёте модели, не в опросе датчиков. Масштабирование переменных
// диагональю улучшает численное условие, не добавляя штрафа и не меняя целевую функцию.

const MAX_ITERATIONS: usize = 10_000;
const RELATIVE_TOLERANCE: f64 = 1e-8;
const REFRESH_EVERY: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitError {
    EmptyInput,
    Dimensions,
    NonFiniteInput,
    InvalidPenalty,
    NumericalFailure,
}

#[derive(Clone, Debug)]
pub struct Fit {
    pub coefficients: Vec<f64>,
    pub converged: bool,
    /// Полных проходов по координатам; ноль — начальное решение уже удовлетворяет условиям.
    pub iterations: usize,
    /// Максимальный остаток условий KKT в координатах, нормированных корнем диагонали Грама.
    pub projected_gradient: f64,
}

impl Fit {
    pub fn predict(&self, x: &[f64]) -> Option<f64> {
        if x.len() != self.coefficients.len() || x.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let value = x.iter().zip(&self.coefficients).map(|(x, beta)| x * beta).sum::<f64>();
        value.is_finite().then_some(value)
    }
}

struct Normal {
    gram: Vec<f64>,
    rhs: Vec<f64>,
    scales: Vec<f64>,
}

fn validate(
    rows: &[Vec<f64>],
    targets: &[f64],
    nonnegative: &[bool],
    penalties: &[f64],
    ridge: f64,
) -> Result<usize, FitError> {
    let Some(first) = rows.first() else {
        return Err(FitError::EmptyInput);
    };
    let width = first.len();
    if width == 0
        || targets.len() != rows.len()
        || nonnegative.len() != width
        || penalties.len() != width
        || rows.iter().any(|row| row.len() != width)
    {
        return Err(FitError::Dimensions);
    }
    if targets.iter().chain(rows.iter().flatten()).any(|v| !v.is_finite()) {
        return Err(FitError::NonFiniteInput);
    }
    if !ridge.is_finite() || ridge < 0.0 || penalties.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return Err(FitError::InvalidPenalty);
    }
    Ok(width)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the number of rows physically held in memory is below 2^53"
)]
fn normal(rows: &[Vec<f64>], targets: &[f64], penalties: &[f64], ridge: f64) -> Result<Normal, FitError> {
    let width = penalties.len();
    let cells = width.checked_mul(width).ok_or(FitError::Dimensions)?;
    let mut gram = vec![0.0; cells];
    let mut rhs = vec![0.0; width];
    let inverse_n = 1.0 / rows.len() as f64;
    for (row, y) in rows.iter().zip(targets) {
        for (j, x) in row.iter().enumerate() {
            rhs[j] += x * y * inverse_n;
            for k in 0..=j {
                gram[j * width + k] += x * row[k] * inverse_n;
            }
        }
    }
    let mut scales = Vec::with_capacity(width);
    for j in 0..width {
        gram[j * width + j] += ridge * penalties[j];
        scales.push(gram[j * width + j].sqrt());
    }
    for j in 0..width {
        if scales[j] > 0.0 {
            rhs[j] /= scales[j];
        }
        for k in 0..=j {
            let value = if scales[j] > 0.0 && scales[k] > 0.0 {
                gram[j * width + k] / scales[j] / scales[k]
            } else {
                0.0
            };
            gram[j * width + k] = value;
            gram[k * width + j] = value;
        }
    }
    if gram.iter().chain(&rhs).chain(&scales).any(|v| !v.is_finite()) {
        return Err(FitError::NumericalFailure);
    }
    Ok(Normal { gram, rhs, scales })
}

fn refresh(normal: &Normal, coefficients: &[f64], gradient: &mut [f64]) -> Result<(), FitError> {
    let width = coefficients.len();
    for (j, g) in gradient.iter_mut().enumerate() {
        *g = normal.gram[j * width..(j + 1) * width]
            .iter()
            .zip(coefficients)
            .map(|(a, beta)| a * beta)
            .sum::<f64>()
            - normal.rhs[j];
    }
    if gradient.iter().any(|g| !g.is_finite()) {
        return Err(FitError::NumericalFailure);
    }
    Ok(())
}

fn projected(gradient: &[f64], coefficients: &[f64], nonnegative: &[bool]) -> f64 {
    gradient
        .iter()
        .zip(coefficients)
        .zip(nonnegative)
        .map(|((&g, &beta), &bounded)| {
            if bounded && beta == 0.0 {
                g.min(0.0).abs()
            } else {
                g.abs()
            }
        })
        .fold(0.0, f64::max)
}

fn free_solution(normal: &Normal, free: &[bool]) -> Option<Vec<f64>> {
    let width = free.len();
    let columns: Vec<usize> = (0..width).filter(|&j| free[j]).collect();
    let n = columns.len();
    if n == 0 {
        return None;
    }
    let mut lower = vec![0.0; n.checked_mul(n)?];
    for (j, &column) in columns.iter().enumerate() {
        for (k, &other) in columns[..=j].iter().enumerate() {
            let cross: f64 = (0..k).map(|i| lower[j * n + i] * lower[k * n + i]).sum();
            let value = normal.gram[column * width + other] - cross;
            if !value.is_finite() {
                return None;
            }
            lower[j * n + k] = if j == k {
                // Диагональ исходного нормированного Грама равна единице. Малый
                // остаточный pivot означает потерю точности; штраф ради решения не добавляем.
                if value <= f64::EPSILON.sqrt() {
                    return None;
                }
                value.sqrt()
            } else {
                value / lower[k * n + k]
            };
        }
    }
    let mut solution = vec![0.0; n];
    for (j, &column) in columns.iter().enumerate() {
        let cross: f64 = (0..j).map(|k| lower[j * n + k] * solution[k]).sum();
        solution[j] = (normal.rhs[column] - cross) / lower[j * n + j];
    }
    for j in (0..n).rev() {
        let cross: f64 = (j + 1..n).map(|k| lower[k * n + j] * solution[k]).sum();
        solution[j] = (solution[j] - cross) / lower[j * n + j];
    }
    let mut candidate = vec![0.0; width];
    for (&column, value) in columns.iter().zip(solution) {
        if !value.is_finite() {
            return None;
        }
        candidate[column] = value;
    }
    Some(candidate)
}

fn nonincreasing(normal: &Normal, previous: &[f64], gradient: &[f64], candidate: &[f64]) -> bool {
    let width = previous.len();
    // Разность квадратичных целей через шаг и точный прежний градиент: без
    // вычитания двух больших почти равных значений и без повторного добавления ridge.
    let step: Vec<f64> = candidate.iter().zip(previous).map(|(new, old)| new - old).collect();
    let change: f64 = step
        .iter()
        .enumerate()
        .map(|(j, delta)| {
            let cross: f64 = normal.gram[j * width..(j + 1) * width]
                .iter()
                .zip(&step)
                .map(|(g, d)| g * d)
                .sum();
            delta * (gradient[j] + 0.5 * cross)
        })
        .sum();
    change.is_finite() && change <= 0.0
}

enum Step {
    FreeMinimum,
    Blocking(usize),
}

fn feasible_step(previous: &[f64], candidate: &mut [f64], nonnegative: &[bool]) -> Option<Step> {
    let mut blocking: Option<(usize, f64)> = None;
    for (j, &bounded) in nonnegative.iter().enumerate() {
        if bounded && candidate[j] < 0.0 {
            let fraction = previous[j] / (previous[j] - candidate[j]);
            if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                return None;
            }
            if blocking.is_none_or(|(_, old)| fraction < old) {
                blocking = Some((j, fraction));
            }
        }
    }
    if let Some((column, fraction)) = blocking {
        for (value, old) in candidate.iter_mut().zip(previous) {
            *value = old + fraction * (*value - old);
        }
        candidate[column] = 0.0;
    }
    if candidate
        .iter()
        .zip(nonnegative)
        .any(|(&v, &bounded)| !v.is_finite() || (bounded && v < 0.0))
    {
        return None;
    }
    Some(blocking.map_or(Step::FreeMinimum, |(column, _)| Step::Blocking(column)))
}

// После лимита координатных проходов уточняем активные границы. Отрицательный
// свободный ответ даёт шаг только до ближайшей границы; затем отрицательный
// градиент на активной границе освобождает координату. Знаковые столбцы свободны.
// Ни один промежуточный шаг не публикуется: отказ сохраняет исходный результат.
fn polish(
    normal: &Normal,
    coefficients: &mut Vec<f64>,
    gradient: &mut Vec<f64>,
    nonnegative: &[bool],
    tolerance: f64,
) -> Option<f64> {
    let width = coefficients.len();
    let limit = width.checked_mul(2)?;
    let mut free: Vec<bool> = (0..width)
        .map(|j| normal.scales[j] > 0.0 && (!nonnegative[j] || coefficients[j] > 0.0))
        .collect();
    // Нужен отдельный кандидат, чтобы ограниченный поиск не портил результат при отказе.
    let mut previous = coefficients.clone();
    let mut previous_gradient = gradient.clone();
    for transition in 0..=limit {
        let mut candidate = free_solution(normal, &free)?;
        let blocking = feasible_step(&previous, &mut candidate, nonnegative)?;
        if !nonincreasing(normal, &previous, &previous_gradient, &candidate) {
            return None;
        }
        let mut next_gradient = vec![0.0; width];
        // Неконечный градиент отвергает только поправку, а не прежний конечный ответ.
        if refresh(normal, &candidate, &mut next_gradient).is_err() {
            return None;
        }
        let residual = projected(&next_gradient, &candidate, nonnegative);
        if residual <= tolerance {
            *coefficients = candidate;
            *gradient = next_gradient;
            return Some(residual);
        }
        if transition == limit {
            return None;
        }
        if let Step::Blocking(column) = blocking {
            free[column] = false;
        } else {
            let release = (0..width)
                .filter(|&j| nonnegative[j] && !free[j] && next_gradient[j] < -tolerance)
                .min_by(|&a, &b| next_gradient[a].total_cmp(&next_gradient[b]))?;
            free[release] = true;
        }
        previous = candidate;
        previous_gradient = next_gradient;
    }
    None
}

pub fn fit(
    rows: &[Vec<f64>],
    targets: &[f64],
    nonnegative: &[bool],
    penalties: &[f64],
    ridge: f64,
) -> Result<Fit, FitError> {
    let width = validate(rows, targets, nonnegative, penalties, ridge)?;
    let normal = normal(rows, targets, penalties, ridge)?;
    let rhs_scale = normal.rhs.iter().map(|b| b.abs()).fold(0.0, f64::max);
    let tolerance = RELATIVE_TOLERANCE * rhs_scale;
    let mut coefficients = vec![0.0; width];
    let mut gradient: Vec<f64> = normal.rhs.iter().map(|b| -b).collect();
    let mut residual = projected(&gradient, &coefficients, nonnegative);
    let mut iterations = 0;
    while residual > tolerance && iterations < MAX_ITERATIONS {
        for j in 0..width {
            let diagonal = normal.gram[j * width + j];
            if diagonal == 0.0 {
                continue;
            }
            let candidate = coefficients[j] - gradient[j] / diagonal;
            let next = if nonnegative[j] { candidate.max(0.0) } else { candidate };
            let change = next - coefficients[j];
            if !change.is_finite() {
                return Err(FitError::NumericalFailure);
            }
            coefficients[j] = next;
            if change != 0.0 {
                for (k, g) in gradient.iter_mut().enumerate() {
                    *g += normal.gram[k * width + j] * change;
                }
            }
        }
        iterations += 1;
        residual = projected(&gradient, &coefficients, nonnegative);
        // Перед заявлением о сходимости считаем точный градиент: накопление обновлений может скрыть остаток.
        if residual <= tolerance || iterations % REFRESH_EVERY == 0 || iterations == MAX_ITERATIONS {
            refresh(&normal, &coefficients, &mut gradient)?;
            residual = projected(&gradient, &coefficients, nonnegative);
        }
        if !residual.is_finite() {
            return Err(FitError::NumericalFailure);
        }
    }
    if residual > tolerance
        && let Some(improved) = polish(&normal, &mut coefficients, &mut gradient, nonnegative, tolerance)
    {
        residual = improved;
    }
    for (beta, scale) in coefficients.iter_mut().zip(&normal.scales) {
        *beta = if *scale > 0.0 { *beta / scale } else { 0.0 };
    }
    if coefficients.iter().any(|beta| !beta.is_finite()) {
        return Err(FitError::NumericalFailure);
    }
    Ok(Fit {
        coefficients,
        converged: residual <= tolerance,
        iterations,
        projected_gradient: residual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_sign_active_bounds_match_independent_scipy_bvls_oracle() {
        // Независимый SciPy BVLS: outputs/conditional-prototype/oracles/mixed-sign-active-bound.json.
        // Коррелированные фоновые и собственные столбцы; оба собственных коэффициента упираются в ноль.
        let rows = [
            [1.0, -1.0, -0.9, 0.1],
            [1.0, -0.5, -0.4, 0.2],
            [1.0, 0.0, 0.1, 0.7],
            [1.0, 0.5, 0.6, 0.1],
            [1.0, 1.0, 1.1, 0.5],
            [1.0, 1.5, 1.4, 0.8],
        ]
        .map(Vec::from);
        let target = [3.0, 2.2, 1.1, 0.5, -0.4, -1.2];
        let penalties = [0.0, 0.03, 0.1, 0.05];
        let result = fit(&rows, &target, &[false, false, true, true], &penalties, 1.0).expect("подгонка");
        assert!(result.converged, "{result:?}");
        for (actual, expected) in
            result
                .coefficients
                .iter()
                .zip([1.270_069_520_673_253_3, -1.613_611_416_026_345_3, 0.0, 0.0])
        {
            assert!((actual - expected).abs() < 1e-7, "{result:?}");
        }
        let loss = rows
            .iter()
            .zip(target)
            .map(|(x, y)| {
                let p = result.predict(x).expect("конечный прогноз");
                (p * p - 2.0 * p * y) / 12.0
            })
            .sum::<f64>()
            + result
                .coefficients
                .iter()
                .zip(penalties)
                .map(|(beta, p)| 0.5 * p * beta * beta)
                .sum::<f64>();
        assert!((loss - (-1.363_892_547_871_691)).abs() < 1e-10, "цель {loss}");
        assert!(result.projected_gradient < 2e-8);
    }

    #[test]
    fn ill_conditioned_unpenalized_fit_reports_iteration_limit_without_claiming_convergence() {
        let rows = vec![vec![1.0, 1.0], vec![0.0, 0.0001]];
        let result = fit(&rows, &[0.0, 1.0], &[false, false], &[0.0, 0.0], 0.0).expect("конечный расчёт");
        assert!(!result.converged, "{result:?}");
        assert_eq!(result.iterations, MAX_ITERATIONS);
        assert!(result.projected_gradient > 1e-6);
    }

    #[test]
    fn caller_ridge_stabilizes_collinear_columns() {
        let rows = vec![vec![1.0, 1.0], vec![2.0, 2.0], vec![3.0, 3.0]];
        let result = fit(&rows, &[2.0, 4.0, 6.0], &[false, true], &[1.0, 1.0], 1.0).expect("подгонка");
        assert!(result.converged, "{result:?}");
        // Две одинаковые колонки и одинаковый штраф дают одинаковые коэффициенты: (28/3)/(31/3) = 28/31.
        for beta in result.coefficients {
            assert!((beta - 28.0 / 31.0).abs() < 1e-7);
        }
    }
}
