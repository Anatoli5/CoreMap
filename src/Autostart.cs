// Запуск программы входа (CoreMapTray.exe, на Rust: rust\tray; DECISIONS #31) при входе в Windows: задание в
// планировщике. Прежний резидент (CoreMap.exe --tray) заданием больше не запускается.
//
// Почему планировщик: программе нужны права администратора, а обычная автозагрузка (папка, реестр) такую
// программу при входе не запускает — Windows не показывает запрос прав без действия человека. Задание с
// наивысшими правами запускается без запроса.
//
// Включает и выключает задание только владелец — выключателем в окне программы; сама программа его не
// создаёт. Включено ли, спрашивается у планировщика каждый раз: своей записи об этом у программы нет.
//
// Работа с планировщиком — через его объекты (Schedule.Service), а не через schtasks.exe: тот отвечает
// текстом в кодировке консоли (путь с русскими буквами пришёл бы испорченным), запускается отдельным
// процессом при каждом открытии окна и требует временный файл с описанием. Объекты отдают те же данные
// строками как есть и умеют разобрать описание задания, не создавая его (Parse) — на этом стоит проверка.
using System;
using System.IO;
using System.Reflection;
using System.Security;
using System.Security.Principal;

namespace CoreMap
{
    static class Autostart
    {
        public const string TaskName = "CoreMap tray";  // латиницей: имя видно в планировщике и в его журнале
        const int CreateOrUpdate = 6, InteractiveToken = 3;
        const int NotFound = unchecked((int)0x80070002);

        /// Программа входа рядом с CoreMap.exe.
        public static string Logon { get { return Path.Combine(Program.Root, "CoreMapTray.exe"); } }

        /// Описание задания. exe — что запускать (без ключей), user — идентификатор (SID) того, при чьём входе.
        public static string Xml(string exe, string user)
        {
            Func<string, string> text = SecurityElement.Escape;
            return string.Join("\r\n", new[]
            {
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>",
                "<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">",
                "  <RegistrationInfo>",
                "    <Description>CoreMap: значок у часов. При входе в Windows ставит профиль потолков множителей. " +
                    "Включается и выключается в окне программы CoreMap.</Description>",
                "  </RegistrationInfo>",
                "  <Triggers>",
                "    <LogonTrigger>",
                "      <Enabled>true</Enabled>",
                "      <UserId>" + text(user) + "</UserId>",
                "    </LogonTrigger>",
                "  </Triggers>",
                "  <Principals>",
                "    <Principal id=\"Author\">",
                "      <UserId>" + text(user) + "</UserId>",
                "      <LogonType>InteractiveToken</LogonType>",
                // С правами администратора и без запроса Windows: без них драйвер PawnIO не открывается.
                "      <RunLevel>HighestAvailable</RunLevel>",
                "    </Principal>",
                "  </Principals>",
                "  <Settings>",
                "    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
                // Ноутбук: по умолчанию планировщик не запускает задание от батареи и снимает его при переходе на неё.
                "    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
                "    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
                "    <AllowHardTerminate>true</AllowHardTerminate>",
                "    <StartWhenAvailable>false</StartWhenAvailable>",
                "    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>",
                "    <IdleSettings>",
                "      <StopOnIdleEnd>false</StopOnIdleEnd>",
                "      <RestartOnIdle>false</RestartOnIdle>",
                "    </IdleSettings>",
                "    <AllowStartOnDemand>true</AllowStartOnDemand>",
                "    <Enabled>true</Enabled>",
                "    <Hidden>false</Hidden>",
                "    <RunOnlyIfIdle>false</RunOnlyIfIdle>",
                "    <WakeToRun>false</WakeToRun>",
                // Резидент живёт до выхода из Windows: по умолчанию планировщик снимает задание через 72 часа.
                "    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
                // Обычный приоритет. По умолчанию у заданий пониженный (7), и окно программы, открытое из значка,
                // унаследовало бы его вместе со своей нагрузкой и измерениями.
                "    <Priority>4</Priority>",
                "  </Settings>",
                "  <Actions Context=\"Author\">",
                "    <Exec>",
                "      <Command>\"" + text(exe) + "\"</Command>",
                "      <WorkingDirectory>" + text(Path.GetDirectoryName(exe)) + "</WorkingDirectory>",
                "    </Exec>",
                "  </Actions>",
                "</Task>",
            });
        }

        // Объекты планировщика вызываются по именам: так не нужна библиотека описаний, которой в Windows нет.

        public static object Call(object target, string method, params object[] arguments)
        {
            return target.GetType().InvokeMember(method, BindingFlags.InvokeMethod, null, target, arguments);
        }

        public static object Get(object target, string property, params object[] index)
        {
            return target.GetType().InvokeMember(property, BindingFlags.GetProperty, null, target, index);
        }

        static object Service()
        {
            object service = Activator.CreateInstance(Type.GetTypeFromProgID("Schedule.Service", true));
            Call(service, "Connect");
            return service;
        }

        /// Отдаёт описание планировщику на разбор и возвращает разобранное; задание при этом не создаётся.
        /// Негодное описание — исключение. Нужно проверке (SelfTest.cs).
        public static object Parse(string xml)
        {
            object definition = Call(Service(), "NewTask", 0);
            definition.GetType().InvokeMember("XmlText", BindingFlags.SetProperty, null, definition, new object[] { xml });
            return definition;
        }

        /// Запускает ли описание задания этот файл программы входа (без ключей).
        public static bool Runs(object definition, string exe)
        {
            object actions = Get(definition, "Actions");
            if ((int)Get(actions, "Count") != 1) return false;
            object action = Get(actions, "Item", 1);
            string path = ((string)Get(action, "Path") ?? "").Trim().Trim('"'), arguments = ((string)Get(action, "Arguments") ?? "").Trim();
            return string.Equals(path, exe, StringComparison.OrdinalIgnoreCase) && arguments == "";
        }

        /// Ошибка вызова планировщика приходит обёрнутой: причина — внутри.
        static Exception Cause(Exception e) { return e is TargetInvocationException && e.InnerException != null ? e.InnerException : e; }

        /// Включён ли запуск при входе: задание есть, не отключено и запускает этот самый файл программы.
        /// Задание от программы, лежавшей в другой папке, — «выключено»: включение его перезапишет.
        /// problem — почему узнать не удалось; null — узнали.
        public static bool Enabled(out string problem) { return Enabled(TaskName, out problem); }

        public static bool Enabled(string name, out string problem)
        {
            problem = null;
            try
            {
                object task = Call(Call(Service(), "GetFolder", "\\"), "GetTask", name);
                return (bool)Get(task, "Enabled") && Runs(Get(task, "Definition"), Logon);
            }
            catch (Exception e)
            {
                e = Cause(e);
                if (e.HResult != NotFound) problem = "планировщик Windows не ответил, включён ли запуск при входе: " + Program.Why(e);
                return false;
            }
        }

        /// Создаёт задание (или перезаписывает прежнее). null — сделано; иначе причина отказа.
        public static string Enable()
        {
            if (!File.Exists(Logon)) return "запуск при входе не включён: рядом с программой нет CoreMapTray.exe";
            try
            {
                string xml = Xml(Logon, WindowsIdentity.GetCurrent().User.Value);
                // Кто и как запускает, сказано в самом описании; имя и пароль пусты.
                Call(Call(Service(), "GetFolder", "\\"), "RegisterTask", TaskName, xml, CreateOrUpdate, null, null, InteractiveToken);
                return null;
            }
            catch (Exception e)
            {
                Program.Log("задание планировщика не создано: " + Cause(e));
                return "запуск при входе не включён, планировщик Windows отказал: " + Program.Why(Cause(e));
            }
        }

        /// Удаляет задание. null — его больше нет; иначе причина отказа.
        public static string Disable()
        {
            try
            {
                Call(Call(Service(), "GetFolder", "\\"), "DeleteTask", TaskName, 0);
                return null;
            }
            catch (Exception e)
            {
                e = Cause(e);
                if (e.HResult == NotFound) return null;
                Program.Log("задание планировщика не удалено: " + e);
                return "запуск при входе не выключен, планировщик Windows отказал: " + Program.Why(e);
            }
        }
    }
}
