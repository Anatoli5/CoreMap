// Ядро программы без интерфейса: датчики, нагрузка, оба измерения и защита от перегрева.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace CoreMap
{
    class Core
    {
        const int GuardMargin = 5;       // предел по умолчанию: на столько ниже порога, где процессор сам сбрасывает частоту
        const double GuardSeconds = 3;   // столько ядро должно держаться на пределе, чтобы нагрузка снялась

        public readonly CpuCore[] Cores;
        public readonly int[] Lps;
        /// Блоки: P-ядро или кластер E-ядер целиком. Список один на всю программу — см. Block.cs.
        public readonly BlockMap Blocks;
        public readonly string CpuName;
        public readonly DieLayout Layout;
        public readonly LoadManager Load;
        public readonly Monitor Monitor;      // null — датчики недоступны
        public readonly ThermalSweep Sweep;
        public readonly LatencyTest Latency;
        public readonly Calibration Calibration;
        public readonly CoreLimits Limits;    // null — потолки недоступны (нет доступа к процессору)
        public readonly Profile Profile;      // действующий профиль потолков; null — потолки недоступны
        public readonly ProfileSets Profiles; // все сохранённые профили по названиям
        public readonly Runner Runner;        // исполнитель измерений: в каждый момент идёт не больше одного
        // Кривые «температура от потолка» и три измерения по ним; null — нет потолков или датчиков.
        public readonly CurveStore Curves;    // снятые кривые и подбор потолка по ним
        public readonly CurveTest Curve;      // лесенка: снятие кривой
        public readonly LimitSearch Search;   // поиск потолков замером под температуру
        public readonly JointTest Joint;      // проверка под общей нагрузкой
        public readonly string Notice;        // что сделано с потолками при запуске после оборванного измерения; null — ничего
        public readonly bool Simulated;
        public readonly string SensorProblem; // почему датчики недоступны
        public readonly int ThrottleAt, Limit;
        public volatile string Trip;          // почему защита сняла нагрузку
        double hotSince = double.NaN;
        public readonly double[] LoadAverage;  // средняя температура ядра под текущей нагрузкой; NaN — нагрузки не было
        public readonly double[] LoadRatio;    // средний фактический множитель за то же время; NaN — ядро не работало
        readonly double[] averageSum, ratioSum;
        readonly int[] averageCount, ratioCount;
        string averageKey;
        volatile bool averageReset;

        public bool Busy { get { return Runner.Busy; } }

        /// showSaved — проверка окна на имитации: показать и сохранённую тепловую таблицу с настоящих датчиков.
        public Core(bool simulate, int limit, bool showSaved = false)
        {
            Cores = Topology.Read().ToArray();
            Lps = Cores.Select(c => c.Lp).ToArray();
            Blocks = new BlockMap(Cores);
            LoadAverage = Enumerable.Repeat(double.NaN, Lps.Length).ToArray();
            LoadRatio = Enumerable.Repeat(double.NaN, Lps.Length).ToArray();
            averageSum = new double[Lps.Length];
            averageCount = new int[Lps.Length];
            ratioSum = new double[Lps.Length];
            ratioCount = new int[Lps.Length];
            string identifier;
            Topology.Identity(out CpuName, out identifier);
            Layout = DieLayout.Pick(Cores, Blocks, identifier, Path.Combine(Program.Root, "layouts"));
            double[][] positions = Layout.Positions(Lps.Length);
            Load = new LoadManager(Lps, simulate);
            Simulated = simulate;

            ISensorBackend backend = null;
            if (simulate)
            {
                backend = new SimBackend(positions, Load.Active);
                Limits = new CoreLimits(null, Cores, Blocks);
            }
            else
                try
                {
                    var io = new PawnIo(Path.Combine(Program.Root, "modules", "IntelMSR.bin"));
                    backend = new LiveBackend(io, Lps);
                    try { Limits = new CoreLimits(io, Cores, Blocks); }
                    catch (SensorException e) { Program.Log("потолки множителей недоступны: " + e.Message); }
                }
                catch (SensorException e) { SensorProblem = e.Message; }

            LimitHold hold = Limits == null ? null : new LimitHold(Limits, Blocks, Lps, CpuName, simulate ? null : Program.DataDir);
            // Прошлый запуск мог оборваться посреди измерения и оставить блоку потолок ступени. Он возвращается
            // до профиля: тот сверяет свои потолки с действующими и решает, применять ли себя.
            if (hold != null) Notice = hold.Recover();
            if (Notice != null) Program.Log(Notice);  // полоса сообщений в окне сменится, запись останется
            Runner = new Runner(Load, hold);

            if (Limits != null)
            {
                Profile = new Profile(Limits, Lps, CpuName, simulate ? null : Program.DataDir);
                Profiles = new ProfileSets(Lps, CpuName, simulate ? null : Program.DataDir);
                try
                {
                    Profiles.Read();
                    Profile.Startup();
                    // Профиль, сохранённый до появления названий, попадает в общий список.
                    if (Profile.Exists && Profiles.Get(Profile.Name) == null) Profiles.Put(Profile.Name, Profile.Ratios);
                }
                catch (Exception e)
                {
                    Program.Log("профиль потолков не прочитан: " + e);
                    Profile.Note = "профиль не прочитан: " + e.Message;
                }
            }

            if (backend != null)
            {
                ThrottleAt = backend.ThrottleAt;
                // Свой предел принимается только в разумных границах: опечатка вроде 950 оставила бы машину без защиты.
                // В имитации границ нет — там он нужен для проверки окна.
                Limit = (simulate ? limit > 0 : limit >= 50 && limit <= ThrottleAt) ? limit : ThrottleAt - GuardMargin;
                Monitor = new Monitor(backend, Lps.Length, 0.5) { OnSample = Guard };
                Monitor.Start();
            }
            string saveDir = simulate ? null : Program.DataDir;  // имитация ничего не пишет: её числа ненастоящие
            Sweep = new ThermalSweep(Runner, Monitor, Lps, Blocks.All, positions, saveDir);
            Latency = new LatencyTest(Runner, Lps, Program.DataDir);
            Calibration = new Calibration(Runner, Monitor, Cores, CpuName, saveDir);
            try { Calibration.LoadSaved(); }
            catch (Exception e) { Program.Log("не прочитан прошлый подбор нагрузки: " + e); }
            if (Limits != null && Monitor != null)
            {
                Curves = new CurveStore(Limits, CpuName, saveDir);
                Curve = new CurveTest(Runner, Monitor, Limits, Curves, simulate);
                Search = new LimitSearch(Runner, Monitor, Limits, Curves, simulate);
                Joint = new JointTest(Runner, Monitor, Limits, Curves, Lps, CpuName, saveDir);
                try { Curves.LoadSaved(simulate && showSaved ? Path.Combine(Program.DataDir, "curves.json") : null); }
                catch (Exception e) { Program.Log("не прочитаны сохранённые кривые: " + e); }
            }
            if (!simulate || showSaved) LoadLatest("thermal-*.json", Sweep.LoadSaved);
            LoadLatest("latency-*.json", Latency.LoadSaved);
        }

        static void LoadLatest(string pattern, Action<string> load)
        {
            if (!Directory.Exists(Program.DataDir)) return;
            string latest = Directory.GetFiles(Program.DataDir, pattern).OrderBy(p => p).LastOrDefault();
            if (latest == null) return;
            try { load(latest); }
            catch (Exception e) { Program.Log("не прочитан файл " + latest + ": " + e); }
        }

        const int AverageSkip = 4;  // столько первых опросов (2 секунды) после смены условий в среднюю не идут: ядро ещё греется
        int averageSkipped;

        /// Средняя температура ядра под его собственной нагрузкой. Считается только пока само ядро нагружено, и
        /// всегда относится к одной комбинации: отсчёт начинается заново, когда меняется набор нагруженных ядер,
        /// сила нагрузки или потолки множителей; первые две секунды после этого не учитываются — ядро ещё греется.
        /// У ядра без нагрузки значение не меняется: остаётся то, что было под его последней нагрузкой.
        void Average(double[] temps)
        {
            if (temps == null) return;
            bool[] active = Load.Active();
            if (!active.Any(x => x)) { averageKey = null; return; }
            string key = string.Concat(active.Select(x => x ? '1' : '0')) + Load.Level +
                (Limits == null ? "" : string.Join(",", Limits.Current));
            if (key != averageKey || averageReset)
            {
                averageKey = key;
                averageReset = false;
                averageSkipped = 0;
                Array.Clear(averageSum, 0, averageSum.Length);
                Array.Clear(averageCount, 0, averageCount.Length);
                Array.Clear(ratioSum, 0, ratioSum.Length);
                Array.Clear(ratioCount, 0, ratioCount.Length);
                for (int i = 0; i < LoadRatio.Length; i++)
                    if (active[i]) LoadAverage[i] = LoadRatio[i] = double.NaN;
            }
            if (averageSkipped < AverageSkip) { averageSkipped++; return; }
            double[] ratio = Monitor.Ratio;
            for (int i = 0; i < temps.Length; i++)
            {
                if (!active[i] || double.IsNaN(temps[i])) continue;
                averageSum[i] += temps[i];
                LoadAverage[i] = averageSum[i] / ++averageCount[i];
                if (double.IsNaN(ratio[i])) continue;
                ratioSum[i] += ratio[i];
                LoadRatio[i] = ratioSum[i] / ++ratioCount[i];
            }
        }

        /// Сбрасывает средние вместе с максимумами.
        public void ResetAverage()
        {
            averageReset = true;
            for (int i = 0; i < LoadAverage.Length; i++) LoadAverage[i] = LoadRatio[i] = double.NaN;
        }

        double blindSince = double.NaN;

        /// У нагруженного ядра нет показаний дольше, чем защита ждёт перегрева: его температуру никто не видит.
        bool Blind(double[] temps)
        {
            bool[] active = Load.Active();
            if (!Enumerable.Range(0, temps.Length).Any(i => active[i] && double.IsNaN(temps[i]))) { blindSince = double.NaN; return false; }
            if (double.IsNaN(blindSince)) blindSince = Native.Now();
            return Native.Now() - blindSince >= GuardSeconds;
        }

        /// Снимает нагрузку и останавливает измерение, если ядро держится на пределе или датчики замолчали.
        void Guard(double[] temps)
        {
            Average(temps);
            if (!Load.Any()) { hotSince = double.NaN; return; }
            string reason;
            if (temps == null) reason = "датчики перестали отвечать";
            else if (Blind(temps)) reason = "датчик нагруженного ядра перестал отвечать";
            else
            {
                int hottest = Enumerable.Range(0, temps.Length).OrderByDescending(i => double.IsNaN(temps[i]) ? -1 : temps[i]).First();
                if (!(temps[hottest] >= Limit)) { hotSince = double.NaN; return; }
                if (double.IsNaN(hotSince)) hotSince = Native.Now();
                if (Native.Now() - hotSince < GuardSeconds) return;
                reason = "ядро " + Lps[hottest] + " держалось на " + temps[hottest] + " °C";
            }
            Runner.Stop();
            Load.StopAll();
            Trip = reason;
        }

        // Команды интерфейса. Пока идёт измерение, ручная нагрузка и её настройки не меняются.

        public void ToggleLoad(int index)
        {
            if (Busy || Monitor == null) return;  // без датчиков нет и защиты от перегрева: нагрузку не даём
            if (Load.Active()[index]) Load.Stop(index);
            else { Trip = null; Load.Start(index); }
        }

        /// Включает или снимает нагрузку со всех ядер блока разом: если хоть одно нагружено — снимает.
        public void ToggleLoad(Block block)
        {
            if (Busy || Monitor == null) return;  // без датчиков нет и защиты от перегрева: нагрузку не даём
            bool[] active = Load.Active();
            if (block.Members.Any(i => active[i]))
            {
                foreach (int i in block.Members) Load.Stop(i);
                return;
            }
            Trip = null;
            foreach (int i in block.Members) Load.Start(i);
        }

        public void StopLoad() { if (!Busy) Load.StopAll(); }

        public void SetLevel(int level) { if (!Busy) Load.SetLevel(level); }

        public void StartSweep(int seconds, int passes)
        {
            if (Busy) return;
            Trip = null;
            Sweep.Start(seconds, passes);
        }

        public void StartLatency() { if (!Busy) Latency.Start(); }

        /// Сохраняет действующие потолки профилем с данным названием и делает его действующим.
        /// null — сделано; иначе причина отказа.
        public string SaveProfile(string name)
        {
            if (Profile == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас временные, сохранять их нельзя";
            if (string.IsNullOrWhiteSpace(name)) return "впиши название профиля";
            try
            {
                Profiles.Put(name, Limits.Current);
                Profile.Save(name.Trim());
            }
            catch (Exception e) { return e.Message; }
            return null;
        }

        /// Ставит потолки из сохранённого профиля и делает его действующим.
        public string ApplyProfile(string name)
        {
            if (Profile == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас менять нельзя";
            int[] ratios = Profiles.Get(name);
            if (ratios == null) return "профиля «" + name + "» нет";
            string refusal = Limits.Apply(ratios);
            if (refusal != null) return refusal;
            try { Profile.Save(name.Trim()); }
            catch (Exception e) { return e.Message; }
            return null;
        }

        /// Удаляет сохранённый профиль; действующий перестаёт применяться при запуске.
        public string DeleteProfile(string name)
        {
            if (Profile == null) return "потолки множителей недоступны";
            try
            {
                Profiles.Delete(name);
                if (Profile.Exists && string.Equals(Profile.Name, name.Trim(), StringComparison.CurrentCultureIgnoreCase)) Profile.Forget();
            }
            catch (Exception e) { return e.Message; }
            return null;
        }

        /// Возвращает потолки, какими они были при запуске программы (до применения профиля).
        public string RestoreLimits()
        {
            if (Limits == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас менять нельзя";
            return Limits.Restore();
        }

        /// Ставит ядру потолок множителя. null — принято; иначе причина отказа.
        public string SetLimit(int index, int ratio)
        {
            if (Limits == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас менять нельзя";
            return Limits.Set(index, ratio);
        }

        /// Снимает кривую «температура от множителя» для блока на текущей силе нагрузки.
        public void StartCurve(Block block, double maxTemp)
        {
            if (Busy || Curve == null) return;
            Trip = null;
            Curve.Start(new[] { block }, Load.Level, maxTemp);
        }

        /// Снимает кривые всех блоков подряд.
        public void StartAllCurves(double maxTemp)
        {
            if (Busy || Curve == null) return;
            Trip = null;
            Curve.Start(SpreadBlocks(), Load.Level, maxTemp);
        }

        /// Порядок обхода блоков, в котором соседние по списку блоки разнесены: половины списка чередуются.
        Block[] SpreadBlocks()
        {
            int half = (Blocks.All.Length + 1) / 2;
            return Blocks.All.OrderBy(b => b.Index < half ? 2 * b.Index : 2 * (b.Index - half) + 1).ToArray();
        }

        /// Подбирает каждому блоку под нагрузкой текущей силы самый высокий потолок, при котором средняя
        /// температура не выше maxTemp. null — подбор пошёл; иначе причина отказа.
        public string FitLimits(double maxTemp)
        {
            if (Limits == null || Curve == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: дождись его конца";
            Trip = null;
            Search.Start(SpreadBlocks(), Load.Level, maxTemp);
            return null;
        }

        /// Что снятые кривые говорят о потолке каждого блока под температуру; null на месте блока — кривой нет.
        /// best — не ровная отсечка, а передел: см. Rebalance.
        public CurveChoice[] ChooseAll(double maxTemp, bool best = false)
        {
            bool[] known = Blocks.All.Select(b => Curve != null && Limits != null && Limits.Known(b.First)).ToArray();
            CurveChoice[] even = Blocks.All.Select(b => known[b.Index] ? Curves.Choose(b, Load.Level, maxTemp) : null).ToArray();
            if (!best) return even;
            return Rebalance(even, Blocks.All.Select(b => known[b.Index] ? Curves.Options(b, Load.Level) : null).ToArray(), maxTemp);
        }

        public const double Spread = 6;  // на сколько градусов блок при переделе может уйти выше температуры с ползунка

        /// Передел потолков «по выгоде» (идея владельца): сумма температур блоков остаётся той же, что при ровной
        /// отсечке even, но делится иначе — так, чтобы сумма множителей была наибольшей. Блок, которому градус даёт
        /// много частоты, поднимается выше отсечки (не больше чем на Spread), блок, у которого ступень стоит
        /// градусов и частоты не добавляет, опускается. Перебор точный: вариантов на блок — по числу ступеней его
        /// кривой, температуры считаются в десятых долях градуса.
        static CurveChoice[] Rebalance(CurveChoice[] even, CurveChoice[][] options, double maxTemp)
        {
            int n = even.Length;
            var allowed = new CurveChoice[n][];
            var weight = new int[n][];
            int room = 0;  // сколько десятых градуса можно раздать сверх самых низких вариантов
            for (int u = 0; u < n; u++)
            {
                if (even[u] == null || options[u] == null) continue;
                // Самый низкий вариант разрешён всегда: иначе блоку, горячему уже на нижней точке, нечего было бы дать.
                allowed[u] = options[u].Where((o, k) => k == 0 || o.Temp <= maxTemp + Spread).ToArray();
                weight[u] = allowed[u].Select(o => (int)Math.Round((o.Temp - allowed[u][0].Temp) * 10)).ToArray();
                room += (int)Math.Round((even[u].Temp - allowed[u][0].Temp) * 10);
            }
            var value = new double[room + 1];
            var pick = new int[n][];
            for (int u = 0; u < n; u++)
            {
                if (allowed[u] == null) continue;
                var next = new double[room + 1];
                pick[u] = new int[room + 1];
                for (int c = 0; c <= room; c++)
                {
                    next[c] = double.NegativeInfinity;
                    for (int k = 0; k < allowed[u].Length; k++)
                    {
                        if (weight[u][k] > c) continue;
                        double v = value[c - weight[u][k]] + allowed[u][k].Ratio;
                        if (v > next[c] + 1e-9) { next[c] = v; pick[u][c] = k; }
                    }
                }
                value = next;
            }
            var result = new CurveChoice[n];
            for (int u = n - 1, c = room; u >= 0; u--)
            {
                if (allowed[u] == null) continue;
                int k = pick[u][c];
                result[u] = allowed[u][k];
                result[u].Hot = k == 0 && result[u].Temp > maxTemp;
                c -= weight[u][k];
            }
            return result;
        }

        /// Ставит блокам потолки под температуру по уже снятым кривым, без нагрузки; блоки без кривой не трогает.
        /// Возвращает итог одной строкой.
        public string FitFromCurves(double maxTemp, bool best = false)
        {
            if (Limits == null || Curve == null) return "Потолки множителей недоступны.";
            if (Busy) return "Идёт измерение: потолки сейчас менять нельзя.";
            var none = new List<string>();
            var hot = new List<string>();
            var top = new List<string>();
            string refusal = null;
            int set = 0;
            CurveChoice[] picks = ChooseAll(maxTemp, best);
            foreach (Block block in Blocks.All)
            {
                int index = block.First;
                if (!Limits.Known(index)) continue;
                CurveChoice choice = picks[block.Index];
                if (choice == null) { none.Add(block.Name); continue; }
                if (Limits.Of(block) != choice.Limit) refusal = Limits.Set(index, choice.Limit) ?? refusal;
                set++;
                if (choice.Hot) hot.Add(block.Name + " (×" + choice.Limit + " — " + choice.Temp.ToString("0") + "°)");
                else if (choice.Top) top.Add(block.Name);
            }
            if (set == 0) return "Под " + maxTemp + " °C ничего не поставлено: на выбранной силе нагрузки кривых нет — сними их кнопкой ▶▶.";
            string report = "Под " + maxTemp + " °C по кривым" + (best ? ", по выгоде," : "") + " поставлено блоков: " + set + " из " + Blocks.All.Length +
                ". Это проба: чтобы потолки остались, сохрани профиль.";
            if (none.Count > 0) report += " Нет кривой: " + string.Join(", ", none) + ".";
            if (hot.Count > 0) report += " Горячее цели и на нижней точке кривой: " + string.Join(", ", hot) + ".";
            if (top.Count > 0) report += " Кривая выше не снята: " + string.Join(", ", top) + ".";
            if (refusal != null) report += " Отказ: " + refusal + ".";
            return report;
        }

        /// Замер под общей нагрузкой: все блоки разом при потолках, которые стоят сейчас, на текущей силе нагрузки.
        public void StartJoint()
        {
            if (Busy || Curve == null) return;
            Trip = null;
            Joint.Start(Load.Level);
        }

        /// Температура блока по его снятой кривой при потолке, который стоит сейчас; NaN — кривой нет или потолок вне её.
        public double CurveTemp(Block block)
        {
            int index = block.First;
            if (Curve == null || Limits == null || !Limits.Known(index)) return double.NaN;
            CurveChoice[] options = Curves.Options(block, Load.Level);
            CurveChoice at = options == null ? null : options.LastOrDefault(o => o.Limit == Limits.Of(block));
            return at == null ? double.NaN : at.Temp;
        }

        public void StartCalibration()
        {
            if (Busy) return;
            Trip = null;
            Calibration.Start();
        }

        public void Close()
        {
            // Измерение возвращает потолки и дописывает файл в своём потоке. Без ожидания процесс завершался раньше,
            // и ядро оставалось с потолком текущей ступени. Не дождались за три секунды — потолки при следующем
            // запуске вернёт метка (LimitHold.Recover).
            Runner.Close(3);
            Load.StopAll();
        }
    }
}
