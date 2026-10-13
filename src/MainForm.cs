// Главное окно: собирает панели, держит верхнюю полосу и полосу сообщений, раз в полсекунды обновляет показания.
//
// Класс разложен по файлам — по разделам окна:
//   MainForm.cs          сборка окна, верхняя полоса, полоса сообщений, обновление по таймеру, закрытие
//   MainForm.Sizing.cs   подгонка размера содержимого под размер окна
//   MainForm.Die.cs      схема кристалла: выбор блока, подсказка плитки, меню потолка
//   MainForm.Profiles.cs колонка потолков: профили
//   MainForm.Limits.cs   колонка потолков: «Под температуру» и кривые
//   MainForm.Thermal.cs  тепловое влияние, задержки, «Под одинаковой нагрузкой»
// Поля раздела объявлены в его файле; общие для всех — здесь. У каждого раздела три части: построение
// панели, действия по кнопкам и обновление по таймеру (Update…), которое вызывает UpdateAll.
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm : Form
    {
        readonly Core core;
        readonly Block[] blocks;  // все блоки по порядку: строки и столбцы таблиц, строки списка
        readonly Timer timer = new Timer { Interval = 500 };
        readonly bool hidden;  // окно для скрытой проверки (Scenario): стоит за краем экрана и ни о чём не спрашивает
        HelpMode help;

        // Верхняя полоса и полоса сообщений.
        Label pkg, pkgMax, watts, since, banner, calibrationStatus;
        Button reset, stopLoad, calibrationButton, helpButton;
        Segmented level;
        public string Problem;  // ошибка программы или отказ действия, показывается в полосе сообщений

        bool wasBusy;           // в прошлый опрос шло измерение
        int sinceRefresh;       // тактов с последнего перечитывания потолков из процессора
        string limitsUnread;    // почему потолки не перечитались два раза подряд; null — перечитались
        bool unreadBefore;      // прошлое перечитывание не удалось

        /// size — размер окна; пустой — обычный: под содержимое, но не больше экрана.
        public MainForm(Core core, bool hidden, Size size)
        {
            this.core = core;
            this.hidden = hidden;
            using (Graphics g = CreateGraphics()) Theme.Scale = g.DpiX / 96f;

            Text = "CoreMap";
            BackColor = Theme.Bg;
            ForeColor = Theme.Text;
            AutoScaleMode = AutoScaleMode.None;
            AutoScroll = true;  // окно меньше содержимого с наименьшими ячейками — появляется прокрутка
            MinimumSize = new Size(Theme.Px(640), Theme.Px(420));
            StartPosition = FormStartPosition.Manual;
            Icon = Icon.ExtractAssociatedIcon(Application.ExecutablePath);
            if (hidden) ShowInTaskbar = false;

            blocks = core.Blocks.All;

            int cell = Theme.Px(NormalCell);
            int pad = Theme.Px(14);
            root = Widgets.Column(Theme.Bg, 0);
            root.Padding = new Padding(pad, pad, pad, 0);  // снизу отступ дают сами панели
            root.Margin = Padding.Empty;
            root.AutoSizeMode = AutoSizeMode.GrowAndShrink;
            root.Controls.Add(topBar = TopBar());
            banner = new Label
            {
                AutoSize = true, Font = Theme.Font(13), Padding = new Padding(Theme.Px(10), Theme.Px(6), Theme.Px(10), Theme.Px(6)),
                Margin = new Padding(0, 0, 0, Theme.Px(12)), Visible = false,
            };
            root.Controls.Add(banner);
            body = Widgets.Row();
            body.Margin = Padding.Empty;
            // Слева — схема и потолки в ряд, под ними таблица «Под одинаковой нагрузкой»; справа — тепловая связь
            // и задержки. Так окно ниже: раньше эта таблица стояла под большой таблицей справа (просьба владельца).
            Control diePanel = DiePanel(), limitsPanel = core.Limits != null || core.Curve != null ? LimitsPanel() : null;
            Control sweepPanel = SweepPanel(cell), ownPanel = OwnPanel(cell);
            leftTop = Widgets.Row();
            leftTop.Margin = Padding.Empty;
            leftTop.Controls.Add(diePanel);
            if (limitsPanel != null) leftTop.Controls.Add(limitsPanel);
            FlowLayoutPanel left = Widgets.Column(Theme.Bg, 0);
            left.Margin = Padding.Empty;
            left.Controls.Add(leftTop);
            left.Controls.Add(ownPanel);
            body.Controls.Add(left);
            body.Controls.Add(sweepPanel);
            leaves = new[] { diePanel, limitsPanel, ownPanel, sweepPanel }.Where(p => p != null).ToArray();
            root.Controls.Add(body);
            Controls.Add(root);

            // Окно открывается под обычный размер ячеек, но не больше экрана, на котором сейчас курсор.
            // Дальше содержимое подстраивается под окно: см. Fit.
            Apply(cell);
            Size want = root.GetPreferredSize(Size.Empty), frame = Size - ClientSize;
            Rectangle area = Screen.FromPoint(Cursor.Position).WorkingArea;
            Size outer = !size.IsEmpty ? size
                : new Size(Math.Min(want.Width + frame.Width, area.Width), Math.Min(want.Height + frame.Height, area.Height));
            // Скрытое окно стоит за краем экрана: на экране ничего не появляется.
            Point corner = hidden ? new Point(-20000, -20000)
                : new Point(area.X + (area.Width - outer.Width) / 2, area.Y + (area.Height - outer.Height) / 2);
            Bounds = new Rectangle(corner, outer);
            // Окно собирается невидимым и показывается готовым: иначе почти сто его элементов рисуются на экране по одному.
            if (!hidden) Opacity = 0;

            help = new HelpMode(this) { Button = helpButton };
            timer.Tick += (s, e) => Tick();
            timer.Start();
            // Сначала надписи, потом одна подгонка: подгонка до надписей выбрасывалась, потому что надписи меняют высоту.
            Tick();
            built = true;
            Fit();
        }

        protected override void OnShown(EventArgs e)
        {
            base.OnShown(e);
            if (Opacity == 1) return;
            Refresh();
            Opacity = 1;
        }

        /// Скрытое окно не забирает ввод у того, чем занят владелец.
        protected override bool ShowWithoutActivation { get { return hidden; } }

        // ----- построение окна -----

        FlowLayoutPanel TopBar()
        {
            FlowLayoutPanel bar = Widgets.Row();
            bar.WrapContents = true;  // в узком окне строка переносится
            bar.Margin = new Padding(0, 0, 0, Theme.Px(12));
            bar.Controls.Add(Widgets.Label(core.CpuName, Theme.Text, 15, true));
            const string pkgTip = "Температура корпуса процессора\nСамая горячая точка кристалла: сейчас и максимум с последнего сброса.";
            bar.Controls.Add(pkg = Widgets.Label("", Theme.Text, 17, true, pkgTip));
            pkg.Margin = new Padding(Theme.Px(8), 0, 0, 0);
            bar.Controls.Add(pkgMax = Widgets.Label("", Theme.Dim, 13, false, pkgTip));
            bar.Controls.Add(watts = Widgets.Label("", Theme.Text, 17, true,
                "Мощность процессора\nСколько ватт сейчас потребляет весь процессор, по его счётчику энергии."));
            bar.Controls.Add(reset = Widgets.Button("↺", () => { if (core.Monitor != null) core.Monitor.ResetMax(); core.ResetAverage(); },
                "Сброс максимумов и средних\nНачинает отсчёт максимальных и средних температур заново."));
            reset.Margin = new Padding(0, 0, Theme.Px(4), 0);
            bar.Controls.Add(since = Widgets.Label("", Theme.Dim, 13, false, "Время с последнего сброса максимумов"));
            bar.Controls.Add(stopLoad = Widgets.Button("■", core.StopLoad, "Снять нагрузку\nОстанавливает нагрузку на всех ядрах."));
            bar.Controls.Add(level = new Segmented(LoadManager.Levels, v => Theme.LevelGlyph[v], core.SetLevel,
                "Сила нагрузки\nНасколько тяжёлые вычисления получает нагруженное ядро.\n" +
                "• при щелчке по ядру — ровно эта сила\n" +
                "• в тепловом измерении — верхняя граница: под каждое ядро сила подбирается сама\n" +
                "• полная сила доводит часть ядер до порога перегрева"));
            bar.Controls.Add(calibrationButton = Widgets.Button("⚙", CalibrationClick,
                "Подбор нагрузки\nИзмеряет, насколько каждый вид вычислений греет ядро, и выбирает по этому три ступени силы.\n" +
                "• около 2 минут: по очереди греет два P-ядра\n" +
                "• повторное нажатие останавливает подбор\n" +
                "• компьютер в это время лучше не нагружать: посторонняя работа искажает замер\n" +
                "• результат запоминается; повторять нужно только на другом процессоре"));
            bar.Controls.Add(calibrationStatus = Widgets.Label("", Theme.Dim, 13, false,
                "Подбор нагрузки\nВо время подбора — его ход. После — нагрев ядра на лёгкой, средней и полной силе, " +
                "среднее по двум P-ядрам."));
            if (core.Monitor != null)
                bar.Controls.Add(Widgets.Label("предел " + core.Limit + "°", Theme.Dim, 13, false,
                    "Защитный предел\nЕсли любое ядро держится на этой температуре 3 секунды, нагрузка снимается сама.\n" +
                    "• процессор сам сбрасывает частоту при " + core.ThrottleAt + " °C\n" +
                    "• свой предел задаётся при запуске: --limit N"));
            bar.Controls.Add(helpButton = Widgets.Button("?", () => help.Set(true),
                "Режим вопроса\nПока он включён, при наведении на любой элемент окна показывается подсказка о нём.\n" +
                "• первый щелчок в окне выключает режим и ничего не нажимает\n" +
                "• клавиша Esc тоже выключает\n" +
                "• без этого режима подсказки не всплывают"));
            foreach (Control c in bar.Controls) c.Anchor = AnchorStyles.Left;
            // Надписи с живыми числами держат ширину под самый длинный текст: иначе каждая смена цифры двигала соседей.
            Hold(pkg, "100 °C");
            Hold(pkgMax, "▲ 100");
            Hold(watts, "200 Вт");
            Hold(since, "00:00:00");
            return bar;
        }

        static void Hold(Label label, string widest)
        {
            label.MinimumSize = new Size(TextRenderer.MeasureText(widest, label.Font).Width, 0);
        }

        FlowLayoutPanel Section(string title, string tip, out FlowLayoutPanel head)
        {
            FlowLayoutPanel panel = Widgets.Column(Theme.Panel, 14);
            panel.Margin = new Padding(0, 0, Theme.Px(14), Theme.Px(14));
            head = Widgets.Row();
            head.Controls.Add(Widgets.Label(title, Theme.Text, 14, true, tip));
            panel.Controls.Add(head);
            return panel;
        }

        /// Заголовок раздела внутри панели.
        static Label Heading(string text)
        {
            Label l = Widgets.Label(text, Theme.Text, 13, true);
            l.Margin = new Padding(0, Theme.Px(16), 0, Theme.Px(6));
            return l;
        }

        static Label StatusLabel(int width)
        {
            Label l = Widgets.Label("", Theme.Dim);
            l.MaximumSize = new Size(width, 0);
            l.MinimumSize = new Size(width, 0);
            l.Margin = new Padding(0, 0, 0, Theme.Px(10));
            return l;
        }

        // ----- действия -----

        /// Показывает отказ действия в полосе сверху; null — действие выполнено.
        void Report(string refusal)
        {
            Problem = refusal == null ? null : "Не сделано: " + refusal;
            Tick();
        }

        void CalibrationClick()
        {
            if (core.Calibration.Running) core.Calibration.Stop();
            else core.StartCalibration();
        }

        protected override void OnFormClosing(FormClosingEventArgs e)
        {
            if (core.Busy && !hidden && MessageBox.Show(this,
                    "Идёт измерение. Если закрыть окно, оно прервётся.\n\nЗакрыть?", "CoreMap", MessageBoxButtons.OKCancel,
                    MessageBoxIcon.Warning, MessageBoxDefaultButton.Button2) != DialogResult.OK)
            {
                e.Cancel = true;
                return;
            }
            timer.Stop();
            Hide();  // остановка измерения занимает до трёх секунд: окно на это время не должно висеть на экране
            core.Close();
            Native.KeepAwake(false);
            base.OnFormClosing(e);
        }

        static string Capital(string text) { return char.ToUpper(text[0]) + text.Substring(1); }

        // ----- обновление -----

        void Tick()
        {
            try { UpdateAll(); }
            catch (Exception e)
            {
                Program.LogOnce("обновление окна", e);  // повторяющаяся ошибка пишется один раз, а не дважды в секунду
                Problem = "Ошибка в программе: " + e.Message + ". Подробности записаны в data\\error.log";
            }
            // Раз в десять секунд потолки перечитываются из процессора: их мог сбросить сон или сменить другая программа.
            if (core.Limits != null && !core.Busy && ++sinceRefresh >= 20)
            {
                string unread = core.Limits.Refresh();
                // Не прочитались — повтор через секунду. Владельцу об этом говорится, если не прочитались и со второго
                // раза: тогда на плитках стоят прежние значения, а не то, что в процессоре.
                sinceRefresh = unread == null ? 0 : 18;
                limitsUnread = unread != null && unreadBefore ? unread : null;
                unreadBefore = unread != null;
            }
            ShowBanner();
        }

        void ShowBanner()
        {
            string text = null;
            bool bad = true;
            // Сначала то, что касается железа: срабатывание защиты и отказ датчиков не должны прятаться за старым отказом кнопки.
            if (core.Trip != null) text = "Нагрузка снята защитой: " + core.Trip;
            else if (core.Monitor != null && core.Monitor.Error != null) text = "Ошибка датчиков: " + core.Monitor.Error;
            else if (limitsUnread != null) text = "Потолки не перечитаны из процессора: " + limitsUnread + ". На плитках — прежние значения.";
            else if (Problem != null) text = Problem;
            else if (core.Simulated) { text = "ИМИТАЦИЯ — температуры ненастоящие, нагрузка на процессор не подаётся"; bad = false; }
            else if (core.Monitor == null) { text = "Датчики недоступны: " + core.SensorProblem + ". Нагрузка на ядра без датчиков не даётся."; bad = false; }
            banner.Visible = text != null;
            if (text == null) return;
            banner.Text = text;
            banner.BackColor = bad ? Theme.BadBg : Theme.WarnBg;
            banner.ForeColor = bad ? Theme.BadText : Theme.WarnText;
        }

        /// Обновляет всё окно: каждый раздел — своё.
        void UpdateAll()
        {
            bool busy = core.Busy;
            bool[] loaded = core.Load.Active();
            UpdateTop(loaded, busy);
            UpdateDie(loaded);
            if (core.Profile != null) UpdateProfiles(busy);
            if (core.Curve != null) UpdateLimits(busy);
            UpdateThermal(busy);
            Text = Title();
            if (busy != wasBusy)
            {
                Native.KeepAwake(busy);
                if (!busy) Native.Flash(Handle);  // измерение закончилось: значок на панели задач мигает
                wasBusy = busy;
            }
        }

        void UpdateTop(bool[] loaded, bool busy)
        {
            Monitor m = core.Monitor;
            if (m != null)
            {
                pkg.Text = double.IsNaN(m.PkgCur) ? "–" : m.PkgCur + " °C";
                pkgMax.Text = double.IsNaN(m.PkgMax) ? "" : "▲ " + m.PkgMax;
                watts.Text = double.IsNaN(m.Watts) ? "" : Math.Round(m.Watts) + " Вт";
                since.Text = Theme.Clock((DateTime.Now - m.ResetAt).TotalSeconds);
            }
            reset.Enabled = m != null;
            stopLoad.Enabled = loaded.Any(x => x) && !busy;
            level.Show(core.Load.Level, !busy);

            Calibration cal = core.Calibration;
            calibrationButton.Text = cal.Running ? "■" : "⚙";
            calibrationButton.Enabled = m != null && (cal.Running || !busy);
            calibrationStatus.Text = CalibrationStatus();
            calibrationStatus.ForeColor = cal.Running ? Theme.Text : cal.Error != null ? Theme.Bad : cal.Fitted ? Theme.Dim : Theme.Accent;
        }

        /// Заголовок окна: какое измерение идёт.
        string Title()
        {
            ThermalSweep sw = core.Sweep;
            LatencyTest lat = core.Latency;
            Calibration cal = core.Calibration;
            return sw.Running ? "CoreMap — тепловое измерение " + sw.Done + " из " + sw.Total
                : lat.Running ? "CoreMap — измерение задержек"
                : cal.Running ? "CoreMap — подбор нагрузки " + cal.Done + " из " + cal.Total
                : core.Search != null && core.Search.Running ? "CoreMap — подбор, блок " + (core.Search.BlockDone + 1) + " из " + core.Search.BlockTotal
                : core.Joint != null && core.Joint.Running ? "CoreMap — проверка под общей нагрузкой"
                : core.Curve != null && core.Curve.Running ? "CoreMap — кривая " + core.Curve.Done + " из " + core.Curve.Total : "CoreMap";
        }

        string CalibrationStatus()
        {
            Calibration cal = core.Calibration;
            if (cal.Running)
                return "подбор нагрузки · шаг " + Math.Min(cal.Done + 1, cal.Total) + " из " + cal.Total + (cal.Current != null ? " · " + cal.Current : "");
            if (cal.Error != null) return cal.Error;
            if (cal.Fitted) return string.Join(" / ", new[] { 1, 2, 3 }.Select(lv => "+" + Math.Round(cal.Heat[lv]))) + " °C";
            return core.Monitor == null || core.Simulated ? "" : "сила не подобрана — нажми ⚙";
        }

        /// Сохраняет картинку окна в файл: скрытая проверка (Scenario) смотрит на окно так.
        public void SavePicture(string path)
        {
            Tick();  // на картинке — показания на этот момент
            using (var image = new Bitmap(Width, Height))
            {
                DrawToBitmap(image, new Rectangle(0, 0, Width, Height));
                image.Save(path, ImageFormat.Png);
            }
        }
    }
}
