// Кривые «температура от потолка множителя»: точки, хранилище и подбор потолка по снятой кривой.
//
// Кривая снимается для одного P-ядра или для целого блока E-ядер: потолок и температура у блока общие.
// В точке рядом с потолком записан фактический множитель — нарочно: частоту ядра может держать не его
// потолок, а общая таблица «по числу работающих ядер», и тогда точка это покажет.
// Здесь только данные и расчёт по ним. Снимают кривые измерения из Ladder.cs: лесенка (CurveTest) и поиск
// потолка под температуру (LimitSearch). Кривые хранятся в data\curves.json.
using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace CoreMap
{
    class CurvePoint
    {
        public int Limit;                              // потолок множителя, при котором снята точка
        public double Ratio, Temp, Peak, Base, Watts;  // фактический множитель, средняя, наибольшая, без нагрузки, прибавка мощности
        // Средняя температура на этой ступени при подъёме по лесенке и при спуске; Temp — их среднее.
        public double TempUp = double.NaN, TempDown = double.NaN;
        public FanStat Air;  // обороты вентиляторов за ступень; null — неизвестны (точка снята до появления оборотов)

        /// Множитель, на котором блок шёл на этой ступени; если он не измерен — заданный потолок.
        public double Actual { get { return double.IsNaN(Ratio) ? Limit : Ratio; } }
    }

    class CurveRun
    {
        public int Lp, Level;  // Lp — первое ядро блока (Block.Lp): под ним кривая лежит в файле
        public string Time;
        public List<CurvePoint> Points = new List<CurvePoint>();
    }

    /// Что снятая кривая блока говорит о потолке под заданную температуру.
    class CurveChoice
    {
        public int Limit;    // потолок, который подходит
        public double Temp;  // температура на нём по кривой
        public double Ratio; // множитель, на котором блок будет работать на самом деле
        public int From;     // потолок точки кривой, с которой взят выбор (сам потолок может быть ниже: см. Settle)
        public bool Top;     // взята верхняя точка кривой, а она ниже заводского предела: выше кривая не снята
        public bool Hot;     // блок горячее цели уже на нижней точке кривой: ниже кривая не снята
    }

    /// Хранилище кривых: по одной на блок и силу нагрузки. Пишет в него поток измерения, читает окно —
    /// окну отдаются копии, снятые под замком.
    class CurveStore
    {
        // Точка годится для подбора, если ядро шло на заданном потолке: фактический множитель не дальше этого.
        public const double Reached = 0.6;

        readonly CoreLimits limits;
        readonly string cpu, path;  // path = null — имитация: кривые не запоминаются
        readonly List<CurveRun> runs = new List<CurveRun>();
        volatile CurveRun live;     // кривая, которая снимается сейчас; null — никакая

        public int Version;  // растёт, когда меняется то, что по кривым показывает окно
        // По какой температуре ступени работать: по средней или по наибольшей за ступень. От этого зависят остановка
        // лесенки на пределе «греть до», подбор потолка по кривой и поиск замером. В кривой хранятся обе.
        public volatile bool ByPeak;

        public CurveStore(CoreLimits limits, string cpu, string saveDir)
        {
            this.limits = limits;
            this.cpu = cpu;
            path = saveDir == null ? null : Path.Combine(saveDir, "curves.json");
        }

        /// Температура ступени, по которой идёт работа: средняя или наибольшая (ByPeak).
        public double Heat(CurvePoint p) { return ByPeak ? p.Peak : p.Temp; }

        /// То, что показывает колонка потолков, изменилось: окно перерисует её.
        public void Touch() { Version++; }

        // ----- запись: поток измерения -----

        /// Начата новая кривая: до Commit окно показывает её вместо сохранённой.
        public void Begin(CurveRun run)
        {
            live = run;
            Version++;
        }

        public void Add(CurveRun run, CurvePoint point)
        {
            lock (runs) run.Points.Add(point);
            Version++;
        }

        /// Кривая закончена. whole — пройдена целиком; прерванный прогон готовую кривую блока не заменяет:
        /// та полнее и снята в обе стороны.
        public void Commit(CurveRun run, bool whole)
        {
            bool kept = false;
            lock (runs)
            {
                bool had = runs.Any(r => r.Lp == run.Lp && r.Level == run.Level);
                if (run.Points.Count >= 2 && (whole || !had))
                {
                    runs.RemoveAll(r => r.Lp == run.Lp && r.Level == run.Level);
                    runs.Add(run);
                    kept = true;
                }
            }
            live = null;
            if (kept) Flush();
            Version++;
        }

        /// Записывает точку на сохранённую кривую блока, заменяя прежнюю с тем же потолком.
        public void Record(Block block, int level, CurvePoint point)
        {
            lock (runs)
            {
                CurveRun run = runs.FirstOrDefault(r => r.Lp == block.Lp && r.Level == level);
                if (run == null) runs.Add(run = new CurveRun { Lp = block.Lp, Level = level });
                run.Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm");
                run.Points.RemoveAll(p => p.Limit == point.Limit);
                run.Points.Add(point);
                run.Points.Sort((a, b) => a.Limit.CompareTo(b.Limit));
            }
            Version++;
        }

        /// Пишет кривые в файл. Не записались — измерение из-за этого не прерывается, но владелец об этом узнаёт:
        /// кривые остались только в памяти программы.
        public void Flush()
        {
            try { Save(); }
            catch (Exception e) { Program.Tell("Кривые не сохранены в файл: " + Program.Why(e) + ".", e); }
        }

        // ----- чтение: окно и подбор -----

        /// Копия кривой для окна: поток измерения дописывает точки, а окно рисует в своём потоке.
        static CurveRun Copy(CurveRun r)
        {
            return new CurveRun { Lp = r.Lp, Level = r.Level, Time = r.Time, Points = new List<CurvePoint>(r.Points) };
        }

        /// Сохранённые кривые блока, по одной на силу нагрузки, и та, что снимается сейчас.
        public CurveRun[] For(Block block)
        {
            int lp = block.Lp;
            CurveRun now = live;
            lock (runs)
                return runs.Where(r => r.Lp == lp && (now == null || now.Lp != lp || now.Level != r.Level))
                    .Concat(now != null && now.Lp == lp ? new[] { now } : new CurveRun[0]).OrderBy(r => r.Level).Select(Copy).ToArray();
        }

        /// Кривые всех блоков на данной силе нагрузки, вместе с той, что снимается сейчас.
        public CurveRun[] All(int level)
        {
            CurveRun now = live;
            lock (runs)
                return runs.Where(r => r.Level == level && (now == null || now.Lp != r.Lp || now.Level != level))
                    .Concat(now != null && now.Level == level ? new[] { now } : new CurveRun[0]).Select(Copy).ToArray();
        }

        /// Ядро до потолка не дошло — потолок опускается к множителю, на котором оно шло на самом деле:
        /// иначе при других условиях (прохладнее в комнате) оно разгонится выше проверенного.
        /// ascending — допустимые потолки по возрастанию.
        public static int Settle(int limit, double ratio, int[] ascending)
        {
            if (double.IsNaN(ratio) || limit - ratio <= Reached) return limit;
            return ascending.First(r => r >= ratio - 0.3);
        }

        /// Выравнивает ряд до неубывающего: соседние значения, идущие вниз, заменяются их общим средним.
        /// Температура с множителем падать не может; спад между соседними ступенями — разброс измерения.
        static double[] Rising(double[] values)
        {
            var sums = new List<double>();
            var counts = new List<int>();
            foreach (double v in values)
            {
                sums.Add(v);
                counts.Add(1);
                for (int last = sums.Count - 1; last > 0 && sums[last - 1] / counts[last - 1] > sums[last] / counts[last]; last--)
                {
                    sums[last - 1] += sums[last];
                    counts[last - 1] += counts[last];
                    sums.RemoveAt(last);
                    counts.RemoveAt(last);
                }
            }
            var result = new List<double>();
            for (int k = 0; k < sums.Count; k++)
                for (int j = 0; j < counts[k]; j++) result.Add(sums[k] / counts[k]);
            return result.ToArray();
        }

        /// Все варианты потолка блока по его снятой кривой, от низкого к высокому: потолок, температура на нём и
        /// множитель, на котором блок будет работать. Между ступенями ничего не достраивается: потолок целый,
        /// и каждая ступень измерена. null — кривой блока на этой силе нагрузки нет.
        public CurveChoice[] Options(Block block, int level)
        {
            int lp = block.Lp, index = block.First;
            int[] ascending = limits.Factory(index).Reverse().ToArray();
            CurvePoint[] points;
            lock (runs)
            {
                CurveRun run = runs.FirstOrDefault(r => r.Lp == lp && r.Level == level);
                if (run == null) return null;
                points = run.Points.Where(p => !double.IsNaN(p.Temp) && ascending.Contains(p.Limit)).OrderBy(p => p.Limit).ToArray();
            }
            if (points.Length < 2) return null;
            double[] temps = Rising(points.Select(p => Heat(p)).ToArray());
            return points.Select((p, k) =>
            {
                int limit = Settle(p.Limit, p.Ratio, ascending);
                return new CurveChoice
                {
                    Limit = limit, From = p.Limit, Temp = temps[k], Ratio = Math.Min(p.Actual, limit),
                    Top = k == points.Length - 1 && p.Limit < limits.Fused[index],
                };
            }).ToArray();
        }

        /// Подбор по снятой кривой, без нагрузки: самый высокий потолок, при котором температура блока по кривой
        /// не выше maxTemp. За края кривой подбор не выходит. null — кривой блока на этой силе нагрузки нет.
        public CurveChoice Choose(Block block, int level, double maxTemp)
        {
            CurveChoice[] options = Options(block, level);
            if (options == null) return null;
            int at = Array.FindLastIndex(options, o => o.Temp <= maxTemp);
            if (at >= 0) return options[at];
            options[0].Hot = true;
            return options[0];
        }

        // Обороты на автоматике, разошедшиеся между точками кривых больше чем на столько, — уже разные условия.
        // Порог выбран без измерения: это около трети хода оборотов (примерно от 2000 до 5500 об/мин). На сколько
        // градусов тысяча оборотов меняет температуру блока, пока неизвестно.
        public const int FanSpread = 1000;

        /// Одной строкой — если кривые на этой силе нагрузки сняты при разных вентиляторах: часть на максимуме,
        /// часть на автоматике; часть без записанных оборотов; обороты на автоматике разошлись больше чем на
        /// FanSpread. null — условия одинаковы либо оборотов нет ни у одной кривой (все сняты до их появления).
        public string Uneven(int level, BlockMap blocks)
        {
            List<string> onMax = new List<string>(), auto = new List<string>(), mixed = new List<string>(), unknown = new List<string>();
            var free = new List<FanStat>();  // точки, снятые на автоматике
            lock (runs)
                foreach (CurveRun run in runs.Where(r => r.Level == level).OrderBy(r => r.Lp))
                {
                    Block block = blocks.ByLp(run.Lp);
                    if (block == null || run.Points.Count == 0) continue;
                    List<FanStat> known = run.Points.Where(p => p.Air != null).Select(p => p.Air).ToList();
                    free.AddRange(known.Where(a => !a.Max));
                    bool whole = known.Count == run.Points.Count, any = known.Any(a => a.Max), all = known.All(a => a.Max);
                    (known.Count == 0 ? unknown : !whole || (any && !all) ? mixed : all ? onMax : auto).Add(block.Name);
                }
            if (onMax.Count + auto.Count + mixed.Count == 0) return null;
            bool wide = free.Count > 0 && free.Max(a => a.Mean) - free.Min(a => a.Mean) > FanSpread;
            int kinds = new[] { onMax, auto, mixed, unknown }.Count(list => list.Count > 0);
            if (kinds < 2 && mixed.Count == 0 && !wide) return null;
            var parts = new List<string>();
            if (onMax.Count > 0) parts.Add("на максимуме — " + string.Join(", ", onMax));
            if (auto.Count > 0) parts.Add("на авто, " + FanStat.Range(free) + " — " + string.Join(", ", auto));
            if (mixed.Count > 0) parts.Add("вперемешку — " + string.Join(", ", mixed));
            if (unknown.Count > 0) parts.Add("без данных — " + string.Join(", ", unknown));
            return "Кривые сняты при разных вентиляторах (" + string.Join("; ", parts) + "): температуры блоков по ним могут быть несравнимы.";
        }

        // ----- файл -----

        void Save()
        {
            if (path == null) return;
            var list = new List<object>();
            lock (runs)
                foreach (CurveRun r in runs)
                    list.Add(new Dictionary<string, object>
                    {
                        { "lp", r.Lp }, { "level", r.Level }, { "time", r.Time },
                        { "points", r.Points.Select(p =>
                            {
                                var point = new Dictionary<string, object>
                                {
                                    { "limit", p.Limit }, { "ratio", p.Ratio }, { "temp", p.Temp }, { "peak", p.Peak }, { "base", p.Base }, { "watts", p.Watts },
                                    { "up", p.TempUp }, { "down", p.TempDown },
                                };
                                FanStat.Put(point, p.Air);
                                return (object)point;
                            }).ToList() },
                    });
            Json.WriteFile(path, new Dictionary<string, object> { { "cpu", cpu }, { "runs", list } });
        }

        /// from — другой файл вместо своего: проверка окна на имитации показывает настоящие кривые.
        public void LoadSaved(string from = null)
        {
            string file = from ?? path;
            if (file == null || !File.Exists(file)) return;
            Dictionary<string, object> saved = Json.ReadFile(file);
            var read = new List<CurveRun>();
            try
            {
                if ((saved["cpu"] as string) != cpu) return;
                foreach (Dictionary<string, object> r in (IEnumerable)saved["runs"])
                {
                    var run = new CurveRun { Lp = Convert.ToInt32(r["lp"]), Level = Convert.ToInt32(r["level"]), Time = r["time"] as string };
                    foreach (Dictionary<string, object> p in (IEnumerable)r["points"])
                        run.Points.Add(new CurvePoint
                        {
                            Limit = Convert.ToInt32(p["limit"]), Ratio = Json.Number(p["ratio"]), Temp = Json.Number(p["temp"]),
                            Peak = Json.Number(p["peak"]), Base = Json.Number(p["base"]), Watts = Json.Number(p["watts"]),
                            TempUp = p.ContainsKey("up") ? Json.Number(p["up"]) : double.NaN,
                            TempDown = p.ContainsKey("down") ? Json.Number(p["down"]) : double.NaN,
                            Air = FanStat.Get(p),
                        });
                    read.Add(run);
                }
            }
            catch (Exception e) { throw Json.Spoiled(file, e); }
            // Кривые попадают в хранилище только все разом: половина файла, записанная потом поверх целого, хуже пустого.
            lock (runs) runs.AddRange(read);
        }
    }
}
