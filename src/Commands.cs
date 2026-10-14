// Что окно может велеть движку по мосту: команды.
//
// Каждая команда — названное действие. Аргументы проверяются здесь по границам, которыми и так владеет движок
// (CoreLimits.Choices, LoadManager.Levels, ThermalSweep.Durations, CurveFit.Low…High), и ещё раз — в самом
// действии. Команд с путём к файлу, номером регистра процессора или программой для запуска нет и появиться не
// должно: канал открыт любой программе, работающей от имени владельца, и она получает ровно этот список.
//
// Поднять потолок выше заводского — единственное в списке, чем можно навредить процессору сразу. Такая команда
// исполняется только после подтверждения владельцем в окошке самого движка (Confirm): в окно с правами
// администратора другая программа щёлкнуть не может. Пока подтверждать некому (Confirm = null), она отклоняется.
//
// Нет здесь пока: fit.view (вместе с темой подбора под температуру) и autostart.set (когда движком станет
// резидент) — см. DECISIONS.md, п. 25.
using System;
using System.Collections.Generic;
using System.Linq;

namespace CoreMap
{
    class Commands
    {
        const int NameLength = 64;  // название профиля длиннее не принимается

        readonly Core core;

        /// Спрашивает владельца о действии, которое поднимает потолок выше заводского; true — согласен.
        public Func<string, bool> Confirm;

        public Commands(Core core) { this.core = core; }

        /// Остановка измерения не ждёт очереди команд: Runner.Stop только взводит событие.
        public bool Urgent(string command) { return command == "run.stop"; }

        /// Исполняет команду. null — сделано; иначе причина отказа. note — итог одной строкой, если он у действия есть.
        public string Execute(string command, Dictionary<string, object> f, out string note)
        {
            note = null;
            string bad = "негодные аргументы команды «" + command + "»";
            int value;
            bool flag;
            string name;
            Block block;
            switch (command)
            {
                case "load.toggle":
                    return Which(f, out block) ? core.ToggleLoad(block) : bad;
                case "load.stop":
                    return core.StopLoad();
                case "level.set":
                    if (!Int(f, "level", out value) || !LoadManager.Levels.Contains(value)) return bad;
                    return core.SetLevel(value);
                case "max.reset":
                    if (core.Monitor != null) core.Monitor.ResetMax();
                    core.ResetAverage();
                    return null;

                case "limit.set":
                    if (!Which(f, out block) || !Int(f, "ratio", out value)) return bad;
                    if (core.Limits == null) return "потолки множителей недоступны";
                    int[] wanted = (int[])core.Limits.Current.Clone();
                    foreach (int i in block.Members) wanted[i] = value;
                    // Значение вне допустимого отклонит сам CoreLimits.Set — владельца о нём спрашивать незачем.
                    if (core.Limits.Choices(block.First).Contains(value) && !Allowed(wanted, "Поставить " + Whom(block) + " потолок ×" + value))
                        return Unconfirmed;
                    return core.SetLimit(block.First, value);
                case "profile.save":
                    return Name(f, out name) ? core.SaveProfile(name) : bad;
                case "profile.open":
                    if (!Name(f, out name)) return bad;
                    if (core.Limits != null && core.Profiles != null)
                    {
                        int[] ratios = name == "" ? core.Limits.Fused : core.Profiles.Get(name);
                        if (ratios != null && !Allowed(ratios, "Открыть профиль «" + name + "»")) return Unconfirmed;
                    }
                    return core.OpenProfile(name);
                case "profile.accept":
                    return core.Accept(core.Preferred);
                case "profile.revert":
                    if (core.Limits != null && !Allowed(core.Was, "Вернуть потолки, которые были")) return Unconfirmed;
                    return core.Revert();
                case "profile.delete":
                    return Name(f, out name) ? core.DeleteProfile(name) : bad;

                case "heat.mode":
                    return Flag(f, "byPeak", out flag) ? core.SetByPeak(flag) : bad;
                case "fit.apply":
                    if (!Temp(f, "target", out value) || !Flag(f, "best", out flag)) return bad;
                    if (core.Fit == null) return "потолки множителей или датчики недоступны";
                    if (core.Busy) return "идёт измерение: потолки сейчас менять нельзя";
                    note = core.FitFromCurves(value, flag);
                    return null;

                case "run.start":
                    return Start(f);
                case "run.stop":
                    core.StopRun();
                    return null;

                case "fan.mode":
                    if (!Int(f, "mode", out value) || value < Fans.Auto || value > Fans.Always) return bad;
                    return core.SetFanMode(value);
                case "notices.seen":
                    if (!Int(f, "seq", out value)) return bad;
                    Program.NoticesSeen(value);
                    return null;
                default:
                    return "неизвестная команда «" + command + "»";
            }
        }

        /// Запуск измерения по его роду. null — пошло; иначе причина отказа.
        string Start(Dictionary<string, object> f)
        {
            const string bad = "негодные аргументы команды «run.start»";
            object kind;
            int seconds, passes, temp;
            Block block;
            f.TryGetValue("kind", out kind);
            switch (kind as string)
            {
                case "sweep":
                    if (!Int(f, "seconds", out seconds) || !ThermalSweep.Durations.Contains(seconds)) return bad;
                    if (!Int(f, "passes", out passes) || !ThermalSweep.PassChoices.Contains(passes)) return bad;
                    return core.StartSweep(seconds, passes);
                case "latency": return core.StartLatency();
                case "calibration": return core.StartCalibration();
                case "curve": return Which(f, out block) && Temp(f, "maxTemp", out temp) ? core.StartCurve(block, temp) : bad;
                case "curves": return Temp(f, "maxTemp", out temp) ? core.StartAllCurves(temp) : bad;
                case "search": return Temp(f, "maxTemp", out temp) ? core.FitLimits(temp) : bad;
                case "joint": return core.StartJoint();
                default: return "неизвестный род измерения";
            }
        }

        // ----- подтверждение -----

        const string Unconfirmed = "потолок выше заводского владельцем не подтверждён";

        static string Whom(Block block) { return block.Kind == 'P' ? "ядру " + block.Name : "блоку E-ядер " + block.Name; }

        /// Можно ли поставить такие потолки: либо ни один из них не поднимается выше заводского дальше, чем стоит
        /// сейчас, либо владелец согласился. what — о чём его спросить.
        bool Allowed(int[] ratios, string what)
        {
            if (core.Busy) return true;  // во время измерения действие и так будет отклонено: спрашивать не о чем
            CoreLimits limits = core.Limits;
            int over = 0;
            for (int i = 0; i < ratios.Length; i++)
                if (limits.Known(i) && ratios[i] > limits.Fused[i] && ratios[i] > limits.Current[i])
                    over = Math.Max(over, ratios[i] - limits.Fused[i]);
            if (over == 0) return true;
            return Confirm != null && Confirm(what + " — выше заводского на " + over + "?");
        }

        // ----- аргументы -----

        static bool Int(Dictionary<string, object> f, string key, out int value)
        {
            object raw;
            value = 0;
            if (!f.TryGetValue(key, out raw) || !(raw is int)) return false;
            value = (int)raw;
            return true;
        }

        static bool Flag(Dictionary<string, object> f, string key, out bool value)
        {
            object raw;
            value = false;
            if (!f.TryGetValue(key, out raw) || !(raw is bool)) return false;
            value = (bool)raw;
            return true;
        }

        /// Температура в границах, в которых работают подбор и лесенка.
        static bool Temp(Dictionary<string, object> f, string key, out int value)
        {
            return Int(f, key, out value) && value >= CurveFit.Low && value <= CurveFit.High;
        }

        /// Название профиля; "" — заводские потолки (годится только там, где так сказано в действии).
        static bool Name(Dictionary<string, object> f, out string value)
        {
            object raw;
            value = f.TryGetValue("name", out raw) ? raw as string : null;
            return value != null && value.Length <= NameLength;
        }

        /// Блок по его порядковому номеру.
        bool Which(Dictionary<string, object> f, out Block block)
        {
            int index;
            block = null;
            if (!Int(f, "block", out index) || index < 0 || index >= core.Blocks.All.Length) return false;
            block = core.Blocks.All[index];
            return true;
        }
    }
}
