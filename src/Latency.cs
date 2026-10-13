// Задержки между ядрами: время передачи одной строки кэша от ядра A к ядру B.
//
// Два потока, привязанные к A и B, перебрасывают одну ячейку памяти туда-обратно; результат —
// время в одну сторону, наносекунды. Время зависит от того, КАКАЯ строка кэша передаётся, а не
// только от пары ядер: на 275HX одна и та же пара даёт от 28 до 43 нс для разных строк, и на одной
// строке какое-то ядро выглядит «быстрым» со всеми. Поэтому каждая пара меряется на многих
// строках, разбросанных по большому массиву, и усредняется.
//
// Нагрузку здесь дают сами потоки замера, а не LoadManager: два ядра заняты, пока меряется их пара.
// Защита от перегрева снимает это измерение наравне с остальными — через исполнителя измерений.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;

namespace CoreMap
{
    class LatencyTest : Measurement
    {
        const int LineCount = 48;      // строк кэша на пару ядер
        const int Iterations = 3000;   // передач туда-обратно в одном замере
        const int SampleCount = 2;     // замеров на строку, берётся лучший
        const long Ping = 1, Pong = 2, Quit = 3;
        const int WordsPerLine = 8, TotalLines = 8192;  // массив 512 КБ
        static readonly long[] memory = new long[TotalLines * WordsPerLine];

        readonly int[] lps;
        readonly int n;
        readonly bool dry;        // имитация: ядра не нагружаются, числа ненастоящие
        readonly string saveDir;  // null — результат не сохранять (имитация)

        public double[][] Matrix, Lowest, Highest;  // среднее, лучшая и худшая строка; NaN — не измерено
        public int Done;

        public int Total { get { return n * (n - 1) / 2; } }

        /// dry — как у LoadManager: в имитации температуры ненастоящие, защита от перегрева их не видит,
        /// поэтому настоящую нагрузку давать нельзя.
        public LatencyTest(Runner runner, int[] lps, bool dry, string saveDir)
            : base(runner, null)
        {
            this.lps = lps;
            this.dry = dry;
            this.saveDir = saveDir;
            n = lps.Length;
            Clear();
        }

        void Clear()
        {
            Func<double[][]> empty = () => Enumerable.Range(0, n).Select(i => Enumerable.Repeat(double.NaN, n).ToArray()).ToArray();
            Matrix = empty();
            Lowest = empty();
            Highest = empty();
        }

        public void Start() { Launch(Run); }

        void Run()
        {
            Clear();
            Done = 0;
            Version++;
            var fake = new Random(3);
            try
            {
                for (int a = 0; a < n && !Stopped; a++)
                    for (int b = a + 1; b < n && !Stopped; b++)
                    {
                        if (!dry) Native.Pin(lps[a]);
                        double sum = 0, lo = double.MaxValue, hi = 0;
                        for (int k = 0; k < LineCount; k++)
                        {
                            // 521 взаимно просто с числом строк: строки разбросаны по всему массиву.
                            int cell = (k * 521 + 37) % TotalLines * WordsPerLine;
                            double ns = dry ? 30 + 12 * fake.NextDouble() : MeasureLine(cell, lps[b], k == 0 ? 10 : 0);
                            sum += ns;
                            lo = Math.Min(lo, ns);
                            hi = Math.Max(hi, ns);
                        }
                        Matrix[a][b] = Matrix[b][a] = Math.Round(sum / LineCount, 1);
                        Lowest[a][b] = Lowest[b][a] = Math.Round(lo, 1);
                        Highest[a][b] = Highest[b][a] = Math.Round(hi, 1);
                        Done++;
                        Version++;
                        if (dry) Wait(0.02);  // чтобы в окне был виден ход измерения
                    }
                if (Done != Total) Error = "измерение прервано, таблица неполная";
                else if (saveDir != null) Save();
            }
            finally { Native.Unpin(lps); }
        }

        static void RoundTrips(int cell, int count)
        {
            for (int i = 0; i < count; i++)
                while (Interlocked.CompareExchange(ref memory[cell], Ping, Pong) != Pong) { }
        }

        /// Одна строка кэша; вызывающий поток уже привязан к ядру A. Лучший из нескольких замеров.
        static double MeasureLine(int cell, int lpB, int warmMs)
        {
            memory[cell] = Pong;
            var ready = new ManualResetEventSlim(false);
            var responder = new Thread(() =>
            {
                Native.Pin(lpB);
                ready.Set();
                while (Interlocked.CompareExchange(ref memory[cell], Pong, Ping) != Quit) { }
            }) { IsBackground = true };
            responder.Start();
            ready.Wait();

            // Разгон: ядра выходят из простоя и поднимают частоту.
            Stopwatch warm = Stopwatch.StartNew();
            while (warm.ElapsedMilliseconds < warmMs) RoundTrips(cell, 1000);
            RoundTrips(cell, 500);

            double best = double.MaxValue;
            for (int s = 0; s < SampleCount; s++)
            {
                long t0 = Stopwatch.GetTimestamp();
                RoundTrips(cell, Iterations);
                long t1 = Stopwatch.GetTimestamp();
                best = Math.Min(best, (t1 - t0) * 1e9 / Stopwatch.Frequency / Iterations / 2);
            }
            while (Interlocked.Read(ref memory[cell]) != Pong) { }
            Interlocked.Exchange(ref memory[cell], Quit);
            responder.Join();
            return best;
        }

        void Save()
        {
            Func<double[][], double[][], object> spans = (lo, hi) =>
                lo.Select((row, i) => (object)row.Select((v, j) => double.IsNaN(v) ? null : (object)new[] { v, hi[i][j] }).ToArray()).ToArray();
            Json.WriteFile(Path.Combine(saveDir, DateTime.Now.ToString("'latency-'yyyyMMdd-HHmmss'.json'")), new Dictionary<string, object>
            {
                { "lps", lps }, { "lines", LineCount }, { "matrix", Matrix }, { "spans", spans(Lowest, Highest) },
            });
        }

        public void LoadSaved(string path)
        {
            Dictionary<string, object> saved = Json.ReadFile(path);
            // Файлы без "lines" — первый вариант замера на одной строке кэша; он вводил в заблуждение.
            if (!Json.Ints(saved["lps"]).SequenceEqual(lps) || !saved.ContainsKey("lines")) return;
            Matrix = Json.Rows(saved["matrix"]).ToArray();
            int i = 0;
            foreach (System.Collections.IEnumerable row in (System.Collections.IEnumerable)saved["spans"])
            {
                int j = 0;
                foreach (object span in row)
                {
                    double[] pair = Json.Row(span);
                    Lowest[i][j] = pair == null ? double.NaN : pair[0];
                    Highest[i][j] = pair == null ? double.NaN : pair[1];
                    j++;
                }
                i++;
            }
            Done = Total;
            Finished = File.GetLastWriteTime(path).ToString("dd.MM HH:mm");
            Version++;
        }
    }
}
