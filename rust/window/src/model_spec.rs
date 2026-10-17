// Тесты тепловой модели (мощность p, средние, NNLS, пригодные строки, слой L3, свой нагрев по целым множителям,
// кривые, эффективность) по спецификации; пишет отдельный агент, не читавший реализацию (rust\AGENTS.md, «Tests from
// the specification»). Источники — бриф outputs\model-impl-brief-2026-10-16.md («Inputs per row», «Layer L3», список
// тестов в «Rules»), описание API model.rs от 16.10 и бриф outputs\efficiency-brief-2026-10-16.md (16.10 вечером:
// шарнирная форма выброшена, по коэффициенту на (блок, целый множитель) — накопительные индикаторы × занятость, веса ≥ 0;
// эффективность = r / свой нагрев при полной нагрузке; ватт нет); путь напряжения убран из модели решением владельца
// 16.10 (напряжение задаёт сам процессор по множителю — не независимый вход), p = занятость·(r/45)³. Раздел 12 (ступени,
// эффективность) дописан исполнителем брифа 16.10 «эффективность» — без независимого агента (лимиты владельца).
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
    ASLEEP, Averages, BlockModel, Coef, Die, Efficiency, GAP_MS, Log, MIN_BIN_SECS, MIN_MINUTES, MIN_ROWS, Model,
    NEIGHBOUR_TAU, OwnSums, REFERENCE, Reference, Steps, TAUS, WARM_MS, fit, log_from_csv, nnls, proxy, retain,
};

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

// Заголовок, как его пишет программа входа (0.5.4), для блоков с этими именами; volts — со столбцами напряжения.
fn header(names: &[String], volts: bool) -> String {
    let mut cols: Vec<String> = ["time", "package", "fan_cpu", "fan_gpu"].map(String::from).to_vec();
    for n in names {
        for what in ["ceiling", "ratio", "busy", "temp"] {
            cols.push(format!("{n}.{what}"));
        }
    }
    cols.extend(["power_cpu", "temp_gpu", "power_gpu", "busy_gpu"].map(String::from));
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
    let mut text = header(blocks, false);
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
fn log_keeps_old_and_millisecond_rows() {
    // Полные прежние строки нужны общей истории; пригодность шага для конкретного расчёта определяет потребитель.
    // Пустой журнал (только заголовок) не должен давать панику вместо нуля строк и пустой модели.
    let blocks = names(&["0", "1"]);
    let mut text = header(&blocks, false);
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
    assert_eq!(log_from_csv(&text).rows(), 8, "все строки с корректным временем");
    let empty = log_from_csv(&(header(&blocks, false) + "\n"));
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
        let mut dice = Dice(noise.unwrap_or(0));
        let mut random = |lo: u32, span: u32| noise.map(|_| lo + dice.below(span));
        let mut lines = vec![header(&self.die.names, volts.is_some())];
        for (t, (&ms, row)) in self.times.iter().zip(&self.loads).enumerate() {
            let mut fields = vec![
                stamp(ms),
                random(40, 60).unwrap_or(70).to_string(),
                random(2000, 4000).unwrap_or(3000).to_string(),
                random(2000, 4000).unwrap_or(2900).to_string(),
            ];
            // Потолок строки случаен, но один у всех блоков: ранг P-ядер (дополнение брифа 16.10 — из порядка
            // потолков) у всех один и тот же, сам потолок входом не бывает.
            let ceiling = random(30, 26).unwrap_or(50).to_string();
            for (l, temp) in row.iter().zip(&self.temps[t]) {
                fields.push(ceiling.clone()); // a short number per field, test data
                fields.push(l.ratio.map_or_else(String::new, |r| r.to_string()));
                fields.push(format!("{}.{:02}", l.busy / 100, l.busy % 100));
                fields.push(temp.clone()); // текст поля строки
            }
            fields.push(format!("{}.0", random(5, 120).unwrap_or(45)));
            fields.push(random(40, 50).unwrap_or(55).to_string());
            fields.push(if t % 8 == 0 {
                format!("{}.0", random(20, 100).unwrap_or(80))
            } else {
                String::new()
            });
            fields.push(random(0, 100).map_or_else(String::new, |v| v.to_string()));
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

// Всё числовое, что модель знает о блоке: пригодные строки и минуты, слой L3 в старом виде и со ступенями (вместе с
// самими ступенями), опорные условия, кривая на ×30…×60 через 2.
fn block_numbers(b: &BlockModel) -> Vec<f64> {
    let mut v = vec![real(b.usable), b.minutes];
    for c in [b.coef.as_ref(), b.shaped.as_ref()].into_iter().flatten() {
        v.extend(coef_numbers(c));
        v.extend(
            c.shape
                .bins
                .iter()
                .map(|&b| f64::from(i32::try_from(b).expect("множитель"))),
        );
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
    let parts = |b: &BlockModel| [b.coef.is_some(), b.shaped.is_some(), b.reference.is_some(), b.drawn()];
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
    // Кривая — установившийся слой L3 со ступенями (shaped) при своей нагрузке 100 %: быстрый свой член — g(r), средние
    // своей нагрузки — p(r) = (r/45)³, входы соседей — их медианным p (дополнение 16.10: кривая — по shaped, не по
    // coef). Сверка с истинными коэффициентами на наблюдённых множителях и с формулой API на числах модели везде.
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
        // На наблюдённых целых множителях ступени повторяют свой нагрев истины K·(r/45)³ (ступень корзины — значение
        // данных на ней); между корзинами и за краями ступени держат значение ближайшей корзины, и с кубом истины там
        // не сверяются (бриф 16.10 «эффективность»: формы не задано).
        let own: f64 = tr.own.iter().sum();
        for r in RATIOS
            .iter()
            .map(|&r| f64::from(r))
            .filter(|r| (rf.lo..=rf.hi).contains(r))
        {
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
    // потолками (у всех блоков строки — один: ранг P-ядер, дополнение 16.10, тот же), корпусом, вентиляторами,
    // мощностями и видеокартой — слой L3 (в старом виде и с формой, с самой формой и её узлами), опорные условия и
    // кривые те же.
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
        // Оба вида слоя (старый — для сравнения в отчёте, со ступенями — для кривой) с неотрицательными коэффициентами.
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

// p при полной своей нагрузке без напряжения: (r/45)³.
fn cube(r: f64) -> f64 {
    (r / 45.0).powi(3)
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

// ─── 11. Нагрев ядра, частные остатки и хранение строк (бриф W 16.10) ─────────────────────────────────────────────
// Источник — outputs/own-heating-view-brief-2026-10-16.md и outputs/window-pass-brief-2026-10-16.md. Нагрев ядра
// H(r) — установившийся свой нагрев при полной нагрузке: форма g(r) + Σ средних · p(r); без свободного члена, кристалла,
// соседей и ранга. Частный остаток строки — T − (a + β·D + соседи + ранг), делённый на занятость; точка корзины —
// средняя с весом занятость × время, то есть Σ остатков / Σ занятостей. Хранение: по клетке (блок, целый множитель)
// новейшие окна 10 с, пока стандартная ошибка средней остатка по окнам не дошла до 0,1 °C (не меньше 10 окон); старше —
// выбрасываются, если строка не нужна другому блоку; ненасыщенные клетки держат всё.

// Числа слоя L3 блока: a, β, свои, соседи (быстрый, 4-с).
fn coef_numbers(c: &Coef) -> Vec<f64> {
    let mut v = vec![c.a, c.beta];
    v.extend(c.own);
    v.extend(c.neighbours.iter().flat_map(|n| [n.fast, n.slow]));
    v
}

// Нагрузки [строка][блок]: отрезки по `segment` строк постоянной нагрузки — множитель и занятость по кругу, у каждого
// блока со своим сдвигом (средние всех времён успевают установиться).
fn steady_loads(rows: usize, blocks: usize, segment: usize) -> Vec<Vec<Load>> {
    let ratios = [38_u32, 40, 42, 44, 46, 48];
    let busies = [100_u32, 60, 85, 40, 95, 70, 50];
    (0..rows)
        .map(|t| {
            (0..blocks)
                .map(|b| {
                    let k = t / segment + 3 * b;
                    Load {
                        busy: busies[k % busies.len()],
                        ratio: Some(ratios[(k / 2 + b) % ratios.len()]),
                    }
                })
                .collect()
        })
        .collect()
}

// Четыре блока квадратом; у блока 0 — свой быстрый нагрев с коленом и средние HISTORY; `a` и множитель соседей блока 0
// заданы (`base`, `near`), остальное — как в square().
fn own_case(base: f64, near: f64, rows: usize) -> (Synth, Vec<Truth>) {
    let mut truths = square();
    truths[0] = truth_with_shape(
        base,
        0.55,
        Box::new(|r: f64| 6.0 + 0.2 * (r - 38.0) + 1.2 * (r - 44.0).max(0.0)),
        HISTORY,
        &[(1, 1.5 * near, 2.0 * near), (2, 0.0, 3.0 * near)],
    );
    let s = synth_from(&truths, steady_loads(rows, 4, 1200), times(rows, &[]), false);
    (s, truths)
}

// Установившийся свой нагрев истины блока 0 при полной нагрузке на r: форма + Σ средних · p(r).
fn true_own(r: f64) -> f64 {
    6.0 + 0.2 * (r - 38.0) + 1.2 * (r - 44.0).max(0.0) + HISTORY.iter().sum::<f64>() * cube(r)
}

#[test]
fn own_heating_is_the_own_part_and_excludes_the_base_and_the_neighbours() {
    // Тот же свой нагрев блока 0, а свободный член (18 и 30 °C) и связь с соседями (×1 и ×2) разные: нагрев ядра один и
    // тот же и равен истинному g(r) + Σ средних · p(r) на сплошной части; кривая абсолютной температуры — разная.
    // Ловит: нагрев ядра со свободным членом, с кристаллом или с соседями; без средних своей нагрузки; не по форме.
    let fitted = |base: f64, near: f64| {
        let (s, _) = own_case(base, near, 14_400);
        fit(&[&log_from_csv(&s.csv(None, None))], &s.die)
    };
    let (one, two) = (fitted(18.0, 1.0), fitted(30.0, 2.0));
    let (x, y) = (&one.blocks[0], &two.blocks[0]);
    let rf = x.reference.as_ref().expect("опорные условия");
    // Наблюдённые множители steady_loads — чётные ×38…×48; между ними ступени держат значение корзины и с истиной не
    // сверяются.
    for r in grid(38.0, 48.0, 2.0)
        .into_iter()
        .filter(|r| (rf.lo..=rf.hi).contains(r))
    {
        let (hx, hy) = (x.own_heat(r).expect("нагрев ядра"), y.own_heat(r).expect("нагрев ядра"));
        assert!(
            close(hx, true_own(r), 0.05) && close(hy, true_own(r), 0.05),
            "×{r}: нагрев ядра {hx:.3} и {hy:.3}, истинный {:.3}",
            true_own(r)
        );
        let (cx, cy) = (x.curve(r).expect("кривая"), y.curve(r).expect("кривая"));
        assert!(
            cy - cx > 10.0,
            "×{r}: кривая абсолютной температуры не сдвинулась ({cx:.2} → {cy:.2})"
        );
    }
}

#[test]
fn partial_residuals_lie_on_the_own_heating_curve() {
    // Точный журнал с отрезками по 5 минут постоянной нагрузки: средняя частных остатков по корзине множителя (Σ остаток
    // / Σ занятость) — на кривой нагрева ядра у каждого множителя с данными (средние своей нагрузки успевают
    // установиться; погрешность — только переходы). Ловит: остаток с кристаллом, соседями или свободным членом;
    // остаток без деления на занятость (точки ниже кривой в разы при занятости 0,4); вес не по занятости.
    let (s, _) = own_case(18.0, 1.0, 14_400);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let b = &model.blocks[0];
    let points = &b.own_bins;
    assert!(points.len() >= 5, "точек {}: {points:?}", points.len());
    for p in points {
        let h = b.own_heat(f64::from(p.ratio)).expect("нагрев ядра");
        assert!(
            close(p.value, h, 0.25),
            "×{}: точка {:.3} (нагрузка {:.2}, {:.0} с), кривая {h:.3}",
            p.ratio,
            p.value,
            p.load,
            p.secs
        );
    }
}

#[test]
fn a_bin_is_the_sum_of_residuals_over_the_sum_of_busy() {
    // Бриф W: точка — все годные строки с весом занятости, остаток строки делится на занятость: Σ w·(о/b) / Σ w при w =
    // b × время = Σ время·о / Σ время·b. Строка A: занятость 1, остаток 10; строка B: 0,1 и 1,5 (1,5/0,1 = 15 °C на полную
    // нагрузку). Точка — 11,5/1,1 = 10,45; не 12,5 (простая средняя о/b) и не 5,75 (средняя остатков). Меньше одной строки
    // полной нагрузки (0,25 с × 1) — точки нет.
    let mut bin = OwnSums::default();
    bin.add(250.0, 1.0, 10.0);
    bin.add(250.0, 0.1, 1.5);
    let p = bin.bin(45).expect("точка");
    assert!(close(p.value, 11.5 / 1.1, 1e-9), "{p:?}");
    assert!(close(p.load, 0.55, 1e-9) && close(p.secs, 0.5, 1e-9), "{p:?}");
    let mut thin = OwnSums::default();
    thin.add(250.0, 0.5, 3.0);
    assert!(thin.bin(45).is_none(), "полстроки полной нагрузки — точки нет");
}

#[test]
fn efficiency_at_a_bin_is_the_multiplier_over_the_own_heating_within_the_fold_range() {
    // Бриф 16.10: эффективность корзины = r / H(r) по всем данным, разброс — по четырём подгонкам без куска (вместе со
    // значением), только где разброс H не доходит до нуля; сам нагрев в точке — H(r). Ловит: r/H по подгонке, а не по
    // всем данным; разброс не как r / (наибольший…наименьший H); точку с H ≤ 0 в какой-то подгонке.
    let (s, _) = own_case(18.0, 1.0, 14_400);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let b = &model.blocks[0];
    let points = b.efficiency();
    let at44 = points.iter().find(|e| e.ratio == 44).expect("точка на ×44");
    let (lo, hi) = b.own_heat_range(44.0).expect("разброс");
    let h = b.own_heat(44.0).expect("нагрев ядра");
    assert!(lo <= h && h <= hi && lo > 0.0, "{lo} {h} {hi}");
    assert!(
        close(at44.value, 44.0 / h, 1e-9) && close(at44.heat, h, 1e-9),
        "{at44:?}"
    );
    assert!(
        close(at44.lo, 44.0 / hi, 1e-9) && close(at44.hi, 44.0 / lo, 1e-9),
        "{at44:?}"
    );
    assert!(at44.lo <= at44.value && at44.value <= at44.hi, "{at44:?}");
    assert!(
        close(at44.value, 44.0 / true_own(44.0), 0.05 * at44.value),
        "×44: {} против истинной {}",
        at44.value,
        44.0 / true_own(44.0)
    );
}

// Двухблочный журнал хранения: 30 мин по 0,25 с; блок 0 на ×45 всё время, кроме строк 3000…3199 (50 с) на ×40;
// температура постоянна в каждой клетке; блок 1 — сосед, всегда ×45.
fn retention_log() -> (Log, Die) {
    let die = Die {
        names: names(&["0", "1"]),
        e: vec![false; 2],
        neighbours: vec![vec![1], vec![0]],
    };
    let rows: Vec<(i64, Vec<Cell>)> = (0..7200_usize)
        .map(|k| {
            let rare = (3000..3200).contains(&k);
            let cells = vec![
                (
                    if rare { "40" } else { "45" }.to_string(),
                    "0.80".to_string(),
                    if rare { "62" } else { "66" }.to_string(),
                ),
                ("45".to_string(), "0.80".to_string(), "58".to_string()),
            ];
            (START_MS + 250 * i64::try_from(k).expect("строк мало"), cells)
        })
        .collect();
    (hand_log(&die.names, &rows), die)
}

#[test]
fn retention_keeps_the_newest_rows_of_a_saturated_cell_and_every_row_of_a_rare_one() {
    // Клетка (0, ×45) и (1, ×45): остаток постоянен — стандартная ошибка 0 уже на 10 окнах: остаются новейшие 10 окон
    // (100 с) и последние 60 с журнала; клетка (0, ×40) — 5 окон, не насыщена: все её строки остаются, и с ними 16 с
    // перед ними (средние) и следующая строка. Ловит: хранение по самым старым строкам; выброс ненасыщенной клетки;
    // строки без средних перед ними (тогда их не взять в счёт); выброс нужной другому блоку строки.
    let (mut log, die) = retention_log();
    let times: Vec<i64> = log.times().to_vec();
    let model = fit(&[&log], &die);
    let kept = retain(&mut [&mut log], &die, &model);
    let after: Vec<i64> = log.times().to_vec();
    assert_eq!(kept.before, 7200);
    assert_eq!(kept.after, after.len());
    assert!(kept.saturated >= 2 && kept.cells >= 3, "{kept:?}");
    let has = |k: usize| after.binary_search(&times[k]).is_ok();
    assert!((3000..3200).all(has), "строки редкой клетки остались");
    assert!((2936..3000).all(has), "16 с перед ними остались");
    assert!(has(3200), "и следующая строка");
    assert!((6800..7200).all(has), "новейшие 100 с остались");
    assert!(
        !has(100) && !has(2000) && !has(5000),
        "старые строки насыщенной клетки выброшены"
    );
    assert!(after.len() < 1000, "оставлено {} строк из 7200", after.len());
    // Оставленное годится в счёт: редкая клетка — в модели, как и была.
    let again = fit(&[&log], &die);
    let rare = |m: &Model| m.blocks[0].own_bins.iter().find(|b| b.ratio == 40).map(|b| b.secs);
    assert!(
        rare(&again).is_some() && close(rare(&again).unwrap_or(0.0), rare(&model).unwrap_or(f64::NAN), 0.6),
        "×40 до {:?} и после {:?}",
        rare(&model),
        rare(&again)
    );
}

// Три P-ядра без соседей, ×45 всегда, занятость — отрезками по 13…40 строк; потолки (50, 48, 46) по кругу каждые 600
// строк — каждое ядро бывает в каждом ранге. Температура: 20 + 10·pfast + 3·pfast в ранге 1 + 1·pfast в ранге 2.
fn ranked_log() -> (Log, Die) {
    let die = Die {
        names: names(&["0", "1", "2"]),
        e: vec![false; 3],
        neighbours: vec![Vec::new(); 3],
    };
    let rows = 7200_usize;
    let busy = |b: usize, k: usize| 0.2 + 0.1 * real((k / (13 + 9 * b) + b) % 8);
    let ceiling = |b: usize, k: usize| [50, 48, 46][(b + k / 600) % 3];
    let mut text = String::from(
        "time,0.ceiling,0.ratio,0.busy,0.temp,1.ceiling,1.ratio,1.busy,1.temp,2.ceiling,2.ratio,2.busy,2.temp",
    );
    for k in 0..rows {
        text.push(char::from(10));
        text.push_str(&stamp(START_MS + 250 * i64::try_from(k).expect("строк мало")));
        for b in 0..3 {
            let next = if k + 1 < rows { busy(b, k + 1) } else { busy(b, k) };
            let fast = f64::midpoint(busy(b, k), next);
            let rank = match ceiling(b, k) {
                50 => 3.0,
                48 => 1.0,
                _ => 0.0,
            };
            let temp = 20.0 + (10.0 + rank) * fast;
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!(",{},45,{:.2},{temp:.6}", ceiling(b, k), busy(b, k)),
            ); // into a String
        }
    }
    text.push(char::from(10));
    (log_from_csv(&text), die)
}

#[test]
fn a_shared_rank_term_is_recovered_and_kept_out_of_the_own_heating() {
    // Дополнение брифа 16.10: общий для P-ядер член на ранг 1 и 2 (знак любой), опознаётся сменой ранга внутри ядра;
    // нагрев ядра — при общем ранге (без члена ранга). Истина: ранг 1 — +3·pfast, ранг 2 — +1·pfast. Ловит: ранг по
    // своему ядру, а не общий; ранг в нагреве ядра (тогда у ядер разный H при одном своём нагреве 10 °C); ранг не по
    // порядку потолков P-ядер.
    let (log, die) = ranked_log();
    let model = fit(&[&log], &die);
    let r = &model.rank;
    assert!(r.used[0] && r.used[1], "{r:?}");
    assert!(
        close(r.value[0], 3.0, 0.02) && close(r.value[1], 1.0, 0.02),
        "ранг {:?}",
        r.value
    );
    assert!(r.determined[0] && r.determined[1], "{r:?}");
    for b in &model.blocks {
        let h = b.own_heat(45.0).expect("нагрев ядра");
        assert!(close(h, 10.0, 0.05), "блок {}: нагрев ядра {h}", b.name);
    }
}

#[test]
fn a_rank_without_rank_changes_is_not_in_the_fit() {
    // У всех ядер всегда один потолок (журнал hand_log: 50 у всех) — смены ранга нет, его член не в счёте.
    let (log, die) = retention_log();
    let model = fit(&[&log], &die);
    assert_eq!(model.rank.used, [false, false]);
    assert_eq!(model.rank.value, [0.0, 0.0]);
}

#[test]
fn retention_without_a_model_of_a_block_keeps_every_row_of_it() {
    // Модели нет (другой кристалл — блоки другие): хранить не по чему — все строки остаются.
    let (mut log, die) = retention_log();
    let other = Die {
        names: names(&["x", "y"]),
        e: vec![false; 2],
        neighbours: vec![vec![1], vec![0]],
    };
    let model = fit(&[&log], &other);
    let kept = retain(&mut [&mut log], &die, &model);
    assert_eq!((kept.before, kept.after), (7200, 7200));
}

// ─── 12. Свой нагрев по целым множителям и эффективность ──────────────────────────────────────────────────────────
// Источник — бриф outputs\efficiency-brief-2026-10-16.md и API model.rs (`Steps`, `BlockModel::efficiency`). Слой со
// ступенями (shaped): быстрый свой член — Σ_j w_j·hfast_j, hfast_j — среднее занятость·[целый множитель ≥ bins[j]] в t и
// t+1 (спящее ядро — 0), корзины bins — целые множители годных строк блока по возрастанию; все w ≥ 0, так что g(r) =
// Σ_{bins[j] ≤ round(r)} w_j — неубывающая ступенчатая без иной формы; ниже первой корзины 0, выше последней — вся сумма.
// Средние своей нагрузки и соседи — по p, как прежде. Эффективность корзины — r / (g(r) + Σ средних · p(r)) там, где
// нагрев по всем четырём подгонкам без куска больше нуля и в корзине не меньше минуты полной нагрузки (занятость ×
// время строк); разброс — по подгонкам (наименьший…наибольший нагрев).

#[test]
fn steps_are_cumulative_sums_of_bin_weights_and_flat_beyond_the_bins() {
    // g на корзине — сумма весов корзин не выше неё: ×40 4, ×42 4,5, ×43 4,5 (вес 0 — площадка), ×45 7, ×46 11; ниже ×40
    // — 0; выше ×46 — 11; множитель строки округляется к ближайшему целому (×41,6 → корзина ×42; ×44 — между корзинами
    // ×43 и ×45 — значение ×43). Ловит: индикаторы не накопительные (вес только своей корзины); округление вниз; продолжение
    // за краями не постоянной; пустые веса не как g ≡ 0.
    let s = Steps {
        bins: vec![40, 42, 43, 45, 46],
        weights: vec![4.0, 0.5, 0.0, 2.5, 4.0],
    };
    let values = [
        (20.0, 0.0),
        (39.4, 0.0),
        (39.5, 4.0),
        (40.0, 4.0),
        (41.6, 4.5),
        (43.0, 4.5),
        (44.0, 4.5),
        (45.0, 7.0),
        (46.0, 11.0),
        (80.0, 11.0),
    ];
    for (r, want) in values {
        assert!(close(s.at(r), want, 1e-12), "g(×{r}) = {} вместо {want}", s.at(r));
    }
    let empty = Steps {
        bins: Vec::new(),
        weights: Vec::new(),
    };
    assert!(close(empty.at(45.0), 0.0, 0.0), "пустые веса — g ≡ 0");
}

// Коэффициенты своих средних 0,5…16 с у истин с формой: по 1 на 1, 4 и 16 с.
const HISTORY: [f64; 6] = [0.0, 1.0, 0.0, 1.0, 0.0, 1.0];

// Нагрузки кристалла 2 × 2 с множителями ×36…×52: множитель каждого отрезка генератора заменён на
// 36 + (5·r + 3·занятость) mod 17 — постоянен на отрезке и при полной нагрузке проходит весь диапазон, так что ступеням
// есть на чём учиться (у RATIOS всего 7 ступеней).
fn wide_loads(rows: usize) -> Vec<Vec<Load>> {
    let mut out = loads(20_261_016, 4, rows, 15);
    for l in out.iter_mut().flatten() {
        let busy = l.busy;
        l.ratio = l.ratio.map(|r| 36 + (5 * r + 3 * busy) % 17);
    }
    out
}

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

// Таблица своего быстрого нагрева блока 0 по целому множителю, °C при полной нагрузке: не выпуклая и не вогнутая —
// площадка до ×40, пологий подъём до ×44, скачок на ×45, площадка до ×48, крутой подъём до ×52; не падает.
fn table(r: f64) -> f64 {
    let k = r.round();
    if k <= 40.0 {
        8.0
    } else if k <= 44.0 {
        8.0 + 0.2 * (k - 40.0)
    } else if k <= 48.0 {
        12.0
    } else {
        12.0 + 1.0 * (k - 48.0)
    }
}

// Целые множители годных строк блока i по возрастанию и у каждого — занятость × время строк, с (время строки — мс от
// прошлой).
fn bins_of(s: &Synth, i: usize) -> Vec<(i64, f64)> {
    let mut out: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    for t in 1..s.times.len() {
        let l = s.loads[t][i];
        if let Some(r) = l.ratio.filter(|_| s.usable[i][t]) {
            *out.entry(i64::from(r)).or_default() += secs(s.times[t] - s.times[t - 1]) * f64::from(l.busy) / 100.0;
        }
    }
    out.into_iter().collect()
}

// Журнал из `rows` строк с табличным нагревом блока 0 (средние HISTORY, соседи как у square()[0]; блоки 1–3 — square()),
// точные температуры.
fn tabled(rows: usize) -> (Synth, Vec<Truth>) {
    let mut truths = square();
    truths[0] = truth_with_shape(18.0, 0.55, Box::new(table), HISTORY, &[(1, 1.5, 2.0), (2, 0.0, 3.0)]);
    let s = synth_from(&truths, wide_loads(rows), times(rows, &GAPS), false);
    (s, truths)
}

#[test]
fn stepped_fit_recovers_a_table_of_own_heating_on_exact_data() {
    // Блок 0 (tabled): свой нагрев — таблица по целому множителю с площадками и скачком, ломаной с узлами через 2
    // ступени не представимая. На точных данных (шесть знаков) ступени возвращают её на каждой корзине: g(r) = таблица,
    // own[0] = 0, средние — HISTORY, кривая — истинная; корзины — целые множители годных строк блока; веса ≥ 0.
    // Ловит: корзины не по годным строкам (по всем строкам, по строкам под нагрузкой, по сплошной части); форму с
    // заданным видом (прямая, ломаная, куб) — она не даст площадку и скачок разом; быстрый член по p в слое со ступенями
    // (own[0] ≠ 0); средние или соседей на ступенях вместо p; кривую не по ступеням.
    let (s, truths) = tabled(4000);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let b = &model.blocks[0];
    let rf = b.reference.as_ref().expect("опорные условия");
    let sh = b.shaped.as_ref().expect("слой со ступенями");
    let tr = &truths[0];
    let bins = bins_of(&s, 0);
    assert_eq!(
        sh.shape.bins,
        bins.iter().map(|b| b.0).collect::<Vec<_>>(),
        "корзины — целые множители годных строк"
    );
    assert_eq!(sh.shape.weights.len(), sh.shape.bins.len(), "по весу на корзину");
    assert!(sh.shape.weights.iter().all(|&w| w >= 0.0), "{:?}", sh.shape.weights);
    for &(r, _) in &bins {
        let x = f64::from(i32::try_from(r).expect("множитель"));
        assert!(
            close(sh.shape.at(x), table(x), 0.02),
            "×{r}: g {} вместо {}",
            sh.shape.at(x),
            table(x)
        );
        let (got, want) = (b.curve(x).expect("кривая"), true_curve(tr, rf, x));
        assert!(close(got, want, 0.02), "×{r}: кривая {got} вместо {want}");
    }
    assert!(
        close(sh.own[0], 0.0, 0.0),
        "быстрый член по p в слое со ступенями: {}",
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
    for other in &model.blocks {
        assert!(
            other.coef.as_ref().is_some_and(|c| c.shape.weights.is_empty()),
            "блок {}: у старого вида слоя ступеней нет",
            other.name
        );
    }
}

#[test]
fn efficiency_points_are_r_over_heating_in_bins_with_a_minute_of_full_load() {
    // Та же истина: у каждой корзины с минутой полной нагрузки (занятость × время ≥ 60 с) и нагревом > 0 во всех
    // подгонках — точка r / (таблица + HISTORY·p(r)); корзины с меньшим временем точки не дают; разброс содержит
    // значение; нагрев и данные точки — те же числа. Ловит: порог по строкам, а не по занятости × времени; точку у тонкой
    // корзины; r / нагрев по подгонке; разброс не по подгонкам.
    // 16 000 строк (67 мин): при 17 корзинах ×36…×52 у большинства набирается минута полной нагрузки, у нескольких — нет.
    let (s, _) = tabled(16_000);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    let b = &model.blocks[0];
    let points: Vec<Efficiency> = b.efficiency();
    let bins = bins_of(&s, 0);
    let thick: Vec<&(i64, f64)> = bins.iter().filter(|(_, secs)| *secs >= MIN_BIN_SECS).collect();
    let thin: Vec<&(i64, f64)> = bins.iter().filter(|(_, secs)| *secs < MIN_BIN_SECS).collect();
    assert!(!thin.is_empty() && thick.len() >= 5, "предпосылка: корзины {bins:?}");
    let ratios: Vec<i64> = points.iter().map(|e| i64::from(e.ratio)).collect();
    assert_eq!(
        ratios,
        thick.iter().map(|b| b.0).collect::<Vec<_>>(),
        "точки — у корзин с минутой полной нагрузки"
    );
    for (e, &&(r, secs)) in points.iter().zip(&thick) {
        let x = f64::from(i32::try_from(r).expect("множитель"));
        let truth = table(x) + HISTORY.iter().sum::<f64>() * cube(x);
        assert!(close(e.heat, truth, 0.03), "×{r}: нагрев {} вместо {truth}", e.heat);
        assert!(close(e.value, x / e.heat, 1e-9), "{e:?}");
        assert!(e.lo <= e.value + 1e-12 && e.value <= e.hi + 1e-12, "{e:?}");
        assert!(
            close(e.secs * e.load, secs, 0.5),
            "×{r}: занятость × время {} вместо {secs}",
            e.secs * e.load
        );
        assert!(
            close(e.value, x / truth, 0.01 * e.value),
            "×{r}: {} вместо {}",
            e.value,
            x / truth
        );
    }
}

#[test]
fn learned_steps_never_fall_whatever_the_truth_and_are_flat_beyond_the_data() {
    // Целые градусы (шум округления). Свой быстрый нагрев: блок 0 — вогнутая ломаная (1,5 °C/ступень до ×44, потом
    // 0,1), блок 1 падает с множителем (20 − 0,5·(r − 36)), блок 2 — парабола с вершиной на ×44, блок 3 — прямая с
    // волной 2·sin r. Все w ≥ 0 — g неубывающая при любых данных; ниже первой корзины 0, выше последней — постоянная;
    // корзины — целые множители годных строк; кривая не падает.
    // Ловит: веса без ограничения; форму, изгибающуюся за краями данных; корзины не по данным; кривую, падающую из-за
    // ступеней.
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
    let s = synth_from(&truths, wide_loads(4000), times(4000, &GAPS), true);
    let model = fit(&[&log_from_csv(&s.csv(None, None))], &s.die);
    for (i, b) in model.blocks.iter().enumerate() {
        let h = &b.shaped.as_ref().expect("слой со ступенями").shape;
        let bins = bins_of(&s, i);
        assert_eq!(
            h.bins,
            bins.iter().map(|b| b.0).collect::<Vec<_>>(),
            "блок {i}: корзины"
        );
        assert!(
            h.weights.len() == h.bins.len() && h.weights.iter().all(|&w| w >= 0.0),
            "блок {i}: веса {:?}",
            h.weights
        );
        let values: Vec<(f64, f64)> = grid(8.0, 80.0, 0.25).into_iter().map(|r| (r, h.at(r))).collect();
        if let Err(why) = rising(&values, 1e-9) {
            panic!("блок {i}: ступени падают: {why}");
        }
        let (first, last) = (h.bins[0], h.bins[h.bins.len() - 1]);
        let (lo, hi) = (
            f64::from(i32::try_from(first).expect("множитель")),
            f64::from(i32::try_from(last).expect("множитель")),
        );
        assert!(
            close(h.at(lo - 1.0), 0.0, 0.0) && close(h.at(8.0), 0.0, 0.0),
            "блок {i}: ниже первой корзины не 0"
        );
        assert!(
            close(h.at(hi + 1.0), h.at(hi), 0.0) && close(h.at(80.0), h.at(hi), 0.0),
            "блок {i}: выше последней корзины не постоянная"
        );
        if let Err(why) = curve_rises(b) {
            panic!("блок {i}: кривая падает: {why}");
        }
    }
}
