// Мост: канал Windows, по которому окно без прав администратора получает состояние движка и шлёт ему команды.
//
// Здесь только перевозка: кто подключился, целы ли сообщения, в каком порядке они уходят. Что в сообщениях —
// решает хозяин моста (BridgeHost): для движка это Feed.cs и Commands.cs. Упаковка сообщений — Wire.cs, доступ
// к каналу — Access.cs. Числа, на которых стоит устройство, — проба 13.10.2026 (outputs\bridge-probe-2026-10-13).
//
// Правила:
// • Канал один и клиент в каждый момент один. Первым говорит клиент: {"id":0,"c":"hello","proto":1}. Только
//   после первых прочитанных байт Windows отвечает, кто на другом конце. Промолчал GreetWithin секунд — отключён.
// • Клиент → движок: {"id":N,"c":"название", …}. Движок → клиент: ответ {"t":"reply","re":N,"ok":true} либо
//   {"ok":false,"why":"…"}; всё остальное от движка несёт своё "t".
// • Нарушение протокола (не JSON, нет номера, длина вне границ) — запись в журнал и разрыв, без ответа.
// • Клиент, который перестал читать, отключается через StallMs: иначе запись в канал встала бы навсегда.
//
// Потоки:
// • «мост-чтение» ждёт клиента и читает его сообщения. Срочные команды (остановить измерение) исполняет сам:
//   им нельзя стоять в очереди за командой, которая ждёт прошивку вентиляторов двенадцать секунд.
// • «мост-команды» исполняет остальные команды по одной, в порядке прихода.
// • «мост-запись» — единственный, кто пишет в канал: ответы, сообщения по изменению, состояние.
// Потоки датчиков и измерений в канал не пишут никогда — они только взводят событие (Pulse): клиент, который
// не читает, не должен задержать ни опрос, ни измерение. Пока клиента нет, все три потока спят без таймеров.
using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Pipes;
using System.Security.Principal;
using System.Text;
using System.Threading;

namespace CoreMap
{
    /// Хозяин моста: тот, чьё состояние по нему уходит и кто исполняет пришедшие команды.
    abstract class BridgeHost
    {
        /// Сообщения сразу после знакомства: «hello» и всё, что дальше шлётся только по изменению.
        public abstract IEnumerable<string> Greeting();

        /// Команду нельзя ставить в очередь за другими: она исполняется в потоке чтения и должна быть мгновенной.
        public virtual bool Urgent(string command) { return false; }

        /// Исполняет команду. null — сделано; иначе причина отказа. note — что сказать владельцу об исполненном.
        public abstract string Execute(string command, Dictionary<string, object> fields, out string note);

        /// Сообщения о том, что изменилось с прошлого вызова; null — ничего. Зовёт поток записи.
        public virtual IEnumerable<string> Changed() { return null; }

        /// Пишет сообщение «состояние». false — слать сейчас нечего. Зовёт поток записи.
        public virtual bool State(StringBuilder text) { return false; }

        /// Клиент назвал себя; who — что о нём известно Windows. Клиент ушёл; how — как.
        public virtual void Joined(string who) { }
        public virtual void Left(string how) { }
    }

    class Bridge : IDisposable
    {
        public const int Protocol = 1;
        const int StallMs = 2000;      // запись не закончилась за это время: клиент не читает
        const double GreetWithin = 2;  // подключился и столько секунд молчит: отключается

        struct Request
        {
            public int Session, Id;
            public string Command;
            public Dictionary<string, object> Fields;
        }

        readonly string name;
        readonly SecurityIdentifier admit;
        readonly BridgeHost host;
        readonly object gate = new object();  // сам канал: запись в него и его замена
        readonly Queue<Request> inbox = new Queue<Request>();
        readonly Queue<string> outbox = new Queue<string>();
        readonly AutoResetEvent work = new AutoResetEvent(false), wake = new AutoResetEvent(false);
        readonly StringBuilder text = new StringBuilder(4096);
        char[] chars = new char[4096];
        byte[] bytes = new byte[8192];
        NamedPipeServerStream pipe;
        volatile bool done, accepted, spoke, greeted, closing, due;  // spoke — клиент прислал первое сообщение
        volatile int session;
        double acceptedAt;

        /// Канал, как его видит эта сторона: владелец, список доступа, метка.
        public string Security { get; private set; }
        /// Клиент, который подключён и назвал себя: процесс, exe, пользователь, уровень целостности; null — никого.
        public volatile string Client;
        /// Уровень целостности этого клиента (Access.Medium, Access.High); null — неизвестен.
        public volatile string ClientIntegrity;
        public int Sessions { get { return session; } }

        public Bridge(string name, SecurityIdentifier admit, BridgeHost host)
        {
            this.name = name;
            this.admit = admit;
            this.host = host;
        }

        /// Создаёт канал и начинает ждать клиента. Имя занято другой программой — исключение: на чужом канале
        /// мост работать не станет.
        public void Start()
        {
            pipe = Access.Create(name, admit);
            Security = Access.Describe(pipe.SafePipeHandle);
            new Thread(ReadLoop) { IsBackground = true, Name = "мост-чтение" }.Start();
            new Thread(DoLoop) { IsBackground = true, Name = "мост-команды" }.Start();
            new Thread(WriteLoop) { IsBackground = true, Name = "мост-запись" }.Start();
        }

        /// Состояние изменилось или пришло время его слать. Звать можно из любого потока: ничего не ждёт.
        public void Pulse()
        {
            if (!greeted) return;
            due = true;
            wake.Set();
        }

        public void Dispose()
        {
            if (done) return;
            done = true;
            try { pipe.Dispose(); }
            catch (Exception) { }
            work.Set();
            wake.Set();
        }

        // ----- чтение -----

        void ReadLoop()
        {
            while (!done)
            {
                try { pipe.WaitForConnection(); }
                catch (Exception e)
                {
                    if (!done) Program.Log("мост: ожидание клиента оборвалось: " + e);
                    return;
                }
                session++;
                acceptedAt = Native.Now();
                accepted = true;
                wake.Set();  // с этого момента поток записи следит, чтобы клиент не молчал
                string how = "клиент закрыл канал";
                try { Serve(); }
                catch (InvalidDataException e) { how = "нарушение протокола: " + e.Message; Program.Log("мост: " + how + "; клиент: " + (Client ?? Describe())); }
                catch (IOException e) { how = "обрыв: " + Program.Why(e); }
                catch (ObjectDisposedException) { how = "канал закрыт"; }
                catch (InvalidOperationException e) { how = "отключён: " + Program.Why(e); }
                catch (OperationCanceledException) { how = "отключён мостом"; }
                bool known = greeted;
                accepted = spoke = greeted = closing = due = false;
                Client = ClientIntegrity = null;
                lock (outbox) outbox.Clear();
                if (known)
                {
                    try { host.Left(how); }
                    catch (Exception e) { Program.Log("мост, уход клиента: " + e); }
                }
                if (done) return;
                lock (gate)
                {
                    try { pipe.Disconnect(); }
                    catch (Exception)
                    {
                        // Объект канала после оборванного чтения бывает негоден: канал создаётся заново. Между двумя
                        // вызовами имя на миг свободно; занял кто-то другой — мост на этом кончается, это видно в журнале.
                        try
                        {
                            pipe.Dispose();
                            pipe = Access.Create(name, admit);
                        }
                        catch (Exception again)
                        {
                            Program.Log("мост: канал не создан заново, мост остановлен: " + again);
                            done = true;
                            return;
                        }
                    }
                }
            }
        }

        void Serve()
        {
            var frames = new FrameReader();
            int offset, length;
            while (frames.Next(pipe, out offset, out length))
            {
                var request = Parse(Encoding.UTF8.GetString(frames.Buffer, offset, length));
                if (!greeted)
                {
                    spoke = true;
                    if (request.Command != "hello") throw new InvalidDataException("первое сообщение — не hello");
                    object proto;
                    bool spoken = request.Fields.TryGetValue("proto", out proto) && proto is int && (int)proto == Protocol;
                    string who = Describe();
                    lock (outbox)
                    {
                        // Версия протокола не та (после замены exe остался работать прежний движок): клиенту говорится
                        // почему, и только потом канал рвётся — иначе он видел бы просто обрыв.
                        outbox.Enqueue(Reply(request.Id, spoken ? null : "движок говорит на протоколе " + Protocol + ", окно — на другом", null));
                        if (spoken)
                            foreach (string message in host.Greeting()) outbox.Enqueue(message);
                    }
                    if (!spoken) { closing = true; wake.Set(); continue; }
                    Client = who;
                    host.Joined(who);
                    greeted = due = true;
                    wake.Set();
                    continue;
                }
                if (host.Urgent(request.Command))
                {
                    Answer(request);
                    continue;
                }
                lock (inbox) inbox.Enqueue(request);
                work.Set();
            }
        }

        Request Parse(string message)
        {
            object parsed;
            try { parsed = Json.Parse(message); }
            catch (Exception e) { throw new InvalidDataException("не JSON: " + Program.Why(e)); }
            var fields = parsed as Dictionary<string, object>;
            object id, command;
            if (fields == null || !fields.TryGetValue("id", out id) || !(id is int)) throw new InvalidDataException("в сообщении нет номера");
            if (!fields.TryGetValue("c", out command) || !(command is string)) throw new InvalidDataException("в сообщении нет команды");
            return new Request { Session = session, Id = (int)id, Command = (string)command, Fields = fields };
        }

        /// Кто на другом конце: процесс, его exe, пользователь и уровень целостности. Маркер клиента берётся только
        /// на время этого вопроса; клиент подключается с правом «узнать, кто я», действовать от его имени мост не может.
        string Describe()
        {
            uint process = Access.ClientProcess(pipe.SafePipeHandle);
            string who = "процесс " + process + ", " + (Access.ImagePath(process) ?? "exe неизвестен");
            try
            {
                pipe.RunAsClient(() =>
                {
                    using (WindowsIdentity identity = WindowsIdentity.GetCurrent(true))
                    {
                        if (identity == null) return;
                        ClientIntegrity = Access.Integrity(identity.Token);
                        who += ", пользователь " + identity.User.Value + ", целостность " + ClientIntegrity;
                    }
                });
            }
            catch (Exception e) { who += ", пользователь не определён (" + Program.Why(e) + ")"; }
            return who;
        }

        // ----- команды -----

        void DoLoop()
        {
            while (true)
            {
                work.WaitOne();
                if (done) return;
                while (true)
                {
                    Request request;
                    lock (inbox)
                    {
                        if (inbox.Count == 0) break;
                        request = inbox.Dequeue();
                    }
                    // Команда, которую клиент успел послать, исполняется и после его ухода: он её отдал.
                    Answer(request);
                }
            }
        }

        void Answer(Request request)
        {
            string refusal, note = null;
            try { refusal = host.Execute(request.Command, request.Fields, out note); }
            catch (Exception e)
            {
                Program.Log("мост, команда " + request.Command + ": " + e);
                refusal = "ошибка в программе: " + Program.Why(e);
            }
            if (request.Session != session || !greeted) return;  // отвечать уже некому
            lock (outbox) outbox.Enqueue(Reply(request.Id, refusal, note));
            due = true;  // следом за ответом — состояние: окно видит исполненное сразу, а не через полсекунды
            wake.Set();
        }

        static string Reply(int id, string refusal, string note)
        {
            return "{\"t\":\"reply\",\"re\":" + id + (refusal == null ? ",\"ok\":true" : ",\"ok\":false,\"why\":" + Json.Quote(refusal)) +
                (note == null ? "" : ",\"note\":" + Json.Quote(note)) + "}";
        }

        // ----- запись -----

        void WriteLoop()
        {
            while (true)
            {
                // Таймер нужен только пока клиент подключился и ещё не назвал себя.
                wake.WaitOne(accepted && !greeted ? 500 : Timeout.Infinite);
                if (done) return;
                if (!accepted) continue;
                try
                {
                    if (!Flush()) continue;
                    if (closing) { Drop("версия протокола"); continue; }
                    if (!greeted)
                    {
                        if (!spoke && Native.Now() - acceptedAt >= GreetWithin) Drop("клиент молчит " + GreetWithin + " с");
                        continue;
                    }
                    if (!due) continue;
                    due = false;
                    IEnumerable<string> changed = host.Changed();
                    if (changed != null)
                        foreach (string message in changed)
                            if (!Send(message)) break;
                    text.Clear();
                    if (host.State(text)) Send();
                }
                catch (Exception e)
                {
                    Program.LogOnce("мост, запись", e);
                    Drop("ошибка записи");
                }
            }
        }

        /// Отправляет всё, что ждёт в очереди. false — клиент не принял.
        bool Flush()
        {
            while (true)
            {
                string message;
                lock (outbox)
                {
                    if (outbox.Count == 0) return true;
                    message = outbox.Dequeue();
                }
                if (!Send(message)) return false;
            }
        }

        bool Send(string message)
        {
            text.Clear();
            text.Append(message);
            return Send();
        }

        /// Отправляет то, что лежит в text. false — не ушло: клиента нет либо он не читает и отключён.
        bool Send()
        {
            int count = Wire.Pack(text, ref chars, ref bytes);
            lock (gate)
            {
                try
                {
                    IAsyncResult result = pipe.BeginWrite(bytes, 0, count, null, null);
                    bool stalled = !result.AsyncWaitHandle.WaitOne(StallMs);
                    if (stalled)
                    {
                        Program.Log("мост: клиент не читает " + StallMs + " мс, отключён: " + Client);
                        Access.CancelAll(pipe.SafePipeHandle);  // и эта запись, и чтение в потоке чтения кончатся ошибкой
                    }
                    pipe.EndWrite(result);
                    return !stalled;
                }
                catch (Exception e)
                {
                    if (!(e is IOException || e is InvalidOperationException || e is ObjectDisposedException || e is OperationCanceledException)) throw;
                    return false;
                }
            }
        }

        /// Отключает клиента: чтение в потоке чтения обрывается, и тот готовит канал к следующему.
        void Drop(string why)
        {
            if (!accepted) return;
            Program.Log("мост: клиент отключён (" + why + "): " + (Client ?? "не назвался"));
            accepted = false;  // чтобы поток записи не повторял это каждые полсекунды, пока поток чтения разбирается
            try { Access.CancelAll(pipe.SafePipeHandle); }
            catch (Exception) { }
        }
    }
}
