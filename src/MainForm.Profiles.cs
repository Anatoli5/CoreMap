// Главное окно, колонка потолков: профили. «Было» и «сейчас» — см. Core; профиль запуска — Profile.cs.
using System;
using System.Drawing;
using System.Linq;
using System.Windows.Forms;

namespace CoreMap
{
    partial class MainForm
    {
        Label profileLabel, profileStatus;
        TextBox profileName;
        Button profileList, profileSave, profileAccept, profileRevert, profileForget;
        FlowLayoutPanel profileRow;  // ряд кнопок: в узкой колонке переносится

        void ProfileSection(FlowLayoutPanel panel)
        {
            Label first = Heading("Профили");
            first.Margin = new Padding(0, 0, 0, Theme.Px(6));
            panel.Controls.Add(first);
            // Поле названия и кнопка списка вместо системного поля-списка: у того стрелка чёрная на тёмном фоне.
            FlowLayoutPanel field = Widgets.Row();
            field.Margin = new Padding(0, 0, 0, Theme.Px(6));
            field.Controls.Add(profileLabel = Widgets.Label("Профиль", Theme.Dim, 13));
            profileName = new TextBox
            {
                BackColor = Theme.Button, ForeColor = Theme.Text, BorderStyle = BorderStyle.FixedSingle, Font = Theme.Font(13),
                Margin = new Padding(0, 0, Theme.Px(4), 0),
            };
            Theme.Tips.SetToolTip(profileName, "Название профиля\nПод этим названием «сохранить» запишет то, что стоит сейчас.\n" +
                "• существующее название — профиль перезаписывается; новое — появляется новый профиль\n" +
                "• открыть сохранённый профиль — кнопка ▾ справа");
            profileName.Text = core.Profile.Exists ? core.Profile.Name : "";
            field.Controls.Add(profileName);
            field.Controls.Add(profileList = Widgets.Button("▾", ProfileMenu,
                "Открыть профиль\nЩелчок по названию ставит потолки этого профиля. То, что было, при этом запоминается.\n" +
                "• разница открытого профиля с тем, что было, видна на плитках и в списке блоков: +N и −N\n" +
                "• «заводские» — потолки, с которыми процессор выпущен\n" +
                "• оставить открытый профиль — кнопка «принять»; вернуться к тому, что было, — «откатить»"));
            profileList.Margin = Padding.Empty;
            foreach (Control c in field.Controls) c.Anchor = AnchorStyles.Left;
            panel.Controls.Add(field);

            FlowLayoutPanel row = Widgets.Row();
            row.Margin = Padding.Empty;
            row.WrapContents = true;  // в узкой панели кнопки переносятся на вторую строку
            profileRow = row;
            row.Controls.Add(profileSave = Widgets.Button("сохранить", () => Report(core.SaveProfile(profileName.Text)),
                "Сохранить профиль\nЗаписывает то, что стоит сейчас, в профиль с названием из поля выше и принимает его.\n" +
                "• существующее название — профиль перезаписывается; новое название — новый профиль\n" +
                "• сохранённый профиль ставится заново при запуске программы\n" +
                "• без сохранения любые изменения потолков пропадают при перезагрузке\n" +
                "• профили хранятся в файле рядом с программой, в прошивку ноутбука ничего не пишется\n" +
                "• если после этого был сбой, профиль с потолками выше заводских отключается сам"));
            row.Controls.Add(profileAccept = Widgets.Button("принять", () => Report(core.Accept(profileName.Text)),
                "Принять то, что стоит сейчас\nТекущие потолки становятся точкой отсчёта: разница обнуляется, «откатить» будет возвращать к ним.\n" +
                "• если стоит сохранённый профиль — он же будет ставиться при запуске программы\n" +
                "• если стоят заводские — при запуске программы ничего ставиться не будет\n" +
                "• несохранённые потолки тоже можно принять, но при перезагрузке они пропадут — для этого «сохранить»\n" +
                "• сами профили не меняются"));
            row.Controls.Add(profileRevert = Widgets.Button("откатить", () => Report(core.Revert()),
                "Откатить к тому, что было\nВозвращает потолки, которые были: прочитанные из процессора при запуске программы " +
                "или принятые последними.\n" +
                "• всё, что менялось после этого — профилем из списка, ползунком, вручную, — отменяется\n" +
                "• после перезагрузки компьютера «было» — заводские потолки, и откат снимает профиль, поставленный при запуске\n" +
                "• сохранённые профили при этом не меняются"));
            row.Controls.Add(profileForget = Widgets.Button("удалить", () => Report(core.DeleteProfile(profileName.Text)),
                "Удалить профиль\nУдаляет профиль с названием из поля выше.\n" +
                "• если он ставился при запуске программы — больше ничего ставиться не будет\n" +
                "• потолки, которые стоят сейчас, остаются до перезагрузки"));
            foreach (Control c in row.Controls) c.Margin = new Padding(0, 0, Theme.Px(4), Theme.Px(4));
            panel.Controls.Add(row);
            panel.Controls.Add(profileStatus = Widgets.Label("", Theme.Dim, 11));
            profileStatus.Margin = new Padding(0, Theme.Px(6), 0, 0);
        }

        void SizeProfiles(int side)
        {
            profileStatus.MaximumSize = new Size(side, 0);
            profileName.Width = Math.Max(Theme.Px(60), side - profileLabel.PreferredSize.Width - profileLabel.Margin.Horizontal
                - profileName.Margin.Horizontal - profileList.PreferredSize.Width);
            profileRow.MaximumSize = new Size(side, 0);
        }

        void UpdateProfiles(bool busy)
        {
            Profile profile = core.Profile;
            // Во время измерения потолки временные: их меняет само измерение, сохранять и принимать их нельзя.
            bool attention;
            string status = ProfileStatus(busy, out attention);
            profileSave.Enabled = profileName.Text.Trim() != "" && !busy;
            profileAccept.Enabled = core.CanAccept(profileName.Text);
            profileForget.Enabled = core.Profiles.Get(profileName.Text) != null;
            profileRevert.Enabled = attention && !busy;
            if (profileStatus.Text != status) profileStatus.Text = status;
            string details = profile.Details() ?? "";
            if (Theme.Readout.GetToolTip(profileStatus) != details) Theme.Readout.SetToolTip(profileStatus, details);
            profileStatus.ForeColor = profile.Disabled ? Theme.Bad : attention ? Theme.Accent : Theme.Dim;
            die.LimitBase = core.Was;
        }

        /// Название набора для надписей: профиль в кавычках, заводские или несохранённый.
        static string Titled(string name) { return name == null ? "не сохранено" : name == "" ? "заводские" : "«" + name + "»"; }

        /// Список под кнопкой ▾: заводские потолки и сохранённые профили. Щелчок открывает набор — ставит его
        /// потолки; «было» при этом прежнее. Пометки словами говорят, что было, что стоит сейчас и что ставится
        /// при запуске программы.
        void ProfileMenu()
        {
            var menu = new ContextMenuStrip { ShowImageMargin = false, ShowCheckMargin = false, Font = Theme.Font(13) };
            Profile profile = core.Profile;
            string standing = core.Standing(profileName.Text), was = core.Named(core.Was, profileName.Text);
            // "" — заводские потолки: у них нет названия, и в поле названия они ничего не вписывают.
            foreach (string name in new[] { "" }.Concat(core.Profiles.Names))
            {
                string picked = name;
                bool startup = picked != "" && profile.Exists && profile.Name == picked;
                string text = (picked == "" ? "заводские" : picked) + (was == picked ? "  · было" : "") +
                    (standing == picked ? "  · стоит сейчас" : "") +
                    (startup ? "  · при запуске" + (profile.Disabled ? " (отключён после сбоя)" : "") : "");
                var item = new ToolStripMenuItem(text) { Enabled = !core.Busy };
                item.Click += (s, e) => OpenProfile(picked);
                menu.Items.Add(item);
                if (picked == "" && core.Profiles.Names.Length > 0) menu.Items.Add(new ToolStripSeparator());
            }
            menu.Closed += (s, e) => BeginInvoke((Action)menu.Dispose);
            menu.Show(profileList, new Point(0, profileList.Height));
        }

        /// Открывает набор из списка: ставит его потолки, «было» остаётся прежним. "" — заводские потолки.
        public void OpenProfile(string name)
        {
            string refusal = core.OpenProfile(name);
            if (refusal == null) { profileName.Text = name; fitReport = null; }
            Report(refusal);
        }

        /// Строка состояния профилей: что было (и что ставится при запуске) и что стоит в процессоре сейчас.
        /// attention — сейчас стоит не то, что было.
        string ProfileStatus(bool busy, out bool attention)
        {
            attention = false;
            // Всегда две короткие строки: высота надписи не меняется, и окно под ней не прыгает.
            string text = "Было — " + Titled(core.Named(core.Was, profileName.Text)) + " · " + core.Profile.Describe() + "\n";
            if (busy) return text + "Сейчас — измерение: потолки временные";
            int differ = Delta().Count(d => d != 0);
            if (differ == 0) return text + "Сейчас — то же";
            attention = true;
            return text + "Сейчас — " + Titled(core.Standing(profileName.Text)) + ": разница у " + differ + " из " + blocks.Length + " блоков";
        }

        /// На сколько ступеней потолок каждого блока, стоящий сейчас, отличается от того, что было; 0 — не отличается или неизвестен.
        int[] Delta()
        {
            int[] now = core.Limits.Current, baseline = core.Was;
            return blocks.Select(b => now[b.First] <= 0 || baseline[b.First] <= 0 ? 0 : now[b.First] - baseline[b.First]).ToArray();
        }
    }
}
