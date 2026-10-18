use super::{BTreeMap, Quantity, Range, Unit, View};

impl View {
    pub(super) fn range_key(&self, argument: bool) -> String {
        match (self.timeline, argument) {
            (false, true) => "ratio".into(),
            (false, false) => format!("ratio-value:{}", self.scale.index()),
            (true, true) => format!("time:{}", self.days().1.unwrap_or("")),
            (true, false) if self.normalized => format!("parameter:{}", self.range_parameter.index()),
            (true, false) => format!("time-value:{}", self.range_unit.index()),
        }
    }

    pub(super) fn time_ranges(&self) -> BTreeMap<Unit, Range> {
        Unit::ALL
            .into_iter()
            .filter_map(|unit| {
                self.ranges
                    .get(&format!("time-value:{}", unit.index()))
                    .map(|r| (unit, *r))
            })
            .collect()
    }

    #[expect(clippy::cast_precision_loss, reason = "milliseconds within one recorded day")]
    fn resolved_range(&self, argument: bool) -> Option<Range> {
        if let Some(range) = self.ranges.get(&self.range_key(argument)) {
            return Some(*range);
        }
        if !self.timeline {
            let shown = self.shown_stats.as_ref()?;
            return if argument {
                shown.argument_range
            } else {
                shown.value_range
            };
        }
        if argument {
            let (min, max) = self.span?;
            return Range::new(min as f64 / 60_000.0, max as f64 / 60_000.0);
        }
        if let Some(range) = self.ranges.get(&self.range_key(false)) {
            return Some(*range);
        }
        if self.normalized {
            return self
                .shown_time
                .as_ref()?
                .parameter_ranges
                .get(&self.range_parameter)
                .copied();
        }
        self.shown_time.as_ref()?.strips.iter().find_map(|strip| {
            let pair = if strip.unit == Some(self.range_unit) {
                strip.range
            } else if strip.right_unit == Some(self.range_unit) {
                strip.right_range
            } else {
                None
            };
            pair.and_then(|(min, max)| Range::new(min, max))
        })
    }

    pub(super) fn show_ranges(&self) {
        let ui = self.ui();
        ui.set_range_auto_arg(!self.ranges.contains_key(&self.range_key(true)));
        ui.set_range_auto_value(!self.ranges.contains_key(&self.range_key(false)));
        ui.set_range_title_arg(
            if self.timeline {
                "Время записи, мин"
            } else {
                "Множитель, ×"
            }
            .into(),
        );
        ui.set_range_title_value(if self.timeline && self.normalized {
            self.range_parameter.title().into()
        } else if self.timeline {
            self.range_unit.title().into()
        } else {
            self.shown_stats
                .as_ref()
                .map_or("Значение", |s| s.value_title.as_str())
                .into()
        });
        ui.set_range_unit(self.range_unit.index());
        ui.set_range_parameter(self.range_parameter.index());
        ui.set_normalized(self.normalized);
        self.show_navigation();
    }

    pub(super) fn refresh_ranges(&mut self) {
        if self.timeline {
            self.span = match self.ranges.get(&self.range_key(true)) {
                Some(range) => range.milliseconds(),
                None => self.day_data.as_ref().and_then(|d| d.bounds()),
            };
            self.anchor_following_window();
            self.refresh_time();
        } else {
            self.refresh_stats();
        }
        self.show_ranges();
    }

    pub fn range_fit(&mut self, argument: bool) {
        let key = self.range_key(argument);
        if self.timeline && argument {
            self.time_follow = true;
        }
        let previous = self.ranges.remove(&key);
        self.refresh_ranges();
        if let Some(old) = previous {
            let range = self.resolved_range(argument).unwrap_or(old);
            self.ranges.insert(key, range);
            self.refresh_ranges();
        }
        self.remember();
    }

    pub fn range_auto(&mut self, argument: bool, auto: bool) {
        let key = self.range_key(argument);
        if auto {
            self.ranges.remove(&key);
        } else if let Some(range) = self.resolved_range(argument) {
            self.ranges.insert(key, range);
        }
        if self.timeline && argument && auto {
            self.time_follow = true;
        }
        self.refresh_ranges();
        self.remember();
    }

    pub fn range_zoom(&mut self, argument: bool, factor: f32) {
        self.zoom_axis(argument, f64::from(factor));
    }

    pub fn range_wheel(&mut self, argument: bool, delta: f32) {
        if delta.is_finite() && delta != 0.0 {
            self.zoom_axis(argument, (-f64::from(delta) / 600.0).exp());
        }
    }

    fn zoom_axis(&mut self, argument: bool, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        if let Some(range) = self
            .resolved_range(argument)
            .and_then(|r| {
                if argument && self.timeline {
                    self.zoom_time_range(r, factor)
                } else {
                    r.zoom(0.5, factor)
                }
            })
            .filter(|r| !self.timeline || !argument || r.milliseconds().is_some())
        {
            self.range_set(argument, range);
        }
    }

    pub fn range_set(&mut self, argument: bool, range: Range) {
        if self.timeline && argument {
            self.time_follow = self.docking_range(range);
        }
        self.ranges.insert(self.range_key(argument), range);
        self.refresh_ranges();
        self.remember();
    }

    pub fn range_unit_changed(&mut self, index: i32) {
        self.range_unit = Unit::of(index);
        self.show_ranges();
    }

    pub(super) fn parameter_ranges(&self) -> BTreeMap<Quantity, Range> {
        Quantity::ALL
            .into_iter()
            .filter_map(|q| self.ranges.get(&format!("parameter:{}", q.index())).map(|r| (q, *r)))
            .collect()
    }

    pub(super) fn metric_tip(&self, quantity: Quantity) -> String {
        let range = self
            .ranges
            .get(&format!("parameter:{}", quantity.index()))
            .copied()
            .or_else(|| self.shown_time.as_ref()?.parameter_ranges.get(&quantity).copied());
        let limits = range
            .map(|r| {
                format!(
                    "\nПоказанный диапазон: {:.3}…{:.3} {}",
                    r.min,
                    r.max,
                    quantity.unit().title()
                )
            })
            .unwrap_or_default();
        format!(
            "{} · {}\nЩелчок — показать или скрыть. В относительном режиме колесо меняет только масштаб этой величины; ↺ возвращает Авто.{}",
            quantity.title(),
            quantity.line_hint(),
            limits
        )
    }

    pub fn normalized_changed(&mut self, on: bool) {
        self.normalized = on;
        self.show_mode();
        self.refresh_time();
        self.remember();
    }

    pub fn range_parameter_changed(&mut self, index: i32) {
        self.range_parameter = Quantity::of(index);
        self.show_ranges();
    }

    pub fn metric_zoom(&mut self, index: i32, delta: f32) {
        if !self.normalized || !delta.is_finite() {
            return;
        }
        let quantity = Quantity::of(index);
        let key = format!("parameter:{}", quantity.index());
        let base = self
            .ranges
            .get(&key)
            .copied()
            .or_else(|| self.shown_time.as_ref()?.parameter_ranges.get(&quantity).copied());
        if let Some(range) = base.and_then(|r| r.zoom(0.5, (-f64::from(delta) / 600.0).exp())) {
            self.ranges.insert(key, range);
            self.refresh_time();
            self.show_mode();
            self.remember();
        }
    }

    pub fn metric_reset(&mut self, index: i32) {
        self.ranges
            .remove(&format!("parameter:{}", Quantity::of(index).index()));
        self.refresh_time();
        self.show_mode();
        self.remember();
    }

    pub fn metric_toggled(&mut self, index: i32) {
        let quantity = Quantity::of(index);
        if let Some(at) = self.selected.iter().position(|q| *q == quantity) {
            self.selected.remove(at);
        } else {
            self.selected.push(quantity);
            self.quantity = quantity;
        }
        self.show_mode();
        self.ask_day();
        self.refresh_time();
        self.remember();
    }

    #[expect(clippy::cast_precision_loss, reason = "milliseconds within one recorded day")]
    pub(super) fn remember_time_span(&mut self) {
        if let Some((min, max)) = self
            .span
            .and_then(|(a, b)| Range::new(a as f64 / 60_000.0, b as f64 / 60_000.0).map(|r| (r.min, r.max)))
        {
            self.ranges.insert(self.range_key(true), Range { min, max });
        }
        self.show_ranges();
    }
}
