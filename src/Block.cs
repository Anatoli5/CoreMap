// Блок — единица, у которой общие потолок множителя и измерения: одно P-ядро или кластер E-ядер целиком.
//
// Кластер — E-ядра с общим L2. Отдельно управлять множителем его ядер нельзя, показатель температуры
// у него один, поэтому на схеме, в нагрузке, в измерениях и в таблицах он идёт одной единицей
// (решение владельца: «одно четырёхголовое ядро»).
// Состав блоков, их названия, перевод «ядро → блок» и то, что считать температурой и множителем блока,
// определены только здесь. Список блоков строит Core (Core.Blocks); остальные получают блоки от него
// и состав заново не вычисляют.
using System.Collections.Generic;
using System.Linq;

namespace CoreMap
{
    class Block
    {
        public readonly int Index;      // порядковый номер блока: строка в таблицах и списках
        public readonly int[] Members;  // ядра блока: позиции в общем списке ядер, по возрастанию
        public readonly int First;      // первое ядро: по его датчику блок меряется, через него блоку ставится потолок
        public readonly int Lp;         // номер первого ядра в Windows: под ним блок записан в файлах кривых
        public readonly char Kind;      // 'P' или 'E'
        public readonly string Name;    // «0» или «2–5»
        public readonly string Title;   // «ядро 0» или «блок E-ядер 2–5»

        /// Блоки создаёт только BlockMap.
        public Block(int index, int[] members, CpuCore[] cores)
        {
            Index = index;
            Members = members;
            First = members[0];
            Lp = cores[First].Lp;
            Kind = cores[First].Kind;
            Name = members.Length == 1 ? Lp.ToString() : Lp + "–" + cores[members[members.Length - 1]].Lp;
            Title = (members.Length == 1 ? "ядро " : "блок E-ядер ") + Name;
        }

        /// Температура блока из показаний по ядрам: датчик первого ядра. У кластера показатель общий
        /// (измерено на 275HX: все четыре ядра показывают одно и то же), а сохранённые кривые сняты именно
        /// по первому ядру — новые замеры должны с ними сравниваться.
        public double Temp(double[] byCore) { return byCore[First]; }

        /// Фактический множитель блока из множителей по ядрам: среднее по тем ядрам, у которых он известен;
        /// NaN — ни у одного.
        public double Ratio(double[] byCore)
        {
            double sum = 0;
            int count = 0;
            foreach (int m in Members)
            {
                if (double.IsNaN(byCore[m])) continue;
                sum += byCore[m];
                count++;
            }
            return count > 0 ? sum / count : double.NaN;
        }
    }

    /// Все блоки процессора по порядку первых ядер и перевод «ядро → блок».
    class BlockMap
    {
        public readonly Block[] All;
        readonly Block[] byCore;

        public BlockMap(CpuCore[] cores)
        {
            // E-ядра одного кластера — один блок; всякое другое ядро — блок само по себе.
            var groups = new List<List<int>>();
            var clusters = new Dictionary<int, List<int>>();
            for (int i = 0; i < cores.Length; i++)
            {
                bool clustered = cores[i].Kind == 'E' && cores[i].Cluster >= 0;
                List<int> group;
                if (!clustered || !clusters.TryGetValue(cores[i].Cluster, out group))
                {
                    groups.Add(group = new List<int>());
                    if (clustered) clusters[cores[i].Cluster] = group;
                }
                group.Add(i);
            }
            All = groups.Select((g, k) => new Block(k, g.ToArray(), cores)).ToArray();
            byCore = new Block[cores.Length];
            foreach (Block b in All)
                foreach (int m in b.Members) byCore[m] = b;
        }

        /// Блок, в который входит ядро: само P-ядро или весь его кластер E-ядер.
        public Block Of(int core) { return byCore[core]; }

        /// Блок по номеру его первого ядра в Windows — так блоки записаны в файлах кривых; null — такого нет.
        public Block ByLp(int lp) { return All.FirstOrDefault(b => b.Lp == lp); }
    }
}
