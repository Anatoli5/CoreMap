// Что движок говорит окну по мосту: сообщения, собранные из Core.
//
// Сообщений два рода.
// • «Состояние» (state) — показания, которые меняются сами: температуры, множители, нагрузка, потолки, ход
//   измерения. Уходит после каждого опроса датчиков и сразу после исполненной команды. Пишется по знаку в
//   готовый буфер (Wire.Out), без деревьев и строк: см. шапку Wire.cs.
// • Всё остальное шлётся один раз при знакомстве и дальше только когда изменилось: hello (что за процессор и
//   схема), limits (заводские потолки, «было», допустимые значения), profiles (профили и что из них стоит),
//   notice (сообщение владельцу), run.finished (измерение кончилось). Меняются они редко, поэтому собираются
//   обычной записью (Json.cs).
// Темы результатов измерений (кривые, тепловая таблица, задержки…) появятся вместе с разделами нового окна,
// которые их читают: форму темы задаёт её читатель (DECISIONS.md, п. 25).
//
// Номера блоков и ядер в сообщениях — порядковые (Block.Index, место ядра в общем списке); ссылок на объекты
// движка у окна нет. Тексты, которые движок и так составляет для владельца (отказы, срабатывание защиты,
// описание профиля запуска), идут готовыми строками; подписи из чисел окно составляет само.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;

namespace CoreMap
{
    class Feed
    {
        readonly Core core;
        readonly object gate = new object();
        readonly double startedAt = Native.Now();
        readonly double[] none;       // «показаний нет» по всем ядрам: датчики недоступны
        readonly bool[] nobody;
        int states;                   // сколько сообщений «состояние» собрано
        int reported;                 // номер измерения, о конце которого окну уже сказано
        int noticeSent;               // номер последнего отправленного сообщения владельцу
        int[] sentFused, sentWas, seenCurrent;
        string sentProfiles;
        volatile bool touched;        // исполнена команда: профили могли измениться
        string sentUnread;

        /// Почему потолки не перечитались из процессора два раза подряд; null — перечитались. Тогда в состоянии
        /// стоят прежние потолки, а не то, что в процессоре. Ставит движок (Engine.Changed).
        public volatile string Unread;
        bool wasBusy;

        public Feed(Core core)
        {
            this.core = core;
            none = Enumerable.Repeat(double.NaN, core.Lps.Length).ToArray();
            nobody = new bool[core.Lps.Length];
        }

        /// Исполнена команда: то, что шлётся по изменению, стоит пересчитать, не дожидаясь признаков.
        public void Touch() { touched = true; }

        // ----- по изменению -----

        /// Всё, что окно должно знать сразу после знакомства.
        public IEnumerable<string> Greeting()
        {
            lock (gate)
            {
                var messages = new List<string> { Hello() };
                // Измерение, которое кончилось без окна, новому окну событием не приходит: его итог — в темах результатов.
                reported = core.Runner.Active == null ? core.Runner.Runs : core.Runner.Runs - 1;
                wasBusy = core.Busy;
                touched = false;
                if (core.Limits != null)
                {
                    messages.Add(Limits());
                    seenCurrent = (int[])core.Limits.Current.Clone();
                }
                if (core.Profile != null) messages.Add(sentProfiles = Profiles());
                // Сообщения владельцу, которые прошлое окно показать не успело (или его не было), — заново.
                noticeSent = 0;
                Notices(messages);
                return messages;
            }
        }

        /// Что изменилось с прошлого вызова; null — ничего.
        public IEnumerable<string> Changed()
        {
            lock (gate)
            {
                var messages = new List<string>();
                int runs = core.Runner.Runs;
                bool busy = core.Busy;
                if (!busy && runs > reported)
                {
                    reported = runs;
                    Measurement last = core.Runner.Last;
                    if (last != null)
                        messages.Add(Json.Text(new Dictionary<string, object>
                        {
                            { "t", "run.finished" }, { "run", runs }, { "kind", Kind(last) }, { "error", last.Error }, { "finished", last.Finished },
                        }));
                }
                if (core.Limits != null)
                {
                    if (!Same(sentFused, core.Limits.Fused) || !Same(sentWas, core.Was) || Unread != sentUnread) messages.Add(Limits());
                    // Профили пересчитываются не на каждый опрос: только после команды, смены потолков или начала и
                    // конца измерения — больше им меняться не от чего.
                    bool moved = !Same(seenCurrent, core.Limits.Current);
                    if (core.Profile != null && (touched || moved || busy != wasBusy))
                    {
                        touched = false;
                        if (moved) seenCurrent = (int[])core.Limits.Current.Clone();
                        string profiles = Profiles();
                        if (profiles != sentProfiles) messages.Add(sentProfiles = profiles);
                    }
                }
                wasBusy = busy;
                Notices(messages);
                return messages.Count == 0 ? null : messages;
            }
        }

        static bool Same(int[] a, int[] b)
        {
            if (a == null || b == null || a.Length != b.Length) return a == b;
            for (int i = 0; i < a.Length; i++)
                if (a[i] != b[i]) return false;
            return true;
        }

        void Notices(List<string> messages)
        {
            if (Program.NoticeLast <= noticeSent) return;
            foreach (KeyValuePair<int, string> notice in Program.NoticesAfter(noticeSent))
            {
                messages.Add(Json.Text(new Dictionary<string, object> { { "t", "notice" }, { "seq", notice.Key }, { "text", notice.Value } }));
                noticeSent = notice.Key;
            }
        }

        static Dictionary<string, object> Map(params object[] pairs)
        {
            var map = new Dictionary<string, object>();
            for (int i = 0; i < pairs.Length; i += 2) map[(string)pairs[i]] = pairs[i + 1];
            return map;
        }

        string Hello()
        {
            DieLayout layout = core.Layout;
            return Json.Text(Map(
                "t", "hello", "proto", Bridge.Protocol, "build", Program.Build, "simulated", core.Simulated, "cpu", core.CpuName,
                "cores", core.Cores.Select(c => (object)Map("lp", c.Lp, "kind", c.Kind.ToString(), "cluster", c.Cluster)).ToList(),
                "blocks", core.Blocks.All.Select(b => (object)Map("index", b.Index, "members", b.Members, "first", b.First, "lp", b.Lp,
                    "kind", b.Kind.ToString(), "name", b.Name, "title", b.Title)).ToList(),
                "layout", Map("name", layout.Name, "note", layout.Note, "confirmed", layout.Confirmed, "cols", layout.Cols, "rows", layout.Rows,
                    "tiles", layout.Tiles.Select(t => (object)Map("block", t.Block.Index, "col", t.Col, "row", t.Row, "places", t.Places)).ToList()),
                // Чего у движка нет: без датчиков нагрузка не даётся, без потолков нет ни профилей, ни кривых.
                "sensors", core.Monitor != null, "sensorProblem", core.SensorProblem, "limits", core.Limits != null, "curves", core.Curve != null,
                "guard", Map("limit", core.Limit, "throttleAt", core.ThrottleAt),
                // Границы, в которых движок принимает команды: окно предлагает владельцу только то, что будет принято.
                "levels", LoadManager.Levels, "durations", ThermalSweep.Durations, "passes", ThermalSweep.PassChoices,
                "fitLow", CurveFit.Low, "fitHigh", CurveFit.High, "over", CoreLimits.Over));
        }

        string Limits()
        {
            sentFused = (int[])core.Limits.Fused.Clone();
            sentWas = (int[])core.Was.Clone();
            sentUnread = Unread;
            return Json.Text(Map(
                "t", "limits", "fused", sentFused, "was", sentWas, "unread", sentUnread,
                // Что можно поставить блоку вручную, по убыванию. Правило — у CoreLimits; окно его не повторяет.
                "choices", core.Blocks.All.Select(b => (object)core.Limits.Choices(b.First)).ToList()));
        }

        string Profiles()
        {
            Profile startup = core.Profile;
            string preferred = core.Preferred;
            return Json.Text(Map(
                "t", "profiles",
                "startup", Map("exists", startup.Exists, "name", startup.Name, "state", startup.State, "disabled", startup.Disabled,
                    "describe", startup.Describe(), "details", startup.Details(), "note", startup.Note),
                "sets", core.Profiles.Names.Select(n => (object)Map("name", n, "ratios", core.Profiles.Get(n))).ToList(),
                // Какой сохранённый набор стоит сейчас и каким было «было»: название, "" — заводские, null — ни один.
                "standing", core.Standing(preferred), "wasNamed", core.Named(core.Was, preferred), "canAccept", core.CanAccept(preferred)));
        }

        // ----- состояние -----

        /// Название рода измерения для окна.
        static string Kind(Measurement m)
        {
            return m is ThermalSweep ? "sweep" : m is LatencyTest ? "latency" : m is Calibration ? "calibration"
                : m is CurveTest ? "curve" : m is LimitSearch ? "search" : m is JointTest ? "joint" : "other";
        }

        /// Пишет сообщение «состояние». false — сейчас его слать не надо.
        public bool State(StringBuilder s)
        {
            Monitor m = core.Monitor;
            Measurement run = core.Runner.Active;
            s.Append("{\"t\":\"state\"");
            Out.Key(s, "n"); Out.Int(s, ++states);
            Out.Key(s, "up"); Out.Fix(s, Native.Now() - startedAt, 1);
            Out.Key(s, "pkg"); Out.Fix(s, m == null ? double.NaN : m.PkgCur, 0);
            Out.Key(s, "pkgMax"); Out.Fix(s, m == null ? double.NaN : m.PkgMax, 0);
            Out.Key(s, "watts"); Out.Fix(s, m == null ? double.NaN : m.Watts, 1);
            Out.Key(s, "since"); Out.Fix(s, m == null ? double.NaN : (DateTime.Now - m.ResetAt).TotalSeconds, 0);
            Out.Key(s, "busy"); Out.Bool(s, run != null);
            Out.Key(s, "level"); Out.Int(s, core.Load.Level);
            Out.Key(s, "cur"); Out.Row(s, m == null ? none : m.Cur, 0);
            Out.Key(s, "max"); Out.Row(s, m == null ? none : m.Max, 0);
            Out.Key(s, "ratio"); Out.Row(s, m == null ? none : m.Ratio, 1);
            Out.Key(s, "avg"); Out.Row(s, core.LoadAverage, 1);
            Out.Key(s, "avgRatio"); Out.Row(s, core.LoadRatio, 1);
            Out.Key(s, "thr"); Out.Row(s, m == null ? nobody : m.Throttled);
            Out.Key(s, "load"); Out.Row(s, core.Load.Active());
            Out.Key(s, "limit");
            if (core.Limits == null) s.Append("null");
            else Out.Row(s, core.Limits.Current);

            Fans fans = core.Fans;
            int[] rpm = fans.Rpm;
            Out.Key(s, "fans").Append('{');
            Out.Key(s, "rpm");
            if (rpm == null) s.Append("null");
            else Out.Row(s, rpm);
            Out.Key(s, "max"); Out.Bool(s, fans.Max);
            Out.Key(s, "spin"); Out.Bool(s, fans.Spinning);
            Out.Key(s, "mode"); Out.Int(s, fans.Mode);
            s.Append('}');

            Out.Key(s, "run");
            if (run == null) s.Append("null");
            else Run(s, run);
            Out.Key(s, "trip"); Out.Str(s, core.Trip);
            Out.Key(s, "sensorError"); Out.Str(s, m == null ? null : m.Error);
            s.Append('}');
            return true;
        }

        /// Ход измерения: у каждого рода свои счётчики, окну они идут под общими названиями; чего у рода нет — ноль.
        void Run(StringBuilder s, Measurement run)
        {
            int done = 0, total = 0, blockDone = 0, blockTotal = 0, stepDone = 0, stepTotal = 0;
            Block block = null;
            string text = null;
            var sweep = run as ThermalSweep;
            var steps = run as StepTest;
            var joint = run as JointTest;
            var calibration = run as Calibration;
            var latency = run as LatencyTest;
            if (sweep != null) { done = sweep.Done; total = sweep.Total; block = sweep.Current; }
            else if (steps != null)
            {
                done = steps.Done; total = steps.Total; blockDone = steps.BlockDone; blockTotal = steps.BlockTotal;
                stepDone = steps.StepDone; stepTotal = steps.StepTotal; block = steps.Active; text = steps.Current;
            }
            else if (joint != null) { done = joint.Done; total = joint.Total; text = joint.Current; }
            else if (calibration != null) { done = calibration.Done; total = calibration.Total; text = calibration.Current; }
            else if (latency != null) { done = latency.Done; total = latency.Total; }

            s.Append('{');
            Out.Key(s, "n"); Out.Int(s, core.Runner.Runs);
            Out.Key(s, "kind"); Out.Str(s, Kind(run));
            Out.Key(s, "block"); Out.Int(s, block == null ? -1 : block.Index);
            Out.Key(s, "done"); Out.Int(s, done);
            Out.Key(s, "total"); Out.Int(s, total);
            Out.Key(s, "blockDone"); Out.Int(s, blockDone);
            Out.Key(s, "blockTotal"); Out.Int(s, blockTotal);
            Out.Key(s, "stepDone"); Out.Int(s, stepDone);
            Out.Key(s, "stepTotal"); Out.Int(s, stepTotal);
            Out.Key(s, "elapsed"); Out.Fix(s, Native.Now() - core.Runner.StartedAt, 1);
            Out.Key(s, "text"); Out.Str(s, text);
            s.Append('}');
        }
    }
}
