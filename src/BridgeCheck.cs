// Проверки моста.
//
// 1. Скрытая, для агента (tools\check.ps1): CoreMap.exe --simulate --engine --snapshot <отчёт>. Движок на
//    имитации без окна открывает канал и запускает вторую копию программы (--bridge-client) — она проходит по
//    протоколу как окно: знакомство, состояние, каждая команда, отказы, измерение с остановкой, негодные
//    сообщения. Первая строка отчёта — «ok» или «FAILED». Обе копии без прав администратора: главное —
//    канал между процессом с правами и процессом без них — так проверить нельзя.
// 2. Кнопка владельца в окне программы (Owner): то самое главное. Окно с правами создаёт пробный канал с
//    одной пустой командой, просит оболочку Windows запустить копию программы без прав (--bridge-check), та
//    подключается и шлёт двадцать команд. Итог — строкой в окне, подробности — в data\bridge-check.txt.
//    Настоящий канал протокола на настоящем процессоре при этом не открывается (DECISIONS.md, п. 25).
//    Кнопка временная: уйдёт, когда появится окно без прав, которое и будет такой проверкой.
// Тот же путь кнопки проходит и скрытая проверка — без прав, чтобы в коде, который нажмёт владелец, не было
// ни одной строки, не исполненной до него.
using System;
using System.Collections;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Security.Principal;
using System.Text;
using System.Threading;

namespace CoreMap
{
    static class BridgeCheck
    {
        const int Pings = 20;

        static readonly List<string> lines = new List<string>();
        static int failed;

        static void Check(bool ok, string what)
        {
            if (!ok) failed++;
            lines.Add((ok ? "ok      " : "FAILED  ") + what);
        }

        // ----- скрытая проверка: движок -----

        public static int Host(string reportPath)
        {
            string dir = Path.Combine(Path.GetTempPath(), "coremap-bridge-" + Process.GetCurrentProcess().Id);
            Directory.CreateDirectory(dir);
            Program.DataDir = dir;  // журнал и отчёт кнопки — сюда, а не в data\ владельца
            try { Hosted(reportPath); }
            catch (Exception e) { Check(false, "проверка оборвалась: " + e); }
            try { Directory.Delete(dir, true); }
            catch (Exception) { }
            lines.Insert(0, failed == 0 ? "ok" : "FAILED: " + failed);
            File.WriteAllLines(reportPath, lines);
            return failed == 0 ? 0 : 1;
        }

        static void Hosted(string reportPath)
        {
            // Предел защиты поднят: имитация под нагрузкой показывает больше ста градусов, а проверяется здесь не защита.
            var core = new Core(true, 400);
            string name = "CoreMap.Check." + Process.GetCurrentProcess().Id, clientReport = reportPath + ".client";
            SecurityIdentifier user = Access.SessionUser();
            var questions = new List<string>();
            // На первый вопрос о потолке выше заводского «владелец» отвечает «нет», дальше — «да».
            Func<string, bool> confirm = question =>
            {
                lock (questions) questions.Add(question);
                return questions.Count > 1;
            };
            using (var engine = new Engine(core, name, user, confirm))
            {
                Program.Tell("проверка моста: сообщение владельцу");
                bool exited;
                int code = -1;
                using (Process child = Process.Start(new ProcessStartInfo(Launch.Exe, "--simulate --bridge-client " + name + " --snapshot \"" + clientReport + "\"")
                    { UseShellExecute = false, WorkingDirectory = Program.Root }))
                {
                    exited = child.WaitForExit(90000);
                    if (exited) code = child.ExitCode;
                    else child.Kill();
                }
                Check(exited && code == 0, "вторая копия программы прошла по протоколу и вышла сама" + (exited ? ", код " + code : " — не вышла за 90 с"));
                string security = engine.Bridge.Security;
                Check(security.Contains("(A;;0x12019b;;;" + user.Value + ")") && security.Contains("(D;;FA;;;NU)") && security.Contains("ML;;NW;;;ME"),
                    "доступ к каналу, прочитанный обратно: пользователь сеанса — чтение и запись, сети — отказ, метка средняя");
                Check(questions.Count == 2 && questions[0].Contains("выше заводского"),
                    "о потолке выше заводского владельца спросили дважды (отказ и согласие): " + string.Join(" | ", questions));
                lines.Add("");
                lines.Add("канал: " + security);
                lines.Add("подключений за проверку: " + engine.Bridge.Sessions);
                lines.Add("");
                lines.Add("--- сторона окна ---");
                if (File.Exists(clientReport))
                {
                    lines.AddRange(File.ReadAllLines(clientReport));
                    File.Delete(clientReport);
                }
                else Check(false, "отчёта стороны окна нет");
            }
            core.Close();

            // Путь кнопки владельца. Здесь обе стороны без прав, поэтому итог обязан сказать, что главного он не показал.
            lines.Add("");
            lines.Add("--- путь кнопки «мост» ---");
            string verdict = Owner();
            string[] details = File.ReadAllLines(Path.Combine(Program.DataDir, "bridge-check.txt"));
            Check(verdict.StartsWith("Мост: канал и команды работают") && verdict.Contains("без прав администратора"), "итог кнопки без прав: " + verdict);
            Check(details.Any(l => l.StartsWith("сторона без прав")) && details.Any(l => l.Contains("команд " + Pings + " из " + Pings)),
                "копия, запущенная оболочкой Windows, подключилась и прислала " + Pings + " команд");
            lines.AddRange(details.Select(l => "    " + l));
        }

        // ----- скрытая проверка: сторона окна -----

        static Dictionary<string, object> Map(object value) { return (Dictionary<string, object>)value; }

        static List<object> List(object value) { return (List<object>)value; }

        static bool Ok(Dictionary<string, object> reply) { return reply != null && (bool)reply["ok"]; }

        /// Отказ с причиной: ответ пришёл, действие не сделано, причина названа.
        static bool Refused(Dictionary<string, object> reply, string reasonPart = null)
        {
            if (reply == null || (bool)reply["ok"]) return false;
            object why;
            return reply.TryGetValue("why", out why) && why is string && (reasonPart == null || ((string)why).Contains(reasonPart));
        }

        static string Why(Dictionary<string, object> reply)
        {
            object why;
            return reply == null ? "ответа нет" : reply.TryGetValue("why", out why) ? "«" + why + "»" : "принято";
        }

        /// Ждёт состояния, в котором выполнено условие.
        static bool Seen(BridgeClient client, Func<Dictionary<string, object>, bool> condition, double seconds = 3)
        {
            return client.Wait(() =>
            {
                Dictionary<string, object> state = client.Last("state");
                return state != null && condition(state);
            }, seconds);
        }

        static int At(Dictionary<string, object> state, string row, int index) { return Convert.ToInt32(List(state[row])[index]); }

        static string Row(Dictionary<string, object> state, string row) { return string.Join(",", List(state[row]).Select(v => Convert.ToString(v))); }

        public static int Client(string name, string reportPath)
        {
            try { Script(name); }
            catch (Exception e) { Check(false, "проверка оборвалась: " + e); }
            File.WriteAllLines(reportPath, lines);
            return failed == 0 ? 0 : 1;
        }

        static void Script(string name)
        {
            var client = new BridgeClient(name);
            string refusal = client.Connect(5000);
            Check(refusal == null, "подключение к каналу движка" + (refusal == null ? "" : ": " + refusal));
            if (refusal != null) return;

            // Знакомство и то, что приходит следом за ним.
            Check(Ok(client.Hello()), "знакомство принято");
            Check(client.Wait(() => client.Last("hello") != null && client.Last("limits") != null && client.Last("profiles") != null &&
                client.Last("state") != null, 3), "следом пришли hello, limits, profiles и state");
            Dictionary<string, object> hello = client.Last("hello"), limits = client.Last("limits");
            if (hello == null || limits == null || client.Last("state") == null) return;
            List<object> blocks = List(hello["blocks"]);
            int cores = List(hello["cores"]).Count, first = (int)Map(blocks[0])["first"];
            Check((int)hello["proto"] == Bridge.Protocol && (string)hello["build"] == Program.Build && (bool)hello["simulated"] &&
                List(Map(hello["layout"])["tiles"]).Count == blocks.Count,
                "hello: протокол " + hello["proto"] + ", сборка " + hello["build"] + ", ядер " + cores + ", блоков и плиток " + blocks.Count);
            Check(client.Wait(() => client.Last("notice") != null, 2) && ((string)client.Last("notice")["text"]).Contains("проверка моста"),
                "сообщение владельцу пришло с номером");
            int fused = Convert.ToInt32(List(limits["fused"])[first]);
            Check(List(limits["fused"]).Count == cores && List(limits["was"]).Count == cores && List(limits["choices"]).Count == blocks.Count &&
                Convert.ToInt32(List(List(limits["choices"])[0])[0]) == fused + CoreLimits.Over,
                "limits: заводские потолки и «было» по ядрам, допустимые значения по блокам, верхнее — заводской плюс " + CoreLimits.Over);

            int before = client.Count("state");
            Thread.Sleep(3000);
            int came = client.Count("state") - before;
            Check(came >= 4 && came <= 8, "состояние приходит дважды в секунду: за три секунды — " + came);
            Dictionary<string, object> state = client.Last("state");
            Check(new[] { "cur", "max", "ratio", "avg", "avgRatio", "thr", "load", "limit" }.All(row => List(state[row]).Count == cores) &&
                state["run"] == null && Json.Number(List(state["cur"])[0]) > 10,
                "в состоянии показания по всем ядрам, измерение не идёт; ядро " + first + ": " + List(state["cur"])[first] + " °C");

            // Команды и отказы.
            Check(Ok(client.Ask("load.toggle", "block", 0)) && Seen(client, s => At(s, "load", first) == 1), "нагрузка на блок включается");
            Check(Ok(client.Ask("load.toggle", "block", 0)) && Seen(client, s => At(s, "load", first) == 0), "и снимается");
            Check(Refused(client.Ask("load.toggle", "block", 99)), "блока 99 нет — отказ");
            Check(Ok(client.Ask("level.set", "level", 2)) && Seen(client, s => (int)s["level"] == 2), "сила нагрузки ставится");
            Check(Refused(client.Ask("level.set", "level", 7)), "силы 7 нет — отказ");
            Check(Ok(client.Ask("level.set", "level", 3)) && Ok(client.Ask("max.reset")) && Ok(client.Ask("load.stop")), "сброс максимумов и снятие нагрузки приняты");

            Check(Ok(client.Ask("limit.set", "block", 0, "ratio", fused - 2)) && Seen(client, s => At(s, "limit", first) == fused - 2), "потолок блока ставится: ×" + (fused - 2));
            Check(client.Wait(() => (bool)client.Last("profiles")["canAccept"], 2), "после этого профили говорят, что есть что принять");
            Dictionary<string, object> reply = client.Ask("limit.set", "block", 0, "ratio", fused + 1);
            Check(Refused(reply, "не подтверждён") && At(client.Last("state"), "limit", first) == fused - 2, "потолок выше заводского без согласия владельца не ставится: " + Why(reply));
            Check(Ok(client.Ask("limit.set", "block", 0, "ratio", fused + 1)) && Seen(client, s => At(s, "limit", first) == fused + 1), "с согласием — ставится: ×" + (fused + 1));
            reply = client.Ask("limit.set", "block", 0, "ratio", 200);
            Check(Refused(reply, "вне допустимого"), "потолок 200 — отказ: " + Why(reply));
            Check(Refused(client.Ask("limit.set", "block", 0)), "команда без аргумента — отказ");

            Check(Ok(client.Ask("limit.set", "block", 0, "ratio", fused - 1)) && Ok(client.Ask("profile.save", "name", "Проба")) &&
                client.Wait(() => (string)client.Last("profiles")["standing"] == "Проба" &&
                    List(client.Last("profiles")["sets"]).Any(p => (string)Map(p)["name"] == "Проба"), 2),
                "профиль сохраняется и называется стоящим");
            Check(client.Wait(() => Convert.ToInt32(List(client.Last("limits")["was"])[first]) == fused - 1, 2), "«было» после сохранения — новые потолки");
            Check(Ok(client.Ask("profile.open", "name", "")) && Seen(client, s => At(s, "limit", first) == fused) &&
                client.Wait(() => (string)client.Last("profiles")["standing"] == "", 2), "открыты заводские потолки");
            Check(Ok(client.Ask("profile.revert")) && Seen(client, s => At(s, "limit", first) == fused - 1), "откат возвращает то, что было");
            Check(Refused(client.Ask("profile.open", "name", "Такого нет")), "профиля нет — отказ");
            Check(Ok(client.Ask("profile.open", "name", "")) && Ok(client.Ask("profile.accept")) && Ok(client.Ask("profile.delete", "name", "Проба")) &&
                client.Wait(() => List(client.Last("profiles")["sets"]).Count == 0, 2), "заводские приняты, профиль удалён");

            Check(Ok(client.Ask("heat.mode", "byPeak", true)) && Ok(client.Ask("heat.mode", "byPeak", false)), "«по средней / по наибольшей» переключается");
            reply = client.Ask("fit.apply", "target", 76, "best", false);
            Check(Ok(reply) && reply.ContainsKey("note"), "потолки под температуру: исполнено, итог строкой — " + (reply != null && reply.ContainsKey("note") ? reply["note"] : "нет"));
            Check(Refused(client.Ask("fit.apply", "target", 20, "best", false)), "температура 20 — отказ");
            Check(Ok(client.Ask("fan.mode", "mode", 2)) && Seen(client, s => (int)Map(s["fans"])["mode"] == 2, 5) && Ok(client.Ask("fan.mode", "mode", 0)),
                "режим вентиляторов ставится и возвращается");
            reply = client.Ask("exec", "path", "cmd.exe");
            Check(Refused(reply, "неизвестная команда") && !client.Closed, "неизвестная команда — отказ, связь цела: " + Why(reply));

            // Измерение: запуск, отказы на его время, остановка, возврат потолков.
            string standing = Row(client.Last("state"), "limit");
            int finishedBefore = client.Count("run.finished");
            Check(Ok(client.Ask("run.start", "kind", "curve", "block", 0, "maxTemp", 80)) &&
                Seen(client, s => s["run"] != null && (string)Map(s["run"])["kind"] == "curve" && (int)Map(s["run"])["block"] == 0), "лесенка запущена, в состоянии её ход");
            Check(Refused(client.Ask("run.start", "kind", "joint"), "идёт измерение"), "второе измерение на это время — отказ");
            Check(Refused(client.Ask("limit.set", "block", 1, "ratio", fused - 1), "идёт измерение"), "потолок на это время — отказ");
            Check(Seen(client, s => Row(s, "limit") != standing, 8), "лесенка двигает потолок блока");
            Check(Ok(client.Ask("run.stop")) && client.Wait(() => client.Count("run.finished") > finishedBefore, 8), "остановка: пришло сообщение о конце измерения");
            Check(Seen(client, s => s["run"] == null && Row(s, "limit") == standing), "после остановки потолки прежние");
            Check(Refused(client.Ask("run.start", "kind", "sweep", "seconds", 7, "passes", 1)), "длительность 7 с — отказ");
            Check(Refused(client.Ask("run.start", "kind", "wipe")), "неизвестный род измерения — отказ");
            Check(Ok(client.Ask("notices.seen", "seq", (int)client.Last("notice")["seq"])), "показанное сообщение подтверждено");

            // Второй клиент при занятом канале; то же имя второй раз.
            using (var second = new BridgeClient(name))
            {
                string busy = second.Connect(600);
                Check(busy == "канал занят", "второе окно при занятом канале не подключается: " + (busy ?? "ПОДКЛЮЧИЛОСЬ"));
            }
            bool created = true;
            try { Access.Create(name, Access.Me()).Dispose(); }
            catch (Exception) { created = false; }
            Check(!created, "второй канал с тем же именем не создаётся");
            client.Dispose();

            // Негодные клиенты: движок каждого отключает и остаётся на месте.
            Violation(name, "молчит после подключения", c => { }, 4);
            Violation(name, "шлёт не JSON", c => c.Raw(Wire.Frame("это не JSON")), 2);
            Violation(name, "шлёт длину в два гигабайта", c => c.Raw(new byte[] { 0xFF, 0xFF, 0xFF, 0x7F, 1, 2 }), 2);
            Violation(name, "первым сообщением шлёт не hello", c => c.Raw(Wire.Frame("{\"id\":1,\"c\":\"load.stop\"}")), 2);
            using (var old = new BridgeClient(name))
            {
                string unreachable = old.Connect(3000);
                Dictionary<string, object> answer = unreachable == null ? old.Hello(Bridge.Protocol + 1) : null;
                Check(Refused(answer, "протокол") && old.Wait(() => old.Closed, 2), "окно другой версии протокола получает причину и отключается: " + Why(answer));
            }
            using (var remote = new BridgeClient(name, "127.0.0.1"))
            {
                string denied = remote.Connect(1500);
                Check(denied != null, "через сетевой путь этой же машины канал не открывается: " + (denied ?? "ОТКРЫЛСЯ"));
            }
            using (var again = new BridgeClient(name))
                Check(again.Connect(3000) == null && Ok(again.Hello()) && again.Wait(() => again.Last("state") != null, 3),
                    "после всего этого движок принимает обычное окно");
        }

        static void Violation(string name, string what, Action<BridgeClient> act, double seconds)
        {
            using (var client = new BridgeClient(name))
            {
                string refusal = client.Connect(3000);
                if (refusal != null) { Check(false, "клиент, который " + what + ": не подключился (" + refusal + ")"); return; }
                try { act(client); }
                catch (Exception) { }
                Check(client.Wait(() => client.Closed, seconds), "клиент, который " + what + ", отключён");
            }
        }

        // ----- проверка кнопкой владельца -----

        /// Хозяин пробного канала: одна пустая команда и счёт.
        class Pinged : BridgeHost
        {
            public volatile int Count;
            public volatile int MedianUs = -1;   // что намерила сторона без прав
            public volatile string Who, Integrity;
            public Bridge Bridge;

            public override IEnumerable<string> Greeting() { return new[] { "{\"t\":\"hello\",\"proto\":" + Bridge.Protocol + ",\"check\":true}" }; }

            public override string Execute(string command, Dictionary<string, object> fields, out string note)
            {
                note = null;
                object median;
                if (command == "ping") { Count++; return null; }
                if (command == "done" && fields.TryGetValue("medianUs", out median) && median is int) { MedianUs = (int)median; return null; }
                return "неизвестная команда";
            }

            public override void Joined(string who)
            {
                Who = who;
                Integrity = Bridge.ClientIntegrity;
            }
        }

        static string Level(string integrity)
        {
            return integrity == Access.High ? "с правами администратора" : integrity == Access.Medium ? "без прав администратора" : "уровень " + (integrity ?? "неизвестен");
        }

        /// Проверка по кнопке. Возвращает итог одной строкой для владельца; подробности — в data\bridge-check.txt.
        /// Занимает до пятнадцати секунд: звать не из потока окна.
        public static string Owner()
        {
            var log = new List<string>();
            string path = Path.Combine(Program.DataDir, "bridge-check.txt"), below = path + ".below", verdict;
            try
            {
                Directory.CreateDirectory(Program.DataDir);
                if (File.Exists(below)) File.Delete(below);
                verdict = Above(log, below);
            }
            catch (Exception e)
            {
                verdict = "Мост: проверка оборвалась — " + Program.Why(e) + ". Подробности — data\\bridge-check.txt";
                log.Add(e.ToString());
            }
            log.Insert(0, DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "  сборка " + Program.Build);
            log.Insert(0, verdict);
            try
            {
                if (File.Exists(below))
                {
                    log.Add("");
                    log.AddRange(File.ReadAllLines(below));
                    File.Delete(below);
                }
                File.WriteAllLines(path, log);
            }
            catch (Exception e) { Program.Log("отчёт проверки моста не записан: " + e); }
            return verdict;
        }

        /// Сторона с правами: канал, запуск копии без прав, счёт её команд.
        static string Above(List<string> log, string below)
        {
            const string details = ". Подробности — data\\bridge-check.txt";
            string mine = Access.Integrity();
            log.Add("сторона с правами: процесс " + Process.GetCurrentProcess().Id + ", пользователь " + Access.Me().Value + ", целостность " + mine +
                " (" + Level(mine) + "), половина маркера " + Access.ElevationType());
            SecurityIdentifier user = Access.SessionUser();
            log.Add("пользователь сеанса: " + user.Value);
            try
            {
                string shellLevel;
                SecurityIdentifier shell = Access.ShellUser(out shellLevel);
                log.Add("оболочка рабочего стола: пользователь " + shell.Value + ", целостность " + shellLevel + " (" + Level(shellLevel) + ")");
            }
            catch (Exception e) { log.Add("оболочка рабочего стола: не определена — " + Program.Why(e)); }

            var probe = new Pinged();
            string name = "CoreMap.BridgeCheck." + Guid.NewGuid().ToString("N");
            using (var bridge = new Bridge(name, user, probe))
            {
                probe.Bridge = bridge;
                bridge.Start();
                log.Add("канал " + name + ": " + bridge.Security);
                bool twice = true;
                try { Access.Create(name, user).Dispose(); }
                catch (Exception e) { twice = false; log.Add("второй канал с тем же именем не создан, как и должно быть: " + Program.Why(e)); }
                if (twice) return "Мост: второй канал с тем же именем создался — имя канала не защищено" + details;

                try { Launch.Unelevated("--bridge-check " + name + " --out " + Launch.Join(new[] { below })); }
                catch (Exception e)
                {
                    log.Add(e.ToString());
                    return "Мост: копия программы без прав не запущена — " + Program.Why(e) + details;
                }
                // Десять секунд на запуск копии и подключение, и ещё пять на команды, если она подключилась.
                double until = Native.Now() + 10;
                while (probe.MedianUs < 0 && Native.Now() < until + (probe.Who != null ? 5 : 0)) Thread.Sleep(50);
                Thread.Sleep(300);  // копия дописывает свой отчёт
                log.Add("подключилась: " + (probe.Who ?? "никто") + "; команд " + probe.Count + " из " + Pings);

                if (probe.Who == null)
                    return "Мост: копия программы без прав не подключилась за 10 с" + (File.Exists(below) ? " — " + File.ReadAllLines(below).Last() : "") + details;
                if (probe.Count < Pings || probe.MedianUs < 0)
                    return "Мост: копия без прав подключилась, но команд прошло " + probe.Count + " из " + Pings + details;
                string works = Pings + " команд, медиана " + (probe.MedianUs / 1000.0).ToString("0.00") + " мс";
                if (mine != Access.High)
                    return "Мост: канал и команды работают (" + works + "), но окно запущено без прав администратора — главного эта проверка не показала" + details;
                if (probe.Integrity != Access.Medium)
                    return "Мост: канал и команды работают (" + works + "), но копия запустилась " + Level(probe.Integrity) + " — главного эта проверка не показала" + details;
                return "Мост: ок — копия программы без прав администратора подключилась к окну с правами, " + works + details;
            }
        }

        /// Сторона без прав: CoreMap.exe --bridge-check <канал> --out <файл>. Запускает её оболочка Windows по
        /// просьбе окна; свой отчёт пишет в файл, который окно приложит к своему.
        public static int Below(string name, string reportPath)
        {
            var log = new List<string>();
            int code = 1;
            try
            {
                string mine = Access.Integrity();
                log.Add("сторона без прав: процесс " + Process.GetCurrentProcess().Id + ", пользователь " + Access.Me().Value + ", целостность " + mine +
                    " (" + Level(mine) + "), половина маркера " + Access.ElevationType());
                using (var client = new BridgeClient(name))
                {
                    string refusal = client.Connect(5000);
                    if (refusal != null) log.Add("НЕ ПОДКЛЮЧИЛАСЬ: " + refusal);
                    else
                    {
                        log.Add("канал, как он виден отсюда: " + client.Security);
                        log.Add("канал держит: " + (client.ServerExe ?? "exe узнать не удалось"));
                        if (!Ok(client.Hello())) log.Add("ЗНАКОМСТВО НЕ ПРИНЯТО");
                        else
                        {
                            var trips = new List<double>();
                            for (int k = 0; k < Pings; k++)
                            {
                                long sent = Stopwatch.GetTimestamp();
                                if (!Ok(client.Ask("ping"))) break;
                                trips.Add((Stopwatch.GetTimestamp() - sent) * 1e6 / Stopwatch.Frequency);
                            }
                            trips.Sort();
                            bool twice = true;
                            try { Access.Create(name, Access.Me()).Dispose(); }
                            catch (Exception) { twice = false; }
                            log.Add("второй канал с тем же именем отсюда " + (twice ? "СОЗДАЛСЯ" : "не создаётся, как и должно быть"));
                            log.Add("команд " + trips.Count + " из " + Pings + (trips.Count == 0 ? "" : "; ответ: медиана " + (trips[trips.Count / 2] / 1000).ToString("0.000") +
                                " мс, дольше всех " + (trips[trips.Count - 1] / 1000).ToString("0.000") + " мс"));
                            // Последней командой — что намерено: окно с правами скажет это владельцу.
                            if (trips.Count == Pings && Ok(client.Ask("done", "medianUs", (int)trips[Pings / 2])) && !twice) code = 0;
                        }
                    }
                }
            }
            catch (Exception e) { log.Add("ОШИБКА: " + e); }
            try { File.WriteAllLines(reportPath, log); }
            catch (Exception) { }
            return code;
        }
    }
}
