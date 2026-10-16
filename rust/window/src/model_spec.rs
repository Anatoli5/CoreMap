// Тесты тепловой модели (мощность p, средние, NNLS, пригодные строки, слой L3, выученная форма своего быстрого нагрева,
// кривые) по спецификации; пишет отдельный агент, не читавший реализацию (rust\AGENTS.md, «Tests from the
// specification»). Источники — бриф outputs\model-impl-brief-2026-10-16.md («Inputs per row», «Layer L3», список
// тестов в «Rules»), описание API model.rs от 16.10 и его дополнение «learned shape of the own fast term» (16.10 днём:
// шарнирная форма g(r), узлы floor(lo) + 2·j до hi − 1, правило колена); путь напряжения убран из модели решением
// владельца 16.10 (напряжение задаёт сам процессор по множителю — не независимый вход), p = занятость·(r/45)³.
// Ожидаемые числа для формы проверены заранее независимой записью генератора и подгонки формы по дополнению (scipy):
// scratchpad\shape-spec\*.py этой сессии.
//
// Ожидаемые числа посчитаны вручную или независимо в самом тесте: синтетический журнал строится из известных
// коэффициентов по определению слоя L3 — свои средние по определению из API, своя система уравнений строки (уровень
// кристалла зависит от температур других блоков), свой разбор пригодности строк; NNLS сверяется перебором подмножеств
// и условиями оптимальности. Формулы реализации сюда не переписывались. Допуски синтетических тестов проверены заранее
// тем же генератором и независимым NNLS (scipy): верная модель восстанавливает коэффициенты с ошибкой ~1e-6, а
// неверные варианты (нет рестарта на перерыве, рестарт уже на 1000 мс, быстрый вход p(t) вместо среднего t и t+1,
// средние до шага строки, сосед на 2- или 8-с средней, шаг Эйлера) — с ошибкой больше допусков.
// У каждого теста в комментарии — какую неверную реализацию он ловит.

use crate::model::{
    ASLEEP, Averages, BlockModel, Coef, Die, GAP_MS, Hinge, KEEP_MS, Log, MIN_MINUTES, MIN_ROWS, Model, NEIGHBOUR_TAU,
    REFERENCE, Reference, TAUS, WARM_MS, fit, log_from_csv, nnls, proxy, trim,
};
use crate::samples::row_time;

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

// Число строк или блоков как f64: их немного, приведение без потерь.
fn real(n: usize) -> f64 {
    f64::from(u32::try_from(n).expect("небольшое число"))
}

// Миллисекунды промежутка в секундах (промежутки тестов — секунды и минуты).
fn secs(ms: i64) -> f64 {
    f64::from(i32::try_from(ms).expect("промежуток в пределах i32")) / 1000.0
}

// ─── 0. Числа брифа ──────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn constants_are_the_numbers_of_the_brief() {
    // Ловит: другие постоянные времени средних (бриф: 0,5; 1; 2; 4; 8; 16 с), соседа не на 4-с средней, перерыв не
    // «больше 1 с», разогрев не 16 с, опорный множитель не ×45, порог сна не 0,05, порог кривой не 10 минут.
    let want = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
    assert!(TAUS.iter().zip(want).all(|(&t, w)| close(t, w, 0.0)), "{TAUS:?}");
    assert!(close(TAUS[NEIGHBOUR_TAU], 4.0, 0.0), "соседи — на 4-с средней");
    assert_eq!(GAP_MS, 1000);
    assert_eq!(WARM_MS, 16_000);
    assert!(close(REFERENCE, 45.0, 0.0));
    assert!(close(ASLEEP, 0.05, 0.0));
    assert!(close(MIN_MINUTES, 10.0, 0.0));
    assert_eq!(MIN_ROWS, 240);
}

// ─── 1. Мощность p ───────────────────────────────────────────────────────────────────────────────────────────────

// p строки известно и равно этому числу.
fn power_is(got: Option<f64>, p: f64) -> bool {
    got.is_some_and(|g| close(g, p, 1e-12))
}

#[test]
fn proxy_is_busy_times_cube_of_relative_ratio() {
    // Решение 16.10: форма мощности принята кубом (не измерена), напряжения в p нет.
    // Ловит: множитель в первой степени или в квадрате вместо куба, деление не на 45, нагрузку не множителем.
    let p = proxy(Some(45.0), Some(1.0));
    assert!(power_is(p, 1.0), "{p:?}");
    // 0,5 × 0,8³ = 0,256
    let p = proxy(Some(36.0), Some(0.5));
    assert!(power_is(p, 0.256), "{p:?}");
    // 1,2³ = 1,728
    let p = proxy(Some(54.0), Some(1.0));
    assert!(power_is(p, 1.728), "{p:?}");
    // 0,25 × (8/9)³ = 0,25 × 512/729
    let p = proxy(Some(40.0), Some(0.25));
    assert!(power_is(p, 0.25 * 512.0 / 729.0), "{p:?}");
    // Множитель есть — p по формуле и при нагрузке ниже порога сна: порог касается только строки без множителя.
    let p = proxy(Some(45.0), Some(0.02));
    assert!(power_is(p, 0.02), "{p:?}");
}

#[test]
fn proxy_is_zero_for_a_sleeping_core_and_unknown_otherwise() {
    // Ловит: пустой множитель как «не известно» при любой нагрузке (спящее ядро выпадало бы из счёта и рвало средние
    // соседей), порог «не больше» вместо «меньше» (ровно 0,05 — уже не сон), значение без нагрузки (пустая нагрузка —
    // не известно, даже с множителем).
    for busy in [0.0, 0.049, 0.01] {
        let p = proxy(None, Some(busy));
        assert!(power_is(p, 0.0), "спит при нагрузке {busy}: {p:?}");
    }
    assert!(proxy(None, Some(0.05)).is_none(), "0,05 — не сон");
    assert!(proxy(None, Some(0.6)).is_none(), "занято, а множителя нет");
    assert!(proxy(None, None).is_none());
    assert!(proxy(Some(45.0), None).is_none(), "без нагрузки");
}

// ─── 2. Скользящие средние ───────────────────────────────────────────────────────────────────────────────────────

fn values_are(got: &[f64; 6], want: &[f64; 6], tol: f64) -> bool {
    got.iter().zip(want).all(|(&g, &w)| close(g, w, tol))
}

#[test]
fn averages_step_toward_power_by_exact_exponential_in_seconds() {
    // Ловит: шаг Эйлера dt/τ вместо 1 − exp(−dt/τ), dt в миллисекундах, сдвиг на долю exp(−dt/τ) вместо 1 − exp(−dt/τ),
    // постоянные в другом порядке, рестарт уже на промежутке ровно 1000 мс.
    let mut m = Averages::start(0, 2.0);
    assert!(values_are(&m.values, &[2.0; 6], 0.0), "{:?}", m.values);
    assert_eq!(m.since(), 0);
    assert!(!m.advance(250, 0.0), "250 мс — не перерыв");
    // 2·exp(−0,25/τ) для τ = 0,5; 1; 2; 4; 8; 16 с — посчитано отдельно.
    let after = [
        1.213_061_319_425_267,
        1.557_601_566_142_81,
        1.764_993_805_169_191,
        1.878_826_125_626_952,
        1.938_466_468_952_688,
        1.968_992_874_010_817,
    ];
    assert!(values_are(&m.values, &after, 1e-12), "{:?}", m.values);
    // Ровно 1000 мс — ещё не перерыв: шаг к 1 при dt = 1 с, v·exp(−1/τ) + (1 − exp(−1/τ)).
    assert!(!m.advance(1250, 1.0), "1000 мс — не перерыв");
    let next = [
        1.028_834_714_011_185,
        1.205_130_152_548_938,
        1.463_992_197_325_347,
        1.684_430_474_821_879,
        1.828_193_752_030_25,
        1.910_284_563_618_934,
    ];
    assert!(values_are(&m.values, &next, 1e-12), "{:?}", m.values);
    assert_eq!(m.since(), 0, "шаги — не рестарт");
}

#[test]
fn averages_restart_after_a_gap_over_a_second_or_when_time_does_not_advance() {
    // Ловит: перерыв от 1000 мс включительно; рестарт без «true»; рестарт, оставивший старые значения или старое время
    // рестарта; перерыв, отсчитанный от времени рестарта, а не от прошлой строки; одинаковое время или часы назад
    // как обычный шаг (dt ≤ 0).
    let mut m = Averages::start(0, 1.0);
    assert!(!m.advance(1000, 5.0), "ровно 1000 мс");
    // τ = 1 с: 1 + 4·(1 − exp(−1)) = 3,528 482 235 314 231
    assert!(close(m.values[1], 3.528_482_235_314_231, 1e-12), "{:?}", m.values);
    assert!(m.advance(2001, 3.0), "1001 мс — перерыв");
    assert!(values_are(&m.values, &[3.0; 6], 0.0), "{:?}", m.values);
    assert_eq!(m.since(), 2001);
    // Шаги по 900 мс: от рестарта уже больше секунды, но каждый промежуток — не перерыв.
    assert!(!m.advance(2901, 3.0));
    assert!(!m.advance(3801, 3.0));
    assert_eq!(m.since(), 2001);
    assert!(m.advance(3801, 4.0), "то же время — рестарт");
    assert!(values_are(&m.values, &[4.0; 6], 0.0), "{:?}", m.values);
    assert_eq!(m.since(), 3801);
    assert!(m.advance(3000, 6.0), "часы назад — рестарт");
    assert!(values_are(&m.values, &[6.0; 6], 0.0), "{:?}", m.values);
    assert_eq!(m.since(), 3000);
}

#[test]
fn averages_two_steps_equal_one_step_of_their_sum() {
    // Свойство точной экспоненты при постоянном p: два шага по 250 мс — то же, что один в 500 мс. Ловит любой
    // приближённый шаг (Эйлер, доля, обрезанная единицей).
    let mut two = Averages::start(0, 0.0);
    assert!(!two.advance(250, 1.0));
    assert!(!two.advance(500, 1.0));
    let mut one = Averages::start(0, 0.0);
    assert!(!one.advance(500, 1.0));
    // 1 − exp(−0,5/τ)
    let want = [
        0.632_120_558_828_558,
        0.393_469_340_287_367,
        0.221_199_216_928_595,
        0.117_503_097_415_405,
        0.060_586_937_186_524,
        0.030_766_765_523_656,
    ];
    assert!(values_are(&one.values, &want, 1e-12), "{:?}", one.values);
    assert!(values_are(&two.values, &want, 1e-12), "{:?}", two.values);
}

// ─── 3. NNLS ─────────────────────────────────────────────────────────────────────────────────────────────────────

// Генератор: SplitMix64 — детерминированный, без зависимостей.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    // Целое 0..n.
    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n)).expect("остаток меньше n")
    }

    // Число в [−1, 1] с шагом 1/1000.
    fn sign(&mut self) -> f64 {
        f64::from(self.below(2001)) / 1000.0 - 1.0
    }
}

// Решение A·x = b (A — size × size по строкам) методом Гаусса с выбором ведущего элемента; None — система вырождена.
fn solve(matrix: &[f64], rhs: &[f64]) -> Option<Vec<f64>> {
    let size = rhs.len();
    let mut rows: Vec<Vec<f64>> = matrix
        .chunks(size)
        .zip(rhs)
        .map(|(row, &last)| row.iter().copied().chain([last]).collect())
        .collect();
    for col in 0..size {
        let pivot = (col..size).max_by(|&p, &q| rows[p][col].abs().total_cmp(&rows[q][col].abs()))?;
        if rows[pivot][col].abs() < 1e-300 {
            return None;
        }
        rows.swap(col, pivot);
        let top = rows[col].clone(); // строка ведущего элемента, пока вычитаем её из нижних
        for row in rows.iter_mut().skip(col + 1) {
            let factor = row[col] / top[col];
            for (cell, &above) in row.iter_mut().zip(&top).skip(col) {
                *cell -= factor * above;
            }
        }
    }
    let mut out = vec![0.0; size];
    for i in (0..size).rev() {
        let tail: f64 = rows[i].iter().zip(&out).skip(i + 1).map(|(c, v)| c * v).sum();
        out[i] = (rows[i][size] - tail) / rows[i][i];
    }
    Some(out)
}

// XᵀX (по строкам) и Xᵀy.
fn normal(x: &[Vec<f64>], y: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let n = x.first().map_or(0, Vec::len);
    let mut gram = vec![0.0; n * n];
    let mut rhs = vec![0.0; n];
    for (row, &target) in x.iter().zip(y) {
        for (i, &xi) in row.iter().enumerate() {
            rhs[i] += xi * target;
            for (g, &xj) in gram[i * n..(i + 1) * n].iter_mut().zip(row) {
                *g += xi * xj;
            }
        }
    }
    (gram, rhs)
}

// cᵀGc − 2rᵀc = ‖Xc − y‖² − ‖y‖²: то, что NNLS делает наименьшим.
fn objective(gram: &[f64], rhs: &[f64], c: &[f64]) -> f64 {
    let n = c.len();
    let quad: f64 = gram
        .chunks(n)
        .zip(c)
        .map(|(row, &ci)| ci * row.iter().zip(c).map(|(g, cj)| g * cj).sum::<f64>())
        .sum();
    quad - 2.0 * rhs.iter().zip(c).map(|(r, ci)| r * ci).sum::<f64>()
}

// Минимум перебором подмножеств: на каждом S решается G_SS·c_S = r_S; допустимы решения с c_S ≥ 0; из допустимых —
// наименьшее значение цели. При положительно определённой G минимум единственный.
fn brute(gram: &[f64], rhs: &[f64]) -> Vec<f64> {
    let n = rhs.len();
    let mut best = (0.0, vec![0.0; n]); // c = 0: цель 0
    for mask in 1_u32..(1 << n) {
        let set: Vec<usize> = (0..n).filter(|&i| (mask >> i) & 1 == 1).collect();
        let sub: Vec<f64> = set
            .iter()
            .flat_map(|&i| set.iter().map(move |&j| gram[i * n + j]))
            .collect();
        let part_rhs: Vec<f64> = set.iter().map(|&i| rhs[i]).collect();
        let Some(part) = solve(&sub, &part_rhs) else {
            continue;
        };
        if part.iter().any(|&v| v < 0.0) {
            continue;
        }
        let mut c = vec![0.0; n];
        for (&i, &v) in set.iter().zip(&part) {
            c[i] = v;
        }
        let f = objective(gram, rhs, &c);
        if f < best.0 {
            best = (f, c);
        }
    }
    best.1
}

// Условия оптимальности (ККТ) задачи min ‖Xc − y‖², c ≥ 0 — необходимые и достаточные: c ≥ 0; w = r − Gc;
// где c > 0 — w = 0; где c = 0 — w ≤ 0. Допуск — доля наибольшего |r|.
fn optimal(gram: &[f64], rhs: &[f64], c: &[f64], share: f64) -> Result<(), String> {
    let n = rhs.len();
    if c.len() != n {
        return Err(format!("длина ответа {} вместо {n}", c.len()));
    }
    let scale = rhs.iter().fold(1e-300_f64, |m, r| m.max(r.abs()));
    let tol = share * scale;
    for (i, (&ci, row)) in c.iter().zip(gram.chunks(n)).enumerate() {
        let w = rhs[i] - row.iter().zip(c).map(|(g, cj)| g * cj).sum::<f64>();
        if !ci.is_finite() || ci < 0.0 {
            return Err(format!("c[{i}] = {ci}"));
        }
        let zero = ci <= 1e-12 * scale.max(1.0);
        if zero && w > tol {
            return Err(format!("c[{i}] = 0, а цель убывает при его росте: w = {w}"));
        }
        if !zero && w.abs() > tol {
            return Err(format!("c[{i}] = {ci} не в минимуме по нему: w = {w}"));
        }
    }
    Ok(())
}

fn all_near(got: &[f64], want: &[f64], tol: f64) -> bool {
    got.len() == want.len() && got.iter().zip(want).all(|(&g, &w)| close(g, w, tol))
}

#[test]
fn nnls_equals_least_squares_when_all_components_are_positive() {
    // G = [[4,1,0],[1,3,1],[0,1,2]], c = (1, 2, 3) → Xᵀy = Gc = (6, 10, 8). Ловит: решение не той системы, остановку
    // до сходимости, обнуление компонент без причины.
    let got = nnls(&[4.0, 1.0, 0.0, 1.0, 3.0, 1.0, 0.0, 1.0, 2.0], &[6.0, 10.0, 8.0]);
    assert!(all_near(&got, &[1.0, 2.0, 3.0], 1e-9), "{got:?}");
}

#[test]
fn nnls_clamps_a_component_whose_unconstrained_optimum_is_negative() {
    // G = [[2,1],[1,2]], Xᵀy = (1, 5): без ограничения c = (−1, 3). При c ≥ 0: c₀ = 0, c₁ — минимум по одному c₁,
    // 2c₁ = 5, c₁ = 2,5; производная цели по c₀ в этой точке 2·(1·2,5 − 1) > 0 — точка оптимальна.
    // Ловит: обычные МНК (−1, 3); отрицательное, просто обрезанное до нуля без пересчёта остальных, (0, 3).
    let got = nnls(&[2.0, 1.0, 1.0, 2.0], &[1.0, 5.0]);
    assert!(all_near(&got, &[0.0, 2.5], 1e-9), "{got:?}");
    // Xᵀy ≤ 0 по всем — лучше всего ноль.
    let none = nnls(&[2.0, 1.0, 1.0, 2.0], &[-1.0, -3.0]);
    assert!(all_near(&none, &[0.0, 0.0], 1e-12), "{none:?}");
}

#[test]
fn nnls_matches_brute_force_on_small_random_problems() {
    // 300 задач: X 10 × 6 и y со знаками вперемешку — у большинства часть компонент в оптимуме на границе, и чтобы
    // попасть в него, активный набор приходится менять (переменная, вошедшая первой, потом выходит). Ловит: жадное
    // добавление без шага назад, выход без пересчёта, неверный выбор входящей переменной, остановку раньше оптимума.
    let mut dice = Dice(11);
    let mut mixed = 0;
    for task in 0..300 {
        let x: Vec<Vec<f64>> = (0..10).map(|_| (0..6).map(|_| dice.sign()).collect()).collect();
        let y: Vec<f64> = (0..10).map(|_| dice.sign() + 0.3).collect();
        let (gram, rhs) = normal(&x, &y);
        let want = brute(&gram, &rhs);
        let got = nnls(&gram, &rhs);
        assert!(all_near(&got, &want, 1e-8), "задача {task}: {got:?}, перебор {want:?}");
        if want.iter().any(|&v| v > 0.0) && want.iter().any(|&v| v <= 0.0) {
            mixed += 1;
        }
    }
    // Предпосылка теста: задачи с частью компонент на границе — большинство.
    assert!(mixed >= 150, "задач с границей только {mixed}");
}

#[test]
fn nnls_is_optimal_on_correlated_moving_averages() {
    // Задача вида слоя L3: уровень кристалла, быстрый вход и 6 средних своего блока, быстрый вход и 4-с средняя четырёх
    // соседей — 16 сильно связанных столбцов (средние соседних τ почти одинаковы); у истины есть отрицательные
    // компоненты, и в оптимуме часть c — на нуле. Проверка — условия оптимальности (необходимы и достаточны).
    // Ловит: остановку по числу шагов, допуск, при котором переменная с выгодой не входит, потерю точности на
    // плохо обусловленной системе.
    let rows = 3000;
    let loads = loads(77, 5, rows, 15);
    let times = times(rows, &[]);
    let heat: Vec<Vec<f64>> = loads
        .iter()
        .map(|row| row.iter().map(|&l| power(l)).collect())
        .collect();
    let means: Vec<(Vec<[f64; 6]>, Vec<i64>)> = (0..5).map(|b| means(&times, &column(&heat, b))).collect();
    let mut dice = Dice(78);
    let mut level = 50.0;
    let mut table = Vec::with_capacity(rows);
    for t in 0..rows - 1 {
        level += f64::from(dice.below(2001)) / 10_000.0 - 0.1;
        let fast = |b: usize| f64::midpoint(heat[t][b], heat[t + 1][b]);
        let mut row = vec![level, fast(0)];
        row.extend(means[0].0[t]);
        for (b, own) in means.iter().enumerate().skip(1) {
            row.push(fast(b));
            row.push(own.0[t][3]);
        }
        table.push(row);
    }
    let truth = [
        0.6, 8.0, 0.0, 2.0, 0.0, 0.0, 3.0, -3.0, 1.0, 1.0, -1.0, 0.5, 0.0, 2.0, 0.3, 0.3,
    ];
    let target: Vec<f64> = table
        .iter()
        .map(|row| row.iter().zip(truth).map(|(v, w)| v * w).sum::<f64>() + dice.sign() / 2.0)
        .collect();
    // Свободный член: столбцы и цель — без своих средних.
    let mean_of = |values: &mut dyn Iterator<Item = f64>| {
        let (sum, count) = values.fold((0.0, 0_usize), |(sum, count), value| (sum + value, count + 1));
        sum / real(count)
    };
    let centres: Vec<f64> = (0..truth.len())
        .map(|k| mean_of(&mut table.iter().map(|row| row[k])))
        .collect();
    let target_mean = mean_of(&mut target.iter().copied());
    let shifted: Vec<Vec<f64>> = table
        .iter()
        .map(|row| row.iter().zip(&centres).map(|(v, m)| v - m).collect())
        .collect();
    let aim: Vec<f64> = target.iter().map(|v| v - target_mean).collect();
    let (gram, rhs) = normal(&shifted, &aim);
    let got = nnls(&gram, &rhs);
    if let Err(why) = optimal(&gram, &rhs, &got, 1e-6) {
        panic!("{why}; ответ {got:?}");
    }
    // Предпосылка: ограничение работает — часть компонент в оптимуме на нуле.
    assert!(got.iter().filter(|&&v| v <= 1e-12).count() >= 2, "{got:?}");
}

#[test]
fn nnls_stays_finite_with_a_zero_column_and_a_repeated_column() {
    // Столбец, который не менялся (сосед проспал весь журнал — его p всегда 0), и два одинаковых столбца: G вырождена,
    // минимум не единственный. Требование спецификации — ответ есть минимум: конечный, неотрицательный, с той же целью,
    // что у задачи без лишних столбцов. Ловит: деление на ноль или NaN при входе второго из одинаковых столбцов, вход
    // нулевого столбца, бесконечный цикл обмена.
    let mut dice = Dice(5);
    let base: Vec<(f64, f64)> = (0..40).map(|_| (dice.sign(), dice.sign())).collect();
    let y: Vec<f64> = base
        .iter()
        .map(|&(a, b)| 2.0 * a + 0.5 * b + dice.sign() / 10.0)
        .collect();
    let full: Vec<Vec<f64>> = base.iter().map(|&(a, b)| vec![a, a, 0.0, b]).collect();
    let short: Vec<Vec<f64>> = base.iter().map(|&(a, b)| vec![a, b]).collect();
    let (gram, rhs) = normal(&full, &y);
    let (gram_s, rhs_s) = normal(&short, &y);
    let got = nnls(&gram, &rhs);
    let want = brute(&gram_s, &rhs_s);
    assert!(
        got.len() == 4 && got.iter().all(|v| v.is_finite() && *v >= 0.0),
        "{got:?}"
    );
    assert!(
        close(objective(&gram, &rhs, &got), objective(&gram_s, &rhs_s, &want), 1e-9),
        "цель {} вместо {}",
        objective(&gram, &rhs, &got),
        objective(&gram_s, &rhs_s, &want)
    );
    assert!(close(got[0] + got[1], want[0], 1e-8), "{got:?} против {want:?}");
    assert!(close(got[3], want[1], 1e-8), "{got:?} против {want:?}");
}

// ─── 4. Сетка множителей и неубывание ───────────────────────────────────────────────────────────────────────────────

// Сетка множителей от lo до hi с шагом step (включая края).
fn grid(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for k in 0_u32.. {
        let r = lo + step * f64::from(k);
        if r > hi + step * 1e-6 {
            break;
        }
        out.push(r);
    }
    out
}

// Неубывание по сетке: первая точка, где значение падает больше чем на tol, — ошибка.
fn rising(values: &[(f64, f64)], tol: f64) -> Result<(), String> {
    for pair in values.windows(2) {
        let ((r0, v0), (r1, v1)) = (pair[0], pair[1]);
        if v1 < v0 - tol {
            return Err(format!("на ×{r0:.2} {v0:.6}, на ×{r1:.2} {v1:.6}"));
        }
    }
    Ok(())
}

// ─── 5. Журнал: строки и столбцы ─────────────────────────────────────────────────────────────────────────────────

// Полночь 15.10 + 10 ч: время первой строки тестовых журналов.
const START_MS: i64 = 36_000_000;

// Заголовок, как его пишет программа входа (0.5.4), для блоков с этими именами; volts — со столбцами напряжения,
// cores — со столбцом мощности ядер power_cores.
fn header(names: &[String], volts: bool, cores: bool) -> String {
    let mut cols: Vec<String> = ["time", "package", "fan_cpu", "fan_gpu"].map(String::from).to_vec();
    for n in names {
        for what in ["ceiling", "ratio", "busy", "temp"] {
            cols.push(format!("{n}.{what}"));
        }
    }
    cols.extend(["power_cpu", "temp_gpu", "power_gpu", "busy_gpu"].map(String::from));
    if cores {
        cols.push("power_cores".to_string());
    }
    if volts {
        cols.extend(names.iter().map(|n| format!("{n}.volts")));
    }
    cols.join(",")
}

// Время строки «2026-10-15 чч:мм:сс.ммм» из миллисекунд от полуночи.
fn stamp(ms: i64) -> String {
    format!(
        "2026-10-15 {:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

// Поля блока в строке: множитель, нагрузка, температура как текст CSV (пустая строка — не известно).
type Cell = (String, String, String);

// Строка журнала без столбцов напряжения; посторонние столбцы постоянны (потолок 50, корпус 70, вентиляторы, мощности).
fn plain_line(ms: i64, cells: &[Cell]) -> String {
    let blocks: Vec<String> = cells
        .iter()
        .map(|(ratio, busy, temp)| format!("50,{ratio},{busy},{temp}"))
        .collect();
    format!("{},70,3000,2900,{},45.0,55,,", stamp(ms), blocks.join(","))
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|&n| n.to_string()).collect()
}

// Журнал из строк: заголовок без напряжения и строки (время, поля блоков).
fn hand_log(blocks: &[String], rows: &[(i64, Vec<Cell>)]) -> Log {
    let mut text = header(blocks, false, false);
    for (ms, cells) in rows {
        text.push('\n');
        text.push_str(&plain_line(*ms, cells));
    }
    text.push('\n');
    log_from_csv(&text)
}

// Обычные поля блока b в строке k: множитель 40…44, нагрузка 0,50…0,90, температура 60…68 °C.
fn usual(k: usize, b: usize) -> Cell {
    (
        format!("{}", 40 + k % 5),
        format!("0.{}", 50 + 5 * (k % 9)),
        format!("{}", 60 + b + k % 7),
    )
}

#[test]
fn log_keeps_only_rows_with_milliseconds() {
    // Ловит: строки старого журнала (время до секунды, шаг 2 с) в модели — каждая начинала бы средние заново, ни одна не
    // была бы пригодна, а счёт строк врал бы; пустой журнал (только заголовок) — паника вместо нуля строк и пустой модели.
    let blocks = names(&["0", "1"]);
    let mut text = header(&blocks, false, false);
    for second in ["57", "58", "59"] {
        text.push_str("\n2026-10-15 09:59:");
        text.push_str(second);
        text.push_str(",70,3000,2900,50,45,0.80,60,50,45,0.80,61,45.0,55,,");
    }
    for k in 0_u8..5 {
        let ms = START_MS + 250 * i64::from(k);
        let cells: Vec<Cell> = (0..2).map(|b| usual(usize::from(k), b)).collect();
        text.push('\n');
        text.push_str(&plain_line(ms, &cells));
    }
    text.push('\n');
    assert_eq!(log_from_csv(&text).rows(), 5, "только строки с миллисекундами");
    let empty = log_from_csv(&(header(&blocks, false, false) + "\n"));
    assert_eq!(empty.rows(), 0);
    let die = Die {
        names: blocks,
        e: vec![false, false],
        neighbours: vec![vec![1], vec![0]],
    };
    let model = fit(&[&empty], &die);
    assert_eq!(model.blocks.len(), 2);
    for b in &model.blocks {
        assert_eq!(b.usable, 0);
        assert!(b.coef.is_none() && b.curve(45.0).is_none() && !b.drawn(), "{}", b.name);
    }
}

// ─── 6. Пригодные строки ─────────────────────────────────────────────────────────────────────────────────────────

// Цепочка 0–1–2 в порядке файла.
fn chain() -> Die {
    Die {
        names: names(&["0", "1", "2"]),
        e: vec![false, false, false],
        neighbours: vec![vec![1], vec![0, 2], vec![1]],
    }
}

#[test]
fn rows_within_warm_up_and_rows_without_a_next_row_are_not_usable() {
    // Ловит: разогрев короче или длиннее 16 с или «>» вместо «≥»; нет рестарта на промежутке 1001 мс или рестарт уже
    // на 1000 мс; новый файл без рестарта (средние тянутся через файлы); строку перед перерывом или последнюю строку
    // файла в счёте (у неё нет t+1), t+1 из следующего файла; минуты как «строки × 0,25 с» (промежуток 1000 мс перед
    // строкой — 1 с, а не 0,25).
    let die = chain();
    let mut first: Vec<(i64, Vec<Cell>)> = Vec::new();
    // Часть 1: 100 строк через 250 мс; перерыв 1001 мс; часть 2: 80 строк; промежуток ровно 1000 мс; часть 3: 40 строк.
    let mut at = START_MS;
    for (part, rows, before) in [(0, 100, 0), (1, 80, 1001), (2, 40, 1000)] {
        for k in 0..rows {
            at += if k == 0 { before } else { 250 };
            let cells = (0..3).map(|b| usual(k + 7 * part, b)).collect();
            first.push((at, cells));
        }
    }
    // Файл 2: 80 строк, первая — через 250 мс после последней строки файла 1.
    let second: Vec<(i64, Vec<Cell>)> = (0_u8..80)
        .map(|k| {
            (
                at + 250 * (1 + i64::from(k)),
                (0..3).map(|b| usual(usize::from(k), b)).collect(),
            )
        })
        .collect();
    let one = hand_log(&die.names, &first);
    let two = hand_log(&die.names, &second);
    assert_eq!((one.rows(), two.rows()), (220, 80));
    let model: Model = fit(&[&one, &two], &die);
    assert_eq!(model.rows, 300);
    // По правилам API, у всех блоков одинаково (все значения известны):
    // часть 1 — k = 64…98 (время от начала ≥ 16 000 мс ⇔ k ≥ 64; у k = 99 следующая строка после перерыва): 35;
    // часть 2 — рестарт в её первой строке; k = 64…79 (у k = 79 следующая через 1000 мс — в пределах): 16;
    // часть 3 — рестарта нет, разогрев прошёл; k = 0…38 (k = 39 — последняя в файле): 39;
    // файл 2 — рестарт в первой строке; k = 64…78: 15. Всего 105.
    // Минуты: у 104 строк промежуток до прошлой 250 мс, у первой строки части 3 — 1000 мс: 27 000 мс = 0,45 мин.
    for b in &model.blocks {
        assert_eq!(b.usable, 105, "блок {}", b.name);
        assert!(close(b.minutes, 0.45, 1e-9), "блок {}: {} мин", b.name, b.minutes);
        assert!(
            b.coef.is_none(),
            "блок {}: меньше {MIN_ROWS} строк — коэффициентов нет",
            b.name
        );
    }
}

#[test]
fn unknown_power_restarts_the_block_and_its_neighbours_but_sleep_does_not() {
    // Один файл, 200 строк через 250 мс, блоки 0–1–2 цепочкой; порядок модели нарочно не порядок файла (2, 0, 1),
    // блок 2 — кластер E. События:
    // • строки 70…79: у блока 2 множителя нет, нагрузка 0,01 — ядро спало, p = 0 известно: ничего не рвётся;
    // • строка 90: нет температуры у блоков 1 и 2 — у блока 0 уровень кристалла не известен (ни одного из двух других),
    //   у блоков 1 и 2 нет своей температуры;
    // • строка 100: нет температуры у блока 2 — уровень для 0 и 1 известен (половина других);
    // • строка 120: у блока 2 множителя нет при нагрузке 0,60 — p не известно; средние блока 2 начинаются заново со
    //   строки 121 (первая строка, где p снова известно), строки до 121 + 64 = 185 — разогрев.
    // Ловит: спящее ядро как неизвестное; соседа, чьё p не известно в t или t+1, без последствий; разогрев средних
    // соседа (его 4-с средняя — вход блока) без последствий; рестарт у всех блоков, а не у того, чьё p пропало;
    // уровень кристалла при пропуске половины других как неизвестный или без своей температуры как известный;
    // блоки модели по месту в файле, а не по имени; имя и признак кластера не того блока.
    let file = names(&["0", "1", "2"]);
    let rows: Vec<(i64, Vec<Cell>)> = (0..200)
        .map(|k| {
            let mut cells: Vec<Cell> = (0..3).map(|b| usual(k, b)).collect();
            if (70..80).contains(&k) {
                cells[2].0.clear();
                cells[2].1 = "0.01".to_string();
            }
            if k == 90 {
                cells[1].2.clear();
                cells[2].2.clear();
            }
            if k == 100 {
                cells[2].2.clear();
            }
            if k == 120 {
                cells[2].0.clear();
                cells[2].1 = "0.60".to_string();
            }
            (START_MS + 250 * i64::try_from(k).expect("строк мало"), cells)
        })
        .collect();
    let log = hand_log(&file, &rows);
    let die = Die {
        names: names(&["2", "0", "1"]),
        e: vec![true, false, false],
        neighbours: vec![vec![2], vec![2], vec![1, 0]],
    };
    let model = fit(&[&log], &die);
    // Блок 0 (сосед — 1): 200 − 64 (разогрев) − 1 (строка 90) − 1 (последняя) = 134.
    // Блок 1 (соседи 0 и 2): − 64 − 1 (90) − 2 (119: p₂(t+1), 120: p₂(t)) − 64 (121…184) − 1 = 68.
    // Блок 2 (сосед 1): − 64 − 1 (90) − 1 (100) − 2 (119, 120) − 64 (121…184) − 1 = 67.
    let want = [("2", true, 67), ("0", false, 134), ("1", false, 68)];
    assert_eq!(model.blocks.len(), 3);
    for (b, (name, e, usable)) in model.blocks.iter().zip(want) {
        assert_eq!((b.name.as_str(), b.e), (name, e), "порядок модели — порядок die.names");
        assert_eq!(b.usable, usable, "блок {name}");
        // Все промежутки — 250 мс.
        let minutes = real(usable) * 0.25 / 60.0;
        assert!(
            close(b.minutes, minutes, 1e-9),
            "блок {name}: {} мин вместо {minutes}",
            b.minutes
        );
    }
}

// ─── 7. Синтетический журнал из известного слоя L3 ───────────────────────────────────────────────────────────────

// Нагрузка блока в строке: сотые доли занятости и множитель (None — ядро спало, множителя в строке нет).
#[derive(Clone, Copy)]
struct Load {
    busy: u32,
    ratio: Option<u32>,
}

// Длительности отрезков постоянной нагрузки в строках (0,25…32 с) — нагрузка меняется на всех масштабах, и средние
// разных τ не сливаются в один столбец.
const SPANS: [u32; 14] = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128];
const RATIOS: [u32; 7] = [38, 40, 42, 44, 45, 46, 48];
const TAU_S: [f64; 6] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

// Нагрузки [строка][блок]: отрезки случайной длины; sleep — процент отрезков, когда блок спит (нагрузка 0…0,04, без
// множителя), иначе нагрузка 0,05…1,00 и множитель из RATIOS.
fn loads(seed: u64, blocks: usize, rows: usize, sleep: u32) -> Vec<Vec<Load>> {
    let mut dice = Dice(seed);
    let mut left = vec![0_u32; blocks];
    let mut now = vec![Load { busy: 0, ratio: None }; blocks];
    let mut out = Vec::with_capacity(rows);
    for _ in 0..rows {
        for (l, n) in left.iter_mut().zip(now.iter_mut()) {
            if *l == 0 {
                *l = SPANS[usize::try_from(dice.below(14)).expect("индекс")];
                *n = if dice.below(100) < sleep {
                    Load {
                        busy: dice.below(5),
                        ratio: None,
                    }
                } else {
                    let busy = 5 + dice.below(96);
                    Load {
                        busy,
                        ratio: Some(RATIOS[usize::try_from(dice.below(7)).expect("индекс")]),
                    }
                };
            }
            *l -= 1;
        }
        out.push(now.clone());
    }
    out
}

// Время строк: через 250 мс, перед строками из gaps — заданный промежуток.
fn times(rows: usize, gaps: &[(usize, i64)]) -> Vec<i64> {
    let mut at = START_MS;
    (0..rows)
        .map(|k| {
            if k > 0 {
                at += gaps.iter().find(|g| g.0 == k).map_or(250, |g| g.1);
            }
            at
        })
        .collect()
}

// p по определению: занятость × (r/45)³; спящее ядро — 0.
fn power(l: Load) -> f64 {
    l.ratio
        .map_or(0.0, |r| f64::from(l.busy) / 100.0 * (f64::from(r) / 45.0).powi(3))
}

fn column(p: &[Vec<f64>], b: usize) -> Vec<f64> {
    p.iter().map(|row| row[b]).collect()
}

// Средние по определению API — своя запись, не модели: рестарт в первой строке, после промежутка > 1000 мс или если
// время не растёт; иначе шаг к p на долю 1 − exp(−dt/τ). Значения после строки и время последнего рестарта.
fn means(times: &[i64], p: &[f64]) -> (Vec<[f64; 6]>, Vec<i64>) {
    let mut v = [0.0; 6];
    let mut last: Option<i64> = None;
    let mut since = 0;
    let mut values = Vec::with_capacity(p.len());
    let mut starts = Vec::with_capacity(p.len());
    for (&at, &pt) in times.iter().zip(p) {
        match last {
            Some(l) if at > l && at - l <= 1000 => {
                let dt = secs(at - l);
                for (vk, tau) in v.iter_mut().zip(TAU_S) {
                    *vk += (1.0 - (-dt / tau).exp()) * (pt - *vk);
                }
            }
            _ => {
                v = [pt; 6];
                since = at;
            }
        }
        last = Some(at);
        values.push(v);
        starts.push(since);
    }
    (values, starts)
}

// Есть ли у строки t следующая строка того же файла в пределах 1000 мс.
fn has_next(times: &[i64], t: usize) -> bool {
    times.get(t + 1).is_some_and(|&n| n > times[t] && n - times[t] <= 1000)
}

// Свой быстрый нагрев блока при полной своей нагрузке как функция фактического множителя, °C.
type Shape = Box<dyn Fn(f64) -> f64>;

// Известный слой L3 блока: a, β, 7 своих коэффициентов (быстрый вход, средние 0,5…16 с), соседи (номер, коэффициент
// быстрого входа, коэффициент 4-с средней) и, если задана, форма своего быстрого нагрева: тогда быстрый член строки —
// среднее занятость·f(r) в t и t+1 (спящее ядро — 0), а own[0] = 0; средние и соседи — по p, как всегда.
struct Truth {
    a: f64,
    beta: f64,
    own: [f64; 7],
    near: Vec<(usize, f64, f64)>,
    fast: Option<Shape>,
}

fn truth(a: f64, beta: f64, own: [f64; 7], near: &[(usize, f64, f64)]) -> Truth {
    Truth {
        a,
        beta,
        own,
        near: near.to_vec(),
        fast: None,
    }
}

// Истина с формой своего быстрого нагрева f и коэффициентами своих средних 0,5…16 с (history).
fn truth_with_shape(a: f64, beta: f64, fast: Shape, history: [f64; 6], near: &[(usize, f64, f64)]) -> Truth {
    let mut own = [0.0; 7];
    own[1..].copy_from_slice(&history);
    Truth {
        a,
        beta,
        own,
        near: near.to_vec(),
        fast: Some(fast),
    }
}

// Кристалл 2 × 2 без диагоналей (0–1, 0–2, 1–3, 2–3) с коэффициентами, при которых температуры 40…85 °C.
fn square() -> Vec<Truth> {
    vec![
        truth(
            18.0,
            0.55,
            [6.0, 0.0, 3.0, 0.0, 4.0, 0.0, 5.0],
            &[(1, 1.5, 2.0), (2, 0.0, 3.0)],
        ),
        truth(
            20.0,
            0.50,
            [4.0, 2.0, 0.0, 5.0, 0.0, 3.0, 2.0],
            &[(0, 1.0, 0.0), (3, 0.5, 1.5)],
        ),
        truth(
            16.0,
            0.60,
            [8.0, 0.0, 0.0, 0.0, 6.0, 0.0, 6.0],
            &[(0, 2.0, 1.0), (3, 0.0, 0.0)],
        ),
        truth(19.0, 0.50, [3.0; 7], &[(1, 0.8, 0.8), (2, 1.2, 2.2)]),
    ]
}

// Синтетический журнал одного файла и всё, что о нём известно по построению.
struct Synth {
    die: Die,
    times: Vec<i64>,
    loads: Vec<Vec<Load>>,                 // [строка][блок]
    p: Vec<Vec<f64>>,                      // [строка][блок]
    means: Vec<(Vec<[f64; 6]>, Vec<i64>)>, // [блок]: средние после строки, время рестарта
    temps: Vec<Vec<String>>,               // [строка][блок], как записаны
    usable: Vec<Vec<bool>>,                // [блок][строка], по правилам API
}

impl Synth {
    // Быстрый вход блока b в строке t: среднее p в t и t+1 (без t+1 строка не пригодна; для температуры — p(t)).
    fn fast(&self, b: usize, t: usize) -> f64 {
        if has_next(&self.times, t) {
            f64::midpoint(self.p[t][b], self.p[t + 1][b])
        } else {
            self.p[t][b]
        }
    }

    // Свой быстрый нагрев блока b в строке t по форме: среднее занятость·форма(r) в t и t+1 (без t+1 — в t); спящее
    // ядро — 0.
    fn heat(&self, b: usize, t: usize, shape: &dyn Fn(f64) -> f64) -> f64 {
        let at = |k: usize| {
            let load = self.loads[k][b];
            load.ratio
                .map_or(0.0, |r| f64::from(load.busy) / 100.0 * shape(f64::from(r)))
        };
        if has_next(&self.times, t) {
            f64::midpoint(at(t), at(t + 1))
        } else {
            at(t)
        }
    }

    // Входы слоя L3 блока i в строке t без уровня кристалла: быстрый, 6 средних, соседи (быстрый, 4-с средняя).
    fn inputs(&self, near: &[(usize, f64, f64)], i: usize, t: usize) -> Vec<f64> {
        let mut x = vec![self.fast(i, t)];
        x.extend(self.means[i].0[t]);
        for &(j, _, _) in near {
            x.push(self.fast(j, t));
            x.push(self.means[j].0[t][3]);
        }
        x
    }

    // Уровень кристалла для блока i: средняя записанных температур остальных блоков.
    fn die_level(&self, i: usize, t: usize) -> f64 {
        let others: f64 = self.temps[t]
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != i)
            .map(|(_, s)| s.parse::<f64>().expect("число"))
            .sum();
        others / real(self.temps[t].len() - 1)
    }

    // Текст CSV. volts — напряжение [строка][блок] (None у строки — поле пустое; без volts — столбцов нет);
    // noise — посторонние столбцы (потолки, корпус, вентиляторы, мощности, видеокарта) случайны с этим зерном.
    fn csv(&self, volts: Option<&[Vec<Option<f64>>]>, noise: Option<u64>) -> String {
        self.write(volts, noise, None)
    }

    // То же; watts — мощности строк вместо постоянной power_cpu (и столбец power_cores, если задан).
    fn write(&self, volts: Option<&[Vec<Option<f64>>]>, noise: Option<u64>, watts: Option<&Watts>) -> String {
        let mut dice = Dice(noise.unwrap_or(0));
        let mut random = |lo: u32, span: u32| noise.map(|_| lo + dice.below(span));
        let cores = watts.and_then(|w| w.cores);
        let shown = |v: Option<f64>| v.map_or_else(String::new, |x| format!("{x:.4}"));
        let mut lines = vec![header(&self.die.names, volts.is_some(), cores.is_some())];
        for (t, (&ms, row)) in self.times.iter().zip(&self.loads).enumerate() {
            let mut fields = vec![
                stamp(ms),
                random(40, 60).unwrap_or(70).to_string(),
                random(2000, 4000).unwrap_or(3000).to_string(),
                random(2000, 4000).unwrap_or(2900).to_string(),
            ];
            for (l, temp) in row.iter().zip(&self.temps[t]) {
                fields.push(random(30, 26).unwrap_or(50).to_string());
                fields.push(l.ratio.map_or_else(String::new, |r| r.to_string()));
                fields.push(format!("{}.{:02}", l.busy / 100, l.busy % 100));
                fields.push(temp.clone()); // текст поля строки
            }
            let cpu = format!("{}.0", random(5, 120).unwrap_or(45));
            fields.push(watts.map_or(cpu, |w| shown(w.cpu[t])));
            fields.push(random(40, 50).unwrap_or(55).to_string());
            fields.push(if t % 8 == 0 {
                format!("{}.0", random(20, 100).unwrap_or(80))
            } else {
                String::new()
            });
            fields.push(random(0, 100).map_or_else(String::new, |v| v.to_string()));
            if let Some(c) = cores {
                fields.push(shown(c[t]));
            }
            if let Some(v) = volts {
                fields.extend(v[t].iter().map(|x| x.map_or_else(String::new, |x| format!("{x:.4}"))));
            }
            lines.push(fields.join(","));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

// Журнал из известного слоя L3: нагрузки и время, свои средние, затем в каждой строке — система уравнений
// T_i − β_i·ср(T_j, j ≠ i) = a_i + Σ own·x + Σ соседи (уровень кристалла — из температур других блоков той же строки).
// whole — температуры целыми градусами, как в настоящем журнале; иначе с шестью знаками (практически без шума).
fn synth(truths: &[Truth], rows: usize, gaps: &[(usize, i64)], sleep: u32, whole: bool) -> Synth {
    let loads = loads(20_261_016, truths.len(), rows, sleep);
    synth_from(truths, loads, times(rows, gaps), whole)
}

// То же из готовых нагрузок и времени строк (тест может связать нагрузки блоков).
fn synth_from(truths: &[Truth], loads: Vec<Vec<Load>>, times: Vec<i64>, whole: bool) -> Synth {
    let n = truths.len();
    let rows = times.len();
    let p: Vec<Vec<f64>> = loads
        .iter()
        .map(|row| row.iter().copied().map(power).collect())
        .collect();
    let means: Vec<(Vec<[f64; 6]>, Vec<i64>)> = (0..n).map(|b| means(&times, &column(&p, b))).collect();
    let die = Die {
        names: (0..n).map(|b| b.to_string()).collect(),
        e: vec![false; n],
        neighbours: truths.iter().map(|t| t.near.iter().map(|x| x.0).collect()).collect(),
    };
    let mut s = Synth {
        die,
        times,
        loads,
        p,
        means,
        temps: Vec::with_capacity(rows),
        usable: Vec::new(),
    };
    let mut system = vec![0.0; n * n];
    for (i, row) in system.chunks_mut(n).enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = if i == j { 1.0 } else { -truths[i].beta / real(n - 1) };
        }
    }
    for t in 0..rows {
        let force: Vec<f64> = truths
            .iter()
            .enumerate()
            .map(|(i, tr)| {
                let x = s.inputs(&tr.near, i, t);
                let weights = tr
                    .own
                    .iter()
                    .copied()
                    .chain(tr.near.iter().flat_map(|&(_, f, sl)| [f, sl]));
                let own_fast = tr.fast.as_ref().map_or(0.0, |f| s.heat(i, t, f));
                tr.a + own_fast + x.iter().zip(weights).map(|(v, w)| v * w).sum::<f64>()
            })
            .collect();
        let temps = solve(&system, &force).expect("система строки решается");
        s.temps.push(
            temps
                .iter()
                .map(|&v| {
                    if whole {
                        format!("{}", v.round())
                    } else {
                        format!("{v:.6}")
                    }
                })
                .collect(),
        );
    }
    s.usable = (0..n)
        .map(|i| {
            (0..rows)
                .map(|t| {
                    has_next(&s.times, t)
                        && std::iter::once(i)
                            .chain(s.die.neighbours[i].iter().copied())
                            .all(|b| s.times[t] - s.means[b].1[t] >= 16_000)
                })
                .collect()
        })
        .collect();
    s
}

const GAPS: [(usize, i64); 2] = [(1800, 3000), (2600, 1000)];

fn used(s: &Synth, i: usize) -> usize {
    s.usable[i].iter().filter(|&&u| u).count()
}

// Допуски сверки с истиной: β, сумма своих, каждый свой (None — не сверяется), сосед, a.
struct Tolerance {
    beta: f64,
    own_total: f64,
    own_each: Option<f64>,
    near: f64,
    a: f64,
}

fn recovered(model: &Model, truths: &[Truth], tol: &Tolerance) {
    for (i, (b, tr)) in model.blocks.iter().zip(truths).enumerate() {
        let c = b
            .coef
            .as_ref()
            .unwrap_or_else(|| panic!("блок {i}: нет коэффициентов при {} строках", b.usable));
        assert!(
            close(c.beta, tr.beta, tol.beta),
            "блок {i}: β {} вместо {}",
            c.beta,
            tr.beta
        );
        let own: f64 = tr.own.iter().sum();
        assert!(
            close(c.own_total(), own, tol.own_total),
            "блок {i}: свои {} вместо {own}",
            c.own_total()
        );
        if let Some(each) = tol.own_each {
            for (k, (&got, &want)) in c.own.iter().zip(&tr.own).enumerate() {
                assert!(
                    close(got, want, each),
                    "блок {i}: свой {k}: {got} вместо {want} ({:?})",
                    c.own
                );
            }
        }
        assert_eq!(
            c.neighbours.iter().map(|x| x.block).collect::<Vec<_>>(),
            tr.near.iter().map(|x| x.0).collect::<Vec<_>>(),
            "блок {i}: соседи в порядке die.neighbours"
        );
        for (got, &(j, fast, slow)) in c.neighbours.iter().zip(&tr.near) {
            assert!(
                close(got.fast, fast, tol.near) && close(got.slow, slow, tol.near),
                "блок {i}, сосед {j}: ({}, {}) вместо ({fast}, {slow})",
                got.fast,
                got.slow
            );
        }
        assert!(close(c.a, tr.a, tol.a), "блок {i}: a {} вместо {}", c.a, tr.a);
    }
}

#[test]
fn synthetic_log_gains_are_recovered() {
    // 4000 строк (≈ 17 мин) кристалла 2 × 2, нагрузка меняется на масштабах 0,25…32 с, 15 % отрезков ядро спит (нет
    // множителя), перерыв 3 с на строке 1800 и промежуток ровно 1 с на строке 2600; температуры — по слою L3 с
    // известными неотрицательными коэффициентами (часть своих и соседских — нули). Модель должна вернуть их.
    // Ловит (проверено независимым NNLS на том же журнале, ошибки неверных вариантов больше допусков): нет рестарта
    // средних на перерыве или рестарт на 1000 мс; быстрый вход p(t) вместо среднего t и t+1; средние до шага строки t;
    // соседа не на 4-с средней; шаг Эйлера; спящее ядро не как p = 0; уровень кристалла с самим блоком; сдвиг
    // соседей между блоками.
    let truths = square();
    let s = synth(&truths, 4000, &GAPS, 15, false);
    let log = log_from_csv(&s.csv(None, None));
    assert_eq!(log.rows(), 4000);
    let model = fit(&[&log], &s.die);
    for (i, b) in model.blocks.iter().enumerate() {
        assert_eq!(b.usable, used(&s, i), "блок {i}: пригодных строк");
        assert!(b.drawn(), "блок {i}: {} мин — кривая рисуется", b.minutes);
    }
    let tol = Tolerance {
        beta: 0.002,
        own_total: 0.005,
        own_each: Some(0.05),
        near: 0.01,
        a: 0.05,
    };
    recovered(&model, &truths, &tol);
}

#[test]
fn synthetic_log_in_whole_degrees_is_recovered_roughly() {
    // Тот же журнал, температуры — целые градусы, как пишет программа входа. Допуски шире на шум округления (тот же
    // журнал, независимый NNLS: β — 0,005, сумма своих — 0,4 %, сосед — 0,08 °C, a — 0,2 °C); отдельные свои
    // коэффициенты на таком шуме делят сумму иначе (до ±1 °C при той же сумме) — их не сверяем.
    // Ловит: модель, верную только на точных данных, — NNLS, не доводящий обмен активного набора до конца, когда
    // остаток не нулевой и часть истинных нулей уходит в плюс; разбор только дробной температуры.
    let truths = square();
    let s = synth(&truths, 4000, &GAPS, 15, true);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let own_max = truths.iter().map(|t| t.own.iter().sum::<f64>()).fold(0.0, f64::max);
    let tol = Tolerance {
        beta: 0.02,
        own_total: 0.02 * own_max,
        own_each: None,
        near: 0.3,
        a: 0.8,
    };
    recovered(&model, &truths, &tol);
}

// Всё числовое, что модель знает о блоке: пригодные строки и минуты, слой L3 в старом виде и с формой (вместе с самой
// формой), опорные условия, кривая на ×30…×60 через 2.
fn block_numbers(b: &BlockModel) -> Vec<f64> {
    let mut v = vec![real(b.usable), b.minutes];
    for c in [b.coef.as_ref(), b.shaped.as_ref()].into_iter().flatten() {
        v.extend(coef_numbers(c));
        v.push(c.shape.lo);
        v.extend(&c.shape.knots);
        v.extend(&c.shape.weights);
    }
    if let Some(rf) = &b.reference {
        v.extend([rf.die, rf.lo, rf.hi, rf.median]);
        v.extend(rf.neighbours.iter().flat_map(|&(j, p)| [real(j), p]));
    }
    v.extend(grid(30.0, 60.0, 2.0).into_iter().filter_map(|r| b.curve(r)));
    v
}

// Модели совпадают поблочно: имена, наличие частей и все числа с допуском tol·(1 + |x|).
fn same_models(x: &Model, y: &Model, tol: f64) {
    assert_eq!(x.blocks.len(), y.blocks.len(), "число блоков");
    let parts = |b: &BlockModel| {
        [
            b.coef.is_some(),
            b.shaped.is_some(),
            b.reference.is_some(),
            b.drawn(),
            b.knee().is_some(),
        ]
    };
    for (bx, by) in x.blocks.iter().zip(&y.blocks) {
        assert_eq!(bx.name, by.name);
        assert_eq!(parts(bx), parts(by), "блок {}: части модели", bx.name);
        let (nx, ny) = (block_numbers(bx), block_numbers(by));
        assert!(
            nx.len() == ny.len() && nx.iter().zip(&ny).all(|(a, b)| close(*a, *b, tol * (1.0 + a.abs()))),
            "блок {}: {nx:?} против {ny:?}",
            bx.name
        );
    }
}

#[test]
fn volts_columns_of_old_logs_do_not_change_the_model() {
    // Решение 16.10: напряжение задаёт сам процессор по множителю и условиям — не независимый вход, путь напряжения из
    // модели убран; журналы 0.5.4 всё ещё несут столбцы <блок>.volts. Тот же журнал со столбцами напряжения (0,90 В на
    // ×38 + 0,010 В на ступень, провал 0,025 В на ×44, шум ±0,015 В; у блоков 0 и 1 во всех строках, у блока 2 только
    // в последних 60 % строк, у блока 3 столбец пуст, у спящего ядра поле пустое) и без них даёт ту же модель до
    // последнего знака; p при полной своей нагрузке — куб при любом напряжении, кривые неубывающие.
    // Ловит: напряжение в p строки (средние, соседи, раскладка), кривую напряжения в power_at или в кривой, строки без
    // напряжения как неизвестные или с другим p (у блоков 2 и 3 изменились бы пригодные строки или коэффициенты).
    let truths = square();
    let s = synth(&truths, 4000, &GAPS, 15, false);
    let cut = 1600; // первые 40 % строк — без напряжения у блока 2
    let mut dice = Dice(7);
    let mut volts: Vec<Vec<Option<f64>>> = Vec::with_capacity(s.times.len());
    for (t, row) in s.loads.iter().enumerate() {
        let mut line = Vec::with_capacity(4);
        for (b, l) in row.iter().enumerate() {
            let shown = l.ratio.map(|ratio| {
                let dip = if ratio == 44 { 0.025 } else { 0.0 };
                0.90 + 0.010 * (f64::from(ratio) - 38.0) - dip + 0.015 * dice.sign()
            });
            line.push(match b {
                0 | 1 => shown,
                2 => shown.filter(|_| t >= cut),
                _ => None,
            });
        }
        volts.push(line);
    }
    let with = fit(&[&log_from_csv(&s.csv(Some(&volts), None))], &s.die);
    let without = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    assert_eq!(with.rows, without.rows);
    same_models(&with, &without, 0.0);
    for b in &with.blocks {
        for r in [30.0_f64, 38.0, 45.0, 48.0, 60.0] {
            assert!(
                close(b.power_at(r), cube(r), 1e-12 * cube(r)),
                "блок {}, ×{r}: {} вместо {}",
                b.name,
                b.power_at(r),
                cube(r)
            );
        }
        if let Err(why) = curve_rises(b) {
            panic!("блок {}: кривая падает: {why}", b.name);
        }
    }
}

// Нижняя и верхняя медиана: при чётном числе значений медианой считается любое число между ними.
fn medians(mut v: Vec<f64>) -> (f64, f64) {
    assert!(!v.is_empty(), "нет строк для медианы");
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        (v[n / 2], v[n / 2])
    } else {
        (v[n / 2 - 1], v[n / 2])
    }
}

// Допуск — 1e-5 от величины: журнал в модели может храниться в f32 (≈ 7 значащих цифр; температуры с шестью знаками
// в синтетическом журнале теряют ~1e-7). Неверная статистика или не те строки дают расхождение на порядки больше.
fn within(x: f64, (lo, hi): (f64, f64)) -> bool {
    let tol = 1e-5 * lo.abs().max(hi.abs()).max(1.0);
    x >= lo - tol && x <= hi + tol
}

// Процентиль по времени (определение оркестратора 16.10, то же, что у линии 95 % графика): наименьшее значение v, при
// котором суммарный вес строк со значением ≤ v — не меньше доли share = (числитель, знаменатель) всего веса. Вес строки
// — миллисекунды от прошлой строки журнала. Счёт в целых — граница без ошибок округления.
fn time_share(points: &[(f64, i64)], (num, den): (i64, i64)) -> f64 {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: i64 = sorted.iter().map(|x| x.1).sum();
    let mut acc = 0;
    for (v, w) in sorted {
        acc += w;
        if acc * den >= num * total {
            return v;
        }
    }
    panic!("нет строк для процентиля")
}

// Опорные строки блока i в синтетическом журнале: пригодные, его нагрузка ≥ 0,5; (множитель, вес строки в мс).
fn solid_points(s: &Synth, i: usize) -> Vec<(f64, i64)> {
    (1..s.times.len())
        .filter(|&t| s.usable[i][t] && s.loads[t][i].busy >= 50)
        .map(|t| {
            let r = s.loads[t][i].ratio.expect("при нагрузке ≥ 0,5 множитель есть");
            (f64::from(r), s.times[t] - s.times[t - 1])
        })
        .collect()
}

#[test]
fn reference_conditions_and_curve_follow_the_true_layer() {
    // Опорные условия — медианы по пригодным строкам блока с его нагрузкой ≥ 0,5: уровень кристалла, p соседей (не 4-с
    // средняя и не быстрый вход), фактический множитель (простая медиана строк); сплошная часть кривой — от 5-го до
    // 95-го процентиля множителя по времени тех же строк (вес строки — мс от прошлой; строка 2600 весит 1000 мс).
    // Кривая — установившийся слой L3 с формой (shaped) при своей нагрузке 100 %: быстрый свой член — g(r), средние
    // своей нагрузки — p(r) = (r/45)³, входы соседей — их медианным p (дополнение 16.10: кривая — по shaped, не по
    // coef). Сверка с истинными коэффициентами внутри сплошной части и с формулой API на числах модели везде.
    // Ловит: опорные строки без порога нагрузки или по всем строкам, медиану не того (средняя вместо медианы, быстрый
    // вход соседа), кривую только по быстрому члену, без соседей или без уровня кристалла, по coef вместо shaped,
    // средние своей нагрузки не по кубу; сплошную часть как min…max, как процентиль по строкам без веса времени или с
    // другой долей.
    let truths = square();
    let s = synth(&truths, 4000, &GAPS, 15, false);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    for (i, (b, tr)) in model.blocks.iter().zip(&truths).enumerate() {
        let rows: Vec<usize> = (0..s.times.len())
            .filter(|&t| s.usable[i][t] && s.loads[t][i].busy >= 50)
            .collect();
        let rf = b
            .reference
            .as_ref()
            .unwrap_or_else(|| panic!("блок {i}: нет опорных условий"));
        let die = medians(rows.iter().map(|&t| s.die_level(i, t)).collect());
        assert!(within(rf.die, die), "блок {i}: уровень {} вне {die:?}", rf.die);
        let ratios: Vec<f64> = rows
            .iter()
            .map(|&t| f64::from(s.loads[t][i].ratio.expect("при нагрузке ≥ 0,5 множитель есть")))
            .collect();
        let solid = solid_points(&s, i);
        let (lo, hi) = (time_share(&solid, (5, 100)), time_share(&solid, (95, 100)));
        assert!(
            close(rf.lo, lo, 1e-9) && close(rf.hi, hi, 1e-9),
            "блок {i}: {}..{} вместо {lo}..{hi}",
            rf.lo,
            rf.hi
        );
        assert!(
            within(rf.median, medians(ratios)),
            "блок {i}: медиана множителя {}",
            rf.median
        );
        let mut steady = tr.a + tr.beta * rf.die;
        for &(j, fast, slow) in &tr.near {
            let want = medians(rows.iter().map(|&t| s.p[t][j]).collect());
            let got = rf
                .neighbours
                .iter()
                .find(|x| x.0 == j)
                .unwrap_or_else(|| panic!("блок {i}: нет соседа {j} в опорных условиях"))
                .1;
            assert!(within(got, want), "блок {i}, сосед {j}: p {got} вне {want:?}");
            steady += (fast + slow) * got;
        }
        // Внутри сплошной части выученная форма повторяет выпуклый свой нагрев истины K·(r/45)³ (на этих данных
        // расхождение кривой ≤ 0,015 °C — проверено независимой подгонкой); за краями g продолжается прямой, а не
        // кубом, и с кубом истины там не сверяется.
        let own: f64 = tr.own.iter().sum();
        for r in grid(rf.lo, rf.hi, 0.5) {
            let want = steady + own * cube(r);
            let got = b.curve(r).unwrap_or_else(|| panic!("блок {i}: нет кривой"));
            assert!(close(got, want, 0.05), "блок {i}, ×{r}: {got} вместо {want}");
        }
        // И внутри, и за краями кривая — по формуле API на числах слоя с формой: a + β·D + g(r) + K·p(r) + соседи по
        // их медианным p, p(r) = (r/45)³.
        let sh = b
            .shaped
            .as_ref()
            .unwrap_or_else(|| panic!("блок {i}: нет слоя с формой"));
        for r in [30.0_f64, 36.0, 40.0, 45.0, 48.0, 52.0, 60.0] {
            let mut want = sh.a + sh.beta * rf.die + sh.shape.at(r) + sh.own_total() * b.power_at(r);
            for n in &sh.neighbours {
                let p = rf.neighbours.iter().find(|x| x.0 == n.block).map_or(f64::NAN, |x| x.1);
                want += (n.fast + n.slow) * p;
            }
            let got = b.curve(r).unwrap_or_else(|| panic!("блок {i}: нет кривой"));
            assert!(
                close(got, want, 1e-9 * (1.0 + want.abs())),
                "блок {i}, ×{r}: {got} вместо {want} по формуле"
            );
            assert!(close(b.power_at(r), cube(r), 1e-12), "блок {i}: p — куб");
        }
    }
}

#[test]
fn ceiling_fan_package_and_power_columns_do_not_change_the_layer() {
    // Слова владельца: «потолок вообще не должен ебать агрессионную модель»; бриф: вентилятор — не вход, мощности
    // процессора и видеокарты — только в слое уровня кристалла (L4). Тот же журнал с постоянными и со случайными
    // потолками, корпусом, вентиляторами, мощностями и видеокартой — слой L3 (в старом виде и с формой, с самой формой
    // и её узлами), опорные условия и кривые те же.
    // Ловит: потолок как вход или подстановку потолка вместо пустого множителя спящего ядра, потолок вместо
    // фактического множителя в форме или в её узлах, вентилятор или корпус как уровень кристалла, мощность в слое
    // блока.
    let truths = square();
    let s = synth(&truths, 3000, &[(1500, 3000)], 15, false);
    let calm = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let wild = fit(&[&log_from_csv(&s.csv(None, Some(99)))], &s.die);
    for b in &calm.blocks {
        assert!(
            b.coef.is_some() && b.shaped.is_some() && b.reference.is_some(),
            "блок {}: слой и опора есть",
            b.name
        );
    }
    same_models(&calm, &wild, 1e-9);
}

#[test]
fn coefficients_need_min_rows_and_the_curve_needs_ten_minutes() {
    // Два соседних блока, без перерывов: пригодных строк — все, кроме 64 строк разогрева и последней. 304 строки → 239
    // пригодных: коэффициентов нет; 305 → 240: есть, кривая считается, но 240 · 0,25 с = 1 мин < 10 мин — не рисуется.
    // Ловит: порог «больше» вместо «не меньше» MIN_ROWS, порог по всем строкам, а не по пригодным; рисование кривой по
    // минуте данных.
    let two = vec![
        truth(20.0, 0.5, [5.0, 0.0, 0.0, 5.0, 0.0, 0.0, 5.0], &[(1, 1.0, 1.0)]),
        truth(22.0, 0.4, [6.0, 0.0, 6.0, 0.0, 0.0, 0.0, 0.0], &[(0, 0.5, 1.5)]),
    ];
    let short = synth(&two, 304, &[], 0, false);
    let model = fit(&[&log_from_csv(&short.csv(None, None))], &short.die);
    for b in &model.blocks {
        assert_eq!(b.usable, 239, "блок {}", b.name);
        assert!(
            b.coef.is_none() && b.curve(45.0).is_none() && !b.drawn(),
            "блок {}",
            b.name
        );
    }
    let enough = synth(&two, 305, &[], 0, false);
    let model = fit(&[&log_from_csv(&enough.csv(None, None))], &enough.die);
    for b in &model.blocks {
        assert_eq!(b.usable, 240, "блок {}", b.name);
        assert!(
            b.coef.is_some(),
            "блок {}: {MIN_ROWS} строк — коэффициенты есть",
            b.name
        );
        assert!(close(b.minutes, 1.0, 1e-9), "блок {}: {} мин", b.name, b.minutes);
        assert!(b.curve(45.0).is_some(), "блок {}: кривая считается", b.name);
        assert!(!b.drawn(), "блок {}: минута данных — кривая не рисуется", b.name);
    }
}

// ─── 8. Монотонные кривые ────────────────────────────────────────────────────────────────────────────────────────

// Кривая блока по сетке ×8…×80 (далеко за наблюдённым ×38…×48) неубывающая.
fn curve_rises(b: &BlockModel) -> Result<(), String> {
    let shown: Result<Vec<(f64, f64)>, String> = grid(8.0, 80.0, 0.25)
        .into_iter()
        .map(|r| b.curve(r).map(|v| (r, v)).ok_or_else(|| format!("нет кривой на ×{r}")))
        .collect();
    rising(&shown?, 1e-9)
}

// Неограниченный МНК (своя запись: нормальные уравнения со свободным членом) по пригодным строкам блока i: [a, β, свои
// 7, соседи по 2]. Предпосылка теста ниже — без ограничения коэффициенты были бы отрицательными.
fn unconstrained(s: &Synth, truths: &[Truth], i: usize) -> Vec<f64> {
    let (x, y): (Vec<Vec<f64>>, Vec<f64>) = (0..s.times.len())
        .filter(|&t| s.usable[i][t])
        .map(|t| {
            let mut row = vec![1.0, s.die_level(i, t)];
            row.extend(s.inputs(&truths[i].near, i, t));
            (row, s.temps[t][i].parse::<f64>().expect("число"))
        })
        .unzip();
    let (gram, rhs) = normal(&x, &y);
    solve(&gram, &rhs).expect("система МНК решается")
}

#[test]
fn curve_is_non_decreasing_even_when_the_data_say_heat_cools() {
    // Данные, где без ограничения выходят отрицательные коэффициенты: у блока 0 16-с средняя своей нагрузки с весом −6
    // (охлаждение долгой нагрузкой), у блока 1 своя нагрузка охлаждает (−4), у блока 1 быстрый вход соседа 3 — −1,5,
    // у блока 3 β = −0,2. Бриф: β, K, M ≥ 0 — «тепло не охлаждает», и поэтому кривая неубывающая по построению;
    // дополнение 16.10: то же у слоя с формой, и все веса формы w ≥ 0.
    // Ловит: обычные МНК без ограничений (в любом из двух видов слоя), ограничение только суммы своих коэффициентов,
    // отрицательный β, кривую, падающую за пределами наблюдённых множителей.
    let truths = vec![
        truth(
            25.0,
            0.5,
            [10.0, 0.0, 0.0, 0.0, 0.0, 0.0, -6.0],
            &[(1, 1.0, 1.0), (2, 0.5, 0.5)],
        ),
        truth(
            30.0,
            0.5,
            [-4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            &[(0, 0.5, 0.5), (3, -1.5, 0.0)],
        ),
        truth(
            20.0,
            0.6,
            [5.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0],
            &[(0, 1.0, 0.0), (3, 0.0, 1.0)],
        ),
        truth(
            40.0,
            -0.2,
            [4.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0],
            &[(1, 1.0, 1.0), (2, 1.0, 1.0)],
        ),
    ];
    let s = synth(&truths, 3000, &[(1500, 2500)], 15, false);
    // Предпосылка: без ограничения — отрицательные (данные точные, МНК возвращает истину).
    let free0 = unconstrained(&s, &truths, 0);
    let free1 = unconstrained(&s, &truths, 1);
    assert!(free0[8] < -1.0, "блок 0, 16-с средняя без ограничения: {}", free0[8]);
    assert!(free1[2] < -1.0, "блок 1, быстрый вход без ограничения: {}", free1[2]);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    for (i, b) in model.blocks.iter().enumerate() {
        // Оба вида слоя (старый — для «°C на ватт», с формой — для кривой) с неотрицательными коэффициентами.
        for (what, layer) in ["coef", "shaped"].into_iter().zip([b.coef.as_ref(), b.shaped.as_ref()]) {
            let c = layer.unwrap_or_else(|| panic!("блок {i}: нет слоя {what}"));
            assert!(c.beta >= 0.0, "блок {i}, {what}: β {}", c.beta);
            assert!(c.own.iter().all(|&k| k >= 0.0), "блок {i}, {what}: свои {:?}", c.own);
            assert!(
                c.shape.weights.iter().all(|&w| w >= 0.0),
                "блок {i}, {what}: веса формы {:?}",
                c.shape.weights
            );
            assert!(
                c.neighbours.iter().all(|n| n.fast >= 0.0 && n.slow >= 0.0),
                "блок {i}, {what}: соседи {:?}",
                c.neighbours.iter().map(|n| (n.fast, n.slow)).collect::<Vec<_>>()
            );
        }
        if let Err(why) = curve_rises(b) {
            panic!("блок {i}: кривая падает: {why}");
        }
    }
}

// ─── 9. Раскладка мощности по блокам, «°C на ватт», кривые при одинаковой работе ─────────────────────────────────
// Источник — изменение брифа 16.10 (слова оркестратора в сообщении исполнителя) и API: P(t) = c0 + Σ c_j·p_j(t),
// c_j ≥ 0, p строки (у мощности нет тепловой задержки), p кластера E ×4; power_cores, если он есть, иначе power_cpu;
// блоки, нагруженные всегда вместе, определяются только суммой — без выдуманного деления; «°C на ватт» =
// K_own,total / c_j; кривые при одинаковой работе T_i(r) = base_i + R_i·c_ref·p(r), c_ref — медиана ватт блоков того
// же типа (с дополнения 16.10 вместо K·p — свой нагрев слоя с формой heat(r) = g(r) + K·p(r), пересчитанный к c_ref);
// без данных о мощности — прежние кривые.

// Мощности строк для журнала: power_cpu и столбец power_cores (None — столбца нет); None в строке — поле пустое.
struct Watts<'a> {
    cpu: &'a [Option<f64>],
    cores: Option<&'a [Option<f64>]>,
}

// Мощность строк по известной раскладке: c0 + Σ w_j·p_j(t) — p самой строки; w_j — ватт на единицу p блока (у кластера
// E — за все четыре ядра).
fn split_power(s: &Synth, c0: f64, w: &[f64]) -> Vec<Option<f64>> {
    s.p.iter()
        .map(|row| Some(c0 + row.iter().zip(w).map(|(p, wj)| p * wj).sum::<f64>()))
        .collect()
}

// p при полной своей нагрузке без напряжения: (r/45)³.
fn cube(r: f64) -> f64 {
    (r / 45.0).powi(3)
}

// Ватты раскладки по блокам (кластер E — блок 3) и что из них следует для блоков 0–3 кристалла square():
// «°C на ватт» = сумма своих коэффициентов / ватты: 18/9 = 2, 16/8 = 2, 20/5 = 4, 21/14 = 1,5; медиана ватт P-блоков
// (9, 8, 5) — 8, у единственного E-блока — его 14.
const WATTS: [f64; 4] = [9.0, 8.0, 5.0, 14.0];
const OHMS: [f64; 4] = [2.0, 2.0, 4.0, 1.5];
const SAME: [f64; 4] = [8.0, 8.0, 8.0, 14.0];

#[test]
fn power_split_recovers_watts_per_block_from_the_cores_column() {
    // Мощность ядер построена из p строк: 6 Вт + 9·p₀ + 8·p₁ + 5·p₂ + 14·p₃, блок 3 — кластер E. В power_cpu — другая
    // зависимость (26 Вт + 18·p₀ + …): при обоих столбцах берётся power_cores. В последней строке у блока 2 p не
    // известно — строка в раскладку не идёт.
    // Ловит: power_cpu при наличии power_cores; сглаженный вход (быстрый или средние) вместо p строки; ватты кластера E
    // без множителя 4 (3,5 вместо 14) или с ним дважды (56); строку с неизвестным p (как p = 0) в счёте; раскладку без
    // свободного члена; признак «не определено» у независимо нагруженных блоков.
    let truths = square();
    let mut s = synth(&truths, 4000, &GAPS, 15, false);
    s.die.e[3] = true;
    let cores = split_power(&s, 6.0, &WATTS);
    let cpu = split_power(&s, 26.0, &[18.0, 8.0, 5.0, 14.0]);
    s.loads[3999][2] = Load { busy: 60, ratio: None };
    let watts = Watts {
        cpu: &cpu,
        cores: Some(&cores),
    };
    let model = fit(&[&log_from_csv(&s.write(None, None, Some(&watts)))], &s.die);
    let split = model.power.as_ref().expect("мощность есть — раскладка есть");
    assert_eq!(split.column, "power_cores");
    assert_eq!(split.rows, 3999, "строка с неизвестным p не в счёте");
    assert!(close(split.c0, 6.0, 0.05), "c0 = {}", split.c0);
    assert!(split.rmse < 0.01, "ошибка на отложенных строках {} Вт", split.rmse);
    for (i, (b, &w)) in model.blocks.iter().zip(&WATTS).enumerate() {
        let bp = b.power.as_ref().unwrap_or_else(|| panic!("блок {i}: нет ватт"));
        assert!(close(bp.watts, w, 0.01), "блок {i}: {} Вт вместо {w}", bp.watts);
        assert!(
            close(bp.lo, w, 0.01) && close(bp.hi, w, 0.01),
            "блок {i}: по частям {}..{} вместо {w}",
            bp.lo,
            bp.hi
        );
        assert!(bp.determined, "блок {i}: нагрузка независимая — ватты определены");
        assert!(bp.collinear < 0.5, "блок {i}: связь с другими {}", bp.collinear);
    }
}

#[test]
fn power_split_uses_one_column_for_all_rows() {
    // Один столбец на все строки: если power_cores есть хоть где-то — только строки с ним; без него — power_cpu.
    // power_cores — во второй половине строк, power_cpu — во всех и с другой зависимостью.
    // Ловит: смесь столбцов по строкам (первая половина из power_cpu сместила бы ватты блока 0 и c0), отказ от
    // power_cpu, когда power_cores нет вовсе.
    let truths = square();
    let s = synth(&truths, 4000, &GAPS, 15, false);
    let cpu_w = [18.0, 8.0, 5.0, 14.0];
    let half: Vec<Option<f64>> = split_power(&s, 6.0, &WATTS)
        .into_iter()
        .enumerate()
        .map(|(t, v)| v.filter(|_| t >= 2000))
        .collect();
    let cpu = split_power(&s, 26.0, &cpu_w);
    let mixed = Watts {
        cpu: &cpu,
        cores: Some(&half),
    };
    let model = fit(&[&log_from_csv(&s.write(None, None, Some(&mixed)))], &s.die);
    let split = model.power.as_ref().expect("раскладка есть");
    assert_eq!((split.column, split.rows), ("power_cores", 2000));
    assert!(close(split.c0, 6.0, 0.05), "c0 = {}", split.c0);
    for (i, (b, &w)) in model.blocks.iter().zip(&WATTS).enumerate() {
        let got = b.power.as_ref().map(|x| x.watts);
        assert!(got.is_some_and(|g| close(g, w, 0.01)), "блок {i}: {got:?} вместо {w}");
    }
    let only_cpu = Watts { cpu: &cpu, cores: None };
    let model = fit(&[&log_from_csv(&s.write(None, None, Some(&only_cpu)))], &s.die);
    let split = model.power.as_ref().expect("раскладка есть");
    assert_eq!((split.column, split.rows), ("power_cpu", 4000));
    assert!(close(split.c0, 26.0, 0.05), "c0 = {}", split.c0);
    for (i, (b, &w)) in model.blocks.iter().zip(&cpu_w).enumerate() {
        let got = b.power.as_ref().map(|x| x.watts);
        assert!(got.is_some_and(|g| close(g, w, 0.01)), "блок {i}: {got:?} вместо {w}");
    }
}

#[test]
fn resistance_is_own_gain_per_watt_and_same_work_curves_use_the_median_watts() {
    // «°C на ватт» = K_own,total / c_j старого вида слоя (coef): 2; 2; 4; 1,5. Кривая при одинаковой работе (дополнение
    // 16.10): curve(r) + (c_ref/c_j − 1)·heat(r), heat(r) = g(r) + K·p(r) слоя с формой — свой нагрев блока,
    // пересчитанный к c_ref, медиане ватт блоков того же типа (P: 8; E: 14). Блоки 1 и 3 — сами на медиане: их кривая
    // не меняется; блок 0 (9 Вт): сдвиг −1/9 своего нагрева (по истине R·8 − 18 = −2 на единицу p); блок 2 (5 Вт): +0,6
    // своего нагрева (+12 на единицу p). Блоки 0 и 1 с одинаковым R при одинаковой работе греются одинаково сверх своей
    // базы.
    // Ловит: R как ватты / K или K / (ватты/4) у кластера E; медиану по всем блокам без разделения на P и E; кривую без
    // вычитания своего нагрева или по своим ваттам вместо медианы; свой нагрев по старому виду (K·p) вместо формы;
    // значение вне своего интервала; интервал, не сходящийся на точных данных.
    let truths = square();
    let mut s = synth(&truths, 4000, &GAPS, 15, false);
    s.die.e[3] = true;
    let cores = split_power(&s, 6.0, &WATTS);
    let none = vec![None; 4000];
    let watts = Watts {
        cpu: &none,
        cores: Some(&cores),
    };
    let model = fit(&[&log_from_csv(&s.write(None, None, Some(&watts)))], &s.die);
    let mut rise = Vec::new();
    for (i, b) in model.blocks.iter().enumerate() {
        let rs = b
            .resistance
            .as_ref()
            .unwrap_or_else(|| panic!("блок {i}: нет «°C на ватт»"));
        assert!(
            close(rs.value, OHMS[i], 1e-3 * OHMS[i]),
            "блок {i}: {} °C/Вт вместо {}",
            rs.value,
            OHMS[i]
        );
        assert!(
            rs.lo <= rs.value && rs.value <= rs.hi,
            "блок {i}: {} вне своего интервала {}..{}",
            rs.value,
            rs.lo,
            rs.hi
        );
        assert!(
            rs.hi - rs.lo < 0.01 * rs.value,
            "блок {i}: интервал {}..{} на точных данных",
            rs.lo,
            rs.hi
        );
        let same = b.same_work.unwrap_or_else(|| panic!("блок {i}: нет опорных ватт"));
        assert!(
            close(same, SAME[i], 0.01),
            "блок {i}: опорные ватты {same} вместо {}",
            SAME[i]
        );
        let sh = b.shaped.as_ref().expect("слой с формой");
        let own_watts = b.power.as_ref().expect("ватты блока").watts;
        for r in [38.0_f64, 45.0, 48.0] {
            let curve = b.curve(r).expect("кривая");
            let shown = b.shown_curve(r).expect("кривая при одинаковой работе");
            // По формуле дополнения на числах самой модели: свой нагрев heat(r) = g(r) + K·p(r) слоя с формой,
            // масштабированный к опорным ваттам своего типа.
            let heat = sh.shape.at(r) + sh.own_total() * b.power_at(r);
            let base = curve - heat;
            let want = curve + (same / own_watts - 1.0) * heat;
            assert!(
                close(shown, want, 1e-6 * want.abs()),
                "блок {i}, ×{r}: {shown} вместо {want}"
            );
            // По истине: сдвиг от кривой L3 — (c_ref/c_j − 1)·K·p(r) = (R·c_ref − K)·p(r); внутри наблюдённых ×38…×48
            // форма повторяет свой нагрев истины (на этих данных расхождение сдвига ≤ 0,01 °C — независимая подгонка).
            let shift = (OHMS[i] * SAME[i] - truths[i].own.iter().sum::<f64>()) * cube(r);
            assert!(
                close(shown - curve, shift, 0.05),
                "блок {i}, ×{r}: сдвиг {} вместо {shift}",
                shown - curve
            );
            if i < 2 {
                rise.push((r, shown - base));
            }
        }
    }
    // Одинаковое R — одинаковый нагрев сверх базы при одинаковой работе (блоки 0 и 1).
    for (&(r, a), &(_, b)) in rise.iter().take(3).zip(rise.iter().skip(3)) {
        assert!(close(a, b, 0.05), "×{r}: блок 0 +{a}, блок 1 +{b}");
    }
}

#[test]
fn blocks_always_loaded_together_get_no_invented_split() {
    // Блоки 0 и 1 нагружены всегда одинаково, блок 3 всё время спит (p = 0); мощность 6 + 9·p₀ + 8·p₁ + 5·p₂. Из
    // журнала определима только сумма 0 и 1 (17 Вт); бриф: «say so plainly, do not invent a split». Ловит: «определено»
    // у неразделимых блоков (решатель делит поровну в каждой части — устойчиво и выдумано); пару, не названную
    // напарником; «°C на ватт», опорные ватты или изменённую кривую у неопределённого блока; потерю суммы; «определено»
    // у блока, который ни разу не работал; падение раскладки на нулевом столбце.
    let truths = square();
    let mut together = loads(20_261_016, 4, 4000, 15);
    for row in &mut together {
        row[1] = row[0];
        row[3] = Load { busy: 1, ratio: None };
    }
    let s = synth_from(&truths, together, times(4000, &GAPS), false);
    let cores = split_power(&s, 6.0, &WATTS);
    let none = vec![None; 4000];
    let watts = Watts {
        cpu: &none,
        cores: Some(&cores),
    };
    let model = fit(&[&log_from_csv(&s.write(None, None, Some(&watts)))], &s.die);
    assert!(model.power.is_some(), "раскладка есть");
    let power = |i: usize| {
        model.blocks[i]
            .power
            .as_ref()
            .unwrap_or_else(|| panic!("блок {i}: нет ватт"))
    };
    for (i, other) in [(0, 1), (1, 0)] {
        let bp = power(i);
        let b = &model.blocks[i];
        assert!(!bp.determined, "блок {i}: делится только сумма — не определено");
        assert!(bp.collinear > 0.9, "блок {i}: связь с другими {}", bp.collinear);
        assert!(
            bp.partner.is_some_and(|(j, c)| j == other && c > 0.99),
            "блок {i}: напарник {:?}, ожидался {other}",
            bp.partner
        );
        assert!(
            b.resistance.is_none() && b.same_work.is_none(),
            "блок {i}: без ватт нет «°C на ватт»"
        );
        for r in [40.0, 45.0, 50.0] {
            let (curve, shown) = (b.curve(r), b.shown_curve(r));
            assert!(
                curve.is_some() && shown.is_some_and(|v| curve.is_some_and(|c| close(v, c, 1e-9))),
                "блок {i}, ×{r}: {shown:?} вместо прежней {curve:?}"
            );
        }
    }
    assert!(
        close(power(0).watts + power(1).watts, 17.0, 0.05),
        "сумма {} + {}",
        power(0).watts,
        power(1).watts
    );
    let asleep = power(3);
    assert!(
        !asleep.determined && asleep.collinear >= 0.9,
        "блок 3 не работал: {}",
        asleep.collinear
    );
    assert!(model.blocks[3].resistance.is_none());
    let free = power(2);
    assert!(
        free.determined && close(free.watts, 5.0, 0.01),
        "блок 2: {} Вт",
        free.watts
    );
    assert!(
        model.blocks[2].resistance.is_some(),
        "блок 2 определён — «°C на ватт» есть"
    );
}

#[test]
fn without_power_data_there_is_no_split_and_the_old_curves_stay() {
    // Бриф: «When it cannot be computed (no power data…) show no number… Until power data exist, keep the current
    // curves». API: раскладки нет при числе строк меньше 2·4·(n + 1) — 40 для четырёх блоков.
    // Ловит: ватты и «°C на ватт» из пустого столбца (нули), изменённую кривую без данных, раскладку по 39 строкам или
    // отказ при 40.
    let truths = square();
    let s = synth(&truths, 3000, &[(1500, 3000)], 15, false);
    let none = vec![None; 3000];
    let empty = Watts {
        cpu: &none,
        cores: None,
    };
    let model = fit(&[&log_from_csv(&s.write(None, None, Some(&empty)))], &s.die);
    assert!(model.power.is_none(), "мощности нет — раскладки нет");
    for (i, b) in model.blocks.iter().enumerate() {
        assert!(
            b.power.is_none() && b.resistance.is_none() && b.same_work.is_none(),
            "блок {i}"
        );
        for r in [40.0, 45.0, 50.0] {
            let (curve, shown) = (b.curve(r), b.shown_curve(r));
            assert!(
                curve.is_some() && shown.is_some_and(|v| curve.is_some_and(|c| close(v, c, 1e-9))),
                "блок {i}, ×{r}: {shown:?} вместо прежней {curve:?}"
            );
        }
    }
    let full = split_power(&s, 6.0, &WATTS);
    for (count, exists) in [(39, false), (40, true)] {
        let some: Vec<Option<f64>> = full
            .iter()
            .enumerate()
            .map(|(t, v)| v.filter(|_| (1000..1000 + count).contains(&t)))
            .collect();
        let few = Watts {
            cpu: &some,
            cores: None,
        };
        let model = fit(&[&log_from_csv(&s.write(None, None, Some(&few)))], &s.die);
        assert_eq!(model.power.is_some(), exists, "{count} строк с мощностью");
        if let Some(split) = &model.power {
            assert_eq!(split.rows, count);
        }
    }
}

// ─── 10. Сплошная часть кривой: процентили по времени ─────────────────────────────────────────────────────────────

// Нагрузки кристалла 2 × 2 без сна; у блока 0 множители ×40…×44 (из ×38…×48 генератора: 40 + r mod 6).
fn narrow_loads() -> Vec<Vec<Load>> {
    let mut out = loads(20_261_016, 4, 4000, 0);
    for row in &mut out {
        row[0].ratio = row[0].ratio.map(|r| 40 + r % 6);
    }
    out
}

// Сплошная часть кривой блока 0: по модели (got), по определению (want) и опорные строки (множитель, вес).
struct Solid {
    got: (f64, f64),
    want: (f64, f64),
    points: Vec<(f64, i64)>,
}

fn solid_range(together: Vec<Vec<Load>>) -> Solid {
    solid_range_at(together, &[])
}

// То же со своими промежутками между строками (строка, промежуток перед ней в мс).
fn solid_range_at(together: Vec<Vec<Load>>, gaps: &[(usize, i64)]) -> Solid {
    let s = synth_from(&square(), together, times(4000, gaps), false);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let rf = model.blocks[0].reference.as_ref().expect("опорные условия");
    let points = solid_points(&s, 0);
    let want = (time_share(&points, (5, 100)), time_share(&points, (95, 100)));
    Solid {
        got: (rf.lo, rf.hi),
        want,
        points,
    }
}

#[test]
fn solid_range_ignores_a_few_outlier_rows_but_follows_a_large_share() {
    // Тысячи опорных строк блока 0 на ×40…×44; в одном журнале 8 строк (2 с из ~8 мин) на ×19, в другом — каждая 10-я
    // строка на ×19 (больше 5 % времени). Определение: 5-й и 95-й процентили по времени. Ловит: min…max (×19 растянул
    // бы сплошную часть из-за 2 с), долю не по времени, порог «больше» вместо «не меньше».
    let base = solid_range(narrow_loads());
    let mut few = narrow_loads();
    for row in few.iter_mut().skip(1000).take(8) {
        row[0] = Load {
            busy: 80,
            ratio: Some(19),
        };
    }
    let few = solid_range(few);
    let mut many = narrow_loads();
    for row in many.iter_mut().skip(100).step_by(10) {
        row[0] = Load {
            busy: 80,
            ratio: Some(19),
        };
    }
    let many = solid_range(many);
    for (what, x) in [("без выбросов", &base), ("8 строк ×19", &few), ("10 % ×19", &many)] {
        assert!(
            close(x.got.0, x.want.0, 1e-9) && close(x.got.1, x.want.1, 1e-9),
            "{what}: {:?} вместо {:?}",
            x.got,
            x.want
        );
    }
    // Предпосылки: 8 строк ×19 — в опорных строках, их время меньше 5 %; в третьем журнале — больше 5 %.
    let share = |points: &[(f64, i64)]| {
        let low: i64 = points.iter().filter(|x| x.0 < 20.0).map(|x| x.1).sum();
        let all: i64 = points.iter().map(|x| x.1).sum();
        (low, all)
    };
    let (low, all) = share(&few.points);
    assert!(low == 8 * 250 && low * 20 < all, "8 строк ×19: {low} мс из {all}");
    let (low, all) = share(&many.points);
    assert!(low * 20 > all, "каждая 10-я строка ×19: {low} мс из {all}");
    assert!(
        close(few.got.0, base.got.0, 0.0) && close(few.got.1, base.got.1, 0.0),
        "выбросы сдвинули сплошную часть: {:?} против {:?}",
        few.got,
        base.got
    );
    assert!(
        close(many.got.0, 19.0, 0.0),
        "больше 5 % времени на ×19 — сплошная часть от ×19: {:?}",
        many.got
    );
}

#[test]
fn solid_range_weighs_rows_by_their_time() {
    // 80 строк блока 0 на ×19, каждой предшествует промежуток ровно 1 с (не перерыв — средние не начинаются заново):
    // по числу строк это меньше 5 % опорных строк, по времени — больше. Определение — доля времени (вес строки — мс от
    // прошлой), так что сплошная часть начинается с ×19. Ловит: процентиль по числу строк (дал бы ×40), вес строки —
    // промежуток до следующей строки, а не до прошлой (×19 весили бы по 250 мс).
    let mut long = narrow_loads();
    let marked: Vec<usize> = (0..80).map(|k| 200 + 40 * k).collect();
    for &t in &marked {
        long[t][0] = Load {
            busy: 80,
            ratio: Some(19),
        };
    }
    let gaps: Vec<(usize, i64)> = marked.iter().map(|&t| (t, 1000)).collect();
    let long = solid_range_at(long, &gaps);
    let low: i64 = long.points.iter().filter(|x| x.0 < 20.0).map(|x| x.1).sum();
    let all: i64 = long.points.iter().map(|x| x.1).sum();
    let rows = long.points.len();
    assert!(
        low == 80 * 1000 && low * 20 > all && 80 * 20 < rows,
        "предпосылка: ×19 — {low} мс из {all}, 80 строк из {rows}"
    );
    assert!(
        close(long.got.0, long.want.0, 1e-9) && close(long.got.1, long.want.1, 1e-9),
        "{:?} вместо {:?}",
        long.got,
        long.want
    );
    assert!(
        close(long.got.0, 19.0, 0.0),
        "больше 5 % времени на ×19: {:?}",
        long.got
    );
}

#[test]
fn solid_range_takes_the_value_where_the_share_is_reached_exactly() {
    // Ручной журнал, 331 строка через 250 мс; у блока 0 опорные (нагрузка 0,8) только строки 310…329: одна на ×19, 18 на
    // ×40, одна на ×50 — 20 строк по 250 мс. На ×19 набирается ровно 5 % времени, на ×40 — ровно 95 %. Определение —
    // «не меньше доли»: сплошная часть ×19…×40, простая медиана ×40. Ловит: «больше доли» (дало бы ×40…×50), процентиль с
    // интерполяцией между строками (значения не из строк).
    let rows: Vec<(i64, Vec<Cell>)> = (0..331)
        .map(|k: usize| {
            let mut cells: Vec<Cell> = (0..4).map(|b| usual(k, b)).collect();
            let (ratio, busy) = match k {
                310 => ("19", "0.80"),
                311..=328 => ("40", "0.80"),
                329 => ("50", "0.80"),
                _ => ("42", "0.30"),
            };
            cells[0].0 = ratio.to_string();
            cells[0].1 = busy.to_string();
            (START_MS + 250 * i64::try_from(k).expect("строк мало"), cells)
        })
        .collect();
    let die = Die {
        names: names(&["0", "1", "2", "3"]),
        e: vec![false; 4],
        neighbours: vec![vec![1, 2], vec![0, 3], vec![0, 3], vec![1, 2]],
    };
    let model = fit(&[&hand_log(&die.names, &rows)], &die);
    let rf = model.blocks[0].reference.as_ref().expect("опорные условия");
    assert!(
        close(rf.lo, 19.0, 0.0) && close(rf.hi, 40.0, 0.0),
        "сплошная часть ×{}…×{} вместо ×19…×40",
        rf.lo,
        rf.hi
    );
    assert!(close(rf.median, 40.0, 1e-9), "медиана ×{}", rf.median);
}

// ─── 11. Окно данных: восемь часов до новейшей строки ─────────────────────────────────────────────────────────────

const HOUR: i64 = 3_600_000;

// Журнал блоков 0–3 из строк с этим временем (мс от полуночи 15.10), обычные поля.
fn timed_log(ms: &[i64]) -> Log {
    let blocks = names(&["0", "1", "2", "3"]);
    let rows: Vec<(i64, Vec<Cell>)> = ms
        .iter()
        .enumerate()
        .map(|(k, &t)| (t, (0..4).map(|b| usual(k, b)).collect()))
        .collect();
    hand_log(&blocks, &rows)
}

// Время строки в масштабе журнала, как его читает окно (samples::row_time): мс от 1970-01-01 по местным часам.
fn scale(ms: i64) -> i64 {
    row_time(&stamp(ms)).expect("время строки").ms
}

// Строка для push: у каждого из четырёх блоков ×45, нагрузка 0,8, 60 °C, без напряжения; мощностей нет.
fn push_row(log: &mut Log, ms: i64) {
    log.push(
        scale(ms),
        std::iter::repeat_n([Some(45.0), Some(0.8), Some(60.0), None], 4),
        [None; 3],
    );
}

#[test]
fn trim_drops_rows_older_than_eight_hours_before_the_newest_row_of_all_logs() {
    // Журнал A: 01:00, 02:59:59.999, 03:00, 02:30 (часы назад), 04:00; журнал B: 05:00, 09:00, 11:00. Новейшая строка —
    // 11:00 в B, граница — 03:00: в A остаются 03:00 (ровно на границе) и 04:00, B не меняется.
    // Ловит: границу по своему файлу (A по своей 04:00 не потерял бы ничего), строку ровно на границе выброшенной
    // («>» вместо «≥»), строку на 1 мс раньше оставленной, срез только с начала файла (02:30 после 03:00 осталась бы),
    // окно не 8 ч, изменённый второй журнал.
    assert_eq!(KEEP_MS, 8 * HOUR);
    let mut a = timed_log(&[HOUR, 3 * HOUR - 1, 3 * HOUR, 2 * HOUR + HOUR / 2, 4 * HOUR]);
    let mut b = timed_log(&[5 * HOUR, 9 * HOUR, 11 * HOUR]);
    assert_eq!(b.newest(), Some(scale(11 * HOUR)));
    let cut = trim(&mut [&mut a, &mut b]);
    assert_eq!(cut, Some(scale(3 * HOUR)));
    assert_eq!((a.rows(), a.newest()), (2, Some(scale(4 * HOUR))));
    assert_eq!((b.rows(), b.newest()), (3, Some(scale(11 * HOUR))));
}

#[test]
fn trim_without_rows_changes_nothing_and_an_empty_log_does_not_decide() {
    // Ловит: границу из пустого журнала (ноль или минимум времени), изменение журналов, когда строк нет нигде.
    let mut a = timed_log(&[]);
    let mut b = timed_log(&[]);
    assert_eq!(a.newest(), None);
    assert_eq!(trim(&mut [&mut a, &mut b]), None);
    let mut c = timed_log(&[HOUR, 2 * HOUR]);
    assert_eq!(trim(&mut [&mut a, &mut c]), Some(scale(2 * HOUR) - KEEP_MS));
    assert_eq!((a.rows(), c.rows()), (0, 2), "всё в пределах 8 ч — ничего не выброшено");
}

#[test]
fn admit_from_rejects_later_older_rows_and_leaves_kept_ones() {
    // API: admit_from меняет только приём следующих строк — push молча пропускает строки старше границы, граница только
    // растёт (меньшая потом не действует); уже принятые строки не трогаются. Ловит: admit_from, выбрасывающий принятое;
    // push, принимающий строку старше границы или отвергающий строку ровно на ней; границу, опущенную вторым вызовом.
    let mut a = timed_log(&[HOUR, 2 * HOUR, 3 * HOUR]);
    a.admit_from(scale(5 * HOUR));
    assert_eq!(
        (a.rows(), a.newest()),
        (3, Some(scale(3 * HOUR))),
        "admit_from не трогает принятое"
    );
    push_row(&mut a, 4 * HOUR);
    assert_eq!(a.rows(), 3, "строка старше границы не принята");
    push_row(&mut a, 5 * HOUR);
    assert_eq!(
        (a.rows(), a.newest()),
        (4, Some(scale(5 * HOUR))),
        "строка ровно на границе принята"
    );
    a.admit_from(scale(2 * HOUR));
    push_row(&mut a, 4 * HOUR + HOUR / 2);
    assert_eq!(a.rows(), 4, "меньшая граница не действует: 04:30 всё ещё старше 05:00");
    push_row(&mut a, 6 * HOUR);
    assert_eq!((a.rows(), a.newest()), (5, Some(scale(6 * HOUR))));
}

#[test]
fn keep_from_drops_older_kept_rows_and_admits_from_the_same_time() {
    // API: keep_from(at) = admit_from(at) и выбросить принятые строки старше at; строка ровно на at остаётся;
    // выброшенное меньшая граница не возвращает. Ловит: keep_from, выбросивший строку на границе или оставивший строку
    // старше её; keep_from без admit_from (строка старше границы, пришедшая потом, принята бы).
    let mut b = timed_log(&[HOUR, 2 * HOUR, 3 * HOUR]);
    b.keep_from(scale(2 * HOUR));
    assert_eq!(
        (b.rows(), b.newest()),
        (2, Some(scale(3 * HOUR))),
        "выброшена только 01:00"
    );
    b.keep_from(scale(HOUR));
    assert_eq!(b.rows(), 2, "выброшенное не возвращается");
    push_row(&mut b, HOUR + HOUR / 2);
    assert_eq!(b.rows(), 2, "строка старше границы keep_from не принята");
    push_row(&mut b, 2 * HOUR);
    assert_eq!(b.rows(), 3, "строка ровно на границе принята");
}

// Числа слоя L3 блока: a, β, свои, соседи (быстрый, 4-с).
fn coef_numbers(c: &Coef) -> Vec<f64> {
    let mut v = vec![c.a, c.beta];
    v.extend(c.own);
    v.extend(c.neighbours.iter().flat_map(|n| [n.fast, n.slow]));
    v
}

#[test]
fn fit_on_trimmed_logs_equals_fit_on_the_recent_rows_only() {
    // Синтетический журнал из 4000 строк с 10:00:00; второй журнал — 10 строк, последняя ровно через 8 ч после строки
    // 2000 первого. trim оставляет в первом строки 2000…3999; модель по ним должна совпасть с моделью по журналу,
    // собранному только из этих строк (первая оставшаяся строка — начало файла).
    // Ловит: выброшенные строки, всё ещё влияющие на модель (средние, тянущиеся от них; разогрев от выброшенного
    // начала; счёт строк и минут; lo, hi и узлы формы по выброшенным строкам); срез, сбивший порядок или поля
    // оставшихся строк.
    let truths = square();
    let s = synth(&truths, 4000, &GAPS, 15, false);
    let text = s.csv(None, None);
    let mut lines = text.lines();
    let head = lines.next().expect("заголовок");
    let rows: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    let recent = format!("{head}\n{}\n", rows[2000..].join("\n"));
    let edge = s.times[2000];
    let late: Vec<i64> = (0..10).map(|k| edge + KEEP_MS - 250 * (9 - k)).collect();
    let mut first = log_from_csv(&text);
    let mut second = timed_log(&late);
    assert_eq!(trim(&mut [&mut first, &mut second]), Some(scale(edge)));
    assert_eq!((first.rows(), second.rows()), (2000, 10));
    let trimmed = fit(&[&first, &second], &s.die);
    let fresh = fit(&[&log_from_csv(&recent), &timed_log(&late)], &s.die);
    assert_eq!(trimmed.rows, fresh.rows);
    for (i, (x, y)) in trimmed.blocks.iter().zip(&fresh.blocks).enumerate() {
        assert_eq!(x.usable, y.usable, "блок {i}: пригодных строк");
        assert!(
            close(x.minutes, y.minutes, 1e-9),
            "блок {i}: минуты {} против {}",
            x.minutes,
            y.minutes
        );
        let (cx, cy) = (
            x.coef.as_ref().expect("коэффициенты"),
            y.coef.as_ref().expect("коэффициенты"),
        );
        let (fx, fy) = (coef_numbers(cx), coef_numbers(cy));
        assert!(
            fx.iter().zip(&fy).all(|(a, b)| close(*a, *b, 1e-9 * (1.0 + a.abs()))),
            "блок {i}: {fx:?} против {fy:?}"
        );
        let (rx, ry) = (
            x.reference.as_ref().expect("опора"),
            y.reference.as_ref().expect("опора"),
        );
        assert!(
            [(rx.die, ry.die), (rx.lo, ry.lo), (rx.hi, ry.hi), (rx.median, ry.median)]
                .iter()
                .all(|&(a, b)| close(a, b, 1e-9 * (1.0 + a.abs()))),
            "блок {i}: опорные условия разные"
        );
    }
    // Дополнение 16.10: и слой с формой (веса, lo и узлы по оставшимся строкам), и кривые те же.
    assert!(trimmed.blocks.iter().all(|b| b.shaped.is_some()), "слой с формой есть");
    same_models(&trimmed, &fresh, 1e-9);
}

// ─── 12. Выученная форма своего быстрого нагрева ─────────────────────────────────────────────────────────────────
// Источник — дополнение API от 16.10 (model-api-shape.md) и уточнение узлов того же дня. Слой с формой (shaped):
// быстрый свой член — Σ w_m·hfast_m, hfast_m — среднее занятость·φ_m(r) в t и t+1 (спящее ядро — 0), φ_0 = 1,
// φ_1 = r − lo, φ_{1+j} = max(0, r − k_j); узлы k_j = floor(lo) + 2·j, пока k_j ≤ hi − 1; все w ≥ 0, так что
// g(r) = Σ w_m·φ_m(r) неубывающая и выпуклая; за узлами — последний наклон. Средние своей нагрузки и соседи — по p, как
// прежде. Колено — узел с наибольшим весом w*, если w* ≥ 0,1 °C/ступень и наклон после ≥ 2× наклона до.

// Колено формы как (где, наклон до, наклон после); None — колена нет.
fn knee_of(h: &Hinge) -> Option<(f64, f64, f64)> {
    h.knee().map(|k| (k.at, k.before, k.after))
}

// Форма из весов [w_0, w_1, веса узлов…]: lo = ×36, узлы ×38, ×40, … — столько, сколько весов узлов дано.
fn hinge(weights: &[f64]) -> Hinge {
    let knots = (1..weights.len().saturating_sub(1))
        .map(|j| 36.0 + 2.0 * real(j))
        .collect();
    Hinge {
        lo: 36.0,
        knots,
        weights: weights.to_vec(),
    }
}

#[test]
fn hinge_is_a_sum_of_ramps_continued_by_the_end_slopes() {
    // g = 5 + 0,1·(r − 36) + 0·(r − 38)⁺ + 0,3·(r − 40)⁺ + 1,0·(r − 42)⁺ + 0·(r − 44)⁺; значения посчитаны вручную.
    // Ловит: φ_1 = r вместо r − lo; вес j-го узла на соседнем узле; рампы без max(0, ·) (вклад ниже узла); продолжение
    // за краями не прямой; наклон в самом узле с его весом (по формуле — только узлы строго левее r); пустые веса не
    // как g ≡ 0.
    let h = hinge(&[5.0, 0.1, 0.0, 0.3, 1.0, 0.0]);
    let values = [
        (20.0, 3.4),
        (30.0, 4.4),
        (36.0, 5.0),
        (38.0, 5.2),
        (40.0, 5.4),
        (42.0, 6.2),
        (44.0, 9.0),
        (50.0, 17.4),
        (60.0, 31.4),
    ];
    for (r, want) in values {
        assert!(close(h.at(r), want, 1e-12), "g(×{r}) = {} вместо {want}", h.at(r));
    }
    let slopes = [
        (20.0, 0.1),
        (39.0, 0.1),
        (41.0, 0.4),
        (42.0, 0.4),
        (43.0, 1.4),
        (45.0, 1.4),
        (80.0, 1.4),
    ];
    for (r, want) in slopes {
        assert!(
            close(h.slope_at(r), want, 1e-12),
            "наклон на ×{r}: {} вместо {want}",
            h.slope_at(r)
        );
    }
    let empty = Hinge {
        lo: 36.0,
        knots: Vec::new(),
        weights: Vec::new(),
    };
    assert!(
        close(empty.at(45.0), 0.0, 0.0) && close(empty.slope_at(45.0), 0.0, 0.0) && empty.knee().is_none(),
        "пустые веса — g ≡ 0"
    );
}

#[test]
fn knee_is_the_heaviest_knot_with_a_tenth_per_step_and_a_doubled_slope() {
    // Ловит: колено на первом ненулевом узле, а не на наибольшем весе; «наклон до» без весов левее колена (только w_1);
    // «после» как вес узла без наклона до; порог веса «больше» вместо «не меньше» или не 0,1; «после ≥ 2·до» строгое;
    // поиск любого подходящего узла вместо проверки узла с наибольшим весом; колено у прямой.
    // Наибольший вес 1,0 на ×42; левее него 0,1 + 0,3: до 0,4, после 1,4.
    let k = knee_of(&hinge(&[5.0, 0.1, 0.0, 0.3, 1.0, 0.0])).expect("колено есть");
    assert!(
        close(k.0, 42.0, 0.0) && close(k.1, 0.4, 1e-12) && close(k.2, 1.4, 1e-12),
        "{k:?}"
    );
    // Порог веса: ровно 0,1 при наклоне до 0 — колено есть; 0,0999 — нет.
    let k = knee_of(&hinge(&[2.0, 0.0, 0.1, 0.0])).expect("вес ровно 0,1");
    assert!(
        close(k.0, 38.0, 0.0) && close(k.1, 0.0, 1e-12) && close(k.2, 0.1, 1e-12),
        "{k:?}"
    );
    assert!(knee_of(&hinge(&[2.0, 0.0, 0.0999, 0.0])).is_none(), "вес меньше 0,1");
    // Наклон после ровно вдвое больше — колено есть (0,25 → 0,5); чуть меньше — нет.
    let k = knee_of(&hinge(&[2.0, 0.25, 0.25, 0.0])).expect("ровно вдвое");
    assert!(
        close(k.0, 38.0, 0.0) && close(k.1, 0.25, 1e-12) && close(k.2, 0.5, 1e-12),
        "{k:?}"
    );
    assert!(knee_of(&hinge(&[2.0, 0.25, 0.2499, 0.0])).is_none(), "меньше чем вдвое");
    // Наибольший вес 0,35 на ×40 не удваивает наклон 0,4 — колена нет, хотя узел ×38 (0,1 → 0,4) удвоил бы.
    assert!(
        knee_of(&hinge(&[2.0, 0.1, 0.3, 0.35])).is_none(),
        "проверяется только узел с наибольшим весом"
    );
    // Прямая: без узлов и с нулевыми весами узлов.
    assert!(knee_of(&hinge(&[2.0, 0.5])).is_none(), "прямая без узлов");
    assert!(knee_of(&hinge(&[2.0, 1.5, 0.0, 0.0, 0.0])).is_none(), "прямая с узлами");
}

// Нагрузки кристалла 2 × 2 с множителями ×36…×52: множитель каждого отрезка генератора заменён на
// 36 + (5·r + 3·занятость) mod 17 — постоянен на отрезке и при полной нагрузке проходит весь диапазон, так что форме
// своего нагрева есть на чём учиться (у RATIOS всего 7 ступеней).
fn wide_loads() -> Vec<Vec<Load>> {
    let mut out = loads(20_261_016, 4, 4000, 15);
    for l in out.iter_mut().flatten() {
        let busy = l.busy;
        l.ratio = l.ratio.map(|r| 36 + (5 * r + 3 * busy) % 17);
    }
    out
}

// Узлы по правилу: floor(lo) + 2·j, j = 1, 2, … пока узел ≤ hi − 1.
fn knots_of(lo: f64, hi: f64) -> Vec<f64> {
    // Узлов не больше, чем ступеней в диапазоне: перебор конечен и при пустом диапазоне.
    (1_u32..=200)
        .map(|j| lo.floor() + 2.0 * f64::from(j))
        .take_while(|&k| k <= hi - 1.0)
        .collect()
}

// Коэффициенты своих средних 0,5…16 с у истин с формой: по 1 на 1, 4 и 16 с.
const HISTORY: [f64; 6] = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];

// Установившийся нагрев истины блока при своей нагрузке 100 % в опорных условиях модели: a + β·уровень + соседи по их
// медианным p + свой быстрый нагрев (форма или own[0]·p(r)) + средние своей нагрузки·p(r).
fn true_curve(tr: &Truth, rf: &Reference, r: f64) -> f64 {
    let mut v = tr.a + tr.beta * rf.die;
    for &(j, fast, slow) in &tr.near {
        v += (fast + slow) * rf.neighbours.iter().find(|x| x.0 == j).map_or(f64::NAN, |x| x.1);
    }
    let own: f64 = tr.own.iter().sum();
    v + own * cube(r) + tr.fast.as_ref().map_or(0.0, |f| f(r))
}

// Журнал с коленом у блока 0 и что о нём известно: свой быстрый нагрев 8 + 0,1·(r − lo) °C, после колена на
// 1,4 °C/ступень круче (0,1 → 1,5 °C/ступень при полной нагрузке); колено — средний из узлов по правилу (lo, hi —
// сплошная часть блока 0 на тех же нагрузках); средние своей нагрузки — HISTORY, соседи как у square()[0]; блоки 1–3 —
// square().
struct Bent {
    s: Synth,
    truths: Vec<Truth>,
    knots: Vec<f64>,
    knee: f64,
}

fn bent(whole: bool) -> Bent {
    let first = synth_from(&square(), wide_loads(), times(4000, &GAPS), false);
    let solid = solid_points(&first, 0);
    let (lo, hi) = (time_share(&solid, (5, 100)), time_share(&solid, (95, 100)));
    let knots = knots_of(lo, hi);
    assert!(knots.len() >= 4, "предпосылка: узлы {knots:?} при ×{lo}…×{hi}");
    let knee = knots[knots.len() / 2];
    let mut truths = square();
    truths[0] = truth_with_shape(
        18.0,
        0.55,
        Box::new(move |r: f64| 8.0 + 0.1 * (r - lo) + 1.4 * (r - knee).max(0.0)),
        HISTORY,
        &[(1, 1.5, 2.0), (2, 0.0, 3.0)],
    );
    let s = synth_from(&truths, wide_loads(), times(4000, &GAPS), whole);
    Bent { s, truths, knots, knee }
}

#[test]
fn shaped_fit_recovers_a_knee_and_the_whole_layer_on_exact_data() {
    // Блок 0 (bent): до колена свой нагрев почти плоский, после — крутой. Такая истина точно представима формой с
    // узлами по правилу, и на точных данных (шесть знаков) модель возвращает её целиком (независимая подгонка: всё до
    // 1e-4): lo и узлы, веса (w_0 = g(lo) = 8, w_1 = 0,1, на колене 1,4, остальные 0), средние и соседей на p, колено,
    // кривую внутри данных и за их краями (там истинный нагрев — прямая с последним наклоном).
    // Ловит: форму не от фактического множителя; узлы не по правилу; φ_1 = r вместо r − lo (w_0 = 8 − 0,1·lo); быстрый
    // член по p в слое с формой (own[0] ≠ 0) или форму в старом виде слоя (coef); средние своей нагрузки или соседей на
    // форме вместо p; колено не на наибольшем весе, наклоны колена не «до/после»; кривую по coef, с кубом вместо формы
    // или с изгибом за краем данных; кривую без подъёма после колена.
    let case = bent(false);
    let model = fit(&[&log_from_csv(&case.s.csv(None, None))], &case.s.die);
    let b = &model.blocks[0];
    let rf = b.reference.as_ref().expect("опорные условия");
    let sh = b.shaped.as_ref().expect("слой с формой");
    let tr = &case.truths[0];
    assert!(
        close(sh.shape.lo, rf.lo, 1e-9),
        "lo формы {} против ×{}",
        sh.shape.lo,
        rf.lo
    );
    assert!(
        all_near(&sh.shape.knots, &case.knots, 1e-9),
        "узлы {:?} вместо {:?}",
        sh.shape.knots,
        case.knots
    );
    let mut weights = vec![8.0, 0.1];
    weights.extend(
        case.knots
            .iter()
            .map(|&k| if close(k, case.knee, 1e-9) { 1.4 } else { 0.0 }),
    );
    assert!(
        all_near(&sh.shape.weights, &weights, 0.02),
        "веса {:?} вместо {weights:?}",
        sh.shape.weights
    );
    assert!(
        close(sh.own[0], 0.0, 0.0),
        "быстрый член по p в слое с формой: {}",
        sh.own[0]
    );
    assert!(
        all_near(&sh.own[1..], &tr.own[1..], 0.05),
        "свои средние {:?} вместо {:?}",
        sh.own,
        tr.own
    );
    assert!(
        close(sh.beta, tr.beta, 0.002) && close(sh.a, tr.a, 0.05),
        "β {}, a {}",
        sh.beta,
        sh.a
    );
    let near: Vec<f64> = sh.neighbours.iter().flat_map(|n| [n.fast, n.slow]).collect();
    let want: Vec<f64> = tr.near.iter().flat_map(|&(_, fast, slow)| [fast, slow]).collect();
    assert!(all_near(&near, &want, 0.01), "соседи {near:?} вместо {want:?}");
    for other in &model.blocks {
        assert!(
            other.coef.as_ref().is_some_and(|c| c.shape.weights.is_empty()),
            "блок {}: у старого вида слоя формы нет",
            other.name
        );
    }
    let (at, before, after) = b.knee().map(|k| (k.at, k.before, k.after)).expect("колено есть");
    assert!(
        close(at, case.knee, 1e-9) && close(before, 0.1, 0.01) && close(after, 1.5, 0.02),
        "колено ×{at}: {before} → {after} вместо ×{}: 0,1 → 1,5",
        case.knee
    );
    assert!(
        knee_of(&sh.shape).is_some_and(|k| close(k.0, at, 0.0) && close(k.1, before, 0.0) && close(k.2, after, 0.0)),
        "knee() блока — колено формы слоя с формой"
    );
    for r in grid(30.0, 60.0, 0.5) {
        let (got, want) = (b.curve(r).expect("кривая"), true_curve(tr, rf, r));
        assert!(close(got, want, 0.02), "×{r}: кривая {got} вместо {want}");
    }
    // Кривая показывает подъём: за 4 ступени после колена — больше чем втрое против 4 ступеней до него.
    let c = |r: f64| b.curve(r).expect("кривая");
    let (low, high) = (c(case.knee) - c(case.knee - 4.0), c(case.knee + 4.0) - c(case.knee));
    assert!(
        high > 3.0 * low && high - low > 4.0,
        "до колена +{low}, после +{high} за 4 ступени"
    );
}

#[test]
fn knee_is_found_in_whole_degree_data() {
    // Тот же журнал с коленом, температуры — целые градусы, как в настоящем журнале (шум округления ±0,5 °C). Колено —
    // не дальше шага узлов (2 ступени) от истинного, наклоны — примерно; кривая внутри сплошной части близка к истине
    // (независимая подгонка на этих данных: колено на истинном узле, 0,093 → 1,501 °C/ступень, кривая в пределах 0,04).
    // Ловит: колено, теряющееся в шуме, форму, сглаженную до прямой, сдвиг колена, кривую не по форме.
    let case = bent(true);
    let model = fit(&[&log_from_csv(&case.s.csv(None, None))], &case.s.die);
    let b = &model.blocks[0];
    let (at, before, after) = b.knee().map(|k| (k.at, k.before, k.after)).expect("колено есть");
    assert!(
        (at - case.knee).abs() <= 2.0 + 1e-9 && close(before, 0.1, 0.1) && close(after, 1.5, 0.2),
        "колено ×{at}: {before} → {after} вместо ×{}: 0,1 → 1,5",
        case.knee
    );
    let rf = b.reference.as_ref().expect("опорные условия");
    for r in grid(rf.lo, rf.hi, 0.5) {
        let (got, want) = (b.curve(r).expect("кривая"), true_curve(&case.truths[0], rf, r));
        assert!(close(got, want, 0.15), "×{r}: кривая {got} вместо {want}");
    }
}

#[test]
fn straight_own_heating_gives_no_knee() {
    // Свой быстрый нагрев — прямая по множителю: блок 0 — 10 °C при любом множителе (наклон 0), блок 1 — 6 + 0,6·(r −
    // 36), блок 2 — 4 + 1,5·(r − 36); блок 3 — square(). И на точных данных, и на целых градусах колена нет, а наклон
    // прямой выучен (независимая подгонка на целых градусах: наибольший вес узла 0,002, w_1 = 0,600 и 1,500).
    // Ловит: колено без порога веса 0,1 (у прямой с наклоном 0 «после ≥ 2·до» верно всегда — любой шумовой вес дал бы
    // колено), без условия «вдвое круче» (у крутой прямой), форму, не выучившую наклон (g ≡ 0 тоже без колена — поэтому
    // сверяется и наклон).
    let ramps = [(10.0, 0.0), (6.0, 0.6), (4.0, 1.5)];
    let plain = square();
    for whole in [false, true] {
        let mut truths = square();
        for ((tr, old), (start, slope)) in truths.iter_mut().zip(&plain).zip(ramps) {
            let own_line: Shape = Box::new(move |r: f64| start + slope * (r - 36.0));
            *tr = truth_with_shape(old.a, old.beta, own_line, HISTORY, &old.near);
        }
        let s = synth_from(&truths, wide_loads(), times(4000, &GAPS), whole);
        let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
        for (b, (_, slope)) in model.blocks.iter().zip(ramps) {
            let h = &b.shaped.as_ref().expect("слой с формой").shape;
            assert!(
                b.knee().is_none() && h.knee().is_none(),
                "блок {}, целые градусы {whole}: колено у прямой, веса {:?}",
                b.name,
                h.weights
            );
            assert!(
                h.weights.len() >= 2 && close(h.weights[1], slope, 0.05) && h.weights[2..].iter().all(|&w| w < 0.05),
                "блок {}, целые градусы {whole}: веса {:?} при наклоне {slope}",
                b.name,
                h.weights
            );
        }
    }
}

// Неубывание и выпуклость выученной формы блока по сетке ×8…×80 (далеко за наблюдённым ×36…×52), согласие наклона с
// приращениями, прямые продолжения ниже lo и правее hi (последний узел ≤ hi − 1).
fn convex_beyond_the_data(b: &BlockModel) -> Result<(), String> {
    let rf = b.reference.as_ref().ok_or("нет опорных условий")?;
    let h = &b.shaped.as_ref().ok_or("нет слоя с формой")?.shape;
    if !(close(h.lo, rf.lo, 1e-9) && all_near(&h.knots, &knots_of(rf.lo, rf.hi), 1e-9)) {
        return Err(format!("lo {} и узлы {:?} при ×{}…×{}", h.lo, h.knots, rf.lo, rf.hi));
    }
    if h.weights.len() != h.knots.len() + 2 || h.weights.iter().any(|&w| w < 0.0) {
        return Err(format!("веса {:?}", h.weights));
    }
    let values: Vec<(f64, f64)> = grid(8.0, 80.0, 0.25).into_iter().map(|r| (r, h.at(r))).collect();
    rising(&values, 1e-9)?;
    for three in values.windows(3) {
        let bend = three[0].1 - 2.0 * three[1].1 + three[2].1;
        if bend < -1e-9 * (1.0 + three[1].1.abs()) {
            return Err(format!("вогнута у ×{}: {bend}", three[1].0));
        }
    }
    for &(r, v) in &values {
        let step = (h.at(r + 0.1) - v) / 0.1;
        if !close(step, h.slope_at(r + 0.05), 1e-6 * (1.0 + step.abs())) {
            return Err(format!(
                "наклон у ×{r}: {} против приращения {step}",
                h.slope_at(r + 0.05)
            ));
        }
    }
    let straight = |a: f64, d: f64| {
        close(
            h.at(a + 2.0 * d) - h.at(a + d),
            h.at(a + d) - h.at(a),
            1e-9 * (1.0 + d.abs()),
        )
    };
    if !(straight(rf.lo, -10.0) && straight(rf.hi, 10.0)) {
        return Err("за краями не прямая".to_string());
    }
    Ok(())
}

#[test]
fn learned_shape_is_rising_and_convex_far_beyond_the_data_whatever_the_truth() {
    // Целые градусы (шум округления). Свой быстрый нагрев: блок 0 — вогнутая ломаная (1,5 °C/ступень до ×44, потом
    // 0,1), блок 1 падает с множителем (20 − 0,5·(r − 36)), блок 2 — парабола с вершиной на ×44, блок 3 — прямая с
    // волной 2·sin r. Без ограничения веса формы у каждого блока выходят отрицательными (независимый МНК на этих
    // данных: −1,4; −0,4; −0,6; −3,1). Дополнение: все w ≥ 0 — g неубывающая и выпуклая при любых данных, ниже lo —
    // прямая с наклоном w_1, правее последнего узла — прямая с наклоном w_1 + Σ w_j; узлы по правилу.
    // Ловит: веса без ограничения; ограничение только неубывания (наклоны ≥ 0, веса узлов любого знака — вогнутая g);
    // w_0 < 0; форму не из шарниров (многочлен, сплайн), изгибающуюся за краями данных; slope_at, не согласный с at;
    // кривую, падающую из-за формы.
    let shapes: [Shape; 4] = [
        Box::new(|r: f64| 8.0 + 1.5 * (r - 36.0) - 1.4 * (r - 44.0).max(0.0)),
        Box::new(|r: f64| 20.0 - 0.5 * (r - 36.0)),
        Box::new(|r: f64| 15.0 - 0.1 * (r - 44.0).powi(2)),
        Box::new(|r: f64| 10.0 + 0.6 * (r - 36.0) + 2.0 * r.sin()),
    ];
    let truths: Vec<Truth> = square()
        .iter()
        .zip(shapes)
        .map(|(old, f)| truth_with_shape(old.a, old.beta, f, HISTORY, &old.near))
        .collect();
    let s = synth_from(&truths, wide_loads(), times(4000, &GAPS), true);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    for b in &model.blocks {
        if let Err(why) = convex_beyond_the_data(b) {
            panic!("блок {}: форма {why}", b.name);
        }
        if let Err(why) = curve_rises(b) {
            panic!("блок {}: кривая падает: {why}", b.name);
        }
    }
}

// Ручной журнал двух соседних блоков, 400 строк через 250 мс; у блока 0 нагрузка 0,8 всегда, множитель по номеру строки
// k: k mod 10 = 0 — ×36,6 (10 % строк), k mod 50 = 49 — ×44 (2 %), остальные k mod 10 = 9 — top (8 %), прочие ×38…×41
// (80 %). Сплошная часть блока 0 — ×36,6…top.
fn knot_log(top: &str) -> (Log, Die) {
    let die = Die {
        names: names(&["0", "1"]),
        e: vec![false; 2],
        neighbours: vec![vec![1], vec![0]],
    };
    let rows: Vec<(i64, Vec<Cell>)> = (0..400)
        .map(|k: usize| {
            let mut cells: Vec<Cell> = (0..2).map(|b| usual(k, b)).collect();
            cells[0].0 = match (k % 10, k % 50) {
                (0, _) => "36.6".to_string(),
                (_, 49) => "44".to_string(),
                (9, _) => top.to_string(),
                _ => (38 + k % 4).to_string(),
            };
            cells[0].1 = "0.80".to_string();
            (START_MS + 250 * i64::try_from(k).expect("строк мало"), cells)
        })
        .collect();
    (hand_log(&die.names, &rows), die)
}

#[test]
fn knots_start_at_floor_of_lo_and_keep_a_whole_step_below_hi() {
    // Уточнение 16.10 (после прогона на данных владельца: узел ×42 при hi = ×42,37 получил излом 8,5 °C/ступень по
    // горстке строк и унёс его в пунктир): узлы floor(lo) + 2·j, пока узел ≤ hi − 1 — над каждым узлом хотя бы ступень
    // наблюдённых данных. Сплошная часть ×36,6…×42,37 → узлы ×38, ×40; ×36,6…×43 → ×38, ×40, ×42.
    // Ловит: узлы от lo, а не от floor(lo) (×38,6, ×40,6), или от ceil(lo) (×39, ×41); прежнее «узел < hi» (×42 при hi
    // ×42,37); строгое «< hi − 1» (нет ×42 при hi ×43); шаг не 2; веса не по числу узлов.
    for (top, want) in [("42.37", &[38.0, 40.0][..]), ("43", &[38.0, 40.0, 42.0][..])] {
        let (log, die) = knot_log(top);
        let model = fit(&[&log], &die);
        let b = &model.blocks[0];
        let rf = b.reference.as_ref().expect("опорные условия");
        let hi: f64 = top.parse().expect("число");
        assert!(
            close(rf.lo, 36.6, 1e-4) && close(rf.hi, hi, 1e-4),
            "предпосылка: сплошная часть ×{}…×{} вместо ×36,6…×{top}",
            rf.lo,
            rf.hi
        );
        let h = &b.shaped.as_ref().expect("слой с формой").shape;
        assert!(close(h.lo, rf.lo, 1e-9), "lo формы {} против ×{}", h.lo, rf.lo);
        assert!(
            all_near(&h.knots, want, 1e-9) && h.weights.len() == want.len() + 2,
            "hi ×{top}: узлы {:?} вместо {want:?}, весов {}",
            h.knots,
            h.weights.len()
        );
    }
}
