// Режим вопроса (кнопка «?»): пока он включён, подсказки-описания всплывают при наведении, а курсор — со знаком
// вопроса. Первый щелчок в окне или Esc выключает режим; до элемента под курсором этот щелчок не доходит.
// Без режима вопроса описания не всплывают: при простом наведении показываются только числа (Theme.Readout).
using System.Collections.Generic;
using System.Windows.Forms;

namespace CoreMap
{
    class HelpMode : IMessageFilter
    {
        readonly Form form;
        // Курсоры элементов, какими они были до режима вопроса.
        readonly Dictionary<Control, Cursor> cursors = new Dictionary<Control, Cursor>();

        public Button Button;  // кнопка «?»: подсвечена, пока режим включён

        public HelpMode(Form form)
        {
            this.form = form;
            Application.AddMessageFilter(this);
            form.FormClosed += (s, e) => Application.RemoveMessageFilter(this);
        }

        /// Включает и выключает режим вопроса: в нём подсказки всплывают при наведении, курсор — со знаком вопроса.
        public void Set(bool on)
        {
            if (Theme.Help == on) return;
            Theme.Help = on;
            Theme.Tips.Active = on;
            Button.ForeColor = on ? Theme.Accent : Theme.Text;
            Button.BackColor = on ? Theme.Cell : Theme.Button;
            if (on) HelpCursor(form);
            else
            {
                foreach (KeyValuePair<Control, Cursor> c in cursors) c.Key.Cursor = c.Value;
                cursors.Clear();
            }
        }

        void HelpCursor(Control control)
        {
            cursors[control] = control.Cursor;
            control.Cursor = Cursors.Help;
            foreach (Control child in control.Controls) HelpCursor(child);
        }

        /// В режиме вопроса первый щелчок в окне или Esc выключает режим; до элемента под курсором щелчок не доходит.
        public bool PreFilterMessage(ref Message m)
        {
            const int KeyDown = 0x100, LeftDown = 0x201, LeftDouble = 0x203, RightDown = 0x204, MiddleDown = 0x207, Escape = 0x1B;
            if (!Theme.Help) return false;
            bool click = m.Msg == LeftDown || m.Msg == LeftDouble || m.Msg == RightDown || m.Msg == MiddleDown;
            if (!click && !(m.Msg == KeyDown && (int)m.WParam == Escape)) return false;
            Control target = Control.FromChildHandle(m.HWnd);
            if (target == null || target.FindForm() != form) return false;
            Set(false);
            return true;
        }
    }
}
