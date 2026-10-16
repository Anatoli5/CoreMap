// Тесты модуля samples по спецификации; написаны без чтения реализации.
// Входные строки подобраны так, что ответ известен заранее. Температуры и обороты целые,
// поэтому суммы точны при любом порядке сложения и итоги можно сравнивать на равенство.
// 15.10 спецификация изменилась (журнал раз в 0,25 с, выбор владельца): вес строки — время, которое она покрывает
// (строки здесь — раз в 2 с, первая — шаг 2 с), «мало для сравнения» — меньше 10 с, а не 5 строк, корзины графика
// берут строку при любой нагрузке. Эти тесты переведены на неё агентом, писавшим реализацию: числа строк стали
// секундами (строка × 2 с), граница «занят» осталась только у сравнения с соседом.

use crate::samples::*;

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;

// Заголовок с двумя блоками, как его пишет программа входа.
const HEAD: &str =
    "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp,2–5.ceiling,2–5.ratio,2–5.busy,2–5.temp";

// Время строки номер i: шаг 2 с, по возрастанию.
fn at(i: u32) -> String {
    format!("2026-10-15 01:{:02}:{:02}", i / 30, (i % 30) * 2)
}

// Строка данных; блок = [множитель, нагрузка, температура]; потолки 48 и 45.
fn row_fans(time: &str, cpu: &str, gpu: &str, a: [&str; 3], b: [&str; 3]) -> String {
    format!(
        "{time},55,{cpu},{gpu},48,{},{},{},45,{},{},{}",
        a[0], a[1], a[2], b[0], b[1], b[2]
    )
}

// То же без оборотов видеокарты.
fn row(time: &str, fan: &str, a: [&str; 3], b: [&str; 3]) -> String {
    row_fans(time, fan, "", a, b)
}

// Тело файла без заголовка: каждая строка завершена \n.
fn body(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

// Считает строки в новый итог.
fn tally(head: &str, lines: &[String]) -> Tally {
    let columns = Columns::parse(head).expect("заголовок разбирается");
    let mut into = Tally::default();
    Scratch::default().count(&columns, &body(lines), &mut None, &mut into);
    into
}

fn block<'a>(t: &'a Tally, name: &str) -> &'a BlockTally {
    t.blocks.get(name).expect("каждый блок заголовка есть в итоге")
}

// Корзина блока; отсутствующая считается пустой.
fn bin(t: &Tally, name: &str, key: i32) -> Bin {
    block(t, name).bins.get(&key).copied().unwrap_or_default()
}

// Сколько строк блока попало во все корзины.
fn counted(t: &Tally, name: &str) -> u32 {
    block(t, name).bins.values().map(|b| b.n).sum()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn near(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| close(a, b))
}

// Сторона сравнения: rows строк по 2 с со средней температурой mean.
fn side(rows: u32, mean: f64) -> Side {
    let secs = f64::from(rows) * 2.0;
    Side { secs, sum: mean * secs }
}

// Итоги совпадают по блокам, корзинам и сравнениям с соседями. Доля работы дробная, и её суммы зависят от порядка
// сложения: они сравниваются с допуском, всё остальное — точно.
fn same_blocks(x: &Tally, y: &Tally) {
    assert_eq!(x.blocks.keys().collect::<Vec<_>>(), y.blocks.keys().collect::<Vec<_>>());
    let exact = |bins: &BTreeMap<i32, Bin>| -> Vec<(i32, Bin)> {
        bins.iter().map(|(k, b)| (*k, Bin { busy_sum: 0.0, ..*b })).collect()
    };
    for (name, bx) in &x.blocks {
        let by = block(y, name);
        assert_eq!(exact(&bx.bins), exact(&by.bins), "корзины блока {name}");
        for (k, b) in &bx.bins {
            let other = by.bins.get(k).map_or(f64::NAN, |o| o.busy_sum);
            assert!(close(b.busy_sum, other), "доля работы блока {name} на ×{k}");
        }
        assert_eq!(bx.with, by.with, "сравнения блока {name}");
    }
}

#[test]
fn block_counts_every_row_with_known_temp_and_ratio_at_any_load() {
    assert!(close(BUSY, 0.5));
    // у соседа нагрузка ни в одной строке не известна — его строки в корзинах всё равно есть
    let other = ["40.00", "", "50"];
    let lines = [
        row(&at(0), "2400", ["50.00", "0.49", "90"], other), // ниже прежнего порога — в счёт
        row(&at(1), "2400", ["45.00", "0.50", "60"], other), // ровно порог
        row(&at(2), "2400", ["52.00", "", "95"], other),     // нагрузка неизвестна — в счёт, без нагрузки
        row(&at(3), "2400", ["53.00", "0.93", ""], other),   // температура неизвестна (не ноль)
        row(&at(4), "2400", ["", "0.93", "99"], other),      // множитель неизвестен (не ноль)
        row(&at(5), "2400", ["44.80", "1.00", "70"], other),
    ];
    let t = tally(HEAD, &lines);
    assert_eq!(
        counted(&t, "0"),
        4,
        "в счёт идут строки с температурой и множителем при любой нагрузке"
    );
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 2);
    assert!(near(b45.mean(), 65.0), "среднее {:?}", b45.mean());
    assert!(close(b45.peak.temp, 70.0), "максимум {}", b45.peak.temp);
    assert!(near(b45.load(), 0.75), "ø нагрузка {:?}", b45.load());
    assert!(close(bin(&t, "0", 50).peak.temp, 90.0));
    assert_eq!(bin(&t, "0", 52).load(), None, "нагрузка неизвестна — не ноль");
    assert_eq!(counted(&t, "2–5"), 6);
}

#[test]
fn bin_of_rounds_to_nearest_and_limits_range() {
    assert_eq!(bin_of(44.6), Some(45));
    assert_eq!(bin_of(45.4), Some(45));
    assert_eq!(bin_of(45.6), Some(46));
    assert_eq!(bin_of(44.4), Some(44));
    assert_eq!(bin_of(0.0), Some(0));
    assert_eq!(bin_of(80.0), Some(80));
    assert_eq!(bin_of(-2.0), None);
    assert_eq!(bin_of(95.0), None);
}

#[test]
fn bins_group_rows_by_rounded_actual_multiplier_not_ceiling() {
    let b = ["40.00", "0.10", "50"];
    let lines = [
        row(&at(0), "2400", ["44.60", "0.90", "50"], b),
        row(&at(1), "2400", ["45.40", "0.90", "60"], b),
        row(&at(2), "2400", ["45.60", "0.90", "70"], b),
        row(&at(3), "2400", ["44.40", "0.90", "40"], b),
        row(&at(4), "2400", ["45.00", "0.90", "58"], b),
    ];
    let t = tally(HEAD, &lines);
    assert_eq!(counted(&t, "0"), 5);
    // потолок 48 корзиной не служит
    assert_eq!(bin(&t, "0", 48).n, 0);
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 3);
    assert!(near(b45.mean(), 56.0), "среднее {:?}", b45.mean());
    assert!(close(b45.peak.temp, 60.0), "70 °C при 45.60 принадлежит корзине 46");
    let b46 = bin(&t, "0", 46);
    assert_eq!(b46.n, 1);
    assert!(near(b46.mean(), 70.0));
    let b44 = bin(&t, "0", 44);
    assert_eq!(b44.n, 1);
    assert!(near(b44.mean(), 40.0));
}

#[test]
fn fan_mean_uses_only_rows_with_known_cpu_fan() {
    assert!(Bin::default().mean().is_none());
    assert!(Bin::default().fan().is_none());
    let b = ["40.00", "0.10", "50"];
    // обороты видеокарты всегда 5000 и в счёт не идут
    let lines = [
        row_fans(&at(0), "2000", "5000", ["45.00", "0.90", "50"], b),
        row_fans(&at(1), "", "5000", ["45.00", "0.90", "60"], b),
        row_fans(&at(2), "3000", "5000", ["45.00", "0.90", "70"], b),
        row_fans(&at(3), "", "5000", ["47.00", "0.90", "80"], b),
    ];
    let t = tally(HEAD, &lines);
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 3);
    assert!(
        near(b45.mean(), 60.0),
        "пустые обороты не убирают строку из температуры"
    );
    assert!(
        near(b45.fan(), 2500.0),
        "пустые обороты не считаются нулём: {:?}",
        b45.fan()
    );
    let b47 = bin(&t, "0", 47);
    assert_eq!(b47.n, 1);
    assert!(near(b47.mean(), 80.0));
    assert!(b47.fan().is_none(), "ни в одной строке корзины обороты не известны");
}

#[test]
fn columns_are_found_by_name_not_position() {
    // блоки в обратном порядке, поля блоков переставлены, лишний столбец power, вентиляторы местами
    let moved_head =
        "time,package,power,fan_gpu,fan_cpu,2–5.temp,2–5.busy,2–5.ratio,2–5.ceiling,0.temp,0.ratio,0.busy,0.ceiling";
    // (номер строки, обороты процессора, обороты видеокарты, блок 0, блок 2–5)
    let data = [
        (0u32, "2400", "", ["45.00", "0.90", "60"], ["40.00", "0.80", "55"]),
        (1, "2500", "1800", ["45.20", "0.95", "62"], ["40.10", "0.10", "50"]),
        (2, "", "1800", ["46.00", "0.70", "66"], ["", "0.60", ""]),
        (3, "2600", "", ["44.90", "0.20", "48"], ["41.00", "0.99", "70"]),
        (4, "2700", "1900", ["45.00", "1.00", "64"], ["40.00", "0.55", "58"]),
    ];
    let plain: Vec<String> = data
        .iter()
        .map(|&(i, cpu, gpu, a, b)| row_fans(&at(i), cpu, gpu, a, b))
        .collect();
    let moved: Vec<String> = data
        .iter()
        .map(|&(i, cpu, gpu, a, b)| {
            format!(
                "{},55,130,{gpu},{cpu},{},{},{},45,{},{},{},48",
                at(i),
                b[2],
                b[1],
                b[0],
                a[2],
                a[0],
                a[1]
            )
        })
        .collect();
    let x = tally(HEAD, &plain);
    let y = tally(moved_head, &moved);
    assert_eq!(
        y.blocks.keys().map(String::as_str).collect::<Vec<_>>(),
        ["0", "2–5"],
        "power — не блок"
    );
    assert_eq!(counted(&x, "0"), 5, "строка с нагрузкой 0.20 тоже в счёт");
    assert!(!block(&x, "0").with.is_empty());
    assert_eq!(x.rows, y.rows);
    same_blocks(&x, &y);
}

#[test]
fn header_without_time_first_or_without_blocks_is_rejected() {
    assert!(Columns::parse(HEAD).is_some());
    assert!(Columns::parse("package,time,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp").is_none());
    assert!(Columns::parse("time,package,fan_cpu,fan_gpu").is_none());
    assert!(Columns::parse("time,package,fan_cpu,fan_gpu,power").is_none());
    assert!(Columns::parse("").is_none());
}

#[test]
fn row_shorter_than_header_is_ignored() {
    let b = ["40.00", "0.10", "50"];
    let lines = [
        // обрезанная строка раньше всех: её время не становится первым
        "2026-10-15 00:59:58,55,2400,,48,45.00,0.93,99".to_owned(),
        row(&at(0), "2400", ["45.00", "0.90", "60"], b),
        row(&at(1), "2400", ["45.00", "0.90", "62"], b),
        // обрезанная строка позже всех: её время не становится последним
        "2026-10-15 01:00:04,55,2400,,48,45.00,0.93,99,45,40.00".to_owned(),
    ];
    // body() завершает текст \n: пустой хвост — тоже не строка
    let t = tally(HEAD, &lines);
    assert_eq!(t.rows, 2);
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 2);
    assert!(close(b45.peak.temp, 62.0), "99 °C из обрезанной строки не в счёт");
    assert_eq!(t.first.as_deref(), Some(at(0).as_str()));
    assert_eq!(t.last.as_deref(), Some(at(1).as_str()));
}

#[test]
fn counting_in_chunks_equals_counting_at_once() {
    let lines: Vec<String> = (0..12u32)
        .map(|i| {
            let ratio = format!("{}.{}0", 44 + i % 3, i % 10);
            let busy = if i % 4 == 0 { "0.30" } else { "0.90" };
            let other = if i % 2 == 0 { "0.80" } else { "0.20" };
            let fan = if i % 5 == 0 {
                String::new()
            } else {
                (2000 + 100 * i).to_string()
            };
            row(
                &at(i),
                &fan,
                [&ratio, busy, &(50 + i).to_string()],
                ["40.00", other, &(45 + i).to_string()],
            )
        })
        .collect();
    let whole = tally(HEAD, &lines);
    assert!(!block(&whole, "0").with.is_empty());
    // тот же Scratch на двух порциях, как при дочитывании файла
    let columns = Columns::parse(HEAD).expect("заголовок разбирается");
    let mut scratch = Scratch::default();
    let mut parts = Tally::default();
    let mut previous = None;
    scratch.count(&columns, &body(&lines[..5]), &mut previous, &mut parts);
    scratch.count(&columns, &body(&lines[5..]), &mut previous, &mut parts);
    assert_eq!(whole.rows, 12);
    assert_eq!(parts.rows, 12);
    assert_eq!(parts.first, whole.first);
    assert_eq!(parts.last, whole.last);
    same_blocks(&whole, &parts);
}

#[test]
fn neighbour_split_uses_other_busy_within_own_multiplier_bin() {
    let mut cases: Vec<([&str; 3], [&str; 3])> = Vec::new();
    // вместе: сосед загружен, в том числе ровно 0.50 и без его множителя и температуры
    for b in [
        ["40.00", "0.90", "55"],
        ["40.00", "0.50", "55"],
        ["40.00", "0.75", "55"],
        ["40.00", "1.00", "55"],
        ["", "0.60", ""],
    ] {
        cases.push((["45.00", "0.90", "70"], b));
    }
    // порознь: нагрузка соседа ниже 0.5, в том числе 0.49
    for busy in ["0.10", "0.49", "0.00", "0.20", "0.30"] {
        cases.push((["45.00", "0.90", "64"], ["40.00", busy, "52"]));
    }
    // нагрузка соседа неизвестна: в сравнение не идёт
    for _ in 0..2 {
        cases.push((["45.00", "0.90", "99"], ["40.00", "", "52"]));
    }
    // блок 0 не нагружен: в его сравнение не идёт, а для соседа это «порознь»
    for _ in 0..2 {
        cases.push((["50.00", "0.20", "30"], ["40.00", "0.90", "55"]));
    }
    let lines: Vec<String> = cases
        .iter()
        .zip(0u32..)
        .map(|(&(a, b), i)| row(&at(i), "2400", a, b))
        .collect();
    let t = tally(HEAD, &lines);

    let mine = block(&t, "0").with.get("2–5").expect("сравнение блока 0 с соседом");
    // ключ — множитель самого блока (45), не соседа (40)
    let s45 = mine.get(&45).copied().unwrap_or_default();
    assert_eq!((s45.together.secs, s45.alone.secs), (10.0, 10.0), "по 5 строк по 2 с");
    assert!(close(s45.together.sum, 700.0));
    assert!(close(s45.alone.sum, 640.0));
    let all: f64 = mine.values().map(|s| s.together.secs + s.alone.secs).sum();
    assert!(
        close(all, 20.0),
        "строки без нагрузки соседа или без нагрузки блока не в сравнении: {all}"
    );
    let e = effect(mine).expect("обе стороны по 10 с");
    assert!(close(e.delta, 6.0), "разница {}", e.delta);
    assert_eq!((e.together, e.alone), (10.0, 10.0));

    // со стороны соседа: 4 строки вместе (у пятой нет его температуры), 2 порознь
    let theirs = block(&t, "2–5").with.get("0").expect("сравнение соседа с блоком 0");
    let s40 = theirs.get(&40).copied().unwrap_or_default();
    assert_eq!((s40.together.secs, s40.alone.secs), (8.0, 4.0));
    assert!(close(s40.together.sum, 440.0));
    assert!(close(s40.alone.sum, 220.0));
    assert!(effect(theirs).is_none(), "меньше 10 с с каждой стороны");
}

#[test]
fn effect_needs_min_pair_seconds_on_both_sides() {
    assert!(close(MIN_PAIR, 10.0));
    assert!(effect(&BTreeMap::new()).is_none());
    let mut splits = BTreeMap::new();
    splits.insert(
        45,
        Split {
            together: side(4, 70.0),
            alone: side(20, 60.0),
        },
    );
    assert!(effect(&splits).is_none());
    splits.insert(
        45,
        Split {
            together: side(20, 70.0),
            alone: side(4, 60.0),
        },
    );
    assert!(effect(&splits).is_none());
    splits.insert(
        45,
        Split {
            together: side(5, 70.0),
            alone: side(5, 60.0),
        },
    );
    let e = effect(&splits).expect("ровно MIN_PAIR (10 с) с каждой стороны");
    assert!(close(e.delta, 10.0), "разница {}", e.delta);
    assert_eq!((e.together, e.alone), (10.0, 10.0));
}

#[test]
fn effect_weights_bins_by_smaller_side_and_skips_thin_bins() {
    let mut splits = BTreeMap::new();
    // разница 2, вес min(10, 5) = 5
    splits.insert(
        40,
        Split {
            together: side(10, 60.0),
            alone: side(5, 58.0),
        },
    );
    // разница 4, вес min(20, 30) = 20
    splits.insert(
        45,
        Split {
            together: side(20, 70.0),
            alone: side(30, 66.0),
        },
    );
    // разница 40, но вместе только 3 строки: корзина не используется
    splits.insert(
        50,
        Split {
            together: side(3, 90.0),
            alone: side(100, 50.0),
        },
    );
    let e = effect(&splits).expect("две корзины годятся");
    // (2·5 + 4·20) / 25; без веса было бы 3.0, с весом n_вместе + n_порознь — 3.54
    assert!(close(e.delta, 3.6), "разница {}", e.delta);
    assert_eq!((e.together, e.alone), (60.0, 70.0));
}

#[test]
fn effect_not_biased_by_multiplier_confounder() {
    // Температура блока 0 зависит от его множителя (40 → 55 °C, 48 → 75 °C) и +3 °C, когда сосед загружен.
    // Сосед загружен в основном тогда, когда блок 0 на горячем множителе.
    let mut cases = Vec::new();
    for (ratio, temp, other, count) in [
        ("48.00", "78", "0.90", 15),
        ("48.00", "75", "0.10", 5),
        ("40.00", "58", "0.90", 5),
        ("40.00", "55", "0.10", 15),
    ] {
        for _ in 0..count {
            cases.push((ratio, temp, other));
        }
    }
    let lines: Vec<String> = cases
        .iter()
        .zip(0u32..)
        .map(|(&(ratio, temp, other), i)| row(&at(i), "2400", [ratio, "0.90", temp], ["45.00", other, "60"]))
        .collect();
    let t = tally(HEAD, &lines);
    let splits = block(&t, "0").with.get("2–5").expect("сравнение блока 0 с соседом");
    // в данных смешение есть: общие средние без корзин дали бы 73 − 60 = 13
    let sum = |pick: fn(&Split) -> Side| {
        splits.values().map(pick).fold(Side::default(), |acc, s| Side {
            secs: acc.secs + s.secs,
            sum: acc.sum + s.sum,
        })
    };
    let together = sum(|s| s.together);
    let alone = sum(|s| s.alone);
    let pooled = together.sum / together.secs - alone.sum / alone.secs;
    assert!(close(pooled, 13.0), "общая разница {pooled}");
    let e = effect(splits).expect("в обеих корзинах по 5+ строк с каждой стороны");
    assert!(close(e.delta, 3.0), "влияние соседа 3 °C, получено {}", e.delta);
    assert_eq!((e.together, e.alone), (40.0, 40.0));
}

#[test]
fn merge_adds_counts_and_keeps_earliest_first_and_latest_last() {
    // X: 09:00–10:00; Y: 08:00–09:30. Итог: первое из Y, последнее из X.
    let x_lines = [
        row(
            "2026-10-14 09:00:00",
            "2000",
            ["45.00", "0.90", "60"],
            ["40.00", "0.90", "50"],
        ),
        row(
            "2026-10-14 10:00:00",
            "2000",
            ["45.10", "0.90", "70"],
            ["40.00", "0.10", "52"],
        ),
    ];
    let y_lines = [
        row(
            "2026-10-14 08:00:00",
            "3000",
            ["44.90", "0.90", "80"],
            ["40.00", "0.90", "51"],
        ),
        row(
            "2026-10-14 08:30:00",
            "",
            ["45.00", "0.90", "50"],
            ["40.00", "0.10", "49"],
        ),
        row(
            "2026-10-14 09:15:00",
            "3000",
            ["46.00", "0.90", "66"],
            ["40.00", "0.90", "53"],
        ),
        row(
            "2026-10-14 09:30:00",
            "3000",
            ["45.00", "0.90", "50"],
            ["41.00", "0.90", "53"],
        ),
    ];
    let x = tally(HEAD, &x_lines);
    let y = tally(HEAD, &y_lines);
    let both: Vec<String> = x_lines.iter().chain(&y_lines).cloned().collect();
    let at_once = tally(HEAD, &both);

    let mut xy = x.clone();
    xy.merge(&y);
    let mut yx = y.clone();
    yx.merge(&x);
    for m in [&xy, &yx] {
        assert_eq!(m.rows, 6);
        assert_eq!(m.first.as_deref(), Some("2026-10-14 08:00:00"));
        assert_eq!(m.last.as_deref(), Some("2026-10-14 10:00:00"));
        same_blocks(m, &at_once);
    }
    let b45 = bin(&xy, "0", 45);
    assert_eq!(b45.n, 5);
    assert!(close(b45.peak.temp, 80.0), "максимум из Y");
    assert!(near(b45.mean(), 62.0));
    assert!(near(b45.fan(), 2500.0));
    assert_eq!(bin(&xy, "0", 46).n, 1, "корзина только из Y");

    // слияние в пустой итог — копия
    let mut empty = Tally::default();
    empty.merge(&x);
    assert_eq!(empty.rows, x.rows);
    assert_eq!(empty.first, x.first);
    assert_eq!(empty.last, x.last);
    same_blocks(&empty, &x);
}

#[test]
fn read_new_returns_each_complete_line_once() {
    // строки файла кроме заголовка (заголовок read_new может вернуть при чтении с начала)
    fn data(text: &str) -> Vec<&str> {
        text.lines().filter(|l| *l != HEAD).collect()
    }
    // новые полные строки или ничего, если полных строк не прибавилось
    fn more(fresh: Fresh) -> Vec<String> {
        match fresh {
            Fresh::Same => Vec::new(),
            Fresh::More(t) => data(&t).into_iter().map(str::to_owned).collect(),
            Fresh::Again(t) => panic!("файл не укорачивался, а прочитан заново: {t:?}"),
        }
    }
    let path = std::env::temp_dir().join(format!("coremap-samples-spec-{}-tail.csv", std::process::id()));
    let b = ["40.00", "0.10", "50"];
    let r: Vec<String> = (0..5)
        .map(|i| row(&at(i), "2400", ["45.00", "0.90", "60"], b))
        .collect();
    let append = |text: &str| {
        let mut f = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("файл открывается для дописывания");
        f.write_all(text.as_bytes()).expect("дописывание");
    };
    // строка r[1] дописана наполовину
    let (head1, rest1) = r[1].split_at(25);
    fs::write(&path, format!("{HEAD}\n{}\n{head1}", r[0])).expect("запись файла");
    let mut tail = Tail::default();

    let first = match read_new(&path, &mut tail).expect("первое чтение") {
        Fresh::More(t) | Fresh::Again(t) => t,
        Fresh::Same => panic!("первое чтение непустого файла не может быть Same"),
    };
    assert_eq!(data(&first), [r[0].as_str()], "неполная строка не возвращается");
    assert!(matches!(read_new(&path, &mut tail).expect("чтение"), Fresh::Same));

    // дописан конец r[1] и целая r[2]: r[1] приходит целиком и один раз
    append(&format!("{rest1}\n{}\n", r[2]));
    assert_eq!(
        more(read_new(&path, &mut tail).expect("чтение")),
        [r[1].as_str(), r[2].as_str()]
    );
    assert!(matches!(read_new(&path, &mut tail).expect("чтение"), Fresh::Same));

    // кусок без \n ничего не даёт; после завершения строка приходит целиком
    let (head3, rest3) = r[3].split_at(30);
    append(head3);
    assert_eq!(more(read_new(&path, &mut tail).expect("чтение")), Vec::<String>::new());
    append(&format!("{rest3}\n"));
    assert_eq!(more(read_new(&path, &mut tail).expect("чтение")), [r[3].as_str()]);

    // файл стал короче: читается заново с начала
    fs::write(&path, format!("{HEAD}\n{}\n", r[4])).expect("перезапись файла");
    match read_new(&path, &mut tail).expect("чтение после укорачивания") {
        Fresh::Again(t) => assert_eq!(data(&t), [r[4].as_str()]),
        other => panic!("ожидалось Again, получено {other:?}"),
    }
    assert!(matches!(read_new(&path, &mut tail).expect("чтение"), Fresh::Same));

    // временный файл теста; ошибка удаления не влияет на результат
    let _ = fs::remove_file(&path);
}

// ─── Спецификация 15.10: строка раз в 0,25 с ─────────────────────────────────────────────────────────────────────
// Тесты ниже написаны по спецификации 15.10 без чтения реализации samples.rs (только открытые сигнатуры): два вида
// времени строки, длительность строки, перенос времени предыдущей строки между порциями, корзины без порога нагрузки,
// пик, обороты только в строках опроса, MIN_PAIR как время. Ответы посчитаны вручную.

// Сутки и полночь 2026-10-15 в мс «как UTC»: 2026-01-01 — день 20454 (1 767 225 600 с), до 15 октября ещё 287 дней.
const DAY: i64 = 86_400_000;
const OCT15: i64 = 1_792_022_400_000;

// Время новой строки (с миллисекундами): мс от полуночи 15.10.
fn new_at(ms: i64) -> String {
    format!(
        "2026-10-15 {:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

// Время старой строки (без дроби): секунды от полуночи 15.10.
fn old_at(s: i64) -> String {
    format!("2026-10-15 {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

// Время строки в мс; строка обязана разбираться.
fn ms_of(text: &str) -> i64 {
    row_time(text).expect("время разбирается").ms
}

// Строки блока 0 (время, множитель, нагрузка, температура); нагрузка соседа неизвестна, сравнений нет.
fn block0(rows: &[(String, &str, &str, &str)]) -> Tally {
    let lines: Vec<String> = rows
        .iter()
        .map(|r| row(&r.0, "", [r.1, r.2, r.3], ["40.00", "", "50"]))
        .collect();
    tally(HEAD, &lines)
}

// Каждая строка в своей корзине: строка номер i — множитель 40 + i, температура 50 + i, блок 0 занят.
// Тогда время корзины ×(40 + i) и есть длительность строки i.
fn own_bins(times: &[String]) -> Tally {
    let lines: Vec<String> = times
        .iter()
        .zip(40u32..)
        .map(|(time, k)| {
            row(
                time,
                "",
                [&format!("{k}.00"), "0.90", &(k + 10).to_string()],
                ["40.00", "", "50"],
            )
        })
        .collect();
    tally(HEAD, &lines)
}

// Длительность каждой строки own_bins (с) и итог файла.
fn spans_are(t: &Tally, expected: &[f64]) {
    for (&want, k) in expected.iter().zip(40i32..) {
        let b = bin(t, "0", k);
        assert_eq!(b.n, 1, "строка в корзине ×{k}");
        assert!(
            close(b.secs, want),
            "строка в корзине ×{k}: {} с, ожидалось {want} с",
            b.secs
        );
        assert!(
            near(b.mean(), f64::from(k + 10)),
            "температура строки ×{k}: {:?}",
            b.mean()
        );
    }
    assert_eq!(t.rows, u64::try_from(expected.len()).expect("число строк"));
    let total: f64 = expected.iter().sum();
    assert!(close(t.secs, total), "итог файла {} с, ожидалось {total} с", t.secs);
}

// Пик: температура, точный множитель строки, её нагрузка (или «неизвестна») и время строки.
fn peak_is(p: &Peak, temp: f64, ratio: f64, busy: Option<f64>, when: i64) {
    let same_busy = match (p.busy, busy) {
        (Some(x), Some(y)) => close(x, y),
        (None, None) => true,
        _ => false,
    };
    assert!(
        close(p.temp, temp) && close(p.ratio, ratio) && same_busy && p.at == when,
        "пик {p:?}, ожидалось {temp} °C, ×{ratio}, нагрузка {busy:?}, время {when}"
    );
}

// Считает порцию строк файла (без заголовка), перенося время предыдущей строки через previous.
fn count_in(scratch: &mut Scratch, lines: &[String], previous: &mut Option<i64>, into: &mut Tally) {
    let columns = Columns::parse(HEAD).expect("заголовок разбирается");
    scratch.count(&columns, &body(lines), previous, into);
}

// Пары блока 0 (занят, ×45) с соседом: первые together строк сосед занят (у блока 70 °C), остальные — свободен (64 °C).
fn pairs(times: &[String], together: usize) -> (Split, Option<Effect>) {
    let lines: Vec<String> = times
        .iter()
        .enumerate()
        .map(|(i, time)| {
            let (other, temp) = if i < together { ("0.90", "70") } else { ("0.10", "64") };
            row(time, "", ["45.00", "0.90", temp], ["40.00", other, "50"])
        })
        .collect();
    let t = tally(HEAD, &lines);
    let splits = block(&t, "0").with.get("2–5").expect("сравнение блока 0 с соседом");
    (splits.get(&45).copied().unwrap_or_default(), effect(splits))
}

#[test]
fn row_time_reads_old_and_new_forms_as_wall_clock_counted_as_utc() {
    assert_eq!((OLD_STEP_MS, STEP_MS), (2000, 250));
    let old = |ms| Some(RowTime { ms, step: OLD_STEP_MS });
    let new = |ms| Some(RowTime { ms, step: STEP_MS });
    assert_eq!(row_time("1970-01-01 00:00:00"), old(0));
    assert_eq!(row_time("2026-01-01 00:00:00"), old(1_767_225_600_000));
    assert_eq!(row_time("2026-10-15 00:00:00"), old(OCT15));
    assert_eq!(row_time("2026-10-15 01:02:03"), old(OCT15 + 3_723_000));
    assert_eq!(row_time("2026-10-15 01:02:03.456"), new(OCT15 + 3_723_456));
    assert_eq!(
        row_time("2026-10-15 01:02:03.000"),
        new(OCT15 + 3_723_000),
        "дробь из нулей — всё равно новая строка"
    );
    assert_eq!(
        row_time("2025-12-31 23:59:59.750"),
        new(1_767_225_600_000 - 250),
        "конец года"
    );
    // от 3 до 9 цифр дроби: миллисекунды — первые три, остальные отбрасываются, а не округляются
    for (fraction, ms) in [
        ("4569", 456),
        ("456999999", 456),
        ("000000001", 0),
        ("999999999", 999),
        ("1234567", 123),
    ] {
        assert_eq!(
            row_time(&format!("2026-10-15 01:02:03.{fraction}")),
            new(OCT15 + 3_723_000 + ms),
            ".{fraction}"
        );
    }
    // високосные годы: 2000 и 2024 — да, 2023 и 2100 — нет
    let feb = |year: &str| ms_of(&format!("{year}-03-01 00:00:00")) - ms_of(&format!("{year}-02-28 00:00:00"));
    assert_eq!(
        [feb("2000"), feb("2023"), feb("2024"), feb("2100")],
        [2 * DAY, DAY, 2 * DAY, DAY]
    );
    assert_eq!(ms_of("2024-02-29 12:00:00"), 19_782 * DAY + 43_200_000);
}

#[test]
fn row_time_short_fraction_is_a_decimal_fraction_of_a_second() {
    // прочтение спецификации: «.4» — 0,4 с (400 мс), а не 4 мс
    let new = |ms| Some(RowTime { ms, step: STEP_MS });
    assert_eq!(row_time("2026-10-15 01:02:03.4"), new(OCT15 + 3_723_400));
    assert_eq!(row_time("2026-10-15 01:02:03.45"), new(OCT15 + 3_723_450));
    assert_eq!(row_time("2026-10-15 01:02:03.05"), new(OCT15 + 3_723_050));
}

#[test]
fn row_time_rejects_other_shapes() {
    for text in [
        "",
        "time",
        "2026-10-15",
        "01:02:03",
        "15.10 01:02:03",
        "2026-10-15T01:02:03",
        "2026-10-15T01:02:03.456",
        "2026-10-15  01:02:03",
        "2026-10-15 1:02:03",
        "2026-10-15 01:02",
        "2026-10-15 01:02:3",
        "26-10-15 01:02:03",
        "2026/10/15 01:02:03",
        "2026-1O-15 01:02:03",
        "2026-10-15 01:02:03.",
        "2026-10-15 01:02:03.1234567890",
        "2026-10-15 01:02:03.12a",
        "2026-10-15 01:02:03,456",
        "2026-10-15 01:02:03x",
    ] {
        assert_eq!(row_time(text), None, "{text:?}");
    }
}

#[test]
fn row_time_rejects_out_of_range_fields_signs_and_spaces() {
    // прочтение «всё остальное — не время»: поля вне календаря и часов, знак перед числом, пробел вокруг;
    // принятые собираются все сразу, чтобы отчёт называл каждую
    let accepted: Vec<(&str, RowTime)> = [
        "2026-13-01 00:00:00",
        "2026-00-10 00:00:00",
        "2026-10-32 00:00:00",
        "2026-10-00 00:00:00",
        "2026-10-15 24:00:00",
        "2026-10-15 01:60:00",
        "2026-10-15 01:02:60",
        "+026-10-15 01:02:03",
        "2026-10-15 01:-2:03",
        "2026-10-15 01:02:+3",
        "2026-10-15 01:02:03.+45",
        " 2026-10-15 01:02:03",
        "2026-10-15 01:02:03 ",
    ]
    .into_iter()
    .filter_map(|text| row_time(text).map(|t| (text, t)))
    .collect();
    assert!(accepted.is_empty(), "приняты как время: {accepted:?}");
}

#[test]
fn first_and_last_are_earliest_and_latest_even_when_the_clock_goes_back() {
    let base = 11 * 3_600_000; // 11:00:00.000
    // вторая строка раньше первой (часы назад), последняя строка — не самая поздняя
    let times: Vec<String> = [1000, 500, 750].iter().map(|ms| new_at(base + ms)).collect();
    let t = own_bins(&times);
    assert_eq!(t.rows, 3);
    assert_eq!(
        (t.first.as_deref(), t.last.as_deref()),
        (Some(new_at(base + 500).as_str()), Some(new_at(base + 1000).as_str())),
        "первое — самое раннее время, последнее — самое позднее"
    );
}

#[test]
fn span_is_the_gap_up_to_one_and_a_half_own_steps_else_one_step() {
    let at_new = RowTime {
        ms: OCT15 + 3_600_000,
        step: STEP_MS,
    };
    let at_old = RowTime {
        ms: OCT15 + 3_600_000,
        step: OLD_STEP_MS,
    };
    // первая строка файла
    assert_eq!(span_ms(None, at_new), 250);
    assert_eq!(span_ms(None, at_old), 2000);
    // (промежуток от предыдущей строки, длительность); отрицательный промежуток — часы пошли назад
    for (gap, span) in [
        (1, 1),
        (100, 100),
        (250, 250),
        (266, 266),
        (375, 375),
        (376, 250),
        (1000, 250),
        (3_600_000, 250),
        (0, 250),
        (-1, 250),
        (-250, 250),
    ] {
        assert_eq!(
            span_ms(Some(at_new.ms - gap), at_new),
            span,
            "новая строка, промежуток {gap} мс"
        );
    }
    // 1000 мс: у новой строки это пауза (шаг 250), у старой — обычная длительность (шаг 2000)
    for (gap, span) in [
        (1, 1),
        (250, 250),
        (1000, 1000),
        (1999, 1999),
        (2000, 2000),
        (2500, 2500),
        (3000, 3000),
        (3001, 2000),
        (4000, 2000),
        (0, 2000),
        (-2000, 2000),
    ] {
        assert_eq!(
            span_ms(Some(at_old.ms - gap), at_old),
            span,
            "старая строка, промежуток {gap} мс"
        );
    }
}

#[test]
fn new_rows_last_their_gap_up_to_375_ms_else_one_step() {
    let base = 3_600_000; // 01:00:00.000
    let times: Vec<String> = [0, 266, 516, 892, 1267, 5267, 5067, 5067, 5250]
        .iter()
        .map(|ms| new_at(base + ms))
        .collect();
    let t = own_bins(&times);
    // первая; дрожь 266; шаг; 376 > 375 — шаг; ровно 375; пауза 4 с; часы назад; то же время;
    // 183 мс от предыдущей строки (5067), а не от самой поздней (5267)
    spans_are(&t, &[0.25, 0.266, 0.25, 0.25, 0.375, 0.25, 0.25, 0.25, 0.183]);
    assert_eq!(t.first.as_deref(), Some(new_at(base).as_str()));
    assert_eq!(
        t.last.as_deref(),
        Some(new_at(base + 5267).as_str()),
        "последнее — самое позднее время, а не последняя строка файла"
    );
}

#[test]
fn old_rows_last_their_gap_up_to_three_seconds_else_two() {
    let base = 2 * 3600; // 02:00:00
    let times: Vec<String> = [0, 3, 7, 9, 10, 8, 8, 11, 311]
        .iter()
        .map(|s| old_at(base + s))
        .collect();
    let t = own_bins(&times);
    // первая; 3 с (ровно 1,5 шага); 4 с — шаг; 2; 1; часы назад; то же время; 3 с от предыдущей (8), а не 1 от самой
    // поздней (10); пауза 5 мин
    spans_are(&t, &[2.0, 3.0, 2.0, 2.0, 1.0, 2.0, 2.0, 3.0, 2.0]);
    assert_eq!(t.first.as_deref(), Some(old_at(base).as_str()));
    assert_eq!(t.last.as_deref(), Some(old_at(base + 311).as_str()));
}

#[test]
fn mixed_file_measures_each_row_by_its_own_step() {
    // день установки 0.5.2: старые строки, затем новые, затем ещё одна старая (откат) и новая
    let times = [
        old_at(10_800),
        old_at(10_802),
        new_at(10_802_300),
        new_at(10_802_550),
        new_at(10_805_000),
        new_at(10_805_250),
        old_at(10_806),
        new_at(10_806_500),
    ];
    let t = own_bins(&times);
    // 2 (первая); 2; 0,3 (новая через 300 мс ≤ 375); 0,25; пауза 2,45 с — шаг 0,25; 0,25;
    // старая через 750 мс ≤ 3 с — 0,75; новая через 500 мс > 375 — 0,25
    spans_are(&t, &[2.0, 2.0, 0.3, 0.25, 0.25, 0.25, 0.75, 0.25]);
    assert_eq!(t.first.as_deref(), Some("2026-10-15 03:00:00"));
    assert_eq!(t.last.as_deref(), Some("2026-10-15 03:00:06.500"));
}

#[test]
fn row_with_unparseable_time_is_not_counted_and_does_not_move_previous() {
    let base = 8 * 3_600_000; // 08:00:00.000
    let b = ["40.00", "0.90", "50"];
    let lines = [
        row("2026-10-15 00:00:00.0000000000", "", ["45.00", "0.90", "99"], b),
        row(&new_at(base), "", ["45.00", "0.90", "60"], b),
        row("2026-10-15T08:00:00.250", "", ["45.00", "0.90", "99"], b),
        row(&new_at(base + 300), "", ["45.00", "0.90", "62"], b),
        row("2026-10-15 23:59:59x", "", ["45.00", "0.90", "99"], b),
    ];
    let t = tally(HEAD, &lines);
    assert_eq!(t.rows, 2);
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 2);
    // вторая принятая строка — через 300 мс после первой: 0,3 с (а не 0,05 от «08:00:00.250»)
    assert!(close(b45.secs, 0.55), "{} с", b45.secs);
    assert!(close(t.secs, 0.55), "{} с", t.secs);
    assert!(close(b45.peak.temp, 62.0), "99 °C из строк без времени не в счёт");
    assert!(near(b45.mean(), (60.0 * 0.25 + 62.0 * 0.3) / 0.55), "{:?}", b45.mean());
    assert_eq!(counted(&t, "2–5"), 2);
    let pair = block(&t, "0")
        .with
        .get("2–5")
        .and_then(|s| s.get(&45))
        .copied()
        .unwrap_or_default();
    assert!(close(pair.together.secs, 0.55), "в сравнение тоже не идут: {pair:?}");
    assert_eq!(t.first.as_deref(), Some(new_at(base).as_str()));
    assert_eq!(t.last.as_deref(), Some(new_at(base + 300).as_str()));
}

#[test]
fn previous_time_carries_across_chunks_and_short_rows_do_not_move_it() {
    let base = 4 * 3_600_000; // 04:00:00.000
    let one = |ms: i64, k: u32| {
        row(
            &new_at(base + ms),
            "",
            [&format!("{k}.00"), "0.90", "60"],
            ["40.00", "", "50"],
        )
    };
    let lines = [
        one(0, 40),
        one(250, 41),
        // обрезанная строка с более поздним временем: не принимается и время предыдущей не сдвигает
        format!("{},55,,", new_at(base + 400)),
        one(516, 42), // 266 мс от строки 250, а не 116 от обрезанной
        one(766, 43),
    ];
    let expected = [0.25, 0.25, 0.266, 0.25];
    let check = |t: &Tally| {
        for (&want, k) in expected.iter().zip(40i32..) {
            let secs = bin(t, "0", k).secs;
            assert!(close(secs, want), "×{k}: {secs} с, ожидалось {want} с");
        }
        assert_eq!(t.rows, 4);
    };
    let whole = tally(HEAD, &lines);
    check(&whole);

    // две порции на одном Scratch; граница сразу после обрезанной строки
    let mut scratch = Scratch::default();
    let mut parts = Tally::default();
    let mut previous = None;
    count_in(&mut scratch, &lines[..3], &mut previous, &mut parts);
    assert_eq!(previous, Some(OCT15 + base + 250), "время последней принятой строки");
    count_in(&mut scratch, &lines[3..], &mut previous, &mut parts);
    check(&parts);
    same_blocks(&whole, &parts);
    assert!(close(parts.secs, whole.secs));
    assert_eq!((&parts.first, &parts.last), (&whole.first, &whole.last));

    // время переносит аргумент previous: новый Scratch на каждой порции даёт то же
    let mut fresh = Tally::default();
    let mut carried = None;
    count_in(&mut Scratch::default(), &lines[..3], &mut carried, &mut fresh);
    count_in(&mut Scratch::default(), &lines[3..], &mut carried, &mut fresh);
    same_blocks(&whole, &fresh);

    // начало другого файла (previous = None) на том же Scratch: первая строка — один шаг
    let mut next_file = Tally::default();
    count_in(&mut scratch, &lines[3..], &mut None, &mut next_file);
    let first = bin(&next_file, "0", 42).secs;
    assert!(close(first, 0.25), "первая строка файла: {first} с");

    // старые строки: 3 с через границу порций
    let old = [
        row(&old_at(5 * 3600), "", ["40.00", "0.90", "60"], ["40.00", "", "50"]),
        row(&old_at(5 * 3600 + 3), "", ["41.00", "0.90", "60"], ["40.00", "", "50"]),
    ];
    let mut old_parts = Tally::default();
    let mut old_previous = None;
    count_in(&mut scratch, &old[..1], &mut old_previous, &mut old_parts);
    count_in(&mut scratch, &old[1..], &mut old_previous, &mut old_parts);
    let second = bin(&old_parts, "0", 41).secs;
    assert!(close(second, 3.0), "вторая старая строка: {second} с");
    assert!(close(old_parts.secs, 5.0), "{} с", old_parts.secs);
}

#[test]
fn bins_take_rows_at_any_load_and_weight_temp_and_load_by_time() {
    let base = 2 * 3600 + 600; // 02:10:00, старые строки
    // длительности 2 (первая), 3, 1, 2, 1; все множители округляются до 45
    let t = block0(&[
        (old_at(base), "45.00", "0.00", "50"),
        (old_at(base + 3), "45.30", "", "90"),
        (old_at(base + 4), "44.60", "1.00", "70"),
        (old_at(base + 6), "45.49", "0.25", "60"),
        (old_at(base + 7), "44.51", "0.50", "40"),
    ]);
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 5, "нагрузка 0, неизвестная и любая другая — в счёт");
    assert!(close(b45.secs, 9.0), "{} с", b45.secs);
    // (50·2 + 90·3 + 70·1 + 60·2 + 40·1) / 9; по строкам было бы 62
    assert!(near(b45.mean(), 600.0 / 9.0), "среднее по времени: {:?}", b45.mean());
    // (0·2 + 1·1 + 0,25·2 + 0,5·1) / 6 — только строки, где нагрузка известна; по строкам было бы 0,4375
    assert!(near(b45.load(), 2.0 / 6.0), "нагрузка по времени: {:?}", b45.load());
    peak_is(&b45.peak, 90.0, 45.3, None, OCT15 + (base + 3) * 1000);
}

#[test]
fn peak_is_the_hottest_row_ties_go_to_the_later_time_in_any_merge_order() {
    let base = 6 * 3_600_000; // 06:00:00.000
    let x = block0(&[
        (new_at(base), "45.20", "0.80", "80"),
        (new_at(base + 250), "44.70", "", "70"),
    ]);
    peak_is(&bin(&x, "0", 45).peak, 80.0, 45.2, Some(0.8), OCT15 + base);
    // другой файл с той же наибольшей температурой позже — пик из него; раньше — пик остаётся из x
    let later = block0(&[
        (new_at(base + 5000), "44.90", "", "80"),
        (new_at(base + 5250), "45.10", "0.30", "75"),
    ]);
    let earlier = block0(&[
        (new_at(base - 5000), "44.90", "", "80"),
        (new_at(base - 4750), "45.10", "0.30", "75"),
    ]);
    for (other, (ratio, busy, when)) in [
        (&later, (44.9, None, OCT15 + base + 5000)),
        (&earlier, (45.2, Some(0.8), OCT15 + base)),
    ] {
        let mut xy = x.clone();
        xy.merge(other);
        let mut yx = other.clone();
        yx.merge(&x);
        for m in [&xy, &yx] {
            let b45 = bin(m, "0", 45);
            peak_is(&b45.peak, 80.0, ratio, busy, when);
            assert_eq!((m.rows, b45.n), (4, 4));
            assert!(close(m.secs, 1.0) && close(b45.secs, 1.0), "{b45:?}");
            assert!(near(b45.mean(), 76.25), "{:?}", b45.mean());
            // нагрузка известна в двух строках по 0,25 с: 0,8 и 0,3
            assert!(near(b45.load(), 0.55), "{:?}", b45.load());
        }
        same_blocks(&xy, &yx);
    }
    // в одном файле часы пошли назад: из двух строк по 85 °C пик — более поздняя по времени, хотя она выше в файле
    let back = block0(&[
        (new_at(base + 10_000), "45.10", "0.90", "85"),
        (new_at(base + 9_000), "44.80", "0.60", "85"),
    ]);
    peak_is(&bin(&back, "0", 45).peak, 85.0, 45.1, Some(0.9), OCT15 + base + 10_000);
}

#[test]
fn fans_are_averaged_over_the_rows_where_they_were_read() {
    let base = 7 * 3_600_000; // 07:00:00.000
    // строки через 0,25 с; обороты прочитаны в строке 0 и в строке 8 (через 266 мс после строки 7), в остальных пусто
    let offsets = [0, 250, 500, 750, 1000, 1250, 1500, 1750, 2016, 2266];
    let lines: Vec<String> = offsets
        .iter()
        .enumerate()
        .map(|(i, &ms)| {
            let (cpu, gpu) = match i {
                0 => ("2400", "1800"),
                8 => ("3200", "1900"),
                _ => ("", ""),
            };
            // строки 3 и 4 — в корзине ×46, где оборотов не читали
            let (ratio, temp) = if i == 3 || i == 4 {
                ("46.00", "66")
            } else {
                ("45.00", "60")
            };
            row_fans(&new_at(base + ms), cpu, gpu, [ratio, "0.90", temp], ["40.00", "", "50"])
        })
        .collect();
    let t = tally(HEAD, &lines);
    assert_eq!(t.rows, 10);
    let b45 = bin(&t, "0", 45);
    assert_eq!((b45.n, b45.fan_n), (8, 2));
    // простое среднее двух опросов; по времени было бы ≈2812, пустые как ноль — 700
    assert!(near(b45.fan(), 2800.0), "обороты {:?}", b45.fan());
    assert!(near(b45.mean(), 60.0), "строки без оборотов остаются в температуре");
    let b46 = bin(&t, "0", 46);
    assert_eq!(b46.n, 2);
    assert!(
        b46.fan().is_none(),
        "в корзине ни одного опроса: обороты неизвестны, не ноль"
    );
    assert!(near(b46.mean(), 66.0));
}

#[test]
fn min_pair_is_ten_seconds_of_rows_in_either_cadence() {
    let new_rows = |n: i64| (0..n).map(|i| new_at(9 * 3_600_000 + 250 * i)).collect::<Vec<_>>();
    let old_rows = |n: i64, step: i64| (0..n).map(|i| old_at(10 * 3600 + step * i)).collect::<Vec<_>>();

    // 40 + 40 новых строк по 0,25 с — по 10 с с каждой стороны: сравнение есть
    let (split, e) = pairs(&new_rows(80), 40);
    assert!(
        close(split.together.secs, 10.0) && close(split.alone.secs, 10.0),
        "{split:?}"
    );
    assert!(
        close(split.together.sum, 700.0) && close(split.alone.sum, 640.0),
        "{split:?}"
    );
    let e = e.expect("по 10 с с каждой стороны");
    assert!(
        close(e.delta, 6.0) && close(e.together, 10.0) && close(e.alone, 10.0),
        "{e:?}"
    );

    // 40 + 39: порознь 9,75 с — мало, хотя строк намного больше пяти
    let (split, e) = pairs(&new_rows(79), 40);
    assert!(close(split.alone.secs, 9.75), "{split:?}");
    assert!(e.is_none(), "{e:?}");
    // 39 + 40: вместе 9,75 с
    let (split, e) = pairs(&new_rows(79), 39);
    assert!(close(split.together.secs, 9.75), "{split:?}");
    assert!(e.is_none(), "{e:?}");

    // старые строки через 3 с: по 4 строки, но вместе 2 + 3 + 3 + 3 = 11 с, порознь 12 с — сравнение есть
    let (split, e) = pairs(&old_rows(8, 3), 4);
    assert!(
        close(split.together.secs, 11.0) && close(split.alone.secs, 12.0),
        "{split:?}"
    );
    assert!(
        close(split.together.sum, 770.0) && close(split.alone.sum, 768.0),
        "{split:?}"
    );
    let e = e.expect("11 и 12 с");
    assert!(
        close(e.delta, 6.0) && close(e.together, 11.0) && close(e.alone, 12.0),
        "{e:?}"
    );
    // те же 4 + 4 строки через 2 с — по 8 с: мало
    let (split, e) = pairs(&old_rows(8, 2), 4);
    assert!(
        close(split.together.secs, 8.0) && close(split.alone.secs, 8.0),
        "{split:?}"
    );
    assert!(e.is_none(), "{e:?}");
}

#[test]
fn civil_dates_round_trip_and_follow_the_gregorian_calendar() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(civil(0), (1970, 1, 1));
    assert_eq!(civil(-1), (1969, 12, 31));
    assert_eq!(days_from_civil(2026, 1, 1), 20_454);
    assert_eq!(civil(20_741), (2026, 10, 15));
    assert_eq!(days_from_civil(2024, 2, 29), 19_782);
    let feb = |year| days_from_civil(year, 3, 1) - days_from_civil(year, 2, 1);
    assert_eq!(
        [feb(1900), feb(2000), feb(2023), feb(2024), feb(2100)],
        [28, 29, 28, 29, 28]
    );
    // 1967–2134: каждый следующий день — следующая дата календаря, обратный перевод даёт тот же день
    let leap = |year: i64| (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let mut date = civil(-1_096);
    assert_eq!(date, (1967, 1, 1));
    for days in -1_095..60_000 {
        let (year, month, day) = date;
        let length = match month {
            2 if leap(year) => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        let expected = if day < length {
            (year, month, day + 1)
        } else if month < 12 {
            (year, month + 1, 1)
        } else {
            (year + 1, 1, 1)
        };
        date = civil(days);
        assert_eq!(date, expected, "день {days}");
        assert_eq!(
            days_from_civil(date.0, i64::from(date.1), i64::from(date.2)),
            days,
            "день {days}"
        );
    }
    assert_eq!(day_time(OCT15 + 3_723_456), "15.10 01:02:03");
    assert_eq!(day_time(19_782 * DAY + 86_399_000), "29.02 23:59:59");
    assert_eq!(day_time(ms_of("2026-01-01 00:00:00.250")), "01.01 00:00:00");
}
