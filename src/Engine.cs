// Движок: Core, который живёт в процессе резидента (Tray.cs) ровно тогда, когда он нужен, и канал моста к нему.
//
// Канал слушается всегда, пока работает движок. Core создаётся, когда окно подключилось и назвало себя, и
// останавливается, когда окна нет и измерение не идёт: без окна и измерения резидент не опрашивает датчики и
// не держит драйвер — остаётся значком у часов, каким был. Окно ушло посреди измерения — измерение идёт
// дальше, Core остановится, когда оно кончится.
//
// Что уходит окну — Feed.cs, что от окна исполняется — Commands.cs, перевозка — Bridge.cs. Здесь: жизнь Core и
// то, что раньше делало для него окно, пока было с ним в одном процессе, — перечитывание потолков раз в десять
// секунд, запрет сна на время измерения, запуск опроса вентиляторов.
//
// Потоки. Core создаёт поток чтения моста (Admit), останавливает — поток хозяина (StopIfIdle): он же после
// остановки перечитывает потолки и профили для меню значка, поэтому второго замка вокруг резидента не нужно.
// Поток датчиков сам себя остановить не может: он просит хозяина (site.Idle). Создание и остановка — под
// одним замком.
//
// Движок и прежнее окно. Прежнее окно — отдельный процесс со своим Core; два Core разом недопустимы: второй
// принял бы метку идущего измерения первого за след обрыва и вернул бы потолки посреди замера. Поэтому пока
// Core движка жив, движок держит именованный мьютекс (CoreMutex), и прежнее окно по нему отказывается
// открываться; а пока открыто прежнее окно, движок отказывает окну в знакомстве (site.Refuse).
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Principal;
using System.Text;
using System.Threading;

namespace CoreMap
{
    /// Что движку нужно от того, в чьём процессе он живёт.
    class EngineSite
    {
        public Func<Core> Create;           // создать Core для подключившегося окна
        public Func<string> Refuse;         // почему окно сейчас принять нельзя; null — можно
        public Action Idle;                 // «в моём потоке вызови StopIfIdle и обнови подсказку значка»; ждать не должен
        public Action<Core> Ended;          // Core остановлен (в потоке, который звал StopIfIdle или Dispose): забрать его «было»
        public Func<string, bool> Confirm;  // спросить владельца о потолке выше заводского; null — спросить некому
        public string LogPath;              // журнал моста; null — не вести
        public string MutexName = Engine.CoreMutex;  // мьютекс «Core движка жив»; у скрытой проверки имя своё
    }

    class Engine : BridgeHost, IDisposable
    {
        /// Жив, пока жив Core движка. По нему прежнее окно узнаёт, что открываться нельзя.
        public const string CoreMutex = "Local\\CoreMap.Engine.Core";
        const double RefreshEvery = 10;  // раз во столько секунд потолки перечитываются из процессора: их мог сбросить сон
        const int LogKept = 256 * 1024;  // журнал моста длиннее этого укорачивается вдвое

        readonly EngineSite site;
        readonly object life = new object();
        public readonly Bridge Bridge;
        volatile Core core;
        volatile Feed feed;
        volatile Commands commands;
        Mutex held;
        volatile bool attached;  // окно принято и ещё не ушло
        volatile bool starting;  // Core создаётся
        double refreshedAt;
        bool unreadBefore, wasBusy;
        int polls;

        public Engine(string pipeName, SecurityIdentifier admit, EngineSite site)
        {
            this.site = site;
            Bridge = new Bridge(pipeName, admit, this);
        }

        /// Имя канала движка: одно на сеанс Windows.
        public static string PipeName() { return "CoreMap.Bridge." + System.Diagnostics.Process.GetCurrentProcess().SessionId; }

        public void Start() { Bridge.Start(); }

        /// Core движка сейчас есть: потолками и профилями распоряжается он, а не меню значка.
        public bool Alive { get { return core != null || starting; } }

        /// К движку подключено окно.
        public bool Attached { get { return attached; } }

        /// Какое измерение идёт; null — никакое или Core нет.
        public string Doing
        {
            get
            {
                Core c = core;
                return c == null ? null : c.Doing();
            }
        }


        /// Мьютекс движка существует: в каком-то процессе жив его Core. Спрашивает прежнее окно.
        public static bool CoreAlive()
        {
            Mutex other;
            try
            {
                if (!Mutex.TryOpenExisting(CoreMutex, out other)) return false;
                other.Dispose();
                return true;
            }
            catch (UnauthorizedAccessException) { return true; }  // есть, но открыть не дали: он существует
        }

        // ----- жизнь Core -----

        public override string Admit(string who, out IEnumerable<string> greeting)
        {
            greeting = null;
            lock (life)
            {
                string refusal = site.Refuse == null ? null : site.Refuse();
                if (refusal != null)
                {
                    Log("отказано в знакомстве: " + refusal + "; " + who);
                    return refusal;
                }
                if (core == null) Begin();
                attached = true;
                greeting = feed.Greeting();
            }
            Log("окно подключилось: " + who);
            site.Idle();  // подсказка значка
            return null;
        }

        void Begin()
        {
            starting = true;
            held = new Mutex(false, site.MutexName);
            try
            {
                Core created = site.Create();
                feed = new Feed(created);
                commands = new Commands(created) { Confirm = site.Confirm };
                refreshedAt = Native.Now();
                unreadBefore = wasBusy = false;
                polls = 0;
                created.Fans.Start();  // обороты и режим вентиляторов — часть состояния; без окна их никто не опрашивает
                core = created;
                if (created.Monitor != null) created.Monitor.OnSample += Polled;
            }
            catch (Exception)
            {
                held.Dispose();
                held = null;
                feed = null;
                commands = null;
                throw;
            }
            finally { starting = false; }
        }

        public override void Left(string how)
        {
            attached = false;
            Log("окно отключилось: " + how);
            site.Idle();
        }

        /// Останавливает Core, если окна нет и измерение не идёт. true — остановлен: хозяину пора перечитать
        /// потолки и профили. Зовёт хозяин в своём потоке.
        public bool StopIfIdle()
        {
            lock (life)
            {
                if (core == null || attached || core.Busy) return false;
                End();
                return true;
            }
        }

        void End()
        {
            Core ending = core;
            if (ending.Monitor != null) ending.Monitor.OnSample -= Polled;
            core = null;
            feed = null;
            commands = null;
            ending.Close();
            if (site.Ended != null) site.Ended(ending);
            held.Dispose();
            held = null;
        }

        /// После каждого опроса датчиков, в их потоке и после защиты от перегрева (Core.Sampled подписан раньше).
        void Polled(double[] temps)
        {
            Core c = core;
            if (c == null) return;
            bool busy = c.Busy;
            // Запрет сна Windows считает по потоку: этот поток живёт ровно столько, сколько Core.
            if (busy != wasBusy) Native.KeepAwake(busy);
            wasBusy = busy;
            // Пока идёт измерение задержек, состояние по опросу не шлётся — как не опрашиваются в это время и
            // вентиляторы: сборка и отправка сообщения занимают ядра, между которыми меряются наносекунды.
            // Команду и конец измерения окно видит и тогда: состояние уходит следом за каждым ответом.
            if (!(c.Runner.Active is LatencyTest)) Bridge.Pulse();
            // Окна нет: Core живёт только ради идущего измерения. Кончилось — хозяин остановит; идёт — раз в пять
            // секунд обновит подсказку значка.
            if (!attached && (!busy || ++polls % 10 == 0)) site.Idle();
        }

        // ----- мост -----

        public override bool Urgent(string command)
        {
            Commands c = commands;
            return c != null && c.Urgent(command);
        }

        public override string Execute(string command, Dictionary<string, object> fields, out string note)
        {
            note = null;
            Commands c = commands;
            Feed f = feed;
            if (c == null || f == null) return "движок остановлен: окно отключилось раньше, чем команда дошла";
            string refusal = c.Execute(command, fields, out note);
            f.Touch();
            if (command != "notices.seen")
                Log(command + string.Concat(fields.Where(p => p.Key != "id" && p.Key != "c").Select(p => " " + p.Key + "=" + p.Value)) +
                    (refusal == null ? "" : " — отказ: " + refusal));
            return refusal;
        }

        /// В потоке записи моста, перед каждым состоянием. Заодно — перечитывание потолков: оно нужно, только
        /// пока окно на них смотрит и измерение их не двигает.
        public override IEnumerable<string> Changed()
        {
            Core c = core;
            Feed f = feed;
            if (c == null || f == null) return null;
            if (c.Limits != null && !c.Busy && Native.Now() - refreshedAt >= RefreshEvery)
            {
                string unread = c.Limits.Refresh();
                // Не прочитались — повтор через секунду. Окну об этом говорится, если не прочитались и со второго
                // раза: тогда на плитках стоят прежние значения, а не то, что в процессоре.
                refreshedAt = Native.Now() - (unread == null ? 0 : RefreshEvery - 1);
                f.Unread = unread != null && unreadBefore ? unread : null;
                unreadBefore = unread != null;
            }
            return f.Changed();
        }

        public override bool State(StringBuilder text)
        {
            Feed f = feed;
            return f != null && f.State(text);
        }

        public void Dispose()
        {
            Bridge.Dispose();
            lock (life)
                if (core != null) End();
        }

        // ----- журнал моста -----

        /// Кто подключался и что велел: команды, меняющие процессор и файлы, отказы. Состояние сюда не пишется.
        void Log(string text)
        {
            if (site.LogPath == null) return;
            try
            {
                lock (site)
                {
                    var file = new FileInfo(site.LogPath);
                    if (file.Exists && file.Length > LogKept)
                    {
                        string[] kept = File.ReadAllLines(site.LogPath);
                        File.WriteAllLines(site.LogPath, kept.Skip(kept.Length / 2));
                    }
                    File.AppendAllText(site.LogPath, DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "  " + text + "\r\n");
                }
            }
            catch (Exception e) { Program.LogOnce("журнал моста", e); }
        }
    }
}
