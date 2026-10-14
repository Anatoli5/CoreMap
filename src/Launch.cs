// Запуск копии программы с другими правами.
//
// Exe один, а ролей у него две: движок работает с процессором и без прав администратора не может ничего;
// окну и проверкам моста права не нужны. Поэтому манифест прав не требует, и копия, которой они нужны,
// перезапускает себя с запросом Windows сама (Elevate). Обратное — из процесса с правами запустить копию
// без них (Unelevated) — прямого вызова в Windows не имеет: копию просят запустить оболочку рабочего стола
// (Проводник), и она получает его права, окружение и родителя. Способ — из блога разработчика Windows
// (Raymond Chen, The Old New Thing, 18.11.2013); из процесса с правами он на этой машине не проверен до
// первого нажатия владельцем кнопки проверки моста (BridgeCheck.cs).
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Linq;
using System.Reflection;
using System.Security.Principal;

namespace CoreMap
{
    static class Launch
    {
        const string Asked = "--elevated";  // пометка перезапуска: вторая просьба о правах не делается

        public static string Exe { get { return typeof(Launch).Assembly.Location; } }

        /// Процесс работает с правами администратора.
        public static bool Elevated()
        {
            using (WindowsIdentity me = WindowsIdentity.GetCurrent())
                return new WindowsPrincipal(me).IsInRole(WindowsBuiltInRole.Administrator);
        }

        /// Перезапускает программу с теми же ключами и с правами администратора: Windows спрашивает владельца,
        /// как раньше спрашивала по манифесту. Возвращает код выхода для этой копии.
        public static int Elevate(string[] args)
        {
            if (args.Contains(Asked))
            {
                // Права уже просили, а их нет: без этой проверки копии запускали бы друг друга без конца.
                Program.Log("права администратора не получены и после запроса Windows");
                return 1;
            }
            try
            {
                var start = new ProcessStartInfo(Exe, Join(args.Concat(new[] { Asked }).ToArray()))
                    { UseShellExecute = true, Verb = "runas", WorkingDirectory = Program.Root };
                using (Process.Start(start)) { }
                return 0;
            }
            catch (Win32Exception e)
            {
                if (e.NativeErrorCode != 1223) Program.Log("запуск с правами администратора: " + e);  // 1223 — владелец ответил «нет»
                return 1;
            }
        }

        /// Ключи одной строкой для запуска; слово с пробелом берётся в кавычки.
        public static string Join(string[] args)
        {
            return string.Join(" ", args.Select(a => a.Length > 0 && a.IndexOfAny(new[] { ' ', '\t', '"' }) < 0 ? a : "\"" + a.Replace("\"", "\\\"") + "\""));
        }

        static object Get(object target, string property)
        {
            return target.GetType().InvokeMember(property, BindingFlags.GetProperty, null, target, null);
        }

        /// Запускает копию программы без прав администратора, через оболочку рабочего стола.
        public static void Unelevated(string arguments)
        {
            Unelevated(Exe, arguments);
        }

        /// Запускает программу file без прав администратора, через оболочку рабочего стола. Нет оболочки —
        /// исключение (тогда нет и значка у часов). Обращение по названиям, без библиотеки типов, — как
        /// к планировщику в Autostart.cs.
        public static void Unelevated(string file, string arguments)
        {
            const int Desktop = 0 /* CSIDL_DESKTOP */, DesktopWindow = 8 /* SWC_DESKTOP */, NeedDispatch = 1 /* SWFO_NEEDDISPATCH */;
            object shell = Activator.CreateInstance(Type.GetTypeFromProgID("Shell.Application", true));
            object windows = shell.GetType().InvokeMember("Windows", BindingFlags.InvokeMethod, null, shell, null);
            var byReference = new ParameterModifier(5);
            byReference[3] = true;  // дескриптор окна возвращается через четвёртый аргумент
            object desktop = windows.GetType().InvokeMember("FindWindowSW", BindingFlags.InvokeMethod, null, windows,
                new object[] { Desktop, null, DesktopWindow, 0, NeedDispatch }, new[] { byReference }, null, null);
            if (desktop == null) throw new InvalidOperationException("окно рабочего стола не найдено");
            object view = Get(desktop, "Document");         // вид папки рабочего стола — объект внутри Проводника
            object application = Get(view, "Application");  // его собственный Shell.Application
            application.GetType().InvokeMember("ShellExecute", BindingFlags.InvokeMethod, null, application,
                new object[] { file, arguments, Program.Root, "open", 1 });
        }
    }
}
