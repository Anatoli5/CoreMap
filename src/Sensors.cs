// Температуры ядер и мощность процессора: источник (датчики через PawnIO или имитация)
// и опрос с запоминанием максимумов.
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;

namespace CoreMap
{
    class SensorException : Exception
    {
        public SensorException(string message) : base(message) { }
    }

    /// Доступ к регистрам процессора через установленный драйвер PawnIO и его модуль IntelMSR.
    class PawnIo
    {
        [DllImport("PawnIOLib.dll")] static extern int pawnio_open(out IntPtr handle);
        [DllImport("PawnIOLib.dll")] static extern int pawnio_load(IntPtr handle, byte[] blob, UIntPtr size);
        [DllImport("PawnIOLib.dll", CharSet = CharSet.Ansi)]
        static extern int pawnio_execute(IntPtr handle, string name, ulong[] input, UIntPtr inputSize,
                                         ulong[] output, UIntPtr outputSize, out UIntPtr returned);

        [DllImport("PawnIOLib.dll")] static extern int pawnio_close(IntPtr handle);

        readonly string modulePath;
        IntPtr handle;  // Zero — закрыт (Close): следующее обращение к регистру откроет драйвер заново
        readonly ulong[] input = new ulong[2], output = new ulong[1];

        public PawnIo(string modulePath)
        {
            this.modulePath = modulePath;
            string lib = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "PawnIO", "PawnIOLib.dll");
            if (!File.Exists(lib)) throw new SensorException("драйвер PawnIO не установлен: нет " + lib);
            if (!File.Exists(modulePath)) throw new SensorException("нет файла модуля " + modulePath);
            if (Native.LoadLibrary(lib) == IntPtr.Zero) throw new SensorException("не загрузилась библиотека " + lib);
            Open();
        }

        /// Открывает драйвер и загружает в него модуль: модуль живёт вместе с дескриптором.
        void Open()
        {
            int hr = pawnio_open(out handle);
            if (hr != 0) handle = IntPtr.Zero;
            if (hr == unchecked((int)0x80070005)) throw new SensorException("программа запущена без прав администратора");
            if (hr != 0) throw new SensorException("PawnIO не открылся, код 0x" + hr.ToString("X8"));
            byte[] blob = File.ReadAllBytes(modulePath);
            hr = pawnio_load(handle, blob, (UIntPtr)blob.Length);
            if (hr == 0) return;
            Close();
            throw new SensorException("модуль IntelMSR не загрузился, код 0x" + hr.ToString("X8"));
        }

        /// Закрывает дескриптор драйвера до следующего обращения к регистрам. Нужен резиденту (Tray.cs): он
        /// обращается к процессору только при запуске и по щелчку в меню, а между этим драйвер не держит.
        /// Окно дескриптор не закрывает.
        public void Close()
        {
            lock (input)
            {
                if (handle == IntPtr.Zero) return;
                pawnio_close(handle);
                handle = IntPtr.Zero;
            }
        }

        /// Читает регистр на том ядре, к которому сейчас привязан вызывающий поток.
        public ulong ReadMsr(uint msr)
        {
            lock (input)
            {
                UIntPtr got;
                if (handle == IntPtr.Zero) Open();
                input[0] = msr;
                int hr = pawnio_execute(handle, "ioctl_read_msr", input, (UIntPtr)1, output, (UIntPtr)1, out got);
                if (hr != 0) throw new SensorException("чтение регистра 0x" + msr.ToString("X") + " не удалось, код 0x" + hr.ToString("X8"));
                // Драйвер сообщает, сколько слов ответа записал. Ни одного — в output осталось значение прошлого
                // чтения, и оно ушло бы за показание этого регистра.
                if (got == UIntPtr.Zero) throw new SensorException("чтение регистра 0x" + msr.ToString("X") + ": драйвер вернул пустой ответ");
                return output[0];
            }
        }

        /// Пишет регистр на том ядре, к которому сейчас привязан вызывающий поток.
        /// Модуль IntelMSR пускает запись только в свой короткий список регистров.
        public void WriteMsr(uint msr, ulong value)
        {
            lock (input)
            {
                UIntPtr got;
                if (handle == IntPtr.Zero) Open();
                input[0] = msr;
                input[1] = value;
                int hr = pawnio_execute(handle, "ioctl_write_msr", input, (UIntPtr)2, output, (UIntPtr)0, out got);
                if (hr != 0) throw new SensorException("запись регистра 0x" + msr.ToString("X") + " не удалась, код 0x" + hr.ToString("X8"));
            }
        }
    }

    class Sample
    {
        public double[] Temps;     // по ядрам, NaN — показание недействительно
        public double Pkg;         // температура корпуса
        public bool[] Throttled;   // ядро на пороге и сбрасывает частоту
        public double[] Ratio;     // фактический множитель ядра с прошлого опроса; NaN — ядро почти всё время спало
        public double Joules;      // энергия, израсходованная процессором с начала опроса
    }

    interface ISensorBackend
    {
        int ThrottleAt { get; }    // температура, на которой процессор сам сбрасывает частоту
        Sample Read();
    }

    /// Цифровые датчики ядер Intel: температура = TjMax минус показание регистра.
    ///
    /// Показание мгновенное. Проверено на 275HX: короткая фоновая задача поднимает датчик ядра
    /// на 15–35 °C, и за 20 мс он плавно возвращается — это настоящий нагрев, не сбой чтения.
    /// Шаг датчика ядра там же — 2 °C, хотя регистр заявляет 1 °C.
    class LiveBackend : ISensorBackend
    {
        const uint ThermStatus = 0x19C, TemperatureTarget = 0x1A2, PackageThermStatus = 0x1B1;
        const uint RaplPowerUnit = 0x606, PkgEnergyStatus = 0x611;
        // Два счётчика ядра: Mperf идёт с постоянной опорной частотой, Aperf — с фактической, и оба
        // стоят, пока ядро спит. Отношение их приращений, умноженное на опорный множитель, — множитель,
        // на котором ядро в среднем работало между опросами.
        const uint Mperf = 0xE7, Aperf = 0xE8, PlatformInfo = 0xCE;
        const ulong AwakeAtLeast = 10000000;  // меньше стольких опорных тактов за опрос — ядро спало, множитель не считается

        readonly PawnIo io;
        readonly int[] lps;
        readonly int[] tjMax;
        readonly double joule;     // единица счётчика энергии
        uint energyRaw;
        double energy;
        readonly ulong[] aperf, mperf;
        readonly double referenceRatio;
        bool counted;

        public int ThrottleAt { get; private set; }

        public LiveBackend(PawnIo io, int[] lps)
        {
            this.io = io;
            this.lps = lps;
            tjMax = new int[lps.Length];
            ulong target = 0;
            try
            {
                for (int i = 0; i < lps.Length; i++)
                {
                    if (!Native.Pin(lps[i])) throw new SensorException("не удалось привязать поток к процессору " + lps[i]);
                    target = io.ReadMsr(TemperatureTarget);
                    tjMax[i] = (int)(target >> 16 & 0xFF);
                }
            }
            finally { Native.Unpin(lps); }  // иначе при ошибке поток окна остался бы привязан к одному ядру
            // Процессор сбрасывает частоту на TjMax минус сдвиг (его задаёт прошивка или утилита вроде
            // ThrottleStop). Сама температура отсчитывается от TjMax без сдвига: так показания
            // совпадают с ThrottleStop.
            ThrottleAt = tjMax.Min() - (int)(target >> 24 & 0x3F);
            joule = 1.0 / (1UL << (int)(io.ReadMsr(RaplPowerUnit) >> 8 & 0x1F));
            energyRaw = (uint)io.ReadMsr(PkgEnergyStatus);
            referenceRatio = io.ReadMsr(PlatformInfo) >> 8 & 0xFF;
            aperf = new ulong[lps.Length];
            mperf = new ulong[lps.Length];
            Native.Unpin(lps);
        }

        public Sample Read()
        {
            var s = new Sample
            {
                Temps = new double[lps.Length], Throttled = new bool[lps.Length], Ratio = new double[lps.Length], Pkg = double.NaN,
            };
            try
            {
                for (int i = 0; i < lps.Length; i++)
                {
                    // Поток не привязался — регистры прочитались бы с какого-то другого ядра. Показаний этого ядра
                    // в опросе нет; если оно нагружено, защита снимет нагрузку (Core.Blind).
                    if (!Native.Pin(lps[i]))
                    {
                        s.Temps[i] = s.Ratio[i] = double.NaN;
                        continue;
                    }
                    ulong v = io.ReadMsr(ThermStatus);
                    s.Temps[i] = (v >> 31 & 1) != 0 ? tjMax[i] - (int)(v >> 16 & 0x7F) : double.NaN;
                    s.Throttled[i] = (v & 1) != 0;
                    ulong a = io.ReadMsr(Aperf), m = io.ReadMsr(Mperf);
                    ulong ran = m - mperf[i];
                    s.Ratio[i] = counted && ran >= AwakeAtLeast ? referenceRatio * (a - aperf[i]) / ran : double.NaN;
                    aperf[i] = a;
                    mperf[i] = m;
                }
                counted = true;
                ulong p = io.ReadMsr(PackageThermStatus);
                if ((p >> 31 & 1) != 0) s.Pkg = tjMax[lps.Length - 1] - (int)(p >> 16 & 0x7F);
                uint raw = (uint)io.ReadMsr(PkgEnergyStatus);
                energy += unchecked(raw - energyRaw) * joule;  // 32-битный счётчик переполняется
                energyRaw = raw;
                s.Joules = energy;
            }
            finally { Native.Unpin(lps); }
            return s;
        }
    }

    /// Имитация для проверки интерфейса без датчиков: нагрев падает с расстоянием на схеме.
    class SimBackend : ISensorBackend
    {
        readonly double[][] pos;
        readonly Func<bool[]> loaded;
        readonly double[] temps;
        readonly Random noise = new Random(1);
        double energy, at = Native.Now();

        public int ThrottleAt { get { return 105; } }

        public SimBackend(double[][] positions, Func<bool[]> loaded)
        {
            pos = positions;
            this.loaded = loaded;
            temps = Enumerable.Repeat(45.0, positions.Length).ToArray();
        }

        public Sample Read()
        {
            bool[] on = loaded();
            int count = on.Count(x => x);
            double now = Native.Now();
            energy += (12 + 9 * count) * (now - at);
            at = now;
            var s = new Sample
            {
                Temps = new double[temps.Length], Throttled = new bool[temps.Length], Joules = energy,
                Ratio = on.Select(x => x ? 52.4 : 31.0).ToArray(),
            };
            for (int i = 0; i < temps.Length; i++)
            {
                double target = 45 + 1.5 * count;
                for (int src = 0; src < temps.Length; src++)
                    if (on[src])
                    {
                        double dx = pos[i][0] - pos[src][0], dy = pos[i][1] - pos[src][1];
                        target += 30 * Math.Exp(-Math.Sqrt(dx * dx + dy * dy) / 1.2);
                    }
                temps[i] += (target - temps[i]) * 0.25;
                s.Temps[i] = Math.Round(temps[i] + noise.NextDouble() * 0.8 - 0.4);
            }
            s.Pkg = s.Temps.Max();
            return s;
        }
    }

    /// Опрашивает датчики в своём потоке, хранит текущие и максимальные значения и недавнюю историю.
    class Monitor
    {
        struct Row
        {
            public double At, Watts;
            public double[] Temps;
        }

        readonly ISensorBackend backend;
        readonly double interval;
        readonly int count;
        readonly object gate = new object();
        readonly Queue<Row> history = new Queue<Row>();
        double lastAt = double.NaN, lastJoules;

        public double[] Cur, Max;
        public double[] Ratio;     // фактический множитель каждого ядра за последний опрос
        public bool[] Throttled;
        public double PkgCur = double.NaN, PkgMax = double.NaN, Watts = double.NaN;
        public DateTime ResetAt = DateTime.Now;
        public volatile string Error;      // почему последний опрос не удался; null — удался
        public Action<double[]> OnSample;  // после каждого опроса, в потоке датчиков: температуры или null при сбое датчиков

        public Monitor(ISensorBackend backend, int count, double interval)
        {
            this.backend = backend;
            this.count = count;
            this.interval = interval;
            Cur = Enumerable.Repeat(double.NaN, count).ToArray();
            Max = (double[])Cur.Clone();
            Ratio = (double[])Cur.Clone();
            Throttled = new bool[count];
        }

        Thread thread;
        volatile bool stop;

        public void Start()
        {
            // Высокий приоритет: иначе под полной нагрузкой опрос ждал бы своей очереди на каждом занятом ядре.
            thread = new Thread(Run) { IsBackground = true, Priority = ThreadPriority.Highest, Name = "sensors" };
            thread.Start();
        }

        /// Кончает опрос: поток датчиков доделывает идущий опрос и выходит. Нужен движку, который живёт дольше,
        /// чем окно: без окна и измерения датчики никто не читает (Core.Close). Ждёт выхода не дольше двух секунд;
        /// из самого потока датчиков не ждёт.
        public void Stop()
        {
            stop = true;
            if (thread != null && thread != Thread.CurrentThread) thread.Join(2000);
        }

        void Run()
        {
            while (!stop)
            {
                double started = Native.Now();
                double[] temps = null;
                string problem = null;
                try
                {
                    Sample s = backend.Read();
                    double readAt = Native.Now();
                    if (!double.IsNaN(lastAt) && readAt > lastAt) Watts = (s.Joules - lastJoules) / (readAt - lastAt);
                    lastAt = readAt;
                    lastJoules = s.Joules;
                    temps = s.Temps;
                    lock (gate)
                    {
                        Cur = s.Temps;
                        Ratio = s.Ratio;
                        Throttled = s.Throttled;
                        PkgCur = s.Pkg;
                        for (int i = 0; i < count; i++)
                            if (!double.IsNaN(s.Temps[i]) && !(Max[i] >= s.Temps[i])) Max[i] = s.Temps[i];
                        if (!double.IsNaN(s.Pkg) && !(PkgMax >= s.Pkg)) PkgMax = s.Pkg;
                        history.Enqueue(new Row { At = started, Temps = s.Temps, Watts = Watts });
                        while (history.Count > 0 && history.Peek().At < started - 60) history.Dequeue();
                    }
                }
                catch (SensorException e) { problem = e.Message; }
                // Этот поток не должен умереть ни от какой ошибки: с ним умерла бы защита от перегрева, а необработанная
                // ошибка в потоке закрывает всю программу. Ошибка — в журнал и в полосу сообщений, опрос идёт дальше.
                catch (Exception e) { problem = Failed("опрос датчиков", e); }
                // Без показаний (temps = null) защита снимает нагрузку: см. Core.Guard.
                try { if (OnSample != null) OnSample(temps); }
                catch (Exception e) { problem = Failed("обработка показаний датчиков", e); }
                Error = problem;
                Thread.Sleep((int)(Math.Max(0.05, interval - (Native.Now() - started)) * 1000));
            }
        }

        static string Failed(string where, Exception e)
        {
            Program.LogOnce(where, e);
            return where + ": " + e.Message + ". Подробности записаны в data\\error.log";
        }

        public void ResetMax()
        {
            lock (gate)
            {
                Max = (double[])Cur.Clone();
                PkgMax = PkgCur;
                ResetAt = DateTime.Now;
            }
        }

        /// Среднее без верхней четверти значений. Фоновые задачи дают короткие всплески,
        /// и только вверх, поэтому отбрасываются самые горячие отсчёты.
        static double Trimmed(List<double> values)
        {
            if (values.Count == 0) return double.NaN;
            values.Sort();
            return values.Take(Math.Max(1, values.Count * 3 / 4)).Average();
        }

        /// Температура каждого ядра за последние seconds секунд.
        public double[] Typical(double seconds)
        {
            double since = Native.Now() - seconds;
            Row[] rows;
            lock (gate) rows = history.Where(r => r.At >= since).ToArray();
            var result = new double[count];
            for (int i = 0; i < count; i++)
                result[i] = Trimmed(rows.Select(r => r.Temps[i]).Where(t => !double.IsNaN(t)).ToList());
            return result;
        }

        /// Мощность процессора за последние seconds секунд, Вт.
        public double Power(double seconds)
        {
            double since = Native.Now() - seconds;
            lock (gate) return Trimmed(history.Where(r => r.At >= since && !double.IsNaN(r.Watts)).Select(r => r.Watts).ToList());
        }
    }
}
