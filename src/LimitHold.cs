// Исходные потолки на время измерения.
//
// Измерение, которое двигает потолки (лесенка кривой, поиск замером), обязано оставить после себя то, что
// стояло до него, — на любом пути выхода. Эта обязанность собрана здесь: измерение берёт исходные потолки
// (Take), по ходу возвращает их блокам (Return) или оставляет блоку найденный (Accept), а в конце основа
// измерений возвращает всё, что ещё не возвращено (Release). Сами измерения исходные потолки не хранят.
//
// На случай, когда вернуть некому — процесс убит или упал посреди измерения, — исходные потолки лежат в
// файле-метке data\limits-held.json: он пишется до первой записи в процессор и удаляется после возврата.
// Следующий запуск программы, найдя метку, возвращает потолки по ней (Recover).
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;

namespace CoreMap
{
    class LimitHold
    {
        readonly CoreLimits limits;
        readonly BlockMap blocks;
        readonly int[] lps;
        readonly string cpu, path;  // path = null — имитация: метка не пишется
        int[] held;                 // исходные потолки по ядрам; null — сейчас ничего не взято

        public LimitHold(CoreLimits limits, BlockMap blocks, int[] lps, string cpu, string saveDir)
        {
            this.limits = limits;
            this.blocks = blocks;
            this.lps = lps;
            this.cpu = cpu;
            path = saveDir == null ? null : Path.Combine(saveDir, "limits-held.json");
        }

        /// Начало измерения: запоминает потолки, которые стоят сейчас, и пишет метку. Не записалась метка —
        /// исключение: измерение без неё потолки трогать не должно.
        public void Take()
        {
            limits.Refresh();  // исходными должны стать потолки из процессора, а не те, что программа помнит
            held = (int[])limits.Current.Clone();
            try { Write(); }
            catch (Exception e)
            {
                held = null;
                throw new IOException("не записан файл с исходными потолками, потолки не тронуты (" + e.Message + ")", e);
            }
        }

        /// Блок остаётся с потолком, который стоит сейчас (поиск замером нашёл его): исходным для блока становится он.
        public void Accept(Block block)
        {
            foreach (int m in block.Members) held[m] = limits.Current[m];
            // Со старой меткой следующий запуск после падения вернул бы блоку потолок до поиска — это безопасно.
            try { Write(); }
            catch (Exception e) { Program.Log("метка исходных потолков не обновлена: " + e); }
        }

        /// Возвращает блоку исходный потолок. null — он и стоит либо принят; иначе причина отказа.
        public string Return(Block block)
        {
            return limits.Of(block) == held[block.First] ? null : limits.Set(block.First, held[block.First]);
        }

        /// Конец измерения: возвращает исходный потолок каждому блоку, у которого стоит другой, и убирает метку.
        /// null — возвращать было нечего либо всё принято; иначе причина отказа.
        public string Release()
        {
            if (held == null) return null;
            // Сверка с процессором, а не с тем, что программа помнит: после отказа записи это могло разойтись.
            // Не прочитались потолки — исходные пишутся всем блокам подряд.
            bool unread = limits.Refresh() != null;
            string refusal = null;
            foreach (Block block in blocks.All)
                if (limits.Known(block.First) && (unread || limits.Of(block) != held[block.First]))
                    refusal = limits.Set(block.First, held[block.First]) ?? refusal;
            held = null;
            Erase();
            return refusal;
        }

        /// Запуск программы. Метка осталась — прошлый запуск оборвался посреди измерения, и у блока мог остаться
        /// потолок ступени: возвращает потолки по метке. Выше заводского предела не поднимает: метка могла остаться
        /// и после зависания машины, а потолки выше заводских после сбоя проверяет профиль (Profile.Startup).
        /// Возвращает строку для владельца; null — метки нет или потолки с ней совпадают.
        public string Recover()
        {
            if (path == null || !File.Exists(path)) return null;
            const string intro = "Прошлый запуск оборвался посреди измерения. ";
            try
            {
                Dictionary<string, object> saved = Json.ReadFile(path);
                if ((saved["cpu"] as string) != cpu || !Json.Ints(saved["lps"]).SequenceEqual(lps)) return null;  // метка от другого процессора
                int[] before = Json.Ints(saved["limits"]);
                var back = new List<string>();
                var above = new List<string>();
                string refusal = null;
                foreach (Block block in blocks.All)
                {
                    int index = block.First;
                    if (!limits.Known(index) || before[index] <= 0 || limits.Of(block) == before[index]) continue;
                    if (before[index] > limits.Fused[index]) { above.Add(block.Name + " ×" + before[index]); continue; }
                    string refused = limits.Set(index, before[index]);
                    if (refused == null) back.Add(block.Name + " ×" + before[index]);
                    else refusal = refused;
                }
                if (back.Count > 0) limits.Rebase();
                if (back.Count == 0 && above.Count == 0 && refusal == null) return null;
                return (intro + (back.Count > 0 ? "Блокам возвращены потолки, стоявшие до него: " + string.Join(", ", back) + ". " : "") +
                    (above.Count > 0 ? "Не возвращены потолки выше заводского предела — поставь их сам: " + string.Join(", ", above) + ". " : "") +
                    (refusal != null ? "Отказ: " + refusal + "." : "")).TrimEnd();
            }
            catch (Exception e)
            {
                Program.Log("метка исходных потолков не прочитана: " + e);
                return intro + "Файл с потолками, стоявшими до него, не прочитан — проверь потолки блоков.";
            }
            finally { Erase(); }
        }

        void Write()
        {
            if (path == null) return;
            Json.WriteFile(path, new Dictionary<string, object>
            {
                { "cpu", cpu }, { "time", DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") }, { "lps", lps }, { "limits", held },
            });
        }

        void Erase()
        {
            if (path == null) return;
            try
            {
                File.Delete(path);
                File.Delete(path + ".bak");  // остаётся от обновления метки (Json.WriteFile)
            }
            catch (Exception e) { Program.Log("метка исходных потолков не удалена: " + e); }
        }
    }
}
