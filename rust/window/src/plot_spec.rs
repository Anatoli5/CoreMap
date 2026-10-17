use crate::plot::{Range, Viewport};

fn range(min: f64, max: f64) -> Range {
    Range::new(min, max).expect("корректный диапазон")
}

fn near(actual: f64, expected: f64) {
    assert!(actual.is_finite(), "неконечная координата: {actual}");
    let tolerance = 1.0e-10 * expected.abs().max(1.0);
    assert!(
        (actual - expected).abs() <= tolerance,
        "получено {actual}, ожидалось {expected}"
    );
}

fn point_near(actual: (f64, f64), expected: (f64, f64)) {
    near(actual.0, expected.0);
    near(actual.1, expected.1);
}

fn viewport(swap: bool) -> Viewport {
    Viewport {
        argument: range(10.0, 20.0),
        value: range(100.0, 200.0),
        swap,
    }
}

#[test]
fn manual_bounds_reject_missing_reversed_and_nonfinite_values() {
    for (min, max) in [
        (2.0, 2.0),
        (3.0, 2.0),
        (f64::NAN, 2.0),
        (2.0, f64::NAN),
        (f64::NEG_INFINITY, 2.0),
        (2.0, f64::INFINITY),
    ] {
        assert!(Range::new(min, max).is_none());
    }
    assert!(Range::fit([]).is_none());
    assert!(Range::fit([f64::NAN, f64::INFINITY, f64::NEG_INFINITY]).is_none());
    let valid = range(-12.0, -3.0);
    near(valid.min, -12.0);
    near(valid.max, -3.0);
}

#[test]
fn auto_limits_follow_finite_visible_data_without_a_zero_baseline() {
    // Видимые выбранные ряды уже отобраны вызывающим кодом.
    let positive = Range::fit([80.0, f64::NAN, 81.0, 82.0, f64::INFINITY]).expect("есть конечные отсчёты");
    assert!(positive.min < 80.0 && positive.max > 82.0);
    assert!(positive.min > 0.0);
    assert!(positive.max - positive.min < 4.0);

    let negative = Range::fit([-82.0, -80.0]).expect("отрицательные отсчёты");
    assert!(negative.min < -82.0 && negative.max > -80.0);
    assert!(negative.max < 0.0);
    assert!(negative.max - negative.min < 4.0);
}

#[test]
fn constant_auto_limits_have_a_usable_span_around_the_reading() {
    for value in [0.0, 55.0, -55.0] {
        let fitted = Range::fit([value, value]).expect("постоянный ряд допустим");
        assert!(fitted.min.is_finite() && fitted.max.is_finite());
        assert!(fitted.min < value && fitted.max > value);
        assert!(fitted.max - fitted.min > 0.0);
        point_near(
            (
                fitted.normal(fitted.min).expect("нижняя граница"),
                fitted.normal(fitted.max).expect("верхняя граница"),
            ),
            (0.0, 1.0),
        );
        if value > 0.0 {
            assert!(fitted.min > 0.0);
        } else if value < 0.0 {
            assert!(fitted.max < 0.0);
        }
    }
}

#[test]
fn normalization_and_inverse_preserve_values_inside_and_outside_the_view() {
    let bounds = range(10.0, 30.0);
    for (value, fraction) in [(0.0, -0.5), (10.0, 0.0), (15.0, 0.25), (30.0, 1.0), (50.0, 2.0)] {
        near(bounds.normal(value).expect("конечное значение"), fraction);
        near(bounds.value(fraction).expect("конечная доля"), value);
        near(
            bounds
                .value(bounds.normal(value).expect("нормализация"))
                .expect("обратное преобразование"),
            value,
        );
    }
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(bounds.normal(invalid).is_none());
        assert!(bounds.value(invalid).is_none());
    }
}

#[test]
fn pan_moves_each_bound_by_the_signed_fraction_of_its_own_span() {
    let original = range(-12.0, 34.0);
    let right = original.pan(0.5).expect("сдвиг вправо");
    near(right.min, 11.0);
    near(right.max, 57.0);
    let left = original.pan(-0.5).expect("сдвиг влево");
    near(left.min, -35.0);
    near(left.max, 11.0);
    let restored = right.pan(-0.5).expect("обратный сдвиг");
    near(restored.min, original.min);
    near(restored.max, original.max);
}

#[test]
fn zoom_keeps_the_physical_value_under_the_pointer_and_is_reversible() {
    let original = range(10.0, 30.0);
    let zoomed = original.zoom(0.25, 0.5).expect("приближение");
    near(zoomed.min, 12.5);
    near(zoomed.max, 22.5);
    near(zoomed.value(0.25).expect("якорь"), 15.0);
    near(zoomed.max - zoomed.min, 10.0);
    let restored = zoomed.zoom(0.25, 2.0).expect("обратное масштабирование");
    near(restored.min, original.min);
    near(restored.max, original.max);
}

#[test]
fn zoom_anchor_beyond_the_plot_is_clamped_to_the_nearest_edge() {
    let original = range(10.0, 30.0);
    let from_left = original.zoom(-4.0, 0.5).expect("якорь слева");
    near(from_left.min, 10.0);
    near(from_left.max, 20.0);
    let from_right = original.zoom(4.0, 0.5).expect("якорь справа");
    near(from_right.min, 20.0);
    near(from_right.max, 30.0);
}

#[test]
fn transforms_reject_bad_inputs_and_arithmetic_overflow() {
    let original = range(10.0, 30.0);
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(original.pan(invalid).is_none());
        assert!(original.zoom(invalid, 0.5).is_none());
        assert!(original.zoom(0.5, invalid).is_none());
    }
    assert!(original.zoom(0.5, 0.0).is_none());
    assert!(original.zoom(0.5, -1.0).is_none());
    assert!(original.pan(f64::MAX).is_none());
    assert!(original.zoom(0.5, f64::MAX).is_none());
    assert!(original.value(f64::MAX).is_none());
    assert!(Range::new(-f64::MAX, f64::MAX).is_none());
    let huge = range(f64::MAX / 2.0, f64::MAX);
    assert!(huge.normal(-f64::MAX).is_none());
    assert!(huge.pan(2.0).is_none());
}

#[test]
fn point_swap_is_exactly_a_transpose_without_changing_domain_or_units() {
    let regular = viewport(false);
    let swapped = viewport(true);
    for (argument, value, expected) in [
        (10.0, 100.0, (0.0, 0.0)),
        (12.0, 170.0, (0.2, 0.7)),
        (20.0, 200.0, (1.0, 1.0)),
        (5.0, 250.0, (-0.5, 1.5)),
    ] {
        point_near(regular.point(argument, value).expect("точка"), expected);
        point_near(
            swapped.point(argument, value).expect("переставленная точка"),
            (expected.1, expected.0),
        );
    }
    assert!(regular.point(f64::NAN, 150.0).is_none());
    assert!(swapped.point(15.0, f64::INFINITY).is_none());
}

#[test]
fn segment_clipping_uses_both_axes_before_transposing() {
    let regular = Viewport {
        argument: range(0.0, 10.0),
        value: range(0.0, 10.0),
        swap: false,
    };
    let swapped = Viewport {
        argument: range(0.0, 10.0),
        value: range(0.0, 10.0),
        swap: true,
    };
    // Вход ограничивает ось аргумента, выход — ось значения.
    let a = (-10.0, -5.0);
    let b = (20.0, 25.0);
    let clipped = regular.segment(a, b).expect("отрезок пересекает окно");
    point_near(clipped.0, (0.0, 0.5));
    point_near(clipped.1, (0.5, 1.0));
    let transposed = swapped.segment(a, b).expect("переставленный отрезок");
    point_near(transposed.0, (0.5, 0.0));
    point_near(transposed.1, (1.0, 0.5));
    let reversed = regular.segment(b, a).expect("обратный порядок концов");
    point_near(reversed.0, clipped.1);
    point_near(reversed.1, clipped.0);
}

#[test]
fn clipping_preserves_horizontal_vertical_and_already_visible_segments() {
    let view = viewport(false);
    for (a, b, expected_a, expected_b) in [
        ((5.0, 150.0), (25.0, 150.0), (0.0, 0.5), (1.0, 0.5)),
        ((15.0, 50.0), (15.0, 250.0), (0.5, 0.0), (0.5, 1.0)),
        ((12.0, 130.0), (18.0, 180.0), (0.2, 0.3), (0.8, 0.8)),
    ] {
        let actual = view.segment(a, b).expect("видимая часть существует");
        point_near(actual.0, expected_a);
        point_near(actual.1, expected_b);
    }
}

#[test]
fn segments_fully_outside_or_with_missing_coordinates_have_no_geometry() {
    for swap in [false, true] {
        let view = viewport(swap);
        for (a, b) in [
            ((0.0, 120.0), (5.0, 180.0)),
            ((12.0, 220.0), (18.0, 250.0)),
            ((0.0, 150.0), (15.0, 300.0)),
            ((f64::NAN, 150.0), (15.0, 170.0)),
            ((15.0, 150.0), (18.0, f64::INFINITY)),
        ] {
            assert!(view.segment(a, b).is_none());
        }
    }
}

#[test]
fn uncertainty_whiskers_rotate_with_the_curve_instead_of_staying_vertical() {
    let regular = viewport(false);
    let swapped = viewport(true);
    let lower = (15.0, 125.0);
    let upper = (15.0, 175.0);
    let whisker = regular.segment(lower, upper).expect("интервал");
    point_near(whisker.0, (0.5, 0.25));
    point_near(whisker.1, (0.5, 0.75));
    let rotated = swapped.segment(lower, upper).expect("интервал после смены осей");
    point_near(rotated.0, (0.25, 0.5));
    point_near(rotated.1, (0.75, 0.5));
    // Разброс остаётся в единицах значения: после перестановки он горизонтален.
    near(rotated.0.1, rotated.1.1);
    assert!(rotated.1.0 > rotated.0.0);
}

#[test]
fn an_uncertainty_envelope_transposes_every_edge_without_losing_clipping() {
    let regular = viewport(false);
    let swapped = viewport(true);
    let envelope = [(8.0, 120.0), (14.0, 135.0), (22.0, 215.0), (14.0, 180.0)];
    for (a, b) in envelope.into_iter().zip(envelope.into_iter().cycle().skip(1)) {
        let original = regular.segment(a, b);
        let transposed = swapped.segment(a, b);
        match (original, transposed) {
            (Some((start, end)), Some((new_start, new_end))) => {
                point_near(new_start, (start.1, start.0));
                point_near(new_end, (end.1, end.0));
                for coordinate in [start.0, start.1, end.0, end.1] {
                    assert!((-1.0e-10..=1.0 + 1.0e-10).contains(&coordinate));
                }
            }
            (None, None) => {}
            _ => panic!("перестановка осей изменила видимость отрезка"),
        }
    }
}
