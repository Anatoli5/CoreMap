// Модель нагрева блоков по журналу срезов (DECISIONS #39). Бриф — outputs/model-impl-brief-2026-10-16.md; основа —
// совместный вывод консилиума (outputs/model-consilium-2026-10-16.md §2–§4) без слоёв потолка, регулятора и размещения
// нагрузки: они нужны подбору потолков, следующему этапу.
//
// Владелец 16.10: «Она должна отталкиваться от фактических показателей. То есть на каком множителе ядро работает, под
// какой нагрузкой, и каковы остальные условия. То есть окружение и темпоральные, изменение системы во времени»; «потолок
// вообще не должен ебать агрессионную модель» — потолка среди входов нет, только то, что ядро делало и что было вокруг.
//
// Слой L3 — температура блока i в строке t (из него — кривые и «°C на ступень»):
//   T_i(t) = a + β·D_i(t) + Σ_k K_k·own_k(t) + Σ_{j∈соседи} [M_fast·pfast_j(t) + M_4s·ema4_j(t)]
// • p — мощность ядра в долях «полностью занято на ×45»: занятость × (r/45)³ — форма мощности принята кубом, не измерена
//   (поправка 16.10: напряжение ставит сам процессор по множителю и условиям, отдельным входом оно не было, с выученной
//   формой ничего не добавляло — его в модели больше нет); множителя нет и занятость < 0,05 — ядро спало, p = 0; иначе
//   p неизвестна. По p — средние, соседи и разделение мощности; своя быстрая нагрузка — выученная форма (ниже);
// • pfast — средняя строк t и t+1: показание отвечает на нагрузку вокруг своего момента, включая следующую строку
//   (консилиум, S2); own — pfast и шесть скользящих средних p (0,5, 1, 2, 4, 8, 16 с — история удвоением, слова
//   владельца «горизонтально и вертикально, то есть темпорально»);
// • D_i — средняя температура остальных блоков в строке, измеренная: «окружение» (samples::die_level);
// • соседи — те же, что у полос связи на схеме (сбоку или сверху-снизу, die.rs); дальние блоки действуют только через
//   D (консилиум, S3);
// • a свободен, остальные множители ≥ 0 — тепло не охлаждает; отсюда кривая не падает с множителем по построению.
// Поправка 16.10 (владелец о кривых 0.5.4 на своих данных: «график идет там чуть ли не линейно до определенного уровня
// и на каком-то… значении начинает резко уходить… твои линии регрессии… линейно»): форма своей быстрой нагрузки от
// множителя больше не задана (r/45)³, а выучивается — `Hinge`, выпуклая неубывающая ломаная с узлами через 2 ступени
// (`shaped`). Средние и соседи — по прежней p. Прежний вид (`coef`) остаётся для «°C на ватт» (K/c по p).
// Счёт — МНК с неотрицательностью (Лоусон — Хенсон) по накопленным нормальным уравнениям блока; проверка — четыре
// сплошных куска по времени (строки соседних моментов похожи — случайное деление подглядывало бы).
//
// Слой L4 (уровень кристалла от мощности процессора и видеокарты) и проверка мощности по пакету — только в скрытом
// отчёте: основа следующего этапа, не рисуются. Вентилятор не вход: прошивка ставит его по температуре (обратная связь),
// как вход он исказил бы коэффициенты.
//
// Здесь только счёт: ни окна, ни потоков, ни файлов. Строки приходят из того же разбора, что статистика графика
// (samples.rs → store.rs, `Log`); рисует chart.rs; отчёт пишет model_report.rs.
use std::time::{Duration, Instant};

use crate::samples;
use crate::wire::Hello;

/// Множитель, на котором p = занятость (при опорном напряжении).
pub const REFERENCE: f64 = 45.0;
/// Множителя нет и занятость ниже этой — ядро спало: p = 0.
pub const ASLEEP: f64 = 0.05;
/// Скользящие средние своей мощности, с.
pub const TAUS: [f64; 6] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
/// Средняя соседа — 4-секундная (номер в TAUS).
pub const NEIGHBOUR_TAU: usize = 3;
/// Промежуток между строками больше этого (или новый файл) — средние начинаются заново.
pub const GAP_MS: i64 = 1000;
/// Строка годится в счёт, только если от начала средних прошло не меньше этого: самая длинная средняя набралась.
pub const WARM_MS: i64 = 16_000;
/// Меньше стольких минут годных строк у блока — кривой нет (бриф: «less than 10 min… draw no curve»).
pub const MIN_MINUTES: f64 = 10.0;
/// Меньше стольких годных строк (минута по 0,25 с) — коэффициентов нет вовсе: неизвестных до 14, на каждое — не меньше
/// пятнадцати строк.
pub const MIN_ROWS: usize = 240;
/// Блок под нагрузкой — для опорных условий кривой и её сплошного диапазона (та же граница, что у статистики).
const LOADED: f64 = samples::BUSY;
/// Ошибка меряется на средних за 10 с (консилиум, §2 S1); окно идёт в счёт, если в нём не меньше 20 годных строк (5 с).
const WINDOW_MS: i64 = 10_000;
const WINDOW_ROWS: u32 = 20;
/// Куски по времени для проверки.
const FOLDS: usize = 4;
/// Свои члены: pfast и шесть средних.
const OWN: usize = 1 + TAUS.len();
/// «°C на ступень» — разница кривой между r + 0,5 и r − 0,5.
const HALF_STEP: f64 = 0.5;
/// Узлы формы своей нагрузки — через столько ступеней множителя по сплошной части кривой.
const KNOT_STEP: f64 = 2.0;
/// Излом формы (правило отчёта и подсказки, не подгонки): самый большой прирост наклона в узле — не меньше 0,1 °C на
/// ступень при полной своей нагрузке, и наклон после узла хотя бы вдвое больше, чем до.
const KNEE_MIN: f64 = 0.1;
const KNEE_RATIO: f64 = 2.0;
/// В модель идут строки не старше 8 ч до самой новой (по времени строк, через все файлы). Зачем: без границы память и
/// цена подгонки растут с журналом (6 ч строк — 17,5 МБ и 0,2 с на подгонку; 30 дней игры — сотни МБ и секунды каждые
/// 5 с); последние часы — и есть нынешняя обстановка (консилиум: 20–40 мин на обстановку хватает). 8 ч — около 115 тыс.
/// строк, ~23 МБ.
pub const KEEP_MS: i64 = 8 * 3600 * 1000;
/// Сплошная часть кривой — множители между 5-м и 95-м процентилями по времени (редкие крайние строки — не опыт блока).
const SOLID: [(u64, u64); 2] = [(5, 100), (95, 100)];

// ----- мощность и средние -----

/// Мощность ядра в долях «полностью занято на ×45»: p = занятость × (r/45)³ — форма мощности принята кубом, не измерена.
/// Множителя нет и занятость < `ASLEEP` — ядро спало, p = 0. Иначе — неизвестно.
#[must_use]
pub fn proxy(ratio: Option<f64>, busy: Option<f64>) -> Option<f64> {
    let busy = busy?;
    match ratio {
        Some(r) => {
            let k = r.max(0.0) / REFERENCE;
            Some(busy * k * k * k)
        }
        None if busy < ASLEEP => Some(0.0),
        None => None,
    }
}

/// Шесть скользящих средних мощности (TAUS) с временем последней строки и начала.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Averages {
    pub values: [f64; 6],
    last: i64,
    since: i64,
}

impl Averages {
    #[must_use]
    pub fn start(at_ms: i64, p: f64) -> Averages {
        Averages {
            values: [p; 6],
            last: at_ms,
            since: at_ms,
        }
    }

    /// Следующая строка: промежуток больше `GAP_MS` или время не позже прошлого — начать заново (true); иначе каждая
    /// средняя идёт к p на долю 1 − e^(−Δt/τ).
    pub fn advance(&mut self, at_ms: i64, p: f64) -> bool {
        let dt = at_ms - self.last;
        if dt > GAP_MS || dt <= 0 {
            *self = Averages::start(at_ms, p);
            return true;
        }
        let secs = millis(dt) / 1000.0;
        for (v, tau) in self.values.iter_mut().zip(TAUS) {
            *v += (1.0 - (-secs / tau).exp()) * (p - *v);
        }
        self.last = at_ms;
        false
    }

    /// Когда средние начались (мс).
    #[must_use]
    pub fn since(&self) -> i64 {
        self.since
    }
}

// ----- строки файла -----

const RATIO: usize = 0;
const BUSY: usize = 1;
const TEMP: usize = 2;
/// Мощности строки в `Log::package`: процессор целиком (MSR 0x611), видеокарта, только ядра (MSR 0x639, с 0.5.4).
pub const POWER_CPU: usize = 0;
pub const POWER_GPU: usize = 1;
pub const POWER_CORES: usize = 2;

/// Строки одного файла срезов по порядку файла — как их оставляет разбор (`samples::Scratch::count_keeping`), второго
/// чтения файла нет. Только строки по 0,25 с (время с миллисекундами): прежние по 2 с отстоят друг от друга больше
/// `GAP_MS`, каждая начинала бы средние заново, и ни одна не годилась бы в счёт. Числа — f32 (в файле у них 2–3 знака),
/// пусто — NaN: множитель, занятость и температура блока и мощности процессора, видеокарты и ядер строки — у 275HX
/// 164 байта на строку, 2,4 МБ на час журнала. Напряжение (`<блок>.volts` журналов 0.5.4) разбор отдаёт, но оно не
/// хранится: модель его не берёт. Строки раньше `from` не берутся (граница `KEEP_MS`, `trim`).
#[derive(Clone, Debug, Default)]
pub struct Log {
    names: Vec<String>,
    times: Vec<i64>,
    cells: Vec<[f32; 3]>,
    package: Vec<[f32; 3]>,
    from: Option<i64>,
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "logged values have 2-3 decimals; f32 keeps 7 significant digits"
)]
fn stored(v: Option<f64>) -> f32 {
    v.map_or(f32::NAN, |v| v as f32)
}

fn loaded(v: f32) -> Option<f64> {
    (!v.is_nan()).then_some(f64::from(v))
}

impl Log {
    /// Блоки файла по порядку его столбцов.
    #[must_use]
    pub fn new(names: Vec<String>) -> Log {
        Log {
            names,
            ..Log::default()
        }
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.times.len()
    }

    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Сколько памяти держат строки, байт.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.times.len() * 8 + self.cells.len() * 12 + self.package.len() * 12
    }

    /// Строка: время (мс, `samples::row_time`), по блоку — множитель, занятость, температура, напряжение (не хранится); мощность
    /// процессора (корпус), видеокарты и ядер процессора (`POWER_CPU`, `POWER_GPU`, `POWER_CORES`). Строка раньше
    /// границы (`admit_from`) не берётся.
    pub fn push(&mut self, at: i64, cells: impl Iterator<Item = [Option<f64>; 4]>, package: [Option<f64>; 3]) {
        if self.from.is_some_and(|from| at < from) {
            return;
        }
        let before = self.cells.len();
        self.cells
            .extend(cells.map(|[ratio, busy, temp, _volts]| [ratio, busy, temp].map(stored)));
        // Разбор отдаёт значения каждого блока заголовка — ни больше, ни меньше; иначе строки поедут.
        debug_assert_eq!(
            self.cells.len() - before,
            self.names.len(),
            "блоков в строке — как в заголовке"
        );
        self.times.push(at);
        self.package.push(package.map(stored));
    }

    /// Время последней строки (мс).
    #[must_use]
    pub fn newest(&self) -> Option<i64> {
        self.times.last().copied()
    }

    /// Строки раньше `at` дальше не брать (граница только растёт); взятые не трогаются. Дёшево: разбор свежего файла
    /// после самого нового не держит строки, которые выбросит следующая подгонка.
    pub fn admit_from(&mut self, at: i64) {
        self.from = Some(self.from.map_or(at, |from| from.max(at)));
    }

    /// `admit_from` и выбросить уже взятые строки раньше `at` (порядок остальных тот же). Сдвигает все строки — зовётся
    /// перед подгонкой, не на каждый проход.
    pub fn keep_from(&mut self, at: i64) {
        self.admit_from(at);
        let from = at;
        if self.times.iter().all(|&t| t >= from) {
            return;
        }
        let width = self.names.len();
        let mut kept = 0;
        for row in 0..self.times.len() {
            if self.times[row] < from {
                continue;
            }
            if kept != row {
                self.times[kept] = self.times[row];
                self.package[kept] = self.package[row];
                self.cells.copy_within(row * width..(row + 1) * width, kept * width);
            }
            kept += 1;
        }
        self.times.truncate(kept);
        self.package.truncate(kept);
        self.cells.truncate(kept * width);
    }

    fn get(&self, row: usize, block: usize, what: usize) -> Option<f64> {
        loaded(self.cells[row * self.names.len() + block][what])
    }

    fn power(&self, row: usize, which: usize) -> Option<f64> {
        loaded(self.package[row][which])
    }
}

/// Граница `KEEP_MS`: самая новая строка всех журналов минус 8 ч; у каждого журнала строки раньше неё выброшены и не
/// берутся дальше. Возвращает границу; None — строк нет ни у одного (ничего не менялось).
pub fn trim(logs: &mut [&mut Log]) -> Option<i64> {
    let newest = logs.iter().filter_map(|l| l.newest()).max()?;
    let cutoff = newest - KEEP_MS;
    for log in logs.iter_mut() {
        log.keep_from(cutoff);
    }
    Some(cutoff)
}

/// Целый CSV (заголовок и строки) через разбор окна — для тестов.
#[cfg(test)]
#[must_use]
pub fn log_from_csv(text: &str) -> Log {
    let (header, body) = text.split_once('\n').unwrap_or((text, ""));
    let Some(columns) = samples::Columns::parse(header) else {
        return Log::default();
    };
    let mut log = Log::new(columns.names().map(str::to_string).collect());
    let mut tally = samples::Tally::default();
    samples::Scratch::default().count_keeping(&columns, body, &mut None, &mut tally, Some(&mut log));
    log
}

// ----- кристалл -----

/// Блоки модели: названия (как в заголовке срезов), кластер ли E-ядер, соседи.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Die {
    pub names: Vec<String>,
    /// Кластер E-ядер: его p — по первому ядру, а ядер в нём четыре.
    pub e: Vec<bool>,
    /// По блоку — номера соседей сбоку и сверху-снизу.
    pub neighbours: Vec<Vec<usize>>,
}

impl Die {
    /// Из знакомства: блоки по порядку знакомства, соседи — те же пары, что у полос связи на схеме (`die::neighbours`).
    #[must_use]
    pub fn of(hello: &Hello) -> Die {
        let n = hello.blocks.len();
        let mut neighbours = vec![Vec::new(); n];
        for (a, b) in crate::die::neighbours(hello) {
            if a < n && b < n {
                neighbours[a].push(b);
                neighbours[b].push(a);
            }
        }
        for list in &mut neighbours {
            list.sort_unstable();
            list.dedup();
        }
        Die {
            names: hello.blocks.iter().map(|b| b.name.clone()).collect(), // a dozen names, once per hello
            e: hello.blocks.iter().map(|b| b.kind == "E").collect(),
            neighbours,
        }
    }

    fn len(&self) -> usize {
        self.names.len()
    }
}

#[expect(clippy::cast_possible_truncation, reason = "multipliers are 0..80, rounded first")]
fn whole(r: f64) -> i64 {
    r.round() as i64
}

// ----- форма своей нагрузки -----

/// Форма своей быстрой нагрузки от фактического множителя, выученная по журналу: g(r) = w₀ + w₁·(r − lo) +
/// `Σ w_{1+j}·max(0, r − k_j)`, все w ≥ 0 — растёт и выпукла по построению и может показать излом; за узлами — последний
/// наклон (ниже lo — w₁). В уравнении блока каждый столбец умножен на занятость (`basis`), так что g — нагрев при полной
/// своей нагрузке.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hinge {
    pub lo: f64,
    pub knots: Vec<f64>,
    /// [w₀, w₁, w по узлам]; пусто — весов нет (g ≡ 0, прежний вид `coef`).
    pub weights: Vec<f64>,
}

/// Излом формы: узел и наклоны сразу до и после него, °C на ступень при полной своей нагрузке.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Knee {
    pub at: f64,
    pub before: f64,
    pub after: f64,
}

impl Hinge {
    /// Узлы по сплошной части [lo, hi]: целые множители floor(lo) + 2, + 4 …, у каждого выше — хотя бы ступень
    /// наблюдённых множителей (k ≤ hi − 1): иначе наклон после узла задают единицы строк, и он уходит в пунктир (так
    /// было у блока 1 на журнале 15.10: узел ×42 при hi ×42,4 — 8,5 °C на ступень).
    fn layout(lo: f64, hi: f64) -> Hinge {
        let mut knots = Vec::new();
        let mut k = lo.floor() + KNOT_STEP;
        while k <= hi - KNOT_STEP / 2.0 {
            knots.push(k);
            k += KNOT_STEP;
        }
        Hinge {
            lo,
            knots,
            weights: Vec::new(),
        }
    }

    /// Столбцов у формы в уравнении блока.
    fn columns(&self) -> usize {
        2 + self.knots.len()
    }

    /// Столбцы формы строки: `[busy, busy·(r − lo), busy·max(0, r − k_j)…]` — в `out`.
    fn basis(&self, ratio: f64, busy: f64, out: &mut [f64]) {
        out[0] = busy;
        out[1] = busy * (ratio - self.lo);
        for (o, k) in out[2..].iter_mut().zip(&self.knots) {
            *o = busy * (ratio - k).max(0.0);
        }
    }

    /// g(r).
    #[must_use]
    pub fn at(&self, ratio: f64) -> f64 {
        let Some((&w0, rest)) = self.weights.split_first() else {
            return 0.0;
        };
        let Some((&w1, bends)) = rest.split_first() else {
            return w0;
        };
        w0 + w1 * (ratio - self.lo)
            + bends
                .iter()
                .zip(&self.knots)
                .map(|(w, k)| w * (ratio - k).max(0.0))
                .sum::<f64>()
    }

    /// Наклон g на множителе r (узлы строго левее r).
    #[must_use]
    pub fn slope_at(&self, ratio: f64) -> f64 {
        let w1 = self.weights.get(1).copied().unwrap_or(0.0);
        w1 + self
            .weights
            .iter()
            .skip(2)
            .zip(&self.knots)
            .filter(|&(_, &k)| k < ratio)
            .map(|(w, _)| w)
            .sum::<f64>()
    }

    /// Излом: узел с самым большим приростом наклона, если прирост не меньше `KNEE_MIN` и наклон после него хотя бы в
    /// `KNEE_RATIO` раза больше, чем до; иначе — нет.
    #[must_use]
    pub fn knee(&self) -> Option<Knee> {
        let (j, &w) = self
            .weights
            .iter()
            .skip(2)
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))?;
        let at = *self.knots.get(j)?;
        let before = self.slope_at(at);
        let after = before + w;
        (w >= KNEE_MIN && after >= KNEE_RATIO * before).then_some(Knee { at, before, after })
    }
}

// ----- МНК с неотрицательностью -----

/// min ‖Xc − y‖² при c ≥ 0 по нормальным уравнениям: `gram` = `XᵀX` (n × n, по строкам), `rhs` = `Xᵀy`. Лоусон — Хенсон
/// (активное множество). Свободного члена здесь нет — его убирает центрирование (`Sums::solve`).
#[must_use]
#[expect(
    clippy::many_single_char_names,
    reason = "Lawson-Hanson in the textbook's names: x, w, z, t, n"
)]
pub fn nnls(gram: &[f64], rhs: &[f64]) -> Vec<f64> {
    let n = rhs.len();
    debug_assert_eq!(gram.len(), n * n, "матрица n × n");
    let mut x = vec![0.0; n];
    let mut passive = vec![false; n];
    let scale = rhs.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
    let tol = 1e-12 * scale * (1.0 + gram.iter().fold(0.0_f64, |m, v| m.max(v.abs())));
    // Каждый внешний шаг переводит одну переменную в свободные; внутренние возвращают хотя бы одну назад — 3n с запасом.
    for _ in 0..3 * n.max(1) {
        let w = gradient(gram, rhs, &x);
        let Some(t) = (0..n)
            .filter(|&j| !passive[j] && w[j] > tol)
            .max_by(|&a, &b| w[a].total_cmp(&w[b]))
        else {
            break;
        };
        passive[t] = true;
        for _ in 0..3 * n.max(1) {
            let z = solve_on(gram, rhs, &passive);
            if (0..n).all(|j| !passive[j] || z[j] > 0.0) {
                x = z;
                break;
            }
            // Шаг к z, пока первая из свободных не упрётся в ноль; упёршиеся — обратно в связанные.
            let mut alpha = 1.0_f64;
            for j in 0..n {
                if passive[j] && z[j] <= 0.0 {
                    let denom = x[j] - z[j];
                    if denom > 0.0 {
                        alpha = alpha.min(x[j] / denom);
                    }
                }
            }
            for j in 0..n {
                x[j] += alpha * (z[j] - x[j]);
                if passive[j] && x[j] <= tol.max(f64::MIN_POSITIVE) {
                    x[j] = 0.0;
                    passive[j] = false;
                }
            }
        }
    }
    x
}

/// w = rhs − gram·x.
fn gradient(gram: &[f64], rhs: &[f64], x: &[f64]) -> Vec<f64> {
    let n = rhs.len();
    (0..n)
        .map(|i| rhs[i] - (0..n).map(|j| gram[i * n + j] * x[j]).sum::<f64>())
        .collect()
}

/// Решение по свободным переменным (остальные — 0): Холецкий с ничтожной добавкой к диагонали (1e-12 её величины) —
/// она держит решение определённым, когда столбцы почти совпадают (средние 0,5 и 1 с близки), и меняет его на
/// относительные 1e-12.
#[expect(clippy::many_single_char_names, reason = "matrix algebra in the textbook's names")]
fn solve_on(gram: &[f64], rhs: &[f64], passive: &[bool]) -> Vec<f64> {
    let n = rhs.len();
    let free: Vec<usize> = (0..n).filter(|&j| passive[j]).collect();
    let k = free.len();
    let mut a = vec![0.0; k * k];
    let mut b = vec![0.0; k];
    for (r, &i) in free.iter().enumerate() {
        b[r] = rhs[i];
        for (c, &j) in free.iter().enumerate() {
            a[r * k + c] = gram[i * n + j];
        }
    }
    let ridge = 1e-12 * (0..k).map(|i| a[i * k + i].abs()).fold(f64::MIN_POSITIVE, f64::max);
    for i in 0..k {
        a[i * k + i] += ridge;
    }
    let z = cholesky_solve(&mut a, &b, k);
    let mut out = vec![0.0; n];
    for (r, &i) in free.iter().enumerate() {
        out[i] = z[r];
    }
    out
}

/// A x = b для симметричной A (k × k) разложением Холецкого на месте; неположительный ведущий элемент (столбец целиком
/// зависит от прежних) — эта переменная 0.
#[expect(clippy::many_single_char_names, reason = "Cholesky in the textbook's names")]
fn cholesky_solve(a: &mut [f64], b: &[f64], k: usize) -> Vec<f64> {
    let mut dead = vec![false; k];
    for j in 0..k {
        let mut d = a[j * k + j];
        for p in 0..j {
            d -= a[j * k + p] * a[j * k + p];
        }
        if d <= 1e-14 * a[j * k + j].abs().max(f64::MIN_POSITIVE) || !d.is_finite() {
            dead[j] = true;
            a[j * k + j] = 1.0;
            for i in j + 1..k {
                a[i * k + j] = 0.0;
            }
            continue;
        }
        let d = d.sqrt();
        a[j * k + j] = d;
        for i in j + 1..k {
            let mut s = a[i * k + j];
            for p in 0..j {
                s -= a[i * k + p] * a[j * k + p];
            }
            a[i * k + j] = s / d;
        }
    }
    // L y = b, затем Lᵀ x = y.
    let mut y = vec![0.0; k];
    for i in 0..k {
        if dead[i] {
            continue;
        }
        let mut s = b[i];
        for p in 0..i {
            s -= a[i * k + p] * y[p];
        }
        y[i] = s / a[i * k + i];
    }
    let mut x = vec![0.0; k];
    for i in (0..k).rev() {
        if dead[i] {
            continue;
        }
        let mut s = y[i];
        for p in i + 1..k {
            s -= a[p * k + i] * x[p];
        }
        x[i] = s / a[i * k + i];
    }
    x
}

// ----- нормальные уравнения -----

/// Накопленные суммы одной задачи: строк, Σx, Σxxᵀ (верхний треугольник), Σxy, Σy, Σy².
#[derive(Clone, Debug)]
struct Sums {
    m: usize,
    n: f64,
    s: Vec<f64>,
    g: Vec<f64>,
    b: Vec<f64>,
    sy: f64,
    syy: f64,
}

impl Sums {
    fn new(m: usize) -> Sums {
        Sums {
            m,
            n: 0.0,
            s: vec![0.0; m],
            g: vec![0.0; m * m],
            b: vec![0.0; m],
            sy: 0.0,
            syy: 0.0,
        }
    }

    fn add(&mut self, x: &[f64], y: f64) {
        let m = self.m;
        self.n += 1.0;
        self.sy += y;
        self.syy += y * y;
        for i in 0..m {
            let xi = x[i];
            self.s[i] += xi;
            self.b[i] += xi * y;
            let row = &mut self.g[i * m..(i + 1) * m];
            for j in i..m {
                row[j] += xi * x[j];
            }
        }
    }

    fn plus(&mut self, other: &Sums) {
        self.n += other.n;
        self.sy += other.sy;
        self.syy += other.syy;
        for (a, b) in self.s.iter_mut().zip(&other.s) {
            *a += b;
        }
        for (a, b) in self.g.iter_mut().zip(&other.g) {
            *a += b;
        }
        for (a, b) in self.b.iter_mut().zip(&other.b) {
            *a += b;
        }
    }

    fn minus(&self, other: &Sums) -> Sums {
        let mut out = self.clone(); // one small matrix per fold, a few per fit
        out.n -= other.n;
        out.sy -= other.sy;
        out.syy -= other.syy;
        for (a, b) in out.s.iter_mut().zip(&other.s) {
            *a -= b;
        }
        for (a, b) in out.g.iter_mut().zip(&other.g) {
            *a -= b;
        }
        for (a, b) in out.b.iter_mut().zip(&other.b) {
            *a -= b;
        }
        out
    }

    /// МНК со свободным членом по столбцам `columns` (остальные — 0), их множители ≥ 0: суммы центрируются (свободный
    /// член уходит), столбцы приводятся к единичной дисперсии (обусловленность), затем `nnls`. None — строк меньше двух.
    fn solve(&self, columns: &[usize]) -> Option<Solved> {
        self.solve_how(columns, How::LAYER)
    }

    /// То же с выбором `How`: закреплённый множитель столбца 0 (уровня кристалла) — он тогда не в `columns`, отклик —
    /// y − β·x₀; без неотрицательности — обычный МНК (только диагностика отчёта).
    fn solve_how(&self, columns: &[usize], how: How) -> Option<Solved> {
        if self.n < 2.0 {
            return None;
        }
        let m = self.m;
        let k = columns.len();
        let fixed = how.beta.unwrap_or(0.0);
        let sy = self.sy - fixed * self.s[0];
        let mut gram = vec![0.0; k * k];
        let mut rhs = vec![0.0; k];
        let mut scale = vec![0.0; k];
        for (r, &i) in columns.iter().enumerate() {
            let var = self.g[i * m + i] - self.s[i] * self.s[i] / self.n;
            scale[r] = if var > 0.0 { var.sqrt() } else { 0.0 };
        }
        for (r, &i) in columns.iter().enumerate() {
            if scale[r] <= 0.0 {
                continue;
            }
            // Σ x_i·(y − β·x₀): x₀·x_i — из верхнего треугольника (i ≥ 0).
            let b = self.b[i] - fixed * self.g[i];
            rhs[r] = (b - self.s[i] * sy / self.n) / scale[r];
            for (c, &j) in columns.iter().enumerate() {
                if scale[c] <= 0.0 {
                    continue;
                }
                let (lo, hi) = if i <= j { (i, j) } else { (j, i) };
                gram[r * k + c] = (self.g[lo * m + hi] - self.s[i] * self.s[j] / self.n) / (scale[r] * scale[c]);
            }
        }
        let solved = if how.signed {
            solve_on(&gram, &rhs, &vec![true; k])
        } else {
            nnls(&gram, &rhs)
        };
        let mut coef = vec![0.0; m];
        for (r, &i) in columns.iter().enumerate() {
            if scale[r] > 0.0 {
                coef[i] = solved[r] / scale[r];
            }
        }
        if how.beta.is_some() {
            coef[0] = fixed;
        }
        let a = (self.sy - (0..m).map(|i| coef[i] * self.s[i]).sum::<f64>()) / self.n;
        Some((a, coef))
    }
}

/// Как решать задачу блока.
#[derive(Clone, Copy, Debug)]
struct How {
    /// Без неотрицательности (обычный МНК) — только диагностика отчёта.
    signed: bool,
    /// Множитель уровня кристалла закреплён (диагностика: β = 1), иначе подбирается.
    beta: Option<f64>,
}

impl How {
    /// Слой L3 по брифу: все множители, кроме свободного члена, ≥ 0, β подбирается.
    const LAYER: How = How {
        signed: false,
        beta: None,
    };
}

/// Решение одной задачи: свободный член и множители всех столбцов (не взятые — 0).
type Solved = (f64, Vec<f64>);
/// Решения пяти видов: (a) D, (b) + свои, (c) + соседи — слой L3 с выученной формой; (d) — диагностика отчёта: как (c),
/// но β = 1 закреплён (уровень кристалла переходит в температуру блока целиком); (e) — прежний вид (c): своя быстрая
/// нагрузка — K·p, без формы (для «°C на ватт» и сравнения в отчёте).
type Variants = [Option<Solved>; VARIANTS];
const VARIANTS: usize = 5;
const DIE_ONLY: usize = 0;
const WITH_OWN: usize = 1;
const FULL: usize = 2;
const UNIT: usize = 3;
const PROXY: usize = 4;

/// Предсказание: a + c·x.
fn predict(fit: &Solved, x: &[f64]) -> f64 {
    fit.0 + fit.1.iter().zip(x).map(|(c, v)| c * v).sum::<f64>()
}

// ----- проход по строкам -----

/// Годная строка блока: что о ней нужно счёту. x: [D, pfast, 6 средних, по соседу pfast и 4-с средняя, столбцы формы].
struct Usable<'a> {
    block: usize,
    /// Строка по порядку всех файлов.
    index: usize,
    log: usize,
    at: i64,
    /// Мс от прошлой строки (строка годна не раньше 16 с от начала средних, так что прошлая — в том же куске).
    span: i64,
    x: &'a [f64],
    y: f64,
    busy: f64,
    ratio: Option<f64>,
    /// `p_j(t)` соседей по порядку `Die::neighbours`.
    near: &'a [f64],
}

/// Всё о строке, что нужно, чтобы выдать её годной, когда придёт следующая (pfast берёт строку t+1).
#[derive(Clone, Debug, Default)]
struct Snap {
    at: i64,
    gap: i64,
    index: usize,
    p: Vec<Option<f64>>,
    ema: Vec<[f64; 6]>,
    since: Vec<Option<i64>>,
    temp: Vec<Option<f64>>,
    level: Vec<Option<f64>>,
    ratio: Vec<Option<f64>>,
    busy: Vec<Option<f64>>,
}

impl Snap {
    fn new(n: usize) -> Snap {
        Snap {
            p: vec![None; n],
            ema: vec![[0.0; 6]; n],
            since: vec![None; n],
            temp: vec![None; n],
            level: vec![None; n],
            ratio: vec![None; n],
            busy: vec![None; n],
            ..Snap::default()
        }
    }
}

/// Файлы по времени первой строки (пустые — не нужны).
fn in_order<'a>(logs: &[&'a Log]) -> Vec<&'a Log> {
    let mut out: Vec<&Log> = logs.iter().copied().filter(|l| l.rows() > 0).collect();
    out.sort_by_key(|l| l.times[0]);
    out
}

/// Столбец файла для каждого блока модели (по названию); блока в файле нет — None.
fn columns_of(log: &Log, die: &Die) -> Vec<Option<usize>> {
    die.names
        .iter()
        .map(|name| log.names.iter().position(|n| n == name))
        .collect()
}

/// Проход по строкам всех файлов: средние, уровень кристалла, и годные строки блоков — в `emit`.
struct Walk<'a> {
    die: &'a Die,
    /// Узлы формы своей нагрузки по блоку (без весов).
    shapes: &'a [Hinge],
}

/// Рабочие строки прохода — одни на весь проход: входы строки, p соседей, столбцы формы строк t и t+1.
struct Buffers {
    x: Vec<f64>,
    near: Vec<f64>,
    now: Vec<f64>,
    then: Vec<f64>,
}

impl Walk<'_> {
    fn run(&self, logs: &[&Log], mut emit: impl FnMut(&Usable)) {
        let n = self.die.len();
        let mut prev = Snap::new(n);
        let mut cur = Snap::new(n);
        let width = (0..n).map(|i| width_of(self.die, i, self.shapes)).max().unwrap_or(1);
        let hinge = self.shapes.iter().map(Hinge::columns).max().unwrap_or(2);
        let mut buffers = Buffers {
            x: vec![0.0; width],
            near: vec![0.0; width],
            now: vec![0.0; hinge],
            then: vec![0.0; hinge],
        };
        let mut index = 0;
        for (number, log) in logs.iter().enumerate() {
            let cols = columns_of(log, self.die);
            // Новый файл — средние заново.
            let mut averages: Vec<Option<Averages>> = vec![None; n];
            for row in 0..log.rows() {
                self.snap(log, row, &cols, &mut averages, &mut cur);
                cur.index = index;
                index += 1;
                if row > 0 {
                    cur.gap = cur.at - prev.at;
                    self.emit_row(&prev, &cur, number, &mut buffers, &mut emit);
                } else {
                    cur.gap = 0;
                }
                std::mem::swap(&mut prev, &mut cur);
            }
        }
    }

    /// Значения строки и средние после неё.
    fn snap(&self, log: &Log, row: usize, cols: &[Option<usize>], averages: &mut [Option<Averages>], into: &mut Snap) {
        let at = log.times[row];
        into.at = at;
        let n = self.die.len();
        let (mut sum, mut known) = (0.0, 0);
        for i in 0..n {
            let value = |what| cols[i].and_then(|c| log.get(row, c, what));
            let (ratio, busy, temp) = (value(RATIO), value(BUSY), value(TEMP));
            let power = proxy(ratio, busy);
            into.ratio[i] = ratio;
            into.busy[i] = busy;
            into.temp[i] = temp;
            into.p[i] = power;
            // p неизвестна — средние блока прерываются и начнутся заново с первой известной.
            averages[i] = match (power, averages[i]) {
                (Some(w), Some(mut a)) => {
                    a.advance(at, w);
                    Some(a)
                }
                (Some(w), None) => Some(Averages::start(at, w)),
                (None, _) => None,
            };
            into.ema[i] = averages[i].map_or([0.0; 6], |a| a.values);
            into.since[i] = averages[i].map(|a| a.since());
            if let Some(t) = temp {
                sum += t;
                known += 1;
            }
        }
        for i in 0..n {
            into.level[i] = into.temp[i].and_then(|t| samples::die_level(sum - t, known - 1, n - 1));
        }
    }

    /// Строка `prev` с её следующей `next`: годные строки блоков — в `emit`.
    fn emit_row(&self, prev: &Snap, next: &Snap, log: usize, buffers: &mut Buffers, emit: &mut impl FnMut(&Usable)) {
        if !(next.gap > 0 && next.gap <= GAP_MS) {
            return;
        }
        let Buffers { x, near, now, then } = buffers;
        let warm = |j: usize| prev.since[j].is_some_and(|s| prev.at - s >= WARM_MS);
        let fast = |j: usize| Some(f64::midpoint(prev.p[j]?, next.p[j]?));
        for i in 0..self.die.len() {
            let (Some(y), Some(level), Some(own), Some(busy)) = (prev.temp[i], prev.level[i], fast(i), prev.busy[i])
            else {
                continue;
            };
            if !warm(i) {
                continue;
            }
            x[0] = level;
            x[1] = own;
            x[2..2 + TAUS.len()].copy_from_slice(&prev.ema[i]);
            let mut ok = true;
            for (q, &j) in self.die.neighbours[i].iter().enumerate() {
                let (Some(f), Some(p)) = (fast(j), prev.p[j]) else {
                    ok = false;
                    break;
                };
                if !warm(j) {
                    ok = false;
                    break;
                }
                x[OWN + 1 + 2 * q] = f;
                x[OWN + 2 + 2 * q] = prev.ema[j][NEIGHBOUR_TAU];
                near[q] = p;
            }
            if !ok {
                continue;
            }
            // Форма своей нагрузки — тоже средняя строк t и t+1; спавшее ядро (множителя нет, p = 0) — нули.
            let shape = &self.shapes[i];
            let at = OWN + 1 + 2 * self.die.neighbours[i].len();
            let columns = shape.columns();
            for (snap, into) in [(prev, &mut *now), (next, &mut *then)] {
                match (snap.ratio[i], snap.busy[i]) {
                    (Some(r), Some(b)) => shape.basis(r, b, into),
                    _ => into[..columns].fill(0.0),
                }
            }
            for m in 0..columns {
                x[at + m] = f64::midpoint(now[m], then[m]);
            }
            let m = width_of(self.die, i, self.shapes);
            emit(&Usable {
                block: i,
                index: prev.index,
                log,
                at: prev.at,
                span: prev.gap,
                x: &x[..m],
                y,
                busy,
                ratio: prev.ratio[i],
                near: &near[..self.die.neighbours[i].len()],
            });
        }
    }
}

/// Сколько входов у блока: D, семь своих, по два на соседа, столбцы формы.
fn width_of(die: &Die, block: usize, shapes: &[Hinge]) -> usize {
    1 + OWN + 2 * die.neighbours[block].len() + shapes[block].columns()
}

/// Столбцы вида `v` у блока с `nb` соседями и формой из `hc` столбцов. Порядок входов: [D | pfast | 6 средних | соседи |
/// форма].
fn variant_columns(v: usize, nb: usize, hc: usize) -> Vec<usize> {
    let shape = OWN + 1 + 2 * nb..OWN + 1 + 2 * nb + hc;
    let near = OWN + 1..OWN + 1 + 2 * nb;
    let ema = 2..=OWN;
    match v {
        DIE_ONLY => vec![0],
        WITH_OWN => std::iter::once(0).chain(ema).chain(shape).collect(),
        FULL => std::iter::once(0).chain(ema).chain(near).chain(shape).collect(),
        UNIT => ema.chain(near).chain(shape).collect(),
        _ => (0..OWN + 1 + 2 * nb).collect(),
    }
}

// ----- итог -----

/// Коэффициенты слоя L3 блока.
#[derive(Clone, Debug, PartialEq)]
pub struct Coef {
    pub a: f64,
    /// На уровень кристалла D.
    pub beta: f64,
    /// [pfast, средние 0,5, 1, 2, 4, 8, 16 с].
    pub own: [f64; 7],
    /// По соседу, в порядке `Die::neighbours`.
    pub neighbours: Vec<Near>,
    /// Выученная форма своей быстрой нагрузки; у прежнего вида (`BlockModel::coef`) весов нет.
    pub shape: Hinge,
}

impl Coef {
    #[must_use]
    pub fn own_total(&self) -> f64 {
        self.own.iter().sum()
    }

    /// Коэффициенты решения; `layout` — узлы формы блока: с ними (`shaped`) веса формы берутся из решения, без них —
    /// форма пустая (прежний вид).
    fn of(die: &Die, block: usize, layout: Option<&Hinge>, fit: &Solved) -> Coef {
        let c = &fit.1;
        let mut own = [0.0; 7];
        own.copy_from_slice(&c[1..=OWN]);
        let at = OWN + 1 + 2 * die.neighbours[block].len();
        let shape = layout.map_or_else(Hinge::default, |l| Hinge {
            lo: l.lo,
            knots: l.knots.clone(), // a dozen knots per solved block
            weights: c[at..at + l.columns()].to_vec(),
        });
        Coef {
            shape,
            a: fit.0,
            beta: c[0],
            own,
            neighbours: die.neighbours[block]
                .iter()
                .enumerate()
                .map(|(q, &j)| Near {
                    block: j,
                    fast: c[OWN + 1 + 2 * q],
                    slow: c[OWN + 2 + 2 * q],
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Near {
    pub block: usize,
    pub fast: f64,
    pub slow: f64,
}

/// Опорные условия кривой — по годным строкам блока под нагрузкой (занятость ≥ 0,5).
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// Медиана уровня кристалла D.
    pub die: f64,
    /// По соседу: (номер, медиана его p).
    pub neighbours: Vec<(usize, f64)>,
    /// Наблюдённые множители — сплошная часть кривой: 5-й и 95-й процентили по времени (`SOLID`); медиана — по строкам.
    pub lo: f64,
    pub hi: f64,
    pub median: f64,
    /// Строк под нагрузкой.
    pub rows: usize,
}

/// Ошибка на отложенных данных: средние за 10 с температуры над кристаллом (T − D), °C (корень из средней квадратов):
/// (a) только смещение и наклон на D, (b) + свои, (c) + соседи (полный L3 с выученной формой).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Errors {
    pub die_only: f64,
    pub own: f64,
    pub full: f64,
    /// Диагностика: (c) при β = 1.
    pub unit: f64,
    /// (c) прежнего вида — своя быстрая нагрузка K·p без выученной формы.
    pub proxy: f64,
    pub windows: usize,
}

/// Диагностика отчёта (окно её не показывает): L3 без неотрицательности и L3 при β = 1 — по всем данным.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnosis {
    pub signed: Coef,
    pub unit: Coef,
}

/// «°C на ступень» при полной своей нагрузке на медиане множителя под нагрузкой: по всем данным и разброс по четырём
/// подгонкам без одного куска.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slope {
    pub at: f64,
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockModel {
    pub name: String,
    pub e: bool,
    /// Годных строк для полного L3 и их время, мин.
    pub usable: usize,
    pub minutes: f64,
    /// Прежний вид L3: своя быстрая нагрузка — K·p (p — куб, принят, не измерен). Из него — «°C на ватт» (K/c).
    pub coef: Option<Coef>,
    /// L3 с выученной формой своей быстрой нагрузки: из него кривая, «°C на ступень», излом.
    pub shaped: Option<Coef>,
    /// Всегда 0: напряжения в модели больше нет (поправка 16.10), «без напряжения» не бывает. Поле ещё читает подсказка
    /// блока (chart.rs) — убрать вместе с её строкой.
    pub fallback: f64,
    pub reference: Option<Reference>,
    pub errors: Option<Errors>,
    pub slope: Option<Slope>,
    /// Отношение большего суммарного своего коэффициента к меньшему у половин данных (стабильность).
    pub halves: Option<f64>,
    pub diagnosis: Option<Diagnosis>,
    /// Мощность блока из слоя разделения мощности; None — строк с мощностью нет.
    pub power: Option<BlockPower>,
    /// «°C на ватт»: тепловое сопротивление; None — не считается (мощности нет или она блока не определена).
    pub resistance: Option<Resistance>,
    /// Опорная мощность для кривой «одинаковая работа»: медиана ватт на единицу p у блоков того же рода; None — кривая
    /// по нагрузке этого ядра (запасной вид).
    pub same_work: Option<f64>,
}

/// «°C на ватт» = суммарный свой коэффициент / ватт блока на единицу p; разброс — из четырёх кусков обоих слоёв
/// (наименьший коэффициент на наибольшую мощность … наибольший на наименьшую): куски слоёв — разные строки, попарно
/// их не сопоставить.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resistance {
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
}

/// Мощность блока: ватт на единицу его p (у кластера E — за четыре ядра) по всем строкам с мощностью и разброс по
/// четырём подгонкам без куска; определена ли она отдельно от других блоков.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockPower {
    pub watts: f64,
    pub lo: f64,
    pub hi: f64,
    pub determined: bool,
    /// Доля изменчивости p блока, которую объясняют p остальных (R², 1 − 1/VIF): близко к 1 — нагружен вместе с ними.
    pub collinear: f64,
    /// Блок, с которым p этого блока связана сильнее всего, и коэффициент корреляции.
    pub partner: Option<(usize, f64)>,
}

/// Слой разделения мощности: `P(t) = c0 + Σ_j c_j·p_j(t)`, `c_j ≥ 0`, p по строке (у мощности нет тепловой задержки),
/// кластер E — ×4. По `power_cores` (только ядра), а где его нет во всех строках — по `power_cpu` (корпус).
#[derive(Clone, Debug, PartialEq)]
pub struct PowerSplit {
    /// Какой столбец: `power_cores` или `power_cpu`.
    pub column: &'static str,
    pub rows: usize,
    pub minutes: f64,
    pub c0: f64,
    /// Ошибка на отложенных кусках, Вт на строку.
    pub rmse: f64,
}

impl BlockModel {
    /// Своя p при полной своей нагрузке на этом множителе: (r/45)³ (форма мощности принята кубом, не измерена).
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "a block's power at full load; since the voltage left (16.10) every block has the same cube"
    )]
    pub fn power_at(&self, ratio: f64) -> f64 {
        proxy(Some(ratio), Some(1.0)).unwrap_or(0.0)
    }

    /// Свой нагрев при полной своей нагрузке на множителе r по коэффициентам `c`: форма + средние × p(r) (у прежнего
    /// вида формы нет, и это K·p(r)).
    #[must_use]
    pub fn heat(&self, c: &Coef, ratio: f64) -> f64 {
        c.shape.at(ratio) + c.own_total() * self.power_at(ratio)
    }

    /// Предсказанная температура на этом фактическом множителе при опорных условиях (своя нагрузка 100 %, соседи и
    /// кристалл — медианы), абсолютная — по L3 с выученной формой.
    #[must_use]
    pub fn curve(&self, ratio: f64) -> Option<f64> {
        let c = self.shaped.as_ref()?;
        let r = self.reference.as_ref()?;
        Some(self.curve_with(c, r, ratio))
    }

    /// То же по прежнему виду (K·p) — для сравнения в отчёте.
    #[must_use]
    pub fn proxy_curve(&self, ratio: f64) -> Option<f64> {
        let c = self.coef.as_ref()?;
        let r = self.reference.as_ref()?;
        Some(self.curve_with(c, r, ratio))
    }

    fn curve_with(&self, c: &Coef, r: &Reference, ratio: f64) -> f64 {
        let near: f64 = c
            .neighbours
            .iter()
            .zip(&r.neighbours)
            .map(|(n, &(_, p))| (n.fast + n.slow) * p)
            .sum();
        c.a + c.beta * r.die + self.heat(c, ratio) + near
    }

    /// Кривая графика: при «одинаковой работе» (есть «°C на ватт» и опорная мощность рода) свой нагрев блока
    /// пересчитан на опорные ватты рода — `curve + (w_ref/w − 1)·heat(r)`, сравнима между блоками того же рода; иначе —
    /// `curve` (нагрузка этого ядра в этой игре).
    #[must_use]
    pub fn shown_curve(&self, ratio: f64) -> Option<f64> {
        let t = self.curve(ratio)?;
        let c = self.shaped.as_ref()?;
        Some(match (self.resistance, self.same_work, self.power) {
            (Some(_), Some(w), Some(p)) if p.watts > 0.0 => t + (w / p.watts - 1.0) * self.heat(c, ratio),
            _ => t,
        })
    }

    /// На сколько градусов выше на ступень множителя (r ± 0,5) при коэффициентах `c`.
    fn step(&self, c: &Coef, ratio: f64) -> f64 {
        self.heat(c, ratio + HALF_STEP) - self.heat(c, ratio - HALF_STEP)
    }

    /// Излом выученной формы (правило — `Hinge::knee`).
    #[must_use]
    pub fn knee(&self) -> Option<Knee> {
        self.shaped.as_ref()?.shape.knee()
    }

    /// Кривую рисовать: есть коэффициенты, опорные условия и не меньше `MIN_MINUTES` данных.
    #[must_use]
    pub fn drawn(&self) -> bool {
        self.shaped.is_some() && self.reference.is_some() && self.minutes >= MIN_MINUTES
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub blocks: Vec<BlockModel>,
    /// Строк во всех файлах (по 0,25 с).
    pub rows: usize,
    /// Слой разделения мощности; None — строк с мощностью процессора нет (или их мало для четырёх кусков).
    pub power: Option<PowerSplit>,
    pub time: Duration,
}

// ----- подгонка -----

/// Что копится по блоку в первом проходе, кроме сумм.
#[derive(Debug, Default)]
struct Gather {
    usable: usize,
    span_ms: i64,
    /// Строки под нагрузкой: D, множитель, его время (мс от прошлой строки: множитель строки — средний за этот
    /// промежуток), p соседей (по соседу).
    level: Vec<f64>,
    ratio: Vec<f64>,
    weight: Vec<i64>,
    near: Vec<Vec<f64>>,
}

/// Окно 10 с одного блока: строк, Σ(T − D), Σ предсказанных (T̂ − D) по трём видам.
#[derive(Debug, Default, Clone, Copy)]
struct Window {
    key: (usize, i64),
    n: u32,
    obs: f64,
    pred: [f64; VARIANTS],
}

/// Ошибки по окнам: сумма квадратов по трём видам и число окон.
#[derive(Debug, Default, Clone, Copy)]
struct Tally3 {
    sse: [f64; VARIANTS],
    windows: usize,
}

impl Tally3 {
    fn close(&mut self, w: &Window) {
        if w.n < WINDOW_ROWS {
            return;
        }
        let n = f64::from(w.n);
        for v in 0..VARIANTS {
            let d = w.pred[v] / n - w.obs / n;
            self.sse[v] += d * d;
        }
        self.windows += 1;
    }
}

/// Решение вида `v` блока с `nb` соседями (`variant_columns`); (d) — при β = 1.
fn solve_variant(sums: &Sums, v: usize, nb: usize) -> Option<Solved> {
    let columns = variant_columns(v, nb, sums.m - (OWN + 1 + 2 * nb));
    if v == UNIT {
        sums.solve_how(
            &columns,
            How {
                signed: false,
                beta: Some(1.0),
            },
        )
    } else {
        sums.solve(&columns)
    }
}

/// Подгонка по строкам файлов (все файлы — один период: тот, что показывает график).
#[must_use]
pub fn fit(logs: &[&Log], die: &Die) -> Model {
    let started = Instant::now();
    let logs = in_order(logs);
    let rows: usize = logs.iter().map(|l| l.rows()).sum();
    let n = die.len();
    // Проход 0: годные строки и опорные условия — по ним узлы формы (сплошная часть кривой).
    let (gather, references, shapes) = prepare(&logs, die, |_| true);
    let walk = Walk { die, shapes: &shapes };

    // Проход 1: суммы по кускам.
    let mut folds: Vec<Vec<Sums>> = (0..n)
        .map(|i| vec![Sums::new(width_of(die, i, &shapes)); FOLDS])
        .collect();
    walk.run(&logs, |u| folds[u.block][fold_of(u.index, rows)].add(u.x, u.y));

    // Решения: по всем данным, без каждого куска (пять видов), по половинам.
    let mut blocks: Vec<BlockModel> = Vec::with_capacity(n);
    let mut held: Vec<Vec<Variants>> = Vec::with_capacity(n);
    for (i, ((sums, g), reference)) in folds.iter().zip(&gather).zip(references).enumerate() {
        let (block, fold_fits) = solve_block(
            die,
            i,
            sums,
            (g, &shapes[i]),
            BlockModel {
                name: die.names[i].clone(), // a dozen names per fit
                e: die.e[i],
                usable: g.usable,
                minutes: millis(g.span_ms) / 60_000.0,
                coef: None,
                shaped: None,
                fallback: 0.0,
                reference,
                errors: None,
                slope: None,
                halves: None,
                diagnosis: None,
                power: None,
                resistance: None,
                same_work: None,
            },
        );
        blocks.push(block);
        held.push(fold_fits);
    }

    held_out(&walk, &logs, rows, &held, &mut blocks);
    let power = split_power(&logs, die, &mut blocks, &held);
    Model {
        blocks,
        rows,
        power,
        time: started.elapsed(),
    }
}

/// Годные строки блоков (`Gather`), опорные условия и узлы формы по строкам, которые берёт `keep` (по времени строки):
/// проход с пустыми формами — годность строки от формы не зависит.
fn prepare(logs: &[&Log], die: &Die, keep: impl Fn(i64) -> bool) -> (Vec<Gather>, Vec<Option<Reference>>, Vec<Hinge>) {
    let n = die.len();
    let flat = vec![Hinge::default(); n];
    let walk = Walk { die, shapes: &flat };
    let mut gather: Vec<Gather> = (0..n)
        .map(|i| Gather {
            near: vec![Vec::new(); die.neighbours[i].len()],
            ..Gather::default()
        })
        .collect();
    walk.run(logs, |u| {
        if !keep(u.at) {
            return;
        }
        let g = &mut gather[u.block];
        g.usable += 1;
        g.span_ms += u.span;
        if u.busy >= LOADED
            && let Some(r) = u.ratio
        {
            g.level.push(u.x[0]);
            g.ratio.push(r);
            g.weight.push(u.span);
            for (into, &p) in g.near.iter_mut().zip(u.near) {
                into.push(p);
            }
        }
    });
    let references: Vec<Option<Reference>> = gather
        .iter()
        .zip(&die.neighbours)
        .map(|(g, near)| reference_of(g, near))
        .collect();
    let shapes = references
        .iter()
        .map(|r| r.as_ref().map_or_else(Hinge::default, |r| Hinge::layout(r.lo, r.hi)))
        .collect();
    (gather, references, shapes)
}

/// Слой разделения мощности и из него — «°C на ватт» блоков и опорная мощность кривой «одинаковая работа».
fn split_power(logs: &[&Log], die: &Die, blocks: &mut [BlockModel], held: &[Vec<Variants>]) -> Option<PowerSplit> {
    let n = die.len();
    // Один столбец на все строки: ядра и корпус — разные величины (корпус — ещё и некоторые ватты вне ядер), смесь
    // исказила бы c0 и c_j.
    let cores = logs
        .iter()
        .any(|l| (0..l.rows()).any(|r| l.power(r, POWER_CORES).is_some()));
    let which = if cores { POWER_CORES } else { POWER_CPU };
    let mut table = Table {
        m: n,
        ..Table::default()
    };
    let mut x = vec![0.0; n];
    let mut span_ms = 0;
    for log in logs {
        let cols = columns_of(log, die);
        let mut last: Option<i64> = None;
        for row in 0..log.rows() {
            let at = log.times[row];
            let gap = last.map(|l| at - l);
            last = Some(at);
            let Some(power) = log.power(row, which) else { continue };
            let mut known = true;
            for i in 0..n {
                let value = |what| cols[i].and_then(|c| log.get(row, c, what));
                let Some(p) = proxy(value(RATIO), value(BUSY)) else {
                    known = false;
                    break;
                };
                x[i] = p * if die.e[i] { 4.0 } else { 1.0 };
            }
            if known {
                table.push(&x, power, at);
                span_ms += gap.filter(|&g| g > 0 && g <= GAP_MS).unwrap_or(0);
            }
        }
    }
    let cross = table.cross()?;
    let links = collinearity(&table);
    for (i, block) in blocks.iter_mut().enumerate() {
        let weight = if die.e[i] { 4.0 } else { 1.0 };
        let folds: Vec<f64> = cross.folds.iter().filter_map(|f| f.as_ref().map(|c| c[i])).collect();
        let lo = folds.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = folds.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // Определена — все четыре подгонки без куска дают её больше нуля и не расходятся больше чем вдвое, и p блока не
        // объясняется p остальных (R² < 0,9, то есть VIF < 10 — обычное правило регрессии): иначе данные различают
        // только сумму с блоками, нагруженными вместе с ним (самый связанный — `partner`). Одной устойчивости кусков
        // мало: при совсем одинаковых столбцах решение делит ватты поровну в каждом куске — устойчиво и выдумано.
        let (collinear, partner) = links[i];
        let determined = folds.len() == FOLDS && lo > 0.0 && hi <= 2.0 * lo && collinear < 0.9;
        block.power = Some(BlockPower {
            watts: cross.c[i] * weight,
            lo: lo * weight,
            hi: hi * weight,
            determined,
            collinear,
            partner,
        });
    }
    for (i, block) in blocks.iter_mut().enumerate() {
        let (Some(coef), Some(power)) = (&block.coef, block.power) else {
            continue;
        };
        if !power.determined {
            continue;
        }
        let totals: Vec<f64> = held[i]
            .iter()
            .filter_map(|f| f[PROXY].as_ref())
            .map(|f| f.1[1..=OWN].iter().sum::<f64>())
            .collect();
        let k_lo = totals.iter().copied().fold(coef.own_total(), f64::min);
        let k_hi = totals.iter().copied().fold(coef.own_total(), f64::max);
        // Ватты по всем данным могут лечь вне разброса кусков (у МНК с неотрицательностью так бывает: 5,79 при кусках
        // 5,96…10,66 у блока 0 на его журнале) — разброс берётся с ними вместе, иначе число выпало бы из своего же
        // промежутка.
        let (w_lo, w_hi) = (power.lo.min(power.watts), power.hi.max(power.watts));
        block.resistance = Some(Resistance {
            value: coef.own_total() / power.watts,
            lo: k_lo / w_hi,
            hi: k_hi / w_lo,
        });
    }
    // Опорная мощность «одинаковой работы» — медиана ватт у блоков того же рода с «°C на ватт».
    for kind in [false, true] {
        let watts: Vec<f64> = blocks
            .iter()
            .filter(|b| b.e == kind && b.resistance.is_some())
            .filter_map(|b| b.power.map(|p| p.watts))
            .collect();
        let reference = median(&watts);
        for b in blocks.iter_mut().filter(|b| b.e == kind && b.resistance.is_some()) {
            b.same_work = reference;
        }
    }
    Some(PowerSplit {
        column: if cores { "power_cores" } else { "power_cpu" },
        rows: cross.rows,
        minutes: millis(span_ms) / 60_000.0,
        c0: cross.a,
        rmse: cross.rmse_rows,
    })
}

/// По каждому столбцу таблицы: доля его изменчивости, которую объясняют остальные (R² = 1 − 1/VIF по обратной
/// корреляционной матрице), и самый связанный с ним столбец с коэффициентом корреляции. Столбец без изменчивости —
/// R² = 1 (о нём строки ничего не говорят).
fn collinearity(table: &Table) -> Vec<(f64, Option<(usize, f64)>)> {
    let (m, rows) = (table.m, table.y.len());
    let mut mean = vec![0.0; m];
    for k in 0..rows {
        for (a, v) in mean.iter_mut().zip(table.row(k)) {
            *a += v;
        }
    }
    for a in &mut mean {
        *a /= count(rows.max(1));
    }
    let mut cov = vec![0.0; m * m];
    for k in 0..rows {
        let r = table.row(k);
        for i in 0..m {
            for j in 0..m {
                cov[i * m + j] += (r[i] - mean[i]) * (r[j] - mean[j]);
            }
        }
    }
    let sd: Vec<f64> = (0..m).map(|i| cov[i * m + i].max(0.0).sqrt()).collect();
    let live: Vec<usize> = (0..m).filter(|&i| sd[i] > 0.0).collect();
    let k = live.len();
    let mut corr = vec![0.0; k * k];
    for (r, &i) in live.iter().enumerate() {
        for (c, &j) in live.iter().enumerate() {
            corr[r * k + c] = cov[i * m + j] / (sd[i] * sd[j]);
        }
    }
    let all = vec![true; k];
    let mut out = vec![(1.0, None); m];
    for (r, &i) in live.iter().enumerate() {
        let mut unit = vec![0.0; k];
        unit[r] = 1.0;
        let column = solve_on(&corr, &unit, &all);
        let vif = column[r];
        let partner = live
            .iter()
            .enumerate()
            .filter(|&(c, _)| c != r)
            .map(|(c, &j)| (j, corr[r * k + c]))
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
        out[i] = (if vif > 1.0 { 1.0 - 1.0 / vif } else { 0.0 }, partner);
    }
    out
}

/// Проход 2: предсказания на отложенных кусках — ошибки средних за 10 с у каждого блока.
fn held_out(walk: &Walk, logs: &[&Log], rows: usize, held: &[Vec<Variants>], blocks: &mut [BlockModel]) {
    let n = blocks.len();
    let mut windows: Vec<Window> = vec![Window::default(); n];
    let mut tallies: Vec<Tally3> = vec![Tally3::default(); n];
    walk.run(logs, |u| {
        let k = fold_of(u.index, rows);
        let fits = &held[u.block][k];
        if fits.iter().any(Option::is_none) {
            return;
        }
        let key = (k, u.at.div_euclid(WINDOW_MS));
        let w = &mut windows[u.block];
        if w.key != key || w.n == 0 {
            tallies[u.block].close(w);
            *w = Window {
                key,
                ..Window::default()
            };
        }
        let level = u.x[0];
        w.n += 1;
        w.obs += u.y - level;
        for (v, fit) in fits.iter().enumerate() {
            if let Some(f) = fit {
                w.pred[v] += predict(f, u.x) - level;
            }
        }
    });
    for ((block, w), t) in blocks.iter_mut().zip(&windows).zip(&mut tallies) {
        t.close(w);
        if t.windows > 0 {
            let rms = |v: usize| (t.sse[v] / count(t.windows)).sqrt();
            block.errors = Some(Errors {
                die_only: rms(DIE_ONLY),
                own: rms(WITH_OWN),
                full: rms(FULL),
                unit: rms(UNIT),
                proxy: rms(PROXY),
                windows: t.windows,
            });
        }
    }
}

/// Решения блока: по всем данным (прежний вид — `coef`, с выученной формой — `shaped`), без каждого куска (пять видов —
/// для ошибок и разброса «°C на ступень»), по половинам (стабильность K прежнего вида). `block` приходит без них и уходит
/// с ними; `gathered` — его годные строки и узлы формы.
fn solve_block(
    die: &Die,
    i: usize,
    sums: &[Sums],
    (gathered, shape): (&Gather, &Hinge),
    mut block: BlockModel,
) -> (BlockModel, Vec<Variants>) {
    let nb = die.neighbours[i].len();
    let m = 1 + OWN + 2 * nb + shape.columns();
    let mut all = Sums::new(m);
    for s in sums {
        all.plus(s);
    }
    let enough = gathered.usable >= MIN_ROWS;
    block.coef = enough
        .then(|| solve_variant(&all, PROXY, nb))
        .flatten()
        .map(|f| Coef::of(die, i, None, &f));
    block.shaped = enough
        .then(|| solve_variant(&all, FULL, nb))
        .flatten()
        .map(|f| Coef::of(die, i, Some(shape), &f));
    // Диагностика отчёта: L3 с формой без неотрицательности и при β = 1.
    block.diagnosis = enough
        .then(|| {
            let signed = all.solve_how(
                &variant_columns(FULL, nb, shape.columns()),
                How {
                    signed: true,
                    beta: None,
                },
            )?;
            Some(Diagnosis {
                signed: Coef::of(die, i, Some(shape), &signed),
                unit: Coef::of(die, i, Some(shape), &solve_variant(&all, UNIT, nb)?),
            })
        })
        .flatten();
    let fold_fits: Vec<Variants> = sums
        .iter()
        .map(|s| {
            let train = all.minus(s);
            let ok = enough && s.n > 0.0;
            std::array::from_fn(|v| if ok { solve_variant(&train, v, nb) } else { None })
        })
        .collect();
    block.halves = enough
        .then(|| {
            let mut first = sums[0].clone(); // one small matrix per block
            first.plus(&sums[1]);
            let second = all.minus(&first);
            let total = |s: &Sums| solve_variant(s, PROXY, nb).map(|f| Coef::of(die, i, None, &f).own_total());
            let (early, late) = (total(&first)?, total(&second)?);
            (early.min(late) > 0.0).then(|| early.max(late) / early.min(late))
        })
        .flatten();
    block.slope = slope_of(&block, die, i, shape, &fold_fits);
    (block, fold_fits)
}

/// Кусок строки по её порядку среди всех строк: четыре сплошных по времени.
fn fold_of(index: usize, rows: usize) -> usize {
    (index * FOLDS / rows.max(1)).min(FOLDS - 1)
}

/// Опорные условия из строк блока под нагрузкой; None — таких строк нет.
fn reference_of(g: &Gather, neighbours: &[usize]) -> Option<Reference> {
    if g.ratio.is_empty() {
        return None;
    }
    let [lo, hi] = SOLID.map(|share| by_time(&g.ratio, &g.weight, share));
    Some(Reference {
        die: median(&g.level)?,
        neighbours: neighbours
            .iter()
            .zip(&g.near)
            .map(|(&j, values)| Some((j, median(values)?)))
            .collect::<Option<Vec<_>>>()?,
        lo: lo?,
        hi: hi?,
        median: median(&g.ratio)?,
        rows: g.ratio.len(),
    })
}

/// «°C на ступень» по выученной форме: по всем данным и по четырём подгонкам без куска.
fn slope_of(block: &BlockModel, die: &Die, i: usize, shape: &Hinge, folds: &[Variants]) -> Option<Slope> {
    let (coef, reference) = (block.shaped.as_ref()?, block.reference.as_ref()?);
    let at = reference.median;
    let per: Vec<f64> = folds
        .iter()
        .filter_map(|f| f[FULL].as_ref())
        .map(|f| block.step(&Coef::of(die, i, Some(shape), f), at))
        .collect();
    let value = block.step(coef, at);
    Some(Slope {
        at,
        value,
        lo: per.iter().copied().fold(value, f64::min),
        hi: per.iter().copied().fold(value, f64::max),
    })
}

/// Процентиль по времени — то же правило, что у линии 95 % графика (`samples::Hist::quantile`): наименьшее значение v,
/// при котором время строк со значением не выше v — не меньше доли `share` всего времени. Всегда значение одной из
/// строк. None — строк нет или всё время нулевое.
fn by_time(values: &[f64], weights: &[i64], share: (u64, u64)) -> Option<f64> {
    let mut order: Vec<(f64, u64)> = values
        .iter()
        .zip(weights)
        .map(|(&v, &w)| (v, u64::try_from(w).unwrap_or(0)))
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: u64 = order.iter().map(|o| o.1).sum();
    if total == 0 || share.1 == 0 {
        return None;
    }
    let mut below = 0u64;
    for &(v, w) in &order {
        below += w;
        if below * share.1 >= total * share.0 {
            return Some(v);
        }
    }
    order.last().map(|o| o.0)
}

/// Медиана (у чётного числа — средняя двух средних).
fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec(); // one column of loaded rows per block, once per fit
    sorted.sort_unstable_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        sorted[mid - 1].midpoint(sorted[mid])
    })
}

// ----- сырые корзины (только отчёт) -----

/// Сырая корзина: строк, средняя температура блока над остальными (T − D, то же правило уровня кристалла) и средний D —
/// в строках, где блок занят не меньше заданного, по целому фактическому множителю.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawBin {
    pub ratio: i64,
    pub rows: usize,
    pub above: f64,
    pub level: f64,
}

/// Сырые корзины блока `block` для сравнения с выученной формой в отчёте.
#[must_use]
pub fn relative_bins(logs: &[&Log], die: &Die, block: usize, busy: f64) -> Vec<RawBin> {
    let n = die.len();
    let mut bins: std::collections::BTreeMap<i64, (usize, f64, f64)> = std::collections::BTreeMap::new();
    for log in in_order(logs) {
        let cols = columns_of(log, die);
        for row in 0..log.rows() {
            let value = |at: usize, what: usize| cols[at].and_then(|c| log.get(row, c, what));
            let (Some(temp), Some(ratio), Some(load)) = (value(block, TEMP), value(block, RATIO), value(block, BUSY))
            else {
                continue;
            };
            if load < busy {
                continue;
            }
            let (sum, known) = (0..n)
                .filter(|&other| other != block)
                .filter_map(|other| value(other, TEMP))
                .fold((0.0, 0), |(sum, known), v| (sum + v, known + 1));
            let Some(level) = samples::die_level(sum, known, n - 1) else {
                continue;
            };
            let bin = bins.entry(whole(ratio)).or_default();
            bin.0 += 1;
            bin.1 += temp - level;
            bin.2 += level;
        }
    }
    bins.into_iter()
        .map(|(ratio, (k, above, level))| RawBin {
            ratio,
            rows: k,
            above: above / count(k),
            level: level / count(k),
        })
        .collect()
}

// ----- проверка по моменту, когда он сменил потолок (только отчёт) -----

/// Предсказание после момента `at` по подгонке до него: для каждого блока — наблюдённая средняя T − D до и после (в
/// файле, где этот момент), предсказанная после по трём видам.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitBlock {
    pub name: String,
    pub train: usize,
    pub test: usize,
    pub before: Option<f64>,
    pub after: Option<f64>,
    /// Предсказанная средняя после по видам (a)–(e).
    pub predicted: [Option<f64>; VARIANTS],
    /// Из чего сложено предсказанное после (средняя по проверочным строкам), по (c) и (c old): свободный член + (β − 1)·D,
    /// своя быстрая нагрузка (форма или K·pfast), средние своей p, соседи; и средняя своя занятость и множитель там.
    pub parts: [[f64; 4]; 2],
    pub busy_test: Option<f64>,
    pub ratio_test: Option<f64>,
    /// β подгонки (c) до момента и средний уровень кристалла D на строках подгонки и на проверочных.
    pub beta: Option<f64>,
    pub level_train: Option<f64>,
    pub level_test: Option<f64>,
}

#[must_use]
pub fn split(logs: &[&Log], die: &Die, at_ms: i64) -> Vec<SplitBlock> {
    let logs = in_order(logs);
    let n = die.len();
    // Узлы формы — по строкам подгонки (до момента).
    let (_, _, shapes) = prepare(&logs, die, |at| at < at_ms);
    let walk = Walk { die, shapes: &shapes };
    let home = logs
        .iter()
        .position(|l| l.times[0] <= at_ms && at_ms <= l.times[l.rows() - 1]);
    let mut train: Vec<Sums> = (0..n).map(|i| Sums::new(width_of(die, i, &shapes))).collect();
    let mut trained = vec![0usize; n];
    let mut level_train = vec![0.0; n];
    let mut before = vec![(0.0, 0usize); n];
    walk.run(&logs, |u| {
        if u.at < at_ms {
            train[u.block].add(u.x, u.y);
            trained[u.block] += 1;
            level_train[u.block] += u.x[0];
            if Some(u.log) == home {
                before[u.block].0 += u.y - u.x[0];
                before[u.block].1 += 1;
            }
        }
    });
    let fits: Vec<Variants> = train
        .iter()
        .enumerate()
        .map(|(i, s)| {
            std::array::from_fn(|v| {
                if trained[i] >= MIN_ROWS {
                    solve_variant(s, v, die.neighbours[i].len())
                } else {
                    None
                }
            })
        })
        .collect();
    let mut after = vec![(0.0, [0.0; VARIANTS], 0usize); n];
    let mut level_test = vec![0.0; n];
    let mut parts = vec![[[0.0; 4]; 2]; n];
    let mut load = vec![(0.0, 0.0, 0usize); n];
    walk.run(&logs, |u| {
        if u.at >= at_ms && Some(u.log) == home {
            let nb = die.neighbours[u.block].len();
            for (into, v) in parts[u.block].iter_mut().zip([FULL, PROXY]) {
                if let Some((a, c)) = &fits[u.block][v] {
                    let sum = |r: std::ops::Range<usize>| r.map(|k| c[k] * u.x[k]).sum::<f64>();
                    let near = OWN + 1 + 2 * nb;
                    into[0] += a + (c[0] - 1.0) * u.x[0];
                    into[1] += sum(1..2) + sum(near..u.x.len());
                    into[2] += sum(2..OWN + 1);
                    into[3] += sum(OWN + 1..near);
                }
            }
            let l = &mut load[u.block];
            l.0 += u.busy;
            if let Some(r) = u.ratio {
                l.1 += r;
                l.2 += 1;
            }
            let a = &mut after[u.block];
            let level = u.x[0];
            level_test[u.block] += level;
            a.0 += u.y - level;
            for (v, fit) in fits[u.block].iter().enumerate() {
                if let Some(f) = fit {
                    a.1[v] += predict(f, u.x) - level;
                }
            }
            a.2 += 1;
        }
    });
    (0..n)
        .map(|i| {
            let mean = |sum: f64, k: usize| (k > 0).then(|| sum / count(k));
            let (obs, pred, k) = after[i];
            SplitBlock {
                name: die.names[i].clone(), // a dozen names, report only
                train: trained[i],
                test: k,
                before: mean(before[i].0, before[i].1),
                after: mean(obs, k),
                predicted: std::array::from_fn(|v| fits[i][v].as_ref().and(mean(pred[v], k))),
                parts: parts[i].map(|p| p.map(|v| mean(v, k).unwrap_or(0.0))),
                busy_test: mean(load[i].0, k),
                ratio_test: mean(load[i].1, load[i].2),
                beta: fits[i][FULL].as_ref().map(|f| f.1[0]),
                level_train: mean(level_train[i], trained[i]),
                level_test: mean(level_test[i], k),
            }
        })
        .collect()
}

// ----- слой L4 и проверка мощности (только отчёт) -----

/// Скользящие средние уровня кристалла, с: 4 … 1024 удвоением.
pub const LEVEL_TAUS: [f64; 9] = [4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0];

/// Подгонка по строкам в памяти (только отчёт: строк с мощностью — тысячи) с проверкой четырьмя кусками.
#[derive(Clone, Debug, PartialEq)]
pub struct Cross {
    pub a: f64,
    pub c: Vec<f64>,
    /// Множители подгонок без каждого куска (None — кусок не решился).
    pub folds: Vec<Option<Vec<f64>>>,
    pub rows: usize,
    /// Отложенные: по строкам и по средним за 10 с; доля дисперсии средних за 10 с, которую модель объясняет.
    pub rmse_rows: f64,
    pub rmse_windows: f64,
    pub explained: f64,
}

/// Строки задачи: входы (по `m` на строку), отклик, время.
#[derive(Debug, Default)]
struct Table {
    m: usize,
    x: Vec<f64>,
    y: Vec<f64>,
    at: Vec<i64>,
}

impl Table {
    fn push(&mut self, x: &[f64], y: f64, at: i64) {
        self.x.extend_from_slice(x);
        self.y.push(y);
        self.at.push(at);
    }

    fn row(&self, k: usize) -> &[f64] {
        &self.x[k * self.m..(k + 1) * self.m]
    }

    /// Только столбцы `keep`.
    fn narrow(&self, keep: &[usize]) -> Table {
        let mut out = Table {
            m: keep.len(),
            ..Table::default()
        };
        for k in 0..self.y.len() {
            let row = self.row(k);
            for &c in keep {
                out.x.push(row[c]);
            }
        }
        out.y.clone_from(&self.y);
        out.at.clone_from(&self.at);
        out
    }

    fn cross(&self) -> Option<Cross> {
        let rows = self.y.len();
        if rows < 2 * FOLDS * (self.m + 1) {
            return None;
        }
        let columns: Vec<usize> = (0..self.m).collect();
        let mut parts: Vec<Sums> = vec![Sums::new(self.m); FOLDS];
        for k in 0..rows {
            parts[fold_of(k, rows)].add(self.row(k), self.y[k]);
        }
        let mut all = Sums::new(self.m);
        for p in &parts {
            all.plus(p);
        }
        let fits: Vec<Option<Solved>> = parts.iter().map(|p| all.minus(p).solve(&columns)).collect();
        let (mut sse, mut held) = (0.0, 0usize);
        let mut windows: Vec<(f64, f64)> = Vec::new();
        let mut w = Window::default();
        let close = |w: &Window, out: &mut Vec<(f64, f64)>| {
            if w.n >= WINDOW_ROWS {
                out.push((w.obs / f64::from(w.n), w.pred[0] / f64::from(w.n)));
            }
        };
        for k in 0..rows {
            let f = fold_of(k, rows);
            let Some(fit) = &fits[f] else { continue };
            let pred = predict(fit, self.row(k));
            sse += (pred - self.y[k]).powi(2);
            held += 1;
            let key = (f, self.at[k].div_euclid(WINDOW_MS));
            if key != w.key || w.n == 0 {
                close(&w, &mut windows);
                w = Window {
                    key,
                    ..Window::default()
                };
            }
            w.n += 1;
            w.obs += self.y[k];
            w.pred[0] += pred;
        }
        close(&w, &mut windows);
        let full = all.solve(&columns)?;
        let mean = windows.iter().map(|w| w.0).sum::<f64>() / count(windows.len().max(1));
        let missed: f64 = windows.iter().map(|w| (w.0 - w.1).powi(2)).sum();
        let spread: f64 = windows.iter().map(|w| (w.0 - mean).powi(2)).sum();
        Some(Cross {
            a: full.0,
            c: full.1,
            folds: fits.into_iter().map(|f| f.map(|f| f.1)).collect(),
            rows,
            rmse_rows: (sse / count(held.max(1))).sqrt(),
            rmse_windows: (missed / count(windows.len().max(1))).sqrt(),
            explained: if spread > 0.0 { 1.0 - missed / spread } else { 0.0 },
        })
    }
}

/// Слой L4: средняя температура всех блоков от скользящих средних мощности процессора (и видеокарты).
#[derive(Clone, Debug, PartialEq)]
pub struct Level {
    /// Самый длинный кусок без перерыва (с), использованные средние (с) и разогрев (с).
    pub longest: f64,
    pub taus: Vec<f64>,
    pub warm: f64,
    pub cpu_only: Option<Cross>,
    pub with_gpu: Option<Cross>,
}

/// Средние с произвольным набором времён (L4).
#[derive(Clone, Debug)]
struct Smooth {
    values: Vec<f64>,
    since: i64,
    last: i64,
}

impl Smooth {
    fn start(at: i64, v: f64, k: usize) -> Smooth {
        Smooth {
            values: vec![v; k],
            since: at,
            last: at,
        }
    }

    fn advance(&mut self, at: i64, v: f64, taus: &[f64]) {
        let secs = millis(at - self.last) / 1000.0;
        for (x, tau) in self.values.iter_mut().zip(taus) {
            *x += (1.0 - (-secs / tau).exp()) * (v - *x);
        }
        self.last = at;
    }
}

#[must_use]
pub fn level(logs: &[&Log], die: &Die) -> Option<Level> {
    let logs = in_order(logs);
    let n = die.len();
    // Самый длинный кусок строк с мощностью процессора без перерыва больше секунды.
    let mut longest = 0;
    for log in &logs {
        let mut start: Option<i64> = None;
        let mut last = 0;
        for row in 0..log.rows() {
            let at = log.times[row];
            if log.power(row, POWER_CPU).is_none() || start.is_some_and(|_| at - last > GAP_MS || at <= last) {
                start = None;
            }
            if log.power(row, POWER_CPU).is_some() {
                let s = *start.get_or_insert(at);
                longest = longest.max(at - s);
            }
            last = at;
        }
    }
    let longest_s = millis(longest) / 1000.0;
    let taus: Vec<f64> = LEVEL_TAUS.iter().copied().filter(|&t| t <= longest_s).collect();
    let warm = taus.last().copied()?;
    let warm_ms = whole(warm * 1000.0);
    let k = taus.len();
    let mut table = Table {
        m: 2 * k,
        ..Table::default()
    };
    let mut gpu_seen = false;
    let mut inputs = vec![0.0; 2 * k];
    for log in &logs {
        let cols = columns_of(log, die);
        let mut cpu: Option<Smooth> = None;
        let mut gpu: Option<Smooth> = None;
        let mut held_gpu: Option<f64> = None;
        let mut last = i64::MIN;
        for row in 0..log.rows() {
            let at = log.times[row];
            let Some(power) = log.power(row, POWER_CPU) else {
                (cpu, gpu, held_gpu) = (None, None, None);
                continue;
            };
            if at - last > GAP_MS || at <= last {
                (cpu, gpu, held_gpu) = (None, None, None);
            }
            last = at;
            match &mut cpu {
                Some(s) => s.advance(at, power, &taus),
                None => cpu = Some(Smooth::start(at, power, k)),
            }
            if let Some(g) = log.power(row, POWER_GPU) {
                held_gpu = Some(g);
                gpu_seen = true;
            }
            if let Some(g) = held_gpu {
                match &mut gpu {
                    Some(s) => s.advance(at, g, &taus),
                    None => gpu = Some(Smooth::start(at, g, k)),
                }
            }
            let temps: Option<Vec<f64>> = (0..n).map(|i| cols[i].and_then(|c| log.get(row, c, TEMP))).collect();
            let (Some(temps), Some(c)) = (temps, &cpu) else {
                continue;
            };
            if at - c.since < warm_ms {
                continue;
            }
            inputs[..k].copy_from_slice(&c.values);
            match &gpu {
                Some(g) if at - g.since >= warm_ms => inputs[k..].copy_from_slice(&g.values),
                // Видеокарта ещё не набралась (или не читается): строка без неё — отдельно отметится.
                _ => inputs[k..].iter_mut().for_each(|v| *v = f64::NAN),
            }
            table.push(&inputs, temps.iter().sum::<f64>() / count(n), at);
        }
    }
    // Сравнение «с видеокартой и без» — на одних и тех же строках: где видеокарта прочитана, если она есть вообще.
    let rows: Vec<usize> = (0..table.y.len())
        .filter(|&r| !gpu_seen || !table.row(r)[k].is_nan())
        .collect();
    let mut same = Table {
        m: 2 * k,
        ..Table::default()
    };
    for &r in &rows {
        same.push(table.row(r), table.y[r], table.at[r]);
    }
    let cpu_columns: Vec<usize> = (0..k).collect();
    Some(Level {
        longest: longest_s,
        taus,
        warm,
        cpu_only: same.narrow(&cpu_columns).cross(),
        with_gpu: if gpu_seen { same.cross() } else { None },
    })
}

/// Проверка мощности: мощность процессора ≈ `c0 + c_P·Σ_P p + c_E·4·Σ_E p` (кластер E — p первого ядра за четыре
/// ядра), p — куб (принят, не измерен).
#[derive(Clone, Debug, PartialEq)]
pub struct PowerCheck {
    /// На всех строках, где p всех блоков известна.
    pub cube: Option<Cross>,
}

#[must_use]
pub fn power_check(logs: &[&Log], die: &Die) -> PowerCheck {
    let logs = in_order(logs);
    let n = die.len();
    let mut cube = Table {
        m: 2,
        ..Table::default()
    };
    for log in &logs {
        let cols = columns_of(log, die);
        for row in 0..log.rows() {
            let Some(power) = log.power(row, POWER_CPU) else {
                continue;
            };
            let mut c = [0.0; 2];
            let mut known = true;
            for i in 0..n {
                let value = |what| cols[i].and_then(|col| log.get(row, col, what));
                let weight = if die.e[i] { 4.0 } else { 1.0 };
                match proxy(value(RATIO), value(BUSY)) {
                    Some(p) => c[usize::from(die.e[i])] += weight * p,
                    None => known = false,
                }
            }
            if known {
                cube.push(&c, power, log.times[row]);
            }
        }
    }
    PowerCheck { cube: cube.cross() }
}

// ----- числа -----

#[expect(clippy::cast_precision_loss, reason = "row counts far below 2^52")]
fn count(n: usize) -> f64 {
    n as f64
}

#[expect(clippy::cast_precision_loss, reason = "milliseconds of a month, far below 2^52")]
fn millis(ms: i64) -> f64 {
    ms as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_solve_recovers_a_plain_line_with_its_intercept() {
        let mut s = Sums::new(2);
        for k in 0..50 {
            let x0 = f64::from(k);
            let x1 = f64::from((k * 7) % 11);
            s.add(&[x0, x1], 3.0 + 0.5 * x0 + 2.0 * x1);
        }
        let (a, c) = s.solve(&[0, 1]).expect("решение");
        assert!(
            (a - 3.0).abs() < 1e-8 && (c[0] - 0.5).abs() < 1e-9 && (c[1] - 2.0).abs() < 1e-9,
            "{a} {c:?}"
        );
        // Только первый столбец: второй — ноль.
        let (_, c) = s.solve(&[0]).expect("решение");
        assert!(c[1].abs() < 1e-15);
    }

    #[test]
    fn a_negative_slope_is_held_at_zero_and_the_intercept_takes_the_mean() {
        let mut s = Sums::new(1);
        for k in 0..20 {
            let x = f64::from(k);
            s.add(&[x], 10.0 - x);
        }
        let (a, c) = s.solve(&[0]).expect("решение");
        assert!(c[0].abs() < 1e-12, "{c:?}");
        assert!((a - (10.0 - 9.5)).abs() < 1e-9, "{a}");
    }

    #[test]
    fn folds_are_four_contiguous_quarters() {
        let got: Vec<usize> = (0..8).map(|k| fold_of(k, 8)).collect();
        assert_eq!(got, [0, 0, 1, 1, 2, 2, 3, 3]);
        assert_eq!(fold_of(0, 0), 0);
    }

    /// Строка генератора: время, множители, занятость, средние и мощность трёх блоков.
    #[derive(Clone, Copy)]
    struct Generated {
        at: i64,
        r: [f64; 3],
        b: [f64; 3],
        ema: [[f64; 6]; 3],
        p: [f64; 3],
    }

    /// Три блока в ряд (0 — 1 — 2), строки по 0,25 с; температуры по форме L3 с известными коэффициентами.
    fn synthetic(rows: u32, gap_at: Option<u32>) -> (String, Die) {
        synthetic_with(rows, gap_at, None, false)
    }

    /// То же; `watts` — мощность ядер в строке: 5 Вт + Σ ватт блока × его p (столбец `power_cores`); `together` — блок
    /// 2 нагружен ровно как блок 1 (та же занятость и множитель).
    fn synthetic_with(rows: u32, gap_at: Option<u32>, watts: Option<[f64; 3]>, together: bool) -> (String, Die) {
        let die = Die {
            names: vec!["0".into(), "1".into(), "2".into()],
            e: vec![false, false, false],
            neighbours: vec![vec![1], vec![0, 2], vec![1]],
        };
        // Нагрузка блока j в строке k: волны разной длины, чтобы средние разных времён не совпадали.
        let busy = |j: u32, k: u32| {
            let t = f64::from(k) * 0.25;
            let wave = |period: f64| (std::f64::consts::TAU * t / period).sin();
            (0.5 + 0.2 * wave(3.0 + f64::from(j)) + 0.2 * wave(37.0 + 5.0 * f64::from(j)) + 0.1 * wave(0.9))
                .clamp(0.0, 1.0)
        };
        let ratio = |j: u32, k: u32| 40.0 + f64::from((k / (40 + 7 * j)) % 6);
        let mut text = String::from("time,0.ratio,0.busy,0.temp,1.ratio,1.busy,1.temp,2.ratio,2.busy,2.temp");
        text.push_str(if watts.is_some() { ",power_cores\n" } else { "\n" });
        let mut at = 0i64;
        let mut averages: Vec<Option<Averages>> = vec![None; 3];
        let mut rowsv: Vec<Generated> = Vec::new();
        for k in 0..rows {
            at += if gap_at == Some(k) { 5000 } else { 250 };
            let mut p = [0.0; 3];
            let mut ema = [[0.0; 6]; 3];
            let mut r = [0.0; 3];
            let mut b = [0.0; 3];
            for j in 0..3u32 {
                let ju = j as usize;
                let source = if together && j == 2 { 1 } else { j };
                r[ju] = ratio(source, k);
                b[ju] = (busy(source, k) * 100.0).round() / 100.0;
                p[ju] = proxy(Some(r[ju]), Some(b[ju])).unwrap_or(0.0);
                averages[ju] = Some(match averages[ju] {
                    Some(mut a) => {
                        a.advance(at, p[ju]);
                        a
                    }
                    None => Averages::start(at, p[ju]),
                });
                ema[ju] = averages[ju].map_or([0.0; 6], |a| a.values);
            }
            rowsv.push(Generated { at, r, b, ema, p });
        }
        // Температура: T_i = 20 + 1.0·D_i + 6·pfast + 9·ema4 + 3·ema16 + 2·pfast соседа + 4·ema4 соседа; D_i — средняя
        // остальных, сама зависит от T — считается по общему уровню: сначала «основа» без D, потом уровень.
        let mut temps: Vec<[f64; 3]> = Vec::new();
        for k in 0..rowsv.len() {
            let next = rowsv.get(k + 1).map_or(rowsv[k].p, |n| n.p);
            let Generated { ema, p, .. } = rowsv[k];
            let fast = |j: usize| f64::midpoint(p[j], next[j]);
            let own = |j: usize| 6.0 * fast(j) + 9.0 * ema[j][3] + 3.0 * ema[j][5];
            let near = |j: usize| 2.0 * fast(j) + 4.0 * ema[j][3];
            let base = [own(0) + near(1), own(1) + near(0) + near(2), own(2) + near(1)];
            // T_i = 20 + D_i + base_i, D_i = (Σ T − T_i)/2 → T_i − (ΣT − T_i)/2 = 20 + base_i; решение для трёх.
            let sum_base: f64 = base.iter().sum();
            // Σ: ΣT − (2ΣT)/2... уровень вырожден при β = 1; берём β = 0,5: T_i = 20 + 0,5·(S − T_i)/2 + base_i.
            // T_i (1 + 0,25) = 20 + 0,25·S + base_i; S·(1,25) = 60 + 0,75·S... → S = (60 + Σbase) / 0,5.
            let s = (60.0 + sum_base) / 0.5;
            temps.push(std::array::from_fn(|i| (20.0 + 0.25 * s + base[i]) / 1.25));
        }
        for (k, Generated { at, r, b, p, .. }) in rowsv.iter().enumerate() {
            let ms = at.rem_euclid(1000);
            let secs = at / 1000;
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("2026-10-15 20:{:02}:{:02}.{ms:03}", secs / 60 % 60, secs % 60),
            );
            for j in 0..3 {
                let _ = std::fmt::Write::write_fmt(&mut text, format_args!(",{:.2},{:.2},{}", r[j], b[j], temps[k][j]));
            }
            if let Some(w) = watts {
                let power = 5.0 + w.iter().zip(p).map(|(w, p)| w * p).sum::<f64>();
                let _ = std::fmt::Write::write_fmt(&mut text, format_args!(",{power}"));
            }
            text.push('\n');
        }
        (text, die)
    }

    #[test]
    fn the_power_split_recovers_known_watts_and_gives_degrees_per_watt() {
        let (csv, die) = synthetic_with(6000, None, Some([10.0, 14.0, 8.0]), false);
        let model = fit(&[&log_from_csv(&csv)], &die);
        let split = model.power.as_ref().expect("слой мощности");
        assert_eq!(split.column, "power_cores");
        assert!((split.c0 - 5.0).abs() < 0.05 && split.rmse < 0.01, "{split:?}");
        for (b, want) in model.blocks.iter().zip([10.0, 14.0, 8.0]) {
            let pw = b.power.expect("мощность блока");
            assert!(
                pw.determined && (pw.watts - want).abs() < 0.05 * want,
                "{} {pw:?}",
                b.name
            );
            let r = b.resistance.expect("°C на ватт");
            let k = b.coef.as_ref().map_or(0.0, Coef::own_total);
            assert!((r.value - k / pw.watts).abs() < 1e-9 && r.lo <= r.value && r.value <= r.hi);
            // Кривая «одинаковой работы»: свой нагрев блока пересчитан на медиану ватт рода (10), остальное — как у
            // кривой L3.
            let reference = b.same_work.unwrap_or(0.0);
            assert!((reference - 10.0).abs() < 0.05, "медиана ватт P-блоков {reference}");
            let shaped = b.shaped.as_ref().expect("форма");
            for x in [36.0, 45.0, 50.0] {
                let (shown, plain) = (b.shown_curve(x).unwrap_or(0.0), b.curve(x).unwrap_or(0.0));
                let want = plain + (reference / pw.watts - 1.0) * b.heat(shaped, x);
                assert!((shown - want).abs() < 1e-9, "{} ×{x}: {shown} {want}", b.name);
            }
        }
    }

    #[test]
    fn blocks_always_loaded_together_get_no_separate_watts() {
        // Блок 2 нагружен ровно как блок 1: данные различают только сумму их ватт — ни у одного «°C на ватт» нет, и
        // каждый назван партнёром другого; блок 0 определён.
        let (csv, die) = synthetic_with(6000, None, Some([10.0, 14.0, 8.0]), true);
        let model = fit(&[&log_from_csv(&csv)], &die);
        let b = &model.blocks;
        let (one, two) = (b[1].power.expect("мощность"), b[2].power.expect("мощность"));
        assert!(!one.determined && !two.determined, "{one:?} {two:?}");
        assert!(one.collinear > 0.99 && one.partner.map(|p| p.0) == Some(2), "{one:?}");
        assert!(b[1].resistance.is_none() && b[2].resistance.is_none());
        assert!(
            b[1].shown_curve(45.0) == b[1].curve(45.0),
            "без «°C на ватт» — прежняя кривая"
        );
        let zero = b[0].power.expect("мощность");
        assert!(zero.determined && (zero.watts - 10.0).abs() < 0.5, "{zero:?}");
    }

    #[test]
    fn knots_keep_a_step_of_data_above_and_the_knee_needs_a_doubled_slope() {
        let h = Hinge::layout(40.3, 45.05);
        assert_eq!(h.knots, [42.0, 44.0], "×44 ≤ 45,05 − 1, ×46 — нет");
        assert!(Hinge::layout(40.3, 42.9).knots.is_empty(), "×42 > 42,9 − 1");
        let bent = Hinge {
            weights: vec![0.5, 0.3, 3.0, 0.0],
            ..h.clone() // two knots, once
        };
        assert!((bent.at(40.3) - 0.5).abs() < 1e-12 && (bent.at(43.0) - (0.5 + 0.3 * 2.7 + 3.0)).abs() < 1e-12);
        assert_eq!(
            bent.knee(),
            Some(Knee {
                at: 42.0,
                before: 0.3,
                after: 3.3
            })
        );
        let mild = Hinge {
            weights: vec![0.0, 0.3, 0.2, 0.0],
            ..h
        };
        assert_eq!(mild.knee(), None, "0,3 → 0,5 — не вдвое");
    }

    #[test]
    fn old_volts_columns_change_nothing() {
        // Журналы 0.5.4 несут `<блок>.volts`: модель их не берёт — та же модель, что без них.
        let (csv, die) = synthetic(3000, None);
        let mut with = String::new();
        for (k, line) in csv.lines().enumerate() {
            with.push_str(line);
            with.push_str(if k == 0 {
                ",0.volts,1.volts,2.volts"
            } else {
                ",0.9,1.1,1.2"
            });
            with.push('\n');
        }
        let (a, b) = (fit(&[&log_from_csv(&csv)], &die), fit(&[&log_from_csv(&with)], &die));
        assert_eq!(a.blocks, b.blocks);
    }

    #[test]
    fn known_gains_come_back_from_a_synthetic_log() {
        let (csv, die) = synthetic(6000, None);
        let log = log_from_csv(&csv);
        assert_eq!(log.rows(), 6000);
        let model = fit(&[&log], &die);
        let b = &model.blocks[1];
        let c = b.coef.as_ref().expect("коэффициенты");
        assert!((c.beta - 0.5).abs() < 0.02, "β {}", c.beta);
        assert!((c.own_total() - 18.0).abs() < 0.3, "свои {:?}", c.own);
        for n in &c.neighbours {
            assert!((n.fast - 2.0).abs() < 0.3 && (n.slow - 4.0).abs() < 0.3, "{n:?}");
        }
        assert!(
            b.errors.is_some_and(|e| e.proxy < 0.05 && e.full < e.die_only),
            "{:?}",
            b.errors
        );
        // Кривая не падает с множителем.
        let curve: Vec<f64> = (300..=600).filter_map(|r| b.curve(f64::from(r) / 10.0)).collect();
        assert!(curve.windows(2).all(|w| w[1] >= w[0] - 1e-12));
    }

    #[test]
    fn a_gap_restarts_the_averages_and_sixteen_seconds_are_not_counted() {
        // 200 строк по 0,25 с, перерыв 5 с, ещё 200: в каждом куске первые 16 с (64 строки) не в счёт, последняя
        // строка куска — без следующей.
        let (csv, die) = synthetic(400, Some(200));
        let log = log_from_csv(&csv);
        let model = fit(&[&log], &die);
        // Первая строка — at = 250 мс, начало; годны строки со временем от начала ≥ 16 с: в первом куске это
        // строки 64..=198 (199-я — последняя перед перерывом), во втором 264..=398.
        assert_eq!(model.blocks[0].usable, (198 - 64 + 1) + (398 - 264 + 1));
    }

    #[test]
    fn median_of_odd_and_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn percentiles_by_time_weigh_rows_by_their_interval() {
        // 1000 строк ×40–44 по 250 мс и пять строк ×19: 0,5 % времени — 5-й процентиль их не видит.
        let mut values: Vec<f64> = (0..1000).map(|k| 40.0 + f64::from(k % 5)).collect();
        values.extend([19.0; 5]);
        let weights = vec![250; values.len()];
        assert_eq!(by_time(&values, &weights, SOLID[0]), Some(40.0));
        assert_eq!(by_time(&values, &weights, SOLID[1]), Some(44.0));
        // Те же пять строк, но по 30 с (после пропусков журнала их время больше): 13 % времени — уже видны.
        let mut long = weights;
        long[1000..].fill(30_000);
        assert_eq!(by_time(&values, &long, SOLID[0]), Some(19.0));
        // Значение — всегда одной из строк; пусто — ничего.
        assert_eq!(by_time(&[41.5], &[250], SOLID[1]), Some(41.5));
        assert_eq!(by_time(&[], &[], SOLID[0]), None);
    }

    #[test]
    fn a_few_outlier_rows_do_not_widen_the_solid_part_of_the_curve() {
        // Синтетический журнал: множители блока 0 — ×40–45; пять строк под нагрузкой на ×19 (короткий провал) — сплошная
        // часть остаётся 40–45. Ловит: прежний диапазон «min–max по строкам» (блок 1 на его журнале — ×19–×43).
        let (csv, die) = synthetic(6000, None);
        let plain = fit(&[&log_from_csv(&csv)], &die);
        let mut lines: Vec<String> = csv.lines().map(str::to_string).collect();
        for line in &mut lines[3001..3006] {
            let mut fields: Vec<&str> = line.split(',').collect();
            fields[1] = "19.00";
            fields[2] = "0.90";
            *line = fields.join(",");
        }
        let dipped = fit(&[&log_from_csv(&(lines.join("\n") + "\n"))], &die);
        let range = |m: &Model| m.blocks[0].reference.as_ref().map(|r| (r.lo, r.hi));
        assert_eq!(range(&plain), Some((40.0, 45.0)));
        assert_eq!(
            range(&dipped),
            Some((40.0, 45.0)),
            "пять строк из тысяч — не опыт блока"
        );
    }
}
