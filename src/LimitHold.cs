// Исходные потолки на время измерения.
//
// Измерение, которое двигает потолки (лесенка кривой, поиск замером), обязано оставить после себя то, что
// стояло до него, — на любом пути выхода. Эта обязанность собрана здесь: измерение берёт исходные потолки
// (Take), по ходу возвращает их блокам (Return) или оставляет блоку найденный (Accept), а в конце основа
// измерений возвращает всё, что ещё не возвращено (Release). Сами измерения исходные потолки не хранят.
//
// На случай, когда вернуть некому — процесс убит или упал посреди измерения, — исходные потолки лежат в
// файле-метке data\limits-held.json: он пишется до первой записи в процессор и удаляется после возврата.
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
