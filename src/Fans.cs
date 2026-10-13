// Вентиляторы ноутбука — всё, что их касается: обороты, режим «максимум», выбор владельца («авто / макс при
// замерах / макс всегда»), максимум на время измерения и возврат прежнего после него.
//
// Зачем. Вентиляторы крутятся по температуре, и одна и та же ступень кривой при разных оборотах даёт разную
// температуру: повторяемость кривых между прогонами одного вечера — около 4 °C, и обороты при этом не
// записывались. Поэтому обороты пишутся в каждый результат измерения (FanStat), а на время измерения их можно
// закрепить на максимуме.
//
// Доступ — служебный интерфейс прошивки HP в WMI (HpFans); что о нём известно и откуда — в
// outputs\fan-interface-2026-10-13.md. На машине без него программа работает как раньше: «обороты
// недоступны», одной строкой в подсказке. В прошивку пишется одно: «максимум» включить или выключить (на
// платах, где этого требует снятие максимума, — ещё «уровень выбирает автоматика»: см. HpFans).
// Ручной уровень, пределы мощности и режимы производительности не пишутся.
//
// Все обращения к прошивке идут из одного своего потока. Вызов WMI может зависнуть, и ждать его не должны ни
// поток датчиков (на нём защита от перегрева), ни окно, ни завершение измерения. Остальные потоки только
// говорят, чего хотят (SetMode, Engage, Release, Close), и ждут исполнения не дольше нескольких секунд.
//
// На случай, когда программу сняли при включённом ею максимуме, лежит файл-метка data\fans-held.json: он
// пишется до записи в прошивку и удаляется после снятия максимума. Следующий запуск, найдя метку, снимает
// максимум (если выбор владельца — не «макс всегда») и говорит об этом.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Management;
using System.Threading;

namespace CoreMap
{
    /// Вентиляторы не ответили или отказали.
    class FanException : Exception
    {
        public FanException(string message) : base(message) { }
    }

    /// Прошивка ответила на запрос кодом отказа: такого запроса у неё нет или он ей не подходит.
    class FanRefusal : FanException
    {
        public FanRefusal(string message) : base(message) { }
    }

    /// Обороты вентиляторов за участок измерения. Хранятся по каждому вентилятору: автоматика ведёт их
    /// порознь (один — по процессору, другой — по видеокарте), и какой из них сколько даёт температуре ядер,
    /// неизвестно. В строках об условиях снятия показывается их среднее.
    class FanStat
    {
        public int[] Rpm;  // средние обороты каждого вентилятора за участок, об/мин
        public bool Max;   // режим «максимум» был включён весь участок

        public double Mean { get { return Rpm.Average(); } }

        /// Один участок из двух (подъём и спуск лесенки): обороты — среднее, «максимум» — если был на обоих.
        public static FanStat Both(FanStat a, FanStat b)
        {
            if (a == null || b == null || a.Rpm.Length != b.Rpm.Length) return a ?? b;
            return new FanStat { Rpm = a.Rpm.Select((v, k) => (v + b.Rpm[k]) / 2).ToArray(), Max = a.Max && b.Max };
        }

        static int Hundreds(double rpm) { return (int)Math.Round(rpm / 100) * 100; }

        /// «от N до M об/мин» по средним оборотам участков.
        public static string Range(IEnumerable<FanStat> parts)
        {
            int low = Hundreds(parts.Min(p => p.Mean)), high = Hundreds(parts.Max(p => p.Mean));
            return low == high ? low + " об/мин" : "от " + low + " до " + high + " об/мин";
        }

        /// При каких вентиляторах снято измерение из нескольких участков; null на месте участка — обороты неизвестны.
        public static string Describe(IEnumerable<FanStat> parts)
        {
            List<FanStat> all = parts.ToList(), known = all.Where(p => p != null).ToList();
            if (known.Count == 0) return "неизвестно";
            string gaps = known.Count < all.Count ? ", часть без данных" : "";
            int onMax = known.Count(p => p.Max);
            if (onMax == known.Count) return "на максимуме" + gaps;
            string range = Range(known.Where(p => !p.Max));
            return (onMax == 0 ? "авто, " + range : "часть на максимуме, часть на авто (" + range + ")") + gaps;
        }

        /// То же коротко, для строки над графиком: «макс», «авто 2400–3100», «неизвестно», «вперемешку».
        public static string Brief(IEnumerable<FanStat> parts)
        {
            List<FanStat> all = parts.ToList(), known = all.Where(p => p != null).ToList();
            if (known.Count == 0) return "неизвестно";
            int onMax = known.Count(p => p.Max);
            if (known.Count < all.Count || (onMax > 0 && onMax < known.Count)) return "вперемешку";
            if (onMax > 0) return "макс";
            int low = Hundreds(known.Min(p => p.Mean)), high = Hundreds(known.Max(p => p.Mean));
            return "авто " + (low == high ? low.ToString() : low + "–" + high);
        }

        /// Обороты одной строкой: «5200 / 5500 об/мин, максимум».
        public override string ToString()
        {
            return string.Join(" / ", Rpm) + " об/мин, " + (Max ? "максимум" : "авто");
        }

        // В файлах результатов: "fans" — обороты по вентиляторам, "fanMax" — был ли включён максимум.
        // Обороты неизвестны — полей нет: так же выглядят файлы, записанные до появления оборотов.

        public static void Put(Dictionary<string, object> to, FanStat air)
        {
            if (air == null) return;
            to["fans"] = air.Rpm;
            to["fanMax"] = air.Max;
        }

        public static FanStat Get(Dictionary<string, object> from)
        {
            object rpm, max;
            if (!from.TryGetValue("fans", out rpm) || rpm == null) return null;
            return new FanStat { Rpm = Json.Ints(rpm), Max = from.TryGetValue("fanMax", out max) && max is bool && (bool)max };
        }

        /// Участок строкой чисел {обороты…, максимум 1/0} — для таблиц, где результат хранится строками чисел.
        public static double[] Row(FanStat air)
        {
            return air == null ? null : air.Rpm.Select(v => (double)v).Concat(new[] { air.Max ? 1.0 : 0 }).ToArray();
        }

        public static FanStat FromRow(double[] row)
        {
            if (row == null || row.Length < 2) return null;
            return new FanStat { Rpm = row.Take(row.Length - 1).Select(v => (int)Math.Round(v)).ToArray(), Max = row[row.Length - 1] > 0 };
        }
    }

    /// Доступ к вентиляторам: прошивка или имитация. Зовёт его только поток вентиляторов.
    interface IFanBackend
    {
        /// Обороты по вентиляторам, об/мин. Исключение — не прочитались.
        int[] Speeds();

        /// Включён ли «максимум»; null — прошивка на этот запрос не отвечает.
        bool? Max();

        /// Включает или выключает «максимум». Исключение — отказ.
        void SetMax(bool on);
    }

    /// Служебный интерфейс прошивки HP: WMI, пространство root\wmi, метод hpqBIOSInt<размер ответа> объекта
    /// hpqBIntM; на входе объект hpqBDataIn с подписью «SECU», командой 0x20008 и номером запроса.
    /// Запросы (подробности и источники — outputs\fan-interface-2026-10-13.md):
    ///   0x2D — обороты: ответ 128 байт, байт 0 — вентилятор процессора, байт 1 — видеокарты, единица 100 об/мин;
    ///   0x26 — включён ли «максимум»: ответ 4 байта, бит 0 первого байта. На платах этого семейства ответу
    ///          «выключен» верить нельзя: прошивка часто отвечает так и при включённом максимуме;
    ///   0x27 — «максимум» включить (1) или выключить (0); ответа с данными нет;
    ///   0x10 — число вентиляторов; этим же запросом прошивке сообщают, что режимом управляет программа:
    ///          без него она через 120 секунд возвращается к своему режиму. Идёт перед каждой записью 0x27;
    ///   0x2E — уровень вентиляторов; пишется только {0, 0} («уровень выбирает автоматика») и только после
    ///          снятия максимума на платах из списка AutoAfterMax: там без этого максимум не снимается.
    class HpFans : IFanBackend
    {
        const uint Command = 0x20008;
        const uint Presence = 0x10, MaxGet = 0x26, MaxSet = 0x27, Levels = 0x2D, LevelSet = 0x2E;
        const int Plausible = 120;  // больше 12 000 об/мин у вентилятора ноутбука не бывает: это не обороты
        static readonly byte[] Sign = { 0x53, 0x45, 0x43, 0x55 };  // «SECU»
        static readonly TimeSpan Patience = TimeSpan.FromSeconds(4);
        // Платы, которым драйвер Linux hp-wmi после снятия максимума отдельно возвращает уровень автоматике
        // («Disabling max fan speed on Victus s1xxx laptops needs a 2nd step»): его таблица hp_wmi_feature_boards
        // на 27.08.2026. Остальным платам запрос 0x2E не посылается: что у них значит нулевой уровень, неизвестно.
        static readonly string[] AutoAfterMax =
        {
            "8902", "8A3D", "8A44", "8A4D", "8BAA", "8BA9", "8BAB", "8B2F", "8BB3", "8BBE", "8BC2", "8BCA", "8BCD", "8BD4", "8BD5",
            "8C76", "8C77", "8C78", "8C99", "8C9C", "8D26", "8D41", "8D87", "8D88", "8DD6", "8E35",
        };

        ManagementObject methods;
        ManagementClass request;
        bool maxUnread;  // на запрос 0x26 прошивка ответила отказом: больше он не посылается
        bool autoAfterMax;  // плата из списка AutoAfterMax

        void Open()
        {
            if (methods != null) return;
            try
            {
                var options = new System.Management.EnumerationOptions { Timeout = Patience, ReturnImmediately = false };
                using (var searcher = new ManagementObjectSearcher(new ManagementScope(@"root\wmi"), new ObjectQuery("SELECT * FROM hpqBIntM"), options))
                using (ManagementObjectCollection found = searcher.Get())
                    foreach (ManagementObject one in found)
                    {
                        if (methods == null) methods = one;
                        else one.Dispose();
                    }
                if (methods == null) throw new FanException("служебного интерфейса прошивки HP на этой машине нет");
                request = new ManagementClass(@"root\wmi", "hpqBDataIn", null);
                request.Get();
                autoAfterMax = Array.IndexOf(AutoAfterMax, Board()) >= 0;
            }
            catch (ManagementException e)
            {
                methods = null;
                throw new FanException(e.ErrorCode == ManagementStatus.InvalidClass || e.ErrorCode == ManagementStatus.InvalidNamespace
                    ? "служебного интерфейса прошивки HP на этой машине нет"
                    : "служебный интерфейс прошивки HP не открылся: " + Program.Why(e));
            }
        }

        /// Название платы из описания машины в Windows («8D41»); null — не прочиталось. К прошивке не обращается.
        static string Board()
        {
            try
            {
                using (var searcher = new ManagementObjectSearcher("SELECT Product FROM Win32_BaseBoard"))
                using (ManagementObjectCollection found = searcher.Get())
                    foreach (ManagementBaseObject board in found)
                        using (board)
                            return (board["Product"] as string ?? "").Trim().ToUpperInvariant();
            }
            catch (Exception e) { Program.Log("название платы не прочитано: " + e); }
            return null;
        }

        /// Один запрос к прошивке. size — размер ответа: 0, 4 или 128 байт; от него зависит имя метода.
        byte[] Call(uint type, byte[] data, int size)
        {
            Open();
            string name = "0x" + type.ToString("X2");
            try
            {
                using (ManagementObject input = request.CreateInstance())
                using (ManagementBaseObject arguments = methods.GetMethodParameters("hpqBIOSInt" + size))
                {
                    input["Sign"] = Sign;
                    input["Command"] = Command;
                    input["CommandType"] = type;
                    input["Size"] = (uint)data.Length;
                    input["hpqBData"] = data;
                    arguments["InData"] = input;
                    using (ManagementBaseObject result = methods.InvokeMethod("hpqBIOSInt" + size, arguments, new InvokeMethodOptions(null, Patience)))
                    {
                        var output = result == null ? null : result["OutData"] as ManagementBaseObject;
                        if (output == null) throw new FanException("прошивка на запрос " + name + " не ответила");
                        using (output)
                        {
                            uint code = Convert.ToUInt32(output["rwReturnCode"]);
                            if (code != 0) throw new FanRefusal("прошивка ответила на запрос " + name + " кодом " + code);
                            if (size == 0) return new byte[0];
                            var bytes = output["Data"] as byte[];
                            if (bytes == null) throw new FanException("прошивка на запрос " + name + " вернула пустой ответ");
                            return bytes;
                        }
                    }
                }
            }
            catch (ManagementException e) { throw new FanException("запрос " + name + " к прошивке не выполнен: " + Program.Why(e)); }
            catch (System.Runtime.InteropServices.COMException e) { throw new FanException("запрос " + name + " к прошивке не выполнен: " + Program.Why(e)); }
        }

        public int[] Speeds()
        {
            byte[] levels = Call(Levels, new byte[4], 128);
            if (levels.Length < 2) throw new FanException("прошивка вернула обороты не того размера");
            if (levels[0] > Plausible || levels[1] > Plausible)
                throw new FanException("прошивка вернула неправдоподобные обороты: " + levels[0] * 100 + " и " + levels[1] * 100);
            return new[] { levels[0] * 100, levels[1] * 100 };
        }

        public bool? Max()
        {
            if (maxUnread) return null;
            try
            {
                byte[] state = Call(MaxGet, new byte[4], 4);
                if (state.Length < 1) throw new FanRefusal("прошивка вернула состояние максимума не того размера");
                return (state[0] & 1) != 0;
            }
            catch (FanRefusal e)
            {
                maxUnread = true;
                Program.Log("состояние максимума вентиляторов не читается, дальше считается по последней записи: " + e.Message);
                return null;
            }
        }

        public void SetMax(bool on)
        {
            // Отказ в запросе 0x10 записи не отменяет: на части машин «максимум» включается и без него.
            try { Call(Presence, new byte[4], 4); }
            catch (FanRefusal e) { Program.LogOnce("вентиляторы, запрос 0x10", e); }
            Call(MaxSet, new byte[] { (byte)(on ? 1 : 0), 0, 0, 0 }, 0);
            if (!on && autoAfterMax) Call(LevelSet, new byte[] { 0, 0 }, 0);
        }
    }

    /// Имитация: обороты идут за самой горячей температурой имитации, «максимум» живёт в памяти.
    class SimFans : IFanBackend
    {
        readonly Func<double> hottest;
        readonly double[] rpm = { 2000, 2300 };

        public volatile bool On;       // «максимум» включён
        public volatile bool Refuse;   // проверка: прошивка отказывает в записи режима
        public volatile bool Broken;   // проверка: обороты не читаются
        public volatile bool Instant;  // проверка: обороты меняются сразу, без раскрутки
        public volatile int Writes;    // сколько раз режим записан

        public SimFans(Func<double> hottest) { this.hottest = hottest; }

        public int[] Speeds()
        {
            if (Broken) throw new FanException("обороты не прочитаны (проверка)");
            // До 50 °C обороты стоят: шум имитации в один градус не должен менять надпись в окне.
            double aim = Math.Min(5500, 2000 + Math.Max(0, hottest() - 50) * 80);
            double[] target = On ? new[] { 5800.0, 6100 } : new[] { aim, Math.Min(5800, aim + 300) };
            for (int k = 0; k < rpm.Length; k++) rpm[k] += (target[k] - rpm[k]) * (Instant ? 1 : 0.5);
            return rpm.Select(v => (int)Math.Round(v / 100) * 100).ToArray();
        }

        public bool? Max() { return On; }

        public void SetMax(bool on)
        {
            if (Refuse) throw new FanRefusal("прошивка отказала (проверка)");
            On = on;
            Writes++;
        }
    }

    class Fans
    {
        public const int Auto = 0, Measure = 1, Always = 2;  // выбор владельца: когда программа сама включает «максимум»

        const double RetryOff = 30;    // не снялся максимум — следующая попытка через столько секунд
        const double Patience = 6;     // столько секунд другие потоки ждут исполнения своей просьбы
        const int Steady = 100;        // обороты стоят на месте, если между проверками изменились не больше чем на столько

        struct Row
        {
            public double At;
            public int[] Rpm;
            public bool Max;
        }

        readonly Func<IFanBackend> create;
        readonly string modePath, markPath;  // null — имитация: ничего не пишется
        readonly double interval, maxEvery, keepEvery, spinStep, spinMin, spinMax;
        readonly object gate = new object();
        readonly AutoResetEvent wake = new AutoResetEvent(false);
        readonly Queue<Row> history = new Queue<Row>();
        IFanBackend backend;
        bool started;
        int asked, served;       // номер последней просьбы и номер, до которого поток вентиляторов дошёл
        string refusal;          // почему нынешний выбор не исполнен; null — исполнен
        volatile int mode;
        volatile bool engaged;   // идёт измерение, которое греет процессор
        volatile bool closing;   // программа закрывается: максимум, включённый ею, снимается
        volatile bool quiet;     // опрос оборотов приостановлен
        // Дальше — только поток вентиляторов.
        volatile bool ours;      // максимум включён программой и ещё не снят
        bool gaveUp;             // включить не удалось: пока выбор не сменится, попытка не повторяется
        bool foreign;            // максимум включён не программой (например, из Omen Gaming Hub): он не трогается
        bool left;               // максимум остался от прошлого запуска программы
        double keptAt, triedOffAt = double.NegativeInfinity, maxReadAt = double.NegativeInfinity;

        public volatile bool Supported;  // обороты читаются
        public volatile int[] Rpm;       // последние обороты по вентиляторам; null — неизвестны
        public volatile bool Max;        // режим «максимум» включён
        public volatile bool Spinning;   // максимум только что включён перед измерением, вентиляторы раскручиваются
        public volatile string Error;    // почему обороты недоступны или не прочитались; null — читаются

        public int Mode { get { return mode; } }

        /// create вызывается в потоке вентиляторов, а не здесь: настоящий доступ тянет за собой System.Management,
        /// и без Start (режим без окна) эта библиотека не загружается. saveDir = null — имитация. quick — паузы короткие.
        public Fans(Func<IFanBackend> create, string saveDir, bool quick)
        {
            this.create = create;
            // Обороты читаются раз в interval секунд, состояние максимума — раз в maxEvery.
            interval = quick ? 0.25 : 1;
            maxEvery = quick ? 0.5 : 5;
            // Максимум, включённый программой, подтверждается раз в keepEvery секунд: без подтверждения прошивка
            // через 120 секунд сама возвращается к своему режиму. Узнать о таком возврате раньше нечем: ответу
            // прошивки «максимум выключен» верить нельзя (см. HpFans).
            keepEvery = quick ? 1 : 60;
            // Раскрутка после включения максимума: проверка раз в spinStep, ждать не меньше spinMin и не больше spinMax.
            spinStep = quick ? 0.25 : 1;
            spinMin = quick ? 0.5 : 4;
            spinMax = quick ? 3 : 15;
            if (saveDir == null) return;
            modePath = Path.Combine(saveDir, "fans.json");
            markPath = Path.Combine(saveDir, "fans-held.json");
            try
            {
                if (File.Exists(modePath)) mode = Math.Max(Auto, Math.Min(Always, Convert.ToInt32(Json.ReadFile(modePath)["mode"])));
            }
            catch (Exception e) { Program.Log("выбор режима вентиляторов не прочитан, стоит «авто»: " + e); }
        }

        /// Запускает поток вентиляторов: первое чтение оборотов, дальше опрос. Зовёт окно.
        public void Start()
        {
            lock (gate)
            {
                if (started) return;
                started = true;
            }
            new Thread(Run) { IsBackground = true, Name = "fans" }.Start();
        }

        // ----- поток вентиляторов -----

        void Run()
        {
            if (!Probe()) return;
            left = markPath != null && File.Exists(markPath);
            if (left) ours = true;  // первый же шаг (Reconcile) его снимет — или продолжит держать при выборе «макс всегда»
            while (true)
            {
                int ticket;
                lock (gate) ticket = asked;
                Step("режим вентиляторов", Reconcile);
                if (!quiet) Step("обороты вентиляторов", Poll);
                lock (gate)
                {
                    served = ticket;
                    System.Threading.Monitor.PulseAll(gate);
                }
                wake.WaitOne(TimeSpan.FromSeconds(interval));
            }
        }

        /// Этот поток не должен умереть от ошибки: с ним пропали бы и обороты, и возврат режима.
        void Step(string what, Action step)
        {
            try { step(); }
            catch (Exception e) { Program.LogOnce(what, e); }
        }

        /// Первое чтение: поддержка определяется по факту — интерфейс есть, ответ пришёл, числа правдоподобны.
        /// Не вышло — «обороты недоступны» до конца работы программы, без сообщений в окне.
        bool Probe()
        {
            try
            {
                backend = create();
                Record(backend.Speeds());
                Supported = true;
                return true;
            }
            catch (Exception e)
            {
                Error = Program.Why(e);
                Program.Log("обороты вентиляторов недоступны: " + e);
                lock (gate) System.Threading.Monitor.PulseAll(gate);
                return false;
            }
        }

        void Record(int[] rpm)
        {
            double now = Native.Now();
            lock (gate)
            {
                history.Enqueue(new Row { At = now, Rpm = rpm, Max = Max });
                while (history.Peek().At < now - 120) history.Dequeue();
            }
            Rpm = rpm;
        }

        void Poll()
        {
            try
            {
                double now = Native.Now();
                // Пока максимум включён программой, прошивку о нём не спрашивают: её ответу «выключен» верить
                // нельзя. Иначе — спрашивают: так виден максимум, включённый не программой (из Omen Gaming Hub).
                if (ours) Max = true;
                else if (now - maxReadAt >= maxEvery)
                {
                    maxReadAt = now;
                    bool? on = backend.Max();
                    Max = on == true;
                    // Чужой максимум сняли: если владелец выбрал держать максимум, его включит программа.
                    if (foreign && on == false) foreign = false;
                }
                Record(backend.Speeds());
                Error = null;
            }
            catch (Exception e)
            {
                Rpm = null;
                Error = Program.Why(e);
                Program.LogOnce("обороты вентиляторов", e);
            }
        }

        /// Приводит режим к выбору владельца: включает максимум, подтверждает его или снимает.
        void Reconcile()
        {
            bool want = !closing && (mode == Always || (mode == Measure && engaged));
            double now = Native.Now();
            if (!want)
            {
                gaveUp = foreign = false;
                if (!ours) refusal = null;
                else if (now - triedOffAt >= RetryOff || served < asked) TurnOff(now);
            }
            else if (ours)
            {
                left = false;  // оставшийся с прошлого запуска максимум владелец и сейчас просит держать
                refusal = null;
                if (now - keptAt >= keepEvery) Keep(now);
            }
            else if (!gaveUp && !foreign) TurnOn(now);
        }

        void TurnOn(double now)
        {
            try
            {
                // Перед записью — проверка, что чтение работает: вслепую режим не меняется.
                if (Rpm == null) throw new FanException("обороты сейчас не читаются");
                // Максимум уже включён кем-то другим: программа его не трогает и после измерения не снимает.
                // (Ответ «выключен» может быть неверным; тогда чужой максимум после измерения будет снят.)
                if (backend.Max() == true)
                {
                    foreign = true;
                    Max = true;
                    refusal = null;
                    return;
                }
                Mark(true);  // до записи в прошивку: программа, снятая сразу после неё, должна оставить след
                backend.SetMax(true);
                ours = true;
                keptAt = maxReadAt = now;
                Max = true;
                refusal = null;
            }
            catch (Exception e)
            {
                gaveUp = true;
                Mark(false);
                refusal = "максимум не включён: " + Program.Why(e);
                Program.Log("вентиляторы: " + refusal + "\r\n" + e);
            }
        }

        void Keep(double now)
        {
            keptAt = now;
            maxReadAt = now;
            try
            {
                backend.SetMax(true);
                Max = true;
            }
            catch (Exception e) { Program.LogOnce("вентиляторы, подтверждение максимума", e); }
        }

        void TurnOff(double now)
        {
            triedOffAt = now;
            try
            {
                // По метке прошлого запуска команда уходит и тогда, когда прошивка отвечает «максимум выключен»:
                // этому ответу верить нельзя, а лишнее «выключить» безвредно.
                backend.SetMax(false);
                if (backend.Max() == true) throw new FanException("прошивка команду приняла, но максимум остался включён");
                ours = false;
                Max = false;
                maxReadAt = now;
                Mark(false);
                refusal = null;
                if (left) Program.Tell("Прошлый запуск программы оставил вентиляторы на максимуме — максимум снят.");
                left = false;
            }
            catch (Exception e)
            {
                refusal = "максимум не снят: " + Program.Why(e);
                Program.LogOnce("вентиляторы, снятие максимума", e);
            }
        }

        /// Ставит и убирает файл-метку «максимум включён программой».
        void Mark(bool held)
        {
            if (markPath == null) return;
            if (held)
            {
                Json.WriteFile(markPath, new Dictionary<string, object> { { "time", DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") } });
                return;
            }
            try
            {
                File.Delete(markPath);
                File.Delete(markPath + ".bak");
            }
            catch (Exception e) { Program.Log("метка максимума вентиляторов не удалена: " + e); }
        }

        // ----- просьбы других потоков -----

        /// Будит поток вентиляторов и ждёт, пока он приведёт режим к нынешнему выбору, не дольше Patience.
        /// null — приведён; иначе причина.
        string Settle()
        {
            if (!Supported) return null;
            lock (gate)
            {
                int ticket = ++asked;
                wake.Set();
                double end = Native.Now() + Patience;
                while (served < ticket)
                {
                    double remaining = end - Native.Now();
                    if (remaining <= 0) return "прошивка не ответила за " + Patience + " с";
                    System.Threading.Monitor.Wait(gate, TimeSpan.FromSeconds(remaining));
                }
                return refusal;
            }
        }

        /// Выбор владельца. null — принят и исполнен; иначе причина. Запоминается между запусками.
        public string SetMode(int value)
        {
            if (!Supported) return "вентиляторы недоступны" + (Error == null ? "" : ": " + Error);
            int before = mode;
            mode = value;
            string refused = Settle();
            if (refused != null && value == Always)
            {
                // Максимум не включился: переключатель не должен показывать то, чего нет.
                mode = before;
                Settle();
                return refused;
            }
            if (modePath != null)
                try { Json.WriteFile(modePath, new Dictionary<string, object> { { "mode", value } }); }
                catch (Exception e) { Program.Tell("Выбор режима вентиляторов не запомнен: " + Program.Why(e) + ".", e); }
            return refused;
        }

        /// Перед измерением, которое греет процессор. При выборе «макс при замерах» включает максимум и ждёт,
        /// пока обороты установятся (wait — ожидание измерения: true — его просят остановить). Отказ измерению
        /// не мешает: оно идёт при автоматических оборотах, владельцу об этом сказано.
        public void Engage(Func<double, bool> wait)
        {
            engaged = true;
            if (!Supported || mode != Measure) return;
            bool was = Max;
            string refused = Settle();
            if (refused != null)
            {
                Program.Tell("Вентиляторы не переведены на максимум (" + refused + "): измерение идёт при автоматических оборотах.");
                return;
            }
            if (was) return;
            // Сколько длится раскрутка, источники не говорят: ждём, пока обороты перестанут расти, но не дольше spinMax.
            Spinning = true;
            try
            {
                double began = Native.Now();
                int[] before = null;
                int steady = 0;  // сколько проверок подряд обороты стоят на месте
                while (!wait(spinStep))
                {
                    int[] now = Rpm;
                    bool same = now != null && before != null && now.Length == before.Length &&
                        Enumerable.Range(0, now.Length).All(k => Math.Abs(now[k] - before[k]) <= Steady);
                    steady = same ? steady + 1 : 0;
                    double passed = Native.Now() - began;
                    if (passed >= spinMax || (passed >= spinMin && steady >= 2)) break;
                    before = now;
                }
            }
            finally { Spinning = false; }
        }

        /// После измерения, на любом пути выхода из него: возвращает вентиляторам то, что было до измерения.
        public void Release()
        {
            if (!engaged) return;
            engaged = false;
            if (!Supported || mode != Measure) return;
            string refused = Settle();
            if (refused != null)
                Program.Tell("Вентиляторы остались на максимуме (" + refused + "). Программа попробует снять его ещё раз; снять можно и в Omen Gaming Hub.");
        }

        /// Опрос оборотов на время приостановить: измерение задержек чувствительно к посторонним обращениям к прошивке.
        public void Hush(bool on) { quiet = on; }

        /// Закрытие программы: максимум, включённый ею, снимается.
        public void Close()
        {
            closing = true;
            Settle();
        }

        // ----- обороты для результатов измерений -----

        /// Обороты за последние seconds секунд; null — неизвестны.
        public FanStat Typical(double seconds)
        {
            double now = Native.Now();
            Row[] rows;
            lock (gate)
            {
                rows = history.Where(r => r.At >= now - seconds).ToArray();
                // Участок короче шага опроса: берётся последнее показание, если оно свежее.
                if (rows.Length == 0 && history.Count > 0 && history.Last().At >= now - Math.Max(seconds, 3 * interval))
                    rows = new[] { history.Last() };
            }
            if (rows.Length == 0) return null;
            int count = rows[rows.Length - 1].Rpm.Length;
            rows = rows.Where(r => r.Rpm.Length == count).ToArray();
            return new FanStat
            {
                Rpm = Enumerable.Range(0, count).Select(k => (int)Math.Round(rows.Average(r => r.Rpm[k]) / 10) * 10).ToArray(),
                Max = rows.All(r => r.Max),
            };
        }
    }
}
