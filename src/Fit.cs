// Подбор потолков под температуру по снятым кривым: расчёт без нагрузки, без измерений и без окна.
//
// Кривая блока говорит, какая температура у него на каждом потолке (CurveStore.Options). Из этого здесь
// считается:
// - выбор потолков под температуру: ровной отсечкой (CurveStore.Choose) или переделом «по выгоде» (Rebalance);
// - что кривые говорят о потолках, которые стоят сейчас (Standing, CurveTemp);
// - средний множитель при выбранных потолках и его зависимость от температуры — график над ползунком (MeanCurve).
// Apply ставит выбранные потолки. Его итог описывает себя сам (FitOutcome.Describe) — как итог поиска замером
// (LimitSearch.Report) и профиль (Profile.Describe): текст составляет тот, кто знает, что произошло, окно
// его только показывает.
using System;
using System.Collections.Generic;
using System.Linq;

namespace CoreMap
{
    /// Итог постановки потолков по кривым: сколько блоков получили потолок и чего кривые не дали.
    class FitOutcome
    {
        public double MaxTemp;
        public bool Best;       // потолки делились «по выгоде», а не ровной отсечкой
        public int Set, Total;  // блоков с потолком по кривой и блоков всего
        public readonly List<string> None = new List<string>();  // блоки без кривой
        public readonly List<string> Hot = new List<string>();   // горячее цели и на нижней точке кривой
        public readonly List<string> Top = new List<string>();   // взята верхняя точка кривой: выше она не снята
        public string Refusal;  // отказ процессора принять потолок; null — отказов не было

        /// Итог одной строкой для владельца.
        public string Describe()
        {
            if (Set == 0) return "Под " + MaxTemp + " °C ничего не поставлено: на выбранной силе нагрузки кривых нет — сними их кнопкой ▶▶.";
            string report = "Под " + MaxTemp + " °C по кривым" + (Best ? ", по выгоде," : "") + " поставлено блоков: " + Set + " из " + Total +
                ". Это проба: чтобы потолки остались, сохрани профиль.";
            if (None.Count > 0) report += " Нет кривой: " + string.Join(", ", None) + ".";
            if (Hot.Count > 0) report += " Горячее цели и на нижней точке кривой: " + string.Join(", ", Hot) + ".";
            if (Top.Count > 0) report += " Кривая выше не снята: " + string.Join(", ", Top) + ".";
            if (Refusal != null) report += " Отказ: " + Refusal + ".";
            return report;
        }
    }

    /// Средний множитель по блокам на каждый градус ползунка: чем выше разрешённая температура, тем он больше.
    class MeanCurve
    {
        public int Low;              // температура первой точки, °C; дальше — по градусу
        public double[] Even, Best;  // при ровной отсечке и при переделе «по выгоде»; NaN — кривых нет
        public double[] P, E;        // при ровной отсечке отдельно: по P-ядрам и по блокам E-ядер
        public int[] Known;          // у скольких блоков есть кривая
        public bool[] Rough;         // часть блоков горячее этой температуры уже на нижней точке кривой: среднее завышено
        public int Knee;             // температура перегиба; -1 — перегиба нет
    }

    class CurveFit
    {
        public const double Spread = 6;  // на сколько градусов блок при переделе может уйти выше температуры с ползунка
        // Пределы температуры, °C: под какую подбираются потолки и до какой греет лесенка. Одни для окна и для
        // команд моста (Commands.cs).
        public const int Low = 55, High = 95;

        readonly BlockMap blocks;
        readonly CoreLimits limits;
        readonly CurveStore curves;

        public CurveFit(BlockMap blocks, CoreLimits limits, CurveStore curves)
        {
            this.blocks = blocks;
            this.limits = limits;
            this.curves = curves;
        }

        /// Что снятые кривые говорят о потолке каждого блока под температуру; null на месте блока — кривой нет.
        /// best — не ровная отсечка, а передел: см. Rebalance.
        public CurveChoice[] Choose(int level, double maxTemp, bool best = false)
        {
            bool[] known = blocks.All.Select(b => limits.Known(b.First)).ToArray();
            CurveChoice[] even = blocks.All.Select(b => known[b.Index] ? curves.Choose(b, level, maxTemp) : null).ToArray();
            if (!best) return even;
            return Rebalance(even, blocks.All.Select(b => known[b.Index] ? curves.Options(b, level) : null).ToArray(), maxTemp);
        }

        /// Передел потолков «по выгоде» (идея владельца): сумма температур блоков остаётся той же, что при ровной
        /// отсечке even, но делится иначе — так, чтобы сумма множителей была наибольшей. Блок, которому градус даёт
        /// много частоты, поднимается выше отсечки (не больше чем на Spread), блок, у которого ступень стоит
        /// градусов и частоты не добавляет, опускается. Перебор точный: вариантов на блок — по числу ступеней его
        /// кривой, температуры считаются в десятых долях градуса.
        static CurveChoice[] Rebalance(CurveChoice[] even, CurveChoice[][] options, double maxTemp)
        {
            int n = even.Length;
            var allowed = new CurveChoice[n][];
            var weight = new int[n][];
            int room = 0;  // сколько десятых градуса можно раздать сверх самых низких вариантов
            for (int u = 0; u < n; u++)
            {
                if (even[u] == null || options[u] == null) continue;
                // Самый низкий вариант разрешён всегда: иначе блоку, горячему уже на нижней точке, нечего было бы дать.
                allowed[u] = options[u].Where((o, k) => k == 0 || o.Temp <= maxTemp + Spread).ToArray();
                weight[u] = allowed[u].Select(o => (int)Math.Round((o.Temp - allowed[u][0].Temp) * 10)).ToArray();
                room += (int)Math.Round((even[u].Temp - allowed[u][0].Temp) * 10);
            }
            var value = new double[room + 1];
            var pick = new int[n][];
            for (int u = 0; u < n; u++)
            {
                if (allowed[u] == null) continue;
                var next = new double[room + 1];
                pick[u] = new int[room + 1];
                for (int c = 0; c <= room; c++)
                {
                    next[c] = double.NegativeInfinity;
                    for (int k = 0; k < allowed[u].Length; k++)
                    {
                        if (weight[u][k] > c) continue;
                        double v = value[c - weight[u][k]] + allowed[u][k].Ratio;
                        if (v > next[c] + 1e-9) { next[c] = v; pick[u][c] = k; }
                    }
                }
                value = next;
            }
            var result = new CurveChoice[n];
            for (int u = n - 1, c = room; u >= 0; u--)
            {
                if (allowed[u] == null) continue;
                int k = pick[u][c];
                result[u] = allowed[u][k];
                result[u].Hot = k == 0 && result[u].Temp > maxTemp;
                c -= weight[u][k];
            }
            return result;
        }

        /// Ставит блокам потолки под температуру по уже снятым кривым, без нагрузки; блоки без кривой не трогает.
        public FitOutcome Apply(int level, double maxTemp, bool best)
        {
            var outcome = new FitOutcome { MaxTemp = maxTemp, Best = best, Total = blocks.All.Length };
            CurveChoice[] picks = Choose(level, maxTemp, best);
            foreach (Block block in blocks.All)
            {
                int index = block.First;
                if (!limits.Known(index)) continue;
                CurveChoice choice = picks[block.Index];
                if (choice == null) { outcome.None.Add(block.Name); continue; }
                if (limits.Of(block) != choice.Limit) outcome.Refusal = limits.Set(index, choice.Limit) ?? outcome.Refusal;
                outcome.Set++;
                if (choice.Hot) outcome.Hot.Add(block.Name + " (×" + choice.Limit + " — " + choice.Temp.ToString("0") + "°)");
                else if (choice.Top) outcome.Top.Add(block.Name);
            }
            return outcome;
        }

        /// Что кривые говорят о потолке, который стоит у каждого блока сейчас. Если он совпал с выбором picks
        /// (выбор ползунка), берётся этот выбор; иначе — точка кривой, снятая при таком потолке. Кривой нет или
        /// потолок вне её — запись без температуры; null — потолок блока неизвестен.
        public CurveChoice[] Standing(int level, CurveChoice[] picks)
        {
            return blocks.All.Select(b =>
            {
                if (!limits.Known(b.First)) return null;
                int now = limits.Of(b);
                if (picks[b.Index] != null && picks[b.Index].Limit == now) return picks[b.Index];
                CurveChoice[] options = curves.Options(b, level) ?? new CurveChoice[0];
                CurveChoice at = options.FirstOrDefault(o => o.From == now) ?? options.LastOrDefault(o => o.Limit == now);
                // В записи кривой потолок может стоять ниже снятого (блок до него не дошёл); в списке нужен тот, что стоит.
                return at == null ? new CurveChoice { Limit = now, From = now, Temp = double.NaN, Ratio = double.NaN }
                    : new CurveChoice { Limit = now, From = at.From, Temp = at.Temp, Ratio = at.Ratio };
            }).ToArray();
        }

        /// Температура блока по его снятой кривой при потолке, который стоит сейчас; NaN — кривой нет или потолок вне её.
        public double CurveTemp(Block block, int level)
        {
            if (!limits.Known(block.First)) return double.NaN;
            CurveChoice[] options = curves.Options(block, level);
            CurveChoice at = options == null ? null : options.LastOrDefault(o => o.Limit == limits.Of(block));
            return at == null ? double.NaN : at.Temp;
        }

        /// Средний фактический множитель по блокам с кривой: по всем, по P-ядрам и по блокам E-ядер; NaN — таких нет.
        public double[] Means(CurveChoice[] picks)
        {
            var all = new List<double>();
            var p = new List<double>();
            var e = new List<double>();
            foreach (Block b in blocks.All)
            {
                if (picks[b.Index] == null) continue;
                all.Add(picks[b.Index].Ratio);
                (b.Kind == 'P' ? p : e).Add(picks[b.Index].Ratio);
            }
            return new[] { all, p, e }.Select(v => v.Count > 0 ? v.Average() : double.NaN).ToArray();
        }

        /// Средний множитель на каждый градус от low до high.
        public MeanCurve Mean(int level, int low, int high)
        {
            int count = high - low + 1;
            var mean = new MeanCurve
            {
                Low = low, Even = new double[count], Best = new double[count], P = new double[count], E = new double[count],
                Known = new int[count], Rough = new bool[count],
            };
            for (int k = 0; k < count; k++)
            {
                CurveChoice[] at = Choose(level, low + k);
                double[] m = Means(at);
                mean.Even[k] = m[0];
                mean.P[k] = m[1];
                mean.E[k] = m[2];
                mean.Best[k] = Means(Choose(level, low + k, true))[0];
                mean.Known[k] = at.Count(c => c != null);
                mean.Rough[k] = at.Any(c => c != null && c.Hot);
            }
            mean.Knee = Knee(mean.Even, mean.Rough, low);
            return mean;
        }

        /// Перегиб кривой «средний множитель от температуры»: точка, дальше всего отстоящая вверх от прямой между
        /// началом достоверной части и местом, где кривые кончаются. Правее него градус даёт меньше множителя, чем левее.
        /// Возвращает температуру; -1 — перегиба нет.
        static int Knee(double[] mean, bool[] rough, int low)
        {
            int a = -1, b = -1;
            for (int k = 0; k < mean.Length; k++)
            {
                if (double.IsNaN(mean[k])) continue;
                if (a < 0 && !rough[k]) a = k;
                if (k > 0 && mean[k] != mean[k - 1]) b = k;
            }
            if (a < 0 || b - a < 4) return -1;
            int best = -1;
            double far = 0.05;
            for (int k = a + 1; k < b; k++)
            {
                double above = mean[k] - (mean[a] + (mean[b] - mean[a]) * (k - a) / (b - a));
                if (above > far) { far = above; best = k; }
            }
            return best < 0 ? -1 : low + best;
        }
    }
}
