// Тесты модуля samples по спецификации; написаны без чтения реализации.
// Входные строки подобраны так, что ответ известен заранее. Температуры и обороты целые,
// поэтому суммы точны при любом порядке сложения и итоги можно сравнивать на равенство.

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
    Scratch::default().count(&columns, &body(lines), &mut into);
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

// Сторона сравнения: n строк со средней температурой mean.
fn side(n: u32, mean: f64) -> Side {
    Side {
        n,
        sum: mean * f64::from(n),
    }
}

// Итоги совпадают по блокам, корзинам и сравнениям с соседями.
fn same_blocks(x: &Tally, y: &Tally) {
    assert_eq!(x.blocks.keys().collect::<Vec<_>>(), y.blocks.keys().collect::<Vec<_>>());
    for (name, bx) in &x.blocks {
        let by = block(y, name);
        assert_eq!(bx.bins, by.bins, "корзины блока {name}");
        assert_eq!(bx.with, by.with, "сравнения блока {name}");
    }
}

#[test]
fn block_counts_only_when_busy_from_half_with_known_temp_and_ratio() {
    assert!(close(BUSY, 0.5));
    // сосед ни в одной строке не нагружен
    let other = ["40.00", "", "50"];
    let lines = [
        row(&at(0), "2400", ["50.00", "0.49", "90"], other), // ниже порога
        row(&at(1), "2400", ["45.00", "0.50", "60"], other), // ровно порог
        row(&at(2), "2400", ["52.00", "", "95"], other),     // нагрузка неизвестна
        row(&at(3), "2400", ["53.00", "0.93", ""], other),   // температура неизвестна (не ноль)
        row(&at(4), "2400", ["", "0.93", "99"], other),      // множитель неизвестен (не ноль)
        row(&at(5), "2400", ["44.80", "1.00", "70"], other),
    ];
    let t = tally(HEAD, &lines);
    assert_eq!(counted(&t, "0"), 2, "в счёт идут только строки 0.50 и 1.00");
    let b45 = bin(&t, "0", 45);
    assert_eq!(b45.n, 2);
    assert!(near(b45.mean(), 65.0), "среднее {:?}", b45.mean());
    assert!(close(b45.temp_max, 70.0), "максимум {}", b45.temp_max);
    // блок без единой засчитанной строки всё равно есть в итоге
    assert_eq!(counted(&t, "2–5"), 0);
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
    assert!(close(b45.temp_max, 60.0), "70 °C при 45.60 принадлежит корзине 46");
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
    assert_eq!(counted(&x, "0"), 4);
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
    assert!(close(b45.temp_max, 62.0), "99 °C из обрезанной строки не в счёт");
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
    scratch.count(&columns, &body(&lines[..5]), &mut parts);
    scratch.count(&columns, &body(&lines[5..]), &mut parts);
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
    assert_eq!((s45.together.n, s45.alone.n), (5, 5));
    assert!(close(s45.together.sum, 350.0));
    assert!(close(s45.alone.sum, 320.0));
    let all: u32 = mine.values().map(|s| s.together.n + s.alone.n).sum();
    assert_eq!(
        all, 10,
        "строки без нагрузки соседа или без нагрузки блока не в сравнении"
    );
    let e = effect(mine).expect("обе стороны по 5 строк");
    assert!(close(e.delta, 6.0), "разница {}", e.delta);
    assert_eq!((e.together, e.alone), (5, 5));

    // со стороны соседа: 4 строки вместе (у пятой нет его температуры), 2 порознь
    let theirs = block(&t, "2–5").with.get("0").expect("сравнение соседа с блоком 0");
    let s40 = theirs.get(&40).copied().unwrap_or_default();
    assert_eq!((s40.together.n, s40.alone.n), (4, 2));
    assert!(close(s40.together.sum, 220.0));
    assert!(close(s40.alone.sum, 110.0));
    assert!(effect(theirs).is_none(), "меньше 5 строк с каждой стороны");
}

#[test]
fn effect_needs_min_pair_rows_on_both_sides() {
    assert_eq!(MIN_PAIR, 5);
    assert!(effect(&BTreeMap::new()).is_none());
    let mut splits = BTreeMap::new();
    splits.insert(
        45,
        Split {
            together: side(MIN_PAIR - 1, 70.0),
            alone: side(20, 60.0),
        },
    );
    assert!(effect(&splits).is_none());
    splits.insert(
        45,
        Split {
            together: side(20, 70.0),
            alone: side(MIN_PAIR - 1, 60.0),
        },
    );
    assert!(effect(&splits).is_none());
    splits.insert(
        45,
        Split {
            together: side(MIN_PAIR, 70.0),
            alone: side(MIN_PAIR, 60.0),
        },
    );
    let e = effect(&splits).expect("ровно MIN_PAIR с каждой стороны");
    assert!(close(e.delta, 10.0), "разница {}", e.delta);
    assert_eq!((e.together, e.alone), (5, 5));
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
    assert_eq!((e.together, e.alone), (30, 35));
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
            n: acc.n + s.n,
            sum: acc.sum + s.sum,
        })
    };
    let together = sum(|s| s.together);
    let alone = sum(|s| s.alone);
    let pooled = together.sum / f64::from(together.n) - alone.sum / f64::from(alone.n);
    assert!(close(pooled, 13.0), "общая разница {pooled}");
    let e = effect(splits).expect("в обеих корзинах по 5+ строк с каждой стороны");
    assert!(close(e.delta, 3.0), "влияние соседа 3 °C, получено {}", e.delta);
    assert_eq!((e.together, e.alone), (20, 20));
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
    assert!(close(b45.temp_max, 80.0), "максимум из Y");
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
