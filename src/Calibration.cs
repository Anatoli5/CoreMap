// Подбор нагрузки: замер нагрева от каждого вида вычислений и выбор по нему трёх ступеней силы.
// Запускается кнопкой в окне. Результат применяется сразу и запоминается в data\calibration.json,
// при следующем запуске читается оттуда.
using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;

namespace CoreMap
{
    class Calibration
    {
        const double IdleSeconds = 4, LoadSeconds = 8;
        const double AbortAt = 96;  // шаг прерывается, если ядро дважды подряд показало столько, °C

        class Row
        {
            public double[] Rise, Peak, Watts, Speed;
            public bool[] Throttled, Aborted;
        }

        readonly Monitor monitor;
        readonly LoadManager load;
        readonly int[] lps, targets;
        readonly string cpu, path;
        volatile bool cancel;

        public volatile bool Running;
        public int Done;
        public string Current, Error, Finished;
        public bool Fitted;                          // ступени силы выбраны замером на этом процессоре
        public readonly double[] Heat = new double[4];  // нагрев ядра на ступенях 1–3, °C

        public int Total { get { return Kernels.All.Length * targets.Length; } }

        /// saveDir = null — результат не применяется и не запоминается (имитация: температуры ненастоящие).
        public Calibration(Monitor monitor, LoadManager load, CpuCore[] cores, string cpu, string saveDir)
        {
            this.monitor = monitor;
            this.load = load;
            this.cpu = cpu;
            lps = cores.Select(c => c.Lp).ToArray();
            // Первые два P-ядра по номеру: на 275HX это прохладное ядро 0 и горячее ядро 1.
            int[] fast = Enumerable.Range(0, cores.Length).Where(i => cores[i].Kind == 'P').ToArray();
            targets = (fast.Length > 0 ? fast : Enumerable.Range(0, cores.Length)).Take(2).ToArray();
            path = saveDir == null ? null : Path.Combine(saveDir, "calibration.json");
        }

        public void Start()
        {
            if (Running || monitor == null) return;
            Running = true;
            cancel = false;
            new Thread(Run) { IsBackground = true, Name = "calibration" }.Start();
        }

        public void Stop() { cancel = true; }

        void Wait(double seconds)
        {
            for (double end = Native.Now() + seconds; Native.Now() < end && !cancel; ) Thread.Sleep(100);
        }

        void Run()
        {
            load.StopAll();
            Done = 0;
            Error = null;
            Finished = null;
            var rows = new List<Row>();
            try
            {
                for (int k = 0; k < Kernels.All.Length && !cancel; k++)
                {
                    int count = targets.Length;
                    var row = new Row
                    {
                        Rise = new double[count], Peak = new double[count], Watts = new double[count], Speed = new double[count],
                        Throttled = new bool[count], Aborted = new bool[count],
                    };
                    for (int t = 0; t < count && !cancel; t++)
                    {
                        int i = targets[t];
                        Current = "вид вычислений " + (k + 1) + " из " + Kernels.All.Length + ", ядро " + lps[i];
                        Wait(IdleSeconds);
                        double idle = monitor.Typical(IdleSeconds / 2)[i], idleWatts = monitor.Power(IdleSeconds / 2);
                        load.StartKernel(i, Kernels.All[k]);
                        double end = Native.Now() + LoadSeconds;
                        int hot = 0;
                        while (Native.Now() < end && !cancel)
                        {
                            Thread.Sleep(500);
                            double now = monitor.Cur[i];
                            row.Peak[t] = Math.Max(row.Peak[t], now);
                            row.Throttled[t] |= monitor.Throttled[i];
                            hot = now >= AbortAt ? hot + 1 : 0;
                            if (hot >= 2) { row.Aborted[t] = true; break; }
                        }
                        double span = row.Aborted[t] ? 1 : LoadSeconds / 2;
                        row.Rise[t] = monitor.Typical(span)[i] - idle;
                        row.Watts[t] = monitor.Power(span) - idleWatts;
                        row.Speed[t] = load.Rate(i);
                        load.Stop(i);
                        if (!cancel) Done++;
                    }
                    rows.Add(row);
                }
                if (cancel) Error = "подбор остановлен, сила нагрузки прежняя";
                else if (path != null && Choose(rows)) Save(rows);
            }
            catch (Exception e)
            {
                Program.Log(e.ToString());
                Error = "подбор не удался: " + e.Message;
            }
            finally
            {
                load.StopAll();
                Current = null;
                Finished = DateTime.Now.ToString("dd.MM HH:mm");
                Running = false;
            }
        }

        /// Полная ступень — вид вычислений с наибольшим нагревом; лёгкая — ближайший к половине этого
        /// нагрева, средняя — ближайший к трём четвертям. Лёгкая ступень служит пробой на каждом ядре
        /// и основой сравнения «под одинаковой нагрузкой»: слишком слабую датчик с шагом 2 °C различает
        /// плохо (замер на 275HX: самый слабый вид даёт +13 °C, половинный — +19 °C).
        bool Choose(List<Row> rows)
        {
            double[] heat = rows.Select(r => r.Rise.Average()).ToArray();
            int[] all = Enumerable.Range(0, heat.Length).ToArray();
            int full = all.OrderByDescending(k => heat[k]).First();
            if (!(heat[full] >= 4))
            {
                Error = "подбор не удался: нагрев от нагрузки не виден на датчиках";
                return false;
            }
            int light = all.Where(k => k != full).OrderBy(k => Math.Abs(heat[k] - heat[full] * 0.5)).First();
            int medium = all.Where(k => k != full && k != light).OrderBy(k => Math.Abs(heat[k] - heat[full] * 0.75)).First();
            if (heat[medium] < heat[light]) { int swap = light; light = medium; medium = swap; }
            Set(new[] { light, medium, full }, new[] { heat[light], heat[medium], heat[full] });
            return true;
        }

        void Set(int[] kernels, double[] heat)
        {
            for (int level = 1; level <= 3; level++)
            {
                Kernels.ByLevel[level] = kernels[level - 1];
                ThermalSweep.Rise[level] = heat[level - 1] / heat[0];
                Heat[level] = heat[level - 1];
            }
            Kernels.ByLevel[0] = kernels[0];
            Fitted = true;
        }

        void Save(List<Row> rows)
        {
            var kernels = new List<object>();
            for (int k = 0; k < rows.Count; k++)
                kernels.Add(new Dictionary<string, object>
                {
                    { "name", Kernels.All[k].Name }, { "unit", Kernels.All[k].Unit }, { "rise", rows[k].Rise }, { "peak", rows[k].Peak },
                    { "watts", rows[k].Watts }, { "speed", rows[k].Speed }, { "throttled", rows[k].Throttled }, { "aborted", rows[k].Aborted },
                });
            Json.WriteFile(path, new Dictionary<string, object>
            {
                { "cpu", cpu }, { "time", DateTime.Now.ToString("yyyy-MM-dd HH:mm") }, { "cores", targets.Select(i => lps[i]).ToArray() },
                { "kernels", kernels },
                { "levels", new[] { 1, 2, 3 }.Select(level => Kernels.ForLevel(level).Name).ToArray() },
                { "heat", new[] { Heat[1], Heat[2], Heat[3] } },
            });
        }

        /// Читает прошлый подбор, если он сделан на этом же процессоре.
        public void LoadSaved()
        {
            if (path == null || !File.Exists(path)) return;
            Dictionary<string, object> saved = Json.ReadFile(path);
            if ((saved["cpu"] as string) != cpu) return;
            int[] kernels = ((IEnumerable)saved["levels"]).Cast<object>()
                .Select(name => Array.FindIndex(Kernels.All, kernel => kernel.Name == (name as string))).ToArray();
            double[] heat = Json.Row(saved["heat"]);
            if (kernels.Length != 3 || heat.Length != 3 || kernels.Contains(-1) || !(heat[0] > 0)) return;
            Set(kernels, heat);
        }
    }
}
