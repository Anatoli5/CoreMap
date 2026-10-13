// Главное окно, колонка потолков: подбор «Под температуру» (ползунок по снятым кривым, поиск замером, проверка
// под общей нагрузкой) и кривые «температура от множителя» с графиком и списком блоков.
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm
    {
        const int FitLow = 55, FitHigh = 95;  // пределы ползунка температуры, °C

        Segmented heatMode;  // по какой температуре ступени работать: по средней или по наибольшей

        // «Под температуру».
        MeanView meanView;
        Slider fitSlider;    // цель подбора потолков, °C
        Label fitValue, fitMean, fitStatus;
        Segmented fitMode;
        Button fitButton, jointButton;
        bool fitBest;        // ползунок делит температуру между блоками «по выгоде», а не ровной отсечкой
        bool fitApplied;     // ползунок в этом запуске уже ставил потолки: смена способа ставит их заново
        string fitReport;    // итог последнего подбора потолков по температуре
        bool wasJoint;       // в прошлый опрос шёл замер под общей нагрузкой
        MeanCurve mean;      // средний множитель от температуры — график над ползунком; null — ещё не считался
        string meanKey;      // по чему он посчитан
        string[] meanReadout = new string[0];  // числа под курсором на этом графике, по градусам

        // «Кривая · блок».
        Label curveTitle, curveStatus;
        FlowLayoutPanel curveRow;
        Button curveButton, curveAllButton;
        NumericUpDown curveTemp;  // предел, до которого греется блок при снятии кривой, °C
        CurveView curveView;
        BlockList blockList;
        string drawnKey;          // по чему нарисованы график кривых и список блоков
        Block followed;           // блок, за которым график уже перешёл во время снятия кривых

        /// Колонка потолков: профили, подбор под температуру, кривая выбранного блока.
        Control LimitsPanel()
        {
            FlowLayoutPanel head;
            FlowLayoutPanel panel = Section("Потолки множителей", null, out head);
            if (core.Curve != null) HeatSwitch(head);
            if (core.Limits != null) ProfileSection(panel);
            // Кривые есть только вместе с потолками: см. Core.
            if (core.Curve != null)
            {
                FitSection(panel);
                CurveSection(panel);
            }
            return panel;
        }

        /// Переключатель в заголовке колонки: «по средней / по наибольшей».
        void HeatSwitch(FlowLayoutPanel head)
        {
            head.Controls.Add(heatMode = new Segmented(new[] { 0, 1 }, v => v == 0 ? "по средней" : "по наибольшей", v =>
                {
                    core.Curves.ByPeak = v == 1;
                    if (fitApplied) ApplySlider();
                    Tick();
                },
                "По какой температуре работать\nВ каждой точке кривой хранятся обе температуры ступени; здесь выбирается, по какой считать.\n" +
                "• по средней — средняя за ступень: то, что блок держит под нагрузкой\n" +
                "• по наибольшей — самая высокая, мелькнувшая за ступень: мгновенные всплески\n" +
                "• выбор действует на ползунок, на «подобрать замером» и на предел «греть до» при снятии кривых\n" +
                "• на графике толстой линией идёт выбранная температура, тонкой — вторая"));
            foreach (Control c in head.Controls) c.Anchor = AnchorStyles.Left;
        }

        void FitSection(FlowLayoutPanel panel)
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
                Moved = t => { ApplySlider(); Tick(); },
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
                    if (fitApplied) ApplySlider();
                    Tick();
                },
                "Как делить температуру между блоками\nОба способа берут потолки из снятых кривых.\n" +
                "• ровно — каждому блоку самый высокий потолок, при котором он не горячее температуры с ползунка\n" +
                "• по выгоде — та же средняя температура по блокам, но поделена иначе: блок, которому градус даёт много " +
                "частоты, поднимается выше отсечки (не больше чем на " + CurveFit.Spread + " °C), а ступени, которые стоят градусов " +
                "и частоты не добавляют, срезаются\n" +
                "• на графике над ползунком белая линия — «ровно», бирюзовая — «по выгоде»\n" +
                "• расчёт по кривым одиночной нагрузки: под общей нагрузкой блоки греют друг друга, это ещё не учтено"));
            fit.Controls.Add(fitButton = Widgets.Button("подобрать замером", () =>
                {
                    if (core.Search.Running) core.Search.Stop();
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
                    if (core.Joint.Running) core.Joint.Stop();
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

        void CurveSection(FlowLayoutPanel panel)
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
                    "• строка над графиком — при каких вентиляторах снята яркая кривая: на максимуме, на авто (от и до скольких " +
                    "оборотов) или неизвестно; жёлтая приписка «у других иначе» — кривые сняты при разных вентиляторах, и их " +
                    "температуры могут быть несравнимы; подробности — под курсором на этой строке\n" +
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

        void SizeLimits(int side, int list, int cell)
        {
            // Высота — чтобы двенадцать строк списка оставались читаемыми и при наименьшей ячейке.
            curveView.Size = new Size(side - list, Math.Max(cell * 8, Theme.Px(192)));
            blockList.Size = new Size(Theme.Px(170), curveView.Height);
            curveStatus.MaximumSize = new Size(side, 0);
            fitStatus.MaximumSize = new Size(side, 0);
            fitSlider.Width = Math.Max(Theme.Px(80), side - fitValue.PreferredSize.Width - fitSlider.Margin.Horizontal);
            meanView.Width = fitSlider.Width;
            fitMean.MaximumSize = new Size(side, 0);
        }

        /// Ставит потолки под температуру с ползунка по снятым кривым и запоминает итог.
        void ApplySlider()
        {
            fitValue.Text = fitSlider.Value + " °C";
            fitApplied = true;
            fitReport = core.FitFromCurves(fitSlider.Value, fitBest) + BackHint;
        }

        /// Ставит ползунок на температуру и потолки под неё — то же, что владелец делает мышью.
        public void SlideTo(int temp, bool best)
        {
            if (fitSlider == null) return;
            fitBest = best;
            fitSlider.Value = temp;
            ApplySlider();
            Tick();
        }

        void CurveClick()
        {
            fitReport = null;
            if (core.Curve.Running) core.Curve.Stop();
            else core.StartCurve(selected, (double)curveTemp.Value);
        }

        void UpdateLimits(bool busy)
        {
            CurveTest curve = core.Curve;
            LimitSearch search = core.Search;
            JointTest joint = core.Joint;
            // График идёт за блоком, который меряется сейчас. Переход — один раз на блок: если владелец
            // во время замера щёлкнул другую плитку, её кривая остаётся на виду до следующего блока.
            Block active = curve.Active ?? search.Active;
            if (active != followed)
            {
                followed = active;
                if (active != null) Select(active);
            }
            curveButton.Text = curve.Running ? "■" : "▶";
            curveButton.Enabled = curve.Running || !busy;
            curveAllButton.Enabled = !busy;
            fitButton.Enabled = search.Running || !busy;
            fitButton.Text = search.Running ? "■ стоп" : "подобрать замером";
            fitSlider.Enabled = !busy;
            jointButton.Enabled = joint.Running || !busy;
            jointButton.Text = joint.Running ? "■ стоп" : "проверить все разом";
            if (wasJoint && !joint.Running) fitReport = joint.Error ?? JointSummary();
            wasJoint = joint.Running;
            fitMode.Show(fitBest ? 1 : 0, !busy);
            heatMode.Show(core.Curves.ByPeak ? 1 : 0, !busy);
            curveStatus.Text = joint.Running ? "Идёт проверка под общей нагрузкой: нагружены все блоки разом."
                : search.Running ? "Идёт подбор потолков по температуре — пройденные ступени добавляются на кривые."
                : curve.Running
                ? (curve.BlockTotal > 1 ? "Снимаю кривые · блок " + Math.Min(curve.BlockDone + 1, curve.BlockTotal) + " из " + curve.BlockTotal + " · шаг "
                    : "Снимаю кривую · шаг ") + Math.Min(curve.Done + 1, curve.Total) + " из " + curve.Total +
                    (curve.Current != null ? " · " + curve.Current : "")
                : curve.Error ?? "";
            curveStatus.Visible = curveStatus.Text != "";
            curveStatus.ForeColor = curve.Running ? Theme.Text : curve.Error != null ? Theme.Bad : Theme.Dim;
            fitStatus.Text = joint.Running
                ? "Все блоки под нагрузкой · " + (joint.Current ?? "") + " · " + Math.Min(joint.Done, joint.Total) + " с из " + joint.Total
                : search.Running
                ? "Подбираю · блок " + Math.Min(search.BlockDone + 1, search.BlockTotal) + " из " + search.BlockTotal +
                    (search.Current != null ? " · " + search.Current : "")
                : fitReport ?? search.Report ?? "Ползунок ставит потолки по снятым кривым, кнопка — замером под нагрузкой.";
            ShowCurves();
        }

        /// Обновляет заголовок и график кривых, если изменилось то, от чего они зависят. Наведение мыши на блок
        /// вызывает только это, а не обновление всего окна.
        void ShowCurves()
        {
            if (core.Curve == null || curveView == null || fitSlider == null) return;
            CurveStore curve = core.Curves;
            Block shown = hovered ?? selected;  // блок, чья кривая поверх остальных
            curveTitle.Text = "Кривая · " + shown.Title;
            string key = curve.Version + "/" + shown.Index + "/" + selected.Index + "/" + fitSlider.Value + "/" + core.Load.Level + "/" + fitBest + "/" + curve.ByPeak +
                "/" + string.Join(",", core.Limits.Current) + "/" + string.Join(",", core.Was);
            if (drawnKey == key) return;
            drawnKey = key;
            DrawCurves(shown);
        }

        /// Рисует график кривых, список блоков и средний множитель для температуры с ползунка.
        void DrawCurves(Block shown)
        {
            CurveStore curve = core.Curves;
            int level = core.Load.Level, target = fitSlider.Value;
            CurveChoice[] picks = core.Fit.Choose(level, target, fitBest);
            // График и список показывают то, что стоит в процессоре; выбор ползунка в них попадает, когда ползунок
            // его поставил. От положения ползунка, пока он ничего не ставил, зависят только отсечка и средний множитель.
            CurveChoice[] standing = core.Fit.Standing(level, picks);
            curveView.Show(curve.For(shown), curve.All(level).Where(r => r.Lp != shown.Lp).ToArray(),
                           core.Blocks, target, standing, curve.ByPeak, "Кривых ещё нет — нажми ▶▶, чтобы снять все",
                           curve.Uneven(level, core.Blocks));
            blockList.Show(standing, selected.Index, shown.Index, JointTemps(), Delta());
            ShowMean(level, target, picks);
        }

        /// Строка «средний множитель» под ползунком и график среднего множителя над ним.
        void ShowMean(int level, int target, CurveChoice[] picks)
        {
            CurveStore curve = core.Curves;
            double[] now = core.Fit.Means(picks);
            double even = fitBest ? core.Fit.Means(core.Fit.Choose(level, target))[0] : double.NaN;
            fitMean.Text = double.IsNaN(now[0]) ? "" : "средний множитель ×" + now[0].ToString("0.00") +
                (double.IsNaN(even) ? "" : " (ровно ×" + even.ToString("0.00") + ")") +
                (double.IsNaN(now[1]) ? "" : " · P ×" + now[1].ToString("0.0")) + (double.IsNaN(now[2]) ? "" : " · E ×" + now[2].ToString("0.0"));
            meanView.Value = target;
            // График пересчитывается по готовым кривым: пока идёт измерение, он не трогается (иначе полный
            // пересчёт шёл бы на каждой ступени) и обновляется, когда оно закончилось.
            string key = curve.Version + "/" + level + "/" + curve.ByPeak;
            bool waits = core.Busy && mean != null && meanKey.EndsWith("/" + level + "/" + curve.ByPeak);
            if (meanKey != key && !waits)
            {
                meanKey = key;
                mean = core.Fit.Mean(level, FitLow, FitHigh);
                meanReadout = MeanReadout(mean);
            }
            meanView.Show(mean.Even, mean.Best, fitBest, mean.Rough, meanReadout, mean.Knee);
        }

        /// Числа под курсором на графике среднего множителя: по строке на каждый градус; null — кривых нет.
        string[] MeanReadout(MeanCurve mean)
        {
            var readout = new string[mean.Even.Length];
            for (int k = 0; k < readout.Length; k++)
            {
                if (double.IsNaN(mean.Even[k])) continue;
                readout[k] = (mean.Low + k) + " °C · средний множитель ×" + mean.Even[k].ToString("0.00") +
                    "\n• по выгоде: ×" + mean.Best[k].ToString("0.00") +
                    (double.IsNaN(mean.P[k]) ? "" : "\n• P-ядра: ×" + mean.P[k].ToString("0.00")) +
                    (double.IsNaN(mean.E[k]) ? "" : "\n• блоки E-ядер: ×" + mean.E[k].ToString("0.00")) +
                    "\n• блоков с кривой: " + mean.Known[k] + " из " + blocks.Length +
                    (mean.Rough[k] ? "\n• часть блоков горячее этой температуры уже на нижней точке кривой: среднее завышено" : "");
            }
            return readout;
        }

        /// Приписка к итогу ползунка: как вернуть потолки, которые стояли до него.
        const string BackHint = " Отменить — кнопка «откатить».";

        /// Температура каждого блока в последнем замере под общей нагрузкой; NaN — замера нет или потолки с тех пор менялись.
        double[] JointTemps()
        {
            JointResult joint = core.Joint.Last;
            bool valid = joint != null && joint.Level == core.Load.Level && joint.Limits.SequenceEqual(core.Limits.Current);
            return blocks.Select(b => valid ? b.Temp(core.Curves.ByPeak ? joint.Peak : joint.Temp) : double.NaN).ToArray();
        }

        /// Итог замера под общей нагрузкой одной строкой: насколько блоки горячее, чем по своим кривым.
        string JointSummary()
        {
            JointResult joint = core.Joint.Last;
            if (joint == null) return null;
            double[] heat = core.Curves.ByPeak ? joint.Peak : joint.Temp;
            var above = new List<double>();
            var ratios = new List<double>();
            Block hottest = blocks[0];
            foreach (Block b in blocks)
            {
                if (b.Temp(heat) > hottest.Temp(heat)) hottest = b;
                double ratio = b.Ratio(joint.Ratio);
                if (!double.IsNaN(ratio)) ratios.Add(ratio);
                double alone = core.Fit.CurveTemp(b, core.Load.Level);
                if (!double.IsNaN(alone)) above.Add(b.Temp(heat) - alone);
            }
            return "Под общей нагрузкой: самый горячий блок " + hottest.Name + " — " + hottest.Temp(heat).ToString("0") + " °C" +
                (above.Count > 0 ? "; в среднем на " + above.Average().ToString("+0.0;−0.0") + " °C к своим кривым (от " + above.Min().ToString("+0;−0") +
                    " до " + above.Max().ToString("+0;−0") + ")" : "") +
                (ratios.Count > 0 ? "; средний множитель ×" + ratios.Average().ToString("0.00") : "") +
                "; прибавка мощности " + joint.Watts.ToString("0") + " Вт. По блокам — в списке после стрелки.";
        }
    }
}
