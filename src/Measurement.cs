// Общая основа измерений — всего, что само нагружает ядра или двигает потолки множителей.
//
// Измерение идёт в своём потоке, и в каждый момент — не больше одного: их запускает общий исполнитель (Runner).
// Запуск, остановка, ожидание конца, перехват неожиданных ошибок и порядок завершения у всех измерений одни:
// снять нагрузку → вернуть потолки → вернуть вентиляторы → дописать своё → объявить, что измерение кончилось. Этот порядок
// выполняется на любом пути выхода: обычный конец, кнопка «стоп», защита от перегрева, ошибка в потоке,
// закрытие окна. Поэтому защите и закрытию окна достаточно исполнителя — перечислять измерения им не нужно.
using System;
using System.Threading;

namespace CoreMap
{
    /// Исполнитель измерений: запускает их по одному и знает, какое идёт сейчас.
    class Runner
    {
        readonly object gate = new object();
        volatile Measurement active;
        Thread thread;

        public readonly LoadManager Load;
        public readonly LimitHold Hold;  // исходные потолки на время измерения; null — потолки недоступны
        public readonly Fans Fans;       // вентиляторы: максимум на время измерения и обороты для результата; null — без них

        public Runner(LoadManager load, LimitHold hold, Fans fans = null)
        {
            Load = load;
            Hold = hold;
            Fans = fans;
        }

        /// Измерение, которое идёт сейчас; null — никакое.
        public Measurement Active { get { return active; } }

        public bool Busy { get { return active != null; } }

        /// Занимает исполнителя под измерение и запускает его поток. false — идёт другое измерение.
        public bool Start(Measurement measurement, Action body)
        {
            lock (gate)
            {
                if (active != null) return false;
                active = measurement;
                // До запуска потока: просьба остановиться, пришедшая сразу после запуска, не должна потеряться.
                measurement.Prepare();
                thread = new Thread(() => measurement.Execute(body)) { IsBackground = true, Name = measurement.GetType().Name };
                thread.Start();
            }
            return true;
        }

        /// Поток измерения сделал всё, что должен был: исполнитель свободен.
        public void Release(Measurement measurement)
        {
            lock (gate)
                if (active == measurement) active = null;
        }

        /// Просит идущее измерение остановиться. Заканчивает оно само, в своём потоке: снимает нагрузку,
        /// возвращает потолки.
        public void Stop()
        {
            Measurement running = active;
            if (running != null) running.Stop();
        }

        /// Останавливает идущее измерение и ждёт, пока его поток закончит, не дольше seconds. false — не дождался.
        public bool Close(double seconds)
        {
            Thread running;
            lock (gate) running = active == null ? null : thread;
            Stop();
            return running == null || running.Join(TimeSpan.FromSeconds(seconds));
        }
    }

    abstract class Measurement
    {
        readonly Runner runner;
        readonly string failure;  // чем начинается сообщение о неожиданной ошибке; null — сообщение без начала
        readonly ManualResetEvent stop = new ManualResetEvent(false);
        volatile bool stopping;

        public string Error;     // почему последнее измерение не доведено до конца; null — доведено или его не было
        public string Finished;  // когда оно закончилось, «ДД.ММ ЧЧ:ММ»; null — идёт или его не было
        public int Version;      // растёт, когда меняется то, что по этому измерению показывает окно

        protected Measurement(Runner runner, string failure)
        {
            this.runner = runner;
            this.failure = failure;
        }

        public bool Running { get { return runner.Active == this; } }

        protected LoadManager Load { get { return runner.Load; } }

        /// Исходные потолки. Измерение, которое двигает потолки, сначала берёт их отсюда (Take); вернёт их основа.
        protected LimitHold Hold { get { return runner.Hold; } }

        /// Измерение греет процессор: на его время вентиляторы переводятся на максимум, если владелец так выбрал.
        /// Измерение, которое не греет (задержки), вместо этого приостанавливает опрос оборотов.
        protected virtual bool Heats { get { return true; } }

        /// Обороты вентиляторов за последние seconds секунд — в результат измерения; null — неизвестны.
        protected FanStat Air(double seconds) { return runner.Fans == null ? null : runner.Fans.Typical(seconds); }

        /// Измерение просят остановить.
        protected bool Stopped { get { return stopping; } }

        public void Stop()
        {
            stopping = true;
            stop.Set();
        }

        /// Ждёт указанное время; true — измерение просят остановить.
        protected bool Wait(double seconds) { return stop.WaitOne(TimeSpan.FromSeconds(seconds)); }

        /// Запускает измерение в своём потоке. false — не запущено: идёт другое измерение.
        protected bool Launch(Action body) { return runner.Start(this, body); }

        /// Своё завершение измерения, на любом пути выхода: нагрузка уже снята, потолки возвращены.
        protected virtual void Finish() { }

        /// То, что показывает окно, изменилось.
        protected virtual void Changed() { Version++; }

        internal void Prepare()
        {
            stopping = false;
            stop.Reset();
            Error = Finished = null;
        }

        internal void Execute(Action body)
        {
            try
            {
                Load.StopAll();  // ручная нагрузка измерению мешает
                if (runner.Fans != null)
                {
                    // Отказ вентиляторов измерение не останавливает: оно идёт при автоматических оборотах.
                    if (Heats) runner.Fans.Engage(Wait);
                    else runner.Fans.Hush(true);
                }
                body();
            }
            catch (Exception e)
            {
                // Без перехвата ошибка в потоке измерения закрывала программу без сообщения.
                Program.Log(GetType().Name + ": " + e);
                Error = failure == null ? e.Message : failure + ": " + e.Message;
            }
            finally
            {
                Safely(Load.StopAll);
                Safely(() =>
                {
                    string refusal = Hold == null ? null : Hold.Release();
                    if (refusal != null) Error = "прежний потолок не возвращён: " + refusal;
                });
                // Вентиляторы — после потолков: их отказ или зависший ответ прошивки возврат потолков не задержит.
                Safely(() =>
                {
                    if (runner.Fans == null) return;
                    if (Heats) runner.Fans.Release();
                    else runner.Fans.Hush(false);
                });
                Safely(Finish);
                Finished = DateTime.Now.ToString("dd.MM HH:mm");
                // Счётчик растёт после того, как измерение перестало числиться идущим: окно перерисовывается
                // по счётчику и должно увидеть уже конечное состояние.
                runner.Release(this);
                Changed();
            }
        }

        /// Шаг завершения: его ошибка не должна отменить следующие шаги.
        void Safely(Action step)
        {
            try { step(); }
            catch (Exception e)
            {
                Program.Log(GetType().Name + ", завершение: " + e);
                if (Error == null) Error = failure == null ? e.Message : failure + ": " + e.Message;
            }
        }
    }
}
