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
    }
}
