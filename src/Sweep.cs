// Тепловое измерение: нагружает блоки по одному и записывает, насколько нагрелся сам блок
// и все остальные ядра. Блок — P-ядро или кластер E-ядер целиком: у кластера общие температура
// и потолок множителя, отдельно его ядрами управлять нельзя, поэтому он нагружается весь разом.
// Результат хранится по ядрам: у всех ядер блока строка одна и та же.
//
// Как устроено и от чего защищено:
// - порядок вразброс: следующее ядро берётся подальше от двух предыдущих, чтобы остаточное тепло
//   не попадало на соседей измеряемого ядра;
// - перед каждым ядром весь кристалл должен вернуться к исходной температуре;
// - исходная температура берётся до и после нагрузки, в расчёт идёт меньшая: фоновые задачи только греют;
// - проба одинаковой лёгкой нагрузкой на каждом ядре: второй тест (кто как греется при равной
//   работе) и подбор силы для основного измерения;
// - несколько проходов в разном порядке: результат — среднее, расхождение проходов показывает,
//   насколько данным можно верить.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace CoreMap
{
    class ThermalSweep : Measurement
    {
        // Замер на 275HX: нагрев самого ядра и соседей выходит на ровное значение за 5–10 секунд
        // нагрузки и дальше не растёт. Первая половина времени — выход на режим, вторая — усреднение.
        public static readonly int[] Durations = { 10, 20, 40 };
        public static readonly int[] PassChoices = { 1, 2, 3 };

        const double BaseSeconds = 5;     // усреднение без нагрузки, до и после каждого ядра
        const double CoolMinSeconds = 3, CoolMaxSeconds = 30;
        const double CoolMargin = 1.5;    // остыло, если ни одно ядро не теплее исходного больше чем на столько
        // Одна и та же нагрузка греет ядра по-разному: на 275HX ядро 0 держится на 84 °C там, где
        // ядра 1, 11 и 13 за секунду доходят до порога 100 °C. Поэтому сила подбирается под ядро.
        const double ProbeSeconds = 5;
        const double Ceiling = 88;        // выше этой расчётной температуры ядро не нагружается
        const double StepDown = 92;       // если ядро всё же дошло до этой температуры — ступень вниз
        // Во сколько раз нагрев под силой 2 и 3 больше, чем под силой 1. Здесь значения до подбора;
        // подбор нагрузки (Calibration) ставит измеренные.
        public static readonly double[] Rise = { 0, 1.0, 1.5, 1.9 };

        readonly Monitor monitor;
        readonly int[] lps;
        readonly Block[] blocks;
        readonly int n;
        readonly int[] order;    // порядок обхода блоков
        readonly string saveDir;  // null — результат не сохранять (имитация)
        double startedAt;

        // Результат пишет поток измерения, читает окно: всё, что его трогает, идёт под замком, окну отдаются копии.
        readonly object gate = new object();
        List<double[][]> passes = new List<double[][]>();  // [проход][нагруженное ядро] → нагрев всех ядер
        List<double[][]> probes = new List<double[][]>();  // [проход][ядро] → {нагрев, прирост Вт, скорость}
        public bool[] Throttled;  // под нагрузкой упёрлось в порог и сбрасывало частоту
        public int[] Levels;      // подобранная сила нагрузки, 0 — ещё не подобрана
        public int Seconds = 20, PassCount = 2;
        public int Done;
        public volatile Block Current;  // блок, который нагружен сейчас; null — никакой

        public int Total { get { return blocks.Length * PassCount; } }
        public double Elapsed { get { return Native.Now() - startedAt; } }

        public ThermalSweep(Runner runner, Monitor monitor, int[] lps, Block[] blocks, double[][] positions, string saveDir)
            : base(runner, "тепловое измерение прервано")
        {
            this.monitor = monitor;
            this.lps = lps;
            this.blocks = blocks;
            this.saveDir = saveDir;
            n = lps.Length;
            order = SpreadOrder(blocks.Select(b => new[] { b.Members.Average(i => positions[i][0]), b.Members.Average(i => positions[i][1]) }).ToArray());
            Throttled = new bool[n];
            Levels = new int[n];
        }

        /// Порядок обхода, в котором каждое следующее ядро далеко на схеме от двух предыдущих.
        /// Жадный выбор «самое дальнее» оставляет соседей на конец обхода, поэтому перебирается
        /// несколько сотен случайных вариантов и берётся тот, где самая близкая пара подряд идущих
        /// ядер дальше всего. Зерно фиксировано: порядок одинаков от запуска к запуску.
        static int[] SpreadOrder(double[][] pos)
        {
            int count = pos.Length;
            Func<int, int, double> dist = (a, b) =>
                Math.Sqrt(Math.Pow(pos[a][0] - pos[b][0], 2) + Math.Pow(pos[a][1] - pos[b][1], 2));
            Func<List<int>, double[]> quality = o =>
            {
                double near = double.MaxValue, skip = double.MaxValue, sum = 0;
                for (int i = 0; i + 1 < o.Count; i++) { double d = dist(o[i], o[i + 1]); near = Math.Min(near, d); sum += d; }
                for (int i = 0; i + 2 < o.Count; i++) skip = Math.Min(skip, dist(o[i], o[i + 2]));
                return new[] { near, skip, sum };
            };
            Func<double[], double[], bool> better = (a, b) =>
            {
                for (int i = 0; i < 3; i++) if (a[i] != b[i]) return a[i] > b[i];
                return false;
            };
            var rng = new Random(0);
            List<int> best = Enumerable.Range(0, count).ToList();
            double[] bestQuality = quality(best);
            for (int attempt = 0; attempt < 500; attempt++)
            {
                List<int> rest = Enumerable.Range(0, count).ToList();
                var o = new List<int>();
                int first = rng.Next(rest.Count);
                o.Add(rest[first]);
                rest.RemoveAt(first);
                while (rest.Count > 0)
                {
                    int from = Math.Max(0, o.Count - 2);
                    rest = rest.OrderByDescending(c => Enumerable.Range(from, o.Count - from).Min(k => dist(c, o[k]))).ToList();
                    int pick = rng.Next(Math.Min(3, rest.Count));
                    o.Add(rest[pick]);
                    rest.RemoveAt(pick);
                }
                double[] q = quality(o);
                if (better(q, bestQuality)) { best = o; bestQuality = q; }
            }
            return best.ToArray();
        }

        public void Start(int seconds, int passes)
        {
            if (monitor == null || !Durations.Contains(seconds) || !PassChoices.Contains(passes)) return;
            Launch(() => Run(seconds, passes));
        }

        void Run(int seconds, int passCount)
        {
            Seconds = seconds;
            PassCount = passCount;
            lock (gate)
            {
                passes = new List<double[][]>();
                probes = new List<double[][]>();
            }
            Throttled = new bool[n];
            Levels = new int[n];
            Done = 0;
            startedAt = Native.Now();
            Version++;
            Settle();
            double[] baseTemps = monitor.Typical(BaseSeconds);
            double baseWatts = monitor.Power(BaseSeconds);
            for (int p = 0; p < PassCount; p++)
            {
                lock (gate)
                {
                    passes.Add(new double[n][]);
                    probes.Add(new double[n][]);
                }
                // Обратный порядок во втором проходе: у каждого ядра другой предшественник.
                foreach (int u in p % 2 == 0 ? order : order.Reverse().ToArray())
                    if (!Measure(p, blocks[u], ref baseTemps, ref baseWatts)) return;
            }
        }

        protected override void Finish()
        {
            Current = null;
            // Записывается и прерванное измерение, если первый проход пройден целиком.
            try { if (saveDir != null && passes.Count > 0 && passes[0].All(row => row != null)) Save(); }
            catch (Exception e) { Program.Log("тепловое измерение не сохранено: " + e); }
        }

        /// Запускает нагрузку на всех ядрах блока с подобранной для него силой.
        void Restart(Block block)
        {
            foreach (int m in block.Members)
            {
                Load.Stop(m);
                Load.Start(m, Levels[m]);
            }
        }

        /// Проба одинаковой для всех блоков лёгкой нагрузкой. Записывает, насколько блок нагрелся сам,
        /// сколько мощности добавилось и с какой скоростью он считает (сумма по его ядрам), и подбирает
        /// силу для основного измерения: самую тяжёлую, с которой блок по расчёту не выйдет за Ceiling.
        /// Блок остаётся под лёгкой нагрузкой.
        void Probe(int p, Block block, double baseTemp, double baseWatts)
        {
            int[] members = block.Members;
            int first = block.First;
            foreach (int m in members) Load.Start(m, 1);
            if (Wait(ProbeSeconds)) return;
            double now = block.Temp(monitor.Typical(ProbeSeconds / 2));
            if (double.IsNaN(now) || double.IsNaN(baseTemp)) return;
            double rise = now - baseTemp;
            var probe = new[]
            {
                Math.Round(rise, 1), Math.Round(monitor.Power(ProbeSeconds / 2) - baseWatts, 1), Math.Round(members.Sum(m => Load.Rate(m))),
            };
            lock (gate)
                foreach (int m in members) probes[p][m] = probe;
            Version++;
            if (Levels[first] != 0) return;
            int level = LoadManager.Levels.Where(lv => lv <= Load.Level && baseTemp + rise * Rise[lv] <= Ceiling).DefaultIfEmpty(1).Max();
            foreach (int m in members) Levels[m] = level;
        }

        /// Нагружает один блок. false — измерение остановлено.
        bool Measure(int p, Block block, ref double[] baseTemps, ref double baseWatts)
        {
            int[] members = block.Members;
            int first = block.First;
            Current = block;
            Probe(p, block, block.Temp(baseTemps), baseWatts);
            if (Levels[first] == 0)
                foreach (int m in members) Levels[m] = 1;
            Restart(block);
            double end = Native.Now() + Seconds;
            int hotSamples = 0;
            while (Native.Now() < end && !Wait(0.5))
            {
                foreach (int m in members)
                    if (monitor.Throttled[m]) Throttled[m] = true;
                // Расчёт ошибся и блок всё же слишком горячий: ступень вниз и отсчёт времени заново.
                hotSamples = block.Temp(monitor.Cur) >= StepDown ? hotSamples + 1 : 0;
                if (hotSamples >= 2 && Levels[first] > 1)
                {
                    foreach (int m in members) Levels[m]--;
                    Restart(block);
                    end = Native.Now() + Seconds;
                    hotSamples = 0;
                }
            }
            double[] hot = monitor.Typical(Seconds / 2.0);
            foreach (int m in members) Load.Stop(m);
            Cool(baseTemps);
            if (Wait(BaseSeconds)) return false;
            double[] after = monitor.Typical(BaseSeconds);
            // Фоновые задачи только греют, поэтому исходной считается меньшая из температур до и после.
            var row = new double[n];
            for (int j = 0; j < n; j++) row[j] = Math.Round(hot[j] - Math.Min(baseTemps[j], after[j]), 1);
            lock (gate)
                foreach (int m in members) passes[p][m] = row;
            baseTemps = after;
            baseWatts = monitor.Power(BaseSeconds);
            Done++;
            Version++;
            return true;
        }

        /// Ждёт, пока кристалл перестанет остывать после предыдущей нагрузки (не дольше минуты).
        void Settle()
        {
            double before = double.NaN;
            for (int k = 0; k < 60 / BaseSeconds; k++)
            {
                if (Wait(BaseSeconds + 0.5)) return;
                double now = monitor.Typical(BaseSeconds).Where(t => !double.IsNaN(t)).DefaultIfEmpty(double.NaN).Average();
                if (!double.IsNaN(before) && !(before - now >= 0.5)) return;
                before = now;
            }
        }

        /// Ждёт, пока весь кристалл, а не только нагруженное ядро, вернётся к исходной температуре.
        void Cool(double[] baseTemps)
        {
            double started = Native.Now();
            while (!Wait(1))
            {
                double waited = Native.Now() - started;
                double[] now = monitor.Typical(2);
                bool cooled = !Enumerable.Range(0, n).Any(j => now[j] - baseTemps[j] > CoolMargin);
                if (waited >= CoolMaxSeconds || (waited >= CoolMinSeconds && cooled)) return;
            }
        }

        // ----- результат -----

        /// Копия проходов для расчёта в потоке окна. Строка прохода (нагрев всех ядер) после записи не меняется,
        /// поэтому копируются только списки строк.
        double[][][] Taken(List<double[][]> list)
        {
            lock (gate) return list.Select(p => (double[][])p.Clone()).ToArray();
        }

        /// Среднее по проходам (mean) и половина размаха между ними (spread) для каждой ячейки.
        public void Combined(out double[][] mean, out double[][] spread)
        {
            mean = new double[n][];
            spread = new double[n][];
            double[][][] taken = Taken(passes);
            for (int i = 0; i < n; i++)
            {
                List<double[]> rows = taken.Where(p => p[i] != null).Select(p => p[i]).ToList();
                if (rows.Count == 0) continue;
                mean[i] = new double[n];
                spread[i] = new double[n];
                for (int j = 0; j < n; j++)
                {
                    List<double> values = rows.Select(r => r[j]).Where(v => !double.IsNaN(v)).ToList();
                    mean[i][j] = values.Count > 0 ? Math.Round(values.Average(), 1) : double.NaN;
                    spread[i][j] = values.Count > 1 ? Math.Round((values.Max() - values.Min()) / 2, 1) : double.NaN;
                }
            }
        }

        /// Среднее по проходам для пробы каждого ядра: 0 — нагрев, 1 — прирост мощности, 2 — скорость.
        public double[] Own(int what)
        {
            var result = new double[n];
            double[][][] taken = Taken(probes);
            for (int i = 0; i < n; i++)
                result[i] = taken.Where(p => p[i] != null && !double.IsNaN(p[i][what])).Select(p => p[i][what])
                    .DefaultIfEmpty(double.NaN).Average();
            return result;
        }

        void Save()
        {
            Func<List<double[][]>, object> rows = list => list.Select(p => (object)p.Select(r => (object)r).ToArray()).ToArray();
            Json.WriteFile(Path.Combine(saveDir, DateTime.Now.ToString("'thermal-'yyyyMMdd-HHmmss'.json'")), new Dictionary<string, object>
            {
                { "lps", lps }, { "blocks", blocks.Select(b => (object)b.Members.Select(i => lps[i]).ToArray()).ToArray() },
                { "order", order.Select(u => blocks[u].Lp).ToArray() }, { "load_seconds", Seconds },
                { "levels", Levels }, { "throttled", Enumerable.Range(0, n).Where(i => Throttled[i]).Select(i => lps[i]).ToArray() },
                { "passes", rows(passes) }, { "probes", rows(probes) },
            });
        }

        public void LoadSaved(string path)
        {
            Dictionary<string, object> saved = Json.ReadFile(path);
            if (!Json.Ints(saved["lps"]).SequenceEqual(lps)) return;
            var readPasses = new List<double[][]>();
            var readProbes = new List<double[][]>();
            if (saved.ContainsKey("passes"))
            {
                foreach (object p in (System.Collections.IEnumerable)saved["passes"]) readPasses.Add(Json.Rows(p).ToArray());
                foreach (object p in (System.Collections.IEnumerable)saved["probes"]) readProbes.Add(Json.Rows(p).ToArray());
                Levels = Json.Ints(saved["levels"]);
                int[] throttled = Json.Ints(saved["throttled"]);
                Throttled = lps.Select(lp => throttled.Contains(lp)).ToArray();
            }
            else
            {
                // Первый вариант измерения: один проход, все ядра на полной силе, пробы не было.
                readPasses.Add(Json.Rows(saved["matrix"]).ToArray());
                Levels = Enumerable.Repeat(3, n).ToArray();
            }
            lock (gate)
            {
                passes = readPasses;
                probes = readProbes;
            }
            PassCount = readPasses.Count;
            Done = readPasses.Sum(p => blocks.Count(b => p[b.First] != null));
            Finished = File.GetLastWriteTime(path).ToString("dd.MM HH:mm");
            Version++;
        }
    }
}
