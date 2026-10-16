// Тесты графика статистики по спецификации; пишет отдельный агент, не читавший реализацию (rust\AGENTS.md).
// Источник — спецификация 15.10: температура над остальным кристаллом, 95-й процентиль по времени вместо одной строки
// максимума, оси местами, 12 различимых цветов (8 тёплых для P, 4 холодных для E), бледность тонких корзин без отсечки.
// Ожидаемые числа посчитаны вручную или независимо в самом тесте (точная обратная функция распределения, своё OKLab
// и своя симуляция дальтонизма); формулы реализации сюда не переписывались. У каждого теста в комментарии — какую
// неверную реализацию он ловит.

use crate::chart::{Curves, E_COLORS, FAINT, Frame, P_COLORS, PointShown, Scale, SeriesShown, color_of, ink, nearest};
use crate::samples::{ABOVE_CELL, ABS_CELL, Bin, BlockTally, Columns, Hist, P95, Scratch, Tally, above_die, die_level};
use crate::wire::BlockInfo;

use std::collections::BTreeMap;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn near(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| close(a, b))
}

// ─── Файл срезов ─────────────────────────────────────────────────────────────────────────────────────────────────

// Заголовок файла срезов для блоков с этими именами, как его пишет программа входа.
fn head(names: &[&str]) -> String {
    let blocks: Vec<String> = names
        .iter()
        .map(|n| format!("{n}.ceiling,{n}.ratio,{n}.busy,{n}.temp"))
        .collect();
    format!("time,package,fan_cpu,fan_gpu,{}", blocks.join(","))
}

// Строка срезов: по блоку (множитель, температура), пустое поле — значение не известно. Потолок 50, нагрузка 0.90.
// Температура корпуса 99 °C нарочно не похожа ни на один уровень кристалла: блок сравнивается с другими блоками.
fn line(time: &str, blocks: &[(&str, &str)]) -> String {
    let fields: Vec<String> = blocks
        .iter()
        .map(|(ratio, temp)| format!("50,{ratio},0.90,{temp}"))
        .collect();
    format!("{time},99,,,{}", fields.join(","))
}

// Старая строка (шаг 2 с): секунды от полуночи 15.10.
fn old_at(s: u32) -> String {
    format!("2026-10-15 {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

// Новая строка (шаг 0,25 с): миллисекунды от полуночи 15.10.
fn new_at(ms: u32) -> String {
    format!(
        "2026-10-15 {:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

// Считает строки одного файла (с начала файла) в новый итог.
fn tally(names: &[&str], lines: &[String]) -> Tally {
    let columns = Columns::parse(&head(names)).expect("заголовок разбирается");
    let mut text = lines.join("\n");
    text.push('\n');
    let mut into = Tally::default();
    Scratch::default().count(&columns, &text, &mut None, &mut into);
    into
}

fn block<'a>(t: &'a Tally, name: &str) -> &'a BlockTally {
    t.blocks.get(name).expect("каждый блок заголовка есть в итоге")
}

// Корзина вида как (строк, секунд, среднее по времени); корзины нет — None.
fn bin_at(bins: &BTreeMap<i32, Bin>, key: i32) -> Option<(u32, f64, Option<f64>)> {
    bins.get(&key).map(|b| (b.n, b.secs, b.mean()))
}

// Корзина есть, в ней n строк на secs секунд со средним mean.
fn bin_is(bins: &BTreeMap<i32, Bin>, key: i32, n: u32, secs: f64, mean: f64, what: &str) {
    let got = bin_at(bins, key);
    assert!(
        got.is_some_and(|(gn, gs, gm)| gn == n && close(gs, secs) && near(gm, mean)),
        "{what}: корзина ×{key} = {got:?}, ожидалось ({n}, {secs}, {mean})"
    );
}

// 95-й процентиль гистограммы корзины key.
fn p95_at(hists: &BTreeMap<i32, Hist>, key: i32) -> Option<f64> {
    hists.get(&key).and_then(|h| h.quantile(P95))
}

fn total_at(hists: &BTreeMap<i32, Hist>, key: i32) -> Option<u64> {
    hists.get(&key).map(Hist::total)
}

// ─── 1. Температура над кристаллом в одной строке ──────────────────────────────────────────────────────────────

#[test]
fn above_die_is_own_minus_mean_of_known_others() {
    // Ловит: свою температуру в уровне кристалла (70 − 55 = 15), пустую как ноль (70 − 30 = 40), деление суммы
    // известных на число всех других (70 − 90/3 = 40), сдвиг на единицу по номеру блока (последний блок).
    let all = [Some(70.0), Some(60.0), Some(50.0), Some(40.0)];
    assert!(near(above_die(&all, 0), 20.0), "{:?}", above_die(&all, 0));
    assert!(near(above_die(&all, 1), 60.0 - 160.0 / 3.0), "{:?}", above_die(&all, 1));
    assert!(near(above_die(&all, 3), -20.0), "{:?}", above_die(&all, 3));
    let gap = [Some(70.0), None, Some(50.0), Some(40.0)];
    assert!(
        near(above_die(&gap, 0), 25.0),
        "70 − (50 + 40)/2: {:?}",
        above_die(&gap, 0)
    );
    assert!(
        near(above_die(&gap, 2), -5.0),
        "50 − (70 + 40)/2: {:?}",
        above_die(&gap, 2)
    );
    assert!(
        near(above_die(&gap, 3), -20.0),
        "40 − (70 + 50)/2: {:?}",
        above_die(&gap, 3)
    );
    // Когда известны все, отклонения от остальных в сумме дают ноль: Σ(Tа − (S − Tа)/(n − 1)) = S − S.
    // Ловит любое смещение уровня (свой блок в среднем, корпус вместо блоков, деление на n).
    let temps = [Some(81.0), Some(64.0), Some(57.0), Some(73.0), Some(49.0)];
    let sum: f64 = (0..temps.len())
        .map(|a| above_die(&temps, a).expect("все известны"))
        .sum();
    assert!(sum.abs() < 1e-9, "сумма отклонений {sum}");
}

#[test]
fn above_die_needs_half_of_the_others_and_its_own_temperature() {
    // Ловит: порог «больше половины» вместо «не меньше» (ровно половина известна — значение есть), порог от числа
    // известных, а не от всех других (n − 1), значение без своей температуры, значение без других блоков.
    // n = 2: другой один
    assert!(near(above_die(&[Some(70.0), Some(60.0)], 0), 10.0));
    assert_eq!(above_die(&[Some(70.0), None], 0), None, "ни одного другого");
    // n = 3: один из двух — ровно половина
    assert!(near(above_die(&[Some(70.0), None, Some(50.0)], 0), 20.0));
    assert!(near(above_die(&[Some(70.0), Some(64.0), None], 0), 6.0));
    assert_eq!(above_die(&[Some(70.0), None, None], 0), None);
    // n = 4: два из трёх — да, один из трёх — нет
    assert!(near(above_die(&[Some(70.0), Some(60.0), None, Some(40.0)], 0), 20.0));
    assert_eq!(above_die(&[Some(70.0), None, None, Some(40.0)], 0), None, "1 · 2 < 3");
    // n = 5: два из четырёх — ровно половина — да, один — нет
    let two = [None, Some(60.0), Some(70.0), None, Some(50.0)];
    assert!(near(above_die(&two, 2), 15.0), "{:?}", above_die(&two, 2));
    let one = [None, None, Some(70.0), None, Some(50.0)];
    assert_eq!(above_die(&one, 2), None, "1 · 2 < 4");
    // своей нет
    assert_eq!(above_die(&[None, Some(60.0), Some(50.0)], 0), None);
    // других блоков нет вовсе
    assert_eq!(above_die(&[Some(70.0)], 0), None);
}

#[test]
fn die_level_is_the_mean_of_known_others_when_at_least_half_are_known() {
    // Прочтение: die_level(сумма известных других, их число, число всех других) — уровень кристалла для блока.
    // Ловит: деление на всех других вместо известных, неверный порог половины, значение при нуле известных.
    assert!(near(die_level(150.0, 3, 3), 50.0));
    assert!(near(die_level(90.0, 2, 3), 45.0));
    assert!(near(die_level(100.0, 2, 4), 50.0), "ровно половина");
    assert!(near(die_level(50.0, 1, 2), 50.0), "ровно половина");
    assert!(near(die_level(60.0, 1, 1), 60.0));
    assert_eq!(die_level(40.0, 1, 3), None);
    assert_eq!(die_level(40.0, 1, 4), None);
    assert_eq!(die_level(0.0, 0, 1), None);
    assert_eq!(die_level(0.0, 0, 0), None, "других блоков нет");
    // согласие с above_die: своя температура минус уровень
    let temps = [Some(72.0), None, Some(50.0), Some(40.0)];
    let level = die_level(90.0, 2, 3).expect("2 из 3");
    assert!(near(above_die(&temps, 0), 72.0 - level));
}

// ─── 2. Два вида корзин в итоге файла ───────────────────────────────────────────────────────────────────────────

#[test]
fn tally_above_view_skips_rows_with_too_few_known_others_while_absolute_view_keeps_them() {
    // Ловит: пустую температуру как ноль; порог «больше половины»; свою температуру в уровне; корпус вместо блоков;
    // «других» по числу известных в строке (тогда строка 3 дала бы блоку 0 значение 90 − 40 = 50); строку без
    // значения над кристаллом, выпавшую и из абсолютного вида; гистограммы, не совпадающие с корзинами.
    let names = ["0", "1", "2–5", "6–9"];
    let lines = [
        line(
            &old_at(3600),
            &[("45.00", "70"), ("44.00", "60"), ("40.00", "50"), ("40.00", "40")],
        ),
        line(
            &old_at(3602),
            &[("45.00", "72"), ("44.00", ""), ("40.00", "50"), ("40.00", "40")],
        ),
        line(
            &old_at(3604),
            &[("45.00", "90"), ("44.00", ""), ("40.00", ""), ("40.00", "40")],
        ),
        line(
            &old_at(3606),
            &[("45.00", ""), ("44.00", "60"), ("40.00", "50"), ("40.00", "46")],
        ),
    ];
    let t = tally(&names, &lines);
    assert_eq!(t.rows, 4);

    // блок 0: строки 1–3 в абсолютном виде; над кристаллом 20 и 27, строка 3 — один другой из трёх — нет
    let b0 = block(&t, "0");
    bin_is(&b0.bins, 45, 3, 6.0, 232.0 / 3.0, "блок 0, абсолютно");
    bin_is(&b0.above, 45, 2, 4.0, 23.5, "блок 0, над кристаллом");
    assert_eq!(total_at(&b0.hist, 45), Some(6000));
    assert_eq!(total_at(&b0.above_hist, 45), Some(4000));
    assert_eq!(p95_at(&b0.hist, 45), Some(90.0));
    assert_eq!(p95_at(&b0.above_hist, 45), Some(27.0));

    // блок 1: 60 − 160/3 и 60 − (50 + 46)/2 = 12 (в строке 4 нет блока 0, известны два других из трёх)
    let b1 = block(&t, "1");
    bin_is(&b1.bins, 44, 2, 4.0, 60.0, "блок 1, абсолютно");
    bin_is(&b1.above, 44, 2, 4.0, 28.0 / 3.0, "блок 1, над кристаллом");
    assert_eq!(p95_at(&b1.above_hist, 44), Some(12.0));

    // блок 2–5: −20/3, −6, −3
    let b25 = block(&t, "2–5");
    bin_is(&b25.bins, 40, 3, 6.0, 50.0, "блок 2–5, абсолютно");
    bin_is(&b25.above, 40, 3, 6.0, -47.0 / 9.0, "блок 2–5, над кристаллом");
    assert_eq!(p95_at(&b25.above_hist, 40), Some(-3.0));

    // блок 6–9: в строке 3 известен один другой — в абсолютном виде строка есть, над кристаллом нет
    let b69 = block(&t, "6–9");
    bin_is(&b69.bins, 40, 4, 8.0, 41.5, "блок 6–9, абсолютно");
    bin_is(&b69.above, 40, 3, 6.0, -50.0 / 3.0, "блок 6–9, над кристаллом");
    assert_eq!(total_at(&b69.hist, 40), Some(8000));
    assert_eq!(total_at(&b69.above_hist, 40), Some(6000));
    assert_eq!(p95_at(&b69.above_hist, 40), Some(-9.0));

    // в обоих видах корзины — по округлённому множителю самого блока, других ключей нет
    for b in [b0, b1, b25, b69] {
        assert_eq!(b.bins.len(), 1);
        assert_eq!(b.above.len(), 1);
        assert_eq!(b.hist.keys().collect::<Vec<_>>(), b.bins.keys().collect::<Vec<_>>());
        assert_eq!(
            b.above_hist.keys().collect::<Vec<_>>(),
            b.above.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn tally_die_level_counts_another_block_with_a_temperature_even_without_its_multiplier() {
    // Прочтение спецификации: из уровня кристалла выпадают только блоки с пустой температурой. Множитель другого
    // блока для уровня не нужен (ядро могло спать весь срез, а датчик температуры ответил).
    // Ловит реализацию, которая берёт в уровень только блоки, попавшие в свои корзины: 70 − 50 = 20 вместо 15.
    let names = ["0", "1", "2"];
    let lines = [line(&old_at(7200), &[("45.00", "70"), ("", "60"), ("40.00", "50")])];
    let t = tally(&names, &lines);
    bin_is(&block(&t, "0").above, 45, 1, 2.0, 15.0, "блок 0");
    bin_is(&block(&t, "2").above, 40, 1, 2.0, -15.0, "блок 2");
    assert!(
        block(&t, "1").bins.is_empty(),
        "без множителя блок 1 сам ни в одну корзину не идёт"
    );
    assert!(block(&t, "1").above.is_empty());
}

// ─── 3. 95-й процентиль по времени ──────────────────────────────────────────────────────────────────────────────

// Точный взвешенный по времени процентиль: наименьшее значение v, при котором время со значением ≤ v — не меньше
// доли share всего времени (обратная функция распределения). Целые миллисекунды — без округлений.
fn exact(rows: &[(f64, u32)], share: (u64, u64)) -> Option<f64> {
    let mut sorted = rows.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: u64 = sorted.iter().map(|r| u64::from(r.1)).sum();
    let mut cumulative = 0u64;
    for (value, ms) in sorted {
        cumulative += u64::from(ms);
        if cumulative * share.1 >= total * share.0 {
            return Some(value);
        }
    }
    None
}

fn hist(cell: f64, rows: &[(f64, u32)]) -> Hist {
    let mut h = Hist::new(cell);
    for &(value, ms) in rows {
        h.add(value, ms);
    }
    h
}

// Псевдослучайные числа (xorshift64): повторяемые наборы без зависимостей.
#[derive(Debug)]
struct Dice(u64);

impl Dice {
    fn roll(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    // Целое от 0 до n − 1.
    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.roll() % u64::from(n)).expect("остаток меньше n")
    }

    // Длительность строки: шаг нового журнала, шаг старого или любая от 1 мс до 3 с.
    fn ms(&mut self) -> u32 {
        match self.below(3) {
            0 => 250,
            1 => 2000,
            _ => 1 + self.below(3000),
        }
    }
}

const SHARES: [(u64, u64); 5] = [(50, 100), (90, 100), (95, 100), (99, 100), (100, 100)];

#[test]
fn hist_p95_reaches_95_percent_inclusively_on_hand_made_sets() {
    // Ловит: «> 95 %» вместо «≥ 95 %» (первый набор дал бы 80), процентиль по числу строк, а не по времени,
    // зависимость от порядка добавления, неверное число в пустой гистограмме.
    assert_eq!(P95, (95, 100));
    assert!(close(ABS_CELL, 1.0) && close(ABOVE_CELL, 0.25));
    let empty = Hist::new(ABS_CELL);
    assert_eq!((empty.total(), empty.quantile(P95)), (0, None));
    // по строкам 20 из 21 — 95,2 % при 60; по времени при 60 только 5 с из 7
    let mut by_time = vec![(80.0, 2000)];
    by_time.extend(std::iter::repeat_n((60.0, 250), 20));
    let cases: Vec<(Vec<(f64, u32)>, f64)> = vec![
        (vec![(60.0, 9000), (70.0, 500), (80.0, 500)], 70.0),
        (vec![(60.0, 9000), (70.0, 499), (80.0, 501)], 80.0),
        (vec![(80.0, 501), (60.0, 9000), (70.0, 499)], 80.0),
        (vec![(60.0, 9501), (90.0, 499)], 60.0),
        (vec![(55.0, 250)], 55.0),
        (by_time, 80.0),
    ];
    for (rows, want) in &cases {
        let h = hist(ABS_CELL, rows);
        let total: u64 = rows.iter().map(|r| u64::from(r.1)).sum();
        assert_eq!(h.total(), total, "{rows:?}");
        assert_eq!(h.quantile(P95), Some(*want), "{rows:?}");
        assert_eq!(exact(rows, P95), Some(*want), "сам расчёт теста, {rows:?}");
    }
    // клетка 0,25 °C и значения ниже нуля (над кристаллом бывает холоднее)
    let rows = [(-2.5, 1000), (3.75, 19_000)];
    assert_eq!(hist(ABOVE_CELL, &rows).quantile(P95), Some(3.75));
    let rows = [(-2.5, 1001), (-0.75, 18_999)];
    assert_eq!(hist(ABOVE_CELL, &rows).quantile(P95), Some(-0.75));
}

#[test]
fn hist_reports_the_centre_of_the_cell_a_value_rounds_to() {
    // Ловит: клетку по отбрасыванию дроби (floor), а не по округлению; возврат самого значения или края клетки.
    for (cell, value, want) in [
        (ABS_CELL, 70.4, 70.0),
        (ABS_CELL, 70.6, 71.0),
        (ABS_CELL, 69.7, 70.0),
        (ABOVE_CELL, 19.6, 19.5),
        (ABOVE_CELL, 19.7, 19.75),
        (ABOVE_CELL, 19.0 + 2.0 / 3.0, 19.75),
        (ABOVE_CELL, -0.3, -0.25),
        (ABOVE_CELL, -7.1, -7.0),
    ] {
        let h = hist(cell, &[(value, 250)]);
        assert_eq!(h.quantile(P95), Some(want), "клетка {cell}, значение {value}");
    }
}

#[test]
fn hist_quantile_equals_inverted_cdf_on_grid_and_is_within_half_a_cell_off_grid() {
    // Ловит: интерполяцию между клетками, процентиль по числу строк, ошибку на краю 95 %, неверную клетку
    // (на сетке ответ точный), сдвиг больше половины клетки вне сетки, ответ не в центре клетки.
    let mut dice = Dice(0x9e37_79b9_7f4a_7c15);
    for trial in 0..300 {
        let n = 1 + dice.below(60);
        // на сетке: абсолютные 25..100 °C по 1 °C, над кристаллом −30..+40 °C по 0,25 °C
        let abs: Vec<(f64, u32)> = (0..n).map(|_| (f64::from(25 + dice.below(76)), dice.ms())).collect();
        let above: Vec<(f64, u32)> = (0..n)
            .map(|_| {
                let k = i32::try_from(dice.below(281)).expect("меньше 281") - 120;
                (f64::from(k) * ABOVE_CELL, dice.ms())
            })
            .collect();
        for (cell, rows) in [(ABS_CELL, &abs), (ABOVE_CELL, &above)] {
            let h = hist(cell, rows);
            for share in SHARES {
                assert_eq!(
                    h.quantile(share),
                    exact(rows, share),
                    "на сетке, проба {trial}, клетка {cell}, доля {share:?}, строки {rows:?}"
                );
            }
        }
        // вне сетки: три знака после запятой
        let abs_off: Vec<(f64, u32)> = (0..n)
            .map(|_| (25.0 + f64::from(dice.below(75_001)) / 1000.0, dice.ms()))
            .collect();
        let above_off: Vec<(f64, u32)> = (0..n)
            .map(|_| (f64::from(dice.below(70_001)) / 1000.0 - 30.0, dice.ms()))
            .collect();
        for (cell, rows) in [(ABS_CELL, &abs_off), (ABOVE_CELL, &above_off)] {
            let h = hist(cell, rows);
            for share in SHARES {
                let want = exact(rows, share).expect("строки есть");
                let got = h.quantile(share).expect("гистограмма не пуста");
                assert!(
                    (got - want).abs() <= cell / 2.0 + 1e-9,
                    "вне сетки, проба {trial}, клетка {cell}, доля {share:?}: {got}, точно {want}"
                );
                let k = got / cell;
                assert!((k - k.round()).abs() < 1e-9, "ответ {got} — не центр клетки {cell}");
            }
        }
    }
}

#[test]
fn hist_merge_equals_one_hist_of_all_rows() {
    // Ловит: слияние, теряющее клетки только одной стороны, сдвиг клеток при разных диапазонах значений, сложение
    // числа строк вместо времени.
    let mut dice = Dice(0x2545_f491_4f6c_dd1d);
    for trial in 0..100 {
        for cell in [ABS_CELL, ABOVE_CELL] {
            let n = 2 + dice.below(40);
            // у двух половин нарочно разные диапазоны значений
            let rows: Vec<(f64, u32)> = (0..n)
                .map(|i| {
                    let base = if i % 2 == 0 { -20.0 } else { 40.0 };
                    (base + f64::from(dice.below(60_001)) / 1000.0, dice.ms())
                })
                .collect();
            let cut = usize::try_from(1 + dice.below(n - 1)).expect("номер строки");
            let whole = hist(cell, &rows);
            let mut left = hist(cell, &rows[..cut]);
            left.merge(&hist(cell, &rows[cut..]));
            let mut right = hist(cell, &rows[cut..]);
            right.merge(&hist(cell, &rows[..cut]));
            let mut empty = Hist::new(cell);
            empty.merge(&whole);
            let mut plus_empty = hist(cell, &rows);
            plus_empty.merge(&Hist::new(cell));
            for (what, m) in [
                ("левая+правая", &left),
                ("правая+левая", &right),
                ("пустая+все", &empty),
                ("все+пустая", &plus_empty),
            ] {
                assert_eq!(m.total(), whole.total(), "{what}, проба {trial}");
                for percent in 1..=100 {
                    assert_eq!(
                        m.quantile((percent, 100)),
                        whole.quantile((percent, 100)),
                        "{what}, проба {trial}, клетка {cell}, {percent} %"
                    );
                }
            }
        }
    }
}

#[test]
fn hist_one_two_second_row_weighs_as_eight_quarter_second_rows() {
    // Ловит гистограмму по числу строк: по строкам 95 % при 60 (20 из 21), по времени — при 80.
    let mut one = Hist::new(ABS_CELL);
    one.add(80.0, 2000);
    let mut eight = Hist::new(ABS_CELL);
    for _ in 0..8 {
        eight.add(80.0, 250);
    }
    for h in [&mut one, &mut eight] {
        for _ in 0..20 {
            h.add(60.0, 250);
        }
    }
    assert_eq!((one.total(), eight.total()), (7000, 7000));
    assert_eq!(one.quantile(P95), Some(80.0), "5 с из 7 при 60 °C — меньше 95 %");
    for percent in 1..=100 {
        assert_eq!(
            one.quantile((percent, 100)),
            eight.quantile((percent, 100)),
            "{percent} %"
        );
    }
}

// Блок 0 на ×45 и два других блока: старые строки (2 с) 80 °C при других 60 и 62 (над кристаллом 19), новые (0,25 с)
// 60 °C при 55 и 56 (над кристаллом 4,5).
fn hot_and_cool(old_rows: u32, hot_quarters: u32, cool_quarters: u32) -> Tally {
    let names = ["0", "1", "2–5"];
    let hot = [("45.00", "80"), ("40.00", "60"), ("40.00", "62")];
    let cool = [("45.00", "60"), ("40.00", "55"), ("40.00", "56")];
    let mut lines: Vec<String> = (0..old_rows).map(|i| line(&old_at(10_800 + 2 * i), &hot)).collect();
    // новые строки начинаются через 2 с после последней старой (больше 1,5 шага — одна строка = 0,25 с)
    let start = 1000 * (10_800 + 2 * old_rows);
    lines.extend((0..hot_quarters).map(|i| line(&new_at(start + 250 * i), &hot)));
    let after = start + 250 * hot_quarters;
    lines.extend((0..cool_quarters).map(|i| line(&new_at(after + 250 * i), &cool)));
    tally(&names, &lines)
}

#[test]
fn tally_mixed_two_second_and_quarter_second_rows_weigh_by_time_in_both_views() {
    // Ловит процентиль и среднее по числу строк: 2 старые строки и 40 новых — по строкам 95 % при 60 °C (40 из 42)
    // и среднее 60,95; по времени 4 с горячих из 14 — процентиль 80 °C, среднее 65,71.
    let t = hot_and_cool(2, 0, 40);
    let b0 = block(&t, "0");
    bin_is(&b0.bins, 45, 42, 14.0, 920.0 / 14.0, "абсолютно");
    bin_is(&b0.above, 45, 42, 14.0, 121.0 / 14.0, "над кристаллом");
    assert_eq!(total_at(&b0.hist, 45), Some(14_000));
    assert_eq!(total_at(&b0.above_hist, 45), Some(14_000));
    assert_eq!(p95_at(&b0.hist, 45), Some(80.0), "по времени 10 с из 14 при 60 °C");
    assert_eq!(p95_at(&b0.above_hist, 45), Some(19.0), "по времени 10 с из 14 при +4,5");

    // те же 4 с горячих — шестнадцатью строками по 0,25 с: всё то же
    let same = hot_and_cool(0, 16, 40);
    let s0 = block(&same, "0");
    bin_is(&s0.bins, 45, 56, 14.0, 920.0 / 14.0, "абсолютно, только новые строки");
    bin_is(
        &s0.above,
        45,
        56,
        14.0,
        121.0 / 14.0,
        "над кристаллом, только новые строки",
    );
    for (a, b) in [(&b0.hist, &s0.hist), (&b0.above_hist, &s0.above_hist)] {
        for percent in 1..=100 {
            assert_eq!(
                a.get(&45).and_then(|h| h.quantile((percent, 100))),
                b.get(&45).and_then(|h| h.quantile((percent, 100))),
                "{percent} %"
            );
        }
    }
}

#[test]
fn tally_above_hist_uses_quarter_degree_cells() {
    // Ловит гистограмму «над кристаллом» с клеткой 1 °C (дала бы 10 и 20 вместо 9,5 и 19,75) и среднее, взятое
    // из клеток, а не из самих значений.
    let t = tally(
        &["0", "1", "2"],
        &[line(&old_at(600), &[("45.00", "70"), ("40.00", "60"), ("40.00", "61")])],
    );
    assert_eq!(p95_at(&block(&t, "0").above_hist, 45), Some(9.5));
    bin_is(&block(&t, "0").above, 45, 1, 2.0, 9.5, "блок 0");

    // три других: 70 − (60 + 50 + 41)/3 = 59/3 ≈ 19,667 — клетка 19,75; 60 − 161/3 ≈ 6,333 — клетка 6,25
    let names = ["0", "1", "2", "3"];
    let row = [("45.00", "70"), ("44.00", "60"), ("40.00", "50"), ("41.00", "41")];
    let t = tally(&names, &[line(&old_at(600), &row)]);
    bin_is(
        &block(&t, "0").above,
        45,
        1,
        2.0,
        59.0 / 3.0,
        "среднее — само значение, не клетка",
    );
    assert_eq!(p95_at(&block(&t, "0").above_hist, 45), Some(19.75));
    assert_eq!(p95_at(&block(&t, "1").above_hist, 44), Some(6.25));
    assert_eq!(p95_at(&block(&t, "2").above_hist, 40), Some(-7.0));
    assert_eq!(p95_at(&block(&t, "3").above_hist, 41), Some(-19.0));
    assert_eq!(p95_at(&block(&t, "0").hist, 45), Some(70.0));
}

// Оба вида двух итогов совпадают: корзины (строк, секунд, среднее) и гистограммы (время, все проценты).
fn same_views(x: &Tally, y: &Tally) {
    assert_eq!(x.blocks.keys().collect::<Vec<_>>(), y.blocks.keys().collect::<Vec<_>>());
    for (name, bx) in &x.blocks {
        let by = block(y, name);
        for (what, a, b) in [("bins", &bx.bins, &by.bins), ("above", &bx.above, &by.above)] {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{what} блока {name}"
            );
            for key in a.keys() {
                let (p, q) = (bin_at(a, *key), bin_at(b, *key));
                let same = match (p, q) {
                    (Some((pn, ps, pm)), Some((qn, qs, qm))) => {
                        pn == qn && close(ps, qs) && pm.is_some() && qm.is_some_and(|m| near(pm, m))
                    }
                    _ => false,
                };
                assert!(same, "{what} блока {name} ×{key}: {p:?} и {q:?}");
            }
        }
        for (what, a, b) in [
            ("hist", &bx.hist, &by.hist),
            ("above_hist", &bx.above_hist, &by.above_hist),
        ] {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{what} блока {name}"
            );
            for (key, h) in a {
                let other = b.get(key).expect("ключи совпали");
                assert_eq!(h.total(), other.total(), "{what} блока {name} ×{key}");
                for percent in 1..=100 {
                    assert_eq!(
                        h.quantile((percent, 100)),
                        other.quantile((percent, 100)),
                        "{what} блока {name} ×{key}, {percent} %"
                    );
                }
            }
        }
    }
}

#[test]
fn tally_merge_keeps_both_views() {
    // Ловит слияние итогов, которое забывает корзины или гистограммы над кристаллом либо складывает их неверно
    // (у блока 0 корзина ×45 есть в обеих половинах).
    let names = ["0", "1", "2–5", "6–9"];
    let lines = [
        line(
            &old_at(3600),
            &[("45.00", "70"), ("44.00", "60"), ("40.00", "50"), ("40.00", "40")],
        ),
        line(
            &old_at(3602),
            &[("45.00", "72"), ("44.00", ""), ("40.00", "50"), ("41.00", "40")],
        ),
        line(
            &old_at(3604),
            &[("45.00", "90"), ("44.00", ""), ("40.00", ""), ("40.00", "40")],
        ),
        line(
            &old_at(3606),
            &[("45.00", "75"), ("44.00", "60"), ("40.00", "50"), ("40.00", "46")],
        ),
        line(
            &old_at(3608),
            &[("46.00", "77"), ("44.00", "61"), ("40.00", "52"), ("40.00", "47")],
        ),
    ];
    let whole = tally(&names, &lines);
    assert!(!block(&whole, "0").above.is_empty());
    let mut merged = tally(&names, &lines[..2]);
    merged.merge(&tally(&names, &lines[2..]));
    same_views(&merged, &whole);
    let mut back = tally(&names, &lines[2..]);
    back.merge(&tally(&names, &lines[..2]));
    same_views(&back, &whole);
}

// ─── 4. Оси местами ─────────────────────────────────────────────────────────────────────────────────────────────

fn two(a: (f64, f64), b: (f64, f64)) -> bool {
    close(a.0, b.0) && close(a.1, b.1)
}

fn four(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    close(a.0, b.0) && close(a.1, b.1) && close(a.2, b.2) && close(a.3, b.3)
}

#[test]
fn frame_normal_and_swapped_geometry_by_hand() {
    // Ловит: полосу от края вместо её середины, ось множителя без одной полосы (hi − lo вместо hi − lo + 1),
    // температуру от нуля вместо t0, перестановку без обмена x и y, полосу не по той оси.
    let normal = Frame {
        lo: 40,
        hi: 44,
        t0: 40.0,
        t1: 80.0,
        swap: false,
    };
    let swapped = Frame { swap: true, ..normal };
    for f in [normal, swapped] {
        assert_eq!(f.bands(), 5);
        assert!(close(f.along(40), 0.1) && close(f.along(42), 0.5) && close(f.along(44), 0.9));
        assert!(close(f.value(40.0), 0.0) && close(f.value(50.0), 0.25) && close(f.value(80.0), 1.0));
    }
    assert!(two(normal.place(42, 50.0), (0.5, 0.25)), "{:?}", normal.place(42, 50.0));
    assert!(two(normal.place(44, 70.0), (0.9, 0.75)), "{:?}", normal.place(44, 70.0));
    assert!(
        two(swapped.place(42, 50.0), (0.25, 0.5)),
        "{:?}",
        swapped.place(42, 50.0)
    );
    assert!(
        two(swapped.place(44, 70.0), (0.75, 0.9)),
        "{:?}",
        swapped.place(44, 70.0)
    );
    assert!(four(normal.band(40), (0.0, 0.0, 0.2, 1.0)), "{:?}", normal.band(40));
    assert!(four(normal.band(43), (0.6, 0.0, 0.2, 1.0)), "{:?}", normal.band(43));
    assert!(four(swapped.band(40), (0.0, 0.0, 1.0, 0.2)), "{:?}", swapped.band(40));
    assert!(four(swapped.band(43), (0.0, 0.6, 1.0, 0.2)), "{:?}", swapped.band(43));

    // ось 275HX x32..x55 (24 полосы) и шкала над кристаллом −5..+25 °C
    let wide = Frame {
        lo: 32,
        hi: 55,
        t0: -5.0,
        t1: 25.0,
        swap: false,
    };
    assert_eq!(wide.bands(), 24);
    assert!(close(wide.along(32), 0.5 / 24.0) && close(wide.along(55), 23.5 / 24.0));
    assert!(close(wide.value(-5.0), 0.0) && close(wide.value(10.0), 0.5) && close(wide.value(25.0), 1.0));

    // одна полоса: середина и вся площадь
    let single = Frame {
        lo: 45,
        hi: 45,
        t0: 50.0,
        t1: 90.0,
        swap: false,
    };
    assert_eq!(single.bands(), 1);
    assert!(close(single.along(45), 0.5));
    assert!(four(single.band(45), (0.0, 0.0, 1.0, 1.0)));
    assert!(four(Frame { swap: true, ..single }.band(45), (0.0, 0.0, 1.0, 1.0)));
}

#[test]
fn frame_swap_exchanges_axes_and_bands_tile_the_multiplier_axis() {
    // Ловит: несогласие полосы и точки (точка не в середине своей полосы), щели или наложения полос, перестановку,
    // меняющую сами доли, а не только оси.
    let normal = Frame {
        lo: 32,
        hi: 55,
        t0: 35.0,
        t1: 100.0,
        swap: false,
    };
    let swapped = Frame { swap: true, ..normal };
    let mut edge = 0.0;
    for ratio in normal.lo..=normal.hi {
        for temp in [35.0, 47.5, 61.25, 88.0, 100.0] {
            let (x, y) = normal.place(ratio, temp);
            assert!(two(swapped.place(ratio, temp), (y, x)), "×{ratio}, {temp} °C");
        }
        let (x, y, w, h) = normal.band(ratio);
        assert!(
            close(x, edge) && close(y, 0.0) && close(h, 1.0),
            "полоса ×{ratio}: {:?}",
            normal.band(ratio)
        );
        assert!(close(x + w / 2.0, normal.along(ratio)), "середина полосы ×{ratio}");
        assert!(
            four(swapped.band(ratio), (0.0, x, 1.0, w)),
            "×{ratio}: {:?} при {:?}",
            swapped.band(ratio),
            normal.band(ratio)
        );
        edge = x + w;
    }
    assert!(close(edge, 1.0), "полосы покрывают ось целиком: конец {edge}");
}

// ─── 4. Ближайшая линия под курсором ────────────────────────────────────────────────────────────────────────────

// Ряд блока на графике: точки (множитель, средняя, 95 %) расставлены Frame::place, как их ставит окно. Пути линий
// (mean, p95) оставлены пустыми: по спецификации линия проходит через точки соседних множителей.
#[expect(
    clippy::cast_possible_truncation,
    reason = "доли графика от 0 до 1, f32 окна держит их без потерь смысла"
)]
fn series(frame: Frame, block: usize, rows: &[(i32, f64, f64)]) -> SeriesShown {
    let points = rows
        .iter()
        .map(|&(ratio, mean, p95)| {
            let (x, y) = frame.place(ratio, mean);
            let (qx, qy) = frame.place(ratio, p95);
            PointShown {
                ratio,
                x: x as f32,
                y: y as f32,
                qx: qx as f32,
                qy: qy as f32,
                samples: 40,
                load: 0.5,
                ink: 1.0,
                ..PointShown::default()
            }
        })
        .collect();
    SeriesShown {
        block,
        color: 0x00ff_ffff,
        faded: false,
        mean: Vec::new(),
        p95: Vec::new(),
        model: String::new(),
        model_dash: String::new(),
        model_line: Vec::new(),
        points,
        ..SeriesShown::default()
    }
}

// Поле графика 500 × 300 px; множители 40..44, температура 40..80 °C.
const FIELD: (f64, f64) = (500.0, 300.0);

fn frame(swap: bool) -> Frame {
    Frame {
        lo: 40,
        hi: 44,
        t0: 40.0,
        t1: 80.0,
        swap,
    }
}

// Блок 3: все множители, средняя 50 °C, 95 % — 70 °C. Блок 7: без ×42, средняя 60 °C, 95 % — 76 °C.
fn pair_of_series(swap: bool) -> Vec<SeriesShown> {
    let f = frame(swap);
    let a: Vec<(i32, f64, f64)> = (40..=44).map(|r| (r, 50.0, 70.0)).collect();
    let b: Vec<(i32, f64, f64)> = [40, 41, 43, 44].iter().map(|&r| (r, 60.0, 76.0)).collect();
    vec![series(f, 3, &a), series(f, 7, &b)]
}

#[test]
fn nearest_in_normal_orientation_points_segments_gaps_and_curve_choice() {
    // Пиксели: x = доля · 500, y = (1 − доля) · 300. Множители 40..44 — x 50, 150, 250, 350, 450.
    // Блок 3: средняя y 225, 95 % y 75. Блок 7: средняя y 150, 95 % y 30, точки ×42 нет.
    // Ловит: поиск только по точкам (курсор между точками на отрезке), линию через пропуск множителя, порог не 10 px,
    // y вниз не перевёрнутый, выбор кривой без учёта curves.
    let set = pair_of_series(false);
    let at = |cursor: (f64, f64), curves: Curves| nearest(&set, cursor, FIELD, curves);
    assert_eq!(at((150.0, 231.0), Curves::Both), Some(3), "6 px от точки ×41");
    assert_eq!(
        at((200.0, 216.0), Curves::Both),
        Some(3),
        "9 px от отрезка ×41–×42, 50 px от точек"
    );
    assert_eq!(at((200.0, 212.0), Curves::Both), None, "13 px от линии");
    assert_eq!(
        at((250.0, 150.0), Curves::Both),
        None,
        "у блока 7 нет ×42: отрезка ×41–×43 нет"
    );
    assert_eq!(at((300.0, 148.0), Curves::Both), None, "то же между ×42 и ×43");
    assert_eq!(at((400.0, 152.0), Curves::Both), Some(7), "отрезок ×43–×44");
    assert_eq!(at((100.0, 152.0), Curves::Both), Some(7), "отрезок ×40–×41");
    // 95 %: блок 3 при y 75
    assert_eq!(at((200.0, 78.0), Curves::P95), Some(3));
    assert_eq!(at((200.0, 78.0), Curves::Both), Some(3));
    assert_eq!(at((200.0, 78.0), Curves::Mean), None, "средние далеко");
    assert_eq!(at((200.0, 216.0), Curves::P95), None, "средние не показаны");
    assert_eq!(at((200.0, 216.0), Curves::Mean), Some(3));
    assert_eq!(
        at((300.0, 30.0), Curves::P95),
        None,
        "у 95 % блока 7 тот же пропуск ×42"
    );
    assert_eq!(at((400.0, 33.0), Curves::P95), Some(7));
}

#[test]
fn nearest_in_swapped_orientation() {
    // Пиксели: x = доля температуры · 500, y = (1 − доля множителя) · 300. Множители 40..44 — y 270, 210, 150, 90, 30.
    // Блок 3: средняя x 125, 95 % x 375. Блок 7: средняя x 250, 95 % x 450, точки ×42 нет.
    // Ловит реализацию, которая считает отрезки идущими вдоль x (здесь они вертикальные), и ту же линию через пропуск.
    let set = pair_of_series(true);
    let at = |cursor: (f64, f64), curves: Curves| nearest(&set, cursor, FIELD, curves);
    assert_eq!(at((131.0, 210.0), Curves::Both), Some(3), "6 px от точки ×41");
    assert_eq!(
        at((134.0, 180.0), Curves::Both),
        Some(3),
        "9 px от отрезка ×41–×42, 30 px от точек"
    );
    assert_eq!(at((138.0, 180.0), Curves::Both), None, "13 px");
    assert_eq!(at((250.0, 150.0), Curves::Both), None, "у блока 7 нет ×42");
    assert_eq!(at((248.0, 120.0), Curves::Both), None, "отрезка ×41–×43 нет");
    assert_eq!(at((252.0, 60.0), Curves::Both), Some(7), "отрезок ×43–×44");
    assert_eq!(at((372.0, 180.0), Curves::P95), Some(3));
    assert_eq!(at((372.0, 180.0), Curves::Mean), None);
    assert_eq!(at((134.0, 180.0), Curves::P95), None);
    // курсор, близкий к линии в обычной раскладке, здесь ничего не находит
    assert_eq!(at((200.0, 216.0), Curves::Both), None);
}

#[test]
fn nearest_picks_the_closer_of_two_curves_and_sees_a_lone_point() {
    // Прочтение: если в 10 px проходят две линии, выбирается ближайшая (имя функции — nearest).
    // Ловит «первый найденный» или «последний найденный» вместо ближайшего, и блок из одной точки без отрезков.
    let f = frame(false);
    let a: Vec<(i32, f64, f64)> = (40..=44).map(|r| (r, 50.0, 70.0)).collect();
    let c: Vec<(i32, f64, f64)> = (40..=44).map(|r| (r, 51.0, 79.0)).collect();
    // блок 3 — y 225, блок 9 — y 217,5
    for set in [
        [series(f, 3, &a), series(f, 9, &c)],
        [series(f, 9, &c), series(f, 3, &a)],
    ] {
        assert_eq!(
            nearest(&set, (200.0, 219.0), FIELD, Curves::Both),
            Some(9),
            "1,5 px против 6"
        );
        assert_eq!(
            nearest(&set, (200.0, 224.0), FIELD, Curves::Both),
            Some(3),
            "1 px против 6,5"
        );
    }
    // блок 11 из одной точки ×42: средняя 46 °C — (250, 255), 95 % 48 °C — (250, 240)
    let lone = [series(f, 11, &[(42, 46.0, 48.0)])];
    assert_eq!(nearest(&lone, (250.0, 262.0), FIELD, Curves::Both), Some(11), "7 px");
    assert_eq!(
        nearest(&lone, (262.0, 255.0), FIELD, Curves::Both),
        None,
        "12 px и 19 px"
    );
    assert_eq!(nearest(&lone, (250.0, 244.0), FIELD, Curves::P95), Some(11), "4 px");
    assert_eq!(nearest(&lone, (250.0, 244.0), FIELD, Curves::Mean), None, "11 px");
    assert_eq!(nearest(&[], (250.0, 244.0), FIELD, Curves::Both), None);
}

// ─── 5. Цвета ───────────────────────────────────────────────────────────────────────────────────────────────────

// Канал sRGB 0..255 — в линейный 0..1.
fn linear(channel: u32) -> f64 {
    let v = f64::from(channel) / 255.0;
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

// 0x00RRGGBB — в линейный RGB.
fn rgb(color: u32) -> [f64; 3] {
    [
        linear((color >> 16) & 0xff),
        linear((color >> 8) & 0xff),
        linear(color & 0xff),
    ]
}

// Линейный RGB — в OKLab (Björn Ottosson, 2020).
fn oklab(rgb: [f64; 3]) -> [f64; 3] {
    let [red, green, blue] = rgb;
    let long = (0.412_221_470_8 * red + 0.536_332_536_3 * green + 0.051_445_992_9 * blue).cbrt();
    let middle = (0.211_903_498_2 * red + 0.680_699_545_1 * green + 0.107_396_956_6 * blue).cbrt();
    let short = (0.088_302_461_9 * red + 0.281_718_837_6 * green + 0.629_978_700_5 * blue).cbrt();
    [
        0.210_454_255_3 * long + 0.793_617_785 * middle - 0.004_072_046_8 * short,
        1.977_998_495_1 * long - 2.428_592_205 * middle + 0.450_593_709_9 * short,
        0.025_904_037_1 * long + 0.782_771_766_2 * middle - 0.808_675_766 * short,
    ]
}

// Разница цветов: расстояние в OKLab × 100.
fn delta(a: [f64; 3], b: [f64; 3]) -> f64 {
    100.0 * ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

// Machado, Oliveira, Fernandes 2009, тяжесть 1,0; применяются к линейному RGB, после — в пределы 0..1.
const PROTAN: [[f64; 3]; 3] = [
    [0.152_286, 1.052_583, -0.204_868],
    [0.114_503, 0.786_281, 0.099_216],
    [-0.003_882, -0.048_116, 1.051_998],
];
const DEUTAN: [[f64; 3]; 3] = [
    [0.367_322, 0.860_646, -0.227_968],
    [0.280_085, 0.672_501, 0.047_413],
    [-0.011_82, 0.042_94, 0.968_881],
];

fn simulate(matrix: &[[f64; 3]; 3], c: [f64; 3]) -> [f64; 3] {
    matrix.map(|row| (row[0] * c[0] + row[1] * c[1] + row[2] * c[2]).clamp(0.0, 1.0))
}

fn normal_vision(color: u32) -> [f64; 3] {
    oklab(rgb(color))
}

fn protan_vision(color: u32) -> [f64; 3] {
    oklab(simulate(&PROTAN, rgb(color)))
}

fn deutan_vision(color: u32) -> [f64; 3] {
    oklab(simulate(&DEUTAN, rgb(color)))
}

// Относительная яркость WCAG (из линейного RGB).
fn luminance(color: u32) -> f64 {
    let [red, green, blue] = rgb(color);
    0.2126 * red + 0.7152 * green + 0.0722 * blue
}

fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

// Оттенок OKLab в градусах 0..360.
fn hue(color: u32) -> f64 {
    let [_, a, b] = normal_vision(color);
    b.atan2(a).to_degrees().rem_euclid(360.0)
}

const PANEL: u32 = 0x001c_1f25;
const CUTOFF_RED: u32 = 0x00ff_5d5d;

// Все двенадцать цветов с подписями.
fn palette() -> Vec<(String, u32)> {
    let p = P_COLORS.iter().enumerate().map(|(i, &c)| (format!("P{i} #{c:06x}"), c));
    let e = E_COLORS.iter().enumerate().map(|(i, &c)| (format!("E{i} #{c:06x}"), c));
    p.chain(e).collect()
}

// Все пары в этом зрении, от ближайшей.
fn pairs(vision: fn(u32) -> [f64; 3]) -> Vec<(f64, String)> {
    let all = palette();
    let mut out = Vec::new();
    for (i, (name_a, a)) in all.iter().enumerate() {
        for (name_b, b) in &all[i + 1..] {
            out.push((delta(vision(*a), vision(*b)), format!("{name_a} – {name_b}")));
        }
    }
    out.sort_by(|left, right| left.0.total_cmp(&right.0));
    out
}

#[test]
fn colour_helpers_of_the_test_agree_with_published_values() {
    // Проверка самого теста: без неё ошибка в своём OKLab или в матрицах сделала бы проверки цветов пустыми.
    let white = normal_vision(0x00ff_ffff);
    assert!(
        (white[0] - 1.0).abs() < 1e-4 && white[1].abs() < 1e-4 && white[2].abs() < 1e-4,
        "{white:?}"
    );
    // sRGB (1, 0, 0) → OKLab (0.627955, 0.224863, 0.125846) — значения из статьи Оттоссона
    let red = normal_vision(0x00ff_0000);
    assert!(
        (red[0] - 0.627_955).abs() < 1e-4 && (red[1] - 0.224_863).abs() < 1e-4 && (red[2] - 0.125_846).abs() < 1e-4,
        "{red:?}"
    );
    // серый в симуляции остаётся серым (строки матриц в сумме дают 1)
    for matrix in [&PROTAN, &DEUTAN] {
        let grey = simulate(matrix, [0.5, 0.5, 0.5]);
        assert!(grey.iter().all(|v| (v - 0.5).abs() < 1e-5), "{grey:?}");
    }
    assert!((contrast(0x00ff_ffff, 0) - 21.0).abs() < 1e-9);
}

#[test]
fn palette_every_pair_differs_by_15_in_normal_vision() {
    // Ловит палитру, где два цвета (любые: сравнивать можно любые блоки) неразличимы с одного взгляда.
    let all = pairs(normal_vision);
    assert_eq!(all.len(), 66, "12 цветов — 66 пар");
    let worst = all.first().expect("пары есть");
    eprintln!("худшая пара, обычное зрение: ΔE {:.1} ({})", worst.0, worst.1);
    let bad: Vec<&(f64, String)> = all.iter().filter(|p| p.0 < 15.0).collect();
    assert!(bad.is_empty(), "пары ближе ΔE 15: {bad:?}; худшая {worst:?}");
}

#[test]
fn palette_every_pair_differs_by_6_under_protan_and_deutan() {
    // Ловит палитру, которая держится только на красно-зелёном различии.
    for (what, vision) in [
        ("протанопия", protan_vision as fn(u32) -> [f64; 3]),
        ("дейтеранопия", deutan_vision),
    ] {
        let all = pairs(vision);
        let worst = all.first().expect("пары есть");
        eprintln!("худшая пара, {what}: ΔE {:.1} ({})", worst.0, worst.1);
        let bad: Vec<&(f64, String)> = all.iter().filter(|p| p.0 < 6.0).collect();
        assert!(bad.is_empty(), "{what}: пары ближе ΔE 6: {bad:?}; худшая {worst:?}");
    }
}

#[test]
fn palette_contrast_hue_families_and_distance_from_the_cutoff_line() {
    // Ловит: цвет, теряющийся на тёмной панели; P-цвет холодный или E-цвет тёплый (две группы видны сразу);
    // цвет, похожий на красную линию отсечки автомодуля.
    let mut report = Vec::new();
    for (name, c) in palette() {
        let ratio = contrast(c, PANEL);
        let h = hue(c);
        let to_red = delta(normal_vision(c), normal_vision(CUTOFF_RED));
        report.push(format!(
            "{name}: контраст {ratio:.2}, оттенок {h:.0}°, ΔE до красной {to_red:.1}"
        ));
        assert!(ratio >= 3.0, "{name}: контраст с #1c1f25 {ratio:.2} < 3");
        assert!(to_red >= 15.0, "{name}: ΔE до линии отсечки #ff5d5d {to_red:.1} < 15");
    }
    for (i, &c) in P_COLORS.iter().enumerate() {
        let h = hue(c);
        assert!(
            h >= 330.0 || h <= 110.0,
            "P{i} #{c:06x}: оттенок {h:.1}° не в тёплой дуге (≥ 330° или ≤ 110°)"
        );
    }
    for (i, &c) in E_COLORS.iter().enumerate() {
        let h = hue(c);
        assert!(
            (170.0..=275.0).contains(&h),
            "E{i} #{c:06x}: оттенок {h:.1}° не в холодной дуге 170–275°"
        );
    }
    eprintln!("{}", report.join("\n"));
}

fn info(index: usize, kind: &str, name: &str, members: &[usize]) -> BlockInfo {
    let first = *members.first().expect("в блоке есть ядро");
    BlockInfo {
        index,
        members: members.to_vec(),
        first,
        lp: i64::try_from(first).expect("номер ядра"),
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

// Блоки 275HX в порядке номеров: P 0, 1; E 2–5, 6–9; P 10–13; E 14–17, 18–21; P 22, 23.
fn hx275() -> Vec<BlockInfo> {
    vec![
        info(0, "P", "0", &[0]),
        info(1, "P", "1", &[1]),
        info(2, "E", "2–5", &[2, 3, 4, 5]),
        info(3, "E", "6–9", &[6, 7, 8, 9]),
        info(4, "P", "10", &[10]),
        info(5, "P", "11", &[11]),
        info(6, "P", "12", &[12]),
        info(7, "P", "13", &[13]),
        info(8, "E", "14–17", &[14, 15, 16, 17]),
        info(9, "E", "18–21", &[18, 19, 20, 21]),
        info(10, "P", "22", &[22]),
        info(11, "P", "23", &[23]),
    ]
}

fn hex(colors: &[u32]) -> String {
    colors.iter().map(|c| format!("#{c:06x}")).collect::<Vec<_>>().join(" ")
}

#[test]
fn color_of_ranks_blocks_within_their_kind_on_275hx() {
    // Ловит: цвет по номеру блока в общей палитре (блок 2–5 получил бы P-цвет), счёт по всем блокам вместо блоков
    // своего вида, повтор цвета, зависимость от порядка вопросов.
    let blocks = hx275();
    let expected = [
        P_COLORS[0],
        P_COLORS[1],
        E_COLORS[0],
        E_COLORS[1],
        P_COLORS[2],
        P_COLORS[3],
        P_COLORS[4],
        P_COLORS[5],
        E_COLORS[2],
        E_COLORS[3],
        P_COLORS[6],
        P_COLORS[7],
    ];
    let got: Vec<u32> = (0..blocks.len()).map(|b| color_of(&blocks, b)).collect();
    assert_eq!(got, expected, "получено {}, ожидалось {}", hex(&got), hex(&expected));
    let mut distinct = got.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 12, "{}", hex(&got));
    let mut backwards: Vec<u32> = (0..blocks.len()).rev().map(|b| color_of(&blocks, b)).collect();
    backwards.reverse();
    assert_eq!(backwards, got);
}

#[test]
fn color_of_counts_ranks_separately_when_kinds_interleave_differently() {
    // Ловит то же на другом процессоре: E-блок первым, P и E вперемешку.
    let blocks = [
        info(0, "E", "0–3", &[0, 1, 2, 3]),
        info(1, "P", "4", &[4]),
        info(2, "E", "5–8", &[5, 6, 7, 8]),
        info(3, "P", "9", &[9]),
    ];
    let got: Vec<u32> = (0..blocks.len()).map(|b| color_of(&blocks, b)).collect();
    let expected = [E_COLORS[0], P_COLORS[0], E_COLORS[1], P_COLORS[1]];
    assert_eq!(got, expected, "получено {}, ожидалось {}", hex(&got), hex(&expected));
}

// ─── 6. Бледность тонких корзин ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn ink_is_a_log_share_from_one_row_to_the_fullest_bin_and_never_vanishes() {
    // Ловит: линейную долю времени (25 из 250 с дали бы 0,1), начало шкалы от 1 с, а не от строки 0,25 с, отсечку
    // в ноль вместо FAINT, яркость больше 1, деление на ноль при самой полной корзине в одну строку.
    assert!(close(FAINT, 0.2));
    let full = 250.0; // 1000 строк по 0,25 с
    assert!(close(ink(250.0, full), 1.0));
    assert!(close(ink(25.0, full), 2.0 / 3.0), "{}", ink(25.0, full));
    assert!(close(ink(2.5, full), 1.0 / 3.0), "{}", ink(2.5, full));
    assert!(
        close(ink(0.25, full), FAINT),
        "одна строка — бледнее всех, но видна: {}",
        ink(0.25, full)
    );
    assert!(close(ink(0.001, full), FAINT), "{}", ink(0.001, full));
    assert!(close(ink(1000.0, full), 1.0), "{}", ink(1000.0, full));
    // самая полная — 2500 с: каждое удесятерение прибавляет четверть
    for (secs, want) in [(2.5, 0.25), (25.0, 0.5), (250.0, 0.75), (2500.0, 1.0)] {
        assert!(close(ink(secs, 2500.0), want), "{secs} с: {}", ink(secs, 2500.0));
    }
    // час: удесятерение прибавляет одно и то же, пока значение между FAINT и 1
    let hour = 3600.0;
    let steps: Vec<f64> = [3.0, 30.0, 300.0, 3000.0].iter().map(|&s| ink(s, hour)).collect();
    assert!(steps[0] > FAINT && steps[3] < 1.0, "{steps:?}");
    for w in steps.windows(3) {
        assert!(close(w[1] - w[0], w[2] - w[1]), "{steps:?}");
    }
    // самая полная корзина не больше одной строки — всё в полную силу
    assert!(close(ink(0.25, 0.25), 1.0) && close(ink(0.1, 0.25), 1.0) && close(ink(0.2, 0.1), 1.0));
    // не убывает и не выходит за [FAINT, 1]
    let mut secs = 0.01;
    let mut last = 0.0;
    while secs < 10_000.0 {
        let v = ink(secs, hour);
        assert!(
            v >= last - 1e-12 && (FAINT..=1.0).contains(&v),
            "{secs} с: {v} после {last}"
        );
        last = v;
        secs *= 1.3;
    }
}

#[test]
fn defaults_show_temperature_above_the_die_with_both_curves() {
    // Ловит возврат к абсолютной шкале или к одной кривой по умолчанию. Бриф 16.10: вид по умолчанию — «нагрев ядра»
    // (сырые виды — «над кристаллом» и «абсолютная» — выбираются).
    assert_eq!(Scale::default(), Scale::OwnHeat);
    assert_eq!(Curves::default(), Curves::Both);
}
