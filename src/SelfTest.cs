// Самопроверка на имитации, без окна. Главное в ней — основа измерений: на каждом пути выхода из измерения
// потолки множителей должны остаться теми, что были до него, а файл-метка — исчезнуть. Кроме того: ядро, о
// потолке которого процессор не отвечает; нечитаемый файл данных и отказ записи; ошибки в потоке датчиков.
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
            Check(new LimitHold(limits, blocks, lps, cpu, dir).Recover() == null, "метка: без метки ничего не делается");

            // 8. То же, но до измерения потолок был выше заводского: сам он не возвращается.
            int over = limits.Fused[first.First] + 1, step = limits.Fused[first.First] - 7;
            limits.Set(first.First, over);
            new LimitHold(limits, blocks, lps, cpu, dir).Take();
            limits.Set(first.First, step);
            notice = new LimitHold(limits, blocks, lps, cpu, dir).Recover();
            Check(limits.Of(first) == step && !File.Exists(mark), "метка: выше заводского не поднимает, метка убрана");
            Check(notice != null && notice.Contains("выше заводского"), "метка: о невозвращённом сказано (" + notice + ")");

            // 9. Процессор не отвечает о потолке ядра: чтение не выдаёт себя за удачное, запись не считается
            // принятой, а исходный потолок в конце измерения всё равно записан.
            limits.Set(first.First, before[first.First]);
            limits.Deaf = i => i == first.First;
            string unread = limits.Refresh();
            Check(unread != null && unread.Contains(first.Lp.ToString()), "нет ответа: чтение потолков сообщает о неудаче (" + unread + ")");
            ladder.Start(new[] { first }, 1, double.NaN);
            Check(Until(() => !runner.Busy, 8), "нет ответа: измерение закончило");
            Check(ladder.Error != null && ladder.Error.Contains("не ответил"), "нет ответа: запись не выдана за принятую (" + ladder.Error + ")");
            limits.Deaf = null;
            Check(limits.Refresh() == null && back() && !File.Exists(mark), "нет ответа: исходный потолок в процессор записан, метки нет");

            // 10. Набор потолков (профиль) при ядре без ответа: потолок пишется и тогда, когда по памяти
            // программы он уже такой, — в процессоре мог остаться другой.
            limits.Deaf = i => i == first.First;
            limits.Set(first.First, step);  // в «процессоре» теперь step, а программа помнит прежний
            Check(limits.Of(first) == before[first.First], "набор: программа помнит прежний потолок");
            string refusal = limits.Apply(before);
            Check(refusal != null && refusal.Contains("не ответил"), "набор: неподтверждённая запись названа (" + refusal + ")");
            limits.Deaf = null;
            Check(limits.Refresh() == null && back(), "набор: потолок в процессор записан");

            // 12. Нечитаемый файл данных не пропадает: при чтении он сохраняется рядом как .bad, и запись
            // новых данных поверх него эту копию не трогает. Половина файла в хранилище не попадает.
            string curvesFile = Path.Combine(dir, "curves.json");
            var store = new CurveStore(limits, cpu, dir);
            var point = new CurvePoint { Limit = 50, Ratio = 50, Temp = 60, Peak = 62, Base = 40, Watts = 10 };
            store.Record(first, 1, point);
            store.Record(first, 1, new CurvePoint { Limit = 51, Ratio = 51, Temp = 64, Peak = 66, Base = 40, Watts = 12 });
            store.Flush();
            Check(File.Exists(curvesFile) && Program.Told() == null, "файлы: кривые записаны, сообщений владельцу нет");
            string whole = File.ReadAllText(curvesFile);
            File.WriteAllText(curvesFile, whole.Substring(0, whole.Length / 2));  // обрыв посреди файла
            string spoiled = null;
            var reread = new CurveStore(limits, cpu, dir);
            try { reread.LoadSaved(); }
            catch (Exception e) { spoiled = e.Message; }
            Check(spoiled != null && spoiled.Contains("curves.json.bad"), "файлы: нечитаемый файл назван, копия обещана (" + spoiled + ")");
            Check(File.Exists(curvesFile + ".bad") && File.ReadAllText(curvesFile + ".bad").Length == whole.Length / 2, "файлы: копия .bad лежит рядом");
            reread.Record(second, 1, point);
            reread.Flush();
            Check(File.ReadAllText(curvesFile + ".bad").Length == whole.Length / 2, "файлы: новая запись копию .bad не тронула");
            // Файл разбирается, но поля нет: это тоже нечитаемый файл.
            File.WriteAllText(curvesFile, "{\"cpu\": " + Json.Quote(cpu) + "}");
            spoiled = null;
            reread = new CurveStore(limits, cpu, dir);
            try { reread.LoadSaved(); }
            catch (Exception e) { spoiled = e.Message; }
            Check(spoiled != null && spoiled.Contains("curves.json.bad") && reread.All(1).Length == 0, "файлы: файл без нужного поля — тоже (" + spoiled + ")");
            // Запись не удалась: владельцу сказано.
            File.Delete(curvesFile);
            Directory.CreateDirectory(curvesFile);  // на месте файла — папка: записать нельзя
            reread.Record(second, 1, point);
            reread.Flush();
            string told = Program.Told();
            Check(told != null && told.StartsWith("Кривые не сохранены"), "файлы: отказ записи доходит до владельца (" + told + ")");
            Directory.Delete(curvesFile, true);

            // 11. Поток датчиков переживает любую ошибку — опроса и обработки показаний (в ней защита от
            // перегрева): ошибка видна, защите передано «показаний нет», опрос идёт дальше.
            var flaky = new Flaky(lps.Length);
            int seen = 0, blind = 0;
            bool breakHandler = false;
            var watch = new Monitor(flaky, lps.Length, 0.05)
            {
                OnSample = temps =>
                {
                    if (temps == null) blind++; else seen++;
                    if (breakHandler) throw new InvalidOperationException("нарочно, в обработке");
                },
            };
            watch.Start();
            Check(Until(() => seen > 0 && watch.Error == null, 3), "датчики: опрос идёт");
            flaky.Broken = true;
            Check(Until(() => blind > 0 && watch.Error != null, 3), "датчики: ошибка опроса видна, защите передано «показаний нет» (" + watch.Error + ")");
            flaky.Broken = false;
            Check(Until(() => watch.Error == null, 3), "датчики: после ошибки опрос продолжился");
            breakHandler = true;
            Check(Until(() => watch.Error != null, 3), "датчики: ошибка обработки показаний видна (" + watch.Error + ")");
            breakHandler = false;
            int was = seen;
            Check(Until(() => seen > was && watch.Error == null, 3), "датчики: поток опроса жив");

            TrayScenarios(Path.Combine(dir, "tray"), cores, blocks, lps, cpu);
        }

        /// 13. Резидент (Tray.cs) без значка. «Процессор» у него свой, в памяти, и при каждом создании — с
        /// заводскими потолками имитации, как после перезагрузки; файлы профилей — в папке dir.
        static void TrayScenarios(string dir, CpuCore[] cores, BlockMap blocks, int[] lps, string cpu)
        {
            Directory.CreateDirectory(dir);
            Block first = blocks.All[0], second = blocks.All[1];
            // Профили собираются на отдельном «процессоре» — как их сохранило бы окно программы до перезагрузки.
            var desk = new CoreLimits(null, cores, blocks);
            var sets = new ProfileSets(lps, cpu, dir);
            desk.Set(first.First, desk.Fused[first.First] - 4);
            sets.Put("Тихий", desk.Current);
            desk.Set(second.First, desk.Fused[second.First] - 5);
            sets.Put("Игровой", desk.Current);
            int[] bad = (int[])desk.Current.Clone();
            bad[first.First] = 999;
            sets.Put("Негодный", bad);
            new Profile(desk, lps, cpu, dir).Save("Игровой");

            // Запуск: профиль «при запуске» поставлен тем же путём, что в окне.
            var resident = new Resident(true, dir);
            string news = resident.Load(true);
            Check(resident.Available && resident.Standing() == "Игровой" && resident.Limits.Same(desk.Current),
                "резидент: при запуске поставлен профиль «при запуске» (" + resident.Tip().Replace("\n", " / ") + ")");
            Check(news == null, "резидент: штатный запуск — без уведомления (" + news + ")");
            List<TrayItem> menu = resident.Menu();
            Check(menu.Count(i => i.Checked) == 1 && menu.Single(i => i.Checked).Text == "Игровой" &&
                menu.Any(i => i.Id == Resident.OpenId && i.Default) && menu.Any(i => i.Id == Resident.ExitId),
                "резидент: в меню отмечен стоящий профиль, есть «Открыть» и «Выход»");

            // Щелчок по другому профилю: его потолки стоят, и при следующем запуске ставится он.
            int quiet = menu.First(i => i.Text != null && i.Text.StartsWith("Тихий")).Id;
            Check(resident.Switch(resident.NameOf(quiet)) == null && resident.Standing() == "Тихий" && resident.AtStartup == "Тихий",
                "резидент: щелчок по профилю ставит его и делает профилем «при запуске»");
            var next = new Resident(true, dir);
            next.Load(true);
            Check(next.Standing() == "Тихий", "резидент: выбор записан в файл — следующий запуск ставит его");

            // Отказ записи потолка: не проглочен, профиль «при запуске» прежний.
            string refused = resident.Switch("Негодный");
            Check(refused != null && refused.Contains("999") && resident.AtStartup == "Тихий" && resident.Tip().Contains("не поставлен"),
                "резидент: отказ записи потолка назван, профиль «при запуске» прежний (" + refused + ")");

            // Открыто окно программы: профили в меню серые, щелчок ничего не ставит.
            resident.WindowOpen = true;
            int[] standing = (int[])resident.Limits.Current.Clone();
            Check(resident.Switch("Игровой") != null && resident.Limits.Current.SequenceEqual(standing) &&
                resident.Menu().Where(i => i.Id >= Resident.FactoryId).All(i => !i.Enabled && !i.Checked),
                "резидент: при открытом окне профили серые и не переключаются");
            resident.WindowOpen = false;

            // «Заводские»: заводские потолки, при запуске ничего не ставится.
            Check(resident.Switch("") == null && resident.Limits.Same(resident.Limits.Fused) && resident.Standing() == "" &&
                resident.AtStartup == "" && !File.Exists(Path.Combine(dir, "profile.json")),
                "резидент: «заводские» ставят заводские потолки и снимают профиль «при запуске»");

            // Окно программы закрылось, изменив файлы: они перечитаны, потолки не тронуты.
            sets.Delete("Тихий");
            new Profile(desk, lps, cpu, dir).Save("Игровой");
            news = resident.Load(false);
            Check(news == null && resident.AtStartup == "Игровой" && resident.Standing() == "" &&
                !resident.Menu().Any(i => i.Text != null && i.Text.StartsWith("Тихий")) && resident.Tip().Contains("при запуске — «Игровой»"),
                "резидент: после окна файлы профилей перечитаны, потолки не тронуты (" + resident.Tip().Replace("\n", " / ") + ")");

            // Окно сняли посреди измерения: метку возвращает и резидент.
            new LimitHold(desk, blocks, lps, cpu, dir).Take();
            news = resident.Load(false);
            Check(news != null && news.StartsWith("Прошлый запуск оборвался") && resident.Limits.Same(desk.Current) &&
                !File.Exists(Path.Combine(dir, "limits-held.json")), "резидент: потолки возвращены по метке, владельцу сказано (" + news + ")");
        }

        /// Датчики-подделка: по просьбе опрос кончается ошибкой, которой настоящие датчики не дают.
        class Flaky : ISensorBackend
        {
            readonly int count;
            public volatile bool Broken;

            public Flaky(int count) { this.count = count; }

            public int ThrottleAt { get { return 100; } }

            public Sample Read()
            {
                if (Broken) throw new InvalidOperationException("нарочно, в опросе");
                return new Sample
                {
                    Temps = Enumerable.Repeat(45.0, count).ToArray(), Throttled = new bool[count],
                    Ratio = Enumerable.Repeat(31.0, count).ToArray(), Pkg = 45,
                };
            }
        }
    }
}
