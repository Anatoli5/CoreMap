// Модель нагрева блоков по журналу срезов (DECISIONS #39). Бриф — outputs/model-impl-brief-2026-10-16.md; основа —
// совместный вывод консилиума (outputs/model-consilium-2026-10-16.md §2–§4) без слоёв потолка, регулятора и размещения
// нагрузки: они нужны подбору потолков, следующему этапу.
//
// Владелец 16.10: «Она должна отталкиваться от фактических показателей. То есть на каком множителе ядро работает, под
// какой нагрузкой, и каковы остальные условия. То есть окружение и темпоральные, изменение системы во времени»; «потолок
// вообще не должен ебать агрессионную модель» — потолка среди входов нет, только то, что ядро делало и что было вокруг
// (и ранг — порядок потолков P-ядер, по которому Windows раздаёт тяжёлые потоки; владелец: «добавь в журнал ранг… мы это
// увидим»).
//
// Слой L3 — температура блока i в строке t (из него — кривые и «°C на ступень»):
//   T_i(t) = a + β·D_i(t) + Σ_k K_k·own_k(t) + Σ_{j∈соседи} [M_fast·pfast_j(t) + M_4s·ema4_j(t)] + Σ_k φ_k·[ранг = k]·pfast_i
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
// Бриф 16.10 «эффективность» (outputs/efficiency-brief-2026-10-16.md; владелец: «конечная метрика у нас
// производительность… частоте на загрузку… ваты… материнская плата может по-разному откалибровать напряжение»): форма
// своей быстрой нагрузки от множителя не задана ни кубом, ни ломаной — у каждого блока по коэффициенту на целый
// множитель (`Steps`: накопительные индикаторы корзин × занятость, веса ≥ 0 — нагрев не падает с множителем, иной формы
// нет; `shaped`). Средние и соседи — по прежней p. Прежний вид (`coef`, K·p) остаётся только для сравнения в отчёте.
// Счёт — МНК с неотрицательностью (Лоусон — Хенсон) по накопленным нормальным уравнениям блока; проверка — четыре
// сплошных куска по времени (строки соседних моментов похожи — случайное деление подглядывало бы).
//
// Бриф 16.10 («нагрев ядра», дополнение «добавь в журнал ранг»), бриф W и бриф «эффективность»:
// • ранг P-ядер — общий для всех P-ядер член на ранг 1 и 2 (наибольший потолок среди P-ядер и следующий; ранг 3 — основа):
//   [ранг = k]·pfast, знак любой; при данном ранге ядра решаются сами, ранг — шагами Ньютона по сумме квадратов всех
//   P-ядер (`joint`); опознаётся только сменой ранга внутри ядра. Сам потолок входом не бывает — только порядок потолков;
// • нагрев ядра H(r) — установившийся свой нагрев при полной нагрузке: ступени + средние × p(r), без свободного члена,
//   кристалла, соседей и ранга (`own_heat`);
// • эффективность корзины — производительность на градус = r / H(r) (производительность = множитель × нагрузка, при
//   полной нагрузке — множитель); только где H по всем четырём подгонкам без куска больше нуля и данных корзины не
//   меньше минуты полной нагрузки; разброс — наименьший и наибольший по подгонкам (`efficiency`). Ватты не входят;
// • частный остаток — T − (a + β·D + соседи + ранг), по корзине множителя Σ остатков / Σ занятостей (`OwnSums`);
// • что влияет — изменчивость вклада групп входов и ошибка без каждой (`Influence`);
// • хранение строк — по насыщению клеток (блок, множитель) вместо границы 8 ч (`retain`).
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
/// Меньше стольких годных строк (минута по 0,25 с) — коэффициентов нет вовсе (неизвестных — до нескольких десятков: по
/// корзине на целый множитель, свои средние, соседи, ранг).
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
/// Корзина множителя показывает эффективность, только если занятости × времени в ней не меньше этого (с): минута полной
/// нагрузки — те же `MIN_ROWS` строк по 0,25 с, что нужны коэффициентам.
pub const MIN_BIN_SECS: f64 = 60.0;
/// Хранение строк для модели (бриф W 16.10, вместо границы 8 ч): по клетке (блок, целый множитель) остаются новейшие
/// строки, пока стандартная ошибка средней частного остатка клетки по окнам 10 с не дошла до этой; старше — выбрасываются.
/// 0,1 °C — треть самого малого шага на ступень, измеренного на его журнале (0,29 °C на ступень у блоков 0 и 12,
/// outputs/model-fit-shape-2026-10-16.txt): клетка дальше этого уже не уточняется.
pub const SATURATED: f64 = 0.1;
/// Стандартная ошибка по меньшему числу окон 10 с — не довод: клетка насыщается не раньше стольких окон (100 с).
pub const MIN_WINDOWS: u32 = 10;
/// Самые новые строки остаются всегда: журнал дописывается, и средним следующих строк нужны строки перед ними.
pub const LIVE_MS: i64 = 60_000;
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

pub const RATIO: usize = 0;
pub const BUSY: usize = 1;
pub const TEMP: usize = 2;
/// Общие показания строки в `Log`: мощности процессора, видеокарты и ядер, температура корпуса, вентиляторы и GPU.
pub const POWER_CPU: usize = 0;
pub const POWER_GPU: usize = 1;
pub const POWER_CORES: usize = 2;
pub const PACKAGE_TEMP: usize = 3;
pub const FAN_CPU: usize = 4;
pub const FAN_GPU: usize = 5;
pub const TEMP_GPU: usize = 6;
pub const BUSY_GPU: usize = 7;

/// Строки одного файла срезов по порядку файла — как их оставляет разбор (`samples::Scratch::count_keeping`), второго
/// чтения файла нет. Все полные строки с корректным временем сохраняются, включая прежний шаг 2 с; пригодность
/// промежутков определяет потребитель истории. Числа — f32 (в файле у них 2–3 знака), пусто — NaN: множитель, занятость
/// и температура блока, восемь общих показаний строки; потолок блока — байт (целые ×1…×255, 0 — неизвестен). Пропуски
/// общих датчиков не заполняются: возраст показания и допустимое удержание определяет потребитель истории.
/// Напряжение журналов 0.5.4 разбор не берёт: модель его не использует.
#[derive(Clone, Debug, Default)]
pub struct Log {
    names: Vec<String>,
    times: Vec<i64>,
    cells: Vec<[f32; 3]>,
    ceilings: Vec<u8>,
    environment: Vec<[f32; 8]>,
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

/// Потолок в байт: целый ×1…×255; иначе (нет, дробный, вне байта) — 0, неизвестен.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "checked to be a whole number within 1..=255 first"
)]
fn ceiling_byte(v: Option<f64>) -> u8 {
    v.filter(|c| c.fract() == 0.0 && (1.0..=255.0).contains(c))
        .map_or(0, |c| c as u8)
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

    /// Время строк (мс, `samples::row_time`) по порядку файла.
    #[must_use]
    pub fn times(&self) -> &[i64] {
        &self.times
    }

    /// Сколько памяти держат строки, байт.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.times.len() * 8 + self.cells.len() * 12 + self.ceilings.len() + self.environment.len() * 32
    }

    /// Строка: время (мс, `samples::row_time`), по блоку — множитель, занятость, температура, потолок; мощность процессора
    /// (корпус), видеокарты и ядер процессора (`POWER_CPU`, `POWER_GPU`, `POWER_CORES`).
    #[cfg(test)]
    pub fn push(&mut self, at: i64, cells: impl Iterator<Item = [Option<f64>; 4]>, package: [Option<f64>; 3]) {
        let [cpu, gpu, cores] = package;
        self.push_with_environment(at, cells, [cpu, gpu, cores, None, None, None, None, None]);
    }

    /// Полная строка: общие показания по индексам `POWER_CPU`…`BUSY_GPU`; только измеренное в этот момент.
    pub fn push_with_environment(
        &mut self,
        at: i64,
        cells: impl Iterator<Item = [Option<f64>; 4]>,
        shared: [Option<f64>; 8],
    ) {
        let before = self.cells.len();
        for [ratio, busy, temp, ceiling] in cells {
            self.cells.push([ratio, busy, temp].map(stored));
            self.ceilings.push(ceiling_byte(ceiling));
        }
        // Разбор отдаёт значения каждого блока заголовка — ни больше, ни меньше; иначе строки поедут.
        debug_assert_eq!(
            self.cells.len() - before,
            self.names.len(),
            "блоков в строке — как в заголовке"
        );
        self.times.push(at);
        self.environment.push(shared.map(stored));
    }

    /// Оставить только строки, у которых `keep` — true (порядок тот же); `keep` — по строке.
    fn keep(&mut self, keep: &[bool]) {
        debug_assert_eq!(keep.len(), self.times.len(), "отметка на каждую строку");
        let width = self.names.len();
        let mut kept = 0;
        for (row, _) in keep.iter().enumerate().filter(|(_, k)| **k) {
            if kept != row {
                self.times[kept] = self.times[row];
                self.environment[kept] = self.environment[row];
                self.cells.copy_within(row * width..(row + 1) * width, kept * width);
                self.ceilings.copy_within(row * width..(row + 1) * width, kept * width);
            }
            kept += 1;
        }
        self.times.truncate(kept);
        self.environment.truncate(kept);
        self.cells.truncate(kept * width);
        self.ceilings.truncate(kept * width);
    }

    pub fn get(&self, row: usize, block: usize, what: usize) -> Option<f64> {
        loaded(self.cells[row * self.names.len() + block][what])
    }

    pub fn ceiling(&self, row: usize, block: usize) -> Option<u8> {
        Some(self.ceilings[row * self.names.len() + block]).filter(|&c| c > 0)
    }

    pub fn power(&self, row: usize, which: usize) -> Option<f64> {
        self.environment(row, which)
    }

    /// Общее показание исходной строки; пустое поле остаётся неизвестным, без удержания прошлого значения.
    pub fn environment(&self, row: usize, which: usize) -> Option<f64> {
        loaded(self.environment[row][which])
    }
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
    let mut carry = samples::Carry::default();
    samples::Scratch::default().count_keeping(&columns, body, &mut carry, &mut tally, Some(&mut log));
    log
}

#[cfg(test)]
#[path = "raw_log_spec.rs"]
mod raw_log_spec;

#[cfg(test)]
mod raw_log_tests {
    use super::*;

    #[test]
    fn old_schema_preserves_available_fields_and_unknown_environment() {
        let log = log_from_csv(
            "time,package,fan_cpu,fan_gpu,0.ceiling,0.ratio,0.busy,0.temp\n\
             2026-10-15 10:00:00,68,3000,,48,45,1,72\n",
        );
        assert_eq!(log.rows(), 1);
        assert_eq!(log.environment(0, PACKAGE_TEMP), Some(68.0));
        assert_eq!(log.environment(0, FAN_CPU), Some(3000.0));
        for which in [POWER_CPU, POWER_GPU, POWER_CORES, FAN_GPU, TEMP_GPU, BUSY_GPU] {
            assert_eq!(log.environment(0, which), None);
        }
        assert_eq!(log.get(0, 0, RATIO), Some(45.0));
        assert_eq!(log.get(0, 0, BUSY), Some(1.0));
        assert_eq!(log.get(0, 0, TEMP), Some(72.0));
        assert_eq!(log.ceiling(0, 0), Some(48));
    }

    #[test]
    fn shuffled_columns_preserve_shared_values_and_sparse_points() {
        let log = log_from_csv(
            "time,temp_gpu,0.temp,fan_gpu,power_cores,0.busy,package,busy_gpu,0.ceiling,power_cpu,fan_cpu,0.ratio,power_gpu\n\
             2026-10-15 10:00:00,62,74,3200,36,1,70,80,48,55,3000,45,120\n\
             2026-10-15 10:00:02,,75,,37,1,71,,48,56,,46,\n\
             2026-10-15 10:00:02.250,63,76,3300,38,1,72,82,48,57,3100,47,121\n",
        );
        assert_eq!(log.rows(), 3);
        assert_eq!(log.times()[1] - log.times()[0], 2000);
        assert_eq!(log.times()[2] - log.times()[1], 250);
        for (which, expected) in [55.0, 120.0, 36.0, 70.0, 3000.0, 3200.0, 62.0, 80.0]
            .into_iter()
            .enumerate()
        {
            assert_eq!(log.environment(0, which), Some(expected));
        }
        for which in [FAN_CPU, FAN_GPU, TEMP_GPU, POWER_GPU, BUSY_GPU] {
            assert_eq!(log.environment(1, which), None, "редкое показание не удерживается");
        }
        assert_eq!(log.power(1, POWER_CORES), Some(37.0));
        assert_eq!(log.environment(2, FAN_CPU), Some(3100.0));
        assert_eq!(log.get(2, 0, RATIO), Some(47.0));
    }

    #[test]
    fn compaction_keeps_shared_values_aligned_with_blocks_and_time() {
        let mut log = Log::new(vec!["0".to_string()]);
        for i in 0..4 {
            let n = f64::from(i);
            log.push_with_environment(
                i64::from(i) * 250,
                std::iter::once([Some(n + 40.0), Some(1.0), Some(n + 70.0), Some(n + 48.0)]),
                [Some(n); 8],
            );
        }
        assert_eq!(log.bytes(), 4 * (8 + 12 + 1 + 32));
        log.keep(&[false, true, false, true]);
        assert_eq!(log.times(), &[250, 750]);
        assert_eq!(log.bytes(), 2 * (8 + 12 + 1 + 32));
        for (row, n) in [1.0, 3.0].into_iter().enumerate() {
            assert_eq!(log.get(row, 0, RATIO), Some(n + 40.0));
            assert_eq!(log.get(row, 0, TEMP), Some(n + 70.0));
            assert_eq!(log.ceiling(row, 0).map(f64::from), Some(n + 48.0));
            for which in 0..8 {
                assert_eq!(log.environment(row, which), Some(n));
            }
        }
    }

    #[test]
    fn legacy_push_shares_power_storage_without_inventing_metadata() {
        let mut log = Log::new(vec!["0".to_string()]);
        log.push(
            250,
            std::iter::once([None, Some(0.0), None, None]),
            [Some(20.0), None, Some(15.0)],
        );
        assert_eq!(log.power(0, POWER_CPU), Some(20.0));
        assert_eq!(log.environment(0, POWER_CPU), Some(20.0));
        assert_eq!(log.power(0, POWER_GPU), None);
        assert_eq!(log.power(0, POWER_CORES), Some(15.0));
        for which in PACKAGE_TEMP..=BUSY_GPU {
            assert_eq!(log.environment(0, which), None);
        }
        assert_eq!(log.get(0, 0, BUSY), Some(0.0), "измеренный ноль остаётся нулём");
        assert_eq!(log.get(0, 0, TEMP), None);
    }
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

// ----- свой нагрев по множителям -----

/// Свой нагрев блока по целым множителям, выученный по журналу (бриф 16.10, «эффективность»): корзины — целые
/// фактические множители годных строк блока по возрастанию; столбец корзины j в уравнении строки —
/// занятость·[целый множитель ≥ `bins[j]`] (накопительные индикаторы), вес `w_j` ≥ 0; нагрев при полной своей нагрузке
/// на корзине k — `Σ_{j≤k} w_j`: не падает с множителем по построению, другой формы не задано — это значения самих данных
/// по множителям. Ниже первой корзины — 0, выше последней — сумма всех весов.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Steps {
    pub bins: Vec<i64>,
    /// По корзине; пусто — весов нет (g ≡ 0, прежний вид `coef`).
    pub weights: Vec<f64>,
}

impl Steps {
    /// Корзины по целым множителям годных строк (по возрастанию, без повторов).
    fn layout(bins: impl IntoIterator<Item = i64>) -> Steps {
        let mut bins: Vec<i64> = bins.into_iter().collect();
        bins.sort_unstable();
        bins.dedup();
        Steps {
            bins,
            weights: Vec::new(),
        }
    }

    /// Столбцов у формы в уравнении блока.
    fn columns(&self) -> usize {
        self.bins.len()
    }

    /// Столбцы формы строки: `busy·[whole(r) ≥ bins[j]]` — в `out`.
    fn basis(&self, ratio: f64, busy: f64, out: &mut [f64]) {
        let r = whole(ratio);
        for (o, &b) in out.iter_mut().zip(&self.bins) {
            *o = if r >= b { busy } else { 0.0 };
        }
    }

    /// g(r) — нагрев при полной своей нагрузке на целом множителе строки.
    #[must_use]
    pub fn at(&self, ratio: f64) -> f64 {
        let r = whole(ratio);
        self.bins
            .iter()
            .zip(&self.weights)
            .filter(|&(&b, _)| b <= r)
            .map(|(_, w)| w)
            .sum()
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

    /// `Σ x_i·x_j` (из верхнего треугольника).
    fn raw(&self, i: usize, j: usize) -> f64 {
        let (lo, hi) = if i <= j { (i, j) } else { (j, i) };
        self.g[lo * self.m + hi]
    }

    /// Центрированные суммы: `Σ(x_i − x̄_i)(x_j − x̄_j)`, `Σ(x_i − x̄_i)(y − ȳ)`, `Σ(y − ȳ)²`.
    fn c(&self, i: usize, j: usize) -> f64 {
        self.raw(i, j) - self.s[i] * self.s[j] / self.n
    }

    fn cy(&self, i: usize) -> f64 {
        self.b[i] - self.s[i] * self.sy / self.n
    }

    fn cyy(&self) -> f64 {
        self.syy - self.sy * self.sy / self.n
    }

    /// Сумма квадратов остатков при множителях `coef` (свободный член — лучший при них, то есть центрирование).
    fn sse(&self, coef: &[f64]) -> f64 {
        let live: Vec<usize> = (0..self.m).filter(|&i| coef[i] != 0.0).collect();
        let mut out = self.cyy();
        for &i in &live {
            out -= 2.0 * coef[i] * self.cy(i);
            for &j in &live {
                out += coef[i] * coef[j] * self.c(i, j);
            }
        }
        out
    }

    /// МНК со свободным членом по столбцам `columns` (остальные — 0), их множители ≥ 0: суммы центрируются (свободный
    /// член уходит), столбцы приводятся к единичной дисперсии (обусловленность), затем `nnls`. None — строк меньше двух.
    fn solve(&self, columns: &[usize]) -> Option<Solved> {
        self.solve_how(columns, &How::LAYER)
    }

    /// То же с выбором `How`: закреплённые множители столбцов (`fixed` — их нет в `columns`, отклик — y − Σ v·x), без
    /// неотрицательности — обычный МНК (только диагностика отчёта).
    fn solve_how(&self, columns: &[usize], how: &How) -> Option<Solved> {
        if self.n < 2.0 {
            return None;
        }
        let m = self.m;
        let k = columns.len();
        let sy = self.sy - how.fixed.iter().map(|&(f, v)| v * self.s[f]).sum::<f64>();
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
            // Σ x_i·(y − Σ v·x_f).
            let b = self.b[i] - how.fixed.iter().map(|&(f, v)| v * self.raw(i, f)).sum::<f64>();
            rhs[r] = (b - self.s[i] * sy / self.n) / scale[r];
            for (c, &j) in columns.iter().enumerate() {
                if scale[c] <= 0.0 {
                    continue;
                }
                gram[r * k + c] = self.c(i, j) / (scale[r] * scale[c]);
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
        for &(f, v) in &how.fixed {
            coef[f] = v;
        }
        let a = (self.sy - (0..m).map(|i| coef[i] * self.s[i]).sum::<f64>()) / self.n;
        Some((a, coef))
    }
}

/// Как решать задачу блока.
#[derive(Clone, Debug, Default)]
struct How {
    /// Без неотрицательности (обычный МНК) — только диагностика отчёта.
    signed: bool,
    /// Закреплённые множители: (столбец, значение) — уровень кристалла при β = 1 (диагностика), ранг — общий для P-ядер.
    fixed: Vec<(usize, f64)>,
}

impl How {
    /// Слой L3 по брифу: все множители, кроме свободного члена, ≥ 0.
    const LAYER: How = How {
        signed: false,
        fixed: Vec::new(),
    };
}

/// Решение одной задачи: свободный член и множители всех столбцов (не взятые — 0).
type Solved = (f64, Vec<f64>);
/// Решения видов: (a) D, (b) + свои, (c) + соседи + ранг — слой L3 с выученной формой; (d) — диагностика отчёта: как (c),
/// но β = 1 закреплён и без ранга; (e) — прежний вид (c) без ранга: своя быстрая нагрузка — K·p, без формы (для «°C на
/// ватт» и сравнения в отчёте); дальше — (c) без одной группы входов (что даёт группа на отложенных данных): без
/// кристалла, без своей работы, без соседей, без предыдущих секунд, без ранга. Ранг в них — тот же общий, что в (c) этих
/// же строк; «без ранга» — ноль.
type Variants = [Option<Solved>; VARIANTS];
const VARIANTS: usize = 10;
const DIE_ONLY: usize = 0;
const WITH_OWN: usize = 1;
const FULL: usize = 2;
const UNIT: usize = 3;
const PROXY: usize = 4;
/// Группы входов в порядке видов «без группы» (`WITHOUT + группа`) и в `Influence`.
const WITHOUT: usize = 5;
pub const GROUPS: usize = 5;
pub const DIE_GROUP: usize = 0;
pub const OWN_GROUP: usize = 1;
pub const NEAR_GROUP: usize = 2;
pub const HISTORY_GROUP: usize = 3;
pub const RANK_GROUP: usize = 4;

/// Предсказание: a + c·x.
fn predict(fit: &Solved, x: &[f64]) -> f64 {
    fit.0 + fit.1.iter().zip(x).map(|(c, v)| c * v).sum::<f64>()
}

// ----- ранг P-ядер -----

/// Столбцов ранга: ранг 1 и ранг 2 (ранг 3 — основа).
pub const RANKS: usize = 2;

/// Ранг P-ядер строки по их потолкам (бриф 16.10, дополнение: «добавь в журнал ранг»): 1 — наибольший потолок среди
/// P-ядер (равные — один ранг), 2 — следующий по величине, 3 — остальные. У кластера E-ядер ранга нет: Windows ранжирует
/// внутри рода, а нагрузка кластера в журнале — по первому ядру. Потолок хоть одного P-ядра неизвестен — ранга нет ни у
/// одного: порядок строки не известен.
pub fn ranks(ceilings: &[Option<u8>], e: &[bool], out: &mut [Option<u8>]) {
    out.fill(None);
    let p = || ceilings.iter().zip(e).filter(|(_, e)| !**e).map(|(c, _)| *c);
    if p().any(|c| c.is_none()) {
        return;
    }
    let first = p().flatten().max();
    let second = p().flatten().filter(|&c| Some(c) < first).max();
    for ((o, c), &is_e) in out.iter_mut().zip(ceilings).zip(e) {
        if is_e {
            continue;
        }
        *o = c.map(|c| {
            if Some(c) == first {
                1
            } else if Some(c) == second {
                2
            } else {
                3
            }
        });
    }
}

// ----- проход по строкам -----

/// Годная строка блока: что о ней нужно счёту. x: [D, pfast, 6 средних, по соседу pfast и 4-с средняя, столбцы формы,
/// ранг 1 и ранг 2 (pfast в строке этого ранга)].
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
    rank: Option<u8>,
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
    ceiling: Vec<Option<u8>>,
    rank: Vec<Option<u8>>,
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
            ceiling: vec![None; n],
            rank: vec![None; n],
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

/// Проход по строкам всех файлов: средние, уровень кристалла, ранг, и годные строки блоков — в `emit`.
struct Walk<'a> {
    die: &'a Die,
    /// Корзины своего нагрева по блоку (без весов).
    shapes: &'a [Steps],
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
        let hinge = self.shapes.iter().map(Steps::columns).max().unwrap_or(0);
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
            into.ceiling[i] = cols[i].and_then(|c| log.ceiling(row, c));
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
        ranks(&into.ceiling, &self.die.e, &mut into.rank);
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
            // Ранг строки t: своя быстрая нагрузка, если ядро в этом ранге (ранг 3 — основа, столбца у него нет).
            let rank = prev.rank[i];
            for (k, tier) in (1..=2u8).enumerate() {
                x[at + columns + k] = if rank == Some(tier) { own } else { 0.0 };
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
                rank,
                near: &near[..self.die.neighbours[i].len()],
            });
        }
    }
}

/// Сколько входов у блока: D, семь своих, по два на соседа, столбцы формы, два ранга.
fn width_of(die: &Die, block: usize, shapes: &[Steps]) -> usize {
    1 + OWN + 2 * die.neighbours[block].len() + shapes[block].columns() + RANKS
}

/// Столбцы ранга у блока с `nb` соседями и формой из `hc` столбцов.
fn rank_columns(nb: usize, hc: usize) -> [usize; RANKS] {
    let at = OWN + 1 + 2 * nb + hc;
    [at, at + 1]
}

/// Столбцы вида `v` у блока с `nb` соседями и формой из `hc` столбцов (без ранга: он решается общим для P-ядер и входит
/// закреплённым). Порядок входов: [D | pfast | 6 средних | соседи | форма | ранг].
fn variant_columns(v: usize, nb: usize, hc: usize) -> Vec<usize> {
    let shape = OWN + 1 + 2 * nb..OWN + 1 + 2 * nb + hc;
    let near = OWN + 1..OWN + 1 + 2 * nb;
    let ema = 2..=OWN;
    let die = std::iter::once(0);
    match v {
        DIE_ONLY => vec![0],
        WITH_OWN => die.chain(ema).chain(shape).collect(),
        FULL => die.chain(ema).chain(near).chain(shape).collect(),
        UNIT => ema.chain(near).chain(shape).collect(),
        PROXY => (0..OWN + 1 + 2 * nb).collect(),
        _ => match v - WITHOUT {
            DIE_GROUP => ema.chain(near).chain(shape).collect(),
            OWN_GROUP => die.chain(ema).chain(near).collect(),
            NEAR_GROUP => die.chain(ema).chain(shape).collect(),
            HISTORY_GROUP => die.chain(near).chain(shape).collect(),
            _ => die.chain(ema).chain(near).chain(shape).collect(),
        },
    }
}

/// Входит ли в вид `v` общий ранг (закреплённым).
fn ranked(v: usize) -> bool {
    v == FULL || (v >= WITHOUT && v - WITHOUT != RANK_GROUP)
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
    /// Выученный свой нагрев по целым множителям; у прежнего вида (`BlockModel::coef`) весов нет.
    pub shape: Steps,
    /// Общий ранг P-ядер — на своей быстрой p в строке ранга 1 и ранга 2 (ранг 3 — основа); знак любой. У кластера и у
    /// вида без ранга — нули.
    pub rank: [f64; RANKS],
}

impl Coef {
    #[must_use]
    pub fn own_total(&self) -> f64 {
        self.own.iter().sum()
    }

    /// Коэффициенты решения блока; `shape` — корзины его нагрева: `shaped` — веса корзин из решения, иначе форма пустая
    /// (прежний вид).
    fn of(die: &Die, block: usize, shape: &Steps, shaped: bool, fit: &Solved) -> Coef {
        let c = &fit.1;
        let nb = die.neighbours[block].len();
        let mut own = [0.0; 7];
        own.copy_from_slice(&c[1..=OWN]);
        let at = OWN + 1 + 2 * nb;
        Coef {
            shape: if shaped {
                Steps {
                    bins: shape.bins.clone(), // a few dozen bins per solved block
                    weights: c[at..at + shape.columns()].to_vec(),
                }
            } else {
                Steps::default()
            },
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
            rank: rank_columns(nb, shape.columns()).map(|k| c[k]),
        }
    }
}

/// Вклады групп входов в температуру строки по решению `fit` (входы строки `x` блока с `nb` соседями и формой из `hc`
/// столбцов): кристалл β·D, своя работа (форма или K·pfast), соседи, предыдущие секунды (средние своей p), ранг.
fn parts(fit: &Solved, nb: usize, hc: usize, x: &[f64]) -> [f64; GROUPS] {
    let c = &fit.1;
    let sum = |r: std::ops::Range<usize>| r.map(|k| c[k] * x[k]).sum::<f64>();
    let near = OWN + 1..OWN + 1 + 2 * nb;
    let shape = near.end..near.end + hc;
    let rank = shape.end..shape.end + RANKS;
    [
        c[0] * x[0],
        c[1] * x[1] + sum(shape),
        sum(near),
        sum(2..OWN + 1),
        sum(rank),
    ]
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
/// (a) только смещение и наклон на D, (b) + свои, (c) + соседи и ранг (полный L3 с выученной формой).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Errors {
    pub die_only: f64,
    pub own: f64,
    pub full: f64,
    /// Диагностика: (c) при β = 1.
    pub unit: f64,
    /// (c) прежнего вида — своя быстрая нагрузка K·p без выученной формы.
    pub proxy: f64,
    /// (c) без группы входов (`DIE_GROUP`…`RANK_GROUP`).
    pub without: [f64; GROUPS],
    pub windows: usize,
}

/// Диагностика отчёта (окно её не показывает): L3 без неотрицательности и L3 при β = 1 — по всем данным.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnosis {
    pub signed: Coef,
    pub unit: Coef,
}

/// Частный остаток блока на целом множителе (бриф W 16.10): температура минус всё, что модель относит не к своей работе
/// ядра (свободный член, кристалл, соседи, ранг), делённая на занятость строки, — средняя с весом занятость × время
/// строки. Вес возвращает строкам с малой нагрузкой их надёжность: Σ остаток / Σ занятость.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OwnBin {
    pub ratio: i32,
    /// Время строк, с; средняя занятость (по времени); нагрев при полной своей нагрузке, °C.
    pub secs: f64,
    pub load: f64,
    pub value: f64,
}

/// Накопление `OwnBin`: Σ время × остаток, Σ время × занятость, Σ время (мс).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OwnSums {
    residual: f64,
    busy: f64,
    span: f64,
}

impl OwnSums {
    /// Строка: её время (мс), занятость и частный остаток (°C, не делённый на занятость).
    pub fn add(&mut self, span: f64, busy: f64, residual: f64) {
        self.residual += span * residual;
        self.busy += span * busy;
        self.span += span;
    }

    /// Корзина; None — нагрузки в ней меньше одной строки полной нагрузки (0,25 с): точки нет.
    #[must_use]
    pub fn bin(&self, ratio: i32) -> Option<OwnBin> {
        (self.busy >= ONE_ROW_MS && self.span > 0.0).then(|| OwnBin {
            ratio,
            secs: self.span / 1000.0,
            load: self.busy / self.span,
            value: self.residual / self.busy,
        })
    }
}

/// Одна строка полной нагрузки, мс — меньше нагрузки в корзине частных остатков не бывает (тот же предел, что у
/// бледности точек графика, chart.rs).
const ONE_ROW_MS: f64 = 250.0;

/// Что влияет на температуру блока (бриф W): по группе входов — изменчивость её вклада по годным строкам
/// (среднеквадратичное отклонение по времени, °C) по всем данным и разброс по четырём подгонкам без куска; и насколько
/// хуже ошибка на отложенных данных без неё.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Influence {
    pub size: [f64; GROUPS],
    pub lo: [f64; GROUPS],
    pub hi: [f64; GROUPS],
    /// Ошибка без группы минус полная, °C; None — не посчитана.
    pub ablation: [Option<f64>; GROUPS],
}

/// Меньше этого разброс вклада (°C) — вклада нет: при неотрицательных множителях «диапазон пересекает ноль» — значит
/// доходит до нуля.
pub const NO_EFFECT: f64 = 0.005;

impl Influence {
    /// Определена ли группа: ни в одной подгонке (по всем данным и без каждого куска) её вклад не ноль.
    #[must_use]
    pub fn determined(&self, group: usize) -> bool {
        self.lo[group] > NO_EFFECT
    }
}

/// Общий ранг P-ядер (только в скрытом отчёте): на ранг 1 и ранг 2, по всем данным и разброс по подгонкам без куска.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rank {
    pub value: [f64; RANKS],
    pub lo: [f64; RANKS],
    pub hi: [f64; RANKS],
    /// Ранг в счёте: хоть у одного P-ядра есть годные строки и в этом ранге, и вне его (не меньше `MIN_ROWS` каждых) —
    /// без смены ранга внутри ядра его не отличить от формы своей нагрузки.
    pub used: [bool; RANKS],
    /// Определён: в счёте, опознан (кривизна по рангам не вырождена), решились все четыре подгонки без куска, и разброс
    /// не пересекает ноль.
    pub determined: [bool; RANKS],
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockModel {
    pub name: String,
    pub e: bool,
    /// Годных строк для полного L3 и их время, мин.
    pub usable: usize,
    pub minutes: f64,
    /// Прежний вид L3: своя быстрая нагрузка — K·p (p — куб, принят, не измерен). Только для сравнения в отчёте.
    pub coef: Option<Coef>,
    /// L3 со своим нагревом по целым множителям (`Steps`): из него кривая, нагрев ядра, эффективность.
    pub shaped: Option<Coef>,
    /// То же по четырём подгонкам без куска (разброс нагрева ядра и эффективности).
    pub folds: Vec<Coef>,
    pub reference: Option<Reference>,
    pub errors: Option<Errors>,
    /// Отношение большего суммарного своего коэффициента к меньшему у половин данных (стабильность).
    pub halves: Option<f64>,
    pub diagnosis: Option<Diagnosis>,
    /// Частные остатки по целым множителям (скрытый отчёт) — и сколько данных в корзине (для эффективности).
    pub own_bins: Vec<OwnBin>,
    pub influence: Option<Influence>,
}

/// Точка вида «эффективность» (бриф 16.10): целый множитель; производительность на градус = r / свой нагрев при полной
/// нагрузке (по всем данным) и её разброс — наименьшая и наибольшая по четырём подгонкам без куска (вместе со значением по
/// всем данным); сам нагрев, °C; данных в корзине — время строк, с, и их ø занятость.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Efficiency {
    pub ratio: i32,
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
    pub heat: f64,
    pub secs: f64,
    pub load: f64,
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

    /// Свой нагрев при полной своей нагрузке на множителе r по коэффициентам `c` — установившийся: форма + средние ×
    /// p(r) (средняя постоянного — оно само); у прежнего вида формы нет, и это K·p(r). Без свободного члена, кристалла,
    /// соседей и ранга.
    #[must_use]
    pub fn heat(&self, c: &Coef, ratio: f64) -> f64 {
        c.shape.at(ratio) + c.own_total() * self.power_at(ratio)
    }

    /// Нагрев ядра: свой установившийся нагрев при полной нагрузке на множителе r по всем данным; всё прочее вынесено.
    #[must_use]
    pub fn own_heat(&self, ratio: f64) -> Option<f64> {
        Some(self.heat(self.shaped.as_ref()?, ratio))
    }

    /// Нагрев ядра по всем данным и по подгонкам без куска: (наименьший, наибольший).
    #[must_use]
    pub fn own_heat_range(&self, ratio: f64) -> Option<(f64, f64)> {
        let full = self.own_heat(ratio)?;
        Some(
            self.folds
                .iter()
                .map(|c| self.heat(c, ratio))
                .fold((full, full), |(lo, hi), v| (lo.min(v), hi.max(v))),
        )
    }

    /// Эффективность по корзинам целого множителя (бриф 16.10): r / нагрев ядра — только где кривая рисуется (`drawn`),
    /// решились все четыре подгонки без куска и нагрев по каждой из них больше нуля, а занятости × времени в корзине не
    /// меньше `MIN_BIN_SECS`; разброс — r / наибольший … r / наименьший нагрев по подгонкам и по всем данным.
    #[must_use]
    pub fn efficiency(&self) -> Vec<Efficiency> {
        if !self.drawn() || self.folds.len() != FOLDS {
            return Vec::new();
        }
        self.own_bins
            .iter()
            .filter(|b| b.secs * b.load >= MIN_BIN_SECS)
            .filter_map(|b| {
                let ratio = f64::from(b.ratio);
                let heat = self.own_heat(ratio)?;
                let (lo, hi) = self.own_heat_range(ratio)?;
                (lo > 0.0).then(|| Efficiency {
                    ratio: b.ratio,
                    value: ratio / heat,
                    lo: ratio / hi,
                    hi: ratio / lo,
                    heat,
                    secs: b.secs,
                    load: b.load,
                })
            })
            .collect()
    }

    /// Предсказанная температура на этом фактическом множителе при опорных условиях (своя нагрузка 100 %, соседи и
    /// кристалл — медианы, ранг — основа), абсолютная — по L3 с выученной формой.
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
    /// Общий ранг P-ядер (скрытый отчёт).
    pub rank: Rank,
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
    /// По рангу 1 и 2: годных строк с нагрузкой в этом ранге и вне его (ранг известен).
    ranks: [[usize; 2]; RANKS],
    /// Целые множители годных строк — корзины своего нагрева (`Steps`).
    bins: std::collections::BTreeSet<i64>,
}

/// Окно 10 с одного блока: строк, Σ(T − D), Σ предсказанных (T̂ − D) по видам.
#[derive(Debug, Default, Clone, Copy)]
struct Window {
    key: (usize, i64),
    n: u32,
    obs: f64,
    pred: [f64; VARIANTS],
}

/// Ошибки по окнам: сумма квадратов по видам и число окон.
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

/// Ступеней Ньютона для общего ранга (каждая — решения всех P-ядер); сходится за единицы — кусочно-квадратичная задача.
const NEWTON: usize = 30;

/// Общий ранг P-ядер и полный L3 (c) всех блоков по одним суммам: ранг φ один на все P-ядра, знак любой; при данном φ
/// каждое ядро решается само (`solve_how` с закреплённым рангом), φ — шагами Ньютона по сумме квадратов остатков всех
/// P-ядер: она выпукла и кусочно-квадратична, градиент и кривизна — из сумм при нынешних свободных множителях ядер. Ранг не
/// в счёте (`used`) — ноль. У кластеров ранга нет. Третье — опознаны ли ранги (`newton_step`).
#[expect(
    clippy::many_single_char_names,
    reason = "Newton's step in the textbook's names: p, s, c, k, t, u"
)]
fn joint(
    die: &Die,
    sums: &[&Sums],
    shapes: &[Steps],
    enough: &[bool],
    used: [bool; RANKS],
) -> ([f64; RANKS], Vec<Option<Solved>>, bool) {
    let n = die.len();
    let layout = |i: usize| {
        let (nb, hc) = (die.neighbours[i].len(), shapes[i].columns());
        (variant_columns(FULL, nb, hc), rank_columns(nb, hc))
    };
    let p: Vec<usize> = (0..n).filter(|&i| enough[i] && !die.e[i]).collect();
    let solve_at = |phi: [f64; RANKS]| -> Vec<Option<Solved>> {
        (0..n)
            .map(|i| {
                if !enough[i] {
                    return None;
                }
                let (columns, tiers) = layout(i);
                let fixed = if die.e[i] {
                    Vec::new()
                } else {
                    (0..RANKS).filter(|&k| used[k]).map(|k| (tiers[k], phi[k])).collect()
                };
                sums[i].solve_how(&columns, &How { signed: false, fixed })
            })
            .collect()
    };
    let total = |fits: &[Option<Solved>]| -> f64 {
        p.iter()
            .filter_map(|&i| fits[i].as_ref().map(|f| sums[i].sse(&f.1)))
            .sum()
    };
    let mut phi = [0.0; RANKS];
    let mut fits = solve_at(phi);
    if p.is_empty() || !used.iter().any(|&u| u) {
        return (phi, fits, true);
    }
    let mut separable = true;
    let mut value = total(&fits);
    for _ in 0..NEWTON {
        let mut grad = [0.0; RANKS];
        let mut hess = [[0.0; RANKS]; RANKS];
        for &i in &p {
            let Some((_, c)) = &fits[i] else { continue };
            let s = sums[i];
            let (columns, tiers) = layout(i);
            let free: Vec<usize> = columns.iter().copied().filter(|&j| c[j] > 0.0).collect();
            let k = free.len();
            let mut inner = vec![0.0; k * k];
            for (r, &a) in free.iter().enumerate() {
                for (q, &b) in free.iter().enumerate() {
                    inner[r * k + q] = s.c(a, b);
                }
            }
            let all = vec![true; k];
            let through: Vec<Vec<f64>> = tiers
                .iter()
                .map(|&t| solve_on(&inner, &free.iter().map(|&a| s.c(a, t)).collect::<Vec<_>>(), &all))
                .collect();
            for t in (0..RANKS).filter(|&t| used[t]) {
                let live = (0..s.m).filter(|&j| c[j] != 0.0);
                grad[t] += s.cy(tiers[t]) - live.map(|j| c[j] * s.c(tiers[t], j)).sum::<f64>();
                for u in (0..RANKS).filter(|&u| used[u]) {
                    let back: f64 = free.iter().zip(&through[u]).map(|(&a, z)| s.c(tiers[t], a) * z).sum();
                    hess[t][u] += s.c(tiers[t], tiers[u]) - back;
                }
            }
        }
        let (step, apart) = newton_step(&hess, &grad, used);
        separable = apart;
        let size = step.iter().map(|v| v.abs()).fold(0.0, f64::max);
        if size <= 1e-10 * (1.0 + phi.iter().map(|v| v.abs()).fold(0.0, f64::max)) {
            break;
        }
        // Шаг не уменьшил сумму квадратов (активные множители ядер сменились) — вдвое короче.
        let mut scale = 1.0;
        let mut moved = false;
        while scale > 1e-6 {
            let candidate: [f64; RANKS] = std::array::from_fn(|k| phi[k] + scale * step[k]);
            let tried = solve_at(candidate);
            let v = total(&tried);
            if v <= value + 1e-12 * value.abs().max(1.0) {
                (phi, fits, value) = (candidate, tried, v);
                moved = true;
                break;
            }
            scale *= 0.5;
        }
        if !moved {
            break;
        }
    }
    (phi, fits, separable)
}

/// Кривизна по рангу вырождена: наименьшее собственное число меньше этой доли наибольшего — направление не опознано.
const SEPARABLE: f64 = 1e-6;

/// Шаг Ньютона по рангу: кривизна × шаг = градиент, только по рангам в счёте; по вырожденному направлению кривизны
/// (ранги не отличить друг от друга или от своих входов ядер) шага нет — псевдообратная: ранг остаётся там, где был
/// (с нуля — ноль). Второе — опознаны ли все ранги в счёте (`SEPARABLE`).
#[expect(
    clippy::many_single_char_names,
    reason = "a 2 x 2 eigen-decomposition in the textbook's names: p, q, r, l, x, y, n"
)]
fn newton_step(hess: &[[f64; RANKS]; RANKS], grad: &[f64; RANKS], used: [bool; RANKS]) -> ([f64; RANKS], bool) {
    let live: Vec<usize> = (0..RANKS).filter(|&k| used[k]).collect();
    let mut step = [0.0; RANKS];
    match *live.as_slice() {
        [k] => {
            let ok = hess[k][k] > 0.0;
            if ok {
                step[k] = grad[k] / hess[k][k];
            }
            (step, ok)
        }
        [a, b] => {
            // Собственные числа и векторы симметричной 2 × 2.
            let (p, q, r) = (hess[a][a], hess[a][b], hess[b][b]);
            let mean = f64::midpoint(p, r);
            let spread = (0.25 * (p - r) * (p - r) + q * q).sqrt();
            let (big, small) = (mean + spread, mean - spread);
            if big <= 0.0 {
                return (step, false);
            }
            let vector = |l: f64| -> (f64, f64) {
                let (x, y) = if q.abs() > 1e-300 {
                    (q, l - p)
                } else if (l - p).abs() <= (l - r).abs() {
                    (1.0, 0.0)
                } else {
                    (0.0, 1.0)
                };
                let n = x.hypot(y);
                (x / n, y / n)
            };
            for l in [big, small] {
                if l <= SEPARABLE * big {
                    continue;
                }
                let (x, y) = vector(l);
                let along = (grad[a] * x + grad[b] * y) / l;
                step[a] += along * x;
                step[b] += along * y;
            }
            (step, small > SEPARABLE * big)
        }
        _ => (step, true),
    }
}

/// Решения всех видов всех блоков по одним суммам (все данные или без одного куска) и общий ранг: (c) — `joint`, виды
/// «без группы» — с тем же рангом, остальные — без ранга. Блок без достаточных строк (`enough`) — без решений.
fn solve_set(
    die: &Die,
    sums: &[&Sums],
    shapes: &[Steps],
    enough: &[bool],
    used: [bool; RANKS],
) -> (Vec<Variants>, [f64; RANKS], bool) {
    let (phi, full, separable) = joint(die, sums, shapes, enough, used);
    let out = full
        .into_iter()
        .enumerate()
        .map(|(i, mut layer)| {
            let (nb, hc) = (die.neighbours[i].len(), shapes[i].columns());
            let tiers = rank_columns(nb, hc);
            std::array::from_fn(|v| {
                if !enough[i] {
                    return None;
                }
                if v == FULL {
                    return layer.take();
                }
                let mut how = How::default();
                if v == UNIT {
                    how.fixed.push((0, 1.0));
                }
                if ranked(v) && !die.e[i] {
                    how.fixed
                        .extend((0..RANKS).filter(|&k| used[k]).map(|k| (tiers[k], phi[k])));
                }
                sums[i].solve_how(&variant_columns(v, nb, hc), &how)
            })
        })
        .collect();
    (out, phi, separable)
}

/// Ранг в счёте (`Rank::used`): хоть у одного P-ядра годных строк с нагрузкой в этом ранге и вне его — не меньше `MIN_ROWS`.
fn rank_used(gather: &[Gather], die: &Die) -> [bool; RANKS] {
    std::array::from_fn(|k| {
        gather
            .iter()
            .zip(&die.e)
            .any(|(g, &e)| !e && g.ranks[k].iter().all(|&rows| rows >= MIN_ROWS))
    })
}

/// Подгонка по строкам файлов (все файлы — один период: тот, что показывает график).
#[must_use]
pub fn fit(logs: &[&Log], die: &Die) -> Model {
    let started = Instant::now();
    let logs = in_order(logs);
    let rows: usize = logs.iter().map(|l| l.rows()).sum();
    let n = die.len();
    // Проход 0: годные строки и опорные условия — по ним узлы формы (сплошная часть кривой); смена ранга внутри ядер.
    let (gather, references, shapes) = prepare(&logs, die, |_| true);
    let used = rank_used(&gather, die);
    let walk = Walk { die, shapes: &shapes };

    // Проход 1: суммы по кускам.
    let mut folds: Vec<Vec<Sums>> = (0..n)
        .map(|i| vec![Sums::new(width_of(die, i, &shapes)); FOLDS])
        .collect();
    walk.run(&logs, |u| folds[u.block][fold_of(u.index, rows)].add(u.x, u.y));
    let all: Vec<Sums> = folds
        .iter()
        .zip(&shapes)
        .enumerate()
        .map(|(i, (parts, _))| {
            let mut sum = Sums::new(width_of(die, i, &shapes));
            for s in parts {
                sum.plus(s);
            }
            sum
        })
        .collect();
    let enough: Vec<bool> = gather.iter().map(|g| g.usable >= MIN_ROWS).collect();

    // Решения: по всем данным и без каждого куска — общий ранг в каждой.
    let (full, phi, separable) = solve_set(die, &all.iter().collect::<Vec<_>>(), &shapes, &enough, used);
    let mut held: Vec<Vec<Variants>> = (0..n).map(|_| Vec::with_capacity(FOLDS)).collect();
    let mut phis: Vec<[f64; RANKS]> = Vec::with_capacity(FOLDS);
    for parts in (0..FOLDS).map(|k| folds.iter().map(|f| &f[k]).collect::<Vec<_>>()) {
        let train: Vec<Sums> = all.iter().zip(&parts).map(|(a, p)| a.minus(p)).collect();
        let ok: Vec<bool> = enough.iter().zip(&parts).map(|(&e, p)| e && p.n > 0.0).collect();
        let (set, phi_k, _) = solve_set(die, &train.iter().collect::<Vec<_>>(), &shapes, &ok, used);
        for (into, fits) in held.iter_mut().zip(set) {
            into.push(fits);
        }
        if ok.iter().zip(&die.e).any(|(&ok, &e)| ok && !e) {
            phis.push(phi_k);
        }
    }

    let mut blocks: Vec<BlockModel> = Vec::with_capacity(n);
    for (i, ((g, reference), fits)) in gather.iter().zip(references).zip(&full).enumerate() {
        let mut block = BlockModel {
            name: die.names[i].clone(), // a dozen names per fit
            e: die.e[i],
            usable: g.usable,
            minutes: millis(g.span_ms) / 60_000.0,
            coef: None,
            shaped: None,
            folds: Vec::new(),
            reference,
            errors: None,
            halves: None,
            diagnosis: None,
            own_bins: Vec::new(),
            influence: None,
        };
        describe(
            &mut block,
            die,
            i,
            (&all[i], &folds[i]),
            &shapes[i],
            (fits, &held[i]),
            phi,
        );
        blocks.push(block);
    }

    second_pass(&walk, &logs, rows, (&full, &held), &mut blocks);
    Model {
        blocks,
        rows,
        rank: rank_of(phi, &phis, used, separable),
        time: started.elapsed(),
    }
}

/// Общий ранг с разбросом по подгонкам без куска.
fn rank_of(phi: [f64; RANKS], folds: &[[f64; RANKS]], used: [bool; RANKS], separable: bool) -> Rank {
    let lo: [f64; RANKS] = std::array::from_fn(|k| folds.iter().map(|f| f[k]).fold(phi[k], f64::min));
    let hi: [f64; RANKS] = std::array::from_fn(|k| folds.iter().map(|f| f[k]).fold(phi[k], f64::max));
    Rank {
        value: phi,
        lo,
        hi,
        used,
        determined: std::array::from_fn(|k| {
            used[k] && separable && folds.len() == FOLDS && (lo[k] > 0.0 || hi[k] < 0.0)
        }),
    }
}

/// Годные строки блоков (`Gather`), опорные условия и корзины своего нагрева по строкам, которые берёт `keep` (по времени
/// строки): проход с пустыми формами — годность строки от формы не зависит.
fn prepare(logs: &[&Log], die: &Die, keep: impl Fn(i64) -> bool) -> (Vec<Gather>, Vec<Option<Reference>>, Vec<Steps>) {
    let n = die.len();
    let flat = vec![Steps::default(); n];
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
        if let Some(r) = u.ratio {
            g.bins.insert(whole(r));
        }
        // Смена ранга внутри ядра: строки с нагрузкой (своя быстрая p > 0) в ранге и вне его.
        if let Some(rank) = u.rank
            && u.x[1] > 0.0
        {
            for (k, tier) in (1..=2u8).enumerate() {
                g.ranks[k][usize::from(rank != tier)] += 1;
            }
        }
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
    let shapes = gather.iter().map(|g| Steps::layout(g.bins.iter().copied())).collect();
    (gather, references, shapes)
}

#[derive(Clone, Copy, Debug, Default)]
struct Spread {
    w: f64,
    wv: f64,
    wvv: f64,
}

impl Spread {
    fn add(&mut self, w: f64, v: f64) {
        self.w += w;
        self.wv += w * v;
        self.wvv += w * v * v;
    }

    fn sd(&self) -> f64 {
        if self.w <= 0.0 {
            return 0.0;
        }
        let mean = self.wv / self.w;
        (self.wvv / self.w - mean * mean).max(0.0).sqrt()
    }
}

/// Проход 2: предсказания на отложенных кусках — ошибки средних за 10 с у каждого блока по всем видам (и без каждой
/// группы входов); частные остатки по целым множителям — по (c) по всем данным; изменчивость вклада каждой группы — по
/// (c) по всем данным и без каждого куска.
fn second_pass(
    walk: &Walk,
    logs: &[&Log],
    rows: usize,
    (full, held): (&[Variants], &[Vec<Variants>]),
    blocks: &mut [BlockModel],
) {
    let n = blocks.len();
    let die = walk.die;
    let mut windows: Vec<Window> = vec![Window::default(); n];
    let mut tallies: Vec<Tally3> = vec![Tally3::default(); n];
    let mut own: Vec<std::collections::BTreeMap<i64, OwnSums>> = vec![std::collections::BTreeMap::new(); n];
    // По блоку: по подгонке (все данные, затем без каждого куска) — по группе.
    let mut spreads: Vec<[[Spread; GROUPS]; 1 + FOLDS]> = vec![[[Spread::default(); GROUPS]; 1 + FOLDS]; n];
    walk.run(logs, |u| {
        let (nb, hc) = (die.neighbours[u.block].len(), walk.shapes[u.block].columns());
        let span = millis(u.span);
        if let Some(fit) = &full[u.block][FULL] {
            let p = parts(fit, nb, hc, u.x);
            if let Some(r) = u.ratio {
                // Частный остаток: всё, кроме своей работы и предыдущих секунд, вынесено.
                let residual = u.y - fit.0 - p[DIE_GROUP] - p[NEAR_GROUP] - p[RANK_GROUP];
                own[u.block].entry(whole(r)).or_default().add(span, u.busy, residual);
            }
            for (g, v) in p.iter().enumerate() {
                spreads[u.block][0][g].add(span, *v);
            }
        }
        for (k, fits) in held[u.block].iter().enumerate() {
            if let Some(fit) = &fits[FULL] {
                for (g, v) in parts(fit, nb, hc, u.x).iter().enumerate() {
                    spreads[u.block][1 + k][g].add(span, *v);
                }
            }
        }
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
    for (i, block) in blocks.iter_mut().enumerate() {
        let t = &mut tallies[i];
        t.close(&windows[i]);
        if t.windows > 0 {
            let rms = |v: usize| (t.sse[v] / count(t.windows)).sqrt();
            block.errors = Some(Errors {
                die_only: rms(DIE_ONLY),
                own: rms(WITH_OWN),
                full: rms(FULL),
                unit: rms(UNIT),
                proxy: rms(PROXY),
                without: std::array::from_fn(|g| rms(WITHOUT + g)),
                windows: t.windows,
            });
        }
        block.own_bins = own[i]
            .iter()
            .filter_map(|(&r, sums)| sums.bin(i32::try_from(r).ok()?))
            .collect();
        if full[i][FULL].is_some() {
            let solved: Vec<usize> = (0..FOLDS).filter(|&k| held[i][k][FULL].is_some()).collect();
            let size: [f64; GROUPS] = std::array::from_fn(|g| spreads[i][0][g].sd());
            let range = |g: usize| {
                solved
                    .iter()
                    .map(|&k| spreads[i][1 + k][g].sd())
                    .fold((size[g], size[g]), |(lo, hi), v| (lo.min(v), hi.max(v)))
            };
            block.influence = Some(Influence {
                size,
                lo: std::array::from_fn(|g| range(g).0),
                hi: std::array::from_fn(|g| range(g).1),
                ablation: std::array::from_fn(|g| block.errors.map(|e| e.without[g] - e.full)),
            });
        }
    }
}

/// Всё о блоке из его решений: (c) и прежний вид по всем данным, (c) без каждого куска, диагностика, половины, «°C на
/// ступень». `sums` — по всем данным и по кускам; `fits` — решения по всем данным и без каждого куска.
fn describe(
    block: &mut BlockModel,
    die: &Die,
    i: usize,
    (all, parts): (&Sums, &[Sums]),
    shape: &Steps,
    (fits, held): (&Variants, &[Variants]),
    phi: [f64; RANKS],
) {
    let nb = die.neighbours[i].len();
    block.coef = fits[PROXY].as_ref().map(|f| Coef::of(die, i, shape, false, f));
    block.shaped = fits[FULL].as_ref().map(|f| Coef::of(die, i, shape, true, f));
    block.folds = held
        .iter()
        .filter_map(|f| f[FULL].as_ref())
        .map(|f| Coef::of(die, i, shape, true, f))
        .collect();
    let tiers = rank_columns(nb, shape.columns());
    // Диагностика отчёта: L3 с формой без неотрицательности (ранг — общий) и при β = 1.
    block.diagnosis = fits[FULL].as_ref().and_then(|_| {
        let fixed = if die.e[i] {
            Vec::new()
        } else {
            tiers.iter().zip(phi).map(|(&t, v)| (t, v)).collect()
        };
        let signed = all.solve_how(
            &variant_columns(FULL, nb, shape.columns()),
            &How { signed: true, fixed },
        )?;
        Some(Diagnosis {
            signed: Coef::of(die, i, shape, true, &signed),
            unit: Coef::of(die, i, shape, true, fits[UNIT].as_ref()?),
        })
    });
    block.halves = fits[PROXY].as_ref().and_then(|_| {
        let mut first = parts[0].clone(); // one small matrix per block
        first.plus(&parts[1]);
        let second = all.minus(&first);
        let columns = variant_columns(PROXY, nb, shape.columns());
        let total = |s: &Sums| {
            s.solve(&columns)
                .map(|f| Coef::of(die, i, shape, false, &f).own_total())
        };
        let (early, late) = (total(&first)?, total(&second)?);
        (early.min(late) > 0.0).then(|| early.max(late) / early.min(late))
    });
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

// ----- хранение строк -----

/// Что сделало хранение (`retain`): строк до и после, клеток (блок, множитель), достигших `SATURATED`, и байт после.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Retained {
    pub before: usize,
    pub after: usize,
    pub saturated: usize,
    pub cells: usize,
    pub bytes: usize,
}

/// Клетка хранения: блок и целый множитель строки (None — множителя нет: ядро спало или не известно).
type Cell = (usize, Option<i64>);

/// Хранение строк для модели (бриф W 16.10, вместо границы 8 ч): по клетке (блок, целый множитель) — частный остаток
/// годных строк блока по модели `model` (T − свободный член − кристалл − соседи − ранг, °C), средние по окнам 10 с, от
/// новейших к старшим; как только окон не меньше `MIN_WINDOWS` и стандартная ошибка средней по окнам — не больше
/// `SATURATED`, клетка насыщена: её строки старше этих окон блоку не нужны. Строка остаётся, если она нужна хоть одному
/// блоку, — и с ней строки её средних (16 с до неё в том же непрерывном куске) и следующая (pfast); и последние
/// `LIVE_MS` журнала — к ним дописываются новые строки. Блок без модели держит все свои строки.
///
/// Остаток здесь не делится на занятость (в отличие от точек графика): иначе клетки спящих и слегка занятых ядер — самые
/// частые строки — не насыщались бы никогда, и ничего бы не выбрасывалось.
pub fn retain(logs: &mut [&mut Log], die: &Die, model: &Model) -> Retained {
    let mut order: Vec<usize> = (0..logs.len()).filter(|&k| logs[k].rows() > 0).collect();
    order.sort_by_key(|&k| logs[k].times[0]);
    let before: usize = logs.iter().map(|l| l.rows()).sum();
    let keep = {
        let views: Vec<&Log> = order.iter().map(|&k| &*logs[k]).collect();
        needed_rows(&views, die, model)
    };
    let mut at = 0;
    for &k in &order {
        let rows = logs[k].rows();
        logs[k].keep(&keep.rows[at..at + rows]);
        at += rows;
    }
    Retained {
        before,
        after: logs.iter().map(|l| l.rows()).sum(),
        saturated: keep.saturated,
        cells: keep.cells,
        bytes: logs.iter().map(|l| l.bytes()).sum(),
    }
}

/// Какие строки (по порядку всех файлов `logs`, уже упорядоченных по времени) нужны модели.
struct Needed {
    rows: Vec<bool>,
    saturated: usize,
    cells: usize,
}

fn needed_rows(logs: &[&Log], die: &Die, model: &Model) -> Needed {
    let n = die.len();
    // Годность строки от формы не зависит; столбцы ранга — последние два входа строки при любой форме.
    let flat = vec![Steps::default(); n];
    let fitted = model.blocks.len() == n && model.blocks.iter().zip(&die.names).all(|(b, name)| &b.name == name);
    let walk = Walk { die, shapes: &flat };
    let coef = |i: usize| fitted.then(|| model.blocks[i].shaped.as_ref()).flatten();
    // Окна 10 с клетки по порядку времени: (окно, Σ остатка, строк).
    let mut cells: std::collections::HashMap<Cell, Vec<(i64, f64, u32)>> = std::collections::HashMap::new();
    walk.run(logs, |u| {
        let Some(c) = coef(u.block) else { return };
        let residual = u.y - c.a - c.beta * u.x[0] - near_part(c, u.x) - rank_part(c, u);
        let windows = cells.entry((u.block, u.ratio.map(whole))).or_default();
        let window = u.at.div_euclid(WINDOW_MS);
        match windows.last_mut() {
            Some(last) if last.0 == window => {
                last.1 += residual;
                last.2 += 1;
            }
            _ => windows.push((window, residual, 1)),
        }
    });
    // Клетка насыщена — с какого окна её строки нужны.
    let mut from: std::collections::HashMap<Cell, i64> = std::collections::HashMap::new();
    for (cell, windows) in &cells {
        let (mut k, mut mean, mut m2) = (0u32, 0.0, 0.0);
        for &(window, sum, rows) in windows.iter().rev() {
            let v = sum / f64::from(rows);
            k += 1;
            let delta = v - mean;
            mean += delta / f64::from(k);
            m2 += delta * (v - mean);
            if k >= MIN_WINDOWS && (m2 / f64::from(k - 1) / f64::from(k)).sqrt() <= SATURATED {
                from.insert(*cell, window);
                break;
            }
        }
    }
    let total: usize = logs.iter().map(|l| l.rows()).sum();
    let mut keep = vec![false; total];
    let mut needed: Vec<usize> = Vec::new();
    walk.run(logs, |u| {
        let saturated_from = coef(u.block).and(from.get(&(u.block, u.ratio.map(whole))));
        if saturated_from.is_none_or(|&w| u.at.div_euclid(WINDOW_MS) >= w) && !keep[u.index] {
            keep[u.index] = true;
            needed.push(u.index);
        }
    });
    // Времена всех строк по порядку — для средних перед нужными и последних `LIVE_MS`.
    let times: Vec<i64> = logs.iter().flat_map(|l| l.times.iter().copied()).collect();
    needed.sort_unstable();
    for &i in &needed {
        // Следующая строка (pfast) и 16 с перед строкой в том же непрерывном куске (до уже отмеченной).
        if i + 1 < total && times[i + 1] - times[i] > 0 && times[i + 1] - times[i] <= GAP_MS {
            keep[i + 1] = true;
        }
        let mut j = i;
        while j > 0 {
            let gap = times[j] - times[j - 1];
            if gap <= 0 || gap > GAP_MS || times[i] - times[j - 1] > WARM_MS || keep[j - 1] {
                break;
            }
            keep[j - 1] = true;
            j -= 1;
        }
    }
    if let Some(&newest) = times.iter().max() {
        for (k, &t) in keep.iter_mut().zip(&times) {
            *k |= t >= newest - LIVE_MS;
        }
    }
    Needed {
        rows: keep,
        saturated: from.len(),
        cells: cells.len(),
    }
}

/// Вклад соседей в строке по коэффициентам блока.
fn near_part(c: &Coef, x: &[f64]) -> f64 {
    c.neighbours
        .iter()
        .enumerate()
        .map(|(q, n)| n.fast * x[OWN + 1 + 2 * q] + n.slow * x[OWN + 2 + 2 * q])
        .sum()
}

/// Вклад ранга в строке (столбцы ранга — последние два входа строки).
fn rank_part(c: &Coef, u: &Usable) -> f64 {
    let at = u.x.len() - RANKS;
    c.rank.iter().zip(&u.x[at..]).map(|(r, x)| r * x).sum()
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
    /// своя быстрая нагрузка (форма или K·pfast), средние своей p, соседи, ранг; и средняя своя занятость и множитель там.
    pub parts: [[f64; 5]; 2],
    /// Общий ранг P-ядер подгонки до момента.
    pub rank: [f64; RANKS],
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
    // Узлы формы и смена ранга — по строкам подгонки (до момента).
    let (gather, _, shapes) = prepare(&logs, die, |at| at < at_ms);
    let used = rank_used(&gather, die);
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
    let enough: Vec<bool> = trained.iter().map(|&t| t >= MIN_ROWS).collect();
    let (fits, rank, _) = solve_set(die, &train.iter().collect::<Vec<_>>(), &shapes, &enough, used);
    let mut after = vec![(0.0, [0.0; VARIANTS], 0usize); n];
    let mut level_test = vec![0.0; n];
    let mut split_parts = vec![[[0.0; 5]; 2]; n];
    let mut load = vec![(0.0, 0.0, 0usize); n];
    walk.run(&logs, |u| {
        if u.at >= at_ms && Some(u.log) == home {
            let (nb, hc) = (die.neighbours[u.block].len(), shapes[u.block].columns());
            for (into, v) in split_parts[u.block].iter_mut().zip([FULL, PROXY]) {
                if let Some(fit) = &fits[u.block][v] {
                    let p = parts(fit, nb, hc, u.x);
                    into[0] += fit.0 + p[DIE_GROUP] - u.x[0];
                    into[1] += p[OWN_GROUP];
                    into[2] += p[HISTORY_GROUP];
                    into[3] += p[NEAR_GROUP];
                    into[4] += p[RANK_GROUP];
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
                parts: split_parts[i].map(|p| p.map(|v| mean(v, k).unwrap_or(0.0))),
                rank,
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
        synthetic_with(rows, gap_at, None)
    }

    /// Свой нагрев блока 1 по целым множителям в `synthetic_with(.., Some(STEPPED))`: не выпуклый — площадка ×41–×42 и
    /// скачок на ×44 (ломаной с узлами не представим), не падает.
    const STEPPED: [(i32, f64); 6] = [(40, 4.0), (41, 4.5), (42, 4.5), (43, 7.0), (44, 11.0), (45, 11.5)];

    /// Нагрев из таблицы по целому множителю строки.
    fn table_heat(table: &[(i32, f64)], ratio: f64) -> f64 {
        table
            .iter()
            .find(|(r, _)| i64::from(*r) == whole(ratio))
            .map_or(0.0, |(_, h)| *h)
    }

    /// То же; `table` — у блока 1 своя быстрая нагрузка не 6·pfast, а занятость × нагрев из таблицы по целому множителю
    /// (среднее строк t и t+1, как у модели), без своих средних.
    fn synthetic_with(rows: u32, gap_at: Option<u32>, table: Option<&[(i32, f64)]>) -> (String, Die) {
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
        let mut text = String::from("time,0.ratio,0.busy,0.temp,1.ratio,1.busy,1.temp,2.ratio,2.busy,2.temp\n");
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
                r[ju] = ratio(j, k);
                b[ju] = (busy(j, k) * 100.0).round() / 100.0;
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
            let cur = rowsv[k];
            let next = rowsv.get(k + 1).copied().unwrap_or(cur);
            let Generated { ema, p, .. } = cur;
            let fast = |j: usize| f64::midpoint(p[j], next.p[j]);
            let stepped = |g: &Generated| g.b[1] * table_heat(table.unwrap_or(&[]), g.r[1]);
            let own = |j: usize| match table {
                Some(_) if j == 1 => f64::midpoint(stepped(&cur), stepped(&next)),
                _ => 6.0 * fast(j) + 9.0 * ema[j][3] + 3.0 * ema[j][5],
            };
            let near = |j: usize| 2.0 * fast(j) + 4.0 * ema[j][3];
            let base = [own(0) + near(1), own(1) + near(0) + near(2), own(2) + near(1)];
            // T_i = 20 + D_i + base_i, D_i = (Σ T − T_i)/2 → T_i − (ΣT − T_i)/2 = 20 + base_i; решение для трёх.
            let sum_base: f64 = base.iter().sum();
            // Σ: ΣT − (2ΣT)/2... уровень вырожден при β = 1; берём β = 0,5: T_i = 20 + 0,5·(S − T_i)/2 + base_i.
            // T_i (1 + 0,25) = 20 + 0,25·S + base_i; S·(1,25) = 60 + 0,75·S... → S = (60 + Σbase) / 0,5.
            let s = (60.0 + sum_base) / 0.5;
            temps.push(std::array::from_fn(|i| (20.0 + 0.25 * s + base[i]) / 1.25));
        }
        for (k, Generated { at, r, b, .. }) in rowsv.iter().enumerate() {
            let ms = at.rem_euclid(1000);
            let secs = at / 1000;
            let _ = std::fmt::Write::write_fmt(
                &mut text,
                format_args!("2026-10-15 20:{:02}:{:02}.{ms:03}", secs / 60 % 60, secs % 60),
            );
            for j in 0..3 {
                let _ = std::fmt::Write::write_fmt(&mut text, format_args!(",{:.2},{:.2},{}", r[j], b[j], temps[k][j]));
            }
            text.push('\n');
        }
        (text, die)
    }

    #[test]
    fn steps_are_cumulative_sums_flat_beyond_the_bins() {
        // Нагрев на корзине — сумма весов корзин не выше неё: ×40 4, ×41 4,5, ×42 4,5 (вес 0 — площадка), ×43 7, ×44 11,
        // ×45 11,5; ниже первой корзины 0, выше последней — вся сумма; множитель строки — к ближайшему целому.
        let s = Steps {
            bins: vec![40, 41, 42, 43, 44, 45],
            weights: vec![4.0, 0.5, 0.0, 2.5, 4.0, 0.5],
        };
        for (r, want) in [
            (39.4, 0.0),
            (40.0, 4.0),
            (41.0, 4.5),
            (42.0, 4.5),
            (43.0, 7.0),
            (44.4, 11.0),
            (44.5, 11.5),
            (60.0, 11.5),
        ] {
            assert!((s.at(r) - want).abs() < 1e-12, "g(×{r}) = {} вместо {want}", s.at(r));
        }
        let mut out = [9.0; 6];
        s.basis(42.0, 0.5, &mut out);
        assert_eq!(
            out,
            [0.5, 0.5, 0.5, 0.0, 0.0, 0.0],
            "столбцы — занятость до своей корзины включительно"
        );
        assert!((Steps::default().at(45.0)).abs() < 1e-15, "пустые веса — g ≡ 0");
        assert_eq!(
            Steps::layout([45, 40, 42, 40]).bins,
            [40, 42, 45],
            "корзины по возрастанию, без повторов"
        );
    }

    /// Журнал со ступенчатым нагревом блока 1 (`STEPPED`) и пятью строками на ×19 (короткий провал — корзина без данных).
    fn stepped_log() -> (Log, Die) {
        let (csv, die) = synthetic_with(6000, None, Some(&STEPPED));
        let mut lines: Vec<String> = csv.lines().map(str::to_string).collect();
        for line in &mut lines[3001..3006] {
            let mut fields: Vec<&str> = line.split(',').collect();
            fields[4] = "19.00";
            fields[5] = "0.90";
            *line = fields.join(",");
        }
        (log_from_csv(&(lines.join("\n") + "\n")), die)
    }

    #[test]
    fn own_heating_per_multiplier_comes_back_from_a_synthetic_log_and_never_falls() {
        // Бриф 16.10: у блока по коэффициенту на целый множитель, значения — данных, не формулы: площадка ×41–×42 и
        // скачок на ×44 возвращаются (ломаная с узлами через 2 ступени дала бы наклон вместо площадки); веса ≥ 0 — нагрев
        // не падает ни на одной корзине и за краями данных; свои средние у блока без истории — около нуля.
        let (log, die) = stepped_log();
        let model = fit(&[&log], &die);
        let b = &model.blocks[1];
        let c = b.shaped.as_ref().expect("слой со ступенями");
        assert_eq!(
            c.shape.bins,
            [19, 40, 41, 42, 43, 44, 45],
            "корзины — целые множители годных строк"
        );
        assert!(c.shape.weights.iter().all(|&w| w >= 0.0), "{:?}", c.shape.weights);
        for (r, want) in STEPPED {
            let got = b.heat(c, f64::from(r));
            assert!((got - want).abs() < 0.2, "×{r}: нагрев {got:.3} вместо {want}");
        }
        assert!(c.own_total() < 0.3, "свои средние без истины: {:?}", c.own);
        let heats: Vec<f64> = (300..=600).map(|r| b.heat(c, f64::from(r) / 10.0)).collect();
        assert!(heats.windows(2).all(|w| w[1] >= w[0] - 1e-12), "нагрев падает");
        // Блок 0 с прежней истиной (6·pfast + средние): его нагрев тоже не падает.
        let a = &model.blocks[0];
        let ca = a.shaped.as_ref().expect("слой");
        let other: Vec<f64> = (300..=600).map(|r| a.heat(ca, f64::from(r) / 10.0)).collect();
        assert!(other.windows(2).all(|w| w[1] >= w[0] - 1e-12));
    }

    #[test]
    fn efficiency_is_the_multiplier_over_the_own_heating_with_its_fold_range_and_needs_data() {
        // Эффективность корзины = r / нагрев по всем данным; разброс — по подгонкам без куска и содержит значение; только
        // корзины с минутой полной нагрузки: ×19 (пять строк) есть в корзинах и в частных остатках, но точки нет.
        let (log, die) = stepped_log();
        let model = fit(&[&log], &die);
        let b = &model.blocks[1];
        let points = b.efficiency();
        let ratios: Vec<i32> = points.iter().map(|e| e.ratio).collect();
        assert_eq!(ratios, [40, 41, 42, 43, 44, 45], "{points:?}");
        assert!(
            b.own_bins.iter().any(|p| p.ratio == 19),
            "корзина ×19 в частных остатках есть"
        );
        for e in &points {
            let r = f64::from(e.ratio);
            let heat = b.own_heat(r).expect("нагрев");
            assert!(
                (e.heat - heat).abs() < 1e-12 && (e.value - r / heat).abs() < 1e-12,
                "{e:?}"
            );
            assert!(e.lo <= e.value + 1e-12 && e.value <= e.hi + 1e-12, "{e:?}");
            let (lo, hi) = b.own_heat_range(r).expect("разброс");
            assert!(
                lo > 0.0 && (e.lo - r / hi).abs() < 1e-12 && (e.hi - r / lo).abs() < 1e-12,
                "{e:?}"
            );
            assert!(e.secs * e.load >= MIN_BIN_SECS, "{e:?}");
            let want = r / table_heat(&STEPPED, r);
            assert!((e.value - want).abs() < 0.05 * want, "×{r}: {} вместо {want}", e.value);
        }
        // Без четырёх подгонок или без 10 минут данных точек нет.
        let (short, die) = synthetic_with(900, None, Some(&STEPPED));
        let brief = fit(&[&log_from_csv(&short)], &die);
        assert!(!brief.blocks[1].drawn() && brief.blocks[1].efficiency().is_empty());
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
