// Главное окно: схема кристалла с температурами, управление нагрузкой и таблицы обоих измерений.
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    class MainForm : Form, IMessageFilter
    {
        const double Shaky = 1.5;  // расхождение проходов, ± °C, с которого ячейка считается ненадёжной
        static readonly string[] LevelGlyph = { "", "▂", "▅", "█" };
        static readonly string[] LevelName = { "", "лёгкая", "средняя", "полная" };
        // Размер ячейки таблиц до масштаба Windows: наименьший, обычный (под него открывается окно) и наибольший.
        const int MinCell = 16, NormalCell = 26, MaxCell = 40;

        readonly Core core;
        readonly int n;
        readonly Timer timer = new Timer { Interval = 500 };
        readonly string snapshotPath;
        int ticks;

        Label pkg, pkgMax, watts, since, banner, sweepStatus, sweepNotes, latencyStatus, dieNote, calibrationStatus, profileStatus;
        Label curveTitle, curveStatus, sweepAxes, fitStatus, profileLabel, fitValue;
        Button curveAllButton, fitButton, profilePin, profileList, helpButton;
        // Режим вопроса: курсоры элементов, какими они были до него.
        readonly Dictionary<Control, Cursor> cursors = new Dictionary<Control, Cursor>();
        TextBox profileName;
        Slider fitSlider;        // цель подбора потолков, °C
        NumericUpDown curveTemp;  // предел, до которого греется блок при снятии кривой, °C
        string fitReport;  // итог последнего подбора потолков по температуре
        Button reset, stopLoad, sweepButton, latencyButton, calibrationButton, profileSave, profileForget, baseRestore, curveButton;
        const int FitLow = 55, FitHigh = 95;  // пределы ползунка температуры, °C
        CurveView curveView;
        BlockList blockList;
        MeanView meanView;
        Label fitMean;
        Block selected;      // выбранный (закреплённый) блок: его кривая поверх остальных, пока курсор не на другом блоке
        Block hovered;       // блок под курсором — на схеме, в списке блоков или на графике; null — нет
        string drawnKey, meanKey;  // по чему нарисованы график кривых и график среднего множителя
        Segmented fitMode, heatMode;
        Button jointButton;
        bool wasJoint;  // в прошлый опрос шёл замер под общей нагрузкой
        bool fitBest;     // ползунок делит температуру между блоками «по выгоде», а не ровной отсечкой
        bool fitApplied;  // ползунок в этом запуске уже ставил потолки: смена способа ставит их заново
        double[] meanEven = new double[0], meanBest = new double[0];
        bool[] meanRough = new bool[0];
        string[] meanReadout = new string[0];
        Block followed;                 // блок, за которым график уже перешёл во время снятия кривых
        Segmented level, secondsPick, passesPick, unitPick;
        CheckBox relative;
        DieView die;
        PairView pairView;
        Block[] blocks;       // все блоки по порядку: строки и столбцы таблиц, строки списка
        StripView ownView;
        RunBar sweepBar, latencyBar;
        FlowLayoutPanel root, topBar, body, sweepHead, profileRow, curveRow;
        FlowLayoutPanel leftTop;  // схема и потолки в ряд; под ними — таблица «Под одинаковой нагрузкой»
        Control[] leaves;         // панели окна по отдельности: по ним проверяется, что самая широкая входит в окно

        int seconds = 20, passes = 2, unit;  // unit: 0 — проценты, 1 — градусы
        int drawnSweep = -1, drawnLatency = -1;
        int headWidth;  // ширина строки переключателей теплового измерения в один ряд
        bool wasBusy, fitting, built;
        string loggedError;     // последняя ошибка окна, уже записанная в журнал
        int sinceRefresh;       // тактов с последнего перечитывания потолков из процессора
        bool sizing;            // владелец тянет границу окна: подгонка откладывается до конца
        Size sizedFrom;         // размер клиентской области в начале перетаскивания
        List<Control> flows;    // все панели-раскладчики окна: на время смены размеров их раскладка приостанавливается
        bool autoCurve;      // проверка на имитации с ключом --curve: вместо теплового измерения снимается кривая
        bool autoSweep;      // запуск с ключом --run: тепловое измерение начинается само, окно остаётся открытым
        bool autoCalibrate;  // запуск с ключом --calibrate: подбор нагрузки начинается сам, по окончании окно закрывается
        double[][] sweepShown;  // нагрев по ядрам в выбранных единицах (% или °C): [нагруженное][нагревшееся]
        double[][] sweepMean;   // то же в °C, как измерено
        public string Problem;  // ошибка программы, показывается в полосе сверху

        /// snapshotSize — размер окна для режима проверки; пустой — обычный.
        public MainForm(Core core, string snapshotPath, Size snapshotSize, bool autoCalibrate, bool autoSweep, bool autoCurve)
        {
            this.autoCurve = autoCurve;
            this.core = core;
            this.snapshotPath = snapshotPath;
            this.autoSweep = autoSweep && core.Monitor != null && snapshotPath == null;
            this.autoCalibrate = autoCalibrate && core.Monitor != null;
            n = core.Lps.Length;
            using (Graphics g = CreateGraphics()) Theme.Scale = g.DpiX / 96f;

            Text = "CoreMap";
            BackColor = Theme.Bg;
            ForeColor = Theme.Text;
            AutoScaleMode = AutoScaleMode.None;
            AutoScroll = true;  // окно меньше содержимого с наименьшими ячейками — появляется прокрутка
            MinimumSize = new Size(Theme.Px(640), Theme.Px(420));
            StartPosition = FormStartPosition.Manual;
            Icon = Icon.ExtractAssociatedIcon(Application.ExecutablePath);
            if (snapshotPath != null) ShowInTaskbar = false;

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
            Size outer = !snapshotSize.IsEmpty ? snapshotSize
                : new Size(Math.Min(want.Width + frame.Width, area.Width), Math.Min(want.Height + frame.Height, area.Height));
            // В режиме проверки окно стоит за краем экрана: на экране ничего не появляется.
            Point corner = snapshotPath != null ? new Point(-20000, -20000)
                : new Point(area.X + (area.Width - outer.Width) / 2, area.Y + (area.Height - outer.Height) / 2);
            Bounds = new Rectangle(corner, outer);
            // Окно собирается невидимым и показывается готовым: иначе почти сто его элементов рисуются на экране по одному.
            if (snapshotPath == null) Opacity = 0;

            Application.AddMessageFilter(this);
            FormClosed += (s, e) => Application.RemoveMessageFilter(this);
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

        /// В режиме проверки окно не забирает ввод у того, чем занят владелец.
        protected override bool ShowWithoutActivation { get { return snapshotPath != null; } }

        // ----- размер содержимого под размер окна -----

        protected override void OnClientSizeChanged(EventArgs e)
        {
            base.OnClientSizeChanged(e);
            // Пока тянут границу, подгонка не идёт: каждая перебирает несколько размеров ячейки с полной раскладкой окна.
            if (!sizing) Fit();
        }

        protected override void OnResizeBegin(EventArgs e)
        {
            base.OnResizeBegin(e);
            sizing = true;
            sizedFrom = ClientSize;
        }

        protected override void OnResizeEnd(EventArgs e)
        {
            base.OnResizeEnd(e);
            sizing = false;
            if (ClientSize != sizedFrom) Fit();  // событие приходит и после простого перемещения окна
        }

        /// Ставит всем таблицам и схеме один размер ячейки и подгоняет под него ширину строк и полос.
        void Apply(int cell)
        {
            // Каждое присваивание размера уходит каскадом вверх по вложенным панелям. Пока раскладка панелей
            // приостановлена, каскада нет, и всё раскладывается один раз в конце. Замер: 176 мс против 24 мс на вызов.
            // Приостанавливать вокруг всей подгонки нельзя: тогда она мерит устаревшие размеры.
            if (flows == null)
            {
                flows = new List<Control>();
                CollectFlows(root);
            }
            foreach (Control c in flows) c.SuspendLayout();
            try { ApplySizes(cell); }
            finally
            {
                for (int k = flows.Count - 1; k >= 0; k--) flows[k].ResumeLayout(true);
            }
        }

        void CollectFlows(Control control)
        {
            if (control is FlowLayoutPanel) flows.Add(control);
            foreach (Control child in control.Controls) CollectFlows(child);
        }

        void ApplySizes(int cell)
        {
            die.SetZoom((double)cell / Theme.Px(NormalCell));
            pairView.SetCell(cell * 2);
            ownView.SetCell(cell * 2, cell);
            int wide = pairView.Width;
            // Строке переключателей не хватает совсем немного — шире делается панель, а не строка в два ряда.
            if (headWidth <= wide * 11 / 10) wide = Math.Max(wide, headWidth);
            sweepHead.MaximumSize = new Size(wide, 0);
            sweepBar.Width = wide;
            SetWidth(sweepStatus, wide);
            sweepNotes.MaximumSize = new Size(wide, 0);
            sweepAxes.MaximumSize = new Size(wide, 0);
            latencyBar.Width = wide;
            SetWidth(latencyStatus, wide);
            if (dieNote != null) dieNote.MaximumSize = new Size(die.Width, 0);
            // Колонка потолков — шириной со схему, но не уже своей самой широкой строки: шрифт с ячейкой не мельчает.
            int list = blockList != null ? Theme.Px(170) + blockList.Margin.Horizontal : 0;  // список блоков справа от графика
            int side = Math.Max(die.Width, curveRow != null ? curveRow.PreferredSize.Width : 0) + list;
            if (profileStatus != null)
            {
                profileStatus.MaximumSize = new Size(side, 0);
                profileName.Width = Math.Max(Theme.Px(60), side - profileLabel.PreferredSize.Width - profileLabel.Margin.Horizontal
                    - profileName.Margin.Horizontal - profileList.PreferredSize.Width);
                profileRow.MaximumSize = new Size(side, 0);
            }
            if (curveView != null)
            {
                // Высота — чтобы двенадцать строк списка оставались читаемыми и при наименьшей ячейке.
                curveView.Size = new Size(side - list, Math.Max(cell * 8, Theme.Px(192)));
                blockList.Size = new Size(Theme.Px(170), curveView.Height);
                curveStatus.MaximumSize = new Size(side, 0);
            }
            if (fitSlider != null)
            {
                fitStatus.MaximumSize = new Size(side, 0);
                fitSlider.Width = Math.Max(Theme.Px(80), side - fitValue.PreferredSize.Width - fitSlider.Margin.Horizontal);
                meanView.Width = fitSlider.Width;
                fitMean.MaximumSize = new Size(side, 0);
            }
        }

        static void SetWidth(Label label, int width)
        {
            label.MinimumSize = Size.Empty;
            label.MaximumSize = new Size(width, 0);
            label.MinimumSize = new Size(width, 0);
        }

        /// Подбирает наибольший размер ячейки, при котором всё помещается в окно без прокрутки.
        /// Если не помещается и при наименьшем — панели переносятся одна под другую, окно прокручивается.
        void Fit()
        {
            if (!built || fitting || WindowState == FormWindowState.Minimized) return;
            fitting = true;
            try
            {
                // Появление полос прокрутки само меняет место в окне, поэтому подбор повторяется.
                for (int round = 0; round < 3; round++)
                {
                    Size room = ClientSize;
                    int inner = Math.Max(1, room.Width - root.Padding.Horizontal);
                    SuspendLayout();
                    topBar.MaximumSize = banner.MaximumSize = new Size(inner, 0);
                    body.WrapContents = leftTop.WrapContents = false;
                    body.MaximumSize = leftTop.MaximumSize = Size.Empty;
                    int best = Largest(MaxCell, () =>
                    {
                        Size need = root.GetPreferredSize(Size.Empty);
                        return need.Width <= room.Width && need.Height <= room.Height;
                    });
                    if (best == 0)
                    {
                        // Всё разом не помещается: панели переносятся, окно прокручивается. Ячейки — не крупнее
                        // обычных и такие, чтобы самая широкая панель входила по ширине.
                        best = Largest(NormalCell, () => leaves.All(p => p.GetPreferredSize(Size.Empty).Width <= inner));
                        body.WrapContents = leftTop.WrapContents = true;
                        body.MaximumSize = leftTop.MaximumSize = new Size(inner, 0);
                    }
                    Apply(best > 0 ? best : Theme.Px(MinCell));
                    ResumeLayout(true);
                    if (ClientSize == room) break;
                }
            }
            finally { fitting = false; }
        }

        /// Наибольший размер ячейки от наименьшего до top, при котором условие выполняется; 0 — ни при каком.
        int Largest(int top, Func<bool> fits)
        {
            int lo = Theme.Px(MinCell), hi = Theme.Px(top), best = 0;
            while (lo <= hi)
            {
                int mid = (lo + hi) / 2;
                Apply(mid);
                if (fits()) { best = mid; lo = mid + 1; }
                else hi = mid - 1;
            }
            return best;
        }

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
            bar.Controls.Add(level = new Segmented(LoadManager.Levels, v => LevelGlyph[v], core.SetLevel,
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
            bar.Controls.Add(helpButton = Widgets.Button("?", () => SetHelp(true),
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

        // ----- режим вопроса -----

        /// Включает и выключает режим вопроса: в нём подсказки всплывают при наведении, курсор — со знаком вопроса.
        void SetHelp(bool on)
        {
            if (Theme.Help == on) return;
            Theme.Help = on;
            Theme.Tips.Active = on;
            helpButton.ForeColor = on ? Theme.Accent : Theme.Text;
            helpButton.BackColor = on ? Theme.Cell : Theme.Button;
            if (on) HelpCursor(this);
            else
            {
                foreach (KeyValuePair<Control, Cursor> c in cursors) c.Key.Cursor = c.Value;
                cursors.Clear();
            }
        }

        void HelpCursor(Control control)
        {
            cursors[control] = control.Cursor;
            control.Cursor = Cursors.Help;
            foreach (Control child in control.Controls) HelpCursor(child);
        }

        /// В режиме вопроса первый щелчок в окне или Esc выключает режим; до элемента под курсором щелчок не доходит.
        public bool PreFilterMessage(ref Message m)
        {
            const int KeyDown = 0x100, LeftDown = 0x201, LeftDouble = 0x203, RightDown = 0x204, MiddleDown = 0x207, Escape = 0x1B;
            if (!Theme.Help) return false;
            bool click = m.Msg == LeftDown || m.Msg == LeftDouble || m.Msg == RightDown || m.Msg == MiddleDown;
            if (!click && !(m.Msg == KeyDown && (int)m.WParam == Escape)) return false;
            Control target = Control.FromChildHandle(m.HWnd);
            if (target == null || target.FindForm() != this) return false;
            SetHelp(false);
            return true;
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

        Control DiePanel()
        {
            FlowLayoutPanel head;
            FlowLayoutPanel panel = Section("Кристалл", null, out head);
            die = new DieView(core.Cores, core.Layout)
            {
                Clicked = b => { Select(b); core.ToggleLoad(b); }, Tip = TileTip, Menu = (b, at) => { Select(b); LimitMenu(b, at); },
                Hovered = b => { hovered = b; ShowCurves(); },
            };
            Select(blocks.FirstOrDefault(b => b.Kind == 'P') ?? blocks[0]);
            if (core.Limits != null)
            {
                die.Limit = core.Limits.Current;
                die.LimitFused = core.Limits.Fused;
            }
            panel.Controls.Add(die);
            if (!core.Layout.Confirmed)
            {
                dieNote = Widgets.Label(core.Layout.Note, Theme.Dim, 11);
                dieNote.Margin = new Padding(0, Theme.Px(10), 0, 0);
                panel.Controls.Add(dieNote);
            }
            return panel;
        }

        /// Колонка потолков: профили, подбор под температуру, кривая выбранного блока.
        Control LimitsPanel()
        {
            FlowLayoutPanel head;
            FlowLayoutPanel panel = Section("Потолки множителей", null, out head);
            if (core.Curve != null)
            {
                head.Controls.Add(heatMode = new Segmented(new[] { 0, 1 }, v => v == 0 ? "по средней" : "по наибольшей", v =>
                    {
                        core.Curve.ByPeak = v == 1;
                        if (fitApplied) fitReport = core.FitFromCurves(fitSlider.Value, fitBest) + BackHint();
                        Tick();
                    },
                    "По какой температуре работать\nВ каждой точке кривой хранятся обе температуры ступени; здесь выбирается, по какой считать.\n" +
                    "• по средней — средняя за ступень: то, что блок держит под нагрузкой\n" +
                    "• по наибольшей — самая высокая, мелькнувшая за ступень: мгновенные всплески\n" +
                    "• выбор действует на ползунок, на «подобрать замером» и на предел «греть до» при снятии кривых\n" +
                    "• на графике толстой линией идёт выбранная температура, тонкой — вторая"));
                foreach (Control c in head.Controls) c.Anchor = AnchorStyles.Left;
            }
            if (core.Limits != null)
            {
                Label first = Heading("Профили");
                first.Margin = new Padding(0, 0, 0, Theme.Px(6));
                panel.Controls.Add(first);
                // Поле названия и кнопка списка вместо системного поля-списка: у того стрелка чёрная на тёмном фоне.
                FlowLayoutPanel field = Widgets.Row();
                field.Margin = new Padding(0, 0, 0, Theme.Px(6));
                field.Controls.Add(profileLabel = Widgets.Label("Профиль", Theme.Dim, 13));
                profileName = new TextBox
                {
                    BackColor = Theme.Button, ForeColor = Theme.Text, BorderStyle = BorderStyle.FixedSingle, Font = Theme.Font(13),
                    Margin = new Padding(0, 0, Theme.Px(4), 0),
                };
                Theme.Tips.SetToolTip(profileName, "Название профиля\nПод этим названием «сохранить» запишет то, что стоит сейчас.\n" +
                    "• существующее название — профиль перезаписывается; новое — появляется новый профиль\n" +
                    "• открыть сохранённый профиль — кнопка ▾ справа");
                profileName.Text = core.Profile.Exists ? core.Profile.Name : "";
                field.Controls.Add(profileName);
                field.Controls.Add(profileList = Widgets.Button("▾", ProfileMenu,
                    "Открыть профиль\nЩелчок по названию ставит потолки этого профиля. База при этом не меняется.\n" +
                    "• разница открытого профиля с базой видна на плитках и в списке блоков: +N и −N\n" +
                    "• «заводские» — потолки, с которыми процессор выпущен\n" +
                    "• чтобы открытый профиль ставился при запуске программы — кнопка «сделать базой»\n" +
                    "• вернуться — кнопка «вернуть базу»"));
                profileList.Margin = Padding.Empty;
                foreach (Control c in field.Controls) c.Anchor = AnchorStyles.Left;
                panel.Controls.Add(field);

                FlowLayoutPanel row = Widgets.Row();
                row.Margin = Padding.Empty;
                row.WrapContents = true;  // в узкой панели кнопки переносятся на вторую строку
                profileRow = row;
                row.Controls.Add(profileSave = Widgets.Button("сохранить", () => Report(core.SaveProfile(profileName.Text)),
                    "Сохранить профиль\nЗаписывает то, что стоит сейчас, в профиль с названием из поля выше и делает его базой.\n" +
                    "• название базы — база перезаписывается; новое название — новый профиль\n" +
                    "• без сохранения любые изменения потолков пропадают при перезагрузке\n" +
                    "• профили хранятся в файле рядом с программой, в прошивку ноутбука ничего не пишется\n" +
                    "• база ставится заново при запуске программы\n" +
                    "• если после этого был сбой, профиль с потолками выше заводских отключается сам"));
                row.Controls.Add(profilePin = Widgets.Button("сделать базой", () => Report(core.PinStanding(profileName.Text)),
                    "Сделать базой то, что стоит сейчас\nОткрытый профиль становится базой: он ставится при запуске программы, " +
                    "от него считается разница потолков.\n" +
                    "• сами профили не меняются\n" +
                    "• если открыты заводские — при запуске программы ничего ставиться не будет\n" +
                    "• если потолки менялись после открытия — сначала «сохранить»"));
                row.Controls.Add(baseRestore = Widgets.Button("вернуть базу", () => Report(core.RestoreBase()),
                    "Вернуть базу\nСтавит потолки базы: всё, что менялось ползунком, вручную или открытием другого профиля, отменяется.\n" +
                    "• сохранённые профили при этом не меняются"));
                row.Controls.Add(profileForget = Widgets.Button("удалить", () => Report(core.DeleteProfile(profileName.Text)),
                    "Удалить профиль\nУдаляет профиль с названием из поля выше.\n" +
                    "• если он база — базой становятся заводские потолки, при запуске программы ничего ставиться не будет\n" +
                    "• потолки, которые стоят сейчас, остаются до перезагрузки"));
                foreach (Control c in row.Controls) c.Margin = new Padding(0, 0, Theme.Px(4), Theme.Px(4));
                panel.Controls.Add(row);
                panel.Controls.Add(profileStatus = Widgets.Label("", Theme.Dim, 11));
                profileStatus.Margin = new Padding(0, Theme.Px(6), 0, 0);
                if (core.Curve != null)
                {
                    panel.Controls.Add(Heading("Под температуру"));
                    panel.Controls.Add(meanView = new MeanView(FitLow, FitHigh, 75));
                    FlowLayoutPanel fit = Widgets.Row();
                    fit.Margin = new Padding(0, 0, 0, Theme.Px(2));
                    fitSlider = new Slider(FitLow, FitHigh, 75,
                        "Потолки под температуру по снятым кривым\nСтавит каждому блоку самый высокий потолок, при котором он по своей кривой " +
                        "не горячее выбранной температуры.\n" +
                        "• потолки меняются сразу, пока двигаешь; процессор при этом не нагружается\n" +
                        "• двигается мышью; колесом — после щелчка по ползунку; щелчок ставит потолки под его нынешнее значение\n" +
                        "• блок без кривой не трогается — сними кривые кнопкой ▶▶ ниже\n" +
                        "• за края кривой подбор не выходит: выше верхней и ниже нижней её точки потолок не ставится\n" +
                        "• кривые сняты при нагрузке на один блок: когда работают соседи, будет горячее\n" +
                        "• поставленные потолки — проба: пропадут при перезагрузке, пока не сохранён профиль")
                    {
                        Margin = new Padding(0, 0, Theme.Px(8), 0),
                        Moved = t => { fitValue.Text = t + " °C"; fitApplied = true; fitReport = core.FitFromCurves(t, fitBest) + BackHint(); Tick(); },
                    };
                    fit.Controls.Add(fitSlider);
                    fit.Controls.Add(fitValue = Widgets.Label("75 °C", Theme.Text, 14, true));
                    fitValue.Margin = Padding.Empty;
                    foreach (Control c in fit.Controls) c.Anchor = AnchorStyles.Left;
                    panel.Controls.Add(fit);
                    panel.Controls.Add(fitMean = Widgets.Label("", Theme.Dim, 12));
                    fitMean.Margin = new Padding(0, 0, 0, Theme.Px(8));
                    fit = Widgets.Row();
                    fit.Margin = Padding.Empty;
                    fit.Controls.Add(fitMode = new Segmented(new[] { 0, 1 }, v => v == 0 ? "ровно" : "по выгоде", v =>
                        {
                            fitBest = v == 1;
                            if (fitApplied) fitReport = core.FitFromCurves(fitSlider.Value, fitBest) + BackHint();
                            Tick();
                        },
                        "Как делить температуру между блоками\nОба способа берут потолки из снятых кривых.\n" +
                        "• ровно — каждому блоку самый высокий потолок, при котором он не горячее температуры с ползунка\n" +
                        "• по выгоде — та же средняя температура по блокам, но поделена иначе: блок, которому градус даёт много " +
                        "частоты, поднимается выше отсечки (не больше чем на " + Core.Spread + " °C), а ступени, которые стоят градусов " +
                        "и частоты не добавляют, срезаются\n" +
                        "• на графике над ползунком белая линия — «ровно», бирюзовая — «по выгоде»\n" +
                        "• расчёт по кривым одиночной нагрузки: под общей нагрузкой блоки греют друг друга, это ещё не учтено"));
                    fit.Controls.Add(fitButton = Widgets.Button("подобрать замером", () =>
                        {
                            if (core.Curve.Searching) core.Curve.Stop();
                            else fitReport = core.FitLimits(fitSlider.Value);
                            Tick();
                        },
                        "Подобрать потолки замером\nДля каждого блока под нагрузкой ищет самый высокий множитель, при котором его " +
                    "средняя температура не выше значения на ползунке, и оставляет его блоку.\n" +
                    "• кривые для этого не нужны: блок меряется заново\n" +
                    "• блоки идут по одному; нагрузка во время поиска не снимается, множитель двигается ступенями по 4 секунды\n" +
                    "• около 20–30 секунд на блок, 5–6 минут на все; повторное нажатие останавливает\n" +
                    "• берётся средняя температура, а не мгновенные всплески; сила нагрузки — та, что выбрана вверху\n" +
                    "• мерится один блок: когда работают соседи, будет горячее — предел бери с запасом\n" +
                    "• найденные потолки — проба: пропадут при перезагрузке, пока не сохранён профиль\n" +
                    "• компьютер в это время лучше не нагружать"));
                    foreach (Control c in fit.Controls) c.Anchor = AnchorStyles.Left;
                    panel.Controls.Add(fit);
                    fit = Widgets.Row();
                    fit.Margin = new Padding(0, Theme.Px(4), 0, 0);
                    fit.Controls.Add(jointButton = Widgets.Button("проверить все разом", () =>
                        {
                            if (core.Curve.Joint) core.Curve.Stop();
                            else { fitReport = null; core.StartJoint(); }
                            Tick();
                        },
                        "Проверка под общей нагрузкой\nНагружает все блоки разом при потолках, которые стоят сейчас, и меряет каждый блок.\n" +
                        "• кривые сняты по одному блоку, а в работе блоки греют друг друга: проверка показывает, насколько\n" +
                        "• 20 секунд прогрева и 20 секунд замера; сила нагрузки — та, что выбрана вверху; повторное нажатие останавливает\n" +
                        "• в списке блоков после стрелки появляется температура под общей нагрузкой\n" +
                        "• потолки не меняются; результат пишется в файл рядом с программой\n" +
                        "• если ядро дойдёт до защитного предела, нагрузка снимется сама\n" +
                        "• компьютер в это время лучше не нагружать"));
                    panel.Controls.Add(fit);
                    panel.Controls.Add(fitStatus = Widgets.Label("", Theme.Text, 11));
                    fitStatus.Margin = new Padding(0, Theme.Px(4), 0, 0);
                }
            }
            if (core.Curve != null)
            {
                panel.Controls.Add(curveTitle = Heading(""));
                FlowLayoutPanel row = curveRow = Widgets.Row();
                row.Margin = new Padding(0, 0, 0, Theme.Px(4));
                row.Controls.Add(curveButton = Widgets.Button("▶", CurveClick,
                    "Снять кривую\nИзмеряет, как температура выбранного ядра зависит от потолка множителя.\n" +
                    "• ядро выбирается щелчком по плитке; сила нагрузки — та, что выбрана вверху окна\n" +
                    "• блок E-ядер меряется целиком: нагружаются все его ядра сразу\n" +
                    "• проходит восемь потолков лесенкой вверх и обратно вниз, по 4 секунды на ступень — около минуты\n" +
                    "• нагрузка между ступенями не снимается; температура на ступени — среднее подъёма и спуска\n" +
                    "• подъём останавливается на первой ступени, где средняя температура за ступень вышла за число из поля " +
                    "«греть до»: выше блок не греется\n" +
                    "• остальные ядра не нагружаются; потолок потом возвращается к прежнему\n" +
                    "• повторное нажатие останавливает\n" +
                    "• компьютер в это время лучше не нагружать"));
                row.Controls.Add(curveAllButton = Widgets.Button("▶▶", () => { fitReport = null; core.StartAllCurves((double)curveTemp.Value); },
                    "Снять все кривые\nСнимает кривую для каждого P-ядра и каждого блока E-ядер подряд, на выбранной вверху силе нагрузки.\n" +
                    "• около 15 минут на все 12 блоков; остановить — кнопкой ■ рядом\n" +
                    "• компьютер в это время лучше не нагружать\n" +
                    "• каждый блок греется не выше числа из поля «греть до»\n" +
                    "• по кривым работает ползунок «Под температуру»"));
                row.Controls.Add(Widgets.Label("греть до", Theme.Dim, 13));
                curveTemp = new NumericUpDown
                {
                    Minimum = 55, Maximum = 95, Value = 80, Width = Theme.Px(54), Font = Theme.Font(13), BackColor = Theme.Button,
                    ForeColor = Theme.Text, BorderStyle = BorderStyle.FixedSingle, TextAlign = HorizontalAlignment.Center,
                    Margin = new Padding(0, 0, Theme.Px(4), 0),
                };
                Theme.Tips.SetToolTip(curveTemp, "Предел при снятии кривой\nКривая снимается вверх по потолкам, пока средняя температура блока " +
                    "за ступень не выйдет за это число.\n• выше блок не греется, и ползунок выше этой температуры для него ничего не знает");
                row.Controls.Add(curveTemp);
                Label degrees = Widgets.Label("°C", Theme.Dim, 13);
                degrees.Margin = Padding.Empty;
                row.Controls.Add(degrees);
                foreach (Control c in row.Controls) c.Anchor = AnchorStyles.Left;
                panel.Controls.Add(row);
                FlowLayoutPanel graph = Widgets.Row();
                graph.Margin = Padding.Empty;
                graph.Controls.Add(curveView = new CurveView
                {
                    Hovered = b => { hovered = b; ShowCurves(); },
                    Help = "Кривые «температура от множителя»\nКривые всех блоков разом; яркая — блок под курсором (в списке, на схеме или на графике).\n" +
                        "• щелчок по строке списка или по плитке закрепляет блок\n" +
                        "• по горизонтали — множитель, на котором блок работал на самом деле\n" +
                        "• пунктир — температура с ползунка, жёлтые кружки — потолки, которые стоят сейчас\n" +
                        "• у яркой кривой: толстая линия — средняя температура, тонкая — наибольшая; чёрточка у точки — разница между " +
                        "подъёмом и спуском; пустой кружок — до заданного потолка блок не дошёл\n" +
                        "• наведи на точку — её числа",
                });
                graph.Controls.Add(blockList = new BlockList(blocks)
                {
                    Margin = new Padding(Theme.Px(8), 0, 0, 0),
                    Hovered = u => { hovered = u < 0 ? null : blocks[u]; ShowCurves(); },
                    Clicked = u => { Select(blocks[u]); die.Invalidate(); ShowCurves(); },
                });
                panel.Controls.Add(graph);
                panel.Controls.Add(curveStatus = Widgets.Label("", Theme.Dim, 11));
                curveStatus.Margin = new Padding(0, Theme.Px(4), 0, 0);
            }
            return panel;
        }

        Control SweepPanel(int cell)
        {
            FlowLayoutPanel head;
            FlowLayoutPanel panel = Section("Тепловое влияние", null, out head);
            head.Controls.Add(sweepButton = Widgets.Button("▶", SweepClick,
                "Тепловое измерение\nПо очереди нагружает каждый блок (P-ядро или все четыре ядра блока E-ядер) и записывает, " +
                "насколько нагрелся он сам и все остальные.\n" +
                "• повторное нажатие останавливает измерение\n" +
                "• ручная нагрузка на это время снимается\n" +
                "• компьютер лучше не трогать: фоновые задачи искажают результат"));
            head.Controls.Add(secondsPick = new Segmented(ThermalSweep.Durations, v => v + " с", v => { seconds = v; Redraw(); },
                "Время нагрузки на одно ядро\nПервая половина — выход на ровную температуру, вторая — усреднение.\n" +
                "• дольше — меньше шума от фоновых задач"));
            head.Controls.Add(passesPick = new Segmented(ThermalSweep.PassChoices, v => v + "×", v => { passes = v; Redraw(); },
                "Число проходов\nКаждый проход нагружает все ядра заново, в другом порядке; в таблице — среднее.\n" +
                "• от двух проходов видно расхождение между ними — насколько данным можно верить\n" +
                "• время измерения растёт во столько же раз"));
            head.Controls.Add(unitPick = new Segmented(new[] { 0, 1 }, v => v == 0 ? "%" : "°C", v => { unit = v; Redraw(); },
                "Единицы таблицы\nВ чём показывать нагрев остальных ядер.\n" +
                "• % — доля от нагрева самого нагруженного ядра: строки сравнимы при разной силе нагрузки\n" +
                "• °C — как измерено"));
            relative = new CheckBox
            {
                Text = "без общего прогрева", AutoSize = true, ForeColor = Theme.Dim, Font = Theme.Font(13), FlatStyle = FlatStyle.Flat,
                Margin = new Padding(0, Theme.Px(3), 0, 0),
            };
            relative.CheckedChanged += (s, e) => Redraw();
            Theme.Tips.SetToolTip(relative, "Без общего прогрева\n" +
                "Вычитает из строки нагрев, общий для всех ненагруженных ядер, — остаётся влияние соседства.");
            head.Controls.Add(relative);
            // В узком окне переключатели переносятся на вторую строку; зазор между строками — нижние поля.
            sweepHead = head;
            head.WrapContents = true;
            head.Margin = new Padding(0, 0, 0, Theme.Px(6));
            foreach (Control c in head.Controls) c.Margin = new Padding(c.Margin.Left, c.Margin.Top, c.Margin.Right, Theme.Px(4));
            headWidth = head.GetPreferredSize(Size.Empty).Width;

            pairView = new PairView(blocks, cell * 2)
            {
                Tip = PairTip, Lower = "Задержки, нс",
            };
            panel.Controls.Add(sweepBar = new RunBar(pairView.Width));
            panel.Controls.Add(sweepStatus = StatusLabel(pairView.Width));

            FlowLayoutPanel latencyHead = Widgets.Row();
            latencyHead.Margin = new Padding(0, 0, 0, Theme.Px(6));
            latencyHead.Controls.Add(Widgets.Label("Задержки", Theme.Text, 14, true));
            latencyHead.Controls.Add(latencyButton = Widgets.Button("▶", LatencyClick,
                "Измерение задержек\nИзмеряет время передачи данных между каждой парой ядер.\n" +
                "• повторное нажатие останавливает измерение\n" +
                "• занимает около 15 секунд\n" +
                "• ручная нагрузка на это время снимается"));
            foreach (Control c in latencyHead.Controls) c.Anchor = AnchorStyles.Left;
            panel.Controls.Add(latencyHead);
            panel.Controls.Add(latencyBar = new RunBar(pairView.Width));
            panel.Controls.Add(latencyStatus = StatusLabel(pairView.Width));

            panel.Controls.Add(pairView);
            Label axes = Widgets.Label("Две таблицы, от каждой — одна половина: вторая была бы её зеркальным повтором. " +
                "Справа вверху — тепловая связь пары блоков: больший из двух нагревов соседа; на диагонали — собственный нагрев блока. " +
                "Слева внизу — задержка между блоками; на диагонали — задержка внутри блока E-ядер. Блок E-ядер идёт одной строкой.",
                Theme.Dim, 11);
            axes.Margin = new Padding(0, Theme.Px(6), 0, 0);
            panel.Controls.Add(axes);
            panel.Controls.Add(sweepNotes = Widgets.Label("", Theme.Dim, 11));
            sweepAxes = axes;

            return panel;
        }

        /// Таблица «Под одинаковой нагрузкой»: отдельная панель под схемой и потолками.
        Control OwnPanel(int cell)
        {
            FlowLayoutPanel head;
            FlowLayoutPanel panel = Section("Под одинаковой нагрузкой",
                "Под одинаковой нагрузкой\nКаждый блок по очереди получает одну и ту же лёгкую нагрузку на все свои ядра.\n" +
                "• скорость — сколько блок успевает посчитать: сумма по его ядрам\n" +
                "• °C — насколько нагрелся сам блок\n" +
                "• °C на единицу скорости — честное сравнение ядер при равной работе\n" +
                "• Вт — насколько выросло потребление всего процессора\n" +
                "• °C/Вт — нагрев на каждый ватт: чем больше, тем хуже ядро отдаёт тепло", out head);
            string speed = Kernels.ForLevel(1).Unit;
            ownView = new StripView(blocks, cell * 2, cell, new[] { speed, "°C", "°C на 1000", "Вт", "°C/Вт" }, new[] { 0, 1, 1, 1, 1 });
            panel.Controls.Add(ownView);
            return panel;
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

        void SweepClick()
        {
            if (core.Sweep.Running) core.Sweep.Stop();
            else core.StartSweep(seconds, passes);
        }

        void LatencyClick()
        {
            if (core.Latency.Running) core.Latency.Stop();
            else core.StartLatency();
        }

        /// Меню потолка множителя для блока: от нескольких ступеней выше заводского предела (CoreLimits.Over) вниз.
        void LimitMenu(Block block, Point at)
        {
            CoreLimits limits = core.Limits;
            int i = block.First;  // потолок блока читается и ставится через первое ядро
            if (limits == null || !limits.Known(i)) return;
            var menu = new ContextMenuStrip { ShowImageMargin = false, ShowCheckMargin = true, Font = Theme.Font(13) };
            menu.Items.Add(new ToolStripLabel("Потолок множителя: " + (block.Kind == 'E' ? "кластер ядра " : "ядро ") + block.Lp));
            menu.Items.Add(new ToolStripSeparator());
            int[] baseline = core.BaseLimits;
            foreach (int ratio in limits.Choices(i))
            {
                int value = ratio;
                string text = "×" + value + (value == limits.Fused[i] ? "  заводской предел" : value > limits.Fused[i] ? "  выше заводского" : "") +
                    (value == baseline[i] && value != limits.Fused[i] ? "  база" : "");
                var item = new ToolStripMenuItem(text) { Checked = value == limits.Current[i], Enabled = !core.Busy };
                item.Click += (s, e) =>
                {
                    string refusal = core.SetLimit(i, value);
                    Problem = refusal == null ? null : "Потолок не изменён: " + refusal;
                    Tick();
                };
                menu.Items.Add(item);
            }
            menu.Closed += (s, e) => BeginInvoke((Action)menu.Dispose);
            menu.Show(die, at);
        }

        void Select(Block block)
        {
            selected = block;
            if (die != null) die.Selected = block;
            drawnKey = null;
        }

        void CurveClick()
        {
            fitReport = null;
            if (core.Curve.Running) core.Curve.Stop();
            else core.StartCurve(selected, (double)curveTemp.Value);
        }

        /// Обновляет заголовок и график кривых, если изменилось то, от чего они зависят. Наведение мыши на блок
        /// вызывает только это, а не обновление всего окна.
        void ShowCurves()
        {
            if (core.Curve == null || curveView == null || fitSlider == null) return;
            CurveTest curve = core.Curve;
            Block shown = hovered ?? selected;  // блок, чья кривая поверх остальных
            curveTitle.Text = "Кривая · " + shown.Title;
            string key = curve.Version + "/" + shown.Index + "/" + selected.Index + "/" + fitSlider.Value + "/" + core.Load.Level + "/" + fitBest + "/" + curve.ByPeak +
                "/" + string.Join(",", core.Limits.Current) + "/" + string.Join(",", core.BaseLimits);
            if (drawnKey == key) return;
            drawnKey = key;
            DrawCurves(shown);
        }

        /// Рисует график кривых, список блоков и средний множитель для температуры с ползунка.
        void DrawCurves(Block shown)
        {
            CurveTest curve = core.Curve;
            int level = core.Load.Level, target = fitSlider.Value;
            CurveChoice[] picks = core.ChooseAll(target, fitBest);
            // График и список показывают то, что стоит в процессоре; выбор ползунка в них попадает, когда ползунок
            // его поставил. От положения ползунка, пока он ничего не ставил, зависят только отсечка и средний множитель.
            CurveChoice[] standing = Standing(picks);
            curveView.Show(curve.For(shown), curve.All(level).Where(r => r.Lp != shown.Lp).ToArray(),
                           core.Blocks, target, standing, curve.ByPeak, "Кривых ещё нет — нажми ▶▶, чтобы снять все");
            blockList.Show(standing, selected.Index, shown.Index, JointTemps(), Delta());

            double[] now = Means(picks);
            double even = fitBest ? Means(core.ChooseAll(target))[0] : double.NaN;
            fitMean.Text = double.IsNaN(now[0]) ? "" : "средний множитель ×" + now[0].ToString("0.00") +
                (double.IsNaN(even) ? "" : " (ровно ×" + even.ToString("0.00") + ")") +
                (double.IsNaN(now[1]) ? "" : " · P ×" + now[1].ToString("0.0")) + (double.IsNaN(now[2]) ? "" : " · E ×" + now[2].ToString("0.0"));
            meanView.Value = target;
            // Кривая среднего множителя пересчитывается по готовым кривым: пока идёт измерение, она не трогается
            // (иначе полный пересчёт шёл бы на каждой ступени) и обновляется, когда оно закончилось.
            string meanNow = curve.Version + "/" + level + "/" + curve.ByPeak;
            if (meanKey != meanNow && !(curve.Running && meanEven.Length > 0 && meanKey != null && meanKey.EndsWith("/" + level + "/" + curve.ByPeak)))
            {
                meanKey = meanNow;
                int count = FitHigh - FitLow + 1;
                meanEven = new double[count];
                meanBest = new double[count];
                meanRough = new bool[count];
                meanReadout = new string[count];
                for (int k = 0; k < count; k++)
                {
                    CurveChoice[] at = core.ChooseAll(FitLow + k);
                    double[] m = Means(at);
                    meanEven[k] = m[0];
                    meanBest[k] = Means(core.ChooseAll(FitLow + k, true))[0];
                    meanRough[k] = at.Any(c => c != null && c.Hot);
                    if (double.IsNaN(m[0])) continue;
                    meanReadout[k] = (FitLow + k) + " °C · средний множитель ×" + m[0].ToString("0.00") +
                        "\n• по выгоде: ×" + meanBest[k].ToString("0.00") +
                        (double.IsNaN(m[1]) ? "" : "\n• P-ядра: ×" + m[1].ToString("0.00")) +
                        (double.IsNaN(m[2]) ? "" : "\n• блоки E-ядер: ×" + m[2].ToString("0.00")) +
                        "\n• блоков с кривой: " + at.Count(c => c != null) + " из " + blocks.Length +
                        (meanRough[k] ? "\n• часть блоков горячее этой температуры уже на нижней точке кривой: среднее завышено" : "");
                }
            }
            meanView.Show(meanEven, meanBest, fitBest, meanRough, meanReadout, Knee(meanEven, meanRough));
        }

        /// На сколько ступеней потолок каждого блока, стоящий сейчас, отличается от базы; 0 — не отличается или неизвестен.
        int[] Delta()
        {
            int[] now = core.Limits.Current, baseline = core.BaseLimits;
            return blocks.Select(b => now[b.First] <= 0 || baseline[b.First] <= 0 ? 0 : now[b.First] - baseline[b.First]).ToArray();
        }

        /// Что кривые говорят о потолке, который стоит у каждого блока сейчас. Если он совпал с выбором ползунка,
        /// берётся этот выбор; иначе — точка кривой, снятая при таком потолке. Кривой нет или потолок вне её —
        /// запись без температуры; null — потолок блока неизвестен.
        CurveChoice[] Standing(CurveChoice[] picks)
        {
            int level = core.Load.Level;
            return blocks.Select(b =>
            {
                if (!core.Limits.Known(b.First)) return null;
                int now = core.Limits.Current[b.First];
                if (picks[b.Index] != null && picks[b.Index].Limit == now) return picks[b.Index];
                CurveChoice[] options = core.Curve.Options(b, level) ?? new CurveChoice[0];
                CurveChoice at = options.FirstOrDefault(o => o.From == now) ?? options.LastOrDefault(o => o.Limit == now);
                // В записи кривой потолок может стоять ниже снятого (блок до него не дошёл); в списке нужен тот, что стоит.
                return at == null ? new CurveChoice { Limit = now, From = now, Temp = double.NaN, Ratio = double.NaN }
                    : new CurveChoice { Limit = now, From = at.From, Temp = at.Temp, Ratio = at.Ratio };
            }).ToArray();
        }

        /// Приписка к итогу ползунка: как вернуть потолки, которые стояли до него.
        string BackHint()
        {
            return " Отменить — кнопка «вернуть базу».";
        }

        /// Температура каждого блока в последнем замере под общей нагрузкой; NaN — замера нет или потолки с тех пор менялись.
        double[] JointTemps()
        {
            JointResult joint = core.Curve.JointLast;
            bool valid = joint != null && joint.Level == core.Load.Level && joint.Limits.SequenceEqual(core.Limits.Current);
            return blocks.Select(b => valid ? b.Temp(core.Curve.ByPeak ? joint.Peak : joint.Temp) : double.NaN).ToArray();
        }

        /// Итог замера под общей нагрузкой одной строкой: насколько блоки горячее, чем по своим кривым.
        string JointSummary()
        {
            JointResult joint = core.Curve.JointLast;
            if (joint == null) return null;
            double[] heat = core.Curve.ByPeak ? joint.Peak : joint.Temp;
            var above = new List<double>();
            var ratios = new List<double>();
            Block hottest = blocks[0];
            foreach (Block b in blocks)
            {
                if (b.Temp(heat) > hottest.Temp(heat)) hottest = b;
                double ratio = b.Ratio(joint.Ratio);
                if (!double.IsNaN(ratio)) ratios.Add(ratio);
                double alone = core.CurveTemp(b);
                if (!double.IsNaN(alone)) above.Add(b.Temp(heat) - alone);
            }
            return "Под общей нагрузкой: самый горячий блок " + hottest.Name + " — " + hottest.Temp(heat).ToString("0") + " °C" +
                (above.Count > 0 ? "; в среднем на " + above.Average().ToString("+0.0;−0.0") + " °C к своим кривым (от " + above.Min().ToString("+0;−0") +
                    " до " + above.Max().ToString("+0;−0") + ")" : "") +
                (ratios.Count > 0 ? "; средний множитель ×" + ratios.Average().ToString("0.00") : "") +
                "; прибавка мощности " + joint.Watts.ToString("0") + " Вт. По блокам — в списке после стрелки.";
        }

        /// Средний фактический множитель по блокам с кривой: по всем, по P-ядрам и по блокам E-ядер; NaN — таких нет.
        double[] Means(CurveChoice[] picks)
        {
            var all = new List<double>();
            var p = new List<double>();
            var e = new List<double>();
            for (int u = 0; u < blocks.Length; u++)
            {
                if (picks[u] == null) continue;
                all.Add(picks[u].Ratio);
                (blocks[u].Kind == 'P' ? p : e).Add(picks[u].Ratio);
            }
            return new[] { all, p, e }.Select(v => v.Count > 0 ? v.Average() : double.NaN).ToArray();
        }

        /// Перегиб кривой «средний множитель от температуры»: точка, дальше всего отстоящая вверх от прямой между
        /// началом достоверной части и местом, где кривые кончаются. Правее него градус даёт меньше множителя, чем левее.
        /// Возвращает температуру; -1 — перегиба нет.
        static int Knee(double[] mean, bool[] rough)
        {
            int a = -1, b = -1;
            for (int k = 0; k < mean.Length; k++)
            {
                if (double.IsNaN(mean[k])) continue;
                if (a < 0 && !rough[k]) a = k;
                if (k > 0 && mean[k] != mean[k - 1]) b = k;
            }
            if (a < 0 || b - a < 4) return -1;
            int best = -1;
            double far = 0.05;
            for (int k = a + 1; k < b; k++)
            {
                double above = mean[k] - (mean[a] + (mean[b] - mean[a]) * (k - a) / (b - a));
                if (above > far) { far = above; best = k; }
            }
            return best < 0 ? -1 : FitLow + best;
        }

        /// Список под кнопкой ▾: заводские потолки и сохранённые профили. Щелчок открывает набор — ставит его
        /// потолки; база при этом прежняя. Пометки словами говорят, что база и что стоит сейчас.
        void ProfileMenu()
        {
            var menu = new ContextMenuStrip { ShowImageMargin = false, ShowCheckMargin = false, Font = Theme.Font(13) };
            Profile profile = core.Profile;
            string standing = core.Standing(profileName.Text);
            // "" — заводские потолки: у них нет названия, и в поле названия они ничего не вписывают.
            foreach (string name in new[] { "" }.Concat(core.Profiles.Names))
            {
                string picked = name;
                bool isBase = picked == "" ? !profile.Exists : profile.Exists && profile.Name == picked;
                string text = (picked == "" ? "заводские" : picked) + (isBase ? "  · база" : "") +
                    (isBase && profile.Disabled ? " (отключена после сбоя)" : "") + (standing == picked ? "  · стоит сейчас" : "");
                var item = new ToolStripMenuItem(text) { Enabled = !core.Busy };
                item.Click += (s, e) =>
                {
                    string refusal = core.OpenProfile(picked);
                    if (refusal == null) { profileName.Text = picked; fitReport = null; }
                    Report(refusal);
                };
                menu.Items.Add(item);
                if (picked == "" && core.Profiles.Names.Length > 0) menu.Items.Add(new ToolStripSeparator());
            }
            menu.Closed += (s, e) => BeginInvoke((Action)menu.Dispose);
            menu.Show(profileList, new Point(0, profileList.Height));
        }

        /// Строка состояния профилей: что база и что стоит в процессоре сейчас. attention — стоит не база.
        string ProfileStatus(bool busy, out bool attention)
        {
            Profile profile = core.Profile;
            attention = false;
            // Всегда две короткие строки: высота надписи не меняется, и окно под ней не прыгает.
            string text = profile.Describe() + "\n";
            if (busy) return text + "Сейчас — измерение: потолки временные";
            int differ = Delta().Count(d => d != 0);
            if (differ == 0) return text + "Сейчас — база";
            attention = true;
            string standing = core.Standing(profileName.Text);
            return text + "Сейчас — " + (standing == null ? "не сохранено" : standing == "" ? "заводские" : "«" + standing + "»") +
                ": разница с базой у " + differ + " из " + blocks.Length + " блоков";
        }

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

        void Redraw()
        {
            drawnSweep = -1;
            Tick();
        }

        protected override void OnFormClosing(FormClosingEventArgs e)
        {
            if (core.Busy && snapshotPath == null && MessageBox.Show(this,
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

        // ----- подсказки по ячейкам -----

        string TileTip(Block block)
        {
            int i = block.First;  // числа подсказки — по первому ядру блока
            string text = block.Members.Length > 1
                ? Capital(block.Title) + "\n" +
                    (block.Members.Any(k => die.Loaded[k]) ? "Щелчок снимает нагрузку со всего блока." : "Щелчок даёт нагрузку на все ядра блока разом.")
                : Capital(block.Title) + " (" + block.Kind + ")\n" +
                    (die.Loaded[i] ? "Щелчок снимает нагрузку с этого ядра." : "Щелчок даёт нагрузку на это ядро.");
            if (!double.IsNaN(die.Cur[i])) text += "\n• сейчас: " + die.Cur[i] + " °C\n• ▲ максимум с последнего сброса: " + die.Max[i] + " °C";
            if (!double.IsNaN(core.LoadAverage[i]))
                text += "\n• ⌀ средняя под нагрузкой: " + core.LoadAverage[i].ToString("0.0") + " °C — считается, только пока это ядро само " +
                    "нагружено, с момента, когда последний раз менялись нагруженные ядра, сила нагрузки или потолки; без нагрузки стоит на месте";
            if (die.Throttled[i]) text += "\n• сейчас сбрасывает частоту из-за перегрева";
            if (core.Monitor != null && !double.IsNaN(core.Monitor.Ratio[i]))
                text += "\n• сейчас работает на множителе " + core.Monitor.Ratio[i].ToString("0.0") +
                    (double.IsNaN(core.LoadRatio[i]) ? "" : ", в среднем под нагрузкой — " + core.LoadRatio[i].ToString("0.0"));
            if (core.Limits != null && core.Limits.Known(i))
                text += "\n• потолок множителя: ×" + core.Limits.Current[i] + ", заводской предел ×" + core.Limits.Fused[i] +
                    (core.Limits.Current[i] == core.BaseLimits[i] ? "" : "\n• в базе ×" + core.BaseLimits[i] + ": число после потолка на плитке — разница с базой") +
                    "\n• правая кнопка — изменить потолок" + (block.Kind == 'E' ? " (общий для кластера)" : "") +
                    "\n• потолок выше заводского показан красным; частоту ограничивает ещё таблица «по числу работающих ядер»";
            if (core.Busy) text += "\n• идёт измерение: ручная нагрузка недоступна";
            return text;
        }

        /// Наибольшее из значений values[из][в] по ядрам двух блоков; NaN — замеров нет.
        static double Strongest(double[][] values, int[] from, int[] to)
        {
            double top = double.NaN;
            if (values != null)
                foreach (int a in from)
                    if (values[a] != null)
                        foreach (int b in to)
                            if (!double.IsNaN(values[a][b]) && !(top >= values[a][b])) top = values[a][b];
            return top;
        }

        static string Capital(string text) { return char.ToUpper(text[0]) + text.Substring(1); }

        string PairTip(int r, int c, bool lower)
        {
            string suffix = unit == 0 ? " %" : " °C";
            if (r == c)
            {
                string text = Capital(blocks[r].Title);
                if (!lower)
                    foreach (int a in blocks[r].Members)
                        if (sweepMean != null && sweepMean[a] != null)
                            text += "\n• под нагрузкой на ядро " + core.Lps[a] + " нагрев +" + sweepMean[a][a].ToString("0.0") + " °C" +
                                (core.Sweep.Levels[a] > 0 ? ", сила " + LevelName[core.Sweep.Levels[a]] : "");
                if (lower && blocks[r].Members.Length > 1)
                {
                    double[] inside = blocks[r].Members.SelectMany(a => blocks[r].Members.Where(b => b != a).Select(b => core.Latency.Matrix[a][b]))
                        .Where(v => !double.IsNaN(v)).ToArray();
                    if (inside.Length > 0) text += "\n• задержка между ядрами блока: " + inside.Average().ToString("0.0") + " нс";
                }
                return text;
            }
            if (!lower)
            {
                var lines = new System.Collections.Generic.List<string>();
                foreach (int[] way in new[] { new[] { r, c }, new[] { c, r } })
                    foreach (int a in blocks[way[0]].Members)
                    {
                        double v = Strongest(sweepShown, new[] { a }, blocks[way[1]].Members);
                        if (!double.IsNaN(v)) lines.Add("• нагрузка на ядро " + core.Lps[a] + " → " + blocks[way[1]].Title + ": " + v.ToString("0.0") + suffix);
                    }
                if (lines.Count == 0) return null;
                return "Тепловая связь: " + blocks[r].Title + " и " + blocks[c].Title + "\nВ ячейке — наибольшее из значений ниже" +
                    (unit == 0 ? "; проценты — от собственного нагрева нагруженного ядра." : ".") + "\n" + string.Join("\n", lines);
            }
            LatencyTest lat = core.Latency;
            double[] mean = blocks[r].Members.SelectMany(a => blocks[c].Members.Select(b => lat.Matrix[a][b])).Where(v => !double.IsNaN(v)).ToArray();
            if (mean.Length == 0) return null;
            double lo = blocks[r].Members.SelectMany(a => blocks[c].Members.Select(b => lat.Lowest[a][b])).Where(v => !double.IsNaN(v)).Min();
            double hi = blocks[r].Members.SelectMany(a => blocks[c].Members.Select(b => lat.Highest[a][b])).Where(v => !double.IsNaN(v)).Max();
            return "Задержка: " + blocks[r].Title + " и " + blocks[c].Title + "\n• в среднем " + mean.Average().ToString("0.0") + " нс" +
                "\n• от " + lo.ToString("0.0") + " до " + hi.ToString("0.0") + " нс для разных пар ядер и участков памяти";
        }

        // ----- обновление -----

        void Tick()
        {
            try { UpdateAll(); }
            catch (Exception e)
            {
                // Повторяющаяся ошибка пишется в журнал один раз, а не дважды в секунду.
                if (e.Message != loggedError) Program.Log(e.ToString());
                loggedError = e.Message;
                Problem = "Ошибка в программе: " + e.Message + ". Подробности записаны в data\\error.log";
            }
            // Раз в десять секунд потолки перечитываются из процессора: их мог сбросить сон или сменить другая программа.
            if (core.Limits != null && !core.Busy && ++sinceRefresh >= 20)
            {
                sinceRefresh = 0;
                core.Limits.Refresh();
            }
            ShowBanner();
            if (autoSweep)
            {
                if (++ticks == 2) { core.StartSweep(seconds, passes); autoSweep = false; }
                return;
            }
            if (autoCalibrate)
            {
                if (++ticks == 2) core.StartCalibration();
                else if (ticks > 2 && !core.Calibration.Running) Close();
                return;
            }
            if (snapshotPath == null) return;
            // Режим проверки: запустить измерение, дать ему дойти до первых результатов, снять картинку.
            // В имитации хватает 30 секунд; на настоящих датчиках — до трёх измеренных ядер, но не дольше 4 минут.
            if (++ticks == 2)
            {
                if (Program.TestProfile)
                {
                    // Два профиля, второй — база, первый открыт поверх неё: на картинке видны пометки и разница с базой.
                    int a = blocks[0].First, b = blocks[2].First, c = blocks[5].First;
                    int top = core.Limits.Current[a];
                    core.SetLimit(a, top - 3);
                    core.SetLimit(b, core.Limits.Current[b] - 2);
                    core.SaveProfile("Тихий");
                    core.SetLimit(a, top);
                    core.SetLimit(c, core.Limits.Current[c] + 1);
                    core.SaveProfile("Игровой");
                    Report(core.OpenProfile("Тихий"));
                    profileName.Text = "Тихий";
                }
                else if (Program.TestSlide > 0) { fitBest = Program.TestBest; fitSlider.Value = Program.TestSlide; fitSlider.Moved(Program.TestSlide); if (Program.TestJoint) core.StartJoint(); }
                else if (Program.TestJoint) core.StartJoint();
                else if (Program.TestFit) core.FitLimits(47);
                else if (autoCurve) core.StartCurve(selected, double.NaN);
                else core.StartSweep(10, 1);
            }
            // Счёт ядер до запуска — от прошлой сохранённой таблицы, поэтому первые 10 секунд он не в счёт.
            if (core.Simulated ? ticks == 60 : ticks >= 20 && core.Sweep.Done >= 3 || ticks >= 480) SaveSnapshot();
        }

        void ShowBanner()
        {
            string text = null;
            bool bad = true;
            // Сначала то, что касается железа: срабатывание защиты и отказ датчиков не должны прятаться за старым отказом кнопки.
            if (core.Trip != null) text = "Нагрузка снята защитой: " + core.Trip;
            else if (core.Monitor != null && core.Monitor.Error != null) text = "Ошибка датчиков: " + core.Monitor.Error;
            else if (Problem != null) text = Problem;
            else if (core.Simulated) { text = "ИМИТАЦИЯ — температуры ненастоящие, нагрузка на процессор не подаётся"; bad = false; }
            else if (core.Monitor == null) { text = "Датчики недоступны: " + core.SensorProblem + ". Нагрузка на ядра без датчиков не даётся."; bad = false; }
            banner.Visible = text != null;
            if (text == null) return;
            banner.Text = text;
            banner.BackColor = bad ? Theme.BadBg : Theme.WarnBg;
            banner.ForeColor = bad ? Theme.BadText : Theme.WarnText;
        }

        void UpdateAll()
        {
            Monitor m = core.Monitor;
            ThermalSweep sw = core.Sweep;
            LatencyTest lat = core.Latency;
            bool busy = core.Busy;
            bool[] loaded = core.Load.Active();

            if (m != null)
            {
                pkg.Text = double.IsNaN(m.PkgCur) ? "–" : m.PkgCur + " °C";
                pkgMax.Text = double.IsNaN(m.PkgMax) ? "" : "▲ " + m.PkgMax;
                watts.Text = double.IsNaN(m.Watts) ? "" : Math.Round(m.Watts) + " Вт";
                since.Text = Theme.Clock((DateTime.Now - m.ResetAt).TotalSeconds);
                die.Cur = m.Cur;
                die.Max = m.Max;
                die.Ratio = m.Ratio;
                die.Throttled = m.Throttled;
            }
            die.Loaded = loaded;
            die.Avg = core.LoadAverage;
            die.Invalidate();
            reset.Enabled = m != null;
            stopLoad.Enabled = loaded.Any(x => x) && !busy;
            level.Show(core.Load.Level, !busy);

            if (core.Profile != null)
            {
                Profile profile = core.Profile;
                // Во время измерения потолки временные: их меняет само измерение, сохранять их и менять базу нельзя.
                bool attention;
                string status = ProfileStatus(busy, out attention);
                profileSave.Enabled = profileName.Text.Trim() != "" && !busy;
                profilePin.Enabled = core.CanPin(profileName.Text);
                profileForget.Enabled = core.Profiles.Get(profileName.Text) != null;
                baseRestore.Enabled = attention && !busy;
                if (profileStatus.Text != status) profileStatus.Text = status;
                string details = profile.Details() ?? "";
                if (Theme.Readout.GetToolTip(profileStatus) != details) Theme.Readout.SetToolTip(profileStatus, details);
                profileStatus.ForeColor = profile.Disabled ? Theme.Bad : attention ? Theme.Accent : Theme.Dim;
                die.LimitBase = core.BaseLimits;
            }

            if (core.Curve != null)
            {
                CurveTest curve = core.Curve;
                // График идёт за блоком, который меряется сейчас. Переход — один раз на блок: если владелец
                // во время замера щёлкнул другую плитку, её кривая остаётся на виду до следующего блока.
                Block active = curve.Active;
                if (active != followed)
                {
                    followed = active;
                    if (active != null) Select(active);
                }
                curveButton.Text = curve.Running ? "■" : "▶";
                curveButton.Enabled = curve.Running || !busy;
                curveAllButton.Enabled = !busy;
                fitButton.Enabled = curve.Searching || !busy;
                fitButton.Text = curve.Searching ? "■ стоп" : "подобрать замером";
                fitSlider.Enabled = !busy;
                jointButton.Enabled = curve.Joint || !busy;
                jointButton.Text = curve.Joint ? "■ стоп" : "проверить все разом";
                if (wasJoint && !curve.Joint) fitReport = curve.Error ?? JointSummary();
                wasJoint = curve.Joint;
                fitMode.Show(fitBest ? 1 : 0, !busy);
                heatMode.Show(curve.ByPeak ? 1 : 0, !busy);
                curveStatus.Text = curve.Joint ? "Идёт проверка под общей нагрузкой: нагружены все блоки разом."
                    : curve.Searching ? "Идёт подбор потолков по температуре — пройденные ступени добавляются на кривые."
                    : curve.Running
                    ? (curve.BlockTotal > 1 ? "Снимаю кривые · блок " + Math.Min(curve.BlockDone + 1, curve.BlockTotal) + " из " + curve.BlockTotal + " · шаг "
                        : "Снимаю кривую · шаг ") + Math.Min(curve.Done + 1, curve.Total) + " из " + curve.Total +
                        (curve.Current != null ? " · " + curve.Current : "")
                    : curve.Error ?? "";
                curveStatus.Visible = curveStatus.Text != "";
                curveStatus.ForeColor = curve.Running ? Theme.Text : curve.Error != null ? Theme.Bad : Theme.Dim;
                fitStatus.Text = curve.Joint
                    ? "Все блоки под нагрузкой · " + (curve.Current ?? "") + " · " + Math.Min(curve.Done, curve.Total) + " с из " + curve.Total
                    : curve.Searching
                    ? "Подбираю · блок " + Math.Min(curve.BlockDone + 1, curve.BlockTotal) + " из " + curve.BlockTotal +
                        (curve.Current != null ? " · " + curve.Current : "")
                    : fitReport ?? curve.Report ?? "Ползунок ставит потолки по снятым кривым, кнопка — замером под нагрузкой.";
                ShowCurves();
            }

            Calibration cal = core.Calibration;
            calibrationButton.Text = cal.Running ? "■" : "⚙";
            calibrationButton.Enabled = m != null && (cal.Running || !busy);
            calibrationStatus.Text = CalibrationStatus();
            calibrationStatus.ForeColor = cal.Running ? Theme.Text : cal.Error != null ? Theme.Bad : cal.Fitted ? Theme.Dim : Theme.Accent;
            ownView.SetUnit(0, Kernels.ForLevel(1).Unit);

            sweepButton.Text = sw.Running ? "■" : "▶";
            sweepButton.Enabled = !lat.Running && !cal.Running && m != null;
            latencyButton.Text = lat.Running ? "■" : "▶";
            latencyButton.Enabled = !sw.Running && !cal.Running;
            secondsPick.Show(seconds, !busy);
            passesPick.Show(passes, !busy);
            unitPick.Show(unit);

            sweepBar.Set(sw.Running && sw.Total > 0 ? (double)sw.Done / sw.Total : 0);
            sweepStatus.Text = SweepStatus();
            sweepStatus.ForeColor = sw.Running ? Theme.Text : Theme.Dim;
            latencyBar.Set(lat.Running ? (double)lat.Done / lat.Total : 0);
            latencyStatus.Text = LatencyStatus();
            latencyStatus.ForeColor = lat.Running ? Theme.Text : lat.Error != null ? Theme.Bad : Theme.Dim;

            if (drawnSweep != sw.Version) { drawnSweep = sw.Version; DrawSweep(); }
            if (drawnLatency != lat.Version) { drawnLatency = lat.Version; DrawLatency(); }

            Text = sw.Running ? "CoreMap — тепловое измерение " + sw.Done + " из " + sw.Total
                : lat.Running ? "CoreMap — измерение задержек"
                : cal.Running ? "CoreMap — подбор нагрузки " + cal.Done + " из " + cal.Total
                : core.Curve != null && core.Curve.Searching ? "CoreMap — подбор, блок " + (core.Curve.BlockDone + 1) + " из " + core.Curve.BlockTotal
                : core.Curve != null && core.Curve.Running ? "CoreMap — кривая " + core.Curve.Done + " из " + core.Curve.Total : "CoreMap";
            if (busy != wasBusy)
            {
                Native.KeepAwake(busy);
                if (!busy) Native.Flash(Handle);  // измерение закончилось: значок на панели задач мигает
                wasBusy = busy;
            }
        }

        string SweepStatus()
        {
            ThermalSweep sw = core.Sweep;
            if (core.Monitor == null) return "Без датчиков тепловое измерение невозможно.";
            if (sw.Running)
            {
                Block now = sw.Current;
                string text = "Идёт измерение · " + (now == null ? "подготовка: жду ровной температуры" : now.Title) +
                    " · готово " + sw.Done + " из " + sw.Total + " · прошло " + Theme.Clock(sw.Elapsed);
                if (sw.Done > 0)
                    text += " · осталось около " + Math.Max(1, Math.Round(sw.Elapsed / sw.Done * (sw.Total - sw.Done) / 60)) + " мин";
                return text;
            }
            if (sw.Finished != null)
                return "Измерение " + (sw.Done >= sw.Total ? "закончено " : "остановлено ") + sw.Finished + " · готово " + sw.Done + " из " + sw.Total;
            return "Измерений ещё не было — нажми ▶";
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

        string LatencyStatus()
        {
            LatencyTest lat = core.Latency;
            if (lat.Running) return "Идёт измерение · готово " + lat.Done + " из " + lat.Total + " пар";
            if (lat.Error != null) return "Измерение не удалось " + lat.Finished + ": " + lat.Error;
            if (lat.Finished != null) return "Измерение закончено " + lat.Finished + " · " + lat.Done + " из " + lat.Total + " пар";
            return "Измерений ещё не было — нажми ▶";
        }

        void DrawSweep()
        {
            ThermalSweep sw = core.Sweep;
            double[][] mean, spread;
            sw.Combined(out mean, out spread);
            bool percent = unit == 0;
            var rows = new double[n][];
            for (int i = 0; i < n; i++)
            {
                if (mean[i] == null) continue;
                double common = 0;
                if (relative.Checked)
                {
                    double[] others = mean[i].Where((v, j) => j != i && !double.IsNaN(v)).OrderBy(v => v).ToArray();
                    if (others.Length > 0) common = others[others.Length / 2];
                }
                // В процентах — доля от нагрева самого нагруженного ядра: строки сравнимы при разной силе нагрузки.
                double own = mean[i][i] - common;
                double scale = percent && own > 0 ? 100 / own : 1;
                rows[i] = mean[i].Select(v => (v - common) * scale).ToArray();
            }
            sweepShown = rows;
            sweepMean = mean;
            // Над диагональю: связь пары блоков — больший из нагревов соседа в обе стороны.
            int count = blocks.Length;
            var pair = new double[count, count];
            double top = 1;
            for (int a = 0; a < count; a++)
                for (int b = a + 1; b < count; b++)
                {
                    double there = Strongest(rows, blocks[a].Members, blocks[b].Members), back = Strongest(rows, blocks[b].Members, blocks[a].Members);
                    pair[a, b] = double.IsNaN(there) ? back : double.IsNaN(back) ? there : Math.Max(there, back);
                    if (pair[a, b] > top) top = pair[a, b];
                }
            for (int a = 0; a < count; a++)
            {
                pairView.Bad[a] = blocks[a].Members.Any(i => sw.Throttled[i]);
                // На диагонали — собственный нагрев блока под нагрузкой, °C (у блока E-ядер — наибольший из четырёх).
                double own = double.NaN;
                foreach (int i in blocks[a].Members)
                    if (mean[i] != null && !double.IsNaN(mean[i][i]) && !(own >= mean[i][i])) own = mean[i][i];
                pairView.UpperOwn[a] = double.IsNaN(own) ? null
                    : new PairView.CellData { Text = "+" + Math.Round(own) + "°", Fill = Theme.Button, Own = true };
                for (int b = a + 1; b < count; b++)
                {
                    double v = pair[a, b];
                    pairView.Cells[a, b] = double.IsNaN(v) ? null : new PairView.CellData
                    {
                        Text = v.ToString(percent || Math.Abs(v) >= 10 ? "0" : "0.0"), Fill = Theme.Heat(v / top),
                        Shaky = Strongest(spread, blocks[a].Members, blocks[b].Members) >= Shaky || Strongest(spread, blocks[b].Members, blocks[a].Members) >= Shaky,
                    };
                }
            }
            pairView.Upper = "Тепловая связь, " + (percent ? "%" : "°C");
            pairView.Invalidate();

            string notes = "";
            double[] diffs = spread.Where(r => r != null).SelectMany(r => r).Where(v => !double.IsNaN(v)).OrderBy(v => v).ToArray();
            if (diffs.Length > 0)
                notes += "расхождение проходов: обычно ±" + diffs[diffs.Length / 2] + " °C, худшая ячейка ±" + diffs.Last() +
                    " °C; пунктиром обведены ячейки от ±" + Shaky + " °C\n";
            if (sw.Levels.Contains(3))
                foreach (int lv in new[] { 2, 1 })
                {
                    string eased = string.Join(", ", core.Lps.Where((lp, i) => sw.Levels[i] == lv));
                    if (eased != "") notes += "сила нагрузки «" + LevelName[lv] + "» подобрана для ядер " + eased + "\n";
                }
            if (sw.Throttled.Any(x => x)) notes += "красный номер строки: ядро под нагрузкой упёрлось в порог и сбрасывало частоту\n";
            sweepNotes.Text = notes.TrimEnd();

            double[] rise = sw.Own(0), power = sw.Own(1), speed = sw.Own(2);
            Func<double[], double[], double, double[]> per = (a, b, k) => a.Select((v, j) => b[j] > 1 ? v / b[j] * k : double.NaN).ToArray();
            double[][] own_ = { speed, rise, per(rise, speed, 1000), power, per(rise, power, 1) };
            // По блокам: у нового замера значения всех ядер блока одинаковы, у старого (по одному ядру) берётся среднее.
            for (int r = 0; r < own_.Length; r++)
                for (int u = 0; u < blocks.Length; u++)
                    ownView.Values[r][u] = blocks[u].Members.Select(i => own_[r][i]).Where(v => !double.IsNaN(v)).DefaultIfEmpty(double.NaN).Average();
            ownView.Invalidate();
        }

        void DrawLatency()
        {
            // Под диагональю: средняя задержка между ядрами двух блоков.
            double[][] m = core.Latency.Matrix;
            int count = blocks.Length;
            var pair = new double[count, count];
            double lo = double.MaxValue, hi = 0;
            for (int a = 0; a < count; a++)
                for (int b = 0; b < a; b++)
                {
                    double[] known = blocks[a].Members.SelectMany(i => blocks[b].Members.Select(j => m[i][j])).Where(v => !double.IsNaN(v)).ToArray();
                    pair[a, b] = known.Length > 0 ? known.Average() : double.NaN;
                    if (known.Length == 0) continue;
                    lo = Math.Min(lo, pair[a, b]);
                    hi = Math.Max(hi, pair[a, b]);
                }
            for (int a = 0; a < count; a++)
            {
                for (int b = 0; b < a; b++)
                    pairView.Cells[a, b] = double.IsNaN(pair[a, b]) ? null : new PairView.CellData
                    {
                        Text = pair[a, b].ToString("0"), Fill = Theme.Heat(hi > lo ? (pair[a, b] - lo) / (hi - lo) : 0),
                    };
                // На диагонали — задержка между ядрами самого блока E-ядер; у P-ядра ячейки нет.
                double[] inside = blocks[a].Members.SelectMany(i => blocks[a].Members.Where(j => j != i).Select(j => m[i][j])).Where(v => !double.IsNaN(v)).ToArray();
                pairView.LowerOwn[a] = inside.Length == 0 ? null
                    : new PairView.CellData { Text = inside.Average().ToString("0"), Fill = Theme.Button, Own = true };
            }
            pairView.Invalidate();
        }

        /// Режим проверки: сохраняет картинку окна в файл и закрывает программу.
        void SaveSnapshot()
        {
            // Закрыться нужно и при неудаче: иначе невидимая копия программы осталась бы висеть.
            try
            {
                using (var image = new Bitmap(Width, Height))
                {
                    DrawToBitmap(image, new Rectangle(0, 0, Width, Height));
                    image.Save(snapshotPath, ImageFormat.Png);
                }
            }
            finally { Close(); }
        }
    }
}
