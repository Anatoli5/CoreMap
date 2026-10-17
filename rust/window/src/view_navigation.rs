use super::View;
use crate::plot::Range;
use crate::timeline;

impl View {
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "normalized position of a recorded day, bounded to 0..1"
    )]
    pub(super) fn show_navigation(&self) {
        let ui = self.ui();
        ui.set_time_follow(self.time_follow);
        let fractions = self
            .day_data
            .as_ref()
            .and_then(|day| day.bounds())
            .zip(self.span)
            .map_or((0.0, 1.0), |(bounds, span)| {
                let width = (i128::from(bounds.1) - i128::from(bounds.0)) as f64;
                if width <= 0.0 {
                    return (0.0, 1.0);
                }
                let at = |v| (((i128::from(v) - i128::from(bounds.0)) as f64 / width).clamp(0.0, 1.0)) as f32;
                (at(span.0), at(span.1))
            });
        ui.set_time_scroll_start(fractions.0);
        ui.set_time_scroll_end(fractions.1);
    }

    pub(super) fn anchor_following_window(&mut self) {
        if !self.time_follow {
            return;
        }
        let Some(bounds) = self.day_data.as_ref().and_then(|day| day.bounds()) else {
            return;
        };
        let Some(span) = self.span else {
            return;
        };
        self.time_follow = true;
        self.span = Some(timeline::follow(span, bounds));
        if self.ranges.contains_key(&self.range_key(true)) {
            self.remember_time_span();
        }
    }

    pub(super) fn docking_range(&self, range: Range) -> bool {
        self.day_data
            .as_ref()
            .and_then(|d| d.bounds())
            .zip(range.milliseconds())
            .is_some_and(|(bounds, span)| bounds.1 == span.1)
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "milliseconds of one recorded day converted to UI minutes"
    )]
    pub(super) fn zoom_time_range(&self, range: Range, factor: f64) -> Option<Range> {
        let bounds = self.day_data.as_ref()?.bounds()?;
        let span = timeline::zoom_end(range.milliseconds()?, bounds, factor);
        Range::new(span.0 as f64 / 60_000.0, span.1 as f64 / 60_000.0)
    }

    pub fn time_scroll(&mut self, start: f32) {
        let Some(bounds) = self.day_data.as_ref().and_then(|day| day.bounds()) else {
            return;
        };
        let Some(span) = self.span else {
            return;
        };
        self.span = Some(timeline::scroll(span, bounds, f64::from(start)));
        self.time_follow = self.span.is_some_and(|s| s.1 == bounds.1);
        self.remember_time_span();
        self.refresh_time();
        self.remember();
    }

    pub fn time_end(&mut self) {
        let Some(bounds) = self.day_data.as_ref().and_then(|day| day.bounds()) else {
            return;
        };
        let Some(span) = self.span else {
            return;
        };
        self.time_follow = true;
        self.span = Some(timeline::follow(span, bounds));
        if self.ranges.contains_key(&self.range_key(true)) {
            self.remember_time_span();
        }
        self.refresh_time();
        self.show_navigation();
        self.remember();
    }
}
