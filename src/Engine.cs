// Движок на мосту: связывает Core с каналом. Что уходит окну — Feed.cs, что от окна исполняется — Commands.cs,
// сама перевозка — Bridge.cs; здесь только их сборка и то, когда слать состояние.
//
// Состояние шлётся после каждого опроса датчиков: поток датчиков взводит событие, пишет в канал поток моста.
// Пока идёт измерение задержек, состояние по опросу не шлётся — как не опрашиваются в это время и вентиляторы:
// сборка и отправка сообщения занимают ядра, между которыми меряются наносекунды. Команду и конец измерения
// окно видит и тогда: состояние уходит следом за каждым ответом и за сообщением о конце.
//
// Пока этим пользуется только скрытая проверка на имитации (BridgeCheck.cs): на настоящем процессоре канал
// протокола никто не открывает, пока нет окна, которому он нужен (DECISIONS.md, п. 25).
using System;
using System.Collections.Generic;
using System.Security.Principal;
using System.Text;

namespace CoreMap
{
    class Engine : BridgeHost, IDisposable
    {
        readonly Core core;
        readonly Feed feed;
        readonly Commands commands;
        public readonly Bridge Bridge;

        /// confirm — как спросить владельца о потолке выше заводского; null — спросить некому, такие команды отклоняются.
        public Engine(Core core, string pipeName, SecurityIdentifier admit, Func<string, bool> confirm)
        {
            this.core = core;
            feed = new Feed(core);
            commands = new Commands(core) { Confirm = confirm };
            Bridge = new Bridge(pipeName, admit, this);
            Bridge.Start();
            core.Fans.Start();  // обороты и режим вентиляторов — часть состояния; без окна их никто не опрашивает
            if (core.Monitor != null) core.Monitor.OnSample += Polled;
        }

        /// После каждого опроса датчиков, в их потоке и после защиты от перегрева (Core.Sampled подписан раньше).
        void Polled(double[] temps)
        {
            if (!(core.Runner.Active is LatencyTest)) Bridge.Pulse();
        }

        public override IEnumerable<string> Greeting() { return feed.Greeting(); }

        public override bool Urgent(string command) { return commands.Urgent(command); }

        public override string Execute(string command, Dictionary<string, object> fields, out string note)
        {
            string refusal = commands.Execute(command, fields, out note);
            feed.Touch();
            return refusal;
        }

        public override IEnumerable<string> Changed() { return feed.Changed(); }

        public override bool State(StringBuilder text) { return feed.State(text); }

        public void Dispose()
        {
            if (core.Monitor != null) core.Monitor.OnSample -= Polled;
            Bridge.Dispose();
        }
    }
}
