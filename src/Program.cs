// Запуск программы.
//
// CoreMap.exe               обычная работа (с правами администратора: без них датчики недоступны)
// CoreMap.exe --limit N     свой защитный предел температуры, °C
// CoreMap.exe --simulate    имитация температур, без датчиков и без настоящей нагрузки
// CoreMap.exe --probe       опрос процессора без изменений: потолки множителей по ядрам, доступные регистры;
//                           результат в data\probe.txt, окна нет
// CoreMap.exe --run         то же, что кнопка ▶ теплового измерения: оно начинается само, окно остаётся открытым
// CoreMap.exe --calibrate   то же, что кнопка «подбор нагрузки» в окне: подбор начинается сам,
//                           по окончании окно закрывается; результат в data\calibration.json
// CoreMap.exe --snapshot F  проверка: запустить тепловое измерение, сохранить картинку окна в файл F и выйти;
//                           окно при этом стоит за краем экрана и на экране не появляется.
//                           С --simulate — через полминуты, на настоящих датчиках — после трёх блоков
// CoreMap.exe --curve       вместе с --simulate --snapshot: вместо теплового измерения снять кривую выбранного блока
// CoreMap.exe --saved       вместе с --simulate: показать сохранённые тепловую таблицу и кривые с настоящих датчиков
// CoreMap.exe --fit         вместе с --simulate --snapshot: вместо теплового измерения запустить подбор под температуру
// CoreMap.exe --slide N     вместе с --simulate --saved --snapshot: вместо измерения поставить ползунок потолков на N °C
// CoreMap.exe --best        вместе с --slide: ползунок работает «по выгоде»
// CoreMap.exe --joint       вместе с --simulate --snapshot: вместо измерения запустить проверку под общей нагрузкой
// CoreMap.exe --profile     вместе с --simulate --snapshot: вместо измерения сохранить два профиля и открыть первый поверх второго
// CoreMap.exe --fans N      вместе с --simulate --snapshot: режим вентиляторов — 1 «макс при замерах», 2 «макс всегда»
// CoreMap.exe --hover       вместе с --simulate --saved --snapshot F: без мыши проверить наведение на точки графика кривых
//                           (MainForm.HoverCheck); отчёт — текстом в файл F
// CoreMap.exe --size WxH    вместе с --snapshot: размер окна в пикселях, например 1280x800
// CoreMap.exe --selftest    вместе с --simulate --snapshot F: без окна проверить, что измерения на любом пути выхода
//                           возвращают потолки, и остальное из SelfTest.cs; отчёт — текстом в файл F
// CoreMap.exe --tray        резидент: без окна поставить профиль «при запуске» и остаться значком у часов; из меню
//                           значка — сменить профиль, открыть окно программы, выйти (Tray.cs). Запускает его
//                           Windows при входе, если в окне включён выключатель «в трее при входе» (Autostart.cs)
// CoreMap.exe --simulate --tray --snapshot F   проверка резидента: значка нет, запуск на имитации, щелчки по
//                           пунктам меню, замер памяти и процессорного времени; отчёт — текстом в файл F.
//                           С --saved — на копии сохранённых профилей из data\
// CoreMap.exe --simulate --engine --snapshot F   проверка моста: движок на имитации без окна открывает канал и
//                           запускает вторую копию программы, которая проходит по протоколу как окно
//                           (BridgeCheck.cs); отчёт — текстом в файл F
// CoreMap.exe --simulate --bridge-client N --snapshot F   та вторая копия: N — имя канала; сама по себе не нужна
// CoreMap.exe --bridge-check N --out F   сторона без прав в проверке моста кнопкой владельца: её запускает окно
//
// Права администратора программа просит сама (Launch.Elevate), когда они нужны: при любом запуске без
// --simulate, кроме сторон моста. В манифесте их требования нет: окну без прав и проверкам моста они ни к чему.
//
// Ключи, по которым программа что-то делает сама (--run, --calibrate, --snapshot и всё, что идёт вместе с ним),
// исполняет Scenario.cs: за запуск — один сценарий. Для владельца они не нужны: всё, чем он пользуется, есть в окне.
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
        /// Мьютекс-одиночка окна программы. Резидент (--tray) — одиночка сам по себе: он не мешает открыть окно
        /// с ярлыка, а по этому мьютексу узнаёт, что окно открыто.
        public const string WindowMutex = "Local\\CoreMap.SingleInstance";

        public static readonly string Root = AppDomain.CurrentDomain.BaseDirectory;
        public static readonly string Build = BuildStamp();  // когда собран этот exe: «13.10.26 05:15»; показан в заголовке окна

        /// Время сборки из заголовка самого exe: его пишет компилятор, и от копирования файла оно не меняется.
        static string BuildStamp()
        {
            try
            {
                byte[] head = new byte[4096];
                using (FileStream file = File.OpenRead(System.Reflection.Assembly.GetExecutingAssembly().Location))
                    file.Read(head, 0, head.Length);
                int pe = BitConverter.ToInt32(head, 0x3C);
                DateTime built = new DateTime(1970, 1, 1, 0, 0, 0, DateTimeKind.Utc).AddSeconds(BitConverter.ToUInt32(head, pe + 8));
                return built.ToLocalTime().ToString("dd.MM.yy HH:mm");
            }
            catch (Exception) { return "неизвестна"; }
        }
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
            lock (logged)
            {
                told = told == null ? text : told + " " + text;
                notices.Add(new KeyValuePair<int, string>(++noticeLast, text));
                if (notices.Count > NoticesKept) notices.RemoveAt(0);
            }
        }

        // Те же сообщения для окна на другом конце моста (Feed.cs): с номерами и без «прочитал — пропало». Окно
        // подтверждает показанное (Seen); неподтверждённое уйдёт ему снова после переподключения. Список
        // ограничен: без окна сообщения остаются в журнале, а здесь — только последние.
        const int NoticesKept = 20;
        static readonly List<KeyValuePair<int, string>> notices = new List<KeyValuePair<int, string>>();
        static volatile int noticeLast;

        /// Номер последнего сообщения владельцу; 0 — их не было.
        public static int NoticeLast { get { return noticeLast; } }

        /// Сообщения владельцу с номером больше after, по порядку.
        public static KeyValuePair<int, string>[] NoticesAfter(int after)
        {
            lock (logged) return notices.FindAll(n => n.Key > after).ToArray();
        }

        /// Окно показало сообщения по номер seq включительно: больше они ему не нужны.
        public static void NoticesSeen(int seq)
        {
            lock (logged) notices.RemoveAll(n => n.Key <= seq);
        }

        /// Причина ошибки для сообщения владельцу: одной строкой, без точки в конце и не длиннее сотни с небольшим
        /// знаков — разбор JSON, например, кладёт в текст ошибки весь файл. Полный текст — в журнале.
        public static string Why(Exception e)
        {
            if (e is SpoiledFile) return e.Message;  // уже составлено для владельца и уже укорочено
            string text = e.Message.Split('\r', '\n')[0].Trim();
            if (text.Length > 120) text = text.Substring(0, 120).TrimEnd() + "…";
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
            bool simulate = Array.IndexOf(args, "--simulate") >= 0;
            string snapshot = Option(args, "--snapshot");
            // Стороны моста без прав администратора: им процессор не нужен, и прав они не просят.
            if (Option(args, "--bridge-check") != null) return BridgeCheck.Below(Option(args, "--bridge-check"), Option(args, "--out"));
            if (simulate && Option(args, "--bridge-client") != null && snapshot != null) return BridgeCheck.Client(Option(args, "--bridge-client"), snapshot);
            // Всё остальное без имитации работает с процессором, а это только с правами администратора. Манифест
            // их не требует (у программы есть роли без прав), поэтому запрос Windows вызывает сама программа.
            if (!simulate && !Launch.Elevated()) return Launch.Elevate(args);
            if (Array.IndexOf(args, "--probe") >= 0) return Probe.Run();  // без окна; работает и при открытой программе
            if (simulate && Array.IndexOf(args, "--selftest") >= 0 && snapshot != null) return SelfTest.Run(snapshot);
            if (simulate && Array.IndexOf(args, "--engine") >= 0 && snapshot != null) return BridgeCheck.Host(snapshot);
            AppDomain.CurrentDomain.UnhandledException += (s, e) => Log(e.ExceptionObject.ToString());
            // Резидент уходит в свой файл до всего, что касается окна: само упоминание окна в этом методе
            // загрузило бы в его процесс библиотеки интерфейса.
            if (Array.IndexOf(args, "--tray") >= 0) return Tray.Run(simulate, snapshot, Array.IndexOf(args, "--saved") >= 0);
            return Window(args, simulate);
        }

        static int Window(string[] args, bool simulate)
        {
            var scenario = new Scenario(args, simulate);
            int limit;
            int.TryParse(Option(args, "--limit"), out limit);

            // Скрытая проверка на имитации процессор не трогает, одиночкой не считается и мьютекс не берёт:
            // иначе окно, которое владелец открыл во время проверки, приняло бы её за свою копию и не открылось.
            bool quiet = simulate && scenario.Hidden, first = true;
            using (quiet ? null : new Mutex(true, WindowMutex, out first))
            {
                if (!first)
                {
                    Native.ActivateOtherCopy();
                    return 0;
                }
                if (!quiet) Tray.Announce();  // резидент с этого момента потолки не трогает
                Application.EnableVisualStyles();
                Application.SetCompatibleTextRenderingDefault(false);
                MainForm form = null;
                Application.ThreadException += (s, e) =>
                {
                    Log(e.Exception.ToString());
                    if (form != null) form.Problem = "Ошибка в программе: " + e.Exception.Message + ". Подробности записаны в data\\error.log";
                };
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
