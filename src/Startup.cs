// Запуск: метка оборванного измерения → профили. Один путь для окна (Core) и резидента (Tray.cs).
//
// Порядок важен. Сначала возврат потолков по метке (LimitHold.Recover): прошлый запуск мог оборваться посреди
// измерения и оставить блоку потолок ступени. Потом профиль: он сверяет свои потолки с действующими и решает,
// применять ли себя (Profile.Startup, с проверкой по журналу Windows, что прошлое применение не кончилось сбоем).
//
// Аренда потолков (DECISIONS #31). Рядом может работать программа входа на Rust (CoreMapTray.exe): она пишет
// потолки, только пока не видит прежнего окна и движка, и каждое своё действие делает под общей меткой
// «CoreMap.Ceilings». Окно и движок к этому моменту уже подняли свои метки, но программа входа могла начать действие
// чуть раньше: старт ждёт его конца под той же меткой и перечитывает потолки — иначе «было» и проверка профиля
// взяли бы наполовину переставленные потолки, прочитанные ещё в конструкторе CoreLimits.
using System;
using System.Linq;
using System.Threading;

namespace CoreMap
{
    class LimitsStartup
    {
        public readonly LimitHold Hold;
        public readonly Profile Profile;       // профиль, который ставится при запуске
        public readonly ProfileSets Profiles;  // все сохранённые профили по названиям
        public readonly string Recovered;      // что возвращено по метке, строкой для владельца; null — ничего
        public readonly int[] Was;             // потолки в процессоре после возврата по метке и до профиля: «было» для окна

        /// saveDir = null — имитация: метки нет, профили живут в памяти.
        /// apply = false — файлы профилей только читаются, потолки профиля не ставятся: так резидент узнаёт,
        /// что изменило окно, пока было открыто. Метка возвращается и тогда: окно могли снять посреди измерения.
        public LimitsStartup(CoreLimits limits, BlockMap blocks, int[] lps, string cpu, string saveDir, bool apply = true)
        {
            // Имитация (скрытые проверки) аренду не берёт: её «процессор» в памяти, программы входа рядом нет.
            bool held = limits.Real && TakeLease(LeaseName, 10000);
            if (limits.Real && !held) Program.Log("аренда потолков не получена за 10 с — программа входа держит её; старт идёт дальше");
            try
            {
                if (limits.Real) limits.Refresh();
                Hold = new LimitHold(limits, blocks, lps, cpu, saveDir);
                Recovered = Hold.Recover();
                Was = (int[])limits.Current.Clone();
                Profile = new Profile(limits, lps, cpu, saveDir);
                Profiles = new ProfileSets(lps, cpu, saveDir);
                try
                {
                    Profiles.Read();
                    if (apply) Profile.Startup();
                    else Profile.Read();
                    // Профиль, сохранённый до появления названий, попадает в общий список.
                    if (Profile.Exists && Profiles.Get(Profile.Name) == null) Profiles.Put(Profile.Name, Profile.Ratios);
                }
                catch (Exception e)
                {
                    Program.Log("профиль потолков не прочитан: " + e);
                    Profile.Note = "профиль не прочитан: " + e.Message;
                }
            }
            finally { if (held) Lease.ReleaseMutex(); }
        }

        public const string LeaseName = "Local\\CoreMap.Ceilings";
        static Mutex lease;  // создаётся при первой аренде; поток, взявший её, её и отпускает (в этом же конструкторе)
        static Mutex Lease { get { return lease ?? (lease = new Mutex(false, LeaseName)); } }

        /// Ждёт аренду до ms миллисекунд. true — взята (брошенная тоже: программа входа кончилась, не отпустив её);
        /// false — не дождались: старт идёт дальше (записывает в журнал тот, кто звал). Имя — параметр ради самопроверки.
        public static bool TakeLease(string name, int ms)
        {
            Mutex mutex = name == LeaseName ? Lease : new Mutex(false, name);
            bool held;
            try { held = mutex.WaitOne(ms); }
            catch (AbandonedMutexException) { held = true; }
            if (held && mutex != Lease) mutex.ReleaseMutex();  // проверочная метка: только узнать, дождались ли
            if (mutex != Lease) mutex.Dispose();
            return held;
        }

        /// Каким сохранённым набором является данный: название профиля, "" — заводские потолки, null — ни одним
        /// (потолки ставились вручную, ползунком или измерением). Ядра, о которых процессор не ответил, не в счёт.
        /// Если подходит несколько профилей, первым идёт профиль запуска, затем preferred, затем по алфавиту.
        /// Правило одно для окна (Core.Named) и резидента (Resident.Standing).
        public static string Named(int[] ratios, CoreLimits limits, Profile profile, ProfileSets profiles, string preferred = null)
        {
            if (ratios == null) return null;
            Func<int[], bool> same = other => Enumerable.Range(0, ratios.Length).All(i => !limits.Known(i) || ratios[i] == other[i]);
            if (!profile.Exists && same(limits.Fused)) return "";
            if (profile.Exists && profiles.Get(profile.Name) != null && same(profile.Ratios)) return profile.Name;
            if (preferred != null && profiles.Get(preferred) != null && same(profiles.Get(preferred))) return preferred.Trim();
            foreach (string name in profiles.Names)
                if (same(profiles.Get(name))) return name;
            return same(limits.Fused) ? "" : null;
        }
    }
}
