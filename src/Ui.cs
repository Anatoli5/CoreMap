// Оформление и собственные элементы окна: схема кристалла, таблицы по ядрам, полоса прогресса.
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    static class Theme
    {
        public static readonly Color Bg = Hex(0x14161a), Panel = Hex(0x1c1f25), Cell = Hex(0x2a2e36), Button = Hex(0x22262d);
        public static readonly Color Text = Hex(0xe6e8ec), Dim = Hex(0x8b93a1), Accent = Hex(0xffd166);
        public static readonly Color Bad = Hex(0xff5d5d), BadBg = Hex(0x5c1f1f), BadText = Hex(0xffc9c9), Good = Hex(0x7bd88f);
        public static readonly Color WarnBg = Hex(0x5a3d12), WarnText = Hex(0xffd9a0);

        public static float Scale = 1;
        // Подсказки показываются только в режиме вопроса (кнопка «?»): сами по себе при наведении они не всплывают.
        public static readonly ToolTip Tips = new ToolTip { InitialDelay = 200, ReshowDelay = 100, AutoPopDelay = 32000, Active = false };
        public static bool Help;  // включён режим вопроса
        // Числа под курсором — значение ячейки таблицы или точки графика — показываются при простом наведении.
        public static readonly ToolTip Readout = new ToolTip { InitialDelay = 350, ReshowDelay = 100, AutoPopDelay = 32000 };
        static readonly Dictionary<string, Font> fonts = new Dictionary<string, Font>();

        static Color Hex(int rgb) { return Color.FromArgb(unchecked((int)0xFF000000) | rgb); }

        public static Color KindText(char kind) { return kind == 'P' ? Hex(0xf0b860) : Hex(0x6fd0c8); }
        public static Color KindBack(char kind) { return kind == 'P' ? Hex(0x6b4d1c) : Hex(0x1d5753); }

        public static int Px(double v) { return (int)Math.Round(v * Scale); }

        /// Шрифт заданного размера в пикселях до масштабирования.
        public static Font Font(double size, bool bold = false) { return PixelFont(Px(size), bold); }

        public static Font PixelFont(int pixels, bool bold = false)
        {
            string key = pixels + (bold ? "b" : "");
            Font font;
            if (!fonts.TryGetValue(key, out font))
                fonts[key] = font = new Font("Segoe UI", pixels, bold ? FontStyle.Bold : FontStyle.Regular, GraphicsUnit.Pixel);
            return font;
        }

        /// 0..1 → цвет от холодного синего к красному.
        public static Color Heat(double k)
        {
            k = double.IsNaN(k) ? 0 : Math.Max(0, Math.Min(1, k));
            double h = (215 - 215 * k) / 360, l = 0.28 + 0.10 * k, s = 0.65;
            double q = l < 0.5 ? l * (1 + s) : l + s - l * s, p = 2 * l - q;
            Func<double, int> channel = t =>
            {
                t = (t % 1 + 1) % 1;
                double v = t < 1.0 / 6 ? p + (q - p) * 6 * t : t < 0.5 ? q : t < 2.0 / 3 ? p + (q - p) * (2.0 / 3 - t) * 6 : p;
                return (int)(v * 255);
            };
            return Color.FromArgb(channel(h + 1.0 / 3), channel(h), channel(h - 1.0 / 3));
        }

        public static string Clock(double seconds)
        {
            int s = (int)seconds;
            return (s >= 3600 ? s / 3600 + ":" : "") + (s / 60 % 60).ToString("00") + ":" + (s % 60).ToString("00");
        }

        const TextFormatFlags Centered = TextFormatFlags.HorizontalCenter | TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding;

        public static void Center(Graphics g, string text, Font font, Rectangle box, Color color)
        {
            TextRenderer.DrawText(g, text, font, box, color, Centered);
        }
    }

    /// Элемент, который сам рисует своё содержимое и показывает подсказку для места под курсором.
    abstract class Painted : Control
    {
        string tip;

        protected Painted()
        {
            SetStyle(ControlStyles.OptimizedDoubleBuffer | ControlStyles.AllPaintingInWmPaint | ControlStyles.UserPaint |
                     ControlStyles.ResizeRedraw, true);
            BackColor = Theme.Panel;
            Margin = Padding.Empty;
        }

        protected abstract string TipAt(Point at);

        /// Подсказка элемента — числа под курсором, а не описание: показывается и без режима вопроса.
        protected virtual bool Data { get { return false; } }

        protected override void OnMouseMove(MouseEventArgs e)
        {
            base.OnMouseMove(e);
            if (!Data && !Theme.Help) { tip = null; return; }
            ToolTip tips = Data ? Theme.Readout : Theme.Tips;
            string text = TipAt(e.Location);
            if (text == tip) return;
            // Место под курсором узнаётся по первой строке подсказки — заголовку. Сменилось место — подсказка
            // показывается заново (для этого её надо выключить и включить); сменились только числа под тем же
            // заголовком — текст обновляется на месте, иначе подсказка мигала бы при каждом новом замере.
            bool moved = Title(text) != Title(tip);
            tip = text;
            if (moved) tips.Active = false;
            tips.SetToolTip(this, text);
            if (moved) tips.Active = true;
        }

        static string Title(string text)
        {
            if (text == null) return null;
            int end = text.IndexOf('\n');
            return end < 0 ? text : text.Substring(0, end);
        }

        protected override void OnMouseLeave(EventArgs e)
        {
            base.OnMouseLeave(e);
            tip = null;
            (Data ? Theme.Readout : Theme.Tips).SetToolTip(this, null);
        }
    }

    /// Общая геометрия таблиц по столбцам: столбцы с зазорами между группами и цветные полосы P/E сверху.
    /// Группа — P-ядра, идущие подряд, или один кластер E-ядер. Столбец — ядро или целый блок: во втором
    /// случае ему даётся своё название.
    class BlockGrid
    {
        public class Block { public char Kind; public int Start, Size; }

        public readonly CpuCore[] Cores;
        public readonly string[] Names;   // названия столбцов; null — номера ядер
        public readonly int Cell, High, Span;  // ширина столбца, высота строки, общая ширина
        public readonly int[] Pos;
        public readonly List<Block> Blocks = new List<Block>();
        public readonly Font CellFont;

        public BlockGrid(CpuCore[] cores, int cell, int high = 0, string[] names = null)
        {
            Cores = cores;
            Names = names;
            Cell = cell;
            High = high > 0 ? high : cell;
            CellFont = Theme.PixelFont(Math.Max(8, (int)(Math.Min(cell, High * 1.25) * 0.42)));
            for (int i = 0; i < cores.Length; i++)
            {
                bool same = i > 0 && cores[i].Kind == cores[i - 1].Kind && cores[i].Cluster == cores[i - 1].Cluster;
                if (same) Blocks[Blocks.Count - 1].Size++;
                else Blocks.Add(new Block { Kind = cores[i].Kind, Start = i, Size = 1 });
            }
            Pos = new int[cores.Length];
            int off = 0;
            foreach (Block b in Blocks)
            {
                for (int k = 0; k < b.Size; k++) { Pos[b.Start + k] = off; off += cell; }
                off += Theme.Px(4);
            }
            Span = off - Theme.Px(4);
        }

        public string Name(int j) { return Names != null ? Names[j] : Cores[j].Lp.ToString(); }

        /// Номер столбца по координате внутри таблицы; -1 — мимо ячеек.
        public int Index(int v)
        {
            for (int k = 0; k < Pos.Length; k++) if (v >= Pos[k] && v < Pos[k] + Cell) return k;
            return -1;
        }

        public void PaintColumnHeaders(Graphics g, int left)
        {
            foreach (Block b in Blocks)
            {
                var band = Rectangle.FromLTRB(left + Pos[b.Start], 0, left + Pos[b.Start + b.Size - 1] + Cell - 1, High - 3);
                using (var brush = new SolidBrush(Theme.KindBack(b.Kind))) g.FillRectangle(brush, band);
                Theme.Center(g, b.Kind.ToString(), Theme.Font(11, true), band, Theme.Text);
            }
            for (int j = 0; j < Cores.Length; j++)
                Theme.Center(g, Name(j), CellFont, new Rectangle(left + Pos[j], High, Cell, High), Theme.KindText(Cores[j].Kind));
        }
    }

    /// Две отдельные таблицы «блок × блок», уложенные рядом. Обе симметричны, вторая половина каждой была бы
    /// повтором, поэтому от каждой показан треугольник: верхняя таблица — справа сверху, нижняя — слева снизу,
    /// между ними зазор. Номера блоков стоят по прямым сторонам треугольников: у верхней таблицы — сверху
    /// и справа, у нижней — слева и снизу. На диагонали каждой таблицы — значение самого блока.
    /// Блок — P-ядро или кластер E-ядер целиком.
    class PairView : Painted
    {
        protected override bool Data { get { return true; } }

        public class CellData { public string Text; public Color Fill; public bool Shaky, Own; }

        readonly string[] names;
        readonly char[] kinds;
        int cell;
        public readonly CellData[,] Cells;         // [строка, столбец]: столбец больше строки — верхняя таблица, меньше — нижняя
        public readonly CellData[] UpperOwn, LowerOwn;  // диагонали двух таблиц
        public readonly bool[] Bad;                // номер блока в верхней таблице красным
        public string Upper = "", Lower = "";      // заголовки верхней и нижней таблиц
        public Func<int, int, bool, string> Tip;   // строка, столбец, нижняя ли таблица

        public PairView(string[] names, char[] kinds, int cell)
        {
            this.names = names;
            this.kinds = kinds;
            Cells = new CellData[names.Length, names.Length];
            UpperOwn = new CellData[names.Length];
            LowerOwn = new CellData[names.Length];
            Bad = new bool[names.Length];
            SetCell(cell);
        }

        int Title { get { return Theme.Px(22); } }
        int Side { get { return cell; } }                                   // ширина столбца с номерами строк
        int Head { get { return Math.Max(Theme.Px(16), cell / 2); } }       // высота строки с номерами столбцов
        int Gap { get { return cell / 2; } }                                // зазор между таблицами
        int UpperTop { get { return Title + Head; } }
        int LowerTop { get { return UpperTop + cell + Gap; } }              // нижняя таблица сдвинута на строку и зазор
        int Span { get { return names.Length * cell; } }

        /// Размер ячейки в пикселях: окно меняет его, когда меняется само.
        public void SetCell(int value)
        {
            if (value == cell) return;
            cell = value;
            Size = new Size(Side + Span + Side, LowerTop + Span + Head + Title);
            Invalidate();
        }

        protected override string TipAt(Point at)
        {
            if (Tip == null || at.X < Side || at.X >= Side + Span) return null;
            int c = (at.X - Side) / cell;
            if (at.Y >= UpperTop && (at.Y - UpperTop) / cell <= c) return Tip((at.Y - UpperTop) / cell, c, false);
            if (at.Y < LowerTop) return null;
            int r = (at.Y - LowerTop) / cell;
            return r < names.Length && c <= r ? Tip(r, c, true) : null;
        }

        void Number(Graphics g, int unit, Rectangle box, Font font, bool bad)
        {
            using (var brush = new SolidBrush(bad ? Theme.Bad : Theme.KindBack(kinds[unit]))) g.FillRectangle(brush, box);
            Theme.Center(g, names[unit], font, box, bad ? Color.Black : Theme.Text);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            int n = names.Length;
            Font value = Theme.PixelFont(Math.Max(9, (int)(cell * 0.36))), label = Theme.PixelFont(Math.Max(8, (int)(cell * 0.28)), true);
            TextRenderer.DrawText(g, Upper, Theme.Font(13, true), new Rectangle(0, 0, Width, Title), Theme.Text,
                                  TextFormatFlags.Right | TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding);
            TextRenderer.DrawText(g, Lower, Theme.Font(13, true), new Rectangle(0, Height - Title, Width, Title), Theme.Text,
                                  TextFormatFlags.Left | TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding);
            for (int k = 0; k < n; k++)
            {
                Number(g, k, new Rectangle(Side + k * cell, Title, cell - 1, Head - 1), label, Bad[k]);                    // верхняя: столбцы сверху
                Number(g, k, new Rectangle(Side + Span, UpperTop + k * cell, Side - 1, cell - 1), label, Bad[k]);          // верхняя: строки справа
                Number(g, k, new Rectangle(0, LowerTop + k * cell, Side - 1, cell - 1), label, false);                     // нижняя: строки слева
                Number(g, k, new Rectangle(Side + k * cell, LowerTop + Span, cell - 1, Head - 1), label, false);           // нижняя: столбцы снизу
            }
            using (var dashed = new Pen(Theme.Text) { DashPattern = new float[] { 2, 2 } })
                for (int r = 0; r < n; r++)
                    for (int c = 0; c < n; c++)
                        foreach (bool lower in new[] { false, true })
                        {
                            if (lower ? c > r : c < r) continue;
                            CellData data = r == c ? (lower ? LowerOwn[r] : UpperOwn[r]) : Cells[r, c];
                            if (r == c && data == null && lower) continue;  // у P-ядра задержки с самим собой нет
                            var box = new Rectangle(Side + c * cell, (lower ? LowerTop : UpperTop) + r * cell, cell - 1, cell - 1);
                            using (var brush = new SolidBrush(data == null ? Theme.Cell : data.Fill)) g.FillRectangle(brush, box);
                            if (data == null) continue;
                            if (data.Shaky) g.DrawRectangle(dashed, box.X + 1, box.Y + 1, box.Width - 3, box.Height - 3);
                            Theme.Center(g, data.Text, value, box, data.Own ? Theme.Dim : Theme.Text);
                        }
        }
    }

    /// Таблица «столбец → значение» в несколько строк, с той же разметкой групп. Столбец — ядро или блок.
    class StripView : Painted
    {
        protected override bool Data { get { return true; } }

        readonly CpuCore[] cores;
        readonly string[] names;
        BlockGrid grid;
        readonly string[] units;
        readonly int left;
        int top;
        public readonly double[][] Values;  // [строка][столбец], NaN — нет значения
        public readonly int[] Digits;

        public StripView(CpuCore[] cores, string[] names, int cell, int high, string[] units, int[] digits)
        {
            this.cores = cores;
            this.names = names;
            this.units = units;
            Digits = digits;
            left = Theme.Px(92);
            Values = units.Select(u => Enumerable.Repeat(double.NaN, cores.Length).ToArray()).ToArray();
            SetCell(cell, high);
        }

        /// Ширина столбца и высота строки в пикселях: окно меняет их, когда меняется само.
        public void SetCell(int cell, int high)
        {
            if (grid != null && grid.Cell == cell && grid.High == high) return;
            grid = new BlockGrid(cores, cell, high, names);
            top = 2 * high;
            Size = new Size(left + grid.Span, top + units.Length * high);
            Invalidate();
        }

        public void SetUnit(int row, string unit)
        {
            if (units[row] == unit) return;
            units[row] = unit;
            Invalidate();
        }

        protected override string TipAt(Point at)
        {
            int j = grid.Index(at.X - left), r = at.Y >= top ? (at.Y - top) / grid.High : -1;
            if (j < 0 || r < 0 || r >= units.Length || double.IsNaN(Values[r][j])) return null;
            return (names != null && names[j].Contains("–") ? "блок E-ядер " : "ядро ") + grid.Name(j) + ": " +
                Values[r][j].ToString("0.#") + " " + units[r];
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            int c = grid.Cell, h = grid.High;
            grid.PaintColumnHeaders(g, left);
            for (int r = 0; r < units.Length; r++)
            {
                int y = top + r * h;
                TextRenderer.DrawText(g, units[r], Theme.Font(11), new Rectangle(0, y, left - Theme.Px(8), h), Theme.Dim,
                                      TextFormatFlags.Right | TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding);
                double[] known = Values[r].Where(v => !double.IsNaN(v)).ToArray();
                double lo = known.Length > 0 ? known.Min() : 0, hi = known.Length > 0 ? known.Max() : 0;
                for (int j = 0; j < grid.Cores.Length; j++)
                {
                    double v = Values[r][j];
                    var box = new Rectangle(left + grid.Pos[j], y, c - 1, h - 1);
                    using (var brush = new SolidBrush(double.IsNaN(v) ? Theme.Cell : Theme.Heat(hi > lo ? (v - lo) / (hi - lo) : 0)))
                        g.FillRectangle(brush, box);
                    if (!double.IsNaN(v)) Theme.Center(g, v.ToString(Digits[r] == 0 ? "0" : "0.0"), grid.CellFont, box, Theme.Text);
                }
            }
        }
    }

    /// Схема кристалла. P-ядро — одна плитка. Кластер E-ядер — тоже одна плитка и одна единица: температура,
    /// средняя, максимум, множитель и потолок у него общие, нагрузка включается и снимается со всех его ядер
    /// разом. Номера четырёх ядер на плитке — только подписи: в других программах они идут как отдельные ядра.
    class DieView : Painted
    {
        readonly CpuCore[] cores;
        readonly DieLayout layout;
        readonly Rectangle[] blocks;   // плитки блоков схемы
        readonly Rectangle[] marks;    // место номера ядра на плитке кластера
        double zoom;
        public double[] Cur, Max;
        public double[] Avg;                   // средняя под нагрузкой; null или NaN — нет
        public double[] Ratio;                 // фактический множитель; null или NaN — неизвестен
        public bool[] Loaded, Throttled;
        public int[] Limit, LimitFused;        // потолок множителя и заводской предел; null — неизвестны
        public int Selected = -1;              // ядро, для которого показана кривая: его плитка обведена
        public Action<int> Clicked;
        public Action<int, Point> Menu;        // правая кнопка по плитке
        public Action<int> Hovered;            // курсор встал на плитку (первое ядро блока) или ушёл с плиток (-1)
        int under = -1;
        public Func<int, string> Tip;

        public DieView(CpuCore[] cores, DieLayout layout)
        {
            this.cores = cores;
            this.layout = layout;
            blocks = new Rectangle[layout.Units.Count];
            marks = new Rectangle[cores.Length];
            Cur = Enumerable.Repeat(double.NaN, cores.Length).ToArray();
            Max = (double[])Cur.Clone();
            Loaded = new bool[cores.Length];
            Throttled = new bool[cores.Length];
            Cursor = Cursors.Hand;
            SetZoom(1);
        }

        /// Размер в пикселях с учётом масштаба Windows и текущего увеличения схемы.
        int Zoomed(double v) { return Math.Max(1, (int)Math.Round(v * Theme.Scale * zoom)); }

        /// Увеличение схемы: окно меняет его вместе с размером ячеек таблиц.
        public void SetZoom(double value)
        {
            if (value == zoom) return;
            zoom = value;
            int uw = Zoomed(150), uh = Zoomed(96), gap = Zoomed(8), pad = Zoomed(7);
            int bw = Zoomed(24), bh = Zoomed(17);
            for (int u = 0; u < blocks.Length; u++)
            {
                DieUnit unit = layout.Units[u];
                Rectangle block = blocks[u] = new Rectangle(unit.Col * (uw + gap), unit.Row * (uh + gap), uw, uh);
                for (int k = 0; k < unit.Indexes.Length; k++)
                    marks[unit.Indexes[k]] = new Rectangle(block.X + pad + k % 2 * bw, block.Y + pad + k / 2 * bh, bw, bh);
            }
            Size = new Size(layout.Cols * (uw + gap) - gap, layout.Rows * (uh + gap) - gap);
            Invalidate();
        }

        /// Первое ядро блока под курсором; -1 — мимо плиток.
        int HitBlock(Point at)
        {
            int u = Array.FindIndex(blocks, b => b.Contains(at));
            return u < 0 ? -1 : layout.Units[u].Indexes.Min();
        }

        protected override string TipAt(Point at)
        {
            int i = HitBlock(at);
            return i < 0 || Tip == null ? null : Tip(i);
        }

        protected override void OnMouseMove(MouseEventArgs e)
        {
            base.OnMouseMove(e);
            Hover(HitBlock(e.Location));
        }

        protected override void OnMouseLeave(EventArgs e)
        {
            base.OnMouseLeave(e);
            Hover(-1);
        }

        void Hover(int i)
        {
            if (i == under) return;
            under = i;
            if (Hovered != null) Hovered(i);
        }

        protected override void OnMouseClick(MouseEventArgs e)
        {
            base.OnMouseClick(e);
            if (e.Button == MouseButtons.Left && Clicked != null && HitBlock(e.Location) >= 0) Clicked(HitBlock(e.Location));
            if (e.Button == MouseButtons.Right && Menu != null && HitBlock(e.Location) >= 0) Menu(HitBlock(e.Location), e.Location);
        }

        /// Наибольшее из значений по ядрам блока; NaN — ни одного нет.
        static double Highest(double[] values, int[] members)
        {
            double top = double.NaN;
            if (values != null)
                foreach (int i in members)
                    if (!double.IsNaN(values[i]) && !(top >= values[i])) top = values[i];
            return top;
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            const TextFormatFlags plain = TextFormatFlags.NoPadding;
            int pad = Zoomed(7);
            Font small = Theme.PixelFont(Math.Max(8, Zoomed(12))), tiny = Theme.PixelFont(Math.Max(8, Zoomed(11)));
            Font large = Theme.PixelFont(Math.Max(9, Zoomed(28)), true);
            for (int u = 0; u < blocks.Length; u++)
            {
                int[] members = layout.Units[u].Indexes;
                int first = members[0];
                Rectangle t = blocks[u];
                double temp = Highest(Cur, members);
                using (var brush = new SolidBrush(double.IsNaN(temp) ? Theme.Cell : Theme.Heat((temp - 40) / 60))) g.FillRectangle(brush, t);

                if (members.Contains(Selected))
                    using (var pen = new Pen(Theme.Text)) g.DrawRectangle(pen, t.X, t.Y, t.Width - 1, t.Height - 1);

                if (members.Any(i => Loaded[i]))
                    using (var pen = new Pen(Theme.Accent, Zoomed(3))) g.DrawRectangle(pen, t.X + 1, t.Y + 1, t.Width - 3, t.Height - 3);

                // Слева вверху: номер ядра, а у кластера — номера всех его ядер.
                int labelWidth;
                if (members.Length == 1)
                {
                    string number = cores[first].Lp.ToString();
                    TextRenderer.DrawText(g, number, small, new Point(t.X + pad, t.Y + pad),
                                          Throttled[first] ? Theme.Bad : Theme.KindText(cores[first].Kind), plain);
                    labelWidth = TextRenderer.MeasureText(g, number, small, Size.Empty, plain).Width;
                }
                else
                {
                    foreach (int i in members)
                        TextRenderer.DrawText(g, cores[i].Lp.ToString(), tiny, marks[i].Location,
                                              Throttled[i] ? Theme.Bad : Theme.KindText(cores[i].Kind), plain);
                    labelWidth = members.Max(i => marks[i].Right) - t.X - pad;
                }

                // Справа вверху: средняя под нагрузкой и максимум. Если обе цифры не помещаются,
                // остаётся максимум; средняя есть в подсказке.
                double max = Highest(Max, members), avg = Highest(Avg, members);
                string peak = double.IsNaN(max) ? "" : "▲ " + max;
                string mean = double.IsNaN(avg) ? "" : "⌀ " + Math.Round(avg);
                string both = mean != "" && peak != "" ? mean + "  " + peak : mean + peak;
                int room = t.Width - 2 * pad - Zoomed(4) - labelWidth;
                if (TextRenderer.MeasureText(g, both, small, Size.Empty, plain).Width > room) both = peak;
                if (both != "")
                    TextRenderer.DrawText(g, both, small, new Rectangle(t.X, t.Y + pad, t.Width - pad, small.Height), Theme.Text,
                                          TextFormatFlags.Right | plain);

                // Слева внизу: температура сейчас.
                string cur = double.IsNaN(temp) ? "–" : temp + "°";
                TextRenderer.DrawText(g, cur, large, new Rectangle(t.X + pad, t.Bottom - pad - large.Height, t.Width - pad, large.Height),
                                      Theme.Text, TextFormatFlags.Left | TextFormatFlags.Bottom | plain);

                // Справа внизу: фактический множитель и потолок («52.4 / ×53»); жёлтым — если потолок ниже
                // заводского предела. Если рядом с температурой пара не помещается, остаётся потолок.
                if (Limit == null || Limit[first] <= 0) continue;
                string ceiling = "×" + Limit[first], pair = ceiling;
                double ratio = Highest(Ratio, members);
                if (!double.IsNaN(ratio))
                {
                    pair = ratio.ToString("0.0") + " / " + ceiling;
                    int free = t.Width - 2 * pad - Zoomed(4) - TextRenderer.MeasureText(g, cur, large, Size.Empty, plain).Width;
                    if (TextRenderer.MeasureText(g, pair, small, Size.Empty, plain).Width > free) pair = ceiling;
                }
                TextRenderer.DrawText(g, pair, small, new Rectangle(t.X, t.Bottom - pad - small.Height, t.Width - pad, small.Height),
                                      Limit[first] < LimitFused[first] ? Theme.Accent : Limit[first] > LimitFused[first] ? Theme.Bad : Theme.Text,
                                      TextFormatFlags.Right | TextFormatFlags.Bottom | plain);
            }
        }
    }

    /// График «температура от множителя»: кривые всех блоков разом, приглушённые, и одна выделенная поверх них —
    /// по кривой на каждую силу нагрузки. Горизонтальная линия — температура с ползунка, кружки на ней —
    /// потолки, которые ползунок ставит блокам.
    /// Точка стоит по множителю, на котором ядро работало на самом деле, а не по заданному потолку:
    /// замер на 275HX показал, что при высоких потолках ядро до потолка не доходит (при потолке ×54
    /// ядро 12 шло на ×50,8), и по потолку кривая получалась искажённой.
    /// У выделенной кривой сплошная линия — средняя температура под нагрузкой, тонкая — наибольшая; пустой
    /// кружок — точка, где ядро шло заметно ниже своего потолка: частоту держало что-то другое.
    class CurveView : Painted
    {
        static readonly Color[] LevelColor = { Theme.Dim, Theme.KindText('E'), Theme.Accent, Theme.Bad };
        static readonly string[] LevelGlyph = { "", "▂", "▅", "█" };

        CurveRun[] front = new CurveRun[0];  // кривые выделенного блока
        CurveRun[] back = new CurveRun[0];   // кривые остальных блоков на текущей силе нагрузки
        Func<int, char> kind;                // вид ядра по его номеру: цвет приглушённой кривой
        Dictionary<int, CurveChoice> picks = new Dictionary<int, CurveChoice>();  // выбор ползунка по номеру первого ядра блока
        double cutoff = double.NaN;
        bool byPeak;  // работа идёт по наибольшей температуре ступени: её линия толстая, средняя — тонкая
        string empty = "";
        readonly List<KeyValuePair<Point, string>> spots = new List<KeyValuePair<Point, string>>();
        readonly List<KeyValuePair<Point, int>> owners = new List<KeyValuePair<Point, int>>();  // точка кривой → её блок
        int under = -1;

        public Action<int> Hovered;  // курсор встал на точку кривой блока (номер его первого ядра) или ушёл (-1)
        public string Help;          // описание графика: показывается в режиме вопроса, когда курсор не на точке

        protected override bool Data { get { return true; } }

        public void Show(CurveRun[] front, CurveRun[] back, Func<int, char> kind, double cutoff, Dictionary<int, CurveChoice> picks, bool byPeak, string empty)
        {
            this.byPeak = byPeak;
            this.front = front;
            this.back = back;
            this.kind = kind;
            this.cutoff = cutoff;
            this.picks = picks;
            this.empty = empty;
            Invalidate();
        }

        static bool Near(Point a, Point b) { return Math.Abs(a.X - b.X) <= Theme.Px(7) && Math.Abs(a.Y - b.Y) <= Theme.Px(7); }

        protected override string TipAt(Point at)
        {
            foreach (KeyValuePair<Point, string> spot in spots)
                if (Near(spot.Key, at)) return spot.Value;
            return Theme.Help ? Help : null;
        }

        protected override void OnMouseMove(MouseEventArgs e)
        {
            base.OnMouseMove(e);
            int lp = -1;
            foreach (KeyValuePair<Point, int> owner in owners)
                if (Near(owner.Key, e.Location)) { lp = owner.Value; break; }
            Hover(lp);
        }

        protected override void OnMouseLeave(EventArgs e)
        {
            base.OnMouseLeave(e);
            Hover(-1);
        }

        void Hover(int lp)
        {
            if (lp == under) return;
            under = lp;
            if (Hovered != null) Hovered(lp);
        }

        static double Actual(CurvePoint p) { return double.IsNaN(p.Ratio) ? p.Limit : p.Ratio; }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            g.SmoothingMode = System.Drawing.Drawing2D.SmoothingMode.AntiAlias;
            spots.Clear();
            owners.Clear();
            Font font = Theme.Font(10);
            // heat — температура, по которой идёт работа (толстая линия, точки, отсечка); other — вторая, тонкой линией.
            Func<CurvePoint, double> heat = p => byPeak ? p.Peak : p.Temp, other = p => byPeak ? p.Temp : p.Peak;
            CurvePoint[] ahead = front.SelectMany(r => r.Points.ToArray()).ToArray();
            CurvePoint[] all = ahead.Concat(back.SelectMany(r => r.Points.ToArray())).ToArray();
            if (all.Length == 0)
            {
                TextRenderer.DrawText(g, empty, Theme.Font(11), ClientRectangle, Theme.Dim,
                                      TextFormatFlags.HorizontalCenter | TextFormatFlags.VerticalCenter | TextFormatFlags.WordBreak);
                return;
            }
            int x0 = (int)Math.Floor(all.Min(p => Actual(p))), x1 = Math.Max((int)Math.Ceiling(all.Max(p => Actual(p))), x0 + 1);
            double high = Math.Max(all.Max(p => heat(p)), ahead.Length > 0 ? ahead.Max(p => Math.Max(p.Temp, p.Peak)) : 0);
            double y0 = Math.Floor(all.Min(p => p.Temp) / 5) * 5 - 5, y1 = Math.Ceiling(high / 5) * 5 + 5;
            var plot = Rectangle.FromLTRB(Theme.Px(26), Theme.Px(16), Width - Theme.Px(10), Height - Theme.Px(18));
            Func<double, int> px = v => plot.Left + (int)Math.Round((v - x0) / (x1 - x0) * plot.Width);
            Func<double, int> py = v => plot.Bottom - (int)Math.Round((v - y0) / (y1 - y0) * plot.Height);

            using (var line = new Pen(Theme.Cell))
            {
                double step = y1 - y0 > 40 ? 10 : 5;
                for (double y = y0; y <= y1; y += step)
                {
                    g.DrawLine(line, plot.Left, py(y), plot.Right, py(y));
                    TextRenderer.DrawText(g, y.ToString("0"), font, new Rectangle(0, py(y) - Theme.Px(7), plot.Left - Theme.Px(4), Theme.Px(14)), Theme.Dim,
                                          TextFormatFlags.Right | TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding);
                }
                int every = Math.Max(1, (int)Math.Ceiling((x1 - x0) * Theme.Px(34) / (double)Math.Max(1, plot.Width)));
                for (int x = x0; x <= x1; x += every)
                    TextRenderer.DrawText(g, "×" + x, font, new Rectangle(px(x) - Theme.Px(20), plot.Bottom + Theme.Px(3), Theme.Px(40), Theme.Px(14)), Theme.Dim,
                                          TextFormatFlags.HorizontalCenter | TextFormatFlags.NoPadding);
            }

            // Остальные блоки — приглушёнными линиями без точек.
            foreach (CurveRun run in back)
            {
                CurvePoint[] points = run.Points.ToArray().OrderBy(p => Actual(p)).ToArray();
                using (var pen = new Pen(Color.FromArgb(120, Theme.KindText(kind(run.Lp))), Theme.Px(1.5)))
                    for (int k = 1; k < points.Length; k++)
                        g.DrawLine(pen, px(Actual(points[k - 1])), py(heat(points[k - 1])), px(Actual(points[k])), py(heat(points[k])));
                foreach (CurvePoint p in points) owners.Add(new KeyValuePair<Point, int>(new Point(px(Actual(p)), py(heat(p))), run.Lp));
            }

            // Отсечка — температура с ползунка.
            if (!double.IsNaN(cutoff) && cutoff >= y0 && cutoff <= y1)
            {
                using (var pen = new Pen(Theme.Accent) { DashStyle = System.Drawing.Drawing2D.DashStyle.Dash })
                    g.DrawLine(pen, plot.Left, py(cutoff), plot.Right, py(cutoff));
                TextRenderer.DrawText(g, cutoff.ToString("0") + "°", font, new Rectangle(plot.Right - Theme.Px(40), py(cutoff) - Theme.Px(14), Theme.Px(40), Theme.Px(13)),
                                      Theme.Accent, TextFormatFlags.Right | TextFormatFlags.NoPadding);
            }

            int legend = plot.Left;
            foreach (CurveRun run in front)
            {
                CurvePoint[] points = run.Points.ToArray().OrderBy(p => Actual(p)).ToArray();
                if (points.Length == 0) continue;
                Color color = LevelColor[Math.Max(0, Math.Min(3, run.Level))];
                TextRenderer.DrawText(g, LevelGlyph[Math.Max(0, Math.Min(3, run.Level))], font, new Point(legend, 0), color, TextFormatFlags.NoPadding);
                legend += Theme.Px(16);
                using (var thick = new Pen(color, Theme.Px(2)))
                using (var thin = new Pen(Color.FromArgb(120, color)))
                using (var fill = new SolidBrush(color))
                using (var hollow = new SolidBrush(Theme.Panel))
                {
                    for (int k = 1; k < points.Length; k++)
                    {
                        g.DrawLine(thin, px(Actual(points[k - 1])), py(other(points[k - 1])), px(Actual(points[k])), py(other(points[k])));
                        g.DrawLine(thick, px(Actual(points[k - 1])), py(heat(points[k - 1])), px(Actual(points[k])), py(heat(points[k])));
                    }
                    int r = Theme.Px(3);
                    foreach (CurvePoint p in points)
                    {
                        var at = new Point(px(Actual(p)), py(heat(p)));
                        // Вертикальная черта — от температуры на подъёме до температуры на спуске: чем она короче,
                        // тем меньше отставание и тем надёжнее точка.
                        if (!byPeak && !double.IsNaN(p.TempUp) && !double.IsNaN(p.TempDown))
                            g.DrawLine(thin, at.X, py(p.TempUp), at.X, py(p.TempDown));
                        bool off = !double.IsNaN(p.Ratio) && Math.Abs(p.Ratio - p.Limit) > CurveTest.Reached;
                        g.FillEllipse(off ? hollow : fill, at.X - r, at.Y - r, 2 * r, 2 * r);
                        g.DrawEllipse(thick, at.X - r, at.Y - r, 2 * r, 2 * r);
                        owners.Add(new KeyValuePair<Point, int>(at, run.Lp));
                        spots.Add(new KeyValuePair<Point, string>(at,
                            (double.IsNaN(p.Ratio) ? "" : "Работало на множителе " + p.Ratio.ToString("0.0") + " при потолке ×" + p.Limit +
                                (off ? " — до потолка не дошло" : "") + "\n") +
                            "Сила нагрузки " + LevelGlyph[Math.Max(0, Math.Min(3, run.Level))] +
                            "\n• средняя под нагрузкой: " + p.Temp.ToString("0.0") + " °C" +
                            (double.IsNaN(p.TempDown) ? "" : " (на подъёме " + p.TempUp.ToString("0.0") + ", на спуске " + p.TempDown.ToString("0.0") + ")") +
                            "\n• наибольшая: " + p.Peak.ToString("0") + " °C" +
                            "\n• без нагрузки перед замером: " + p.Base.ToString("0.0") + " °C" +
                            "\n• прибавка мощности: " + p.Watts.ToString("0.0") + " Вт" +
                            "\n• снято " + run.Time));
                    }
                }
            }

            // Выбор ползунка: кружок на той точке кривой каждого блока, с которой взят потолок.
            using (var mark = new SolidBrush(Theme.Accent))
            using (var rim = new Pen(Theme.Panel, Theme.Px(1.5)))
                foreach (CurveRun run in back.Concat(front))
                {
                    CurveChoice pick;
                    if (!picks.TryGetValue(run.Lp, out pick) || pick == null) continue;
                    CurvePoint p = run.Points.ToArray().FirstOrDefault(q => q.Limit == pick.From);
                    if (p == null) continue;
                    int r = Theme.Px(front.Contains(run) ? 5 : 3.5);
                    g.FillEllipse(mark, px(Actual(p)) - r, py(heat(p)) - r, 2 * r, 2 * r);
                    g.DrawEllipse(rim, px(Actual(p)) - r, py(heat(p)) - r, 2 * r, 2 * r);
                }
        }
    }

    /// Список блоков рядом с графиком: название, потолок под температуру с ползунка и температура на нём по кривой.
    /// Наведение на строку выводит кривую блока поверх остальных, щелчок закрепляет её.
    class UnitList : Painted
    {
        readonly string[] names;
        readonly char[] kinds;
        CurveChoice[] picks;
        double[] joint;  // температура блока под общей нагрузкой; NaN — замера нет
        int[] delta;     // на сколько ступеней выбор ползунка отличается от действующего профиля
        int pinned = -1, under = -1, lit = -1;

        public Action<int> Hovered, Clicked;  // номер блока по порядку; -1 — курсор ушёл

        public UnitList(string[] names, char[] kinds)
        {
            this.names = names;
            this.kinds = kinds;
            picks = new CurveChoice[names.Length];
            joint = Enumerable.Repeat(double.NaN, names.Length).ToArray();
            delta = new int[names.Length];
            Cursor = Cursors.Hand;
        }

        /// pinned — закреплённый блок, lit — блок, чья кривая сейчас поверх остальных.
        public void Show(CurveChoice[] picks, int pinned, int lit, double[] joint, int[] delta)
        {
            this.picks = picks;
            this.joint = joint;
            this.delta = delta;
            this.pinned = pinned;
            this.lit = lit;
            Invalidate();
        }

        int Row(Point at) { return at.Y < 0 || at.Y >= Height ? -1 : Math.Min(names.Length - 1, at.Y * names.Length / Math.Max(1, Height)); }

        protected override string TipAt(Point at)
        {
            return "Блоки\nНаведи на строку — кривая блока выходит на графике поверх остальных; щелчок закрепляет её.\n" +
                "• ×N — потолок, который ползунок ставит блоку; правее — температура на нём по кривой\n" +
                "• +N или −N после потолка — разница с действующим профилем, а без профиля — с потолками при запуске программы\n" +
                "• красная температура — блок горячее цели уже на нижней точке своей кривой\n" +
                "• ▲ — взята верхняя точка кривой: выше она не снята\n" +
                "• после стрелки — температура блока, когда нагружены все блоки разом (кнопка «проверить все разом»)\n" +
                "• прочерк — кривой блока нет";
        }

        protected override void OnMouseMove(MouseEventArgs e)
        {
            base.OnMouseMove(e);
            Hover(Row(e.Location));
        }

        protected override void OnMouseLeave(EventArgs e)
        {
            base.OnMouseLeave(e);
            Hover(-1);
        }

        void Hover(int row)
        {
            if (row == under) return;
            under = row;
            if (Hovered != null) Hovered(row);
        }

        protected override void OnMouseClick(MouseEventArgs e)
        {
            base.OnMouseClick(e);
            if (e.Button == MouseButtons.Left && Clicked != null && Row(e.Location) >= 0) Clicked(Row(e.Location));
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            int n = names.Length, pad = Theme.Px(6);
            int size = Math.Max(Theme.Px(10), Math.Min(Theme.Px(13), Height / n - Theme.Px(3)));
            Font font = Theme.PixelFont(size), bold = Theme.PixelFont(size, true);
            const TextFormatFlags Flags = TextFormatFlags.VerticalCenter | TextFormatFlags.NoPadding | TextFormatFlags.SingleLine;
            for (int u = 0; u < n; u++)
            {
                var row = Rectangle.FromLTRB(0, u * Height / n, Width, (u + 1) * Height / n);
                if (u == lit || u == pinned)
                    using (var back = new SolidBrush(u == lit ? Theme.Cell : Theme.Button)) g.FillRectangle(back, row);
                if (u == pinned)
                    using (var bar = new SolidBrush(Theme.Accent)) g.FillRectangle(bar, row.Left, row.Top, Theme.Px(2), row.Height);
                TextRenderer.DrawText(g, names[u], u == lit || u == pinned ? bold : font, new Rectangle(pad, row.Top, Width * 27 / 100, row.Height),
                                      Theme.KindText(kinds[u]), Flags);
                CurveChoice pick = picks[u];
                if (pick == null)
                {
                    TextRenderer.DrawText(g, "—", font, new Rectangle(Width * 27 / 100, row.Top, Width * 18 / 100, row.Height), Theme.Dim, Flags);
                    continue;
                }
                TextRenderer.DrawText(g, "×" + pick.Limit, font, new Rectangle(Width * 27 / 100, row.Top, Width * 18 / 100, row.Height), Theme.Text, Flags);
                if (delta[u] != 0)
                    TextRenderer.DrawText(g, (delta[u] > 0 ? "+" : "−") + Math.Abs(delta[u]), font, new Rectangle(Width * 45 / 100, row.Top, Width * 13 / 100, row.Height),
                                          delta[u] > 0 ? Theme.Good : Theme.Bad, Flags);
                bool both = !double.IsNaN(joint[u]);
                TextRenderer.DrawText(g, pick.Temp.ToString("0") + "°" + (both ? "→" : pick.Top ? " ▲" : ""), font,
                                      new Rectangle(Width * 58 / 100, row.Top, (both ? Width * 21 / 100 : Width * 42 / 100 - pad), row.Height),
                                      pick.Hot ? Theme.Bad : Theme.Dim, Flags | TextFormatFlags.Right);
                if (both)
                    TextRenderer.DrawText(g, joint[u].ToString("0") + "°", font, new Rectangle(Width * 79 / 100, row.Top, Width * 21 / 100 - pad, row.Height),
                                          Theme.Text, Flags | TextFormatFlags.Right);
            }
        }
    }

    /// График «средний множитель от температуры» над ползунком: шкала по горизонтали — та же, что у ползунка,
    /// так что ползунок показывает место на кривой. Считается из снятых кривых, без замеров.
    class MeanView : Painted
    {
        readonly int minimum, maximum;
        double[] mean = new double[0];    // средний множитель на каждый градус от minimum; NaN — кривых нет
        double[] best = new double[0];    // то же при переделе «по выгоде»
        bool useBest;                     // ползунок сейчас работает «по выгоде»: его точка стоит на второй линии
        bool[] rough = new bool[0];       // на этой температуре часть блоков горячее цели: среднее завышено
        string[] readout = new string[0];
        int value, knee = -1;

        public MeanView(int minimum, int maximum, int value)
        {
            this.minimum = minimum;
            this.maximum = maximum;
            this.value = value;
            Height = Theme.Px(62);
        }

        protected override bool Data { get { return true; } }

        /// knee — температура перегиба; -1 — не найден.
        public void Show(double[] mean, double[] best, bool useBest, bool[] rough, string[] readout, int knee)
        {
            this.mean = mean;
            this.best = best;
            this.useBest = useBest;
            this.rough = rough;
            this.readout = readout;
            this.knee = knee;
            Invalidate();
        }

        public int Value { set { if (this.value != value) { this.value = value; Invalidate(); } } }

        int Inset { get { return Theme.Px(7); } }  // как половина бегунка ползунка: шкалы совпадают

        int X(double t) { return Inset + (int)Math.Round((t - minimum) / (maximum - minimum) * (Width - 2 * Inset)); }

        protected override string TipAt(Point at)
        {
            int k = (int)Math.Round((at.X - Inset) / (double)Math.Max(1, Width - 2 * Inset) * (maximum - minimum));
            return k < 0 || k >= readout.Length ? null : readout[k];
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            g.SmoothingMode = System.Drawing.Drawing2D.SmoothingMode.AntiAlias;
            Font font = Theme.Font(10);
            double[] known = mean.Where(v => !double.IsNaN(v)).ToArray();
            if (known.Length == 0)
            {
                TextRenderer.DrawText(g, "Средний множитель от температуры появится, когда будут сняты кривые", Theme.Font(11), ClientRectangle, Theme.Dim,
                                      TextFormatFlags.HorizontalCenter | TextFormatFlags.VerticalCenter | TextFormatFlags.WordBreak);
                return;
            }
            double low = known.Min(), high = Math.Max(Math.Max(known.Max(), best.Where(v => !double.IsNaN(v)).DefaultIfEmpty(0).Max()), low + 0.5);
            int top = Theme.Px(14), bottom = Height - Theme.Px(4);
            Func<double, int> y = v => bottom - (int)Math.Round((v - low) / (high - low) * (bottom - top));
            using (var grid = new Pen(Theme.Cell))
            {
                g.DrawLine(grid, Inset, y(low), Width - Inset, y(low));
                g.DrawLine(grid, Inset, y(high), Width - Inset, y(high));
            }
            // Линия «по выгоде» — под линией «ровно»: где они совпадают, видна одна.
            using (var gain = new Pen(Theme.KindText('E'), Theme.Px(2)))
                for (int k = 1; k < best.Length; k++)
                    if (!double.IsNaN(best[k - 1]) && !double.IsNaN(best[k]))
                        g.DrawLine(gain, X(minimum + k - 1), y(best[k - 1]), X(minimum + k), y(best[k]));
            using (var sure = new Pen(Theme.Text, Theme.Px(2)))
            using (var unsure = new Pen(Theme.Dim, Theme.Px(1.5)) { DashStyle = System.Drawing.Drawing2D.DashStyle.Dot })
                for (int k = 1; k < mean.Length; k++)
                    if (!double.IsNaN(mean[k - 1]) && !double.IsNaN(mean[k]))
                        g.DrawLine(rough[k - 1] || rough[k] ? unsure : sure, X(minimum + k - 1), y(mean[k - 1]), X(minimum + k), y(mean[k]));
            if (knee >= minimum && knee <= maximum)
            {
                using (var pen = new Pen(Theme.Dim)) g.DrawLine(pen, X(knee), top, X(knee), bottom);
                TextRenderer.DrawText(g, "перегиб " + knee + "°", font, new Point(Math.Min(X(knee) + Theme.Px(3), Width - Theme.Px(70)), bottom - Theme.Px(13)),
                                      Theme.Dim, TextFormatFlags.NoPadding);
            }
            int at = value - minimum;
            using (var pen = new Pen(Theme.Accent)) g.DrawLine(pen, X(value), top, X(value), Height);
            double[] line = useBest && best.Length == mean.Length ? best : mean;
            if (at >= 0 && at < line.Length && !double.IsNaN(line[at]))
            {
                int r = Theme.Px(4);
                using (var dot = new SolidBrush(Theme.Accent)) g.FillEllipse(dot, X(value) - r, y(line[at]) - r, 2 * r, 2 * r);
            }
            string range = "средний множитель ×" + low.ToString("0.0") + " … ×" + high.ToString("0.0") + " · ровно";
            TextRenderer.DrawText(g, range, font, new Point(Inset, 0), Theme.Dim, TextFormatFlags.NoPadding);
            int after = Inset + TextRenderer.MeasureText(g, range, font, Size.Empty, TextFormatFlags.NoPadding).Width;
            TextRenderer.DrawText(g, " · по выгоде", font, new Point(after, 0), Theme.KindText('E'), TextFormatFlags.NoPadding);
        }
    }

    /// Полоса прогресса измерения.
    class RunBar : Painted
    {
        double fraction;

        public RunBar(int width)
        {
            Size = new Size(width, Theme.Px(10));
            Margin = new Padding(0, 0, 0, Theme.Px(4));
        }

        public void Set(double value)
        {
            if (value == fraction) return;
            fraction = value;
            Invalidate();
        }

        protected override string TipAt(Point at) { return null; }

        protected override void OnPaint(PaintEventArgs e)
        {
            using (var back = new SolidBrush(Theme.Cell)) e.Graphics.FillRectangle(back, ClientRectangle);
            using (var fill = new SolidBrush(Theme.Accent)) e.Graphics.FillRectangle(fill, 0, 0, (int)(Width * fraction), Height);
        }
    }

    /// Ползунок целого значения: двигается мышью (щелчок или перетаскивание) и колесом.
    /// Рисуется сам: системный ползунок светлый и на тёмном окне не читается.
    class Slider : Painted
    {
        readonly int minimum, maximum;
        readonly string tip;
        int value;

        public Action<int> Moved;  // пользователь поставил значение — в том числе щелчком на прежнее место

        public Slider(int minimum, int maximum, int value, string tip)
        {
            this.minimum = minimum;
            this.maximum = maximum;
            this.value = value;
            this.tip = tip;
            Height = Theme.Px(24);
            Cursor = Cursors.Hand;
        }

        public int Value
        {
            get { return value; }
            set { this.value = Math.Max(minimum, Math.Min(maximum, value)); Invalidate(); }
        }

        int Knob { get { return Theme.Px(7); } }  // половина ширины бегунка

        protected override string TipAt(Point at) { return tip; }

        void Take(int x, bool always)
        {
            double share = (x - Knob) / (double)Math.Max(1, Width - 2 * Knob);
            int taken = Math.Max(minimum, Math.Min(maximum, minimum + (int)Math.Round(share * (maximum - minimum))));
            if (taken == value && !always) return;
            value = taken;
            Invalidate();
            if (Moved != null) Moved(value);
        }

        protected override void OnMouseDown(MouseEventArgs e)
        {
            base.OnMouseDown(e);
            if (e.Button != MouseButtons.Left) return;
            Focus();  // колесо действует только после щелчка по ползунку: иначе прокрутка окна над ним меняла бы потолки
            Take(e.X, true);
        }

        protected override void OnMouseMove(MouseEventArgs e)
        {
            base.OnMouseMove(e);
            if (e.Button == MouseButtons.Left && Capture) Take(e.X, false);
        }

        protected override void OnMouseWheel(MouseEventArgs e)
        {
            base.OnMouseWheel(e);
            if (!Focused) return;
            int taken = Math.Max(minimum, Math.Min(maximum, value + Math.Sign(e.Delta)));
            if (taken == value) return;
            value = taken;
            Invalidate();
            if (Moved != null) Moved(value);
        }

        protected override void OnEnabledChanged(EventArgs e)
        {
            base.OnEnabledChanged(e);
            Invalidate();
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            g.SmoothingMode = System.Drawing.Drawing2D.SmoothingMode.AntiAlias;
            int knob = Knob, middle = Height / 2, line = Theme.Px(4);
            int at = knob + (int)Math.Round((value - minimum) / (double)(maximum - minimum) * (Width - 2 * knob));
            using (var back = new SolidBrush(Theme.Cell)) g.FillRectangle(back, knob, middle - line / 2, Width - 2 * knob, line);
            using (var fill = new SolidBrush(Enabled ? Theme.Accent : Theme.Dim)) g.FillRectangle(fill, knob, middle - line / 2, at - knob, line);
            using (var head = new SolidBrush(Enabled ? Theme.Text : Theme.Dim)) g.FillEllipse(head, at - knob, middle - knob, 2 * knob, 2 * knob);
        }
    }

    /// Переключатель из нескольких значений: одна нажатая кнопка.
    class Segmented : FlowLayoutPanel
    {
        readonly int[] values;
        readonly Button[] buttons;

        public Segmented(int[] values, Func<int, string> label, Action<int> picked, string tip)
        {
            this.values = values;
            AutoSize = true;
            WrapContents = false;
            Margin = new Padding(0, 0, Theme.Px(10), 0);
            buttons = values.Select(v =>
            {
                Button b = Widgets.Button(label(v), () => picked(v), tip);
                b.Margin = new Padding(0, 0, 1, 0);
                return b;
            }).ToArray();
            Controls.AddRange(buttons);
        }

        public void Show(int current, bool enabled = true)
        {
            for (int k = 0; k < values.Length; k++)
            {
                // Присваивание цвета перерисовывает кнопку, даже если цвет прежний: без проверки тринадцать кнопок
                // рисовались заново каждые полсекунды.
                Color fore = values[k] == current ? Theme.Accent : Theme.Text, back = values[k] == current ? Theme.Cell : Theme.Button;
                if (buttons[k].ForeColor != fore) buttons[k].ForeColor = fore;
                if (buttons[k].BackColor != back) buttons[k].BackColor = back;
                if (buttons[k].Enabled != enabled) buttons[k].Enabled = enabled;
            }
        }
    }

    /// Кнопка окна. Неактивную рисует сама: системная отрисовка берёт для её надписи затемнённый цвет фона,
    /// и на тёмном фоне выходит чёрное по чёрному. Здесь надпись неактивной кнопки — приглушённый цвет активной.
    class PlainButton : Button
    {
        protected override void OnPaint(PaintEventArgs e)
        {
            if (Enabled) { base.OnPaint(e); return; }
            Color f = ForeColor, b = BackColor;
            Color muted = Color.FromArgb((f.R * 45 + b.R * 55) / 100, (f.G * 45 + b.G * 55) / 100, (f.B * 45 + b.B * 55) / 100);
            using (var back = new SolidBrush(b)) e.Graphics.FillRectangle(back, ClientRectangle);
            TextRenderer.DrawText(e.Graphics, Text, Font, ClientRectangle, muted,
                TextFormatFlags.HorizontalCenter | TextFormatFlags.VerticalCenter | TextFormatFlags.SingleLine);
        }
    }

    static class Widgets
    {
        public static Button Button(string text, Action click, string tip)
        {
            var b = new PlainButton
            {
                Text = text, FlatStyle = FlatStyle.Flat, BackColor = Theme.Button, ForeColor = Theme.Text, Font = Theme.Font(13),
                AutoSize = true, AutoSizeMode = AutoSizeMode.GrowAndShrink, Cursor = Cursors.Hand, TabStop = false,
                Padding = new Padding(Theme.Px(4), Theme.Px(1), Theme.Px(4), Theme.Px(1)), Margin = new Padding(0, 0, Theme.Px(8), 0),
            };
            b.FlatAppearance.BorderSize = 0;
            b.Click += (s, e) => click();
            Theme.Tips.SetToolTip(b, tip);
            return b;
        }

        public static Label Label(string text, Color color, double size = 13, bool bold = false, string tip = null)
        {
            var l = new Label
            {
                Text = text, ForeColor = color, Font = Theme.Font(size, bold), AutoSize = true, Margin = new Padding(0, 0, Theme.Px(12), 0),
                TextAlign = ContentAlignment.MiddleLeft,
            };
            if (tip != null) Theme.Tips.SetToolTip(l, tip);
            return l;
        }

        public static FlowLayoutPanel Row()
        {
            return new FlowLayoutPanel { AutoSize = true, WrapContents = false, Margin = new Padding(0, 0, 0, Theme.Px(10)) };
        }

        public static FlowLayoutPanel Column(Color back, int padding)
        {
            return new FlowLayoutPanel
            {
                AutoSize = true, WrapContents = false, FlowDirection = FlowDirection.TopDown, BackColor = back,
                Padding = new Padding(Theme.Px(padding)), Margin = new Padding(0, 0, Theme.Px(14), 0),
            };
        }
    }
}
