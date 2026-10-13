// Запуск: метка оборванного измерения → профили. Один путь для окна (Core) и резидента (Tray.cs).
//
// Порядок важен. Сначала возврат потолков по метке (LimitHold.Recover): прошлый запуск мог оборваться посреди
// измерения и оставить блоку потолок ступени. Потом профиль: он сверяет свои потолки с действующими и решает,
// применять ли себя (Profile.Startup, с проверкой по журналу Windows, что прошлое применение не кончилось сбоем).
using System;

namespace CoreMap
{
    class LimitsStartup
    {
        public readonly LimitHold Hold;
        public readonly Profile Profile;       // профиль, который ставится при запуске
        public readonly ProfileSets Profiles;  // все сохранённые профили по названиям
        public readonly string Recovered;      // что возвращено по метке, строкой для владельца; null — ничего

        /// saveDir = null — имитация: метки нет, профили живут в памяти.
        /// apply = false — файлы профилей только читаются, потолки профиля не ставятся: так резидент узнаёт,
        /// что изменило окно, пока было открыто. Метка возвращается и тогда: окно могли снять посреди измерения.
        public LimitsStartup(CoreLimits limits, BlockMap blocks, int[] lps, string cpu, string saveDir, bool apply = true)
        {
            Hold = new LimitHold(limits, blocks, lps, cpu, saveDir);
            Recovered = Hold.Recover();
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
    }
}
