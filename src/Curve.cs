// Кривая «температура от потолка множителя» для одного P-ядра или для целого блока E-ядер.
//
// Блок E-ядер меряется целиком, со всеми четырьмя ядрами под нагрузкой: потолок и температура у блока
// общие, и в обычной работе его ядра заняты вместе.
// Программа по очереди ставит несколько потолков, от низкого к высокому, на каждом даёт нагрузку
// и записывает среднюю и наибольшую температуру, фактический множитель и прибавку мощности.
// Фактический множитель пишется рядом с потолком нарочно: частоту ядра может держать не его потолок,
// а общая таблица «по числу работающих ядер» — тогда точка это покажет.
// По окончании потолок возвращается к прежнему. Кривые хранятся в data\curves.json.
using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;

namespace CoreMap
{
    class CurvePoint
    {
        public int Limit;                              // потолок множителя, при котором снята точка
        public double Ratio, Temp, Peak, Base, Watts;  // фактический множитель, средняя, наибольшая, без нагрузки, прибавка мощности
        // Средняя температура на этой ступени при подъёме по лесенке и при спуске; Temp — их среднее.
        public double TempUp = double.NaN, TempDown = double.NaN;
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

    /// Замер под общей нагрузкой: что показало каждое ядро, когда нагружены все блоки разом.
    class JointResult
    {
        public string Time;
        public int Level;
        public double Seconds, Watts;        // сколько секунд усреднялось; прибавка мощности процессора
        public int[] Limits;                 // потолки ядер, при которых снят замер
        public double[] Temp, Peak, Ratio;   // по ядрам: средняя температура, наибольшая, фактический множитель
    }

    class CurveTest
    {
        const double JointWarm = 20, JointHold = 20;  // общий замер: прогрев и участок усреднения, секунды
        const int Steps = 8;  // сколько потолков проходит, начиная от заводского предела вниз
        const double Between = 5;    // пауза между блоками при снятии всех кривых подряд, секунды
        // Точка годится для подбора, если ядро шло на заданном потолке: фактический множитель не дальше этого.
        public const double Reached = 0.6;

        readonly Monitor monitor;
        readonly LoadManager load;
        readonly CoreLimits limits;
        readonly int[] lps;
        readonly string cpu, path;
        readonly double loadSeconds, coolSeconds;
        readonly bool saveDirQuick;  // имитация: паузы короткие
        volatile bool cancel;

        public volatile bool Running;
        public int Done, Total, Version;
        public int BlockDone, BlockTotal;  // сколько блоков пройдено и сколько всего в этом запуске
        public volatile bool Searching;  // идёт не снятие кривых, а подбор потолков под температуру
        public volatile bool Joint;      // идёт замер под общей нагрузкой
        // По какой температуре ступени работать: по средней или по наибольшей за ступень. От этого зависят остановка
        // лесенки на пределе «греть до», подбор потолка по кривой и поиск замером. В кривой хранятся обе.
        public volatile bool ByPeak;

        double Heat(CurvePoint p) { return ByPeak ? p.Peak : p.Temp; }
        public volatile JointResult JointLast;  // последний замер под общей нагрузкой; null — не было
        public string Report;            // итог последнего подбора
        public string Current, Error;
        public volatile CurveRun Live;  // кривая, которая снимается сейчас
        public volatile Block Active;   // блок, который меряется сейчас (кривая или подбор); null — никакой
        readonly List<CurveRun> runs = new List<CurveRun>();

        /// saveDir = null — имитация: шаги короткие, кривые не запоминаются.
        public CurveTest(Monitor monitor, LoadManager load, CoreLimits limits, int[] lps, string cpu, string saveDir)
        {
            this.monitor = monitor;
            this.load = load;
            this.limits = limits;
            this.lps = lps;
            this.cpu = cpu;
            path = saveDir == null ? null : Path.Combine(saveDir, "curves.json");
            saveDirQuick = saveDir == null;
            // Лесенка (решения владельца): нагрузка не снимается, потолок идёт ступенями снизу вверх и обратно
            // сверху вниз. На подъёме температура отстаёт вниз, на спуске — вверх; среднее двух проходов это
            // отставание гасит, поэтому ступень может быть короткой. Без нагрузки блок стоит только перед
            // первой ступенью. В расчёт идёт ступень без первой секунды.
            loadSeconds = saveDir == null ? 2 : 4;
            coolSeconds = saveDir == null ? 1 : 4;
        }

        /// Копия кривой для окна: поток измерения дописывает точки, а окно рисует в своём потоке.
        static CurveRun Copy(CurveRun r)
        {
            return new CurveRun { Lp = r.Lp, Level = r.Level, Time = r.Time, Points = new List<CurvePoint>(r.Points) };
        }

        /// Сохранённые кривые блока, по одной на силу нагрузки, и та, что снимается сейчас.
        public CurveRun[] For(Block block)
        {
            int lp = block.Lp;
            CurveRun live = Live;
            lock (runs)
                return runs.Where(r => r.Lp == lp && (live == null || live.Lp != lp || live.Level != r.Level))
                    .Concat(live != null && live.Lp == lp ? new[] { live } : new CurveRun[0]).OrderBy(r => r.Level).Select(Copy).ToArray();
        }

        /// Снимает кривые указанных блоков подряд. maxTemp — подъём по ступеням
        /// прекращается, когда средняя температура вышла за него; NaN — идти до верхней ступени.
        public void Start(Block[] blocks, int level, double maxTemp)
        {
            if (Running || monitor == null || blocks.Length == 0) return;
            Running = true;
            cancel = false;
            new Thread(() => RunAll(blocks, level, maxTemp)) { IsBackground = true, Name = "curve" }.Start();
        }

        public void Stop() { cancel = true; }

        /// Ждёт указанное время; true — просят остановить.
        bool Wait(double seconds)
        {
            for (double end = Native.Now() + seconds; Native.Now() < end; Thread.Sleep(100))
                if (cancel) return true;
            return cancel;
        }

        void RunAll(Block[] blocks, int level, double maxTemp)
        {
            load.StopAll();
            Error = null;
            Done = 0;
            BlockDone = 0;
            BlockTotal = blocks.Length;
            Total = blocks.Sum(b => 2 * Math.Min(Steps, limits.Factory(b.First).Length));
            try
            {
                foreach (Block block in blocks)
                {
                    if (!limits.Known(block.First)) continue;
                    if (BlockDone > 0 && Wait(saveDirQuick ? 1 : Between)) break;
                    Run(block, level, maxTemp);
                    if (cancel || Error != null) break;
                    BlockDone++;
                }
                if (cancel && Error == null) Error = "снятие кривых остановлено";
            }
            catch (Exception e)
            {
                Program.Log(e.ToString());
                Error = "снятие кривых прервано: " + e.Message;
            }
            finally
            {
                load.StopAll();
                Live = null;
                Active = null;
                Current = null;
                Running = false;
                Version++;
            }
        }

        /// Кривая одного блока: проходит потолки от низкого к высокому и возвращает прежний.
        void Run(Block block, int level, double maxTemp)
        {
            int[] members = block.Members;
            int index = block.First;  // потолок блока читается и ставится через первое ядро
            int original = limits.Current[index];
            int[] ratios = limits.Factory(index).Take(Steps).Reverse().ToArray();
            var run = new CurveRun { Lp = block.Lp, Level = level, Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm") };
            Live = run;
            Active = block;
            Version++;
            try
            {
                Current = "без нагрузки";
                string refused = limits.Set(index, ratios[0]);
                if (refused != null) Error = refused;
                else if (!Wait(coolSeconds))
                {
                    double idle = block.Temp(monitor.Typical(coolSeconds / 2)), idleWatts = monitor.Power(coolSeconds / 2);
                    foreach (int m in members) load.Start(m, level);
                    // Вверх по ступеням — до верхней или до первой, где средняя температура вышла за maxTemp:
                    // греть блок выше того, чем владелец собирается пользоваться, незачем.
                    var climbed = new List<CurvePoint>();
                    foreach (int ratio in ratios)
                    {
                        Current = "вверх · потолок ×" + ratio;
                        CurvePoint point = Step(block, ratio, idle, idleWatts);
                        if (point == null) break;
                        point.TempUp = point.Temp;
                        lock (runs) run.Points.Add(point);
                        climbed.Add(point);
                        Done++;
                        Version++;
                        if (Heat(point) > maxTemp) break;
                    }
                    // Вниз по тем же ступеням; температура ступени — среднее подъёма и спуска.
                    for (int k = climbed.Count - 1; k >= 0 && !cancel && Error == null; k--)
                    {
                        CurvePoint point = climbed[k];
                        Current = "вниз · потолок ×" + point.Limit;
                        CurvePoint again = Step(block, point.Limit, idle, idleWatts);
                        if (again == null) break;
                        if (!double.IsNaN(again.Temp))
                        {
                            point.TempDown = again.Temp;
                            point.Temp = double.IsNaN(point.TempUp) ? again.Temp : (point.TempUp + again.Temp) / 2;
                        }
                        point.Ratio = double.IsNaN(again.Ratio) ? point.Ratio : double.IsNaN(point.Ratio) ? again.Ratio : (point.Ratio + again.Ratio) / 2;
                        point.Peak = Math.Max(point.Peak, again.Peak);
                        point.Watts = (point.Watts + again.Watts) / 2;
                        Done++;
                        Version++;
                    }
                }
            }
            catch (Exception e)
            {
                Program.Log(e.ToString());
                Error = "кривая не снята: " + e.Message;
            }
            finally
            {
                foreach (int m in members) load.Stop(m);
                bool whole = !cancel && Error == null, kept = false;
                string refusal = limits.Current[index] == original ? null : limits.Set(index, original);
                if (refusal != null) Error = "прежний потолок не возвращён: " + refusal;
                lock (runs)
                {
                    // Прерванный прогон готовую кривую блока не заменяет: та полнее и снята в обе стороны.
                    bool had = runs.Any(r => r.Lp == run.Lp && r.Level == run.Level);
                    if (run.Points.Count >= 2 && (whole || !had))
                    {
                        runs.RemoveAll(r => r.Lp == run.Lp && r.Level == run.Level);
                        runs.Add(run);
                        kept = true;
                    }
                }
                Live = null;
                try { if (kept) Save(); }
                catch (Exception e) { Program.Log("кривые не сохранены: " + e); }
                Version++;
            }
        }

        /// Подбор под температуру: для каждого блока под нагрузкой ищет самый высокий потолок, при котором
        /// средняя температура не выше maxTemp, и оставляет его блоку. Нагрузка во время поиска не снимается,
        /// потолок двигается ступенями; поиск идёт делением пополам, потом найденное перепроверяется.
        /// Каждая пройденная ступень записывается точкой на кривую блока.
        public void StartSearch(Block[] blocks, int level, double maxTemp)
        {
            if (Running || monitor == null || blocks.Length == 0) return;
            Running = Searching = true;
            cancel = false;
            new Thread(() => SearchAll(blocks, level, maxTemp)) { IsBackground = true, Name = "search" }.Start();
        }

        void SearchAll(Block[] blocks, int level, double maxTemp)
        {
            load.StopAll();
            Error = Report = null;
            Done = Total = 0;
            BlockDone = 0;
            BlockTotal = blocks.Length;
            var set = new List<string>();
            var hot = new List<string>();
            string back = null;  // блок, на котором подбор прервался: ему возвращён прежний потолок
            try
            {
                foreach (Block block in blocks)
                {
                    if (!limits.Known(block.First)) continue;
                    if (BlockDone > 0 && Wait(saveDirQuick ? 1 : Between)) break;
                    bool tooHot;
                    int found = Search(block, level, maxTemp, out tooHot);
                    if (cancel || Error != null) { back = block.Name; break; }
                    (tooHot ? hot : set).Add(block.Name + " ×" + found);
                    BlockDone++;
                }
            }
            catch (Exception e)
            {
                Program.Log(e.ToString());
                Error = "подбор не удался: " + e.Message;
            }
            finally
            {
                load.StopAll();
                string report = (cancel ? "Подбор остановлен. " : "") + "Под " + maxTemp + " °C поставлено: " + (set.Count > 0 ? string.Join(", ", set) : "ничего");
                if (back != null) report += ". Блок " + back + " не подобран — ему возвращён прежний потолок";
                if (hot.Count > 0) report += ". Горячее предела даже на самом низком потолке: " + string.Join(", ", hot);
                if (Error != null) report += ". Ошибка: " + Error;
                Report = report + ". Потолки не сохранены — проверь под нагрузкой и сохрани профиль.";
                Live = null;
                Active = null;
                Current = null;
                Running = Searching = false;
                Version++;
            }
        }

        /// Одна ступень под нагрузкой: ставит потолок, держит ступень и возвращает точку. null — остановлено или отказ.
        CurvePoint Step(Block block, int ratio, double idle, double idleWatts)
        {
            string refusal = limits.Set(block.First, ratio);
            if (refusal != null) { Error = refusal; return null; }
            double window = loadSeconds - 1, end = Native.Now() + loadSeconds, peak = 0, ratioSum = 0;
            int ratioCount = 0;
            while (Native.Now() < end && !cancel)
            {
                Thread.Sleep(250);
                peak = Math.Max(peak, block.Temp(monitor.Cur));
                if (Native.Now() < end - window) continue;
                double ratioNow = block.Ratio(monitor.Ratio);
                if (double.IsNaN(ratioNow)) continue;
                ratioSum += ratioNow;
                ratioCount++;
            }
            if (cancel) return null;
            return new CurvePoint
            {
                Limit = ratio, Ratio = ratioCount > 0 ? ratioSum / ratioCount : double.NaN, Peak = peak, Base = idle,
                Temp = block.Temp(monitor.Typical(window)), Watts = monitor.Power(window) - idleWatts,
            };
        }

        /// Записывает точку на сохранённую кривую блока, заменяя прежнюю с тем же потолком.
        void Record(int lp, int level, CurvePoint point)
        {
            lock (runs)
            {
                CurveRun run = runs.FirstOrDefault(r => r.Lp == lp && r.Level == level);
                if (run == null) runs.Add(run = new CurveRun { Lp = lp, Level = level });
                run.Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm");
                run.Points.RemoveAll(p => p.Limit == point.Limit);
                run.Points.Add(point);
                run.Points.Sort((a, b) => a.Limit.CompareTo(b.Limit));
            }
            Version++;
        }

        /// Поиск для одного блока. Возвращает поставленный потолок; tooHot — блок горячее предела и на самом низком.
        int Search(Block block, int level, double maxTemp, out bool tooHot)
        {
            tooHot = false;
            int[] members = block.Members;
            int index = block.First;  // потолок блока читается и ставится через первое ядро
            int[] ratios = limits.Factory(index).Reverse().ToArray();  // по возрастанию
            int lp = block.Lp, chosen = ratios[0], original = limits.Current[index];
            bool finished = false;  // поиск дошёл до конца; иначе блоку возвращается прежний потолок
            string name = block.Name;
            Active = block;
            try
            {
                Current = name + " · без нагрузки";
                if (limits.Set(index, ratios[0]) != null || Wait(coolSeconds)) return chosen;
                double idle = block.Temp(monitor.Typical(coolSeconds / 2)), idleWatts = monitor.Power(coolSeconds / 2);
                foreach (int m in members) load.Start(m, level);

                // Деление пополам: температура растёт с множителем, ищется самый высокий подходящий.
                int lo = 0, hi = ratios.Length - 1, best = -1;
                CurvePoint bestPoint = null;
                while (lo <= hi)
                {
                    int mid = (lo + hi) / 2;
                    Current = name + " · пробую ×" + ratios[mid];
                    CurvePoint point = Step(block, ratios[mid], idle, idleWatts);
                    if (point == null) return chosen;
                    Record(lp, level, point);
                    Current = name + " · ×" + ratios[mid] + " → " + Heat(point).ToString("0.0") + " °C";
                    if (Heat(point) <= maxTemp) { best = mid; bestPoint = point; lo = mid + 1; }
                    else hi = mid - 1;
                }
                // Перепроверка: после более горячей ступени температура ещё оседает, после более холодной —
                // ещё растёт. Найденный потолок держится ещё ступень; не уложился — на ступень ниже.
                while (best >= 0)
                {
                    Current = name + " · проверяю ×" + ratios[best];
                    CurvePoint point = Step(block, ratios[best], idle, idleWatts);
                    if (point == null) return chosen;
                    Record(lp, level, point);
                    bestPoint = point;
                    if (Heat(point) <= maxTemp) break;
                    best--;
                }
                tooHot = best < 0;
                chosen = ratios[Math.Max(best, 0)];
                if (!tooHot && bestPoint != null) chosen = Settle(chosen, bestPoint.Ratio, ratios);
                finished = true;
                return chosen;
            }
            finally
            {
                foreach (int m in members) load.Stop(m);
                int put = finished ? chosen : original;
                string refusal = limits.Current[index] == put ? null : limits.Set(index, put);
                if (refusal != null) Error = refusal;
                try { Save(); }
                catch (Exception e) { Program.Log("кривые не сохранены: " + e); }
            }
        }

        /// Замер под общей нагрузкой: все ядра нагружаются разом при потолках, которые стоят сейчас; после прогрева
        /// записываются средняя и наибольшая температура и фактический множитель каждого ядра. Кривые сняты по
        /// одному блоку, а в работе блоки греют друг друга — этот замер показывает, насколько.
        public void StartJoint(int level)
        {
            if (Running || monitor == null) return;
            Running = Joint = true;
            cancel = false;
            new Thread(() => RunJoint(level)) { IsBackground = true, Name = "joint" }.Start();
        }

        void RunJoint(int level)
        {
            load.StopAll();
            Error = null;
            double warm = saveDirQuick ? 2 : JointWarm, hold = saveDirQuick ? 3 : JointHold;
            Done = 0;
            Total = (int)(warm + hold);
            BlockDone = 0;
            BlockTotal = 1;
            try
            {
                Current = "без нагрузки";
                if (Wait(coolSeconds)) { Error = "замер под общей нагрузкой остановлен"; return; }
                double idleWatts = monitor.Power(coolSeconds / 2);
                int n = lps.Length;
                for (int i = 0; i < n; i++) load.Start(i, level);
                var peak = new double[n];
                var ratioSum = new double[n];
                var ratioCount = new int[n];
                double start = Native.Now(), end = start + warm + hold;
                while (Native.Now() < end && !cancel)
                {
                    Thread.Sleep(250);
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
                if (cancel) { Error = "замер под общей нагрузкой остановлен"; return; }
                JointLast = new JointResult
                {
                    Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss"), Level = level, Seconds = hold, Watts = monitor.Power(hold) - idleWatts,
                    Limits = (int[])limits.Current.Clone(), Temp = monitor.Typical(hold), Peak = peak,
                    Ratio = ratioSum.Select((v, i) => ratioCount[i] > 0 ? v / ratioCount[i] : double.NaN).ToArray(),
                };
                try { SaveJoint(JointLast); }
                catch (Exception e) { Program.Log("замер под общей нагрузкой не сохранён: " + e); }
            }
            catch (Exception e)
            {
                Program.Log(e.ToString());
                Error = "замер под общей нагрузкой не удался: " + e.Message;
            }
            finally
            {
                load.StopAll();
                Current = null;
                Running = Joint = false;
                Version++;
            }
        }

        void SaveJoint(JointResult r)
        {
            if (path == null) return;
            Json.WriteFile(Path.Combine(Path.GetDirectoryName(path), "joint-" + DateTime.Now.ToString("yyyyMMdd-HHmmss") + ".json"),
                new Dictionary<string, object>
                {
                    { "cpu", cpu }, { "time", r.Time }, { "level", r.Level }, { "seconds", r.Seconds }, { "watts", r.Watts },
                    { "lps", lps.Cast<object>().ToList() }, { "limits", r.Limits.Cast<object>().ToList() },
                    { "temp", r.Temp.Cast<object>().ToList() }, { "peak", r.Peak.Cast<object>().ToList() }, { "ratio", r.Ratio.Cast<object>().ToList() },
                });
        }

        /// Ядро до потолка не дошло — потолок опускается к множителю, на котором оно шло на самом деле:
        /// иначе при других условиях (прохладнее в комнате) оно разгонится выше проверенного.
        /// ascending — допустимые потолки по возрастанию.
        static int Settle(int limit, double ratio, int[] ascending)
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
                    Limit = limit, From = p.Limit, Temp = temps[k], Ratio = Math.Min(Actual(p), limit),
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

        static double Actual(CurvePoint p) { return double.IsNaN(p.Ratio) ? p.Limit : p.Ratio; }

        /// Кривые всех блоков на данной силе нагрузки, вместе с той, что снимается сейчас.
        public CurveRun[] All(int level)
        {
            CurveRun live = Live;
            lock (runs)
                return runs.Where(r => r.Level == level && (live == null || live.Lp != r.Lp || live.Level != level))
                    .Concat(live != null && live.Level == level ? new[] { live } : new CurveRun[0]).Select(Copy).ToArray();
        }

        void Save()
        {
            if (path == null) return;
            var list = new List<object>();
            lock (runs)
                foreach (CurveRun r in runs)
                    list.Add(new Dictionary<string, object>
                    {
                        { "lp", r.Lp }, { "level", r.Level }, { "time", r.Time },
                        { "points", r.Points.Select(p => (object)new Dictionary<string, object>
                            {
                                { "limit", p.Limit }, { "ratio", p.Ratio }, { "temp", p.Temp }, { "peak", p.Peak }, { "base", p.Base }, { "watts", p.Watts },
                                { "up", p.TempUp }, { "down", p.TempDown },
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
            if ((saved["cpu"] as string) != cpu) return;
            lock (runs)
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
                        });
                    runs.Add(run);
                }
        }
    }
}
