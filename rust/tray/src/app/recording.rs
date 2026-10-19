// Доказательство намеренной фильтрации между двумя успешно записанными строками.
use machine::thermal::Reading;
use std::time::Duration;

#[derive(Debug, Default)]
pub(super) struct FilterTrace {
    stamp: String,
    written_at: Option<Duration>,
    checked_at: Option<Duration>,
}
impl FilterTrace {
    pub fn written(&mut self, now: Duration, stamp: &str) {
        self.stamp.clear();
        self.stamp.push_str(stamp);
        self.written_at = Some(now);
        self.checked_at = None;
    }
    pub fn filtered(&mut self, now: Duration, max_gap: Duration) {
        let previous = self.checked_at.or(self.written_at);
        if previous
            .and_then(|at| now.checked_sub(at))
            .is_some_and(|age| age <= max_gap)
        {
            self.checked_at = Some(now);
        } else {
            self.disrupt();
        }
    }
    pub fn token(&self, now: Duration, max_gap: Duration) -> Option<&str> {
        let age = now.checked_sub(self.checked_at?)?;
        (self.written_at.is_some() && age <= max_gap).then_some(self.stamp.as_str())
    }
    pub fn disrupt(&mut self) {
        self.written_at = None;
        self.checked_at = None;
        self.stamp.clear();
    }
}

fn values(reading: Reading) -> Option<(f64, f64)> {
    Some((
        reading.busy.filter(|v| v.is_finite() && (0.0..=1.0).contains(v))?,
        reading.temp.filter(|v| v.is_finite())?,
    ))
}
pub(super) fn complete(mut readings: impl Iterator<Item = Reading>) -> bool {
    readings.all(|r| values(r).is_some())
}

/// Порог отбора по ядрам: ниже минимальных 42 % у ядра в имеющихся тепловых событиях; не закон нагрева.
pub(super) fn relevant(readings: impl Iterator<Item = Reading>, target: f64, thermal_action: bool) -> bool {
    if thermal_action {
        return true;
    }
    let mut known = false;
    for r in readings {
        known = true;
        let Some((busy, temp)) = values(r) else { return true };
        if busy >= 0.4 || temp >= target {
            return true;
        }
    }
    !known
}

#[cfg(test)]
#[path = "recording_spec.rs"]
mod contract;
