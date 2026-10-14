// Окно: временная кнопка «мост» — проверка канала между окном с правами администратора и копией программы без
// них (BridgeCheck.Owner). Уйдёт вместе с проверкой, когда появится окно без прав.
using System.Threading;

namespace CoreMap
{
    partial class MainForm
    {
        volatile bool bridgeChecking;

        void BridgeControls(System.Windows.Forms.Control bar)
        {
            bar.Controls.Add(Widgets.Button("мост", BridgeClick,
                "Проверка моста\nПроверяет, что копия программы без прав администратора может связаться с этим окном: " +
                "на такой связи будет работать новое окно.\n" +
                "• занимает до 15 секунд; на экране ничего не появляется, процессор не нагружается и потолки не трогаются\n" +
                "• итог — строкой в полосе сообщений, подробности — в data\\bridge-check.txt\n" +
                "• кнопка временная"));
        }

        void BridgeClick()
        {
            if (bridgeChecking) return;
            bridgeChecking = true;
            Problem = null;
            note = "Проверка моста идёт, до 15 секунд…";
            // Не в потоке окна: проверка ждёт запуска копии программы и её команд.
            var thread = new Thread(() =>
            {
                try { note = BridgeCheck.Owner(); }
                finally { bridgeChecking = false; }
            }) { IsBackground = true, Name = "проверка моста" };
            thread.SetApartmentState(ApartmentState.STA);  // обращение к оболочке Windows идёт через COM
            thread.Start();
        }
    }
}
