// Память измеренных состояний: потолки — контекст команды, не ключ достигнутой частоты.
use machine::auto::Cause;
use machine::thermal::Reading;
use serde_json::{Value, json};

#[cfg(test)]
#[path = "configurations_spec.rs"]
mod contract_spec;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Instant,
    Window,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Instant => "instant",
            Self::Window => "window",
        }
    }
    fn read(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "instant" => Some(Self::Instant),
            "window" => Some(Self::Window),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rule {
    pub limit: i32,
    pub emergency: i32,
    pub window_secs: i32,
}
impl Rule {
    fn value(self) -> Value {
        json!({ "limit": self.limit, "emergency": self.emergency, "windowSecs": self.window_secs })
    }
    fn read(value: &Value) -> Option<Self> {
        let integer = |key: &str| value[key].as_i64().and_then(|v| i32::try_from(v).ok());
        let rule = Self {
            limit: integer("limit")?,
            emergency: integer("emergency")?,
            window_secs: integer("windowSecs")?,
        };
        ((60..=95).contains(&rule.limit)
            && rule.emergency > rule.limit
            && rule.emergency <= 100
            && (1..=60).contains(&rule.window_secs))
        .then_some(rule)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RatioState {
    Unknown,
    Idle,
    Step(i32),
}
fn actual_ratio(ratio: Option<f64>) -> Option<f64> {
    ratio.filter(|r| r.is_finite() && *r > 0.0 && *r < f64::from(i32::MAX))
}
#[expect(
    clippy::cast_possible_truncation,
    reason = "Положительный множитель проверен на конечность и границы i32; владелец явно выбрал округление .5 вверх"
)]
fn nearest_step(ratio: f64) -> i32 {
    ratio.round() as i32
}
fn window_state(reading: Reading) -> RatioState {
    RatioState::observed(reading.temp.filter(|v| v.is_finite()).map(|_| reading))
}
impl RatioState {
    fn observed(reading: Option<Reading>) -> Self {
        let Some(reading) = reading else { return Self::Unknown };
        if let Some(ratio) = actual_ratio(reading.ratio) {
            return Self::Step(nearest_step(ratio));
        }
        if reading.ratio.is_none() && reading.busy == Some(0.0) {
            Self::Idle
        } else {
            Self::Unknown
        }
    }
    fn value(self) -> Value {
        match self {
            Self::Unknown => Value::Null,
            Self::Idle => json!("idle"),
            Self::Step(step) => json!(step),
        }
    }
    fn read(value: &Value) -> Option<Self> {
        if value.is_null() {
            Some(Self::Unknown)
        } else if value == "idle" {
            Some(Self::Idle)
        } else {
            value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v >= 0)
                .map(Self::Step)
        }
    }
    fn known_difference(self, other: Self) -> bool {
        self != Self::Unknown && other != Self::Unknown && self != other
    }
}
#[derive(Debug)]
struct Profile {
    block: usize,
    kind: Kind,
    ratio: RatioState,
    window_secs: Option<i32>,
    max: f64,
    snapshot: Rc<Value>,
}
#[derive(Debug)]
struct Failure {
    block: usize,
    kind: Kind,
    ratio: Option<f64>,
    peers: Vec<RatioState>,
    rule: Rule,
    snapshot: Rc<Value>,
}
#[derive(Debug, Clone, Copy)]
pub(super) struct Trigger {
    pub block: usize,
    pub cause: Cause,
    pub kind: Kind,
    pub ratio: Option<f64>,
}

#[derive(Debug, Default)]
pub(super) struct Ledger {
    layout: String,
    profiles: Vec<Profile>,
    failures: Vec<Failure>,
    configured_window_secs: Option<i32>,
    pub changed: bool,
    pub failure_changed: bool,
    pub blocked: bool,
}
impl Ledger {
    pub fn initialize(&mut self, layout: String) {
        self.layout = layout;
    }

    pub fn set_window_secs(&mut self, seconds: i32) {
        self.configured_window_secs = Some(seconds);
    }
    /// Снимок строится один раз на опрос, только при новом максимуме хотя бы одной собственной корзины.
    pub fn observe(
        &mut self,
        kind: Kind,
        readings: &[Option<Reading>],
        build: impl FnOnce() -> Value,
    ) -> Option<Rc<Value>> {
        let window_secs = (kind == Kind::Window).then_some(self.configured_window_secs.unwrap_or(2));
        let grows = readings.iter().enumerate().any(|(block, reading)| {
            let Some(temp) = reading.and_then(|r| r.temp).filter(|v| v.is_finite()) else {
                return false;
            };
            let ratio = RatioState::observed(*reading);
            self.profiles
                .iter()
                .find(|p| p.block == block && p.kind == kind && p.ratio == ratio && p.window_secs == window_secs)
                .is_none_or(|p| temp > p.max)
        });
        if !grows {
            return None;
        }
        let snapshot = Rc::new(build());
        for (block, reading) in readings.iter().enumerate() {
            let Some(temp) = reading.and_then(|r| r.temp).filter(|v| v.is_finite()) else {
                continue;
            };
            let ratio = RatioState::observed(*reading);
            if let Some(profile) = self
                .profiles
                .iter_mut()
                .find(|p| p.block == block && p.kind == kind && p.ratio == ratio && p.window_secs == window_secs)
            {
                if temp > profile.max {
                    profile.max = temp;
                    profile.snapshot = Rc::clone(&snapshot);
                }
            } else {
                self.profiles.push(Profile {
                    block,
                    kind,
                    ratio,
                    window_secs,
                    max: temp,
                    snapshot: Rc::clone(&snapshot),
                });
            }
        }
        self.changed = true;
        Some(snapshot)
    }
    pub fn failure(&mut self, trigger: Trigger, readings: &[Option<Reading>], rule: Rule, snapshot: Rc<Value>) -> bool {
        let Trigger {
            block,
            cause,
            kind,
            ratio: raw_ratio,
        } = trigger;
        if !matches!(
            (cause, kind),
            (Cause::Emergency, Kind::Instant) | (Cause::Target, Kind::Window)
        ) || block >= readings.len()
        {
            return false;
        }
        let ratio = actual_ratio(raw_ratio);
        let own_bin = |ratio: Option<f64>| ratio.map_or(RatioState::Unknown, |r| RatioState::Step(nearest_step(r)));
        let same = |f: &&mut Failure| {
            f.block == block
                && f.kind == kind
                && own_bin(f.ratio) == own_bin(ratio)
                && f.rule == rule
                && f.peers
                    .iter()
                    .copied()
                    .zip(readings)
                    .enumerate()
                    .all(|(b, (saved, current))| b == block || saved == RatioState::observed(*current))
                && f.peers.len() == readings.len()
        };
        if let Some(failure) = self.failures.iter_mut().find(same) {
            if let (Some(old), Some(new)) = (failure.ratio, ratio)
                && new < old
            {
                failure.ratio = ratio;
                failure.snapshot = snapshot;
                self.changed = true;
                self.failure_changed = true;
                return true;
            }
            return false;
        }
        self.failures.push(Failure {
            block,
            kind,
            ratio,
            peers: readings.iter().map(|r| RatioState::observed(*r)).collect(),
            rule,
            snapshot,
        });
        self.changed = true;
        self.failure_changed = true;
        true
    }
    /// Командный потолок лишь допускает известную горячую частоту, но не предсказывает её достижение.
    #[cfg(test)]
    pub fn allows(
        &self,
        block: usize,
        to: i32,
        instant: &[Option<Reading>],
        window: Option<&[Option<Reading>]>,
        rule: Rule,
    ) -> bool {
        let window: Vec<Reading> = window
            .unwrap_or_default()
            .iter()
            .map(|r| r.unwrap_or_default())
            .collect();
        self.allows_closed(block, to, instant, &window, rule)
    }
    pub fn allows_closed(
        &self,
        block: usize,
        to: i32,
        instant: &[Option<Reading>],
        window: &[Reading],
        rule: Rule,
    ) -> bool {
        self.allows_with(block, to, rule, |kind, peers| match kind {
            Kind::Instant => {
                instant.len() == peers.len()
                    && instant
                        .iter()
                        .zip(peers)
                        .enumerate()
                        .any(|(b, (now, saved))| b != block && RatioState::observed(*now).known_difference(*saved))
            }
            Kind::Window => {
                window.len() == peers.len()
                    && window
                        .iter()
                        .zip(peers)
                        .enumerate()
                        .any(|(b, (now, saved))| b != block && window_state(*now).known_difference(*saved))
            }
        })
    }
    fn allows_with(&self, block: usize, to: i32, rule: Rule, differs: impl Fn(Kind, &[RatioState]) -> bool) -> bool {
        !self.blocked
            && !self.failures.iter().any(|f| {
                f.block == block
                    && f.rule == rule
                    && f.ratio.is_none_or(|r| f64::from(to) >= r)
                    && !differs(f.kind, &f.peers)
            })
    }
    pub fn reset_statistics(&mut self) {
        self.profiles.clear();
        self.changed = true;
    }
    pub fn failures(&self, rule: Rule) -> usize {
        self.failures.iter().filter(|f| f.rule == rule).count()
    }
    pub fn value(&self) -> Result<Value, String> {
        if self.blocked {
            return Err("непрочитанная память физических состояний сохранена без перезаписи".into());
        }
        let profiles: Vec<Value> = self.profiles.iter().map(|p| json!({ "block": p.block, "kind": p.kind.name(), "windowSecs": p.window_secs, "ratioState": p.ratio.value(), "max": p.max, "snapshot": p.snapshot.as_ref() })).collect();
        let failures: Vec<Value> = self.failures.iter().map(|f| json!({ "block": f.block, "kind": f.kind.name(), "ratio": f.ratio, "peerStates": f.peers.iter().map(|s| s.value()).collect::<Vec<_>>(), "rule": f.rule.value(), "snapshot": f.snapshot.as_ref() })).collect();
        Ok(
            json!({ "version": "physical-profiles-nearest-step-v1", "layout": self.layout, "profiles": profiles, "failures": failures }),
        )
    }
    pub fn restore(&mut self, value: &Value, blocks: usize) -> Result<(), String> {
        if value["version"] != "physical-profiles-nearest-step-v1"
            || value["layout"].as_str() != Some(self.layout.as_str())
        {
            return Err("память физических состояний относится к другому процессору, раскладке или формату".into());
        }
        let invalid = || "повреждена память физических состояний".to_owned();
        let mut saved = Self::default();
        saved.initialize(self.layout.clone()); // один раз при запуске
        for p in value["profiles"].as_array().ok_or_else(invalid)? {
            let block = p["block"]
                .as_u64()
                .and_then(|b| usize::try_from(b).ok())
                .filter(|b| *b < blocks)
                .ok_or_else(invalid)?;
            let kind = Kind::read(&p["kind"]).ok_or_else(invalid)?;
            let ratio = RatioState::read(&p["ratioState"]).ok_or_else(invalid)?;
            let max = p["max"].as_f64().filter(|v| v.is_finite()).ok_or_else(invalid)?;
            let window_secs = if kind == Kind::Window {
                Some(
                    p["windowSecs"]
                        .as_i64()
                        .and_then(|v| i32::try_from(v).ok())
                        .filter(|v| (1..=60).contains(v))
                        .ok_or_else(invalid)?,
                )
            } else {
                None
            };
            if !p["snapshot"].is_object() {
                return Err(invalid());
            }
            saved.profiles.push(Profile {
                block,
                kind,
                ratio,
                window_secs,
                max,
                snapshot: Rc::new(p["snapshot"].clone()),
            });
        }
        for f in value["failures"].as_array().ok_or_else(invalid)? {
            let block = f["block"]
                .as_u64()
                .and_then(|b| usize::try_from(b).ok())
                .filter(|b| *b < blocks)
                .ok_or_else(invalid)?;
            let kind = Kind::read(&f["kind"]).ok_or_else(invalid)?;
            let ratio = if f["ratio"].is_null() {
                None
            } else {
                Some(actual_ratio(f["ratio"].as_f64()).ok_or_else(invalid)?)
            };
            let peers = f["peerStates"]
                .as_array()
                .ok_or_else(invalid)?
                .iter()
                .map(|v| RatioState::read(v).ok_or_else(invalid))
                .collect::<Result<Vec<_>, _>>()?;
            if peers.len() != blocks || !f["snapshot"].is_object() {
                return Err(invalid());
            }
            saved.failures.push(Failure {
                block,
                kind,
                ratio,
                peers,
                rule: Rule::read(&f["rule"]).ok_or_else(invalid)?,
                snapshot: Rc::new(f["snapshot"].clone()),
            });
        }
        *self = saved;
        Ok(())
    }
}
