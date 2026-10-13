// Сценарий запуска: что программа делает сама после старта и когда закрывается.
//
// Сценарии нужны агенту — проверять окно скрытыми снимками на имитации (tools\check.ps1) — и двум ключам
// обычной работы, --run и --calibrate. Ключи описаны в шапке Program.cs. Окно о сценариях не знает, кроме
// одного: скрытое оно или обычное. Сценарий считает время своим таймером и по часам, а не по обновлениям
// окна, и пользуется окном так же, как владелец, — через его команды.
// За запуск выполняется один сценарий: снимок (--snapshot), иначе подбор нагрузки (--calibrate), иначе
// тепловое измерение (--run).
using System;
using System.Drawing;
using System.Windows.Forms;

namespace CoreMap
{
    class Scenario
    {
        const double StartAfter = 0.5;  // через столько секунд после сборки окна сценарий делает своё действие
        const double ShotAfter = 29;    // на имитации снимок снимается через столько секунд
        // На настоящих датчиках снимок снимается после трёх измеренных блоков, но не раньше и не позже этого.
        const double ShotNoSooner = 10, ShotNoLater = 240;

        readonly string snapshot;  // куда сохранить картинку окна; null — снимок не нужен
        readonly bool calibrate, run, curve, fit, joint, best, profile, hover;
        readonly int slide;        // на сколько градусов поставить ползунок потолков; 0 — не ставить
        readonly int fans;         // режим вентиляторов (Fans.Measure, Fans.Always); 0 — не менять

        public readonly Size Size;   // размер окна; пустой — обычный
        public readonly bool Saved;  // на имитации показать сохранённые настоящие замеры и кривые

        MainForm form;
        Core core;
        Timer timer;
        double attachedAt;
        bool begun;

        /// Окно для скрытого снимка: стоит за краем экрана, ввод не забирает, при закрытии ни о чём не спрашивает.
        public bool Hidden { get { return snapshot != null; } }

        public Scenario(string[] args, bool simulate)
        {
            Func<string, bool> has = name => Array.IndexOf(args, name) >= 0;
            snapshot = Program.Option(args, "--snapshot");
            calibrate = has("--calibrate");
            run = has("--run");
            Saved = has("--saved");
            // Остальные ключи — только для имитации: на настоящем процессоре они двигали бы потолки и грели ядра.
            curve = simulate && has("--curve");
            fit = simulate && has("--fit");
            joint = simulate && has("--joint");
            best = simulate && has("--best");
            profile = simulate && has("--profile");
            hover = simulate && has("--hover");
            if (simulate) int.TryParse(Program.Option(args, "--slide"), out slide);
            if (simulate) int.TryParse(Program.Option(args, "--fans"), out fans);
            string[] size = (Program.Option(args, "--size") ?? "").Split('x');
            int width = 0, height = 0;
            if (size.Length == 2) { int.TryParse(size[0], out width); int.TryParse(size[1], out height); }
            if (width > 0 && height > 0) Size = new Size(width, height);
        }

        /// Запускает сценарий для собранного окна. Без ключей сценария ничего не делает.
        public void Attach(MainForm form, Core core)
        {
            this.form = form;
            this.core = core;
            // Подбор нагрузки и тепловое измерение без датчиков не начнутся: окно остаётся обычным.
            if (snapshot == null && (core.Monitor == null || !(calibrate || run))) return;
            attachedAt = Native.Now();
            timer = new Timer { Interval = 250 };
            timer.Tick += (s, e) => Step();
            timer.Start();
        }

        void Step()
        {
            double passed = Native.Now() - attachedAt;
            if (passed < StartAfter) return;
            if (!begun)
            {
                begun = true;  // до действия: его ошибка не должна повторять действие каждые четверть секунды
                Begin();
                return;
            }
            if (snapshot != null)
            {
                // Измерению дают дойти до первых результатов. Счёт блоков до запуска — от прошлой сохранённой
                // таблицы, поэтому первые секунды он не в счёт.
                bool ready = core.Simulated ? passed >= ShotAfter
                    : passed >= ShotNoSooner && core.Sweep.Done >= 3 || passed >= ShotNoLater;
                if (ready && hover) Finish(() => System.IO.File.WriteAllText(snapshot, form.HoverCheck()));
                else if (ready) Finish(() => form.SavePicture(snapshot));
            }
            else if (calibrate)
            {
                if (!core.Calibration.Running) Finish(null);
            }
            else timer.Stop();  // --run: измерение запущено, окно остаётся открытым
        }

        void Begin()
        {
            if (snapshot == null)
            {
                if (calibrate) core.StartCalibration();
                else form.SweepClick();
                return;
            }
            if (fans > 0) core.SetFanMode(fans);
            if (hover) return;  // проверка наведения: измерение не нужно, график берётся как есть
            if (profile) TwoProfiles();
            else if (slide > 0)
            {
                form.SlideTo(slide, best);
                if (joint) core.StartJoint();
            }
            else if (joint) core.StartJoint();
            else if (fit) core.FitLimits(47);
            else if (curve) core.StartCurve(form.Selected, double.NaN);
            else core.StartSweep(10, 1);
        }

        /// Два профиля, второй сохранён последним (он «было»), первый открыт поверх него: на картинке видны пометки и разница.
        void TwoProfiles()
        {
            Block[] blocks = core.Blocks.All;
            int a = blocks[0].First, b = blocks[2].First, c = blocks[5].First;
            int top = core.Limits.Current[a];
            core.SetLimit(a, top - 3);
            core.SetLimit(b, core.Limits.Current[b] - 2);
            core.SaveProfile("Тихий");
            core.SetLimit(a, top);
            core.SetLimit(c, core.Limits.Current[c] + 1);
            core.SaveProfile("Игровой");
            form.OpenProfile("Тихий");
        }

        /// Конец сценария: последнее действие и закрытие окна. Закрыться нужно и при неудаче действия: иначе
        /// невидимая копия программы осталась бы висеть.
        void Finish(Action last)
        {
            timer.Stop();
            try { if (last != null) last(); }
            finally { form.Close(); }
        }
    }
}
