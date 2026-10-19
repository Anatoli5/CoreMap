// Независимые проверки по контракту: намеренный фильтр скрывает только маркер, не разрыв ряда.
use crate::timeline::{Day, Quantity, decimate, read_day};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn read(files: &[(&str, &str)]) -> Day {
    let directory = std::env::temp_dir().join(format!(
        "coremap-filtered-gap-spec-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).expect("каталог независимой проверки");
    let mut names = Vec::new();
    for &(name, contents) in files {
        std::fs::write(directory.join(name), contents).expect("записать входной CSV");
        names.push(name.to_string());
    }
    let day = read_day(&directory, "2026-10-19", &names, &["0".into()], &[Quantity::Temp]);
    for name in names {
        std::fs::remove_file(directory.join(name)).expect("убрать CSV проверки");
    }
    std::fs::remove_dir(directory).expect("убрать каталог проверки");
    assert!(day.trouble.is_none(), "{:?}", day.trouble);
    day
}

#[test]
fn exact_previous_written_anchor_hides_marker_only() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.000,40,\n1970-01-01 12:00:00.250,42,\n1970-01-01 12:10:00.000,71,1970-01-01 12:00:00.250\n1970-01-01 12:10:00.250,72,\n",
    )]);
    assert_eq!(day.gaps(), []);
    assert_eq!(day.t, [43_200_000, 43_200_250, 43_800_000, 43_800_250]);
    assert_eq!(day.x, [0, 250, 250, 500]);
    assert_eq!(day.starts, [true, false, true, false]);
    assert_eq!(day.blocks[0].temp, [40.0, 42.0, 71.0, 72.0]);
    assert_eq!(day.stretches.len(), 2);
    let mut points = Vec::new();
    decimate(
        &day.x,
        &day.starts,
        &day.blocks[0].temp,
        (0, 500),
        100,
        false,
        &mut points,
    );
    assert_eq!(points.len(), 4, "фильтр не добавляет вымышленные отсчёты");
    assert_eq!(
        points.iter().filter(|p| p.pen_up).count(),
        2,
        "линия не соединяет края фильтра"
    );
}

#[test]
fn legacy_absent_cause_keeps_unknown_gap() {
    let day = read(&[(
        "old.csv",
        "time,0.temp\n1970-01-01 12:00:00.000,40\n1970-01-01 12:10:00.000,71\n",
    )]);
    assert_eq!(day.gaps(), [(0, 43_200_000, 43_800_000)]);
    assert_eq!(day.starts, [true, true]);
}

#[test]
fn empty_new_cause_keeps_unknown_gap() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.000,40,\n1970-01-01 12:10:00.000,71,\n",
    )]);
    assert_eq!(day.gaps(), [(0, 43_200_000, 43_800_000)]);
}

#[test]
fn malformed_cause_does_not_discard_sample_or_hide_gap() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.000,40,\n1970-01-01 12:10:00.000,71,no-proof\n",
    )]);
    assert_eq!(day.t.len(), 2);
    assert_eq!(day.gaps(), [(0, 43_200_000, 43_800_000)]);
}

#[test]
fn anchor_must_match_the_immediately_previous_written_row() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.000,40,\n1970-01-01 12:00:00.250,42,\n1970-01-01 12:10:00.000,71,1970-01-01 12:00:00.000\n",
    )]);
    assert_eq!(day.gaps(), [(250, 43_200_250, 43_800_000)]);
}

#[test]
fn millisecond_mismatch_does_not_hide_gap() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.250,40,\n1970-01-01 12:10:00.000,71,1970-01-01 12:00:00.251\n",
    )]);
    assert_eq!(day.gaps(), [(0, 43_200_250, 43_800_000)]);
}

#[test]
fn mixed_filtered_and_unexplained_intervals_keep_only_unexplained_marker() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.000,40,\n1970-01-01 12:00:00.250,41,\n1970-01-01 12:10:00.000,71,1970-01-01 12:00:00.250\n1970-01-01 12:10:00.250,72,\n1970-01-01 12:20:00.000,50,\n1970-01-01 12:20:00.250,51,\n",
    )]);
    assert_eq!(day.gaps(), [(500, 43_800_250, 44_400_000)]);
    assert_eq!(day.x, [0, 250, 250, 500, 500, 750]);
    assert_eq!(day.starts, [true, false, true, false, true, false]);
}

#[test]
fn filtered_metadata_follows_its_row_through_sorting_and_file_boundaries() {
    let day = read(&[
        (
            "later.csv",
            "time,0.temp,filtered_from\n1970-01-01 12:10:00.250,72,\n1970-01-01 12:10:00.000,71,1970-01-01 12:00:00.250\n",
        ),
        (
            "earlier.csv",
            "time,0.temp,filtered_from\n1970-01-01 12:00:00.250,42,\n1970-01-01 12:00:00.000,40,\n",
        ),
    ]);
    assert_eq!(day.gaps(), []);
    assert_eq!(day.starts, [true, false, true, false]);
    assert_eq!(day.blocks[0].temp, [40.0, 42.0, 71.0, 72.0]);
}

#[test]
fn proof_on_first_row_after_midnight_does_not_fabricate_previous_day_endpoint() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 00:10:00.000,40,1969-12-31 23:59:59.750\n1970-01-01 00:10:00.250,42,\n1970-01-01 00:20:00.000,71,1970-01-01 00:10:00.250\n",
    )]);
    assert_eq!(day.t, [600_000, 600_250, 1_200_000]);
    assert_eq!(day.x, [0, 250, 250]);
    assert_eq!(day.starts, [true, false, true]);
    assert_eq!(day.gaps(), []);
}

#[test]
fn valid_proof_is_scoped_to_its_resume_row_and_does_not_leak_to_next_gap() {
    let day = read(&[(
        "day.csv",
        "time,0.temp,filtered_from\n1970-01-01 12:00:00.000,40,\n1970-01-01 12:10:00.000,71,1970-01-01 12:00:00.000\n1970-01-01 12:20:00.000,50,\n",
    )]);
    assert_eq!(day.gaps(), [(0, 43_800_000, 44_400_000)]);
    assert_eq!(day.starts, [true, true, true]);
}
