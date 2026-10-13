// Главное окно, колонка потолков: профили. База и то, что стоит в процессоре сейчас, — см. Profile.cs и Core.
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
        Button profileList, profileSave, profilePin, baseRestore, profileForget;
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
                "Открыть профиль\nЩелчок по названию ставит потолки этого профиля. База при этом не меняется.\n" +
                "• разница открытого профиля с базой видна на плитках и в списке блоков: +N и −N\n" +
                "• «заводские» — потолки, с которыми процессор выпущен\n" +
                "• чтобы открытый профиль ставился при запуске программы — кнопка «сделать базой»\n" +
                "• вернуться — кнопка «вернуть базу»"));
            profileList.Margin = Padding.Empty;
            foreach (Control c in field.Controls) c.Anchor = AnchorStyles.Left;
            panel.Controls.Add(field);

            FlowLayoutPanel row = Widgets.Row();
            row.Margin = Padding.Empty;
            row.WrapContents = true;  // в узкой панели кнопки переносятся на вторую строку
            profileRow = row;
            row.Controls.Add(profileSave = Widgets.Button("сохранить", () => Report(core.SaveProfile(profileName.Text)),
                "Сохранить профиль\nЗаписывает то, что стоит сейчас, в профиль с названием из поля выше и делает его базой.\n" +
                "• название базы — база перезаписывается; новое название — новый профиль\n" +
                "• без сохранения любые изменения потолков пропадают при перезагрузке\n" +
                "• профили хранятся в файле рядом с программой, в прошивку ноутбука ничего не пишется\n" +
                "• база ставится заново при запуске программы\n" +
                "• если после этого был сбой, профиль с потолками выше заводских отключается сам"));
            row.Controls.Add(profilePin = Widgets.Button("сделать базой", () => Report(core.PinStanding(profileName.Text)),
                "Сделать базой то, что стоит сейчас\nОткрытый профиль становится базой: он ставится при запуске программы, " +
                "от него считается разница потолков.\n" +
                "• сами профили не меняются\n" +
                "• если открыты заводские — при запуске программы ничего ставиться не будет\n" +
                "• если потолки менялись после открытия — сначала «сохранить»"));
            row.Controls.Add(baseRestore = Widgets.Button("вернуть базу", () => Report(core.RestoreBase()),
                "Вернуть базу\nСтавит потолки базы: всё, что менялось ползунком, вручную или открытием другого профиля, отменяется.\n" +
                "• сохранённые профили при этом не меняются"));
            row.Controls.Add(profileForget = Widgets.Button("удалить", () => Report(core.DeleteProfile(profileName.Text)),
                "Удалить профиль\nУдаляет профиль с названием из поля выше.\n" +
                "• если он база — базой становятся заводские потолки, при запуске программы ничего ставиться не будет\n" +
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
            // Во время измерения потолки временные: их меняет само измерение, сохранять их и менять базу нельзя.
            bool attention;
            string status = ProfileStatus(busy, out attention);
            profileSave.Enabled = profileName.Text.Trim() != "" && !busy;
            profilePin.Enabled = core.CanPin(profileName.Text);
            profileForget.Enabled = core.Profiles.Get(profileName.Text) != null;
            baseRestore.Enabled = attention && !busy;
            if (profileStatus.Text != status) profileStatus.Text = status;
            string details = profile.Details() ?? "";
            if (Theme.Readout.GetToolTip(profileStatus) != details) Theme.Readout.SetToolTip(profileStatus, details);
            profileStatus.ForeColor = profile.Disabled ? Theme.Bad : attention ? Theme.Accent : Theme.Dim;
            die.LimitBase = core.BaseLimits;
        }

        /// Список под кнопкой ▾: заводские потолки и сохранённые профили. Щелчок открывает набор — ставит его
        /// потолки; база при этом прежняя. Пометки словами говорят, что база и что стоит сейчас.
        void ProfileMenu()
        {
            var menu = new ContextMenuStrip { ShowImageMargin = false, ShowCheckMargin = false, Font = Theme.Font(13) };
            Profile profile = core.Profile;
            string standing = core.Standing(profileName.Text);
            // "" — заводские потолки: у них нет названия, и в поле названия они ничего не вписывают.
            foreach (string name in new[] { "" }.Concat(core.Profiles.Names))
            {
                string picked = name;
                bool isBase = picked == "" ? !profile.Exists : profile.Exists && profile.Name == picked;
                string text = (picked == "" ? "заводские" : picked) + (isBase ? "  · база" : "") +
                    (isBase && profile.Disabled ? " (отключена после сбоя)" : "") + (standing == picked ? "  · стоит сейчас" : "");
                var item = new ToolStripMenuItem(text) { Enabled = !core.Busy };
                item.Click += (s, e) =>
                {
                    string refusal = core.OpenProfile(picked);
                    if (refusal == null) { profileName.Text = picked; fitReport = null; }
                    Report(refusal);
                };
                menu.Items.Add(item);
                if (picked == "" && core.Profiles.Names.Length > 0) menu.Items.Add(new ToolStripSeparator());
            }
            menu.Closed += (s, e) => BeginInvoke((Action)menu.Dispose);
            menu.Show(profileList, new Point(0, profileList.Height));
        }

        /// Строка состояния профилей: что база и что стоит в процессоре сейчас. attention — стоит не база.
        string ProfileStatus(bool busy, out bool attention)
        {
            Profile profile = core.Profile;
            attention = false;
            // Всегда две короткие строки: высота надписи не меняется, и окно под ней не прыгает.
            string text = profile.Describe() + "\n";
            if (busy) return text + "Сейчас — измерение: потолки временные";
            int differ = Delta().Count(d => d != 0);
            if (differ == 0) return text + "Сейчас — база";
            attention = true;
            string standing = core.Standing(profileName.Text);
            return text + "Сейчас — " + (standing == null ? "не сохранено" : standing == "" ? "заводские" : "«" + standing + "»") +
                ": разница с базой у " + differ + " из " + blocks.Length + " блоков";
        }

        /// На сколько ступеней потолок каждого блока, стоящий сейчас, отличается от базы; 0 — не отличается или неизвестен.
        int[] Delta()
        {
            int[] now = core.Limits.Current, baseline = core.BaseLimits;
            return blocks.Select(b => now[b.First] <= 0 || baseline[b.First] <= 0 ? 0 : now[b.First] - baseline[b.First]).ToArray();
        }
    }
}
