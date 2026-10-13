// Проверка основы измерений на имитации, без окна: на каждом пути выхода из измерения потолки множителей
// должны остаться теми, что были до него, а файл-метка — исчезнуть.
//
// Запускает её tools\check.ps1: CoreMap.exe --simulate --snapshot <отчёт.txt> --selftest. Процессор не
// трогается и не нагружается; файлы пишутся во временную папку, а не в data\. Первая строка отчёта — «ok»
// или «FAILED», дальше по строке на каждое условие.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;

namespace CoreMap
{
    static class SelfTest
    {
        /// Измерение-подделка на общей основе: двигает потолок блока и обрывается нарочно — ошибкой в потоке
        /// либо записью потолка, которую процессор не примет.
        class Faulty : StepTest
        {
            public Faulty(Runner runner, Monitor monitor, CoreLimits limits, CurveStore curves)
                : base(runner, "проверка прервана", monitor, limits, curves, true) { }

            public void Start(Block block, bool refuse)
            {
                Launch(() =>
                {
                    Begin(new[] { block }, 1, 0);
                    int[] ratios = limits.Factory(block.First).Reverse().ToArray();
                    Under(block, ratios[0], "", (idle, idleWatts) =>
                    {
                        if (refuse) return Put(block, 999);
                        Step(block, ratios[1], idle, idleWatts);
                        throw new InvalidOperationException("нарочно");
                    });
                });
            }
        }

        static readonly List<string> lines = new List<string>();
        static int failed;

        static void Check(bool ok, string what)
        {
            if (!ok) failed++;
            lines.Add((ok ? "ok      " : "FAILED  ") + what);
        }

        /// Ждёт условия не дольше seconds; false — не дождался.
        static bool Until(Func<bool> condition, double seconds)
        {
            for (double end = Native.Now() + seconds; Native.Now() < end; Thread.Sleep(50))
                if (condition()) return true;
            return condition();
        }

        public static int Run(string reportPath)
        {
            string dir = Path.Combine(Path.GetTempPath(), "coremap-selftest-" + System.Diagnostics.Process.GetCurrentProcess().Id);
            Program.DataDir = dir;  // и журнал ошибок — туда же: проверка нарочно вызывает ошибку в потоке измерения
            try { Scenarios(dir); }
            catch (Exception e) { Check(false, "проверка оборвалась: " + e); }
            try { if (Directory.Exists(dir)) Directory.Delete(dir, true); }
            catch (Exception) { }
            lines.Insert(0, failed == 0 ? "ok" : "FAILED: " + failed);
            File.WriteAllLines(reportPath, lines);
            return failed == 0 ? 0 : 1;
        }

        static void Scenarios(string dir)
        {
            CpuCore[] cores = Topology.Read().ToArray();
            int[] lps = cores.Select(c => c.Lp).ToArray();
            var blocks = new BlockMap(cores);
            string cpu, identifier;
            Topology.Identity(out cpu, out identifier);
            double[][] positions = DieLayout.Pick(cores, blocks, identifier, Path.Combine(Program.Root, "layouts")).Positions(lps.Length);
            var load = new LoadManager(lps, true);
            var limits = new CoreLimits(null, cores, blocks);
            var monitor = new Monitor(new SimBackend(positions, load.Active), lps.Length, 0.5);
            monitor.Start();
            string mark = Path.Combine(dir, "limits-held.json");
            var runner = new Runner(load, new LimitHold(limits, blocks, lps, cpu, dir));
            var curves = new CurveStore(limits, cpu, null);
            var ladder = new CurveTest(runner, monitor, limits, curves, true);
            var search = new LimitSearch(runner, monitor, limits, curves, true);
            var faulty = new Faulty(runner, monitor, limits, curves);
            Block first = blocks.All[0], second = blocks.All[1];

            // Исходные потолки — не заводские: возврат «к заводским» вместо «к прежним» был бы виден.
            limits.Set(first.First, limits.Fused[first.First] - 2);
            limits.Set(second.First, limits.Fused[second.First] - 3);
            int[] before = (int[])limits.Current.Clone();
            Func<bool> back = () => limits.Current.SequenceEqual(before);
            Func<bool> moved = () => limits.Of(first) != before[first.First];

            // 1. Обычный конец лесенки (одна ступень вверх и вниз).
            ladder.Start(new[] { first }, 1, 0);
            Check(ladder.Running && runner.Busy, "лесенка: запущена");
            search.Start(new[] { second }, 1, 80);
            Check(!search.Running, "исполнитель: второе измерение при идущем не запускается");
            Check(Until(moved, 3), "лесенка: потолок блока сдвинут ступенью");
            Check(File.Exists(mark), "лесенка: метка лежит, пока потолок временный");
            Check(Until(() => !runner.Busy, 15), "лесенка: дошла до конца");
            Check(back() && !File.Exists(mark) && ladder.Error == null, "обычный конец: потолки прежние, метки нет, ошибки нет");

            // 2. Кнопка «стоп» и защита от перегрева: обе просят остановиться через исполнителя.
            ladder.Start(new[] { first }, 1, double.NaN);
            Check(Until(moved, 3), "стоп: потолок блока сдвинут");
            runner.Stop();
            Check(Until(() => !runner.Busy, 3), "стоп: измерение закончило за 3 с");
            Check(back() && !File.Exists(mark), "стоп: потолки прежние, метки нет");
            Check(ladder.Error == "снятие кривых остановлено", "стоп: причина названа (" + ladder.Error + ")");

            // 3. Закрытие окна посреди измерения.
            ladder.Start(new[] { first }, 1, double.NaN);
            Check(Until(moved, 3), "закрытие: потолок блока сдвинут");
            Check(runner.Close(3), "закрытие: поток измерения закончил за 3 с");
            Check(back() && !File.Exists(mark), "закрытие: потолки прежние, метки нет");

            // 4. Неожиданная ошибка в потоке измерения.
            faulty.Start(first, false);
            Check(Until(moved, 3), "ошибка: потолок блока сдвинут");
            Check(Until(() => !runner.Busy, 8), "ошибка: измерение закончило");
            Check(back() && !File.Exists(mark), "ошибка: потолки прежние, метки нет");
            Check(faulty.Error == "проверка прервана: нарочно", "ошибка: показана (" + faulty.Error + ")");
            Check(load.Active().All(x => !x), "ошибка: нагрузка снята");

            // 5. Отказ записи потолка.
            faulty.Start(first, true);
            Check(Until(() => !runner.Busy, 8), "отказ записи: измерение закончило");
            Check(faulty.Error != null && faulty.Error.Contains("999"), "отказ записи: не проглочен (" + faulty.Error + ")");
            Check(back() && !File.Exists(mark), "отказ записи: потолки прежние, метки нет");

            // 6. Поиск замером: законченному блоку найденный потолок остаётся, прерванному возвращается прежний.
            search.Start(new[] { first, second }, 1, 200);
            Check(Until(() => search.BlockDone == 1 && search.Active == second && limits.Of(second) != before[second.First], 40),
                "поиск: первый блок закончен, второй в работе");
            int found = limits.Of(first);
            Check(found != before[first.First], "поиск: первому блоку поставлен найденный потолок ×" + found);
            Check(File.Exists(mark) && Json.Ints(Json.ReadFile(mark)["limits"])[first.First] == found, "поиск: найденный потолок записан в метку");
            runner.Stop();
            Check(Until(() => !runner.Busy, 3), "поиск: остановлен за 3 с");
            Check(limits.Of(first) == found, "поиск: найденный потолок первого блока остался");
            Check(limits.Of(second) == before[second.First], "поиск: второму блоку возвращён прежний потолок");
            Check(!File.Exists(mark), "поиск: метки нет");
            Check(search.Report != null && search.Report.Contains("Блок " + second.Name + " не подобран"), "поиск: итог называет блок (" + search.Report + ")");

            // 7. Процесс убит посреди измерения: метка осталась, следующий запуск возвращает потолки по ней.
            limits.Set(first.First, before[first.First]);
            new LimitHold(limits, blocks, lps, cpu, dir).Take();
            limits.Set(first.First, limits.Fused[first.First] - 7);  // потолок ступени, с которым процесс «умер»
            string notice = new LimitHold(limits, blocks, lps, cpu, dir).Recover();
            Check(back() && !File.Exists(mark), "метка: потолки возвращены, метка убрана");
            Check(notice != null && notice.Contains(first.Name + " ×" + before[first.First]), "метка: владельцу сказано (" + notice + ")");
            Check(!limits.Changed, "метка: «как было при запуске» — возвращённые потолки");
            Check(new LimitHold(limits, blocks, lps, cpu, dir).Recover() == null, "метка: без метки ничего не делается");

            // 8. То же, но до измерения потолок был выше заводского: сам он не возвращается.
            int over = limits.Fused[first.First] + 1, step = limits.Fused[first.First] - 7;
            limits.Set(first.First, over);
            new LimitHold(limits, blocks, lps, cpu, dir).Take();
            limits.Set(first.First, step);
            notice = new LimitHold(limits, blocks, lps, cpu, dir).Recover();
            Check(limits.Of(first) == step && !File.Exists(mark), "метка: выше заводского не поднимает, метка убрана");
            Check(notice != null && notice.Contains("выше заводского"), "метка: о невозвращённом сказано (" + notice + ")");
        }
    }
}
