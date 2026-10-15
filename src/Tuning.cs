// Разгонный интерфейс процессора Intel (регистр 0x150) и опрос того, что процессор сейчас держит
// по каждому ядру. Intel этот интерфейс не документирует; раскладка полей и номера команд взяты
// из исходников Linux (drivers/platform/x86/intel_turbo_max_3.c) и справочного кода прошивок.
using System;
using System.Collections.Generic;
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
    ///
    /// Читают и пишут потолки два потока: окно раз в десять секунд перечитывает их, измерение в своём потоке
    /// ставит ступени. Каждое обращение к процессору — чтение всех ядер, запись с обратным чтением, применение
    /// набора — идёт под одним замком целиком: иначе устаревшее чтение одного потока ложилось бы поверх свежего
    /// чтения другого, и запись выглядела бы непринятой либо, наоборот, принятой.
    /// Fused и Current — то, что программа знает после последнего обращения; читать их можно без замка.
    class CoreLimits
    {
        // Ниже этого потолок не опускается: самый низкий экономичный множитель процессора (LFM, регистр 0xCE биты
        // 47:40; у 275HX — ×8, проба 12.10). Прежде здесь было «14 ступеней ниже заводского» — граница, взятая
        // агентом с потолка; владелец 15.10: «смысла ограничения вниз… зачем нам хуёвое ядро держать в состоянии
        // перегрева» (DECISIONS #31). Понижение потолка только охлаждает ядро.
        public const int Lowest = 8;
        // Выше заводского предела потолок не поднимается. Было 4 ступени (DECISIONS #17, 13.10); владелец 15.10:
        // «Плюс 4 не надо… Никаких плюсов… вверх расширять… продакшен такую утилиту я пускать не хочу… может…
        // компьютер кого не спалит» (DECISIONS #31). Проверки выше заводского (профиль на испытании, подтверждение,
        // метка) остаются: профиль старого вида может нести такие значения — они теперь просто не ставятся.
        public const int Over = 0;
        const int Attempts = 3;  // столько раз ядро спрашивается о потолке, прежде чем чтение считается неудачным

        readonly PawnIo io;        // null — имитация: «процессор» — массив в памяти
        readonly int[] simulated;  // имитация: потолки, которые стоят в «процессоре»
        readonly BlockMap blocks;
        readonly int[] lps;
        readonly object gate = new object();
        readonly bool[] silent;    // о потолке этого ядра процессор при последнем чтении не ответил: Current устарел

        public readonly int[] Fused, Current;  // 0 — процессор не ответил ни разу
        /// Самопроверка на имитации: ядра, о потолке которых «процессор» не отвечает. В работе не используется.
        public Predicate<int> Deaf;

        public CoreLimits(PawnIo io, CpuCore[] cores, BlockMap blocks)
        {
            this.io = io;
            this.blocks = blocks;
            lps = cores.Select(c => c.Lp).ToArray();
            Fused = new int[cores.Length];
            Current = new int[cores.Length];
            silent = new bool[cores.Length];
            if (io == null)
            {
                simulated = new int[cores.Length];
                for (int i = 0; i < cores.Length; i++)
                {
                    simulated[i] = cores[i].Kind == 'P' ? 53 : 46;
                    Fused[i] = lps[i] == 12 || lps[i] == 23 ? 54 : simulated[i];
                }
            }
            // Ядро, о котором процессор не ответил, остаётся неизвестным (Known): его потолок не показывается и не меняется.
            string unread = Read();
            if (unread != null) Program.Log("потолки множителей при запуске: " + unread);
        }

        public bool Known(int index) { return Fused[index] > 0; }

        /// Настоящий процессор, а не имитация в памяти.
        public bool Real { get { return io != null; } }

        /// Действующий потолок блока: он у блока общий и читается по первому ядру.
        public int Of(Block block) { return Current[block.First]; }

        /// Значения, которые можно поставить ядру вручную, по убыванию: от заводского предела (плюс Over, сейчас 0) вниз
        /// до самого низкого экономичного множителя процессора (Lowest).
        public int[] Choices(int index)
        {
            int top = Fused[index] + Over;
            return top < Lowest ? new int[0] : Enumerable.Range(Lowest, top - Lowest + 1).Reverse().ToArray();
        }

        /// То же, но не выше заводского предела: в этих пределах работают измерения и подбор — кривые, поиск, ползунок.
        public int[] Factory(int index) { return Choices(index).Where(r => r <= Fused[index]).ToArray(); }

        // ----- обращения к процессору: только под замком -----

        /// Спрашивает процессор о потолке ядра. false — поток не привязался к ядру или процессор не ответил.
        bool Ask(int index, out int fused, out int current)
        {
            fused = current = 0;
            if (io == null)
            {
                if (Deaf != null && Deaf(index)) return false;
                fused = Fused[index];
                current = simulated[index];
                return true;
            }
            if (!Native.Pin(lps[index])) return false;
            int error;
            uint data = OcMailbox.Query(io, OcMailbox.ReadCoreLimit, 0, 0, 0, out error);
            if (error != 0) return false;
            fused = (int)(data >> 8 & 0xFF);
            current = (int)(data & 0xFF);
            return true;
        }

        /// Пишет потолок ядра; у кластера E-ядер он общий, процессор сам распространяет запись на остальные ядра.
        /// false — поток не привязался к ядру, запись не отправлена. Принята ли запись, показывает только обратное
        /// чтение: error — что процессор ответил на саму команду.
        bool Tell(int index, int ratio, out int error)
        {
            error = 0;
            if (io == null)
            {
                foreach (int i in blocks.Of(index).Members) simulated[i] = ratio;
                return true;
            }
            if (!Native.Pin(lps[index])) return false;
            OcMailbox.Query(io, OcMailbox.WriteCoreLimit, 0, 0, (uint)ratio, out error);
            return true;
        }

        /// Читает потолки всех ядер. null — прочитаны все; иначе — о каких ядрах процессор не ответил.
        /// Неудачный ответ не стирает то, что известно: заводской предел читается один раз, действующий потолок
        /// остаётся прежним до следующего удачного чтения — но ядро помечается (silent), и чтение не выдаёт
        /// себя за удачное.
        string Read()
        {
            var unread = new List<int>();
            try
            {
                for (int i = 0; i < lps.Length; i++)
                {
                    int fused = 0, current = 0;
                    bool answered = false;
                    for (int attempt = 0; attempt < Attempts && !answered; attempt++) answered = Ask(i, out fused, out current);
                    silent[i] = !answered;
                    if (!answered) { unread.Add(lps[i]); continue; }
                    if (Fused[i] == 0) Fused[i] = fused;
                    Current[i] = current;
                }
            }
            catch (SensorException)
            {
                // Чтение оборвалось: про все ядра известно только то, что было раньше.
                for (int i = 0; i < silent.Length; i++) silent[i] = true;
                throw;
            }
            finally { if (io != null) Native.Unpin(lps); }
            return unread.Count == 0 ? null : "процессор не ответил о потолке ядер: " + string.Join(", ", unread);
        }

        // ----- что видят остальные -----

        /// Перечитывает действующие потолки из процессора: их мог сбросить сон или изменить другая программа.
        /// null — прочитаны все; иначе причина, и тогда часть потолков в Current — прежние, а не из процессора.
        public string Refresh()
        {
            lock (gate)
            {
                try { return Read(); }
                catch (SensorException e) { return e.Message; }
            }
        }

        /// Действующие потолки совпадают с набором (ядра, о которых процессор не ответил, не в счёт).
        public bool Same(int[] ratios)
        {
            return Enumerable.Range(0, lps.Length).All(i => !Known(i) || Current[i] == ratios[i]);
        }

        /// Ставит потолок каждому ядру из набора. null — все приняты.
        public string Apply(int[] ratios)
        {
            lock (gate)
            {
                Refresh();
                string refusal = null;
                for (int i = 0; i < lps.Length; i++)
                    // Потолок, о котором процессор не ответил, пишется и тогда, когда по памяти программы он уже такой.
                    if (Known(i) && (silent[i] || Current[i] != ratios[i])) refusal = Set(i, ratios[i]) ?? refusal;
                return refusal;
            }
        }

        /// Ставит потолок ядру, а с ним — всему его блоку. null — принято; иначе причина отказа.
        public string Set(int index, int ratio)
        {
            if (!Known(index)) return "процессор не сообщил потолок этого ядра";
            if (!Choices(index).Contains(ratio)) return "потолок " + ratio + " вне допустимого: от ×" + Choices(index).Last() + " до ×" + Choices(index).First();
            lock (gate)
            {
                int error;
                try
                {
                    if (!Tell(index, ratio, out error)) return "не удалось привязать поток к процессору " + lps[index] + ": потолок не записан";
                    Read();
                }
                catch (SensorException e) { return e.Message; }
                finally { if (io != null) Native.Unpin(lps); }
                if (silent[index])
                    return "потолок " + ratio + " для ядра " + lps[index] + " записан, но процессор не ответил, принял ли он его";
                if (Current[index] == ratio) return null;
                return "процессор не принял потолок " + ratio + " для ядра " + lps[index] + ": код ответа " + error +
                    ", действующий потолок остался " + Current[index];
            }
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
