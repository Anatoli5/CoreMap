// Резидент: CoreMap.exe --tray. Значок у часов без окна программы.
//
// Зачем: чтобы профиль потолков ставился при входе в Windows, не нужно держать в памяти окно со всеми его
// таблицами, потоком датчиков и нагрузкой. Резидент делает только три вещи: при запуске возвращает потолки
// по метке оборванного измерения и ставит профиль «при запуске» (тем же путём, что окно, — Startup.cs);
// по щелчку в меню значка ставит другой профиль; открывает окно программы отдельным процессом.
//
// Что он не делает: не создаёт Core, не опрашивает датчики, не заводит таймеров и потоков. Между действиями
// владельца процесс спит в ожидании сообщения Windows; дескриптор драйвера PawnIO открыт только на время
// чтения и записи потолков. Значок и меню — прямыми вызовами Windows, без System.Windows.Forms: значок с
// меню из этой библиотеки, даже не показанный, добавляет процессу 9,5 МБ выделенной памяти — больше половины
// того, что занимает весь резидент (замер на 275HX 13.10.2026, цифры резидента — в отчёте скрытой проверки).
//
// Резидент и окно. Окно — отдельный процесс того же exe. Пока оно открыто, резидент потолки не трогает и
// профили в его меню серые: два места, меняющие потолки одновременно, расходились бы. Открыто ли окно, говорит
// его мьютекс-одиночка (Program.WindowMutex); окно при запуске ещё и сообщает резиденту номер своего процесса
// (Announce), и тот ждёт его конца, чтобы перечитать файлы профилей и потолки — без опроса.
//
// Resident — всё, что не касается значка: потолки, профили, что показать в подсказке и меню. Его проверяет
// самопроверка (SelfTest.cs). Tray — значок, меню и ожидание сообщений.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;

namespace CoreMap
{
    /// Строка меню значка.
    class TrayItem
    {
        public int Id;        // что сделать по щелчку (Resident.OpenId и другие); 0 — строка-пояснение или разделитель
        public string Text;   // null — разделитель
        public bool Checked, Enabled, Default;
    }

    class Resident
    {
        public const int OpenId = 1, ExitId = 2, FactoryId = 100;  // 101 и дальше — профили в порядке списка

        readonly bool simulate;
        readonly string saveDir;
        readonly CpuCore[] cores;
        readonly int[] lps;
        readonly BlockMap blocks;
        readonly string cpu;
        PawnIo io;              // null — имитация или драйвер не открылся
        Profile profile;        // профиль, который ставится при запуске
        ProfileSets profiles;
        string[] listed = new string[0];  // названия профилей в порядке последнего построенного меню
        string unavailable;     // почему потолки недоступны
        string refusal;         // отказ последнего действия; сбрасывается следующим

        public CoreLimits Limits { get; private set; }  // null — потолки недоступны
        /// Открыто окно программы: потолки и профили меняются в нём, резидент их не трогает и о них не говорит.
        /// То же — пока у движка жив Core (к нему подключено окно или идёт измерение без окна).
        public bool WindowOpen;
        /// Какое измерение идёт у движка без окна, для подсказки значка; null — никакое.
        public string Doing;
        /// «Было»: потолки, которые стояли в процессоре при запуске резидента до профиля «при запуске», а дальше —
        /// поставленные из меню значка или принятые в окне. Движок отдаёт их Core (Inherited). null — неизвестны.
        public int[] Was;

        /// Доступ к процессору, которым резидент делится с Core движка; null — имитация или драйвер не открылся.
        public PawnIo Io { get { return io; } }

        /// saveDir — папка файлов профилей и метки. simulate — «процессор» в памяти, как в окне с --simulate.
        public Resident(bool simulate, string saveDir)
        {
            this.simulate = simulate;
            this.saveDir = saveDir;
            cores = Topology.Read().ToArray();
            lps = cores.Select(c => c.Lp).ToArray();
            blocks = new BlockMap(cores);
            string identifier;
            Topology.Identity(out cpu, out identifier);
        }

        public bool Available { get { return Limits != null && profile != null; } }

        /// Название профиля, который ставится при запуске; "" — никакой, остаются заводские потолки.
        public string AtStartup { get { return profile != null && profile.Exists ? profile.Name : ""; } }

        /// Читает потолки из процессора и профили из файлов. apply — запуск резидента: потолки возвращаются по
        /// метке оборванного измерения, затем ставится профиль «при запуске». Без apply профиль только читается
        /// (метка возвращается и тогда: окно могли снять посреди измерения).
        /// Возвращает то, о чём владельцу надо сказать уведомлением; null — нечего.
        public string Load(bool apply)
        {
            refusal = null;
            try
            {
                if (Limits == null)
                {
                    if (!simulate && io == null) io = new PawnIo(Path.Combine(Program.Root, "modules", "IntelMSR.bin"));
                    Limits = new CoreLimits(io, cores, blocks);
                }
                else
                {
                    string unread = Limits.Refresh();
                    if (unread != null) refusal = "потолки не перечитаны из процессора: " + unread;
                }
                unavailable = null;
                var startup = new LimitsStartup(Limits, blocks, lps, cpu, saveDir, apply);
                profile = startup.Profile;
                profiles = startup.Profiles;
                if (apply || Was == null) Was = startup.Was;
                // Уведомления стоит только то, что профиль при запуске не встал. Встал с оговоркой (после прошлого
                // применения в журнале Windows был сбой) — оговорка остаётся в подсказке и меню.
                string attention = apply && !(profile.Exists && profile.InEffect && refusal == null) ? Attention() : null;
                return startup.Recovered == null ? attention : startup.Recovered + (attention == null ? "" : " И ещё: " + attention + ".");
            }
            catch (SensorException e)
            {
                Limits = null;
                unavailable = e.Message;
                return null;  // о недоступных потолках говорит Attention: значок решает, когда сказать (он повторяет попытку)
            }
            finally { Release(); }
        }

        /// Между обращениями к процессору драйвер закрыт.
        void Release() { if (io != null) io.Close(); }

        /// Что не так, одной строкой; null — всё штатно.
        public string Attention()
        {
            if (!Available) return "потолки множителей недоступны: " + (unavailable ?? "профили не прочитаны");
            if (refusal != null) return refusal;
            if (profile.Note != null) return profile.Note;  // не прочитан, применён не полностью, применён после сбоя
            if (profile.Disabled) return "профиль «" + profile.Name + "» отключён после сбоя и при запуске не ставится";
            return null;
        }

        static bool Same(string a, string b) { return string.Equals(a, b, StringComparison.CurrentCultureIgnoreCase); }

        /// Какой сохранённый набор стоит в процессоре: название профиля, "" — заводские потолки, null — ни один.
        /// Правило — LimitsStartup.Named, общее с окном.
        public string Standing()
        {
            return Available ? LimitsStartup.Named(Limits.Current, Limits, profile, profiles) : null;
        }

        /// Подсказка значка: что стоит в процессоре и, если есть, что не так.
        public string Tip()
        {
            if (Doing != null) return "CoreMap — " + Doing;
            if (WindowOpen) return "CoreMap — открыто окно программы";
            string attention = Attention();
            if (!Available) return "CoreMap — " + attention;
            string standing = Standing();
            string text = "CoreMap — " + (standing == null ? "потолки не из профиля" : standing == "" ? "заводские потолки" : "«" + standing + "»");
            if (attention != null) return text + "\n" + attention;
            if (standing != null && Same(standing, AtStartup)) return text;
            return text + "\nпри запуске — " + (AtStartup == "" ? "заводские" : "«" + AtStartup + "»");
        }

        /// Меню значка: пояснение (если есть что сказать), заводские потолки и профили, «Открыть», «Выход».
        /// Отмечен набор, который стоит в процессоре.
        public List<TrayItem> Menu()
        {
            var items = new List<TrayItem>();
            string attention = WindowOpen ? "окно CoreMap открыто — профили переключаются в нём" : Attention();
            if (attention != null)
            {
                // Строка меню — короткая; целиком причина есть в подсказке значка и в журнале.
                items.Add(new TrayItem { Text = attention.Length <= 90 ? attention : attention.Substring(0, 89) + "…" });
                items.Add(new TrayItem());
            }
            if (Available)
            {
                // При открытом окне, что стоит в процессоре, резидент не знает: отметки нет.
                string standing = WindowOpen ? null : Standing();
                listed = profiles.Names;
                for (int k = -1; k < listed.Length; k++)
                {
                    string name = k < 0 ? "" : listed[k];
                    bool marked = standing != null && Same(standing, name), atStartup = Same(name, AtStartup);
                    items.Add(new TrayItem
                    {
                        Id = FactoryId + 1 + k, Checked = marked, Enabled = !WindowOpen,
                        Text = (name == "" ? "заводские" : name) + (atStartup && !marked ? "  · ставится при запуске" : "") +
                            (atStartup && profile.Disabled ? "  · отключён после сбоя" : ""),
                    });
                    if (k < 0 && listed.Length > 0) items.Add(new TrayItem());
                }
                items.Add(new TrayItem());
            }
            items.Add(new TrayItem { Id = OpenId, Text = "Открыть CoreMap", Enabled = true, Default = true });
            items.Add(new TrayItem { Id = ExitId, Text = "Выход", Enabled = true });
            return items;
        }

        /// Название набора по номеру пункта последнего построенного меню: "" — заводские; null — такого пункта нет.
        public string NameOf(int id)
        {
            int k = id - FactoryId - 1;
            return k == -1 ? "" : k >= 0 && k < listed.Length ? listed[k] : null;
        }

        /// Щелчок по набору в меню: ставит его потолки и делает его профилем «при запуске»; "" — заводские
        /// потолки, при запуске ничего ставиться не будет. У окна это два действия (открыть и «принять»),
        /// у резидента одно: сравнивать профили здесь негде.
        /// null — сделано; иначе причина, и тогда профиль «при запуске» остаётся прежним.
        public string Switch(string name)
        {
            if (WindowOpen) return "окно CoreMap открыто — профили переключаются в нём";
            if (!Available) return Attention();
            string title = name == "" ? "заводские потолки" : "профиль «" + name + "»";
            try
            {
                int[] ratios = name == "" ? Limits.Fused : profiles.Get(name);
                if (ratios == null) return refusal = "профиля «" + name + "» нет";
                string refused = Limits.Apply(ratios);
                if (refused != null) return refusal = Capital(title) + " не поставлен: " + refused;
                if (name == "") profile.Forget();
                // Профиль запуска, который и так стоит, заново не записывается: это обнулило бы его испытательный срок
                // (то же правило у окна — Core.Accept).
                else if (!(profile.Exists && Same(profile.Name, name)) || profile.Disabled) profile.Save(name);
                Was = (int[])Limits.Current.Clone();  // поставить из меню — то же, что открыть и принять в окне
                return refusal = null;
            }
            catch (Exception e)
            {
                Program.Log("резидент, " + title + ": " + e);
                return refusal = Capital(title) + ": " + Program.Why(e);
            }
            finally { Release(); }
        }

        static string Capital(string text) { return char.ToUpper(text[0]) + text.Substring(1); }
    }

    static class Tray
    {
        const string WindowClass = "CoreMap.Tray";      // по нему окно программы находит резидента
        const string CheckClass = "CoreMap.Tray.Check"; // скрытая проверка: её окно программа за резидента не примет
        const string Single = "Local\\CoreMap.Tray";    // одна копия резидента; у окна программы одиночка своя
        const uint IconMessage = 0x8001;                // от значка: lParam — что сделала мышь
        const uint WindowOpened = 0x8002;               // от окна программы: оно запустилось, wParam — номер его процесса
        const uint EngineIdle = 0x8003;                 // от движка: проверить, не пора ли остановить Core, и обновить подсказку
        const int RetryTimes = 3;                       // столько раз повторяется то, что не вышло при запуске (драйвер не
        const uint RetryAfter = 10000;                  // открылся, значок не встал), и через столько миллисекунд

        static Resident resident;
        // Движок (Engine.cs): канал моста и Core на то время, пока к нему подключено окно или идёт измерение.
        // null — резидент запущен без роли движка (--tray без --bridge) или канал не открылся.
        static Engine engine;
        static readonly object sync = new object();  // действия резидента с процессором и создание Core — по одному
        static int guardLimit;                   // свой защитный предел температуры для Core движка (--limit); 0 — обычный
        static bool engineWanted;                // резидент запущен в роли движка
        static int stops;                        // скрытая проверка: сколько раз Core движка останавливался
        static readonly List<string> asked = new List<string>();  // скрытая проверка: о чём «спросили владельца»
        static bool simulate, hidden;
        static IntPtr window, icon;
        static IntPtr watched;                 // процесс открытого окна программы: резидент ждёт его конца; Zero — не ждёт
        static readonly IntPtr[] waits = new IntPtr[1];
        static bool applied;                   // профиль «при запуске» уже ставился (или его поставило открытое окно)
        static int retries;                    // сколько повторов запуска осталось
        static bool shown;                     // значок стоит в области уведомлений
        static string pending;                 // уведомление, которое ещё не показано: значка пока нет
        static uint taskbarCreated;
        static NotifyData data;
        static WindowProc proc;                // держит делегат живым, пока жив класс окна
        static readonly List<string> told = new List<string>();  // скрытая проверка: что ушло бы уведомлениями
        static int announced, closed;          // скрытая проверка: сколько раз окно программы сообщило о себе и закрылось

        /// Режим --tray. snapshot — скрытая проверка (только с --simulate): значка нет, резидент проходит запуск
        /// на имитации, пишет отчёт в файл и выходит; saved — с копией сохранённых профилей из data\.
        /// bridge — роль движка (--bridge): резидент ещё и слушает канал моста и держит Core, пока тот нужен;
        /// limit — свой защитный предел температуры для этого Core.
        public static int Run(bool simulated, string snapshot, bool saved, bool bridge = false, int limit = 0)
        {
            if (snapshot != null && !simulated) return 1;  // проверка на настоящем процессоре не предусмотрена
            simulate = simulated;
            hidden = snapshot != null;
            guardLimit = limit;
            engineWanted = bridge;
            bool first = true;
            // Скрытая проверка копией резидента не считается и настоящему резиденту не мешает.
            using (hidden ? null : new Mutex(true, Single, out first))
            {
                if (!first) return 0;  // резидент уже есть
                string temp = null;
                try
                {
                    // В имитации «процессор» в памяти, а файлы профилей — настоящие, во временной папке: так
                    // резидент идёт тем же путём, что и в работе, и ничего не пишет в data\.
                    if (simulate) Program.DataDir = temp = TempCopy(saved);
                    resident = new Resident(simulate, Program.DataDir);
                    CreateWindow();
                    if (WindowIsOpen())
                    {
                        // Резидента запустили при открытом окне (выключателем в нём): метку и профиль при своём
                        // запуске уже отработало оно, а посреди его измерения метка — не признак обрыва.
                        applied = true;
                        WatchRunning();
                    }
                    else Reload();
                    if (bridge) StartEngine();
                    if (hidden)
                    {
                        if (!resident.Available) Notify(resident.Attention());
                        return Snapshot(snapshot);
                    }
                    AddIcon();
                    // Сразу после входа в Windows драйвер может быть ещё не готов, а панель задач — не принять
                    // значок: и то и другое повторяется несколько раз (Retry).
                    if (!applied || !shown) retries = RetryTimes;
                    Trim();
                    Loop(double.PositiveInfinity);
                    return 0;
                }
                catch (Exception e)
                {
                    Program.Log("резидент: " + e);
                    if (hidden) File.WriteAllLines(snapshot, new[] { "FAILED: " + e });
                    else MessageBox(IntPtr.Zero, "Резидент CoreMap не запустился:\n\n" + e.Message + "\n\nПодробности записаны в data\\error.log",
                        "CoreMap", 0x10);
                    return 1;
                }
                finally
                {
                    // Выход из резидента при живом Core — это закрытие программы: измерение останавливается и
                    // возвращает потолки, вентиляторы отпускаются (Core.Close).
                    try { if (engine != null) engine.Dispose(); }
                    catch (Exception e) { Program.Log("резидент, остановка движка: " + e); }
                    RemoveIcon();
                    try { if (temp != null) Directory.Delete(temp, true); }
                    catch (Exception) { }
                }
            }
        }

        // ----- движок -----

        /// Открывает канал моста. Не открылся (имя занято другой программой) — резидент работает как без роли движка.
        static void StartEngine()
        {
            int process = GetCurrentProcessId();
            var site = new EngineSite
            {
                Create = CreateCore,
                Refuse = () => OldWindowOpen() ? "открыто прежнее окно CoreMap: закрой его, и это окно подключится" : null,
                Idle = () => PostMessage(window, EngineIdle, IntPtr.Zero, IntPtr.Zero),
                // «Было» живёт у резидента: Core берёт его при создании и отдаёт при остановке.
                Ended = core => { if (core.Was != null) resident.Was = (int[])core.Was.Clone(); },
                // Скрытая проверка владельца не спрашивает: на первый вопрос «он» отвечает «нет», дальше — «да».
                Confirm = hidden ? (Func<string, bool>)(question => { lock (asked) { asked.Add(question); return asked.Count > 1; } }) : Native.Ask,
                LogPath = Path.Combine(Program.DataDir, "bridge.log"),
                // Скрытая проверка ни именем канала, ни мьютексом не мешает программе владельца.
                MutexName = hidden ? Engine.CoreMutex + ".Check." + process : Engine.CoreMutex,
            };
            try
            {
                engine = new Engine(hidden ? "CoreMap.Check." + process : Engine.PipeName(), Access.SessionUser(), site);
                engine.Start();
            }
            catch (Exception e)
            {
                engine = null;
                Program.Log("резидент, канал моста: " + e);
                Notify("Канал для окна программы не открыт: " + Program.Why(e));
            }
        }

        /// Core для подключившегося окна — в потоке чтения моста. Процессор, потолки и «было» у него общие с резидентом.
        static Core CreateCore()
        {
            Core core;
            lock (sync)
                core = new Core(simulate, guardLimit, false, new Inherited { Io = resident.Io, Limits = resident.Limits, Was = resident.Was });
            return core;
        }

        /// По просьбе движка, в потоке резидента: Core больше не нужен — остановить его и снова стать хозяином
        /// потолков; нужен — обновить подсказку значка (идёт измерение без окна).
        static void EngineTick()
        {
            if (engine == null) return;
            // Замок резидента здесь не берётся: Core создаётся под замком движка и уже внутри него берёт замок
            // резидента (CreateCore) — обратный порядок запер бы оба потока.
            bool stopped = engine.StopIfIdle();
            if (stopped) stops++;
            resident.Doing = engine.Attached ? null : engine.Doing;  // при окне ход измерения показывает оно
            resident.WindowOpen = WindowIsOpen();
            if (stopped && !resident.WindowOpen) Reload();
            ShowTip();
            if (stopped) Trim();
        }

        static string TempCopy(bool saved)
        {
            string from = Program.DataDir, dir = Path.Combine(Path.GetTempPath(), "coremap-tray-" + GetCurrentProcessId());
            Directory.CreateDirectory(dir);
            if (saved)
                foreach (string name in new[] { "profile.json", "profiles.json" })
                    if (File.Exists(Path.Combine(from, name))) File.Copy(Path.Combine(from, name), Path.Combine(dir, name), true);
            return dir;
        }

        // ----- связь с окном программы -----

        /// Потолками и профилями сейчас распоряжается не резидент: открыто прежнее окно программы либо у движка
        /// жив Core (к нему подключено окно или идёт измерение без окна).
        static bool WindowIsOpen() { return OldWindowOpen() || engine != null && engine.Alive; }

        /// Открыто ли прежнее окно программы (отдельный процесс со своим Core): резидент ждёт конца его процесса
        /// либо жив его мьютекс-одиночка. Скрытая проверка на окно владельца не смотрит.
        static bool OldWindowOpen() { return watched != IntPtr.Zero || !hidden && WindowRunning(); }

        static bool WindowRunning()
        {
            IntPtr mutex = OpenMutex(Synchronize, false, Program.WindowMutex);
            if (mutex == IntPtr.Zero) return false;
            CloseHandle(mutex);
            return true;
        }

        /// Окно программы сообщает резиденту, что запустилось. Зовёт окно (Program.cs).
        public static void Announce()
        {
            IntPtr tray = FindWindow(WindowClass, null);
            if (tray != IntPtr.Zero) PostMessage(tray, WindowOpened, (IntPtr)GetCurrentProcessId(), IntPtr.Zero);
        }

        /// Запускает резидента сейчас, если его ещё нет. Зовёт окно, когда владелец включает резидента:
        /// иначе значок появился бы только после следующего входа в Windows. null — сделано.
        public static string Launch()
        {
            if (FindWindow(WindowClass, null) != IntPtr.Zero) return null;
            try
            {
                // Окно работает с правами администратора, поэтому запроса Windows не будет.
                using (Process.Start(new ProcessStartInfo(Exe, "--tray") { UseShellExecute = false, WorkingDirectory = Program.Root })) { }
                return null;
            }
            catch (Exception e) { return "значок не запущен: " + Program.Why(e); }
        }

        /// Просит резидента выйти. Зовёт окно, когда владелец выключает резидента.
        public static void Dismiss()
        {
            const uint Close = 0x10;
            IntPtr tray = FindWindow(WindowClass, null);
            if (tray != IntPtr.Zero) PostMessage(tray, Close, IntPtr.Zero, IntPtr.Zero);
        }

        static string Exe { get { return typeof(Tray).Assembly.Location; } }

        /// Ждать конца процесса окна программы.
        static void Watch(int process)
        {
            if (watched != IntPtr.Zero || process == 0) return;
            watched = OpenProcess(Synchronize, false, process);
            if (watched == IntPtr.Zero) return;  // не открылся — открытое окно всё равно видно по мьютексу перед каждым действием
            resident.WindowOpen = true;
            ShowTip();
        }

        /// Окно программы открыто, а о себе не сообщило (резидент запущен позже него): найти его процесс.
        static void WatchRunning()
        {
            resident.WindowOpen = true;
            using (Process me = Process.GetCurrentProcess())
                foreach (Process p in Process.GetProcessesByName(me.ProcessName))
                    using (p)
                        if (p.Id != me.Id && p.MainWindowHandle != IntPtr.Zero) Watch(p.Id);
            ShowTip();
        }

        /// Окно программы закрылось: оно могло сменить профили и потолки.
        static void WindowClosed()
        {
            CloseHandle(watched);
            watched = IntPtr.Zero;
            closed++;
            if (WindowIsOpen()) { WatchRunning(); return; }  // тут же открыто другое
            resident.WindowOpen = false;
            Reload();
            ShowTip();
            Trim();
        }

        /// «Открыть CoreMap»: окно — отдельный процесс, чтобы память резидента не росла от интерфейса.
        static void OpenWindow()
        {
            if (WindowRunning())
            {
                Native.ActivateOtherCopy();
                return;
            }
            if (engine != null && engine.Alive)
            {
                // Два Core разом недопустимы (см. шапку Engine.cs); прежнее окно и само отказалось бы открыться.
                Notify("Прежнее окно сейчас не открыть: к программе подключено новое окно или идёт измерение");
                return;
            }
            try
            {
                // Резидент работает с правами администратора, поэтому запроса Windows не будет.
                using (Process child = Process.Start(new ProcessStartInfo(Exe, simulate ? "--simulate" : "") { UseShellExecute = false, WorkingDirectory = Program.Root }))
                    Watch(child.Id);
            }
            catch (Exception e)
            {
                Program.Log("резидент, запуск окна: " + e);
                Notify("Окно CoreMap не открылось: " + Program.Why(e));
            }
        }

        // ----- действия -----

        /// Перечитывает потолки и файлы профилей. До первого удачного чтения это ещё и запуск: метка, профиль.
        static void Reload()
        {
            bool start = !applied;
            string news;
            lock (sync) news = resident.Load(start);
            if (resident.Available) applied = true;
            // Оговорка запуска, о которой уведомления нет, остаётся в журнале.
            if (start && news == null && resident.Available && resident.Attention() != null) Program.Log("резидент: " + resident.Attention());
            Notify(news);
        }

        /// Повтор того, что не вышло при запуске: чтения потолков (драйвер при входе в Windows мог быть ещё не
        /// готов) и постановки значка (панель задач могла его не принять). О потолках владельцу говорится один
        /// раз — после последней попытки.
        static void Retry()
        {
            retries--;
            if (!shown) AddIcon();
            if (!applied && !WindowIsOpen())
            {
                Reload();
                if (!applied && retries == 0) Notify(resident.Attention());
            }
            if (applied && shown) retries = 0;
            ShowTip();
            Trim();
        }

        static void ShowMenu()
        {
            // Перед меню всё перечитывается: потолки мог сбросить сон или сменить другая программа, а окно,
            // запущенное с ярлыка, — изменить профили. Поэтому в меню то, что есть, а не то, что было.
            resident.WindowOpen = WindowIsOpen();
            if (OldWindowOpen() && watched == IntPtr.Zero) WatchRunning();
            if (!resident.WindowOpen) Reload();
            ShowTip();
            IntPtr menu = CreatePopupMenu();
            int picked;
            try
            {
                Fill(menu, resident.Menu());
                Point at;
                GetCursorPos(out at);
                SetForegroundWindow(window);  // без этого меню не закрывается щелчком мимо него
                picked = TrackPopupMenu(menu, 0x0100 | 0x0002 | 0x0080, at.X, at.Y, 0, window, IntPtr.Zero);  // вернуть выбор, правая кнопка, без сообщений
                PostMessage(window, 0, IntPtr.Zero, IntPtr.Zero);
            }
            finally { DestroyMenu(menu); }
            Command(picked);
            ShowTip();
            Trim();
        }

        static void Fill(IntPtr menu, List<TrayItem> items)
        {
            const uint Grayed = 0x1, Checked = 0x8, Separator = 0x800;
            foreach (TrayItem item in items)
            {
                if (item.Text == null) { AppendMenu(menu, Separator, UIntPtr.Zero, null); continue; }
                // «&» в названии профиля меню приняло бы за знак подчёркивания буквы.
                AppendMenu(menu, (item.Enabled ? 0 : Grayed) | (item.Checked ? Checked : 0), (UIntPtr)(uint)item.Id,
                    item.Text.Replace("&", "&&").Replace('\t', ' '));
                if (item.Default) SetMenuDefaultItem(menu, (uint)item.Id, 0);
            }
        }

        static void Command(int id)
        {
            if (id == Resident.OpenId) OpenWindow();
            else if (id == Resident.ExitId) DestroyWindow(window);
            else if (id >= Resident.FactoryId)
            {
                string name = resident.NameOf(id);
                if (name == null) return;
                lock (sync)
                {
                    resident.WindowOpen = WindowIsOpen();  // окно могли открыть или подключить, пока висело меню
                    Notify(resident.Switch(name));
                }
            }
        }

        // ----- значок -----

        static void CreateWindow()
        {
            proc = Proc;
            IntPtr module = GetModuleHandle(null);
            string name = hidden ? CheckClass : WindowClass;
            var description = new WindowDescription { Proc = proc, Instance = module, ClassName = name };
            if (RegisterClass(ref description) == 0) throw new InvalidOperationException("класс окна резидента не создан, код " + Marshal.GetLastWin32Error());
            // Окно невидимое и не показывается никогда: оно нужно только как адресат сообщений значка. Не «окно только
            // для сообщений»: такому Windows не сообщает о перезапуске панели задач.
            window = CreateWindowEx(0, name, "CoreMap", 0, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, module, IntPtr.Zero);
            if (window == IntPtr.Zero) throw new InvalidOperationException("окно резидента не создано, код " + Marshal.GetLastWin32Error());
            taskbarCreated = RegisterWindowMessage("TaskbarCreated");
            data = new NotifyData { Size = Marshal.SizeOf(typeof(NotifyData)), Window = window, Id = 1, CallbackMessage = (int)IconMessage };
        }

        static IntPtr Proc(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam)
        {
            const uint Destroy = 0x2;
            const int RightUp = 0x205, LeftDouble = 0x203, ContextMenu = 0x7B;
            // Исключение не должно уйти в Windows: оно сняло бы процесс без записи в журнал.
            try
            {
                if (message == IconMessage)
                {
                    int what = (int)((long)lParam & 0xFFFF);
                    if (what == RightUp || what == ContextMenu) ShowMenu();
                    else if (what == LeftDouble) OpenWindow();
                    return IntPtr.Zero;
                }
                if (message == WindowOpened) { announced++; Watch((int)(long)wParam); return IntPtr.Zero; }
                if (message == EngineIdle) { EngineTick(); return IntPtr.Zero; }
                // Панель задач создана заново (перезапуск Проводника) или только что появилась после входа в Windows.
                if (message == taskbarCreated && taskbarCreated != 0) { shown = false; AddIcon(); return IntPtr.Zero; }
                if (message == Destroy) { RemoveIcon(); PostQuitMessage(0); return IntPtr.Zero; }
            }
            catch (Exception e)
            {
                Program.Log("резидент: " + e);
                try { Notify("Ошибка в программе: " + Program.Why(e) + ". Подробности записаны в data\\error.log"); }
                catch (Exception) { }
            }
            return DefWindowProc(hwnd, message, wParam, lParam);
        }

        const int WithMessage = 0x1, WithIcon = 0x2, WithTip = 0x4, WithInfo = 0x10;

        /// Ставит значок. Не встал (панели задач ещё нет) — встанет по сообщению о её появлении.
        static void AddIcon()
        {
            if (hidden || shown) return;
            if (icon == IntPtr.Zero)
            {
                // Значок программы — тот, что у exe; первый попавшийся системный, если его нет.
                var small = new IntPtr[1];
                ExtractIconEx(Exe, 0, null, small, 1);
                icon = small[0] != IntPtr.Zero ? small[0] : LoadIcon(IntPtr.Zero, (IntPtr)32512);
            }
            data.Flags = WithMessage | WithIcon | WithTip;
            data.Icon = icon;
            data.Tip = Cut(resident.Tip(), 127);
            shown = ShellNotifyIcon(0, ref data);
            ShowPending();
        }

        static void RemoveIcon()
        {
            if (!shown) return;
            shown = false;
            data.Flags = 0;
            ShellNotifyIcon(2, ref data);
        }

        static void ShowTip()
        {
            if (!shown) return;
            data.Flags = WithTip;
            data.Tip = Cut(resident.Tip(), 127);
            ShellNotifyIcon(1, ref data);
        }

        /// Говорит владельцу о том, что случилось не так, как он ждал: запись в журнал и уведомление у значка.
        static void Notify(string text)
        {
            if (text == null) return;
            Program.Log("резидент: " + text);
            if (hidden) { told.Add(text); return; }
            pending = text;
            ShowPending();
        }

        static void ShowPending()
        {
            if (pending == null || !shown) return;
            data.Flags = WithInfo;
            data.Info = Cut(Capital(pending), 255);
            data.InfoTitle = "CoreMap";
            data.InfoFlags = 2;  // значок предупреждения
            if (ShellNotifyIcon(1, ref data)) pending = null;
            data.Info = "";
        }

        static string Capital(string text) { return char.ToUpper(text[0]) + text.Substring(1); }

        static string Cut(string text, int length) { return text.Length <= length ? text : text.Substring(0, length - 1) + "…"; }

        // ----- ожидание -----

        /// Отдаёт системе всё, что процессу сейчас не нужно: собирает мусор и сбрасывает рабочий набор. Страницы,
        /// к которым резидент больше не обратится, в оперативной памяти не лежат.
        static void Trim()
        {
            GC.Collect();
            GC.WaitForPendingFinalizers();
            GC.Collect();
            SetProcessWorkingSetSize(GetCurrentProcess(), (IntPtr)(-1), (IntPtr)(-1));
        }

        /// Ждёт сообщений Windows и конца процесса окна программы. Таймеров и опросов нет: пока ничего не
        /// происходит, процесс не получает процессорного времени. seconds — сколько ждать (скрытая проверка).
        static void Loop(double seconds)
        {
            const uint Infinite = 0xFFFFFFFF, Timeout = 258, AnyInput = 0x04FF, InputAvailable = 0x4, Remove = 1, Quit = 0x12;
            double until = double.IsInfinity(seconds) ? double.PositiveInfinity : Native.Now() + seconds;
            while (true)
            {
                Message m;
                while (PeekMessage(out m, IntPtr.Zero, 0, 0, Remove))
                {
                    if (m.Id == Quit) return;
                    TranslateMessage(ref m);
                    DispatchMessage(ref m);
                }
                uint wait = retries > 0 ? RetryAfter : Infinite;
                if (!double.IsInfinity(until))
                {
                    double left = until - Native.Now();
                    if (left <= 0) return;
                    wait = (uint)Math.Min(wait, left * 1000);
                }
                waits[0] = watched;
                uint count = watched != IntPtr.Zero ? 1u : 0u;
                uint result = MsgWaitForMultipleObjectsEx(count, waits, wait, AnyInput, InputAvailable);
                if (result == 0 && count == 1) WindowClosed();
                else if (result == Timeout && retries > 0) Retry();
                else if (result == Infinite)
                {
                    // Ожидание отказало (негодный дескриптор окна программы). Без паузы цикл крутился бы вхолостую.
                    if (watched != IntPtr.Zero) WindowClosed();
                    else Thread.Sleep(1000);
                }
            }
        }

        // ----- скрытая проверка -----

        struct Usage
        {
            public long Private, Set, PrivateSet;  // байт: выделено процессу; рабочий набор; его часть, не общая с другими процессами
            public long Heap;                      // байт занято объектами .NET

            public override string ToString()
            {
                return "private bytes " + Mb(Private) + " MB, working set " + Mb(Set) + " MB, private working set " + Mb(PrivateSet) + " MB, .NET heap " + Mb(Heap) + " MB";
            }

            static string Mb(long bytes) { return (bytes / 1048576.0).ToString("0.0"); }
        }

        static Usage Measure()
        {
            IntPtr me = GetCurrentProcess();
            var counters = new MemoryCounters { Size = (uint)Marshal.SizeOf(typeof(MemoryCounters)) };
            GetProcessMemoryInfo(me, ref counters, counters.Size);
            var usage = new Usage { Private = (long)counters.PrivateUsage, Set = (long)counters.WorkingSetSize, Heap = GC.GetTotalMemory(false) };
            // Страницы рабочего набора по одной: у каждой признак, общая ли она с другими процессами. Не общие —
            // это «Память» в диспетчере задач.
            int room = (int)(usage.Set / 4096) + 4096;
            IntPtr buffer = Marshal.AllocHGlobal((room + 1) * 8);
            try
            {
                if (QueryWorkingSet(me, buffer, (uint)((room + 1) * 8)))
                {
                    long count = Marshal.ReadInt64(buffer), own = 0;
                    for (long k = 1; k <= count; k++)
                        if ((Marshal.ReadInt64(buffer, (int)(k * 8)) & 0x100) == 0) own++;
                    usage.PrivateSet = own * 4096;
                }
            }
            finally { Marshal.FreeHGlobal(buffer); }
            return usage;
        }

        /// Процессорное время процесса с его запуска, мс: по учёту Windows оно идёт шагами около 16 мс.
        static double CpuTime()
        {
            long created, exited, kernel, user;
            GetProcessTimes(GetCurrentProcess(), out created, out exited, out kernel, out user);
            return (kernel + user) / 10000.0;
        }

        /// То же без шагов: такты процессора, потраченные процессом, в миллионах.
        static double CpuCycles()
        {
            ulong cycles;
            QueryProcessCycleTime(GetCurrentProcess(), out cycles);
            return cycles / 1e6;
        }

        static string[] Assemblies()
        {
            return AppDomain.CurrentDomain.GetAssemblies().Select(a => a.GetName().Name).OrderBy(n => n).ToArray();
        }

        /// Скрытая проверка: запуск уже пройден (как при входе в Windows, но на имитации). Мерит память сразу
        /// после запуска, после сброса рабочего набора и после простоя в настоящем цикле ожидания, затем щёлкает
        /// по каждому пункту меню так же, как это делает значок, и пишет отчёт. Первая строка — «ok» или «FAILED».
        static int Snapshot(string path)
        {
            const double IdleSeconds = 3;
            var lines = new List<string>();
            int failed = 0;
            Action<bool, string> check = (ok, what) =>
            {
                if (!ok) failed++;
                lines.Add((ok ? "ok      " : "FAILED  ") + what);
            };

            Usage started = Measure();
            Trim();
            Usage trimmed = Measure();
            // Простой мерится со второго захода в цикл ожидания: в первый попадают его компиляция и возврат
            // страниц после сброса рабочего набора. Счётчики до простоя читаются последним действием перед ним,
            // после — первым.
            CpuTime();
            CpuCycles();
            Loop(0.5);
            double idleTime = CpuTime(), idleCycles = CpuCycles();
            Loop(IdleSeconds);
            idleCycles = CpuCycles() - idleCycles;
            idleTime = CpuTime() - idleTime;
            double startTime = CpuTime();
            Usage idle = Measure();
            string[] loaded = Assemblies();

            string startTip = resident.Tip(), atStartup = resident.AtStartup;
            List<TrayItem> startMenu = resident.Menu();
            check(resident.Available, "потолки и профили прочитаны");
            if (atStartup != "")
                check(resident.Standing() == atStartup && startMenu.Any(i => i.Checked && i.Text == atStartup),
                    "профиль «при запуске» поставлен и отмечен в меню: " + atStartup);
            // Что резидент сказал бы при запуске, зависит от журнала Windows этой машины (сбой после прошлого
            // применения профиля), поэтому это не условие, а строка отчёта.
            string startNews = told.Count == 0 ? "none" : string.Join(" | ", told);
            told.Clear();

            // Щелчки по меню — тем же путём, что у значка: номер пункта → Command.
            var clicks = new List<string>();
            foreach (TrayItem item in startMenu.Where(i => i.Id > Resident.FactoryId).Concat(startMenu.Where(i => i.Id == Resident.FactoryId)))
            {
                resident.Menu();
                string name = resident.NameOf(item.Id);
                Command(item.Id);
                bool done = resident.Standing() == name && resident.AtStartup == name && resident.Menu().Count(i => i.Checked) == 1;
                check(done && told.Count == 0, "щелчок «" + item.Text + "»: стоит и ставится при запуске" + (told.Count > 0 ? " (" + string.Join(" | ", told) + ")" : ""));
                clicks.Add(resident.Tip().Replace("\n", " / "));
                told.Clear();
            }
            Usage used = Measure();
            Trim();
            Usage usedTrimmed = Measure();

            // То, что у значка делают вызовы Windows, проверяется без значка: размеры структур, меню, собранное
            // настоящим вызовом и прочитанное обратно, доставка сообщения окну резидента, ожидание конца процесса.
            check(Marshal.SizeOf(typeof(NotifyData)) == 976 && Marshal.SizeOf(typeof(Message)) == 48 && Marshal.SizeOf(typeof(WindowDescription)) == 72,
                "структуры Windows нужного размера");
            List<TrayItem> items = resident.Menu();
            IntPtr menu = CreatePopupMenu();
            Fill(menu, items);
            TrayItem marked = items.First(i => i.Checked);
            check(GetMenuItemCount(menu) == items.Count && (GetMenuState(menu, (uint)marked.Id, 0) & 0x8) != 0 &&
                (GetMenuState(menu, Resident.ExitId, 0) & 0x9) == 0 && GetMenuDefaultItem(menu, 0, 0) == Resident.OpenId,
                "меню Windows собрано: строк " + items.Count + ", отметка и пункт по умолчанию на месте");
            DestroyMenu(menu);
            PostMessage(window, WindowOpened, IntPtr.Zero, IntPtr.Zero);
            Loop(0.2);
            check(announced == 1, "сообщение окна программы доходит до резидента");
            // Вместо окна программы — процесс, который сразу кончается: резидент ждёт его конца без опроса.
            using (Process child = Process.Start(new ProcessStartInfo("cmd.exe", "/c exit") { UseShellExecute = false, CreateNoWindow = true }))
            {
                Watch(child.Id);
                check(resident.WindowOpen && resident.Tip().Contains("открыто окно") && resident.Switch("") != null,
                    "пока «окно» открыто, резидент профили не переключает");
                Loop(1);
            }
            check(closed == 1 && watched == IntPtr.Zero && !resident.WindowOpen, "конец процесса окна замечен, профили снова доступны");

            var engineLines = new List<string>();
            if (engineWanted)
            {
                if (engine == null) check(false, "канал моста не открылся: " + string.Join(" | ", told));
                else EngineCheck(check, engineLines, path);
            }

            lines.Add("");
            lines.Add("notifications at start: " + startNews);
            lines.Add("tip at start: " + startTip.Replace("\n", " / "));
            lines.Add("menu at start:");
            foreach (TrayItem item in startMenu)
                lines.Add("  " + (item.Text == null ? "----" : (item.Checked ? "[x] " : "[ ] ") + item.Text + (item.Enabled ? "" : "  (grayed)") + (item.Default ? "  (default)" : "")));
            lines.Add("tip after each click: " + string.Join(" | ", clicks));
            lines.Add("");
            lines.Add("memory, measured inside the process (GetProcessMemoryInfo, QueryWorkingSet):");
            lines.Add("  after start:                  " + started);
            lines.Add("  after working set reset:      " + trimmed);
            lines.Add("  after " + (IdleSeconds + 0.5) + " s in the wait loop: " + idle);
            lines.Add("  after " + clicks.Count + " menu clicks:         " + used);
            lines.Add("  after reset again:            " + usedTrimmed);
            lines.Add("cpu in " + IdleSeconds + " s of waiting: " + idleTime.ToString("0.0") + " ms (GetProcessTimes), " + idleCycles.ToString("0.00") +
                " million cycles (QueryProcessCycleTime); from process start to the end of that wait: " + startTime.ToString("0") + " ms");
            using (Process me = Process.GetCurrentProcess())
                lines.Add("threads " + me.Threads.Count + ", handles " + me.HandleCount);
            lines.Add("assemblies loaded when idle (" + loaded.Length + "): " + string.Join(", ", loaded));
            string[] later = Assemblies().Except(loaded).ToArray();
            lines.Add("assemblies added by menu clicks and this report: " + (later.Length == 0 ? "none" : string.Join(", ", later)));
            lines.AddRange(engineLines);
            lines.Insert(0, failed == 0 ? "ok" : "FAILED: " + failed);
            File.WriteAllLines(path, lines);
            return failed == 0 ? 0 : 1;
        }

        // ----- скрытая проверка роли движка -----

        static long PrivateBytes()
        {
            var counters = new MemoryCounters { Size = (uint)Marshal.SizeOf(typeof(MemoryCounters)) };
            GetProcessMemoryInfo(GetCurrentProcess(), ref counters, counters.Size);
            return (long)counters.PrivateUsage;
        }

        /// Ждёт условия в настоящем цикле ожидания резидента (сообщения движка должны доходить); false — не дождался.
        static bool Pump(Func<bool> condition, double seconds)
        {
            for (double until = Native.Now() + seconds; Native.Now() < until; Loop(0.1))
                if (condition()) return true;
            return condition();
        }

        /// Вторая копия программы в роли окна (BridgeCheck.Client). each — что делать, пока она работает.
        /// Возвращает её код выхода; -1 — не вышла за seconds.
        static int RunWindow(string pipe, string report, string keys, double seconds, Action each)
        {
            return RunCopy("--simulate --bridge-client " + pipe + " --snapshot \"" + report + "\"" + keys, seconds, each);
        }

        /// Вторая копия программы с такими ключами; ждёт её в цикле ожидания резидента. Код выхода; -1 — не вышла.
        static int RunCopy(string arguments, double seconds, Action each)
        {
            using (Process child = Process.Start(new ProcessStartInfo(Exe, arguments) { UseShellExecute = false, WorkingDirectory = Program.Root }))
            {
                bool exited = Pump(() =>
                {
                    if (each != null) each();
                    return child.HasExited;
                }, seconds);
                if (exited) return child.ExitCode;
                child.Kill();
                return -1;
            }
        }

        static bool Same(int[] a, int[] b) { return a.SequenceEqual(b); }

        /// Резидент в роли движка: окно подключается и уходит — Core появляется и исчезает, а резидент остаётся
        /// лёгким; окно уходит посреди измерения — оно идёт дальше и возвращает потолки само. Условия — в check,
        /// числа и отчёт стороны окна — в report.
        static void EngineCheck(Action<bool, string> check, List<string> report, string path)
        {
            int process = GetCurrentProcessId();
            string pipe = "CoreMap.Check." + process, mutex = Engine.CoreMutex + ".Check." + process, windowReport = path + ".window";
            Func<bool> held = () =>
            {
                Mutex other;
                if (!Mutex.TryOpenExisting(mutex, out other)) return false;
                other.Dispose();
                return true;
            };
            Func<int> threads = () => { using (Process me = Process.GetCurrentProcess()) return me.Threads.Count; };
            told.Clear();

            Trim();
            Usage before = Measure();
            int threadsBefore = threads();
            string[] loadedBefore = Assemblies();
            string security = engine.Bridge.Security, user = Access.SessionUser().Value;
            check(security.Contains("(A;;0x12019b;;;" + user + ")") && security.Contains("(D;;FA;;;NU)") && security.Contains("ML;;NW;;;ME"),
                "доступ к каналу, прочитанный обратно: пользователь сеанса — чтение и запись, сети — отказ, метка средняя");
            check(!engine.Alive && !held(), "до первого окна Core нет и мьютекса движка нет");

            // 0. Пока открыто прежнее окно, движок новому окну отказывает. «Прежнее окно» здесь — сам резидент:
            // его процесс не кончится, пока идёт проверка.
            watched = OpenProcess(Synchronize, false, process);
            int code = RunWindow(pipe, windowReport + "0", " --as refused", 20, null);
            CloseHandle(watched);
            watched = IntPtr.Zero;
            check(code == 0 && !engine.Alive, "пока открыто прежнее окно, движок новому окну отказывает, Core не создаётся; код " + code);

            // 1. Окно проходит по всему протоколу и уходит.
            Program.Tell("проверка моста: сообщение владельцу");
            long peak = 0;
            bool sawMutex = false;
            string tipWithWindow = null;
            code = RunWindow(pipe, windowReport, "", 120, () =>
            {
                if (!engine.Alive) return;
                peak = Math.Max(peak, PrivateBytes());
                if (!sawMutex) sawMutex = held();
                if (engine.Attached && tipWithWindow == null && resident.WindowOpen) tipWithWindow = resident.Tip();
            });
            check(code == 0, "вторая копия программы прошла по протоколу как окно и вышла сама, код " + code);
            check(peak > 0 && sawMutex, "пока окно подключено, Core жив и мьютекс движка существует");
            check(tipWithWindow != null && tipWithWindow.Contains("открыто окно"), "подсказка значка при окне: " + (tipWithWindow ?? "не менялась"));
            check(asked.Count == 2 && asked[0].Contains("выше заводского"),
                "о потолке выше заводского владельца спросили дважды (отказ и согласие): " + string.Join(" | ", asked));
            Pump(() => !engine.Alive, 10);
            check(!engine.Alive && !held() && stops >= 1, "окно ушло — Core остановлен, мьютекс снят; остановок за проход: " + stops);
            check(resident.Available && !resident.WindowOpen && resident.Switch("") == null, "меню значка снова распоряжается потолками");
            Trim();
            Usage after = Measure();
            int threadsAfter = threads();
            CpuTime();
            CpuCycles();
            Loop(0.5);
            double idleTime = CpuTime(), idleCycles = CpuCycles();
            Loop(3);
            idleCycles = CpuCycles() - idleCycles;
            idleTime = CpuTime() - idleTime;
            check(idleTime < 20, "без окна резидент снова не тратит процессорное время: " + idleTime.ToString("0.0") + " мс за 3 с");
            string[] added = Assemblies().Except(loadedBefore).ToArray();

            // 2. Окно запускает измерение и уходит: Core живёт до конца измерения и останавливается сам.
            int[] standing = (int[])resident.Limits.Current.Clone();
            int stopsBefore = stops;
            code = RunWindow(pipe, windowReport + "2", " --as leave", 30, null);
            check(code == 0, "второе окно запустило лесенку и вышло, код " + code);
            Pump(() => resident.Doing != null, 2);
            check(engine.Alive && !engine.Attached && held(), "окно ушло посреди измерения — Core жив");
            check(resident.Tip().Contains("кривая"), "подсказка значка без окна говорит об измерении: " + resident.Tip().Replace("\n", " / "));
            check(resident.Switch("") != null, "меню значка на это время потолки не трогает");
            check(Pump(() => !Same(resident.Limits.Current, standing), 10), "измерение идёт: потолок блока двигается");
            bool ended = Pump(() => !engine.Alive, 90);
            check(ended && stops == stopsBefore + 1 && !held(), "измерение кончилось — Core остановился сам");
            check(Same(resident.Limits.Current, standing), "потолки после измерения прежние");
            check(resident.Tip().IndexOf("кривая") < 0, "подсказка значка вернулась: " + resident.Tip().Replace("\n", " / "));

            // 3. «Было» переживает Core: одно окно оставляет его отличным от «сейчас», следующее получает таким же.
            int[] fused = resident.Limits.Fused;
            code = RunWindow(pipe, windowReport + "3", " --as mark", 20, null);
            Pump(() => !engine.Alive, 10);
            check(code == 0 && !engine.Alive && resident.Was != null && !Same(resident.Was, fused) && !Same(resident.Was, resident.Limits.Current),
                "после окна «было» у резидента — не заводские и не то, что стоит сейчас; код " + code);
            code = RunWindow(pipe, windowReport + "4", " --as was", 20, null);
            Pump(() => !engine.Alive, 10);
            check(code == 0 && !engine.Alive && Same(resident.Was, fused) && Same(resident.Limits.Current, fused),
                "следующее окно получило то же «было», вернуло заводские — и они снова «было» у резидента; код " + code);

            // 4. Новое окно (WebWindow.cs): вторая копия в роли окна со страницей. Она подключается к каналу, страница
            // знакомится с движком, рисует состояние и проходит свою проверку (ui/src/check.ts): правки только на
            // изменившиеся числа, щелчок по плитке, меню потолка. Картинки страницы ложатся рядом с отчётом: их
            // смотрит агент — как выглядит страница, проверка не судит.
            string outputs = Path.GetDirectoryName(Path.GetFullPath(path)), picture = Path.Combine(outputs, "page.png");
            foreach (string old in Directory.GetFiles(outputs, "page*.png")) File.Delete(old);  // картинки прошлого прохода не должны сойти за нынешние
            code = RunCopy("--simulate --window --pipe " + pipe + " --snapshot \"" + picture + "\" --size 1920x1040", 90, null);
            Pump(() => !engine.Alive, 10);
            check(code == 0 && File.Exists(picture) && !engine.Alive,
                "новое окно показало страницу с состоянием от движка, прошло проверку страницы и вышло, Core остановлен; код " + code);
            check(told.Count == 0, "уведомлений владельцу за проверку не было" + (told.Count == 0 ? "" : ": " + string.Join(" | ", told)));

            // Покой в самом конце: потоки, которые система заводила под обмен по каналу, к этому времени должны уйти.
            Trim();
            Usage last = Measure();
            CpuTime();
            CpuCycles();
            Loop(0.5);
            double lastTime = CpuTime(), lastCycles = CpuCycles();
            Loop(3);
            lastCycles = CpuCycles() - lastCycles;
            lastTime = CpuTime() - lastTime;

            report.Add("");
            report.Add("engine role (--bridge): the resident also listens on the pipe and holds Core while a window is connected");
            report.Add("  listening, before the first window: " + before);
            report.Add("  highest while Core was alive:        private bytes " + (peak / 1048576.0).ToString("0.0") + " MB");
            report.Add("  after the first Core stopped:        " + after + "  (left behind: " + ((after.Private - before.Private) / 1048576.0).ToString("0.0") + " MB private)");
            report.Add("  at the end, after every Core life:   " + last + "  (left behind: " + ((last.Private - before.Private) / 1048576.0).ToString("0.0") + " MB private)");
            report.Add("  cpu in 3 s of waiting after the first stop: " + idleTime.ToString("0.0") + " ms (GetProcessTimes), " + idleCycles.ToString("0.00") + " million cycles");
            report.Add("  cpu in 3 s of waiting at the end:           " + lastTime.ToString("0.0") + " ms (GetProcessTimes), " + lastCycles.ToString("0.00") + " million cycles");
            report.Add("  threads: before the first window " + threadsBefore + ", after the first stop " + threadsAfter + ", at the end " + threads() +
                "; pipe sessions " + engine.Bridge.Sessions + "; Core stops " + stops);
            report.Add("  assemblies added by Core: " + (added.Length == 0 ? "none" : string.Join(", ", added)));
            report.Add("  pipe: " + security);
            var sides = new Dictionary<string, string>
            {
                { "0", "refused while the old window is open" }, { "", "the whole protocol" }, { "2", "leaving in the middle of a measurement" },
                { "3", "leaving accepted and current ceilings that differ" }, { "4", "finding them in the next Core" },
            };
            foreach (KeyValuePair<string, string> side in sides)
            {
                report.Add("");
                report.Add("--- the window's side: " + side.Value + " ---");
                if (File.Exists(windowReport + side.Key))
                {
                    string[] theirs = File.ReadAllLines(windowReport + side.Key);
                    check(theirs.All(l => !l.StartsWith("FAILED")), "у стороны окна отказов нет (" + side.Value + ")");
                    report.AddRange(theirs);
                    File.Delete(windowReport + side.Key);
                }
                else check(false, "отчёта стороны окна нет (" + side.Value + ")");
            }

            report.Add("");
            report.Add("--- the new window: the page in WebView2, picture " + picture + " ---");
            if (File.Exists(picture + ".txt"))
            {
                report.AddRange(File.ReadAllLines(picture + ".txt"));
                File.Delete(picture + ".txt");
            }
            else check(false, "отчёта нового окна нет");

            // 5. Путь кнопки «мост» из окна программы — здесь без прав администратора с обеих сторон.
            report.Add("");
            report.Add("--- the code path of the owner's bridge button ---");
            BridgeCheck.Button(check, report);
        }

        // ----- вызовы Windows -----

        const uint Synchronize = 0x00100000;

        delegate IntPtr WindowProc(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct WindowDescription  // WNDCLASS
        {
            public uint Style;
            public WindowProc Proc;
            public int ClassExtra, WindowExtra;
            public IntPtr Instance, Icon, Cursor, Background;
            public string MenuName, ClassName;
        }

        [StructLayout(LayoutKind.Sequential)]
        struct Point { public int X, Y; }

        [StructLayout(LayoutKind.Sequential)]
        struct Message  // MSG
        {
            public IntPtr Window;
            public uint Id;
            public IntPtr WParam, LParam;
            public uint Time;
            public Point At;
            public uint Private;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        struct NotifyData  // NOTIFYICONDATA
        {
            public int Size;
            public IntPtr Window;
            public int Id, Flags, CallbackMessage;
            public IntPtr Icon;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Tip;
            public int State, StateMask;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Info;
            public int Version;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string InfoTitle;
            public int InfoFlags;
            public Guid Item;
            public IntPtr BalloonIcon;
        }

        [StructLayout(LayoutKind.Sequential)]
        struct MemoryCounters  // PROCESS_MEMORY_COUNTERS_EX
        {
            public uint Size, PageFaults;
            public UIntPtr PeakWorkingSetSize, WorkingSetSize, QuotaPeakPagedPool, QuotaPagedPool, QuotaPeakNonPagedPool, QuotaNonPagedPool,
                PagefileUsage, PeakPagefileUsage, PrivateUsage;
        }

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern ushort RegisterClass(ref WindowDescription description);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern IntPtr CreateWindowEx(int exStyle, string className, string title, int style, int x, int y, int width, int height,
                                            IntPtr parent, IntPtr menu, IntPtr instance, IntPtr parameter);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr DefWindowProc(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
        [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr window);
        [DllImport("user32.dll")] static extern void PostQuitMessage(int code);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool PeekMessage(out Message message, IntPtr window, uint first, uint last, uint remove);
        [DllImport("user32.dll")] static extern bool TranslateMessage(ref Message message);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr DispatchMessage(ref Message message);
        [DllImport("user32.dll")] static extern uint MsgWaitForMultipleObjectsEx(uint count, IntPtr[] handles, uint milliseconds, uint wakeMask, uint flags);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint RegisterWindowMessage(string name);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr FindWindow(string className, string title);
        [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
        [DllImport("user32.dll")] static extern bool GetCursorPos(out Point point);
        [DllImport("user32.dll")] static extern IntPtr CreatePopupMenu();
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern bool AppendMenu(IntPtr menu, uint flags, UIntPtr id, string text);
        [DllImport("user32.dll")] static extern bool SetMenuDefaultItem(IntPtr menu, uint item, uint byPosition);
        [DllImport("user32.dll")] static extern int TrackPopupMenu(IntPtr menu, uint flags, int x, int y, int reserved, IntPtr window, IntPtr rectangle);
        [DllImport("user32.dll")] static extern bool DestroyMenu(IntPtr menu);
        [DllImport("user32.dll")] static extern int GetMenuItemCount(IntPtr menu);
        [DllImport("user32.dll")] static extern uint GetMenuState(IntPtr menu, uint item, uint flags);
        [DllImport("user32.dll")] static extern uint GetMenuDefaultItem(IntPtr menu, uint byPosition, uint flags);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern IntPtr LoadIcon(IntPtr instance, IntPtr name);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int MessageBox(IntPtr owner, string text, string caption, uint type);
        [DllImport("shell32.dll", CharSet = CharSet.Unicode, EntryPoint = "Shell_NotifyIconW")] static extern bool ShellNotifyIcon(uint action, ref NotifyData data);
        [DllImport("shell32.dll", CharSet = CharSet.Unicode)] static extern uint ExtractIconEx(string file, int index, IntPtr[] large, IntPtr[] small, uint count);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern IntPtr GetModuleHandle(string name);
        [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
        [DllImport("kernel32.dll")] static extern int GetCurrentProcessId();
        [DllImport("kernel32.dll")] static extern bool SetProcessWorkingSetSize(IntPtr process, IntPtr minimum, IntPtr maximum);
        [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, int process);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern IntPtr OpenMutex(uint access, bool inherit, string name);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll")] static extern bool GetProcessTimes(IntPtr process, out long created, out long exited, out long kernel, out long user);
        [DllImport("kernel32.dll", EntryPoint = "K32GetProcessMemoryInfo")] static extern bool GetProcessMemoryInfo(IntPtr process, ref MemoryCounters counters, uint size);
        [DllImport("kernel32.dll")] static extern bool QueryProcessCycleTime(IntPtr process, out ulong cycles);
        [DllImport("kernel32.dll", EntryPoint = "K32QueryWorkingSet")] static extern bool QueryWorkingSet(IntPtr process, IntPtr buffer, uint size);
    }
}
