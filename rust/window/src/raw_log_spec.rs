// Независимые проверки протокола; подключаются дочерним модулем model.rs.
use super::{Log, log_from_csv};

const OLD: &str = include_str!("raw-old-schema.csv");
const CANONICAL: &str = include_str!("raw-canonical.csv");
const SHUFFLED: &str = include_str!("raw-shuffled.csv");

fn block(log: &Log, name: &str) -> usize {
    log.names()
        .iter()
        .position(|value| value == name)
        .expect("fixture block exists")
}

fn assert_preserved_rows(log: &Log) {
    assert_eq!(log.rows(), 5);
    let origin = log.times()[0];
    let expected_offsets = [0, 2_000, 4_000, 4_250, 4_500];
    let expected_ratios = [30.0, 31.0, 32.0, 33.0, 34.0];
    let expected_temperatures = [60.0, 61.0, 62.0, 63.0, 64.0];
    let expected_ceilings = [40, 41, 42, 43, 44];
    let first = block(log, "0");
    for (row, offset) in expected_offsets.into_iter().enumerate() {
        assert_eq!(log.times()[row] - origin, offset);
        assert_eq!(log.get(row, first, 0), Some(expected_ratios[row]));
        assert_eq!(log.get(row, first, 2), Some(expected_temperatures[row]));
        assert_eq!(log.ceiling(row, first), Some(expected_ceilings[row]));
    }
}

#[test]
fn old_columns_and_both_sampling_intervals_remain_available() {
    let log = log_from_csv(OLD);
    assert_preserved_rows(&log);
    for row in 0..log.rows() {
        for quantity in 0..8 {
            assert_eq!(log.environment(row, quantity), None);
        }
    }
}

#[test]
fn header_order_does_not_change_any_block_or_shared_reading() {
    let canonical = log_from_csv(CANONICAL);
    let shuffled = log_from_csv(SHUFFLED);
    assert_preserved_rows(&canonical);
    assert_preserved_rows(&shuffled);
    assert_eq!(canonical.times(), shuffled.times());
    for row in 0..canonical.rows() {
        for name in ["0", "1"] {
            let left = block(&canonical, name);
            let right = block(&shuffled, name);
            assert_eq!(canonical.ceiling(row, left), shuffled.ceiling(row, right));
            for what in 0..3 {
                assert_eq!(canonical.get(row, left, what), shuffled.get(row, right, what));
            }
        }
        for quantity in 0..8 {
            assert_eq!(
                canonical.environment(row, quantity),
                shuffled.environment(row, quantity)
            );
        }
    }
}

#[test]
fn raw_sparse_point_values_are_not_filled_from_previous_rows() {
    let log = log_from_csv(CANONICAL);
    assert_eq!(log.environment(0, 4), Some(2_000.0));
    assert_eq!(log.environment(1, 4), None);
    assert_eq!(log.environment(2, 4), None);
    assert_eq!(log.environment(3, 4), Some(2_300.0));
    assert_eq!(log.environment(4, 4), None);
    assert_eq!(log.environment(0, 6), Some(45.0));
    assert_eq!(log.environment(1, 6), None);
    assert_eq!(log.environment(2, 6), None);
    assert_eq!(log.environment(3, 6), Some(48.0));
    assert_eq!(log.environment(4, 6), None);
    for quantity in 0..8 {
        assert_eq!(log.environment(2, quantity), None);
    }
    for row in 0..log.rows() {
        for quantity in 0..3 {
            assert_eq!(log.power(row, quantity), log.environment(row, quantity));
        }
    }
}

#[test]
fn shared_field_positions_match_the_public_eight_field_contract() {
    let log = log_from_csv(CANONICAL);
    let expected = [20.0, 40.0, 15.0, 65.0, 2_000.0, 2_100.0, 45.0, 0.2];
    for (quantity, value) in expected.into_iter().enumerate() {
        let actual = log.environment(0, quantity).expect("present shared reading");
        assert!((actual - value).abs() < 1.0e-6, "field {quantity}: {actual} vs {value}");
    }
}

#[test]
fn nonconsecutive_compaction_keeps_all_metadata_with_its_source_row() {
    let original = log_from_csv(CANONICAL);
    let mut compacted = log_from_csv(CANONICAL);
    compacted.keep(&[true, false, true, false, true]);
    assert_eq!(compacted.rows(), 3);
    for (row, source) in [0, 2, 4].into_iter().enumerate() {
        assert_eq!(compacted.times()[row], original.times()[source]);
        for name in ["0", "1"] {
            let first = block(&original, name);
            let second = block(&compacted, name);
            assert_eq!(compacted.ceiling(row, second), original.ceiling(source, first));
            for what in 0..3 {
                assert_eq!(compacted.get(row, second, what), original.get(source, first, what));
            }
        }
        for quantity in 0..8 {
            assert_eq!(
                compacted.environment(row, quantity),
                original.environment(source, quantity)
            );
        }
    }
}

#[test]
fn a_missing_optional_cell_preserves_all_other_fields() {
    let damaged = CANONICAL.replacen("65,2000,2100,40,30,0.1,60", "65,not-a-number,2100,40,30,0.1,60", 1);
    let complete = log_from_csv(CANONICAL);
    let damaged = log_from_csv(&damaged);
    assert_preserved_rows(&damaged);
    assert_eq!(damaged.environment(0, 4), None);
    for quantity in [0, 1, 2, 3, 5, 6, 7] {
        assert_eq!(damaged.environment(0, quantity), complete.environment(0, quantity));
    }
}
