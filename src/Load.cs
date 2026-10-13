// Нагрузка на выбранные ядра: по одному потоку на ядро, привязанному к нему.
using System;
using System.Linq;
using System.Security.Cryptography;
using System.Threading;

namespace CoreMap
{
    /// Вид вычислений. Сила нагрузки — это вид вычислений, а не доля времени: ядро занято
    /// непрерывно, и показание датчика стоит ровно, без рывков.
    class Kernel
    {
        public string Name;
        public Action<Scratch> Chunk;  // одна порция работы
        public double Units;           // сколько единиц скорости в одной порции
        public string Unit;            // название единицы скорости
    }

    /// Рабочая память одного потока нагрузки. Помещается в кэш ядра: оперативная память не участвует.
    /// Создаётся на каждый запуск нагрузки и освобождается с её концом: внутри — системный объект хэша.
    class Scratch : IDisposable
    {
        public readonly byte[] Bytes = new byte[64 * 1024];
        public readonly long[] Words = new long[16 * 1024];
        public readonly SHA256 Sha = new SHA256Cng();
        public ulong A = 0x9E3779B97F4A7C15UL, B = 0xD1B54A32D192ED03UL, C = 0x8CB92BA72F3D8DD7UL, D = 0xABC98388FB8FAC03UL;
        public double X = 1.5;
        public readonly ulong[] State = new ulong[25], Column = new ulong[5];

        public Scratch()
        {
            new Random(7).NextBytes(Bytes);
            for (int i = 0; i < State.Length; i++) State[i] = A * (ulong)(i + 1) ^ B;
        }

        public void Dispose() { Sha.Dispose(); }
    }

    static class Kernels
    {
        const int Steps = 20000;

        static void Sha(Scratch s) { s.Sha.ComputeHash(s.Bytes); }

        static void Divide(Scratch s)
        {
            double x = s.X;
            for (int i = 0; i < Steps; i++) x = 1e9 / (x + 1.5) + 0.25;
            s.X = x;
        }

        static void IntMix(Scratch s)
        {
            ulong a = s.A, b = s.B, c = s.C, d = s.D;
            for (int i = 0; i < Steps; i++)
            {
                a = (a ^ a >> 29) * 0xBF58476D1CE4E5B9UL + 1;
                b = (b ^ b >> 31) * 0x94D049BB133111EBUL + 3;
                c = (c ^ c >> 27) * 0xFF51AFD7ED558CCDUL + 5;
                d = (d ^ d >> 33) * 0xC4CEB9FE1A85EC53UL + 7;
            }
            s.A = a; s.B = b; s.C = c; s.D = d;
        }

        static void Float8(Scratch s)
        {
            double f0 = s.X, f1 = 1.1, f2 = 1.2, f3 = 1.3, f4 = 1.4, f5 = 1.5, f6 = 1.6, f7 = 1.7;
            for (int i = 0; i < Steps; i++)
            {
                f0 = f0 * 0.999999 + 1e-6; f1 = f1 * 0.999998 + 2e-6; f2 = f2 * 0.999997 + 3e-6; f3 = f3 * 0.999996 + 4e-6;
                f4 = f4 * 0.999995 + 5e-6; f5 = f5 * 0.999994 + 6e-6; f6 = f6 * 0.999993 + 7e-6; f7 = f7 * 0.999992 + 8e-6;
            }
            s.X = (f0 + f1 + f2 + f3 + f4 + f5 + f6 + f7) / 8;
        }

        static void Memory(Scratch s)
        {
            long[] w = s.Words;
            long acc = (long)s.A;
            for (int i = 0; i < w.Length; i++)
            {
                acc += w[i] * 31 + i;
                w[i] = acc ^ acc >> 7;
            }
            s.A = (ulong)acc;
        }

        // Восемь целочисленных и восемь вещественных независимых цепочек в одном цикле: занято больше
        // исполнительных блоков ядра сразу, чем в IntMix или Float8 по отдельности.
        static void Dense(Scratch s)
        {
            const ulong k = 0xBF58476D1CE4E5B9UL;
            ulong a = s.A, b = s.B, c = s.C, d = s.D, e = ~s.A, f = ~s.B, g = ~s.C, h = ~s.D;
            double f0 = s.X, f1 = 1.1, f2 = 1.2, f3 = 1.3, f4 = 1.4, f5 = 1.5, f6 = 1.6, f7 = 1.7;
            for (int i = 0; i < Steps; i++)
            {
                a = (a ^ a >> 29) * k + 1; b = (b ^ b >> 31) * k + 3; c = (c ^ c >> 27) * k + 5; d = (d ^ d >> 33) * k + 7;
                e = (e ^ e >> 28) * k + 9; f = (f ^ f >> 30) * k + 11; g = (g ^ g >> 26) * k + 13; h = (h ^ h >> 32) * k + 15;
                f0 = f0 * 0.999999 + 1e-6; f1 = f1 * 0.999998 + 2e-6; f2 = f2 * 0.999997 + 3e-6; f3 = f3 * 0.999996 + 4e-6;
                f4 = f4 * 0.999995 + 5e-6; f5 = f5 * 0.999994 + 6e-6; f6 = f6 * 0.999993 + 7e-6; f7 = f7 * 0.999992 + 8e-6;
            }
            s.A = a ^ e; s.B = b ^ f; s.C = c ^ g; s.D = d ^ h;
            s.X = (f0 + f1 + f2 + f3 + f4 + f5 + f6 + f7) / 8;
        }

        const int Rounds = 1000;
        static readonly int[] Turn = { 1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44 };
        static readonly int[] Place = { 10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1 };

        // Перемешивание 25 слов по образцу SHA-3: сдвиги и исключающее «или» широким фронтом.
        // В Python-версии именно SHA-3 грел ядро сильнее всего.
        static void Sponge(Scratch s)
        {
            ulong[] a = s.State, c = s.Column;
            for (int round = 0; round < Rounds; round++)
            {
                for (int x = 0; x < 5; x++) c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
                for (int x = 0; x < 5; x++)
                {
                    ulong next = c[(x + 1) % 5], mix = c[(x + 4) % 5] ^ (next << 1 | next >> 63);
                    for (int y = 0; y < 25; y += 5) a[y + x] ^= mix;
                }
                ulong carry = a[1];
                for (int i = 0; i < 24; i++)
                {
                    int to = Place[i], turn = Turn[i];
                    ulong kept = a[to];
                    a[to] = carry << turn | carry >> 64 - turn;
                    carry = kept;
                }
                for (int y = 0; y < 25; y += 5)
                {
                    ulong a0 = a[y], a1 = a[y + 1], a2 = a[y + 2], a3 = a[y + 3], a4 = a[y + 4];
                    a[y] = a0 ^ ~a1 & a2; a[y + 1] = a1 ^ ~a2 & a3; a[y + 2] = a2 ^ ~a3 & a4; a[y + 3] = a3 ^ ~a4 & a0; a[y + 4] = a4 ^ ~a0 & a1;
                }
                a[0] ^= (ulong)round * 0x9E3779B97F4A7C15UL + 1;
            }
        }

        /// Все опробованные виды вычислений; подбор нагрузки (Calibration) меряет нагрев от каждого.
        public static readonly Kernel[] All =
        {
            new Kernel { Name = "sha256", Chunk = Sha, Units = 1.0 / 16, Unit = "МБ/с" },
            new Kernel { Name = "divide", Chunk = Divide, Units = Steps / 1e6, Unit = "млн оп./с" },
            new Kernel { Name = "intmix", Chunk = IntMix, Units = 4 * Steps / 1e6, Unit = "млн оп./с" },
            new Kernel { Name = "float8", Chunk = Float8, Units = 8 * Steps / 1e6, Unit = "млн оп./с" },
            new Kernel { Name = "memory", Chunk = Memory, Units = 16 * 1024 / 1e6, Unit = "млн оп./с" },
            new Kernel { Name = "dense", Chunk = Dense, Units = 16 * Steps / 1e6, Unit = "млн оп./с" },
            new Kernel { Name = "sponge", Chunk = Sponge, Units = 25 * Rounds / 1e6, Unit = "млн оп./с" },
        };

        /// Что значат ступени силы 1, 2, 3: какой вид вычислений каждой соответствует и во сколько раз нагрев
        /// под ней больше, чем под силой 1. Набор после публикации не меняется.
        class LevelSet
        {
            public int[] Kernel;
            public double[] Rise;
        }

        // Здесь значения до подбора; подбор нагрузки (Calibration) в своём потоке ставит выбранные по замеру —
        // новым набором целиком, чтобы ступени из старого и нового набора вперемешку никто не увидел.
        static volatile LevelSet levels = new LevelSet { Kernel = new[] { 0, 0, 2, 3 }, Rise = new[] { 0, 1.0, 1.5, 1.9 } };

        public static Kernel ForLevel(int level) { return All[levels.Kernel[level]]; }

        /// Во сколько раз нагрев под силой 1, 2, 3 больше, чем под силой 1; по номеру ступени. Массив не менять.
        public static double[] Rise { get { return levels.Rise; } }

        /// Ставит ступени по замеру: kernels — виды вычислений (номера в All) для силы 1, 2, 3, heat — нагрев на них.
        public static void Assign(int[] kernels, double[] heat)
        {
            levels = new LevelSet
            {
                Kernel = new[] { kernels[0], kernels[0], kernels[1], kernels[2] },
                Rise = new[] { 0, 1.0, heat[1] / heat[0], heat[2] / heat[0] },
            };
        }
    }

    class LoadManager
    {
        class Worker
        {
            public Thread Thread;
            public volatile bool Stop;
            public volatile float Rate = float.NaN;  // порций в секунду за последние полсекунды
            public Kernel Kernel;
        }

        public static readonly int[] Levels = { 1, 2, 3 };

        readonly int[] lps;
        readonly bool dry;
        readonly Worker[] workers;
        readonly object gate = new object();

        /// Сила ручной нагрузки и верхняя граница для автоподбора в тепловом измерении.
        public int Level = 3;

        /// dry: потоки не запускаются. В имитации температуры ненастоящие, защита от перегрева
        /// их не видит, поэтому настоящую нагрузку давать нельзя.
        public LoadManager(int[] lps, bool dry)
        {
            this.lps = lps;
            this.dry = dry;
            workers = new Worker[lps.Length];
        }

        public void Start(int index, int level = 0) { StartKernel(index, Kernels.ForLevel(level > 0 ? level : Level)); }

        public void StartKernel(int index, Kernel kernel)
        {
            lock (gate)
            {
                if (workers[index] != null) return;
                var w = workers[index] = new Worker { Kernel = kernel };
                if (dry) return;
                int lp = lps[index];
                w.Thread = new Thread(() => Run(w, lp)) { IsBackground = true, Name = "load " + lp };
                w.Thread.Start();
            }
        }

        static void Run(Worker w, int lp)
        {
            Native.Pin(lp);
            // Пониженный приоритет оставляет систему отзывчивой; на нагрев ядра он не влияет
            // (замер на 275HX: 70 °C и 71 °C при пониженном и обычном приоритете).
            Native.LowerThreadPriority();
            using (var scratch = new Scratch())
            {
                int done = 0;
                double since = Native.Now();
                while (!w.Stop)
                {
                    w.Kernel.Chunk(scratch);
                    if ((++done & 15) != 0) continue;
                    double now = Native.Now();
                    if (now - since < 0.5) continue;
                    w.Rate = (float)(done / (now - since));
                    done = 0;
                    since = now;
                }
            }
        }

        public void Stop(int index)
        {
            Worker w;
            lock (gate)
            {
                w = workers[index];
                workers[index] = null;
            }
            if (w == null || w.Thread == null) return;
            w.Stop = true;
            w.Thread.Join();
        }

        public void StopAll()
        {
            for (int i = 0; i < workers.Length; i++) Stop(i);
        }

        public bool[] Active()
        {
            lock (gate) return workers.Select(w => w != null).ToArray();
        }

        public bool Any()
        {
            lock (gate) return workers.Any(w => w != null);
        }

        /// Скорость счёта ядра в единицах его вида вычислений; NaN, если неизвестна.
        public double Rate(int index)
        {
            Worker w = workers[index];
            return w == null || float.IsNaN(w.Rate) ? double.NaN : w.Rate * w.Kernel.Units;
        }

        /// Меняет силу нагрузки; уже нагруженные ядра перезапускаются с новой.
        public void SetLevel(int level)
        {
            if (!Levels.Contains(level) || level == Level) return;
            Level = level;
            bool[] active = Active();
            for (int i = 0; i < active.Length; i++)
                if (active[i]) { Stop(i); Start(i); }
        }
    }
}
