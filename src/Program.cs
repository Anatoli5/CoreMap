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
// CoreMap.exe --size WxH    вместе с --snapshot: размер окна в пикселях, например 1280x800
using System;
using System.Globalization;
using System.IO;
using System.Threading;
using System.Windows.Forms;

namespace CoreMap
{
    static class Program
    {
        public static readonly string Root = AppDomain.CurrentDomain.BaseDirectory;
        public static readonly string DataDir = Path.Combine(Root, "data");
        public static int TestSlide;  // проверка на имитации с ключом --slide: ползунок потолков ставится на это значение
        public static bool TestBest;  // проверка на имитации с ключом --best: ползунок делит температуру «по выгоде»
        public static bool TestJoint;  // проверка на имитации с ключом --joint
        public static bool TestFit;  // проверка на имитации с ключом --fit: вместо измерения запускается подбор под температуру

        public static void Log(string text)
        {
            try
            {
                Directory.CreateDirectory(DataDir);
                File.AppendAllText(Path.Combine(DataDir, "error.log"), DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "\r\n" + text + "\r\n\r\n");
            }
            catch (Exception) { }  // журнал не должен ронять программу, какой бы ни была причина
        }

        static string Option(string[] args, string name)
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
            TestFit = simulate && Array.IndexOf(args, "--fit") >= 0;
            if (simulate) int.TryParse(Option(args, "--slide"), out TestSlide);
            TestBest = simulate && Array.IndexOf(args, "--best") >= 0;
            TestJoint = simulate && Array.IndexOf(args, "--joint") >= 0;
            int limit;
            int.TryParse(Option(args, "--limit"), out limit);

            bool first;
            using (new Mutex(true, "Local\\CoreMap.SingleInstance", out first))
            {
                // Скрытая проверка на имитации процессор не трогает и открытой программе не мешает.
                if (!first && !(simulate && Option(args, "--snapshot") != null))
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
                    var core = new Core(simulate, limit, Array.IndexOf(args, "--saved") >= 0);
                    string[] size = (Option(args, "--size") ?? "").Split('x');
                    int width = 0, height = 0;
                    if (size.Length == 2) { int.TryParse(size[0], out width); int.TryParse(size[1], out height); }
                    form = new MainForm(core, Option(args, "--snapshot"),
                                        width > 0 && height > 0 ? new System.Drawing.Size(width, height) : System.Drawing.Size.Empty,
                                        Array.IndexOf(args, "--calibrate") >= 0, Array.IndexOf(args, "--run") >= 0,
                                        simulate && Array.IndexOf(args, "--curve") >= 0);
                    // Потолки возвращены после оборванного измерения: владелец видит это в полосе сообщений.
                    if (core.Notice != null) form.Problem = core.Notice;
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
