// Измерения, которые идут по блокам и двигают потолок блока ступенями под нагрузкой.
//
// Блок E-ядер меряется целиком, со всеми четырьмя ядрами под нагрузкой: потолок и температура у блока
// общие, и в обычной работе его ядра заняты вместе.
// Лесенка кривой (CurveTest) по очереди ставит блоку несколько потолков, на каждом записывает среднюю и
// наибольшую температуру, фактический множитель и прибавку мощности, а в конце блоку возвращается прежний
// потолок. Поиск под температуру (LimitSearch) ищет самый высокий потолок, при котором блок не горячее
// заданного, и оставляет его блоку.
// Общее у них собрано в StepTest: обход блоков, блок под нагрузкой, одна ступень. Исходные потолки они сами
// не хранят и не возвращают — этим занят LimitHold.
using System;
using System.Collections.Generic;
using System.Linq;

namespace CoreMap
{
    abstract class StepTest : Measurement
    {
        const double Between = 5;  // пауза между блоками, секунды

        protected readonly Monitor monitor;
        protected readonly CoreLimits limits;
        protected readonly CurveStore curves;
        readonly bool quick;       // имитация: паузы короткие
        readonly double loadSeconds, coolSeconds;
        protected int Level;       // сила нагрузки этого запуска

        public int Done, Total;            // сколько ступеней пройдено и сколько всего в этом запуске
        public int BlockDone, BlockTotal;  // то же по блокам
        public int StepDone, StepTotal;    // ступени блока, который меряется сейчас; StepTotal = 0 — заранее неизвестно
        public volatile Block[] Order;     // блоки этого запуска в порядке обхода: окно отмечает пройденные
        public string Current;             // что делается сейчас, для строки состояния
        public volatile Block Active;      // блок, который меряется сейчас; null — никакой

        protected StepTest(Runner runner, string failure, Monitor monitor, CoreLimits limits, CurveStore curves, bool quick)
            : base(runner, failure)
        {
            this.monitor = monitor;
            this.limits = limits;
            this.curves = curves;
            this.quick = quick;
            // Лесенка (решения владельца): нагрузка не снимается, потолок идёт ступенями снизу вверх и обратно
            // сверху вниз. На подъёме температура отстаёт вниз, на спуске — вверх; среднее двух проходов это
            // отставание гасит, поэтому ступень может быть короткой. Без нагрузки блок стоит только перед
            // первой ступенью. В расчёт идёт ступень без первой секунды.
            loadSeconds = quick ? 2 : 4;
            coolSeconds = quick ? 1 : 4;
        }

        // Окно рисует колонку потолков по счётчику хранилища кривых.
        protected override void Changed() { curves.Touch(); }

        protected override void Finish()
        {
            Active = null;
            Current = null;
        }

        /// Начало запуска: счётчики с нуля, исходные потолки взяты — дальше блокам можно ставить свои.
        /// steps — сколько ступеней предстоит; 0 — заранее неизвестно.
        protected void Begin(Block[] blocks, int level, int steps)
        {
            Level = level;
            Done = BlockDone = StepDone = StepTotal = 0;
            Total = steps;
            Order = blocks.Where(b => limits.Known(b.First)).ToArray();
            BlockTotal = blocks.Length;
            Hold.Take();
        }

        /// Обходит блоки с паузой между ними. Прекращает, когда измерение просят остановить или на блоке случилась ошибка.
        protected void Each(Block[] blocks, Action<Block> visit)
        {
            foreach (Block block in blocks)
            {
                if (!limits.Known(block.First)) continue;
                if (BlockDone > 0 && Wait(quick ? 1 : Between)) return;
                visit(block);
                if (Stopped || Error != null) return;
                BlockDone++;
            }
        }

        /// Ставит блоку потолок. false — отказ; причина становится ошибкой измерения: отказ записи не глотается.
        protected bool Put(Block block, int ratio)
        {
            string refusal = limits.Set(block.First, ratio);
            if (refusal != null) Error = refusal;
            return refusal == null;
        }

        /// Блок под нагрузкой. Ставит блоку потолок lowest, ждёт, запоминает температуру и мощность без нагрузки,
        /// нагружает все ядра блока и отдаёт их работе work. На любом пути выхода снимает нагрузку и возвращает
        /// блоку исходный потолок. Исключение одно: work вернула true — блоку остаётся потолок, который она поставила.
        protected void Under(Block block, int lowest, string prefix, Func<double, double, bool> work)
        {
            bool keep = false;
            Active = block;
            try
            {
                Current = prefix + "без нагрузки";
                if (!Put(block, lowest) || Wait(coolSeconds)) return;
                double idle = block.Temp(monitor.Typical(coolSeconds / 2)), idleWatts = monitor.Power(coolSeconds / 2);
                foreach (int m in block.Members) Load.Start(m, Level);
                keep = work(idle, idleWatts);
            }
            finally
            {
                foreach (int m in block.Members) Load.Stop(m);
                if (keep) Hold.Accept(block);
                else
                {
                    string refusal = Hold.Return(block);
                    if (refusal != null) Error = "прежний потолок не возвращён: " + refusal;
                }
            }
        }

        /// Одна ступень под нагрузкой: ставит потолок, держит ступень и возвращает точку. null — остановлено или отказ.
        protected CurvePoint Step(Block block, int ratio, double idle, double idleWatts)
        {
            if (!Put(block, ratio)) return null;
            double window = loadSeconds - 1, end = Native.Now() + loadSeconds, peak = 0, ratioSum = 0;
            int ratioCount = 0;
            while (Native.Now() < end && !Wait(0.25))
            {
                peak = Math.Max(peak, block.Temp(monitor.Cur));
                if (Native.Now() < end - window) continue;
                double ratioNow = block.Ratio(monitor.Ratio);
                if (double.IsNaN(ratioNow)) continue;
                ratioSum += ratioNow;
                ratioCount++;
            }
            if (Stopped) return null;
            return new CurvePoint
            {
                Limit = ratio, Ratio = ratioCount > 0 ? ratioSum / ratioCount : double.NaN, Peak = peak, Base = idle,
                Temp = block.Temp(monitor.Typical(window)), Watts = monitor.Power(window) - idleWatts,
            };
        }
    }

    /// Лесенка кривой: проходит потолки блока от низкого к высокому и обратно; прежний потолок блоку возвращается.
    class CurveTest : StepTest
    {
        const int Steps = 8;  // сколько потолков проходит, начиная от заводского предела вниз

        public CurveTest(Runner runner, Monitor monitor, CoreLimits limits, CurveStore curves, bool quick)
            : base(runner, "снятие кривых прервано", monitor, limits, curves, quick) { }

        /// Снимает кривые указанных блоков подряд. maxTemp — подъём по ступеням прекращается, когда температура
        /// ступени вышла за него; NaN — идти до верхней ступени.
        public void Start(Block[] blocks, int level, double maxTemp)
        {
            if (blocks.Length == 0) return;
            Launch(() =>
            {
                Begin(blocks, level, blocks.Sum(b => 2 * Math.Min(Steps, limits.Factory(b.First).Length)));
                Each(blocks, block => Climb(block, maxTemp));
                if (Stopped && Error == null) Error = "снятие кривых остановлено";
            });
        }

        /// Кривая одного блока.
        void Climb(Block block, double maxTemp)
        {
            int[] ratios = limits.Factory(block.First).Take(Steps).Reverse().ToArray();
            var run = new CurveRun { Lp = block.Lp, Level = Level, Time = DateTime.Now.ToString("yyyy-MM-dd HH:mm") };
            bool whole = false;
            StepDone = 0;
            StepTotal = 2 * ratios.Length;
            curves.Begin(run);
            try
            {
                Under(block, ratios[0], "", (idle, idleWatts) =>
                {
                    // Вверх по ступеням — до верхней или до первой, где температура ступени вышла за maxTemp:
                    // греть блок выше того, чем владелец собирается пользоваться, незачем.
                    var climbed = new List<CurvePoint>();
                    foreach (int ratio in ratios)
                    {
                        Current = "вверх · потолок ×" + ratio;
                        CurvePoint point = Step(block, ratio, idle, idleWatts);
                        if (point == null) break;
                        point.TempUp = point.Temp;
                        curves.Add(run, point);
                        climbed.Add(point);
                        Done++;
                        StepDone++;
                        if (curves.Heat(point) > maxTemp) break;
                    }
                    StepTotal = 2 * climbed.Count;  // подъём мог кончиться раньше верхней ступени: спуск — по пройденным
                    // Вниз по тем же ступеням; температура ступени — среднее подъёма и спуска.
                    for (int k = climbed.Count - 1; k >= 0 && !Stopped && Error == null; k--)
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
                        StepDone++;
                        curves.Touch();
                    }
                    whole = !Stopped && Error == null;
                    return false;  // потолок ступени блоку не остаётся
                });
            }
            finally { curves.Commit(run, whole); }
        }
    }

    /// Подбор под температуру: для каждого блока под нагрузкой ищет самый высокий потолок, при котором
    /// температура ступени не выше заданной, и оставляет его блоку. Нагрузка во время поиска не снимается,
    /// потолок двигается ступенями; поиск идёт делением пополам, потом найденное перепроверяется.
    /// Каждая пройденная ступень записывается точкой на кривую блока.
    /// Закончен блок — найденный потолок остаётся ему; прерван — блоку возвращается прежний.
    class LimitSearch : StepTest
    {
        readonly List<string> set = new List<string>(), hot = new List<string>();
        string back;      // блок, на котором подбор прервался: ему возвращён прежний потолок
        double maxTemp;

        public string Report;  // итог последнего подбора

        public LimitSearch(Runner runner, Monitor monitor, CoreLimits limits, CurveStore curves, bool quick)
            : base(runner, "подбор не удался", monitor, limits, curves, quick) { }

        public void Start(Block[] blocks, int level, double maxTemp)
        {
            if (blocks.Length == 0) return;
            Launch(() =>
            {
                this.maxTemp = maxTemp;
                Report = back = null;
                set.Clear();
                hot.Clear();
                Begin(blocks, level, 0);
                Each(blocks, Seek);
            });
        }

        /// Поиск для одного блока.
        void Seek(Block block)
        {
            int[] ratios = limits.Factory(block.First).Reverse().ToArray();  // по возрастанию
            string name = block.Name;
            int chosen = ratios[0];
            bool found = false, tooHot = false;  // tooHot — блок горячее предела и на самом низком потолке
            try
            {
                Under(block, ratios[0], name + " · ", (idle, idleWatts) =>
                {
                    // Деление пополам: температура растёт с множителем, ищется самый высокий подходящий.
                    int lo = 0, hi = ratios.Length - 1, best = -1;
                    CurvePoint bestPoint = null;
                    while (lo <= hi)
                    {
                        int mid = (lo + hi) / 2;
                        Current = name + " · пробую ×" + ratios[mid];
                        CurvePoint point = Step(block, ratios[mid], idle, idleWatts);
                        if (point == null) return false;
                        curves.Record(block, Level, point);
                        Current = name + " · ×" + ratios[mid] + " → " + curves.Heat(point).ToString("0.0") + " °C";
                        if (curves.Heat(point) <= maxTemp) { best = mid; bestPoint = point; lo = mid + 1; }
                        else hi = mid - 1;
                    }
                    // Перепроверка: после более горячей ступени температура ещё оседает, после более холодной —
                    // ещё растёт. Найденный потолок держится ещё ступень; не уложился — на ступень ниже.
                    while (best >= 0)
                    {
                        Current = name + " · проверяю ×" + ratios[best];
                        CurvePoint point = Step(block, ratios[best], idle, idleWatts);
                        if (point == null) return false;
                        curves.Record(block, Level, point);
                        bestPoint = point;
                        if (curves.Heat(point) <= maxTemp) break;
                        best--;
                    }
                    tooHot = best < 0;
                    chosen = ratios[Math.Max(best, 0)];
                    if (!tooHot && bestPoint != null) chosen = CurveStore.Settle(chosen, bestPoint.Ratio, ratios);
                    // Найденный потолок ставится блоку и остаётся ему. Процессор его не принял — блоку вернётся прежний.
                    found = limits.Of(block) == chosen || Put(block, chosen);
                    return found;
                });
            }
            finally
            {
                curves.Flush();  // пройденные ступени уже лежат на кривой блока
                if (found) (tooHot ? hot : set).Add(name + " ×" + chosen);
                else back = name;
            }
        }

        protected override void Finish()
        {
            base.Finish();
            string report = (Stopped ? "Подбор остановлен. " : "") + "Под " + maxTemp + " °C поставлено: " + (set.Count > 0 ? string.Join(", ", set) : "ничего");
            if (back != null) report += ". Блок " + back + " не подобран — ему возвращён прежний потолок";
            if (hot.Count > 0) report += ". Горячее предела даже на самом низком потолке: " + string.Join(", ", hot);
            if (Error != null) report += ". Ошибка: " + Error;
            Report = report + ". Потолки не сохранены — проверь под нагрузкой и сохрани профиль.";
        }
    }
}
