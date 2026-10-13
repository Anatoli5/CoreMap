// Профили потолков множителей.
//
// Профилей может быть несколько, у каждого своё название (ProfileSets, data\profiles.json). Один из них —
// действующий (Profile, data\profile.json): тот, что сохранён или применён последним; он и ставится заново
// при запуске программы.
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
        public string Note;         // что произошло с профилем при запуске программы

        public Profile(CoreLimits limits, int[] lps, string cpu, string saveDir)
        {
            this.limits = limits;
            this.lps = lps;
            this.cpu = cpu;
            path = saveDir == null ? null : Path.Combine(saveDir, "profile.json");
        }

        public bool Exists { get { return Ratios != null; } }

        /// Действующие потолки совпадают с профилем.
        public bool InEffect
        {
            get { return Exists && Enumerable.Range(0, lps.Length).All(i => !limits.Known(i) || limits.Current[i] == Ratios[i]); }
        }

        /// В профиле есть потолок выше заводского предела: такой профиль проходит испытательный срок.
        bool Risky { get { return Enumerable.Range(0, lps.Length).Any(i => limits.Known(i) && Ratios[i] > limits.Fused[i]); } }

        /// Делает действующие потолки действующим профилем с данным названием.
        public void Save(string name)
        {
            Name = name;
            Ratios = (int[])limits.Current.Clone();
            Saved = Applied = DateTime.Now;
            Boots = 0;
            Hours = 0;
            Failure = Note = null;
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

        void Read()
        {
            if (path == null || !File.Exists(path)) return;
            Dictionary<string, object> saved = Json.ReadFile(path);
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

        /// Запуск программы: читает профиль и, если потолки с прошлого раза сбросились (перезагрузка,
        /// сон, другая программа), применяет его заново — кроме случая, когда прошлое применение
        /// кончилось сбоем.
        public void Startup()
        {
            Read();
            if (!Exists || State == Failed || InEffect) return;
            DateTime? cleanShutdown = null;
            try
            {
                DateTime failedAt;
                string failure = FirstEvent(Applied, FailureEvents, out failedAt);
                DateTime stoppedAt;
                if (FirstEvent(Applied, "(Provider[@Name='EventLog'] and EventID=6006)", out stoppedAt) != null) cleanShutdown = stoppedAt;
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
                if (cleanShutdown.HasValue) Hours += Math.Max(0, (cleanShutdown.Value - Applied).TotalHours);
                if (Boots >= TrialBoots && Hours >= TrialHours) State = Trusted;
            }
            // Отметка времени пишется до записи в процессор: если машина зависнет сразу после неё, следующий запуск
            // отнесёт сбой к этому применению, а не к прошлому.
            Applied = DateTime.Now;
            Write();
            string refusal = Apply();
            if (refusal != null) Note = "профиль применён не полностью: " + refusal;
            else if (Note == null) Note = "применён при запуске " + DateTime.Now.ToString("HH:mm");
            Write();
        }

        /// Ставит потолки из профиля. null — все приняты.
        public string Apply()
        {
            string refusal = null;
            for (int i = 0; i < lps.Length; i++)
                if (limits.Known(i) && limits.Current[i] != Ratios[i])
                    refusal = limits.Set(i, Ratios[i]) ?? refusal;
            return refusal;
        }

        const string FailureEvents =
            "(Provider[@Name='Microsoft-Windows-Kernel-Power'] and EventID=41) or " +
            "(Provider[@Name='EventLog'] and EventID=6008) or " +
            "(Provider[@Name='Microsoft-Windows-WER-SystemErrorReporting'] and EventID=1001) or " +
            "(Provider[@Name='Microsoft-Windows-WHEA-Logger'] and (Level=1 or Level=2 or Level=3))";

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
                    default: return "аппаратная ошибка процессора";
                }
            }
        }

        /// Строка состояния для окна.
        public string Describe(bool changed)
        {
            string unsaved = changed ? " · потолки изменены и не сохранены: пропадут при перезагрузке" : "";
            if (!Exists) return changed ? "Действующего профиля нет" + unsaved : "Действующего профиля нет: потолки как у системы";
            string text = "Действует профиль «" + Name + "» от " + Saved.ToString("dd.MM HH:mm");
            if (State == Failed) return text + " отключён: " + Failure + unsaved;
            if (State == Trial)
                text += " · пробный: пережил перезапусков " + Boots + " из " + TrialBoots + ", часов " + Hours.ToString("0.0") + " из " + TrialHours;
            else text += " · проверенный";
            text += InEffect ? "" : " · сейчас потолки другие";
            if (Note != null) text += " · " + Note;
            return text + unsaved;
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
            if ((saved["cpu"] as string) != cpu || !Json.Ints(saved["lps"]).SequenceEqual(lps)) return;  // профили от другого процессора
            foreach (Dictionary<string, object> set in (System.Collections.IEnumerable)saved["sets"])
                sets[(string)set["name"]] = Json.Ints(set["ratios"]);
        }
    }
}
