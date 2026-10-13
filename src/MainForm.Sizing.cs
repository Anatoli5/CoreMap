// Главное окно: размер содержимого под размер окна. Схема, таблицы и графики масштабируются одним числом —
// размером ячейки таблиц; подбирается наибольший, при котором всё помещается без прокрутки.
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm
    {
        // Размер ячейки таблиц до масштаба Windows: наименьший, обычный (под него открывается окно) и наибольший.
        const int MinCell = 16, NormalCell = 26, MaxCell = 40;

        FlowLayoutPanel root, topBar, body;
        FlowLayoutPanel leftTop;  // схема и потолки в ряд; под ними — таблица «Под одинаковой нагрузкой»
        Control[] leaves;         // панели окна по отдельности: по ним проверяется, что самая широкая входит в окно
        List<Control> flows;      // все панели-раскладчики окна: на время смены размеров их раскладка приостанавливается
        bool built, fitting;      // окно собрано; идёт подгонка
        bool sizing;              // владелец тянет границу окна: подгонка откладывается до конца
        Size sizedFrom;           // размер клиентской области в начале перетаскивания
        Size fittedFor;           // размер окна, под который содержимое подогнано в последний раз

        protected override void OnClientSizeChanged(EventArgs e)
        {
            base.OnClientSizeChanged(e);
            // Пока тянут границу, подгонка не идёт: каждая перебирает несколько размеров ячейки с полной раскладкой окна.
            // Заново она идёт, только когда изменился размер самого окна. Клиентская область меняется ещё и оттого,
            // что надпись выросла на строку и появилась полоса прокрутки: подгонка по этому поводу переводила всё
            // окно на другой размер ячеек — оно «прыгало» из-за одной строки текста.
            if (!sizing && Size != fittedFor) Fit();
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

        /// Размеры всех разделов под ячейку: каждый раздел ставит свои.
        void ApplySizes(int cell)
        {
            SizeDie(cell);
            SizeThermal(cell);
            // Колонка потолков — шириной со схему, но не уже своей самой широкой строки: шрифт с ячейкой не мельчает.
            int list = blockList != null ? Theme.Px(170) + blockList.Margin.Horizontal : 0;  // список блоков справа от графика
            int side = Math.Max(die.Width, curveRow != null ? curveRow.PreferredSize.Width : 0) + list;
            if (profileStatus != null) SizeProfiles(side);
            if (curveView != null) SizeLimits(side, list, cell);
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
            fittedFor = Size;
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
    }
}
