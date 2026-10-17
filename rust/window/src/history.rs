// История одного измеряемого параметра по физическому времени. Пропущенная строка медленного датчика — не новое
// измерение: вызывающий код её не добавляет. Явное неизвестное значение прерывает удержание (например, после разрыва
// записи). Удержание ограничено давностью; неизвестная часть окна не становится нулём или наблюдённым состоянием.
// Префиксные интегралы строятся один раз при подготовке данных модели и дают средние окон без повторного обхода строк.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    InvalidHold,
    NonIncreasingTime,
    NonFiniteValue,
    InvalidWindow,
    TimeOverflow,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub value: f64,
    pub observed_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub mean: Option<f64>,
    pub covered_ms: i64,
    pub span_ms: i64,
}

impl Window {
    pub fn coverage(&self) -> f64 {
        milliseconds(self.covered_ms) / milliseconds(self.span_ms)
    }
}

#[derive(Clone, Copy, Debug)]
struct Point {
    at: i64,
    value: Option<f64>,
    area: f64,
    covered: i64,
}

#[derive(Clone, Debug)]
pub struct Signal {
    hold_ms: i64,
    points: Vec<Point>,
}

// Приводятся длительности окна и удержания, а не абсолютное время календаря: точности f64 для миллисекунд достаточно.
#[expect(
    clippy::cast_precision_loss,
    reason = "durations in milliseconds, not absolute epoch timestamps"
)]
fn milliseconds(value: i64) -> f64 {
    value as f64
}

impl Signal {
    pub fn new(max_hold_ms: i64) -> Result<Self, HistoryError> {
        if max_hold_ms <= 0 {
            return Err(HistoryError::InvalidHold);
        }
        Ok(Self {
            hold_ms: max_hold_ms,
            points: Vec::new(),
        })
    }

    pub fn push(&mut self, at_ms: i64, value: Option<f64>) -> Result<(), HistoryError> {
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(HistoryError::NonFiniteValue);
        }
        let (area, covered) = if let Some(last) = self.points.last() {
            if at_ms <= last.at {
                return Err(HistoryError::NonIncreasingTime);
            }
            let elapsed = at_ms.checked_sub(last.at).ok_or(HistoryError::TimeOverflow)?;
            let known = if last.value.is_some() {
                elapsed.min(self.hold_ms)
            } else {
                0
            };
            (
                last.area + last.value.unwrap_or(0.0) * milliseconds(known),
                last.covered.checked_add(known).ok_or(HistoryError::TimeOverflow)?,
            )
        } else {
            (0.0, 0)
        };
        self.points.push(Point {
            at: at_ms,
            value,
            area,
            covered,
        });
        Ok(())
    }

    fn before(&self, at_ms: i64) -> Option<&Point> {
        let end = self.points.partition_point(|p| p.at <= at_ms);
        end.checked_sub(1).and_then(|i| self.points.get(i))
    }

    #[cfg(test)]
    pub fn at(&self, at_ms: i64) -> Option<Reading> {
        let p = self.before(at_ms)?;
        let age = at_ms.checked_sub(p.at)?;
        (age < self.hold_ms).then_some(Reading {
            value: p.value?,
            observed_at: p.at,
        })
    }

    fn integral(&self, at_ms: i64) -> Result<(f64, i64), HistoryError> {
        let Some(p) = self.before(at_ms) else {
            return Ok((0.0, 0));
        };
        let elapsed = at_ms.checked_sub(p.at).ok_or(HistoryError::TimeOverflow)?;
        let known = if p.value.is_some() {
            elapsed.min(self.hold_ms)
        } else {
            0
        };
        Ok((
            p.area + p.value.unwrap_or(0.0) * milliseconds(known),
            p.covered.checked_add(known).ok_or(HistoryError::TimeOverflow)?,
        ))
    }

    pub fn window(&self, now_ms: i64, recent_ms: i64, oldest_ms: i64) -> Result<Window, HistoryError> {
        if recent_ms < 0 || oldest_ms <= recent_ms {
            return Err(HistoryError::InvalidWindow);
        }
        let start = now_ms.checked_sub(oldest_ms).ok_or(HistoryError::TimeOverflow)?;
        let end = now_ms.checked_sub(recent_ms).ok_or(HistoryError::TimeOverflow)?;
        let (before, covered_before) = self.integral(start)?;
        let (after, covered_after) = self.integral(end)?;
        let covered_ms = covered_after - covered_before;
        Ok(Window {
            mean: (covered_ms > 0).then(|| (after - before) / milliseconds(covered_ms)),
            covered_ms,
            span_ms: oldest_ms - recent_ms,
        })
    }
}
