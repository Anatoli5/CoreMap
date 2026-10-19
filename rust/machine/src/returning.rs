//! Сохранённое ограничение и проверка возврата под наблюдаемой нагрузкой.

use std::time::Duration;

/// Одновременные признаки нагрузки; относительная доля считается только среди блоков одного типа.
#[derive(Debug, Clone, Copy)]
pub struct LoadFrame<'a> {
    pub global_busy: Option<f64>,
    pub relative: &'a [Option<f64>],
}

/// Контекст работы при отсечке. Это активность счётчиков, не производительность приложения.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Workload {
    pub busy: f64,
    pub ratio: f64,
    pub global_busy: f64,
    pub relative: f64,
}

impl Workload {
    pub(crate) fn valid(self) -> bool {
        [self.busy, self.global_busy, self.relative]
            .into_iter()
            .all(|v| v.is_finite() && v > 0.0 && v <= 1.0)
            && self.ratio.is_finite()
            && self.ratio > 0.0
    }

    fn covers(self, reference: Self, ceiling: i32) -> bool {
        self.valid()
            && reference.valid()
            && self.busy >= reference.busy
            && self.global_busy >= reference.global_busy
            && self.relative >= reference.relative
            // Допуск — одна целая ступень множителя; это выбор дискретизации, не измеренная точность счётчика.
            && self.ratio >= f64::from(ceiling - 1)
    }
}

/// Незавершённая проверка одной ступени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trial {
    pub from: i32,
    pub to: i32,
}

/// Сохраняемая защита; held — последний подтверждённый или уменьшенный потолок.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Protection {
    pub held: i32,
    /// Консервативная покомпонентная граница нагрузки всех отсечек, а не одна измеренная ситуация.
    /// ratio сохраняет максимум для объяснения; сравнение частоты использует достижимый текущий потолок.
    pub reference: Option<Workload>,
    pub trial: Option<Trial>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Guard {
    pub protection: Option<Protection>,
    eligible_since: Option<Duration>,
    trial_since: Option<Duration>,
}

impl Guard {
    pub fn reset_episode(&mut self) {
        self.eligible_since = None;
        self.trial_since = None;
    }

    pub fn seed(&mut self, protection: Protection) {
        self.protection = Some(Protection {
            reference: protection.reference.filter(|w| w.valid()),
            ..protection
        });
        self.eligible_since = None;
        self.trial_since = None;
    }

    pub fn cut(&mut self, to: i32, reference: Option<Workload>) {
        let reference = reference.filter(|w| w.valid());
        let prior = self.protection.and_then(|p| p.reference);
        self.protection = Some(Protection {
            held: to,
            reference: match (prior, reference) {
                (Some(a), Some(b)) => Some(Workload {
                    busy: a.busy.max(b.busy),
                    ratio: a.ratio.max(b.ratio),
                    global_busy: a.global_busy.max(b.global_busy),
                    relative: a.relative.max(b.relative),
                }),
                (_, None) => None,
                (None, Some(b)) => Some(b),
            },
            trial: None,
        });
        self.eligible_since = None;
        self.trial_since = None;
    }

    /// Полное окно относится к одному потолку. None означает, что ступень пока не разрешена.
    pub fn window(
        &mut self,
        now: Duration,
        start: Duration,
        pause: Duration,
        ceiling: i32,
        samples: &[Option<Workload>],
        cool: bool,
    ) -> Option<i32> {
        let p = self.protection.as_mut()?;
        let expected = p.trial.map_or(p.held, |t| t.to);
        if ceiling != expected {
            self.eligible_since = None;
            return None;
        }
        let qualifies = cool
            && !samples.is_empty()
            && p.reference.is_some_and(|reference| {
                samples.iter().all(|w| {
                    w.is_some_and(|w| {
                        w.covers(reference, ceiling) && p.trial.is_none_or(|trial| w.ratio > f64::from(trial.from))
                    })
                })
            });
        if qualifies {
            self.eligible_since.get_or_insert(start);
        } else {
            self.eligible_since = None;
        }
        let ready = self.eligible_since.is_some_and(|t| now.saturating_sub(t) >= pause);
        if let Some(trial) = p.trial {
            let trial_since = *self.trial_since.get_or_insert(start);
            if ready {
                p.held = trial.to;
                p.trial = None;
                self.trial_since = None;
                self.eligible_since = None;
            } else if now.saturating_sub(trial_since) >= pause {
                p.trial = None;
                self.trial_since = None;
                self.eligible_since = None;
                return Some(trial.from);
            }
        }
        None
    }

    pub fn ready(&self, now: Duration, pause: Duration, ceiling: i32) -> bool {
        self.protection.is_some_and(|p| p.held == ceiling && p.trial.is_none())
            && self.eligible_since.is_some_and(|t| now.saturating_sub(t) >= pause)
    }

    pub fn begin(&mut self, from: i32) {
        if let Some(p) = self.protection.as_mut() {
            p.trial = Some(Trial { from, to: from + 1 });
            self.trial_since = None;
            self.eligible_since = None;
        }
    }
}
