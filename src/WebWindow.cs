// Новое окно программы (DECISIONS #27): страница из папки page\ в окне без прав администратора.
//
// Страницу показывает движок Edge, который входит в Windows 11 (WebView2). Окно держит конец моста и ничего в
// сообщениях не понимает: что пришло из канала — отдаёт странице как есть, что прислала страница — кладёт в канал
// как есть. Кто что значит, знают двое: движок (Feed.cs, Commands.cs) и страница.
//
// Своих слов у окна немного:
//   страница → окно, строкой:   "ready" — страница слушает
//   окно → страница, объектом:  {"t":"link","up":true} или {"t":"link","up":false,"why":"…"} — есть ли канал к движку
// и ещё два — только у скрытой проверки (страница открыта с ?check, её шаги — в ui/src/check.ts):
//   страница → окно:            "shot:<имя>" — снять картинку страницы; "done:<строки отчёта>" — проверка кончилась
//   окно → страница:            {"t":"shot","name":"<имя>"} — картинка снята
// Объект от страницы — всегда сообщение движку. Строка — всегда слово окну.
//
// Порядок: окно молчит, пока страница не сказала "ready"; потом сообщает ей о канале; на «канал есть» страница
// сама называет себя движку (hello). Поэтому ни одно сообщение не приходит раньше, чем его есть кому принять, и
// знакомство — одно на соединение.
//
// Три библиотеки Microsoft (управляющая, для Windows Forms и загрузчик) лежат в webview2\ рядом с exe, а не
// внутри него: так у каждой остаётся подпись Microsoft, по которой их пропускает Smart App Control. Резидент и
// прежнее окно их не загружают — роль окна уходит сюда из Program.Main раньше всего остального.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Threading.Tasks;
using System.Windows.Forms;
using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;

namespace CoreMap
{
    /// Вход в роль окна. Отдельно от WebWindow: тот держит поля с типами WebView2, и его нельзя трогать, пока не
    /// сказано, где лежат библиотеки.
    static class WebHost
    {
        public static readonly string Libraries = Path.Combine(Program.Root, "webview2");
        public static readonly string Pages = Path.Combine(Program.Root, "page");

        /// pipe — имя канала движка; null — тот, что слушает движок этого сеанса. snapshot — скрытая проверка: окно
        /// за краем экрана, картинка страницы в этот файл, отчёт рядом (snapshot + ".txt"), выход. size — «ШxВ»
        /// для проверки.
        public static int Run(string pipe, string snapshot, string size)
        {
            if (pipe == null) pipe = Engine.PipeName();
            AppDomain.CurrentDomain.AssemblyResolve += (sender, e) =>
            {
                string file = Path.Combine(Libraries, new AssemblyName(e.Name).Name + ".dll");
                return File.Exists(file) ? Assembly.LoadFrom(file) : null;
            };
            try { return Open(pipe, snapshot, size); }
            catch (FileNotFoundException e)
            {
                // Библиотек окна нет на месте: до окна дело не дошло.
                string text = "Не найдены библиотеки окна: папка webview2 должна лежать рядом с программой. " + Program.Why(e);
                if (snapshot == null) MessageBox.Show(text, "CoreMap", MessageBoxButtons.OK, MessageBoxIcon.Error);
                else File.WriteAllLines(snapshot + ".txt", new[] { "FAILED", text });
                return 1;
            }
        }

        [MethodImpl(MethodImplOptions.NoInlining)]
        static int Open(string pipe, string snapshot, string size)
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            WebWindow window;
            using (window = new WebWindow(pipe, snapshot, size)) Application.Run(window);
            if (snapshot != null) window.Sweep();
            return window.Outcome;
        }
    }

    class WebWindow : Form
    {
        const string Host = "coremap.example";  // имя из зарезервированных: в сеть за ним никто не пойдёт
        const string Origin = "https://" + Host + "/";

        readonly string pipe, snapshot, profile;
        readonly WebView2 view = new WebView2 { Dock = DockStyle.Fill };
        // Пока канала нет, окно раз в секунду спрашивает Windows, появился ли он (Access.Exists — один дешёвый
        // вызов). Движок может запуститься позже окна или перезапуститься; когда канал есть, таймер стоит.
        readonly Timer knock = new Timer { Interval = 1000 };
        readonly Stopwatch clock = Stopwatch.StartNew();
        BridgeClient link;
        bool listening;   // страница сказала "ready"
        bool gone;        // окно закрывается
        string told;      // что странице сказано о канале последним: одно и то же второй раз не говорится
        double linkedSince;

        // Только скрытой проверке.
        readonly List<string> faults = new List<string>();
        readonly Timer deadline = new Timer { Interval = 60000 };
        double readyAt = -1, linkedAt = -1, drawnAt = -1;
        int toPage, toEngine, browser;
        string version = "?";
        string[] pageLines = new string[0];  // отчёт страницы: её строки "ok …" / "FAILED  …"

        /// 0 — окно отработало; у проверки — страница нарисовала состояние, ошибок не было.
        public int Outcome = 1;

        public WebWindow(string pipe, string snapshot, string size)
        {
            this.pipe = pipe;
            this.snapshot = snapshot;
            bool hidden = snapshot != null;
            // Профиль браузера (кэш, настройки движка Edge) — не рядом с программой: её папка может быть закрыта
            // на запись, а data\ — это измерения владельца. Проверка берёт временную папку и убирает её за собой.
            profile = hidden ? Path.Combine(Path.GetTempPath(), "CoreMap.page." + Process.GetCurrentProcess().Id)
                : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "CoreMap", "WebView2");

            Text = "CoreMap";
            Icon = Icon.ExtractAssociatedIcon(Application.ExecutablePath);
            // Цвет под страницей, пока она не нарисована: тот же, что --bg в page\style.css, иначе окно мигнёт белым.
            BackColor = view.DefaultBackgroundColor = Color.FromArgb(0x14, 0x16, 0x1a);
            StartPosition = FormStartPosition.Manual;
            if (hidden)
            {
                ShowInTaskbar = false;
                Location = new Point(-20000, -20000);
                ClientSize = Sized(size);
                deadline.Tick += (s, e) => Finish("проверка страницы не кончилась за " + deadline.Interval / 1000 + " с");
                deadline.Start();
            }
            else
            {
                Rectangle area = Screen.FromPoint(Cursor.Position).WorkingArea;
                Size = new Size(area.Width * 9 / 10, area.Height * 9 / 10);
                Location = new Point(area.Left + (area.Width - Width) / 2, area.Top + (area.Height - Height) / 2);
            }
            Controls.Add(view);
            knock.Tick += (s, e) => Connect();
        }

        protected override bool ShowWithoutActivation { get { return snapshot != null; } }

        static Size Sized(string text)
        {
            int width, height;
            string[] parts = (text ?? "").Split('x');
            return parts.Length == 2 && int.TryParse(parts[0], out width) && int.TryParse(parts[1], out height) ? new Size(width, height) : new Size(1920, 1040);
        }

        protected override async void OnLoad(EventArgs e)
        {
            base.OnLoad(e);
            try
            {
                CoreWebView2Environment.SetLoaderDllFolderPath(WebHost.Libraries);
                CoreWebView2Environment environment = await CoreWebView2Environment.CreateAsync(null, profile, new CoreWebView2EnvironmentOptions { Language = "ru" });
                await view.EnsureCoreWebView2Async(environment);
                CoreWebView2 web = view.CoreWebView2;
                version = environment.BrowserVersionString;
                browser = (int)web.BrowserProcessId;

                // Окно — не браузер: в нём нет ни чужих страниц, ни того, что браузер предлагает от себя.
                CoreWebView2Settings settings = web.Settings;
                settings.AreDevToolsEnabled = false;
                settings.AreDefaultContextMenusEnabled = false;      // меню «назад, обновить, просмотреть код»; свои меню рисует страница
                settings.AreBrowserAcceleratorKeysEnabled = false;   // F5, Ctrl+P, Ctrl+F и прочие; правка текста остаётся
                settings.IsStatusBarEnabled = false;
                settings.IsZoomControlEnabled = false;
                settings.IsPinchZoomEnabled = false;
                settings.IsSwipeNavigationEnabled = false;
                settings.IsPasswordAutosaveEnabled = false;
                settings.IsGeneralAutofillEnabled = false;
                settings.AreHostObjectsAllowed = false;              // страница говорит с окном только сообщениями

                // Файлы страницы — из папки рядом с exe под выдуманным именем; другим адресам они недоступны.
                web.SetVirtualHostNameToFolderMapping(Host, WebHost.Pages, CoreWebView2HostResourceAccessKind.Deny);
                web.NavigationStarting += (s, a) =>
                {
                    if (!a.Uri.StartsWith(Origin, StringComparison.OrdinalIgnoreCase)) a.Cancel = true;
                };
                web.NewWindowRequested += (s, a) => a.Handled = true;
                web.WebMessageReceived += FromPage;
                if (snapshot != null) await Watch(web);
                web.Navigate(Origin + (snapshot == null ? "index.html" : "index.html?check"));
            }
            catch (Exception error) { Broken("окно не открылось: " + Program.Why(error)); }
        }

        void Broken(string what)
        {
            if (snapshot != null)
            {
                Finish(what);  // проверка пишет в свой отчёт, а не в журнал ошибок владельца
                return;
            }
            Program.Log(what);
            MessageBox.Show(this, "Новое окно CoreMap: " + what + ".", "CoreMap", MessageBoxButtons.OK, MessageBoxIcon.Error);
            Close();
        }

        // ----- страница → окно -----

        void FromPage(object sender, CoreWebView2WebMessageReceivedEventArgs e)
        {
            if (!e.Source.StartsWith(Origin, StringComparison.OrdinalIgnoreCase)) return;
            string json = e.WebMessageAsJson;
            if (json.Length > 0 && json[0] == '"')
            {
                Word(e.TryGetWebMessageAsString());
                return;
            }
            if (link == null || link.Closed) return;  // канала нет, и страница об этом знает
            try
            {
                link.Raw(Wire.Frame(json));
                toEngine++;
            }
            catch (Exception) { }  // обрыв: его увидит поток чтения и скажет странице
        }

        void Word(string word)
        {
            if (word == "ready")
            {
                // Страница слушает. Если это она же после перезагрузки, разговор с движком начинается заново:
                // прежнее соединение закрывается, знакомство — одно на соединение.
                listening = true;
                told = null;
                if (readyAt < 0) readyAt = clock.Elapsed.TotalSeconds;
                Drop();
                Connect();
            }
            else if (snapshot != null && word.StartsWith("shot:")) Shoot(word.Substring(5));
            else if (snapshot != null && word.StartsWith("done:"))
            {
                pageLines = word.Substring(5).Split('\n');
                Finish(null);
            }
        }

        // ----- канал -----

        void Connect()
        {
            if (!listening || gone || link != null && !link.Closed) return;
            Drop();
            var next = new BridgeClient(pipe);
            next.Text = text => Later(() => Post(text));
            next.Lost = () => Later(() =>
            {
                if (link != next) return;  // соединение закрыло само окно
                // Канал, закрытый движком сразу после знакомства, — отказ (например, открыто прежнее окно).
                // Стучаться каждую секунду значило бы каждую секунду получать тот же отказ: следующая попытка позже.
                knock.Interval = clock.Elapsed.TotalSeconds - linkedSince < 2 ? 5000 : 1000;
                Say(false, "связь с движком прервалась");
                knock.Start();
            });
            string why = next.Connect(0);
            if (why != null)
            {
                next.Dispose();
                Say(false, why == "канала нет" ? "движок не запущен" : why);
                knock.Start();
                return;
            }
            knock.Stop();
            link = next;
            linkedSince = clock.Elapsed.TotalSeconds;
            if (linkedAt < 0) linkedAt = linkedSince;
            Say(true, null);
        }

        void Drop()
        {
            if (link == null) return;
            BridgeClient old = link;
            link = null;
            old.Dispose();
        }

        void Say(bool up, string why)
        {
            var message = new Dictionary<string, object> { { "t", "link" }, { "up", up } };
            if (!up) message["why"] = why;
            string text = Json.Text(message);
            if (text == told) return;
            told = text;
            Post(text);
        }

        /// Из потока чтения канала — в поток окна, в порядке прихода.
        void Later(Action action)
        {
            try { BeginInvoke(action); }
            catch (InvalidOperationException) { }  // окно уже закрыто
        }

        void Post(string text)
        {
            if (gone || view.CoreWebView2 == null) return;
            view.CoreWebView2.PostWebMessageAsJson(text);
            toPage++;
        }

        protected override void OnFormClosed(FormClosedEventArgs e)
        {
            gone = true;
            knock.Stop();
            deadline.Stop();
            Drop();
            if (snapshot == null) Outcome = 0;
            base.OnFormClosed(e);
        }

        // ----- скрытая проверка -----

        /// Ошибки страницы, которых без человека у экрана никто не увидит: исключение в её коде, console.error,
        /// файл, который не загрузился.
        async Task Watch(CoreWebView2 web)
        {
            web.GetDevToolsProtocolEventReceiver("Runtime.exceptionThrown").DevToolsProtocolEventReceived +=
                (s, e) => faults.Add("ошибка в коде страницы: " + Short(e.ParameterObjectAsJson));
            web.GetDevToolsProtocolEventReceiver("Runtime.consoleAPICalled").DevToolsProtocolEventReceived += (s, e) =>
            {
                if (e.ParameterObjectAsJson.Contains("\"type\":\"error\"")) faults.Add("console.error: " + Short(e.ParameterObjectAsJson));
            };
            web.GetDevToolsProtocolEventReceiver("Log.entryAdded").DevToolsProtocolEventReceived += (s, e) =>
            {
                if (e.ParameterObjectAsJson.Contains("\"level\":\"error\"")) faults.Add("браузер: " + Short(e.ParameterObjectAsJson));
            };
            await web.CallDevToolsProtocolMethodAsync("Runtime.enable", "{}");
            await web.CallDevToolsProtocolMethodAsync("Log.enable", "{}");
        }

        static string Short(string text) { return text.Length <= 600 ? text : text.Substring(0, 600) + "…"; }

        /// Картинка страницы: "main" — в файл проверки, другие — рядом, с именем через дефис («page-menu.png»).
        async void Shoot(string name)
        {
            if (drawnAt < 0) drawnAt = clock.Elapsed.TotalSeconds;
            string file = name == "main" ? snapshot
                : Path.Combine(Path.GetDirectoryName(snapshot), Path.GetFileNameWithoutExtension(snapshot) + "-" + name + ".png");
            try
            {
                using (var stream = new MemoryStream())
                {
                    await view.CoreWebView2.CapturePreviewAsync(CoreWebView2CapturePreviewImageFormat.Png, stream);
                    File.WriteAllBytes(file, stream.ToArray());
                }
                Post(Json.Text(new Dictionary<string, object> { { "t", "shot" }, { "name", name } }));
            }
            catch (Exception e) { Finish("картинка страницы не снята: " + Program.Why(e)); }
        }

        void Finish(string failure)
        {
            if (gone) return;
            deadline.Stop();
            if (failure != null) faults.Add(failure);
            bool failed = faults.Count > 0 || pageLines.Length == 0 || Array.Exists(pageLines, l => l.StartsWith("FAILED"));
            var lines = new List<string> { failed ? "FAILED" : "ok" };
            foreach (string fault in faults) lines.Add("FAILED  " + fault);
            if (pageLines.Length == 0) lines.Add("FAILED  страница не прислала отчёт проверки");
            lines.Add("WebView2 " + version + ", page " + ClientSize.Width + "x" + ClientSize.Height);
            lines.Add("seconds from the window's start: page listening " + Seconds(readyAt) + ", pipe connected " + Seconds(linkedAt) + ", first picture " + Seconds(drawnAt));
            lines.Add("messages relayed: to the page " + toPage + ", to the engine " + toEngine);
            lines.AddRange(pageLines);
            File.WriteAllLines(snapshot + ".txt", lines);
            Outcome = failed ? 1 : 0;
            Close();
        }

        static string Seconds(double at) { return at < 0 ? "never" : at.ToString("0.00"); }

        /// После проверки: дождаться, пока процессы браузера отпустят временный профиль, и убрать его.
        public void Sweep()
        {
            try
            {
                if (browser != 0)
                    using (Process process = Process.GetProcessById(browser)) process.WaitForExit(10000);
            }
            catch (ArgumentException) { }  // уже вышел
            for (int attempt = 0; attempt < 20 && Directory.Exists(profile); attempt++)
            {
                try { Directory.Delete(profile, true); }
                catch (Exception) { System.Threading.Thread.Sleep(250); }
            }
            if (Directory.Exists(profile)) File.AppendAllLines(snapshot + ".txt", new[] { "left behind: " + profile });
        }
    }
}
