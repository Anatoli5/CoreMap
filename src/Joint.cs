// Замер под общей нагрузкой: все ядра нагружаются разом при потолках, которые стоят сейчас; после прогрева
// записываются средняя и наибольшая температура и фактический множитель каждого ядра. Кривые сняты по
// одному блоку, а в работе блоки греют друг друга — этот замер показывает, насколько.
// Потолки замер не меняет. Результат пишется в data\joint-<время>.json.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace CoreMap
{
    /// Что показало каждое ядро, когда нагружены все блоки разом.
    class JointResult
    {
        public string Time;
        public int Level;
        public double Seconds, Watts;        // сколько секунд усреднялось; прибавка мощности процессора
        public int[] Limits;                 // потолки ядер, при которых снят замер
        public double[] Temp, Peak, Ratio;   // по ядрам: средняя температура, наибольшая, фактический множитель
    }

    class JointTest : Measurement
    {
        const double CoolSeconds = 4, WarmSeconds = 20, HoldSeconds = 20;  // без нагрузки, прогрев, участок усреднения
        const string Halted = "замер под общей нагрузкой остановлен";

        readonly Monitor monitor;
        readonly CoreLimits limits;
        readonly CurveStore curves;
        readonly int[] lps;
        readonly string cpu, saveDir;  // saveDir = null — имитация: паузы короткие, результат не пишется

        public int Done, Total;           // сколько секунд под нагрузкой прошло и сколько всего
        public string Current;            // что делается сейчас, для строки состояния
        public volatile JointResult Last;  // последний замер; null — не было

        public JointTest(Runner runner, Monitor monitor, CoreLimits limits, CurveStore curves, int[] lps, string cpu, string saveDir)
            : base(runner, "замер под общей нагрузкой не удался")
        {
            this.monitor = monitor;
            this.limits = limits;
            this.curves = curves;
            this.lps = lps;
            this.cpu = cpu;
            this.saveDir = saveDir;
        }

        public void Start(int level) { Launch(() => Run(level)); }

        // Результат показан в списке блоков, а колонку потолков окно рисует по счётчику хранилища кривых.
        protected override void Changed() { curves.Touch(); }

        protected override void Finish() { Current = null; }

        void Run(int level)
        {
            bool quick = saveDir == null;
            double warm = quick ? 2 : WarmSeconds, hold = quick ? 3 : HoldSeconds, cool = quick ? 1 : CoolSeconds;
            Done = 0;
            Total = (int)(warm + hold);
            Current = "без нагрузки";
            if (Wait(cool)) { Error = Halted; return; }
            double idleWatts = monitor.Power(cool / 2);
            int n = lps.Length;
            for (int i = 0; i < n; i++) Load.Start(i, level);
            var peak = new double[n];
            var ratioSum = new double[n];
            var ratioCount = new int[n];
            double start = Native.Now(), end = start + warm + hold;
            while (Native.Now() < end && !Wait(0.25))
            {
                Done = (int)(Native.Now() - start);
                bool measuring = Native.Now() >= end - hold;
                Current = measuring ? "замер" : "прогрев";
                if (!measuring) continue;
                double[] cur = monitor.Cur, ratio = monitor.Ratio;
                for (int i = 0; i < n; i++)
                {
                    peak[i] = Math.Max(peak[i], cur[i]);
                    if (double.IsNaN(ratio[i])) continue;
                    ratioSum[i] += ratio[i];
                    ratioCount[i]++;
                }
            }
            if (Stopped) { Error = Halted; return; }
            Last = new JointResult
            {
                Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss"), Level = level, Seconds = hold, Watts = monitor.Power(hold) - idleWatts,
                Limits = (int[])limits.Current.Clone(), Temp = monitor.Typical(hold), Peak = peak,
                Ratio = ratioSum.Select((v, i) => ratioCount[i] > 0 ? v / ratioCount[i] : double.NaN).ToArray(),
            };
            try { Save(Last); }
            catch (Exception e) { Program.Tell("Замер под общей нагрузкой не сохранён в файл: " + Program.Why(e) + ".", e); }
        }

        void Save(JointResult r)
        {
            if (saveDir == null) return;
            Json.WriteFile(Path.Combine(saveDir, "joint-" + DateTime.Now.ToString("yyyyMMdd-HHmmss") + ".json"),
                new Dictionary<string, object>
                {
                    { "cpu", cpu }, { "time", r.Time }, { "level", r.Level }, { "seconds", r.Seconds }, { "watts", r.Watts },
                    { "lps", lps.Cast<object>().ToList() }, { "limits", r.Limits.Cast<object>().ToList() },
                    { "temp", r.Temp.Cast<object>().ToList() }, { "peak", r.Peak.Cast<object>().ToList() }, { "ratio", r.Ratio.Cast<object>().ToList() },
                });
        }
    }
}
