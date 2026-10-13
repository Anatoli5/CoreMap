// Запуск программы.
//
// CoreMap.exe               обычная работа (нужны права администратора: без них датчики недоступны)
// CoreMap.exe --limit N     свой защитный предел температуры, °C
// CoreMap.exe --simulate    имитация температур, без датчиков и без настоящей нагрузки
// CoreMap.exe --probe       опрос процессора без изменений: потолки множителей по ядрам, доступные регистры;
//                           результат в data\probe.txt, окна нет
// CoreMap.exe --run       то же, что кнопка ▶ теплового измерения: оно начинается само, окно остаётся открытым
// CoreMap.exe --calibrate  то же, что кнопка «подбор нагрузки» в окне: подбор начинается сам,
//                           по окончании окно закрывается; результат в data\calibration.json
// CoreMap.exe --snapshot F  проверка: запустить тепловое измерение, сохранить картинку окна в файл F и выйти;
//                           окно при этом стоит за краем экрана и на экране не появляется.
//                           С --simulate — через 30 секунд, на настоящих датчиках — после трёх ядер
// CoreMap.exe --curve       вместе с --simulate --snapshot: вместо теплового измерения снять кривую ядра
// CoreMap.exe --saved       вместе с --simulate: показать сохранённую тепловую таблицу с настоящих датчиков
// CoreMap.exe --fit         вместе с --simulate --snapshot: вместо теплового измерения запустить подбор под температуру
// CoreMap.exe --slide N     вместе с --simulate --saved --snapshot: вместо измерения поставить ползунок потолков на N °C
// CoreMap.exe --best        вместе с --slide: ползунок работает «по выгоде»
// CoreMap.exe --joint       вместе с --simulate --snapshot: вместо измерения запустить проверку под общей нагрузкой
// CoreMap.exe --profile     вместе с --simulate --snapshot: вместо измерения сохранить два профиля и открыть первый поверх базы
// CoreMap.exe --size WxH    вместе с --snapshot: размер окна в пикселях, например 1280x800
// CoreMap.exe --selftest    вместе с --simulate --snapshot F: без окна проверить, что измерения на любом пути выхода
//                           возвращают потолки (SelfTest.cs); отчёт — текстом в файл F
//
// Ключи, по которым программа что-то делает сама (--run, --calibrate, --snapshot и всё, что идёт вместе с ним),
// исполняет Scenario.cs. Для владельца они не нужны: всё, чем он пользуется, есть в окне.
using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Threading;
using System.Windows.Forms;

namespace CoreMap
{
    static class Program
    {
        public static readonly string Root = AppDomain.CurrentDomain.BaseDirectory;
        public static string DataDir = Path.Combine(Root, "data");  // проверка --selftest подставляет временную папку

        public static void Log(string text)
        {
            try
            {
                Directory.CreateDirectory(DataDir);
                File.AppendAllText(Path.Combine(DataDir, "error.log"), DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "\r\n" + text + "\r\n\r\n");
            }
            catch (Exception) { }  // журнал не должен ронять программу, какой бы ни была причина
        }

        static readonly Dictionary<string, string> logged = new Dictionary<string, string>();

        /// Пишет в журнал ошибку, которая может повторяться много раз в секунду (опрос датчиков, обновление окна):
        /// одна и та же ошибка в одном и том же месте where пишется один раз, а не при каждом повторе.
        public static void LogOnce(string where, Exception e)
        {
            lock (logged)
            {
                string last;
                if (logged.TryGetValue(where, out last) && last == e.Message) return;
                logged[where] = e.Message;
            }
            Log(where + ": " + e);
        }

        static string told;

        /// Сообщение владельцу о том, что случилось не по его действию: не записался файл результата, не
        /// прочитались сохранённые данные. Пишется в журнал; окно показывает его в полосе сообщений, пока
        /// владелец не сделает следующее действие. Звать можно из любого потока.
        public static void Tell(string text, Exception e = null)
        {
            Log(e == null ? text : text + "\r\n" + e);
            lock (logged) told = told == null ? text : told + " " + text;
        }

        /// Причина ошибки для сообщения владельцу: одной строкой, без точки в конце и не длиннее полутора сотен
        /// знаков — разбор JSON, например, кладёт в текст ошибки весь файл. Полный текст — в журнале.
        public static string Why(Exception e)
        {
            string text = e.Message.Split('\r', '\n')[0].Trim();
            if (text.Length > 150) text = text.Substring(0, 150).TrimEnd() + "…";
            return text.TrimEnd('.');
        }

        /// Отдаёт окну сообщения владельцу, накопившиеся с прошлого раза; null — их нет.
        public static string Told()
        {
            lock (logged)
            {
                string text = told;
                told = null;
                return text;
            }
        }

        /// Значение ключа командной строки — слово после него; null — ключа нет.
        public static string Option(string[] args, string name)
        {
            int at = Array.IndexOf(args, name);
            return at >= 0 && at + 1 < args.Length ? args[at + 1] : null;
        }

        [STAThread]
        static int Main(string[] args)
        {
            Thread.CurrentThread.CurrentCulture = CultureInfo.InvariantCulture;  // числа с точкой, а не с запятой
            CultureInfo.DefaultThreadCurrentCulture = CultureInfo.InvariantCulture;  // и в потоках измерений тоже
            if (Array.IndexOf(args, "--probe") >= 0) return Probe.Run();  // без окна; работает и при открытой программе
            bool simulate = Array.IndexOf(args, "--simulate") >= 0;
            if (simulate && Array.IndexOf(args, "--selftest") >= 0 && Option(args, "--snapshot") != null) return SelfTest.Run(Option(args, "--snapshot"));
            var scenario = new Scenario(args, simulate);
            int limit;
            int.TryParse(Option(args, "--limit"), out limit);

            bool first;
            using (new Mutex(true, "Local\\CoreMap.SingleInstance", out first))
            {
                // Скрытая проверка на имитации процессор не трогает и открытой программе не мешает.
                if (!first && !(simulate && scenario.Hidden))
                {
                    Native.ActivateOtherCopy();
                    return 0;
                }
                Application.EnableVisualStyles();
                Application.SetCompatibleTextRenderingDefault(false);
                MainForm form = null;
                Application.ThreadException += (s, e) =>
                {
                    Log(e.Exception.ToString());
                    if (form != null) form.Problem = "Ошибка в программе: " + e.Exception.Message + ". Подробности записаны в data\\error.log";
                };
                AppDomain.CurrentDomain.UnhandledException += (s, e) => Log(e.ExceptionObject.ToString());
                try
                {
                    var core = new Core(simulate, limit, scenario.Saved);
                    form = new MainForm(core, scenario.Hidden, scenario.Size);
                    scenario.Attach(form, core);
                    Application.Run(form);
                }
                catch (Exception e)
                {
                    Log(e.ToString());
                    MessageBox.Show("CoreMap не смогла запуститься:\n\n" + e.Message + "\n\nПодробности записаны в data\\error.log",
                                    "CoreMap", MessageBoxButtons.OK, MessageBoxIcon.Error);
                    return 1;
                }
            }
            return 0;
        }
    }
}
