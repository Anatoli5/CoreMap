// Топология процессора из Windows и схема расположения ядер на кристалле.
using System;
using System.Collections;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using Microsoft.Win32;

namespace CoreMap
{
    class CpuCore
    {
        public int Lp;            // номер логического процессора Windows
        public int Core;          // номер физического ядра
        public char Kind;         // 'P' или 'E'
        public int Cluster = -1;  // номер кластера с общим L2; -1 — L2 свой
    }

    static class Topology
    {
        const int RelationCore = 0, RelationCache = 2, RelationAll = 0xFFFF;

        static List<int> Bits(ulong mask)
        {
            var bits = new List<int>();
            for (int i = 0; i < 64; i++) if ((mask >> i & 1) != 0) bits.Add(i);
            return bits;
        }

        public static List<CpuCore> Read()
        {
            int size = 0;
            Native.GetLogicalProcessorInformationEx(RelationAll, IntPtr.Zero, ref size);
            IntPtr buf = Marshal.AllocHGlobal(size);
            var cores = new List<KeyValuePair<int, List<int>>>();  // (класс эффективности, процессоры ядра)
            var l2 = new List<List<int>>();                        // процессоры с общим L2
            try
            {
                if (!Native.GetLogicalProcessorInformationEx(RelationAll, buf, ref size))
                    throw new InvalidOperationException("Windows не отдала топологию процессора");
                for (int off = 0; off < size; off += Marshal.ReadInt32(buf, off + 4))
                {
                    int relation = Marshal.ReadInt32(buf, off);
                    if (relation == RelationCore)
                    {
                        if (Marshal.ReadInt16(buf, off + 40) != 0)
                            throw new NotSupportedException("больше 64 логических процессоров пока не поддерживается");
                        cores.Add(new KeyValuePair<int, List<int>>(
                            Marshal.ReadByte(buf, off + 9), Bits((ulong)Marshal.ReadInt64(buf, off + 32))));
                    }
                    else if (relation == RelationCache && Marshal.ReadByte(buf, off + 8) == 2)
                    {
                        if (Marshal.ReadInt16(buf, off + 48) == 0)
                            l2.Add(Bits((ulong)Marshal.ReadInt64(buf, off + 40)));
                    }
                }
            }
            finally { Marshal.FreeHGlobal(buf); }

            int topClass = cores.Max(c => c.Key);
            var result = new List<CpuCore>();
            for (int i = 0; i < cores.Count; i++)
                foreach (int lp in cores[i].Value)
                    result.Add(new CpuCore { Lp = lp, Core = i, Kind = cores[i].Key == topClass ? 'P' : 'E' });
            result = result.OrderBy(c => c.Lp).ToList();

            int clusters = 0;
            foreach (List<int> group in l2.OrderBy(g => g[0]))
            {
                var members = result.Where(c => group.Contains(c.Lp)).ToList();
                if (members.Select(c => c.Core).Distinct().Count() < 2) continue;
                foreach (CpuCore c in members) c.Cluster = clusters;
                clusters++;
            }
            return result;
        }

        public static void Identity(out string name, out string identifier)
        {
            using (RegistryKey key = Registry.LocalMachine.OpenSubKey(@"HARDWARE\DESCRIPTION\System\CentralProcessor\0"))
            {
                name = ((string)key.GetValue("ProcessorNameString", "процессор")).Trim();
                identifier = (string)key.GetValue("Identifier", "");
            }
        }
    }

    /// Плитка схемы кристалла: где стоит блок и в каком порядке его ядра лежат внутри плитки.
    /// Состав плитка не определяет — он у блока; своё у неё только место и порядок.
    class DieTile
    {
        public Block Block;
        public int[] Places;  // ядра блока в порядке мест на плитке: слева направо, сверху вниз
        public int Col, Row;
    }

    /// Схема кристалла: готовая из папки layouts или строка в порядке нумерации Windows.
    class DieLayout
    {
        public string Name, Note;
        public bool Confirmed;
        public int Cols, Rows;
        public List<DieTile> Tiles = new List<DieTile>();

        public static DieLayout Pick(CpuCore[] cores, BlockMap blocks, string identifier, string folder)
        {
            string pattern = new string(cores.Select(c => c.Kind).ToArray());
            if (Directory.Exists(folder))
                foreach (string path in Directory.GetFiles(folder, "*.json").OrderBy(p => p))
                {
                    var doc = Json.ReadFile(path);
                    var match = (Dictionary<string, object>)doc["match"];
                    if ((string)match["pattern"] != pattern || !identifier.Contains((string)match["identifier"])) continue;
                    var layout = new DieLayout
                    {
                        Name = (string)doc["name"], Note = (string)doc["note"], Confirmed = (bool)doc["confirmed"],
                        Cols = Convert.ToInt32(doc["cols"]), Rows = Convert.ToInt32(doc["rows"]),
                    };
                    foreach (Dictionary<string, object> u in (IEnumerable)doc["units"])
                    {
                        int[] places = ((IEnumerable)u["lps"]).Cast<object>()
                            .Select(lp => Array.FindIndex(cores, c => c.Lp == Convert.ToInt32(lp))).ToArray();
                        layout.Tiles.Add(new DieTile
                        {
                            Block = places.Length > 0 && places[0] >= 0 ? blocks.Of(places[0]) : null, Places = places,
                            Col = Convert.ToInt32(u["col"]), Row = Convert.ToInt32(u["row"]),
                        });
                    }
                    if (layout.Matches(blocks)) return layout;
                    // Схема с плитками, не совпадающими с блоками, показывала бы одно, а нагружала и меряла другое.
                    Program.Log("схема " + Path.GetFileName(path) + " не подходит: её плитки не совпадают с блоками процессора");
                }
            return Linear(blocks);
        }

        /// Плитки схемы — ровно блоки процессора: каждому блоку одна плитка с теми же ядрами.
        bool Matches(BlockMap blocks)
        {
            return Tiles.Count == blocks.All.Length && Tiles.Select(t => t.Block).Distinct().Count() == Tiles.Count &&
                Tiles.All(t => t.Block != null && t.Places.OrderBy(i => i).SequenceEqual(t.Block.Members));
        }

        static DieLayout Linear(BlockMap blocks)
        {
            var layout = new DieLayout
            {
                Name = "порядок нумерации Windows",
                Note = "Для этого процессора схемы кристалла нет: ядра показаны в порядке нумерации Windows.",
            };
            layout.Cols = Math.Min(blocks.All.Length, 6);
            layout.Rows = (blocks.All.Length + layout.Cols - 1) / layout.Cols;
            foreach (Block b in blocks.All)
                layout.Tiles.Add(new DieTile { Block = b, Places = b.Members, Col = b.Index % layout.Cols, Row = b.Index / layout.Cols });
            return layout;
        }

        /// Условные координаты каждого ядра на схеме, шаг — половина клетки.
        public double[][] Positions(int count)
        {
            var pos = new double[count][];
            foreach (DieTile t in Tiles)
                for (int k = 0; k < t.Places.Length; k++)
                    pos[t.Places[k]] = t.Places.Length == 1
                        ? new[] { t.Col * 2 + 0.5, t.Row * 2 + 0.5 }
                        : new[] { t.Col * 2 + (double)(k % 2), t.Row * 2 + (double)(k / 2) };
            return pos;
        }
    }
}
