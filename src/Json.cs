// Чтение и запись файлов результатов и схем кристалла в формате JSON.
using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Text;
using System.Web.Script.Serialization;

namespace CoreMap
{
    /// Файл данных есть, но не читается: испорчен или записан не этой программой.
    class SpoiledFile : Exception
    {
        public SpoiledFile(string message, Exception cause) : base(message, cause) { }
    }

    static class Json
    {
        public static Dictionary<string, object> ReadFile(string path)
        {
            string text = File.ReadAllText(path, Encoding.UTF8);
            try
            {
                var reader = new JavaScriptSerializer { MaxJsonLength = int.MaxValue };
                var value = reader.Deserialize<Dictionary<string, object>>(text);
                if (value == null) throw new InvalidDataException("файл пуст");
                return value;
            }
            catch (Exception e) { throw Spoiled(path, e); }
        }

        /// Файл прочитан, но не разобран — как JSON или как данные программы (нет нужного поля). Сохраняет его
        /// рядом как .bad: следующая запись заменила бы его новым, а так прежнее содержимое остаётся владельцу.
        /// Возвращает ошибку с текстом для него. Зовут те, кто читает файл: ReadFile и разбор его полей.
        public static Exception Spoiled(string path, Exception cause)
        {
            if (cause is SpoiledFile) return cause;
            string name = Path.GetFileName(path), kept = "";
            try
            {
                File.Copy(path, path + ".bad", true);
                kept = "; он сохранён рядом как " + name + ".bad";
            }
            catch (Exception) { }  // копия — если получится: важнее причина, по которой файл не прочитан
            return new SpoiledFile("файл " + name + " не читается (" + Program.Why(cause) + ")" + kept, cause);
        }

        public static void WriteFile(string path, Dictionary<string, object> value)
        {
            var text = new StringBuilder();
            Write(text, value);
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            // Сначала во временный файл, потом подмена: обрыв посреди записи не оставит половину файла на месте целого.
            string fresh = path + ".tmp";
            File.WriteAllText(fresh, text.ToString(), new UTF8Encoding(false));
            if (File.Exists(path)) File.Replace(fresh, path, path + ".bak");
            else File.Move(fresh, path);
        }

        static void Write(StringBuilder text, object value)
        {
            if (value == null) { text.Append("null"); return; }
            if (value is string) { text.Append(new JavaScriptSerializer().Serialize(value)); return; }
            if (value is bool) { text.Append((bool)value ? "true" : "false"); return; }
            if (value is double)
            {
                double d = (double)value;  // отсутствующее значение хранится как NaN
                text.Append(double.IsNaN(d) ? "null" : d.ToString("0.###", CultureInfo.InvariantCulture));
                return;
            }
            if (value is int) { text.Append(((int)value).ToString(CultureInfo.InvariantCulture)); return; }
            var map = value as IDictionary<string, object>;
            if (map != null)
            {
                text.Append('{');
                bool firstKey = true;
                foreach (KeyValuePair<string, object> pair in map)
                {
                    if (!firstKey) text.Append(", ");
                    firstKey = false;
                    Write(text, pair.Key);
                    text.Append(": ");
                    Write(text, pair.Value);
                }
                text.Append('}');
                return;
            }
            text.Append('[');
            bool first = true;
            foreach (object item in (IEnumerable)value)
            {
                if (!first) text.Append(", ");
                first = false;
                Write(text, item);
            }
            text.Append(']');
        }

        /// Число из прочитанного файла; null превращается в NaN.
        public static double Number(object value)
        {
            return value == null ? double.NaN : Convert.ToDouble(value, CultureInfo.InvariantCulture);
        }

        public static double[] Row(object value)
        {
            if (value == null) return null;
            var row = new List<double>();
            foreach (object item in (IEnumerable)value) row.Add(Number(item));
            return row.ToArray();
        }

        public static List<double[]> Rows(object value)
        {
            var rows = new List<double[]>();
            foreach (object item in (IEnumerable)value) rows.Add(Row(item));
            return rows;
        }

        public static int[] Ints(object value)
        {
            var list = new List<int>();
            foreach (object item in (IEnumerable)value) list.Add(item == null ? 0 : Convert.ToInt32(item));
            return list.ToArray();
        }
    }
}
