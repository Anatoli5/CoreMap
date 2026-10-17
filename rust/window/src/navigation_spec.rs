// Независимые проверки по запросу владельца: живое продолжение, прокрутка всего журнала и устойчивый правый край.
// Проверка «ручной вид не едет при новых данных» относится к View: чистая функция не знает, когда её вызывают.
use super::{follow, scroll, zoom};

fn width(view: (i64, i64)) -> i128 {
    i128::from(view.1) - i128::from(view.0)
}
fn valid(view: (i64, i64), bounds: (i64, i64), expected_width: i128) {
    assert!(
        bounds.0 <= view.0 && view.0 < view.1 && view.1 <= bounds.1,
        "{view:?} вне {bounds:?}"
    );
    assert_eq!(width(view), expected_width, "прокрутка изменила масштаб");
}

#[test]
fn follow_moves_the_existing_window_to_each_new_right_edge_without_resetting_scale() {
    let initial = (2_000, 12_000);
    assert_eq!(follow(initial, (0, 30_000)), (20_000, 30_000));
    let first = follow(initial, (0, 30_000));
    assert_eq!(follow(first, (0, 33_500)), (23_500, 33_500));
    assert_eq!(
        follow(first, (0, 30_000)),
        first,
        "повторное обновление без роста ничего не двигает"
    );
}

#[test]
fn follow_keeps_a_narrow_zoom_and_limits_an_oversized_window_to_available_data() {
    assert_eq!(follow((20_000, 20_250), (10_000, 80_000)), (79_750, 80_000));
    assert_eq!(follow((-50_000, 150_000), (10_000, 80_000)), (10_000, 80_000));
    assert_eq!(follow((10_000, 80_000), (10_000, 80_000)), (10_000, 80_000));
    assert_eq!(
        follow((100_000, 110_000), (10_000, 80_000)),
        (70_000, 80_000),
        "прежний вид после смены источника за его краем"
    );
}

#[test]
fn scrollbar_fraction_refers_to_the_full_recording_instead_of_the_remaining_travel() {
    let bounds = (10_000, 110_000);
    let view = (40_000, 60_000);
    assert_eq!(scroll(view, bounds, 0.0), (10_000, 30_000));
    assert_eq!(scroll(view, bounds, 0.25), (35_000, 55_000));
    assert_eq!(scroll(view, bounds, 0.5), (60_000, 80_000));
    assert_eq!(scroll(view, bounds, 0.8), (90_000, 110_000));
    assert_eq!(
        scroll(view, bounds, 1.0),
        (90_000, 110_000),
        "правый конец ограничен шириной окна"
    );
}

#[test]
fn scrolling_is_absolute_reversible_and_clamped_without_changing_zoom() {
    let bounds = (-20_000, 80_000);
    let view = (0, 10_000);
    let middle = scroll(view, bounds, 0.45);
    assert_eq!(middle, (25_000, 35_000));
    assert_eq!(
        scroll(middle, bounds, 0.45),
        middle,
        "ползунок не накапливает относительный сдвиг"
    );
    assert_eq!(scroll(middle, bounds, -4.0), (-20_000, -10_000));
    assert_eq!(scroll(middle, bounds, 4.0), (70_000, 80_000));
    assert_eq!(scroll(scroll(view, bounds, 1.0), bounds, 0.0), (-20_000, -10_000));
    for fraction in [0.0, 0.1, 0.33, 0.75, 0.9, 1.0] {
        valid(scroll(view, bounds, fraction), bounds, 10_000);
    }
}

#[test]
fn full_span_has_no_scroll_travel_and_short_recordings_do_not_keep_a_fake_wide_window() {
    let bounds = (5_000, 6_000);
    for fraction in [-2.0, 0.0, 0.5, 1.0, 2.0] {
        assert_eq!(scroll(bounds, bounds, fraction), bounds);
        assert_eq!(scroll((0, 100_000), bounds, fraction), bounds);
    }
}

#[test]
fn navigation_uses_differences_without_signed_timestamp_overflow() {
    let top = i64::MAX - 100;
    let bottom = i64::MIN + 100;
    let bounds = (bottom, top);
    assert_eq!(follow((-100, 100), bounds), (top - 200, top));
    assert_eq!(scroll((-100, 100), bounds, 0.0), (bottom, bottom + 200));
    assert_eq!(scroll((-100, 100), bounds, 1.0), (top - 200, top));
    valid(scroll((-100, 100), bounds, 0.5), bounds, 200);
    assert_eq!(
        follow(bounds, bounds),
        bounds,
        "ширина больше i64::MAX не должна схлопываться"
    );
    assert_eq!(scroll(bounds, bounds, 0.5), bounds);
}

#[test]
fn nonfinite_scroll_input_is_safe_and_keeps_a_valid_existing_scale() {
    let bounds = (0, 100_000);
    let view = (10_000, 30_000);
    for fraction in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let result =
            std::panic::catch_unwind(|| scroll(view, bounds, fraction)).expect("нечисловой ввод не вызывает панику");
        valid(result, bounds, 20_000);
    }
}

#[test]
fn zoom_at_the_latest_edge_keeps_the_latest_timestamp_for_both_directions() {
    let bounds = (0, 100_000);
    let view = (60_000, 100_000);
    for delta in [-240.0, -120.0, 0.0, 120.0, 240.0] {
        let changed = zoom(view, bounds, 1.0, delta);
        assert_eq!(changed.1, bounds.1, "правый якорь уехал при delta={delta}");
        assert!(bounds.0 <= changed.0 && changed.0 < changed.1);
    }
}
