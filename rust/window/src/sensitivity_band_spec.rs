use crate::plot::{Range, Viewport};

type Point = (f64, f64);

fn viewport(swap: bool) -> Viewport {
    Viewport {
        argument: Range::new(0.0, 1.0).expect("диапазон аргумента"),
        value: Range::new(0.0, 1.0).expect("диапазон значения"),
        swap,
    }
}

fn near(actual: f64, expected: f64) {
    assert!(actual.is_finite(), "неконечное число {actual}");
    assert!((actual - expected).abs() <= 1.0e-10, "{actual} != {expected}");
}

fn same_point(a: Point, b: Point) -> bool {
    (a.0 - b.0).abs() <= 1.0e-10 && (a.1 - b.1).abs() <= 1.0e-10
}

fn area(points: &[Point]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a.0 * b.1 - b.0 * a.1)
        .sum::<f64>()
        .abs()
        / 2.0
}

// Проверяет заданную геометрию независимо от начальной вершины, обхода и
// добавочных коллинеарных вершин; это не повторяет алгоритм обрезки.
fn polygon_matches(actual: &[Point], expected: &[Point], expected_area: f64) {
    assert!(actual.len() >= 3, "площадь потеряна: {actual:?}");
    near(area(actual), expected_area);
    for &(x, y) in actual {
        assert!(x.is_finite() && y.is_finite());
        assert!((-1.0e-10..=1.0 + 1.0e-10).contains(&x));
        assert!((-1.0e-10..=1.0 + 1.0e-10).contains(&y));
        let on_expected_boundary = expected
            .iter()
            .zip(expected.iter().cycle().skip(1))
            .take(expected.len())
            .any(|(&(ax, ay), &(bx, by))| {
                let cross = (x - ax) * (by - ay) - (y - ay) * (bx - ax);
                cross.abs() <= 1.0e-10
                    && x >= ax.min(bx) - 1.0e-10
                    && x <= ax.max(bx) + 1.0e-10
                    && y >= ay.min(by) - 1.0e-10
                    && y <= ay.max(by) + 1.0e-10
            });
        assert!(on_expected_boundary, "лишняя вершина ({x}, {y})");
    }
    for &corner in expected {
        assert!(
            actual.iter().any(|&p| same_point(p, corner)),
            "потерян угол {corner:?}: {actual:?}"
        );
    }
}

#[test]
fn sensitivity_polygon_preserves_an_inside_triangle() {
    let source = [(0.25, 0.2), (0.75, 0.2), (0.5, 0.8)];
    polygon_matches(&viewport(false).polygon(&source), &source, 0.15);
}

#[test]
fn sensitivity_polygon_clips_each_side_instead_of_discarding_a_crossing_field() {
    for (source, expected) in [
        (
            [(-0.5, 0.2), (0.5, 0.2), (0.5, 0.8), (-0.5, 0.8)],
            [(0.0, 0.2), (0.5, 0.2), (0.5, 0.8), (0.0, 0.8)],
        ),
        (
            [(0.5, 0.2), (1.5, 0.2), (1.5, 0.8), (0.5, 0.8)],
            [(0.5, 0.2), (1.0, 0.2), (1.0, 0.8), (0.5, 0.8)],
        ),
        (
            [(0.2, -0.5), (0.8, -0.5), (0.8, 0.5), (0.2, 0.5)],
            [(0.2, 0.0), (0.8, 0.0), (0.8, 0.5), (0.2, 0.5)],
        ),
        (
            [(0.2, 0.5), (0.8, 0.5), (0.8, 1.5), (0.2, 1.5)],
            [(0.2, 0.5), (0.8, 0.5), (0.8, 1.0), (0.2, 1.0)],
        ),
    ] {
        polygon_matches(&viewport(false).polygon(&source), &expected, 0.3);
    }
}

#[test]
fn sensitivity_polygon_intersects_sloped_edges_at_their_true_boundary_points() {
    let source = [(-0.5, 0.25), (0.5, 0.25), (0.5, 1.25)];
    let expected = [(0.0, 0.25), (0.5, 0.25), (0.5, 1.0), (0.25, 1.0), (0.0, 0.75)];
    // Прямоугольник 0.5 × 0.75 за вычетом треугольника 0.25 × 0.25 / 2.
    polygon_matches(&viewport(false).polygon(&source), &expected, 0.34375);
}

#[test]
fn sensitivity_polygon_keeps_a_whole_field_enclosure_with_no_source_vertex_inside() {
    let source = [(-2.0, -3.0), (3.0, -3.0), (3.0, 4.0), (-2.0, 4.0)];
    let expected = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    polygon_matches(&viewport(false).polygon(&source), &expected, 1.0);
}

#[test]
fn sensitivity_polygon_keeps_an_intersecting_strip_with_no_source_vertex_inside() {
    let source = [(-1.0, 0.4), (2.0, 0.4), (2.0, 0.6), (-1.0, 0.6)];
    let expected = [(0.0, 0.4), (1.0, 0.4), (1.0, 0.6), (0.0, 0.6)];
    polygon_matches(&viewport(false).polygon(&source), &expected, 0.2);
}

#[test]
fn sensitivity_polygon_drops_disjoint_fields_on_every_side() {
    for source in [
        [(-2.0, 0.2), (-1.0, 0.2), (-1.0, 0.8), (-2.0, 0.8)],
        [(2.0, 0.2), (3.0, 0.2), (3.0, 0.8), (2.0, 0.8)],
        [(0.2, -2.0), (0.8, -2.0), (0.8, -1.0), (0.2, -1.0)],
        [(0.2, 2.0), (0.8, 2.0), (0.8, 3.0), (0.2, 3.0)],
    ] {
        assert_eq!(viewport(false).polygon(&source), Vec::<(f64, f64)>::new());
    }
}

#[test]
fn sensitivity_polygon_boundary_contact_alone_has_no_fill() {
    for source in [
        [(-1.0, 0.2), (0.0, 0.2), (0.0, 0.8), (-1.0, 0.8)],
        [(-1.0, -1.0), (0.0, -1.0), (0.0, 0.0), (-1.0, 0.0)],
    ] {
        assert_eq!(viewport(false).polygon(&source), Vec::<(f64, f64)>::new());
    }
}

#[test]
fn sensitivity_polygon_rejects_empty_point_line_and_zero_area_inputs() {
    for source in [
        vec![],
        vec![(0.2, 0.2)],
        vec![(0.2, 0.2), (0.8, 0.8)],
        vec![(0.2, 0.2), (0.2, 0.2), (0.2, 0.2)],
        vec![(0.2, 0.2), (0.5, 0.5), (0.8, 0.8)],
    ] {
        assert_eq!(viewport(false).polygon(&source), Vec::<(f64, f64)>::new());
    }
}

#[test]
fn sensitivity_polygon_rejects_any_nonfinite_vertex_in_either_coordinate() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for source in [
            [(bad, 0.2), (0.8, 0.2), (0.5, 0.8)],
            [(0.2, bad), (0.8, 0.2), (0.5, 0.8)],
        ] {
            assert_eq!(viewport(false).polygon(&source), Vec::<(f64, f64)>::new());
        }
    }
}

#[test]
fn sensitivity_polygon_uses_manual_offsets_and_spans_without_zero_origin_assumptions() {
    let manual = Viewport {
        argument: Range::new(10.0, 20.0).expect("диапазон аргумента"),
        value: Range::new(100.0, 200.0).expect("диапазон значения"),
        swap: false,
    };
    let source = [(12.0, 120.0), (18.0, 120.0), (18.0, 160.0), (12.0, 160.0)];
    let expected = [(0.2, 0.2), (0.8, 0.2), (0.8, 0.6), (0.2, 0.6)];
    polygon_matches(&manual.polygon(&source), &expected, 0.24);
}

#[test]
fn sensitivity_polygon_swap_is_a_pure_transpose_even_after_oblique_clipping() {
    let source = [(5.0, 125.0), (15.0, 125.0), (15.0, 225.0)];
    let expected = [(0.0, 0.25), (0.5, 0.25), (0.5, 1.0), (0.25, 1.0), (0.0, 0.75)];
    for swap in [false, true] {
        let manual = Viewport {
            argument: Range::new(10.0, 20.0).expect("диапазон аргумента"),
            value: Range::new(100.0, 200.0).expect("диапазон значения"),
            swap,
        };
        let expected: Vec<_> = expected
            .iter()
            .map(|&(x, y)| if swap { (y, x) } else { (x, y) })
            .collect();
        polygon_matches(&manual.polygon(&source), &expected, 0.34375);
    }
}

#[test]
fn sensitivity_polygon_winding_does_not_change_its_visible_field() {
    let source = [(0.5, 1.25), (0.5, 0.25), (-0.5, 0.25)];
    let expected = [(0.0, 0.25), (0.5, 0.25), (0.5, 1.0), (0.25, 1.0), (0.0, 0.75)];
    polygon_matches(&viewport(false).polygon(&source), &expected, 0.34375);
}

#[test]
fn sensitivity_polygon_rejects_invalid_public_range_bounds() {
    for invalid in [
        Range { min: 1.0, max: 1.0 },
        Range { min: 2.0, max: 1.0 },
        Range {
            min: f64::NAN,
            max: 1.0,
        },
        Range {
            min: 0.0,
            max: f64::INFINITY,
        },
        Range {
            min: -f64::MAX,
            max: f64::MAX,
        },
    ] {
        for bad_view in [
            Viewport {
                argument: invalid,
                ..viewport(false)
            },
            Viewport {
                value: invalid,
                ..viewport(true)
            },
        ] {
            assert_eq!(
                bad_view.polygon(&[(0.2, 0.2), (0.8, 0.2), (0.5, 0.8)]),
                Vec::<(f64, f64)>::new()
            );
        }
    }
}
