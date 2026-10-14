// Сторона окна на мосту: подключается к каналу движка, называет себя, шлёт команды и принимает сообщения.
//
// Сейчас ею пользуются проверки (BridgeCheck.cs); она же станет концом моста у нового окна. Подключение — с
// правом «узнать, кто я» (Identification): движок может спросить Windows, кто клиент, но действовать от его
// имени не может — как не смог бы и тот, кто занял бы имя канала вместо движка.
// Проверкам нужны поля пришедшего сообщения, и для них оно разбирается. Окно (WebWindow.cs) не разбирает: оно
// задаёт Text и получает текст каждого сообщения как есть — для страницы.
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Pipes;
using System.Security.Principal;
using System.Text;
using System.Threading;

namespace CoreMap
{
    class BridgeClient : IDisposable
    {
        readonly string name, server;
        readonly object gate = new object();  // запись в канал
        readonly Dictionary<int, Dictionary<string, object>> replies = new Dictionary<int, Dictionary<string, object>>();
        readonly Dictionary<string, Dictionary<string, object>> last = new Dictionary<string, Dictionary<string, object>>();
        readonly Dictionary<string, int> counts = new Dictionary<string, int>();
        readonly AutoResetEvent arrived = new AutoResetEvent(false);  // пришло сообщение или канал закрылся
        NamedPipeClientStream pipe;
        int lastId;

        /// Канал закрыт движком или оборван; до подключения — false.
        public volatile bool Closed;

        /// Задано — сообщения не разбираются, а отдаются сюда текстом, в потоке чтения. Задаётся до Connect.
        public Action<string> Text;

        /// Вызывается один раз, в потоке чтения, когда канал закрылся. Задаётся до Connect.
        public Action Lost;

        /// server — «.» для этой машины; другое имя нужно только проверке, что по сети канал не открывается.
        public BridgeClient(string name, string server = ".")
        {
            this.name = name;
            this.server = server;
        }

        /// Подключается, ждёт канал не дольше milliseconds. null — подключён; иначе причина.
        public string Connect(int milliseconds)
        {
            double until = Native.Now() + milliseconds / 1000.0;
            while (true)
            {
                // Пока канала нет, подключение .NET крутит ядро вхолостую: сначала дешёвый вопрос, есть ли он.
                if (Access.Exists(server, name))
                {
                    var stream = new NamedPipeClientStream(server, name, PipeDirection.InOut, PipeOptions.Asynchronous, TokenImpersonationLevel.Identification);
                    try
                    {
                        stream.Connect(200);
                        pipe = stream;
                        new Thread(ReadLoop) { IsBackground = true, Name = "мост-окно" }.Start();
                        return null;
                    }
                    // Канал занят другим клиентом: Windows отвечает то истечением времени, то ошибкой ввода-вывода
                    // («время ожидания семафора истекло»). И то и другое — повод попробовать ещё раз до срока.
                    catch (TimeoutException) { stream.Dispose(); }
                    catch (IOException) { stream.Dispose(); }
                    catch (UnauthorizedAccessException e) { stream.Dispose(); return "доступ запрещён: " + Program.Why(e); }
                }
                if (Native.Now() >= until) return Access.Exists(server, name) ? "канал занят" : "канала нет";
                Thread.Sleep(50);
            }
        }

        /// Канал, как его видит эта сторона: владелец, список доступа, метка.
        public string Security { get { return Access.Describe(pipe.SafePipeHandle); } }

        /// Exe процесса, который держит канал; null — узнать не удалось.
        public string ServerExe { get { return Access.ImagePath(Access.ServerProcess(pipe.SafePipeHandle)); } }

        void ReadLoop()
        {
            try
            {
                var frames = new FrameReader();
                int offset, length;
                while (frames.Next(pipe, out offset, out length))
                {
                    string text = Encoding.UTF8.GetString(frames.Buffer, offset, length);
                    if (Text != null)
                    {
                        Text(text);
                        continue;
                    }
                    var message = Json.Parse(text) as Dictionary<string, object>;
                    object topic;
                    if (message == null || !message.TryGetValue("t", out topic) || !(topic is string)) continue;
                    lock (last)
                    {
                        if ((string)topic == "reply") replies[(int)message["re"]] = message;
                        else
                        {
                            int count;
                            counts.TryGetValue((string)topic, out count);
                            counts[(string)topic] = count + 1;
                            last[(string)topic] = message;
                        }
                    }
                    arrived.Set();
                }
            }
            catch (Exception) { }  // обрыв канала и сообщение, которое не разобралось, для этой стороны одно: связи больше нет
            Closed = true;
            arrived.Set();
            if (Lost != null) Lost();
        }

        /// Шлёт в канал байты как есть — проверке, что движок переживает негодное сообщение.
        public void Raw(byte[] bytes)
        {
            lock (gate) pipe.Write(bytes, 0, bytes.Length);
        }

        /// Называет себя движку. Ответ — как у Ask.
        public Dictionary<string, object> Hello(int protocol = Bridge.Protocol) { return Send(0, "hello", new object[] { "proto", protocol }, 5); }

        /// Шлёт команду с аргументами (название, значение, название, значение…) и ждёт ответа не дольше seconds.
        /// null — ответа нет: канал закрыт или время вышло.
        public Dictionary<string, object> Ask(string command, params object[] pairs) { return Send(++lastId, command, pairs, 20); }

        Dictionary<string, object> Send(int id, string command, object[] pairs, double seconds)
        {
            var message = new Dictionary<string, object> { { "id", id }, { "c", command } };
            for (int i = 0; i < pairs.Length; i += 2) message[(string)pairs[i]] = pairs[i + 1];
            try { Raw(Wire.Frame(Json.Text(message))); }
            catch (Exception) { return null; }
            Dictionary<string, object> reply = null;
            Wait(() =>
            {
                lock (last) return replies.TryGetValue(id, out reply);
            }, seconds);
            return reply;
        }

        /// Последнее пришедшее сообщение с таким "t"; null — не приходило.
        public Dictionary<string, object> Last(string topic)
        {
            Dictionary<string, object> message;
            lock (last) return last.TryGetValue(topic, out message) ? message : null;
        }

        /// Сколько сообщений с таким "t" пришло.
        public int Count(string topic)
        {
            int count;
            lock (last) return counts.TryGetValue(topic, out count) ? count : 0;
        }

        /// Ждёт условия не дольше seconds; false — не дождался. Закрытый канал ждать не заставляет.
        /// Условие проверяется по приходу каждого сообщения, а не по таймеру: таймер Windows идёт шагами по
        /// 15 мс, и ответ на команду, приходящий за доли миллисекунды, казался бы в сто раз медленнее.
        public bool Wait(Func<bool> condition, double seconds)
        {
            double until = Native.Now() + seconds;
            while (true)
            {
                if (condition()) return true;
                double left = until - Native.Now();
                if (Closed || left <= 0) return condition();
                arrived.WaitOne((int)Math.Min(left * 1000 + 1, 100));
            }
        }

        public void Dispose()
        {
            if (pipe != null) pipe.Dispose();
        }
    }
}
