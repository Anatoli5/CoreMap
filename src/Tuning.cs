// Разгонный интерфейс процессора Intel (регистр 0x150) и опрос того, что процессор сейчас держит
// по каждому ядру. Intel этот интерфейс не документирует; раскладка полей и номера команд взяты
// из исходников Linux (drivers/platform/x86/intel_turbo_max_3.c) и справочного кода прошивок.
using System;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading;

namespace CoreMap
{
    static class OcMailbox
    {
        const uint Register = 0x150;
        public const int ReadCapabilities = 0x01;  // возможности разгона по доменам
        // Ответ на 0x1C, проверено на 275HX по HWiNFO: байт 1 — заводской предел множителя ядра,
        // байт 0 — действующий потолок. У кластера E-ядер значение общее.
        public const int ReadCoreLimit = 0x1C;
        // Запись потолка. Номер — предположение по парности команд интерфейса (чтение чётное, запись на
        // единицу больше); Intel его не публикует. Принята ли запись, видно только перечитыванием.
        public const int WriteCoreLimit = 0x1D;

        /// Отправляет команду на ядре, к которому привязан поток, и ждёт ответа.
        /// error: 0 — команда принята; иначе код отказа процессора; -1 — ответа нет.
        public static uint Query(PawnIo io, int command, int param1, int param2, uint data, out int error)
        {
            ulong request = 1UL << 63 | (ulong)param2 << 48 | (ulong)param1 << 40 | (ulong)command << 32 | data;
            // Запрос и ответ идут через один регистр: вторая копия программы (опрос --probe) не должна вклиниться между ними.
            bool held = false;
            try { held = Gate.WaitOne(2000); }
            catch (AbandonedMutexException) { held = true; }
            try
            {
                io.WriteMsr(Register, request);
                for (int attempt = 0; attempt < 50; attempt++)
                {
                    ulong answer = io.ReadMsr(Register);
                    if (answer >> 63 == 0)
                    {
                        error = (int)(answer >> 32 & 0xFF);
                        return (uint)answer;
                    }
                    Thread.SpinWait(5000);
                }
                error = -1;
                return 0;
            }
            finally
            {
                if (held) Gate.ReleaseMutex();
            }
        }

        static readonly Mutex Gate = new Mutex(false, "Local\\CoreMap.Mailbox");
    }

    /// Потолки множителя по ядрам: заводской предел, действующий потолок и его изменение.
    class CoreLimits
    {
        const int Span = 14;  // на столько ступеней ниже заводского предела можно опустить потолок
        // На столько ступеней выше заводского предела потолок можно поднять вручную (как в Omen Gaming Hub).
        // Примет ли процессор такое значение, видно по обратному чтению в Set. Частоту сверх того держит ещё и
        // таблица «множитель от числа работающих ядер» (54, 54, 52…): действует меньшее из двух.
        public const int Over = 4;

        readonly PawnIo io;   // null — имитация: значения ненастоящие, запись только меняет их в памяти
        readonly BlockMap blocks;
        readonly int[] lps;
        public readonly int[] Fused, Current;  // 0 — процессор не ответил
        public readonly int[] Initial;         // действующие потолки на момент запуска программы

        public CoreLimits(PawnIo io, CpuCore[] cores, BlockMap blocks)
        {
            this.io = io;
            this.blocks = blocks;
            lps = cores.Select(c => c.Lp).ToArray();
            Fused = new int[cores.Length];
            Current = new int[cores.Length];
            if (io == null)
                for (int i = 0; i < cores.Length; i++)
                {
                    Current[i] = cores[i].Kind == 'P' ? 53 : 46;
                    Fused[i] = lps[i] == 12 || lps[i] == 23 ? 54 : Current[i];
                }
            else Read();
            Initial = (int[])Current.Clone();
        }

        public bool Known(int index) { return Fused[index] > 0; }

        /// Значения, которые можно поставить ядру вручную, по убыванию: от Over ступеней выше заводского предела вниз.
        public int[] Choices(int index)
        {
            return Enumerable.Range(-Over, Span + Over + 1).Select(k => Fused[index] - k).Where(r => r >= 8).ToArray();
        }

        /// То же, но не выше заводского предела: в этих пределах работают измерения и подбор — кривые, поиск, ползунок.
        public int[] Factory(int index) { return Choices(index).Where(r => r <= Fused[index]).ToArray(); }

        void Read()
        {
            try
            {
                for (int i = 0; i < lps.Length; i++)
                {
                    if (!Native.Pin(lps[i])) continue;
                    int error;
                    uint data = OcMailbox.Query(io, OcMailbox.ReadCoreLimit, 0, 0, 0, out error);
                    // Один неудачный ответ не стирает то, что известно: заводской предел читается один раз,
                    // действующий потолок остаётся прежним до следующего удачного чтения.
                    if (error != 0) continue;
                    if (Fused[i] == 0) Fused[i] = (int)(data >> 8 & 0xFF);
                    Current[i] = (int)(data & 0xFF);
                }
            }
            finally { Native.Unpin(lps); }
        }

        /// Перечитывает действующие потолки из процессора: их мог сбросить сон или изменить другая программа.
        /// null — прочитано; иначе причина.
        public string Refresh()
        {
            if (io == null) return null;
            try { Read(); }
            catch (SensorException e) { return e.Message; }
            return null;
        }

        /// Действующие потолки отличаются от тех, что были при запуске программы.
        public bool Changed { get { return !Current.SequenceEqual(Initial); } }

        /// Возвращает потолки, какими они были при запуске программы. null — все приняты.
        public string Restore() { return Apply(Initial); }

        /// Ставит потолок каждому ядру из набора. null — все приняты.
        public string Apply(int[] ratios)
        {
            Refresh();
            string refusal = null;
            for (int i = 0; i < lps.Length; i++)
                if (Known(i) && Current[i] != ratios[i]) refusal = Set(i, ratios[i]) ?? refusal;
            return refusal;
        }

        /// Ставит потолок ядру, а с ним — всему его блоку: у кластера E-ядер потолок общий, процессор сам
        /// распространяет запись на остальные ядра. null — принято; иначе причина отказа.
        public string Set(int index, int ratio)
        {
            if (!Known(index)) return "процессор не сообщил потолок этого ядра";
            if (!Choices(index).Contains(ratio)) return "потолок " + ratio + " вне допустимого: от ×" + Choices(index).Last() + " до ×" + Choices(index).First();
            if (io == null)
            {
                foreach (int i in blocks.Of(index).Members) Current[i] = ratio;
                return null;
            }
            int error;
            try
            {
                if (!Native.Pin(lps[index])) return "не удалось привязать поток к процессору " + lps[index] + ": потолок не записан";
                OcMailbox.Query(io, OcMailbox.WriteCoreLimit, 0, 0, (uint)ratio, out error);
                Native.Unpin(lps);
                Read();
            }
            catch (SensorException e) { return e.Message; }
            finally { Native.Unpin(lps); }
            if (Current[index] == ratio) return null;
            return "процессор не принял потолок " + ratio + " для ядра " + lps[index] + ": код ответа " + error +
                ", действующий потолок остался " + Current[index];
        }
    }

    /// Режим --probe: ничего не меняет, только спрашивает. Результат — в data\probe.txt.
    static class Probe
    {
        public static int Run()
        {
            var text = new StringBuilder();
            try
            {
                CpuCore[] cores = Topology.Read().ToArray();
                int[] lps = cores.Select(c => c.Lp).ToArray();
                var io = new PawnIo(Path.Combine(Program.Root, "modules", "IntelMSR.bin"));
                text.AppendLine("probe " + DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss"));

                Native.Pin(lps[0]);
                foreach (uint msr in new uint[] { 0xCE, 0x194, 0x1AD, 0x1AE, 0x650, 0x651, 0x198, 0x199, 0x770, 0x771, 0x774 })
                    text.AppendLine("msr 0x" + msr.ToString("X3") + " on lp " + lps[0] + ": " + Read(io, msr));

                text.AppendLine();
                text.AppendLine("capabilities (command 0x01), by domain:");
                for (int domain = 0; domain < 6; domain++)
                {
                    int error;
                    uint data = OcMailbox.Query(io, OcMailbox.ReadCapabilities, domain, 0, 0, out error);
                    text.AppendLine("  domain " + domain + ": error " + error + " data 0x" + data.ToString("X8"));
                }

                text.AppendLine();
                text.AppendLine("per core: kind, command 0x1C three times (error:data), perf status ratio, aperf/mperf over 0.3 s");
                var aperf = new ulong[lps.Length];
                var mperf = new ulong[lps.Length];
                for (int i = 0; i < lps.Length; i++)
                {
                    Native.Pin(lps[i]);
                    aperf[i] = io.ReadMsr(0xE8);
                    mperf[i] = io.ReadMsr(0xE7);
                }
                Thread.Sleep(300);
                for (int i = 0; i < lps.Length; i++)
                {
                    Native.Pin(lps[i]);
                    ulong a = io.ReadMsr(0xE8) - aperf[i], m = io.ReadMsr(0xE7) - mperf[i];
                    string limits = "";
                    for (int round = 0; round < 3; round++)
                    {
                        int error;
                        uint data = OcMailbox.Query(io, OcMailbox.ReadCoreLimit, 0, 0, 0, out error);
                        limits += " " + error + ":0x" + data.ToString("X8");
                    }
                    text.AppendLine("  lp " + lps[i].ToString().PadLeft(2) + " " + cores[i].Kind + limits +
                        "  status " + (io.ReadMsr(0x198) >> 8 & 0xFF) + "  aperf/mperf " + (m > 0 ? ((double)a / m).ToString("0.000") : "-") +
                        "  mperf " + m);
                }
            }
            catch (Exception e) { text.AppendLine("ERROR: " + e); }
            Directory.CreateDirectory(Program.DataDir);
            File.WriteAllText(Path.Combine(Program.DataDir, "probe.txt"), text.ToString(), Encoding.UTF8);
            return 0;
        }

        static string Read(PawnIo io, uint msr)
        {
            try { return "0x" + io.ReadMsr(msr).ToString("X16"); }
            catch (SensorException e) { return "no access (" + e.Message.Substring(e.Message.LastIndexOf(' ') + 1) + ")"; }
        }
    }
}
