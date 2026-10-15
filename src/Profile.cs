// Профили потолков множителей.
//
// Профилей может быть несколько, у каждого своё название (ProfileSets, data\profiles.json). Один из них —
// профиль запуска (Profile, data\profile.json): тот, что сохранён или принят последним; он ставится заново
// при запуске программы. Профиль, выбранный в списке, только ставит свои потолки — профилем запуска он
// становится, когда его принимают. От чего окно считает разницу потолков («было»), решает Core, а не профиль:
// это то, что прочитано из процессора.
//
// Изменение потолка в окне — проба: оно действует сразу, нигде не записывается и пропадает при
// перезагрузке. Профиль появляется только по кнопке «сохранить» и хранится в data\profile.json;
// в прошивку ноутбука ничего не пишется, поэтому после любого сбоя машина загружается на прежних
// значениях. При запуске программы профиль применяется заново — но не раньше, чем проверено по
// журналу Windows, что прошлое применение не кончилось сбоем.
using System;
using System.Collections.Generic;
using System.Diagnostics.Eventing.Reader;
using System.Globalization;
using System.IO;
using System.Linq;

namespace CoreMap
{
    class Profile
    {
        // Испытательный срок профиля с потолками выше заводских: столько штатных перезапусков и столько
        // часов работы он должен пережить без сбоев, чтобы считаться проверенным.
        const int TrialBoots = 1;
        const double TrialHours = 3;
        const string Trial = "trial", Trusted = "trusted", Failed = "failed";

        readonly CoreLimits limits;
        readonly int[] lps;
        readonly string cpu, path;  // path = null — имитация: профиль живёт только в памяти

        public int[] Ratios;        // null — профиля нет
        public string Name = "";
        public string State, Failure;
        public DateTime Saved, Applied;
        public int Boots;
        public double Hours;
        public string Note;         // что пошло не так с профилем при запуске программы; null — всё штатно
        public string AtStartup;    // во сколько профиль поставлен при этом запуске программы; null — не ставился

        public Profile(CoreLimits limits, int[] lps, string cpu, string saveDir)
        {
            this.limits = limits;
            this.lps = lps;
            this.cpu = cpu;
            path = saveDir == null ? null : Path.Combine(saveDir, "profile.json");
        }

        public bool Exists { get { return Ratios != null; } }

        /// Действующие потолки совпадают с профилем.
        public bool InEffect { get { return Exists && limits.Same(Ratios); } }

        /// Профиль отключён после сбоя: при запуске программы он не ставится.
        public bool Disabled { get { return Exists && State == Failed; } }

        /// В профиле есть потолок выше заводского предела: такой профиль проходит испытательный срок.
        bool Risky { get { return Enumerable.Range(0, lps.Length).Any(i => limits.Known(i) && Ratios[i] > limits.Fused[i]); } }

        /// Делает то, что стоит в процессоре сейчас, профилем запуска с данным названием.
        public void Save(string name)
        {
            Name = name;
            Ratios = (int[])limits.Current.Clone();
            Saved = Applied = DateTime.Now;
            Boots = 0;
            Hours = 0;
            Failure = Note = AtStartup = null;
            State = Risky ? Trial : Trusted;
            Write();
        }

        public void Forget()
        {
            Ratios = null;
            Note = null;
            if (path != null && File.Exists(path)) File.Delete(path);
        }

        void Write()
        {
            if (path == null) return;
            Json.WriteFile(path, new Dictionary<string, object>
            {
                { "cpu", cpu }, { "name", Name }, { "lps", lps }, { "ratios", Ratios }, { "state", State }, { "saved", Saved.ToString("o") },
                { "applied", Applied.ToString("o") }, { "boots", Boots }, { "hours", Hours }, { "failure", Failure },
            });
        }

        /// Читает профиль из файла и ничего не ставит: резидент так узнаёт, что изменило окно (LimitsStartup).
        public void Read()
        {
            if (path == null || !File.Exists(path)) return;
            Dictionary<string, object> saved = Json.ReadFile(path);
            try
            {
                if ((saved["cpu"] as string) != cpu || !Json.Ints(saved["lps"]).SequenceEqual(lps)) return;  // профиль от другого процессора
                Ratios = Json.Ints(saved["ratios"]);
                Name = saved.ContainsKey("name") ? saved["name"] as string ?? "" : "Профиль";
                State = saved["state"] as string;
                Saved = DateTime.Parse((string)saved["saved"], CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind);
                Applied = DateTime.Parse((string)saved["applied"], CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind);
                Boots = Convert.ToInt32(saved["boots"]);
                Hours = Json.Number(saved["hours"]);
                Failure = saved["failure"] as string;
            }
            catch (Exception e)
            {
                Ratios = null;  // прочитанный наполовину профиль профилем запуска не становится
                throw Json.Spoiled(path, e);
            }
        }

        /// Запуск программы: читает профиль и, если потолки с прошлого раза сбросились (перезагрузка,
        /// сон, другая программа), применяет его заново — кроме случая, когда прошлое применение
        /// кончилось сбоем.
        public void Startup()
        {
            Read();
            if (!Exists || State == Failed || InEffect) return;
            DateTime? cleanShutdown = null, stretchEnd = null;
            try
            {
                DateTime failedAt;
                string failure = FirstEvent(Applied, FailureEvents, out failedAt);
                DateTime stoppedAt;
                if (FirstEvent(Applied, "(Provider[@Name='EventLog'] and EventID=6006)", out stoppedAt) != null) cleanShutdown = stoppedAt;
                DateTime endedAt;
                if (FirstEvent(Applied, StretchEnd, out endedAt) != null) stretchEnd = endedAt;
                // Сбой относится к профилю, только если случился в том же сеансе Windows, где профиль
                // применили: до первого штатного завершения работы.
                if (failure != null && !(cleanShutdown.HasValue && cleanShutdown.Value < failedAt))
                {
                    if (Risky)
                    {
                        State = Failed;
                        Failure = failure + " " + failedAt.ToString("dd.MM HH:mm") + " — после применения профиля " + Applied.ToString("dd.MM HH:mm");
                        Write();
                        return;
                    }
                    Note = "после прошлого применения был сбой (" + failure + " " + failedAt.ToString("dd.MM HH:mm") +
                        "); потолки в профиле не выше заводских, поэтому он применён";
                }
            }
            catch (Exception e)
            {
                if (Risky)
                {
                    Note = "профиль не применён: не удалось прочитать журнал Windows и убедиться, что сбоя не было (" + e.Message + ")";
                    return;
                }
            }
            if (State == Trial)
            {
                Boots++;
                if (stretchEnd.HasValue) Hours += Math.Max(0, (stretchEnd.Value - Applied).TotalHours);
                if (Boots >= TrialBoots && Hours >= TrialHours) State = Trusted;
            }
            // Отметка времени пишется до записи в процессор: если машина зависнет сразу после неё, следующий запуск
            // отнесёт сбой к этому применению, а не к прошлому.
            Applied = DateTime.Now;
            Write();
            string refusal = limits.Apply(Ratios);
            if (refusal != null) Note = "профиль применён не полностью: " + refusal;
            else AtStartup = DateTime.Now.ToString("HH:mm");
            Write();
        }

        // Конец отрезка, который идёт в часы пробного профиля: штатное завершение работы или уход в S4 — быстрое
        // выключение либо гибернация (Kernel-Power 42). При быстром запуске событие 6006 не пишется (измерено: 10 из 23
        // загрузок за 20 дней), и часы набирались только после полного выключения. Сбой же относится к профилю, как и
        // прежде, до первого 6006: событие 42 пишет и гибернация, а сбой после пробуждения из неё идёт в счёт профиля.
        // То же правило — в программе входа на Rust (rust\machine\src\profile.rs), DECISIONS #31.
        const string StretchEnd = "(Provider[@Name='EventLog'] and EventID=6006) or (Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=42)";

        const string FailureEvents =
            "(Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=41) or " +
            "(Provider[@Name='EventLog'] and EventID=6008) or " +
            "(Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting'] and EventID=1001) or " +
            // Из аппаратных ошибок в счёт идут неисправимые (уровни 1 и 2) и исправленная ошибка самого процессора
            // (событие 19 — первый признак неустойчивого разгона). Остальные исправленные ошибки — предупреждения
            // о других устройствах: на машине владельца событие 17 (исправленная ошибка шины PCI Express) пишется
            // пачками при каждой загрузке, и по прежнему условию «любое предупреждение» оно считалось сбоем профиля.
            "(Provider[@Name='Microsoft-Windows-WHEA-Logger'] and (Level=1 or Level=2 or EventID=19))";

        /// Первое после указанного времени событие системного журнала Windows, подходящее под условие.
        /// Возвращает его название по-русски или null, если таких нет.
        static string FirstEvent(DateTime since, string condition, out DateTime at)
        {
            string query = "*[System[TimeCreated[@SystemTime>='" + since.ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.fffZ") +
                "'] and (" + condition + ")]]";
            using (var reader = new EventLogReader(new EventLogQuery("System", PathType.LogName, query)))
            using (EventRecord record = reader.ReadEvent())
            {
                at = record != null && record.TimeCreated.HasValue ? record.TimeCreated.Value : DateTime.MinValue;
                if (record == null) return null;
                switch (record.ProviderName)
                {
                    case "Microsoft-Windows-Kernel-Power": return "аварийное выключение или зависание";
                    case "EventLog": return record.Id == 6008 ? "неожиданное выключение" : "завершение работы";
                    case "Microsoft-Windows-WER-SystemErrorReporting": return "синий экран";
                    default: return record.Id == 19 ? "исправленная ошибка процессора" : "неисправимая аппаратная ошибка";
                }
            }
        }

        /// Что ставится при запуске программы — коротко, для строки состояния окна.
        public string Describe()
        {
            string note = Note == null ? "" : " · " + Note;  // редкое и важное: профиль не прочитан, не применён, применён после сбоя
            if (!Exists) return "при запуске ничего не ставится" + note;
            return "при запуске ставится «" + Name + "»" +
                (State == Failed ? " — отключён после сбоя" : State == Trial ? " (пробный)" : "") + note;
        }

        /// Подробности о профиле запуска для подсказки под курсором; null — сказать нечего.
        public string Details()
        {
            if (!Exists) return null;
            string text = "Профиль запуска «" + Name + "», сохранён " + Saved.ToString("dd.MM HH:mm");
            if (State == Failed) text += "\n• отключён и при запуске программы не ставится: " + Failure;
            else if (State == Trial)
                text += "\n• пробный, потому что в нём есть потолки выше заводских: пережил перезапусков " + Boots + " из " + TrialBoots +
                    ", часов " + Hours.ToString("0.0") + " из " + TrialHours;
            if (AtStartup != null) text += "\n• поставлен при запуске программы в " + AtStartup;
            return text;
        }
    }

    /// Сохранённые профили по названиям: у каждого — потолок на каждое ядро.
    class ProfileSets
    {
        readonly int[] lps;
        readonly string cpu, path;  // path = null — имитация: профили живут только в памяти
        readonly SortedDictionary<string, int[]> sets = new SortedDictionary<string, int[]>(StringComparer.CurrentCultureIgnoreCase);

        public ProfileSets(int[] lps, string cpu, string saveDir)
        {
            this.lps = lps;
            this.cpu = cpu;
            path = saveDir == null ? null : Path.Combine(saveDir, "profiles.json");
        }

        public string[] Names { get { return sets.Keys.ToArray(); } }

        public int[] Get(string name)
        {
            int[] ratios;
            return name != null && sets.TryGetValue(name.Trim(), out ratios) ? ratios : null;
        }

        public void Put(string name, int[] ratios)
        {
            sets[name.Trim()] = (int[])ratios.Clone();
            Write();
        }

        public void Delete(string name)
        {
            if (sets.Remove(name.Trim())) Write();
        }

        void Write()
        {
            if (path == null) return;
            Json.WriteFile(path, new Dictionary<string, object>
            {
                { "cpu", cpu }, { "lps", lps },
                { "sets", sets.Select(p => (object)new Dictionary<string, object> { { "name", p.Key }, { "ratios", p.Value } }).ToList() },
            });
        }

        public void Read()
        {
            if (path == null || !File.Exists(path)) return;
            Dictionary<string, object> saved = Json.ReadFile(path);
            var read = new Dictionary<string, int[]>();
            try
            {
                if ((saved["cpu"] as string) != cpu || !Json.Ints(saved["lps"]).SequenceEqual(lps)) return;  // профили от другого процессора
                foreach (Dictionary<string, object> set in (System.Collections.IEnumerable)saved["sets"])
                    read[(string)set["name"]] = Json.Ints(set["ratios"]);
            }
            catch (Exception e) { throw Json.Spoiled(path, e); }
            // Профили попадают в список только все разом: половина файла, записанная потом поверх целого, хуже пустого.
            foreach (KeyValuePair<string, int[]> set in read) sets[set.Key] = set.Value;
        }
    }
}
