// Общая геометрия графиков: диапазоны в исходных единицах и доли поля снизу вверх.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

impl Range {
    pub fn new(min: f64, max: f64) -> Option<Self> {
        (min.is_finite() && max.is_finite() && max > min && (max - min).is_finite()).then_some(Self { min, max })
    }

    pub fn fit(values: impl IntoIterator<Item = f64>) -> Option<Self> {
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        for value in values.into_iter().filter(|v| v.is_finite()) {
            low = low.min(value);
            high = high.max(value);
        }
        if !low.is_finite() {
            return None;
        }
        let padding = if high > low {
            (high - low) * 0.05
        } else {
            low.abs().max(1.0) * 0.02
        };
        Self::new(low - padding, high + padding)
    }

    pub fn normal(self, value: f64) -> Option<f64> {
        let fraction = (value - self.min) / (self.max - self.min);
        fraction.is_finite().then_some(fraction)
    }

    pub fn value(self, fraction: f64) -> Option<f64> {
        let value = self.min + fraction * (self.max - self.min);
        value.is_finite().then_some(value)
    }

    pub fn zoom(self, anchor: f64, factor: f64) -> Option<Self> {
        if !anchor.is_finite() || !factor.is_finite() || factor <= 0.0 {
            return None;
        }
        let anchor = anchor.clamp(0.0, 1.0);
        let physical = self.value(anchor)?;
        let span = (self.max - self.min) * factor;
        Self::new(physical - anchor * span, physical + (1.0 - anchor) * span)
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "finite minutes checked against the signed millisecond range before conversion"
    )]
    pub fn milliseconds(self) -> Option<(i64, i64)> {
        let a = (self.min * 60_000.0).round();
        let b = (self.max * 60_000.0).round();
        if !a.is_finite()
            || !b.is_finite()
            || a <= i64::MIN as f64
            || b >= i64::MAX as f64
            || b <= a
            || b - a >= i64::MAX as f64
        {
            return None;
        }
        Some((a as i64, b as i64))
    }

    #[cfg(test)]
    pub fn pan(self, fraction: f64) -> Option<Self> {
        let shift = fraction * (self.max - self.min);
        Self::new(self.min + shift, self.max + shift)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub argument: Range,
    pub value: Range,
    pub swap: bool,
}

impl Viewport {
    pub fn point(self, argument: f64, value: f64) -> Option<(f64, f64)> {
        let point = (self.argument.normal(argument)?, self.value.normal(value)?);
        Some(if self.swap { (point.1, point.0) } else { point })
    }

    pub fn segment(self, a: (f64, f64), b: (f64, f64)) -> Option<((f64, f64), (f64, f64))> {
        let a = self.point(a.0, a.1)?;
        let b = self.point(b.0, b.1)?;
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        let (mut enter, mut leave): (f64, f64) = (0.0, 1.0);
        for (p, q) in [(-dx, a.0), (dx, 1.0 - a.0), (-dy, a.1), (dy, 1.0 - a.1)] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
            } else {
                let t = q / p;
                if p < 0.0 {
                    enter = enter.max(t);
                } else {
                    leave = leave.min(t);
                }
                if enter > leave {
                    return None;
                }
            }
        }
        Some((
            (a.0 + enter * dx, a.1 + enter * dy),
            (a.0 + leave * dx, a.1 + leave * dy),
        ))
    }
}

#[cfg(test)]
#[path = "plot_spec.rs"]
mod spec;
