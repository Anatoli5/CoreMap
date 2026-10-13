// Главное окно: тепловое влияние блоков друг на друга и задержки между ними (две таблицы-половинки справа)
// и таблица «Под одинаковой нагрузкой» под схемой.
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm
    {
        const double Shaky = 1.5;  // расхождение проходов, ± °C, с которого ячейка считается ненадёжной
        static readonly string[] LevelName = { "", "лёгкая", "средняя", "полная" };

        Button sweepButton, latencyButton;
        Segmented secondsPick, passesPick, unitPick;
        CheckBox relative;         // «без общего прогрева»
        FlowLayoutPanel sweepHead;  // строка переключателей теплового измерения
        int headWidth;             // её ширина в один ряд
        RunBar sweepBar, latencyBar;
        Label sweepStatus, latencyStatus, sweepAxes, sweepNotes;
        PairView pairView;
        StripView ownView;

        int seconds = 20, passes = 2, unit;  // unit: 0 — проценты, 1 — градусы
        int drawnSweep = -1, drawnLatency = -1;  // по какой версии результата нарисованы таблицы
        // Копии результатов на момент последней перерисовки таблиц: измерение пишет своё в другом потоке, а
        // подсказки по ячейкам должны показывать те же числа, что и таблица.
        double[][] sweepShown;  // нагрев по ядрам в выбранных единицах (% или °C): [нагруженное][нагревшееся]
        double[][] sweepMean;   // то же в °C, как измерено
        int[] sweepLevels;      // сила нагрузки, подобранная каждому ядру в этом измерении
        double[][] latencyMean, latencyLow, latencyHigh;  // задержки по парам ядер: средняя, лучшая и худшая строка кэша

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

        void SizeThermal(int cell)
        {
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
        }

        // ----- действия -----

        public void SweepClick()
        {
            if (core.Sweep.Running) core.Sweep.Stop();
            else core.StartSweep(seconds, passes);
        }

        void LatencyClick()
        {
            if (core.Latency.Running) core.Latency.Stop();
            else core.StartLatency();
        }

        void Redraw()
        {
            drawnSweep = -1;
            Tick();
        }

        // ----- обновление -----

        void UpdateThermal(bool busy)
        {
            Monitor m = core.Monitor;
            ThermalSweep sw = core.Sweep;
            LatencyTest lat = core.Latency;
            Calibration cal = core.Calibration;
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
            sweepStatus.ForeColor = sw.Running ? Theme.Text : sw.Error != null ? Theme.Bad : Theme.Dim;
            latencyBar.Set(lat.Running ? (double)lat.Done / lat.Total : 0);
            latencyStatus.Text = LatencyStatus();
            latencyStatus.ForeColor = lat.Running ? Theme.Text : lat.Error != null ? Theme.Bad : Theme.Dim;

            if (drawnSweep != sw.Version) { drawnSweep = sw.Version; DrawSweep(); }
            if (drawnLatency != lat.Version) { drawnLatency = lat.Version; DrawLatency(); }
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
            if (sw.Error != null) return Capital(sw.Error) + " · " + sw.Finished + " · готово " + sw.Done + " из " + sw.Total;
            if (sw.Finished != null)
                return "Измерение " + (sw.Done >= sw.Total ? "закончено " : "остановлено ") + sw.Finished + " · готово " + sw.Done + " из " + sw.Total;
            return "Измерений ещё не было — нажми ▶";
        }

        string LatencyStatus()
        {
            LatencyTest lat = core.Latency;
            if (lat.Running) return "Идёт измерение · готово " + lat.Done + " из " + lat.Total + " пар";
            if (lat.Error != null) return "Измерение не удалось " + lat.Finished + ": " + lat.Error;
            if (lat.Finished != null) return "Измерение закончено " + lat.Finished + " · " + lat.Done + " из " + lat.Total + " пар";
            return "Измерений ещё не было — нажми ▶";
        }

        /// Перерисовывает всё, что показано по тепловому измерению: таблицу связи, приписки под ней и таблицу
        /// «Под одинаковой нагрузкой».
        void DrawSweep()
        {
            ThermalSweep sw = core.Sweep;
            double[][] mean, spread;
            sw.Combined(out mean, out spread);
            bool[] throttled;
            sw.Marks(out sweepLevels, out throttled);
            sweepMean = mean;
            sweepShown = ShownRows(mean);
            DrawCoupling(sweepShown, mean, spread, throttled);
            sweepNotes.Text = SweepNotes(spread, sweepLevels, throttled);
            DrawOwn(sw);
        }

        /// Нагрев по ядрам в выбранных единицах: [нагруженное][нагревшееся]; null — ядро не нагружалось.
        double[][] ShownRows(double[][] mean)
        {
            bool percent = unit == 0;
            var rows = new double[mean.Length][];
            for (int i = 0; i < mean.Length; i++)
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
            return rows;
        }

        /// Верхняя таблица-половинка: тепловая связь пар блоков, на диагонали — собственный нагрев блока.
        void DrawCoupling(double[][] rows, double[][] mean, double[][] spread, bool[] throttled)
        {
            bool percent = unit == 0;
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
                pairView.Bad[a] = blocks[a].Members.Any(i => throttled[i]);
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
        }

        /// Приписки под таблицами: насколько расходятся проходы, каким ядрам сила нагрузки снижена, какие упёрлись в порог.
        string SweepNotes(double[][] spread, int[] levels, bool[] throttled)
        {
            string notes = "";
            double[] diffs = spread.Where(r => r != null).SelectMany(r => r).Where(v => !double.IsNaN(v)).OrderBy(v => v).ToArray();
            if (diffs.Length > 0)
                notes += "расхождение проходов: обычно ±" + diffs[diffs.Length / 2] + " °C, худшая ячейка ±" + diffs.Last() +
                    " °C; пунктиром обведены ячейки от ±" + Shaky + " °C\n";
            if (levels.Contains(3))
                foreach (int lv in new[] { 2, 1 })
                {
                    string eased = string.Join(", ", core.Lps.Where((lp, i) => levels[i] == lv));
                    if (eased != "") notes += "сила нагрузки «" + LevelName[lv] + "» подобрана для ядер " + eased + "\n";
                }
            if (throttled.Any(x => x)) notes += "красный номер строки: ядро под нагрузкой упёрлось в порог и сбрасывало частоту\n";
            // Строки нет, пока оборотов нет ни у одного блока: измерение снято до их появления.
            string air = core.Sweep.FanNote();
            if (air != null) notes += "вентиляторы при измерении: " + air + "\n";
            return notes.TrimEnd();
        }

        /// Таблица «Под одинаковой нагрузкой»: проба одной и той же лёгкой нагрузкой на каждом блоке.
        void DrawOwn(ThermalSweep sw)
        {
            double[] rise = sw.Own(0), power = sw.Own(1), speed = sw.Own(2);
            Func<double[], double[], double, double[]> per = (a, b, k) => a.Select((v, j) => b[j] > 1 ? v / b[j] * k : double.NaN).ToArray();
            double[][] rows = { speed, rise, per(rise, speed, 1000), power, per(rise, power, 1) };
            // По блокам: у нового замера значения всех ядер блока одинаковы, у старого (по одному ядру) берётся среднее.
            for (int r = 0; r < rows.Length; r++)
                for (int u = 0; u < blocks.Length; u++)
                    ownView.Values[r][u] = blocks[u].Members.Select(i => rows[r][i]).Where(v => !double.IsNaN(v)).DefaultIfEmpty(double.NaN).Average();
            ownView.Invalidate();
        }

        void DrawLatency()
        {
            // Под диагональю: средняя задержка между ядрами двух блоков.
            core.Latency.Taken(out latencyMean, out latencyLow, out latencyHigh);
            double[][] m = latencyMean;
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

        // ----- подсказки по ячейкам -----

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
                                (sweepLevels[a] > 0 ? ", сила " + LevelName[sweepLevels[a]] : "");
                if (lower && blocks[r].Members.Length > 1)
                {
                    double[] inside = blocks[r].Members.SelectMany(a => blocks[r].Members.Where(b => b != a).Select(b => latencyMean[a][b]))
                        .Where(v => !double.IsNaN(v)).ToArray();
                    if (inside.Length > 0) text += "\n• задержка между ядрами блока: " + inside.Average().ToString("0.0") + " нс";
                }
                return text;
            }
            if (!lower)
            {
                var lines = new List<string>();
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
            double[] mean = blocks[r].Members.SelectMany(a => blocks[c].Members.Select(b => latencyMean[a][b])).Where(v => !double.IsNaN(v)).ToArray();
            if (mean.Length == 0) return null;
            double lo = blocks[r].Members.SelectMany(a => blocks[c].Members.Select(b => latencyLow[a][b])).Where(v => !double.IsNaN(v)).Min();
            double hi = blocks[r].Members.SelectMany(a => blocks[c].Members.Select(b => latencyHigh[a][b])).Where(v => !double.IsNaN(v)).Max();
            return "Задержка: " + blocks[r].Title + " и " + blocks[c].Title + "\n• в среднем " + mean.Average().ToString("0.0") + " нс" +
                "\n• от " + lo.ToString("0.0") + " до " + hi.ToString("0.0") + " нс для разных пар ядер и участков памяти";
        }
    }
}
