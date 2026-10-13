// Связка программы без окна: собирает датчики, нагрузку, потолки, профили и измерения, держит защиту от
// перегрева и среднюю под нагрузкой, принимает команды окна. Сами измерения, расчёты и хранилища — в своих
// файлах; здесь только то, что связывает их между собой.
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
        readonly Block[] spread;              // порядок обхода блоков в измерениях: вразброс по схеме
        public readonly LoadManager Load;
        public readonly Monitor Monitor;      // null — датчики недоступны
        public readonly ThermalSweep Sweep;
        public readonly LatencyTest Latency;
        public readonly Calibration Calibration;
        public readonly CoreLimits Limits;    // null — потолки недоступны (нет доступа к процессору)
        public readonly Profile Profile;      // профиль, который ставится при запуске программы; null — потолки недоступны
        public readonly ProfileSets Profiles; // все сохранённые профили по названиям
        public readonly Runner Runner;        // исполнитель измерений: в каждый момент идёт не больше одного
        public readonly Fans Fans;            // вентиляторы ноутбука; опрос начинается, когда его запустит окно (Fans.Start)
        // Кривые «температура от потолка» и три измерения по ним; null — нет потолков или датчиков.
        public readonly CurveStore Curves;    // снятые кривые и подбор потолка по ним
        public readonly CurveFit Fit;         // расчёт потолков под температуру по снятым кривым
        public readonly CurveTest Curve;      // лесенка: снятие кривой
        public readonly LimitSearch Search;   // поиск потолков замером под температуру
        public readonly JointTest Joint;      // проверка под общей нагрузкой
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
            spread = Layout.Spread(Blocks);
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
                    catch (SensorException e) { Program.Tell("Потолки множителей недоступны: " + Program.Why(e) + "."); }
                }
                catch (SensorException e) { SensorProblem = e.Message; }

            LimitHold hold = Limits == null ? null : new LimitHold(Limits, Blocks, Lps, CpuName, simulate ? null : Program.DataDir);
            // Прошлый запуск мог оборваться посреди измерения и оставить блоку потолок ступени. Он возвращается
            // до профиля: тот сверяет свои потолки с действующими и решает, применять ли себя.
            string recovered = hold == null ? null : hold.Recover();
            if (recovered != null) Program.Tell(recovered);  // полоса сообщений в окне сменится, запись в журнале останется
            // Доступ к вентиляторам создаётся в их потоке и только после Fans.Start: без окна он не нужен.
            Fans = new Fans(simulate ? (Func<IFanBackend>)(() => new SimFans(() => Monitor == null ? 45 : Monitor.PkgCur)) : () => new HpFans(),
                simulate ? null : Program.DataDir, simulate);
            Runner = new Runner(Load, hold);
            // «Было» читается из процессора до того, как профиль запуска встанет поверх: после перезагрузки это
            // заводские потолки, и разница с профилем должна быть видна, а откат — возвращать к ним.
            if (Limits != null) was = (int[])Limits.Current.Clone();

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
                Monitor = new Monitor(backend, Lps.Length, 0.5) { OnSample = Sampled };
                Monitor.Start();
            }
            string saveDir = simulate ? null : Program.DataDir;  // имитация ничего не пишет: её числа ненастоящие
            Sweep = new ThermalSweep(Runner, Monitor, Lps, Blocks.All, spread, saveDir);
            Latency = new LatencyTest(Runner, Lps, simulate, saveDir);
            Calibration = new Calibration(Runner, Monitor, Cores, CpuName, saveDir);
            try { Calibration.LoadSaved(); }
            catch (Exception e) { Program.Tell("Прошлый подбор нагрузки не прочитан: " + Program.Why(e) + ".", e); }
            if (Limits != null && Monitor != null)
            {
                Curves = new CurveStore(Limits, CpuName, saveDir);
                Fit = new CurveFit(Blocks, Limits, Curves);
                Curve = new CurveTest(Runner, Monitor, Limits, Curves, simulate);
                Search = new LimitSearch(Runner, Monitor, Limits, Curves, simulate);
                Joint = new JointTest(Runner, Monitor, Limits, Curves, Lps, CpuName, saveDir);
                try { Curves.LoadSaved(simulate && showSaved ? Path.Combine(Program.DataDir, "curves.json") : null); }
                catch (Exception e) { Program.Tell("Сохранённые кривые не прочитаны: " + Program.Why(e) + ".", e); }
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
            catch (Exception e) { Program.Tell("Прошлый замер " + Path.GetFileName(latest) + " не прочитан: " + Program.Why(e) + ".", e); }
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

        /// После каждого опроса датчиков, в их потоке: средняя под нагрузкой и защита от перегрева.
        /// temps = null — датчики не прочитались.
        void Sampled(double[] temps)
        {
            // Средняя — показание для окна: её ошибка не должна оставить программу без защиты.
            try { Average(temps); }
            catch (Exception e) { Program.LogOnce("средняя под нагрузкой", e); }
            try { Guard(temps); }
            catch (Exception e)
            {
                // Защита не отработала, и перегрева она сейчас не увидит: нагрузка снимается.
                Program.LogOnce("защита от перегрева", e);
                Trip = "защита от перегрева дала ошибку: " + e.Message + ". Подробности записаны в data\\error.log";
                Runner.Stop();
                Load.StopAll();
            }
        }

        /// Снимает нагрузку и останавливает измерение, если ядро держится на пределе или датчики замолчали.
        void Guard(double[] temps)
        {
            // Идущее измерение — тоже нагрузка, даже когда у LoadManager пусто: измерение задержек занимает ядра
            // своими потоками.
            if (!Load.Any() && !Runner.Busy) { hotSince = double.NaN; return; }
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

        public void StartLatency()
        {
            if (Busy) return;
            Trip = null;
            Latency.Start();
        }

        // Профили. Состояний два, оба — потолки в процессоре: «было» и «сейчас».
        // «Было» — то, что прочитано из процессора при запуске программы (до того как профиль запуска встал
        // поверх), а дальше — то, что принято последним. «Сейчас» — то, что стоит в процессоре в эту минуту.
        // Всё, что показывает разницу потолков, считает её как «сейчас минус было». Открыть профиль, подвинуть
        // ползунок, поставить потолок вручную — меняет только «сейчас». «Принять» и «сохранить» делают «сейчас»
        // новым «было»; «откатить» возвращает «было» в процессор.
        // Отдельно от этого — профиль запуска (Profile): какой сохранённый набор ставится при запуске программы.
        // Им становится профиль, который приняли или сохранили последним.

        int[] was;

        /// Потолки, которые были: прочитанные из процессора при запуске или принятые последними; null — потолки недоступны.
        public int[] Was { get { return was; } }

        /// Набор совпадает с другим (ядра, о которых процессор не ответил, не в счёт).
        bool Equal(int[] a, int[] b)
        {
            return Enumerable.Range(0, a.Length).All(i => !Limits.Known(i) || a[i] == b[i]);
        }

        /// Каким сохранённым набором является данный: название профиля, "" — заводские потолки, null — ни одним
        /// (потолки ставились вручную, ползунком или измерением). Если подходит несколько профилей, первым идёт
        /// профиль запуска, затем preferred, затем по алфавиту.
        public string Named(int[] ratios, string preferred = null)
        {
            if (Limits == null || Profiles == null || ratios == null) return null;
            if (!Profile.Exists && Equal(ratios, Limits.Fused)) return "";
            if (Profile.Exists && Profiles.Get(Profile.Name) != null && Equal(ratios, Profile.Ratios)) return Profile.Name;
            if (preferred != null && Profiles.Get(preferred) != null && Equal(ratios, Profiles.Get(preferred))) return preferred.Trim();
            foreach (string name in Profiles.Names)
                if (Equal(ratios, Profiles.Get(name))) return name;
            return Equal(ratios, Limits.Fused) ? "" : null;
        }

        /// Какой сохранённый набор стоит в процессоре сейчас — см. Named.
        public string Standing(string preferred = null) { return Limits == null ? null : Named(Limits.Current, preferred); }

        bool AtStartup(string name)
        {
            return Profile.Exists && name != null && string.Equals(Profile.Name, name.Trim(), StringComparison.CurrentCultureIgnoreCase);
        }

        /// Сохраняет то, что стоит сейчас, профилем с данным названием и принимает его: он становится «было»
        /// и профилем запуска. null — сделано; иначе причина отказа.
        public string SaveProfile(string name)
        {
            if (Profile == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас временные, сохранять их нельзя";
            if (string.IsNullOrWhiteSpace(name)) return "впиши название профиля";
            try
            {
                Profiles.Put(name, Limits.Current);
                Profile.Save(name.Trim());
                was = (int[])Limits.Current.Clone();
            }
            catch (Exception e) { return e.Message; }
            return null;
        }

        /// Открывает сохранённый профиль: ставит его потолки, «было» остаётся прежним — так видна разница профиля
        /// с тем, что было. name = "" — заводские потолки.
        public string OpenProfile(string name)
        {
            if (Profile == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас менять нельзя";
            int[] ratios = name == "" ? Limits.Fused : Profiles.Get(name);
            if (ratios == null) return "профиля «" + name + "» нет";
            return Limits.Apply(ratios);
        }

        /// Есть ли что принимать: «сейчас» отличается от «было», либо стоит набор, который ещё не профиль запуска
        /// (в том числе заводские при назначенном профиле и профиль, отключённый после сбоя, — так он включается заново).
        public bool CanAccept(string preferred = null)
        {
            if (Profile == null || Busy) return false;
            if (!Equal(Limits.Current, was)) return true;
            string standing = Standing(preferred);
            if (standing == null) return false;
            if (standing == "") return Profile.Exists;
            return !AtStartup(standing) || Profile.Disabled;
        }

        /// Принимает то, что стоит сейчас: оно становится «было». Если это сохранённый профиль, он же становится
        /// профилем запуска; если заводские потолки — при запуске ничего ставиться не будет; несохранённые потолки
        /// профиль запуска не меняют. Сами профили не меняются.
        public string Accept(string preferred = null)
        {
            if (Profile == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас временные";
            string standing = Standing(preferred);
            try
            {
                if (standing == "") Profile.Forget();
                // Профиль запуска, который и так стоит, заново не записывается: это обнулило бы его испытательный срок.
                else if (standing != null && (!AtStartup(standing) || Profile.Disabled)) Profile.Save(standing);
            }
            catch (Exception e) { return e.Message; }
            was = (int[])Limits.Current.Clone();
            return null;
        }

        /// Возвращает в процессор потолки, которые были.
        public string Revert()
        {
            if (Limits == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: потолки сейчас менять нельзя";
            return Limits.Apply(was);
        }

        /// Удаляет сохранённый профиль. Если он был профилем запуска, при запуске программы ничего ставиться не будет.
        public string DeleteProfile(string name)
        {
            if (Profile == null) return "потолки множителей недоступны";
            try
            {
                Profiles.Delete(name);
                if (AtStartup(name)) Profile.Forget();
            }
            catch (Exception e) { return e.Message; }
            return null;
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
            Curve.Start(spread, Load.Level, maxTemp);
        }

        /// Подбирает каждому блоку под нагрузкой текущей силы самый высокий потолок, при котором средняя
        /// температура не выше maxTemp. null — подбор пошёл; иначе причина отказа.
        public string FitLimits(double maxTemp)
        {
            if (Limits == null || Curve == null) return "потолки множителей недоступны";
            if (Busy) return "идёт измерение: дождись его конца";
            Trip = null;
            Search.Start(spread, Load.Level, maxTemp);
            return null;
        }

        /// Ставит блокам потолки под температуру по уже снятым кривым, без нагрузки (ползунок); best — «по выгоде».
        /// Возвращает итог одной строкой.
        public string FitFromCurves(double maxTemp, bool best = false)
        {
            if (Fit == null) return "Потолки множителей недоступны.";
            if (Busy) return "Идёт измерение: потолки сейчас менять нельзя.";
            return Fit.Apply(Load.Level, maxTemp, best).Describe();
        }

        /// Замер под общей нагрузкой: все блоки разом при потолках, которые стоят сейчас, на текущей силе нагрузки.
        public void StartJoint()
        {
            if (Busy || Curve == null) return;
            Trip = null;
            Joint.Start(Load.Level);
        }

        public void StartCalibration()
        {
            if (Busy) return;
            Trip = null;
            Calibration.Start();
        }

        /// Выбор владельца, когда программа сама включает вентиляторам «максимум» (Fans.Auto / Measure / Always).
        /// null — принят; иначе причина отказа.
        public string SetFanMode(int mode)
        {
            if (Busy) return "идёт измерение: режим вентиляторов сейчас менять нельзя";
            return Fans.SetMode(mode);
        }

        public void Close()
        {
            // Измерение возвращает потолки и дописывает файл в своём потоке. Без ожидания процесс завершался раньше,
            // и ядро оставалось с потолком текущей ступени. Не дождались за три секунды — потолки при следующем
            // запуске вернёт метка (LimitHold.Recover).
            Runner.Close(3);
            Load.StopAll();
            // Максимум, включённый программой, с её закрытием снимается. Не снялся — останется метка
            // (data\fans-held.json), и его снимет следующий запуск.
            Fans.Close();
        }
    }
}
