// Главное окно: схема кристалла. Плитка на блок: щелчок — нагрузка и выбор блока, правая кнопка — меню потолка.
using System;
using System.Drawing;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm
    {
        DieView die;
        Label dieNote;   // приписка под схемой, если расположение ядер не подтверждено; null — её нет
        Block selected;  // выбранный (закреплённый) блок: его кривая поверх остальных, пока курсор не на другом блоке
        Block hovered;   // блок под курсором — на схеме, в списке блоков или на графике; null — нет

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

        void SizeDie(int cell)
        {
            die.SetZoom((double)cell / Theme.Px(NormalCell));
            if (dieNote != null) dieNote.MaximumSize = new Size(die.Width, 0);
        }

        void UpdateDie(bool[] loaded)
        {
            Monitor m = core.Monitor;
            if (m != null)
            {
                die.Cur = m.Cur;
                die.Max = m.Max;
                die.Ratio = m.Ratio;
                die.Throttled = m.Throttled;
            }
            die.Loaded = loaded;
            die.Avg = core.LoadAverage;
            die.Invalidate();
        }

        /// Выбранный блок: его кривая снимается кнопкой ▶.
        public Block Selected { get { return selected; } }

        void Select(Block block)
        {
            selected = block;
            if (die != null) die.Selected = block;
            drawnKey = null;
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

        string TileTip(Block block)
        {
            // Числа подсказки — те же, что на плитке под курсором: наибольшее по ядрам блока (DieView.Highest).
            // Потолок у блока общий и читается по первому ядру.
            int i = block.First;
            int[] members = block.Members;
            double cur = DieView.Highest(die.Cur, members), max = DieView.Highest(die.Max, members);
            double average = DieView.Highest(die.Avg, members);
            double ratio = DieView.Highest(die.Ratio, members), loadRatio = DieView.Highest(core.LoadRatio, members);
            string text = block.Members.Length > 1
                ? Capital(block.Title) + "\n" +
                    (block.Members.Any(k => die.Loaded[k]) ? "Щелчок снимает нагрузку со всего блока." : "Щелчок даёт нагрузку на все ядра блока разом.")
                : Capital(block.Title) + " (" + block.Kind + ")\n" +
                    (die.Loaded[i] ? "Щелчок снимает нагрузку с этого ядра." : "Щелчок даёт нагрузку на это ядро.");
            if (!double.IsNaN(cur)) text += "\n• сейчас: " + cur + " °C\n• ▲ максимум с последнего сброса: " + max + " °C";
            if (!double.IsNaN(average))
                text += "\n• ⌀ средняя под нагрузкой: " + average.ToString("0.0") + " °C — считается, только пока это ядро само " +
                    "нагружено, с момента, когда последний раз менялись нагруженные ядра, сила нагрузки или потолки; без нагрузки стоит на месте";
            if (members.Any(k => die.Throttled[k])) text += "\n• сейчас сбрасывает частоту из-за перегрева";
            if (!double.IsNaN(ratio))
                text += "\n• сейчас работает на множителе " + ratio.ToString("0.0") +
                    (double.IsNaN(loadRatio) ? "" : ", в среднем под нагрузкой — " + loadRatio.ToString("0.0"));
            if (core.Limits != null && core.Limits.Known(i))
                text += "\n• потолок множителя: ×" + core.Limits.Current[i] + ", заводской предел ×" + core.Limits.Fused[i] +
                    (core.Limits.Current[i] == core.BaseLimits[i] ? "" : "\n• в базе ×" + core.BaseLimits[i] + ": число после потолка на плитке — разница с базой") +
                    "\n• правая кнопка — изменить потолок" + (block.Kind == 'E' ? " (общий для кластера)" : "") +
                    "\n• потолок выше заводского показан красным; частоту ограничивает ещё таблица «по числу работающих ядер»";
            if (core.Busy) text += "\n• идёт измерение: ручная нагрузка недоступна";
            return text;
        }
    }
}
