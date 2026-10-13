// Главное окно, верхняя полоса: обороты вентиляторов и выбор, когда программа сама включает им «максимум».
// Сами вентиляторы — в Fans.cs; здесь только показ и кнопка выбора режима.
using System;
using System.Drawing;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm
    {
        const string FanTip = "Обороты вентиляторов\nСкорость двух вентиляторов ноутбука сейчас, как её сообщает прошивка.\n" +
            "• первое число — вентилятор процессора, второе — видеокарты; шаг показаний 100 об/мин\n" +
            "• «макс» — включён режим «максимум», «раскрутка» — он только что включён перед измерением\n" +
            "• обороты записываются в каждую точку кривой и в остальные замеры: по ним видно, в одинаковых ли условиях они сняты";

        Label fanSpeed;
        Button fanMode;        // показывает выбранный режим, по щелчку — меню выбора (как ▾ у профилей)
        string fanState = "";  // что о доступности оборотов сказано в подсказке сейчас

        static string FanModeName(int mode) { return mode == Fans.Auto ? "авто" : mode == Fans.Measure ? "макс при замерах" : "макс всегда"; }

        /// Обороты и выбор режима вентиляторов — одним куском верхней полосы. Здесь же начинается опрос
        /// оборотов: без окна он не идёт.
        void FanControls(FlowLayoutPanel bar)
        {
            bar.Controls.Add(fanSpeed = Widgets.Label("", Theme.Dim, 13, false, FanTip));
            Hold(fanSpeed, "5500 / 5500 об/мин");
            // Одна кнопка с меню, а не три кнопки в ряд: с тремя верхняя полоса в окне 1280×800 не помещалась в строку.
            bar.Controls.Add(fanMode = Widgets.Button("", FanMenu,
                "Вентиляторы на время замеров\nВыбирает, когда программа сама включает вентиляторам режим «максимум».\n" +
                "• авто — программа вентиляторы не трогает: ими управляет ноутбук\n" +
                "• макс при замерах — включает максимум перед измерением, которое греет процессор (кривые, подбор замером, " +
                "проверка под общей нагрузкой, тепловое измерение, подбор нагрузки), ждёт раскрутки и после измерения возвращает то, что было\n" +
                "• макс всегда — держит максимум, пока программа открыта\n" +
                "• зачем: на автоматике обороты идут за температурой, и одна и та же ступень в разных прогонах выходит разной; " +
                "при постоянных оборотах кривые сравнимы между собой\n" +
                "• выбор запоминается; пока идёт измерение, он не меняется\n" +
                "• если программу сняли при включённом ею максимуме, следующий запуск снимет его сам"));
            core.Fans.Start();
        }

        /// Меню под кнопкой режима — того же вида, что список профилей (ProfileMenu): выбранный режим помечен словом.
        void FanMenu()
        {
            var menu = new ContextMenuStrip { ShowImageMargin = false, ShowCheckMargin = false, Font = Theme.Font(13) };
            foreach (int mode in new[] { Fans.Auto, Fans.Measure, Fans.Always })
            {
                int picked = mode;
                var item = new ToolStripMenuItem(FanModeName(picked) + (core.Fans.Mode == picked ? "  · выбрано" : "")) { Enabled = !core.Busy };
                item.Click += (s, e) => Report(core.SetFanMode(picked));
                menu.Items.Add(item);
            }
            menu.Closed += (s, e) => BeginInvoke((Action)menu.Dispose);
            menu.Show(fanMode, new Point(0, fanMode.Height));
        }

        void UpdateFans(bool busy)
        {
            Fans fans = core.Fans;
            int[] rpm = fans.Rpm;
            string text = rpm == null ? "– об/мин"
                : string.Join(" / ", rpm) + " об/мин" + (fans.Spinning ? " · раскрутка" : fans.Max ? " · макс" : "");
            Color color = rpm == null ? Theme.Dim : fans.Max ? Theme.Accent : Theme.Text;
            // Обороты читаются раз в секунду во время измерения и раз в пять секунд в покое (Fans.Period), а окно
            // обновляется дважды в секунду: без проверки надпись и кнопка рисовались бы зря.
            if (fanSpeed.Text != text) fanSpeed.Text = text;
            if (fanSpeed.ForeColor != color) fanSpeed.ForeColor = color;
            string mode = FanModeName(fans.Mode) + " ▾";
            bool free = fans.Supported && !busy;
            if (fanMode.Text != mode) fanMode.Text = mode;
            if (fanMode.Enabled != free) fanMode.Enabled = free;
            // Почему оборотов нет — одной строкой в подсказке: программа работает и без вентиляторов.
            string state = fans.Error == null ? "" : "\n• " + (fans.Supported ? "сейчас обороты не читаются: " : "на этой машине обороты недоступны: ") + fans.Error;
            if (state == fanState) return;
            fanState = state;
            Theme.Tips.SetToolTip(fanSpeed, FanTip + state);
        }
    }
}
