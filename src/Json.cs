// Чтение и запись файлов результатов и схем кристалла в формате JSON.
using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Text;

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
                if (text.Trim().Length == 0) throw new InvalidDataException("файл пуст");
                var value = new Reader(text).Whole() as Dictionary<string, object>;
                if (value == null) throw new InvalidDataException("в файле не объект");
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
            if (value is string) { text.Append(Quote((string)value)); return; }
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

        /// Строка в кавычках, как её пишут в JSON.
        public static string Quote(string value)
        {
            var text = new StringBuilder("\"", value.Length + 2);
            foreach (char c in value)
            {
                if (c == '"' || c == '\\') text.Append('\\').Append(c);
                else if (c == '\n') text.Append("\\n");
                else if (c == '\r') text.Append("\\r");
                else if (c == '\t') text.Append("\\t");
                else if (c < ' ') text.Append("\\u").Append(((int)c).ToString("x4"));
                else text.Append(c);
            }
            return text.Append('"').ToString();
        }

        /// Разбор JSON. Свой, а не JavaScriptSerializer из System.Web.Extensions: тот тянет в процесс ещё и
        /// System.Web, System.Configuration и System.Xml. Резиденту (Tray.cs), которому нужны два маленьких файла
        /// профилей, это стоило 5,8 МБ выделенной памяти из 21,6 (замер на 275HX 13.10.2026).
        /// Объект читается как Dictionary<string, object>, массив — List<object>, число — int, если оно целое
        /// и помещается, иначе double; остальное — string, bool и null.
        class Reader
        {
            readonly string text;
            int at;

            public Reader(string text) { this.text = text; }

            /// Весь текст — одно значение, и после него ничего нет.
            public object Whole()
            {
                object value = Value();
                Skip();
                if (at < text.Length) throw Bad("лишнее после конца данных");
                return value;
            }

            Exception Bad(string what) { return new FormatException(what + ", знак " + at); }

            void Skip() { while (at < text.Length && (text[at] == ' ' || text[at] == '\n' || text[at] == '\r' || text[at] == '\t')) at++; }

            /// Следующий значащий знак — тот, что нужен; иначе файл не разобран.
            void Expect(char c)
            {
                Skip();
                if (at >= text.Length) throw Bad("файл обрывается");
                if (text[at] != c) throw Bad("ждали «" + c + "», а стоит «" + text[at] + "»");
                at++;
            }

            bool Next(char c)
            {
                Skip();
                if (at >= text.Length || text[at] != c) return false;
                at++;
                return true;
            }

            object Value()
            {
                Skip();
                if (at >= text.Length) throw Bad("файл обрывается");
                char c = text[at];
                if (c == '"') return String();
                if (c == '{')
                {
                    at++;
                    var map = new Dictionary<string, object>();
                    if (Next('}')) return map;
                    do
                    {
                        Skip();
                        if (at >= text.Length || text[at] != '"') throw Bad("ждали название поля");
                        string key = String();
                        Expect(':');
                        map[key] = Value();
                    }
                    while (Next(','));
                    Expect('}');
                    return map;
                }
                if (c == '[')
                {
                    at++;
                    var list = new List<object>();
                    if (Next(']')) return list;
                    do list.Add(Value());
                    while (Next(','));
                    Expect(']');
                    return list;
                }
                if (Word("true")) return true;
                if (Word("false")) return false;
                if (Word("null")) return null;
                return Numeral();
            }

            bool Word(string word)
            {
                if (string.CompareOrdinal(text, at, word, 0, word.Length) != 0) return false;
                at += word.Length;
                return true;
            }

            object Numeral()
            {
                int from = at;
                bool whole = true;
                for (; at < text.Length; at++)
                {
                    char c = text[at];
                    if (c == '.' || c == 'e' || c == 'E') whole = false;
                    else if (!(c >= '0' && c <= '9' || c == '-' || c == '+')) break;
                }
                string number = text.Substring(from, at - from);
                int small;
                if (whole && int.TryParse(number, NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out small)) return small;
                double value;
                if (number.Length == 0 || !double.TryParse(number, NumberStyles.Float, CultureInfo.InvariantCulture, out value))
                {
                    at = from;
                    throw Bad("не число и не значение");
                }
                return value;
            }

            string String()
            {
                at++;  // открывающая кавычка
                var value = new StringBuilder();
                while (true)
                {
                    if (at >= text.Length) throw Bad("строка не закрыта");
                    char c = text[at++];
                    if (c == '"') return value.ToString();
                    if (c != '\\') { value.Append(c); continue; }
                    if (at >= text.Length) throw Bad("строка не закрыта");
                    c = text[at++];
                    if (c == 'n') value.Append('\n');
                    else if (c == 'r') value.Append('\r');
                    else if (c == 't') value.Append('\t');
                    else if (c == 'b') value.Append('\b');
                    else if (c == 'f') value.Append('\f');
                    else if (c == 'u')
                    {
                        int code;
                        if (at + 4 > text.Length || !int.TryParse(text.Substring(at, 4), NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out code))
                            throw Bad("негодный знак \\u");
                        value.Append((char)code);
                        at += 4;
                    }
                    else value.Append(c);  // кавычка, обратная и прямая косая черта
                }
            }
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
