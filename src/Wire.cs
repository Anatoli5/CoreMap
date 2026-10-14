// Провод моста: как сообщение лежит в потоке байтов.
//
// Сообщение — четыре байта длины (младший первым) и столько же байт UTF-8: один объект JSON. От канала такая
// упаковка не зависит — её можно положить на любой поток байтов, поэтому канал заменяем.
//
// Здесь же запись JSON без выделения памяти (Out) — для сообщения «состояние», которое уходит дважды в секунду.
// Обычная запись (Json.cs) строит на него дерево словарей: 21 КБ мусора на сообщение против нуля (замер пробы
// 13.10.2026, outputs\bridge-probe-2026-10-13). Мусор — это сборка, а сборка останавливает все потоки программы,
// в том числе те, что меряют задержки. Сообщения, которые шлются по изменению, пишутся обычной записью.
using System;
using System.IO;
using System.Text;

namespace CoreMap
{
    static class Wire
    {
        public const int Cap = 1 << 20;  // сообщение длиннее — нарушение протокола: соединение рвётся
        public const int Head = 4;

        /// Кладёт текст в buffer следом за четырьмя байтами длины; возвращает, сколько байт слать. Буферы растут сами.
        public static int Pack(StringBuilder text, ref char[] chars, ref byte[] buffer)
        {
            int count = text.Length;
            if (chars.Length < count) chars = new char[count * 2];
            text.CopyTo(0, chars, 0, count);
            int most = Encoding.UTF8.GetMaxByteCount(count) + Head;
            if (buffer.Length < most) buffer = new byte[most];
            int bytes = Encoding.UTF8.GetBytes(chars, 0, count, buffer, Head);
            buffer[0] = (byte)bytes;
            buffer[1] = (byte)(bytes >> 8);
            buffer[2] = (byte)(bytes >> 16);
            buffer[3] = (byte)(bytes >> 24);
            return bytes + Head;
        }

        /// То же одним новым массивом — там, где сообщений мало (сторона окна).
        public static byte[] Frame(string message)
        {
            byte[] body = Encoding.UTF8.GetBytes(message);
            var frame = new byte[body.Length + Head];
            frame[0] = (byte)body.Length;
            frame[1] = (byte)(body.Length >> 8);
            frame[2] = (byte)(body.Length >> 16);
            frame[3] = (byte)(body.Length >> 24);
            Buffer.BlockCopy(body, 0, frame, Head, body.Length);
            return frame;
        }
    }

    /// Читает из потока сообщения целиком; несколько пришедших разом стоят одного чтения.
    class FrameReader
    {
        public byte[] Buffer = new byte[64 * 1024];
        int have, taken;

        /// false — другая сторона закрыла поток. Сообщение — Buffer[offset .. offset + length).
        public bool Next(Stream stream, out int offset, out int length)
        {
            if (taken > 0)
            {
                System.Buffer.BlockCopy(Buffer, taken, Buffer, 0, have - taken);
                have -= taken;
                taken = 0;
            }
            while (true)
            {
                if (have >= Wire.Head)
                {
                    int size = Buffer[0] | Buffer[1] << 8 | Buffer[2] << 16 | Buffer[3] << 24;
                    if (size <= 0 || size > Wire.Cap) throw new InvalidDataException("длина сообщения " + size + " вне 1.." + Wire.Cap);
                    if (have >= Wire.Head + size)
                    {
                        offset = Wire.Head;
                        length = size;
                        taken = Wire.Head + size;
                        return true;
                    }
                    if (Wire.Head + size > Buffer.Length) Array.Resize(ref Buffer, Wire.Head + size);
                }
                int got = stream.Read(Buffer, have, Buffer.Length - have);
                if (got == 0) { offset = length = 0; return false; }
                have += got;
            }
        }
    }

    /// Запись JSON по знаку, без промежуточных строк: цифры дописываются по одной.
    static class Out
    {
        public static void Int(StringBuilder s, long v)
        {
            if (v < 0) { s.Append('-'); v = -v; }
            if (v >= 10) Int(s, v / 10);
            s.Append((char)('0' + (int)(v % 10)));
        }

        /// Число с не более чем decimals знаками после запятой (до трёх); NaN пишется как null — так же, как в Json.cs.
        public static void Fix(StringBuilder s, double v, int decimals)
        {
            if (double.IsNaN(v) || double.IsInfinity(v)) { s.Append("null"); return; }
            long scale = decimals == 0 ? 1 : decimals == 1 ? 10 : decimals == 2 ? 100 : 1000;
            long n = (long)Math.Round(v * scale);
            if (n < 0) { s.Append('-'); n = -n; }
            Int(s, n / scale);
            long rest = n % scale;
            if (rest == 0) return;
            s.Append('.');
            for (long digit = scale / 10; digit > 0; digit /= 10)
            {
                s.Append((char)('0' + (int)(rest / digit % 10)));
                if (rest % digit == 0) break;
            }
        }

        public static void Str(StringBuilder s, string v)
        {
            if (v == null) { s.Append("null"); return; }
            s.Append('"');
            foreach (char c in v)
            {
                if (c == '"' || c == '\\') s.Append('\\').Append(c);
                else if (c == '\n') s.Append("\\n");
                else if (c == '\r') s.Append("\\r");
                else if (c == '\t') s.Append("\\t");
                else if (c < ' ') s.Append("\\u00").Append("0123456789abcdef"[c >> 4]).Append("0123456789abcdef"[c & 15]);
                else s.Append(c);
            }
            s.Append('"');
        }

        /// Название поля; запятая перед ним ставится сама, если поле в объекте не первое.
        public static StringBuilder Key(StringBuilder s, string name)
        {
            if (s[s.Length - 1] != '{') s.Append(',');
            return s.Append('"').Append(name).Append("\":");
        }

        public static void Bool(StringBuilder s, bool v) { s.Append(v ? "true" : "false"); }

        public static void Row(StringBuilder s, double[] values, int decimals)
        {
            s.Append('[');
            for (int i = 0; i < values.Length; i++)
            {
                if (i > 0) s.Append(',');
                Fix(s, values[i], decimals);
            }
            s.Append(']');
        }

        public static void Row(StringBuilder s, int[] values)
        {
            s.Append('[');
            for (int i = 0; i < values.Length; i++)
            {
                if (i > 0) s.Append(',');
                Int(s, values[i]);
            }
            s.Append(']');
        }

        /// Признаки по ядрам: 1 и 0 — короче, чем true и false, а сообщение уходит дважды в секунду.
        public static void Row(StringBuilder s, bool[] values)
        {
            s.Append('[');
            for (int i = 0; i < values.Length; i++)
            {
                if (i > 0) s.Append(',');
                s.Append(values[i] ? '1' : '0');
            }
            s.Append(']');
        }
    }
}
