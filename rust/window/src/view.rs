// Что окно показывает: из сообщений движка (или программы входа) и сводки статистики — строки и цвета для
// ui/app.slint; из щелчков — команды. Правила показа плиток — те же, что у прежнего окна (DieView в src/Ui.cs).
//
// Плитка — строка модели. Пришло состояние — для каждой плитки считается новая строка, и заменяется только та, что
// отличается от показанной: Slint перерисовывает только её. Панели профилей и статистики пересчитываются только от
// того, что они показывают (профили, потолки, надстройка автомодуля, выбор; сводка статистики, выбранный блок), и
// свойства окна ставятся, только если значение другое.
//
// Жесты (владелец 15.10: «по две функции на одно действие… так не делается»): щелчок по плитке — добавить блок в
// статистику или убрать его оттуда (сравнить можно любые блоки, хоть все, владелец 15.10: «почему только 4 ядра можно
// выбирать»), и только; правая кнопка — меню потолка. Нагрузки у окна нет.
//
// Профили (владелец 15.10, DECISIONS #38): текущее состояние — то, что стоит, — и профили — разные вещи. Щелчок по
// строке списка только выбирает профиль: окно показывает разницу выбранного с текущим (строкой панели и на плитках) и
// ничего не шлёт. «Применить» ставит выбранный, «сохранить» пишет в него текущее, «загружать при старте Windows» —
// отдельный выбор (команда profile.startup), видимый только знаком в списке.
//
// Настройки автомодуля (DECISIONS #36, бриф T) — выбор программы у часов: окно показывает тему settings и шлёт изменения
// командой auto.set; ползунок (цель или аварийная) — одной командой, когда его отпустили. Пока его тянут и пока
// программа не подтвердила новое число, ручка и линия стоят на числе под рукой (`limit_held`, `emergency_held`), а не
// прыгают назад. Журнал срезов — свой выключатель той же программы (владелец 16.10): «запись» в шапке статистики,
// команда log.set.
//
// Ось x — время (бриф W с поправкой 16.10, timeline.rs): день журнала поток статистики читает по просьбе окна
// (`Order::Day`) — блоки и ряды раскладки; окно держит показанный отрезок, колесо и перетаскивание меняют его, полосы
// пересчитываются из дня в памяти. События (events.rs) — списком под графиком; щелчок по строке — ось x время, день
// события, момент посередине.
// Что окно помнит о себе (сравниваемые блоки, подсветка, вид графика, размер и место) — его собственный файл
// (src/saved.rs).
#[path = "view_navigation.rs"]
mod navigation;
#[path = "view_ranges.rs"]
mod ranges;

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::{Arc, mpsc};

use serde_json::{Map, Value};
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::chart::{self, Curves, Scale, StatsShown};
use crate::die::{self, Geometry};
use crate::events::{self, Event as Happened};
use crate::model::Die;
use crate::panels::{self, ProfileInput, ProfileShown};
use crate::pipe::{Event, Link};
use crate::plot::Range;
use crate::saved::{self, Remembered};
use crate::store::{DayOrder, Digest, Order};
use crate::timeline::{self, Act, Day, Layout, Quantity, TimeInput, TimeShown, Under, Unit};
use crate::wire::{AutoSettings, Hello, Limits, Message, Profiles, State};
use crate::{
    AppWindow, CellData, LineRow, LinkData, MenuEntry, SetRow, Tick, TileData, TimeDot, TimeMark, TimeRef, TimeStrip,
};

const TEXT: [u8; 3] = [0xe6, 0xe8, 0xec];
const ACCENT: [u8; 3] = [0xff, 0xd1, 0x66];
const BAD: [u8; 3] = [0xff, 0x5d, 0x5d];
const CELL: [u8; 3] = [0x2a, 0x2e, 0x36];
/// Полоска плитки, пока ▲ наибольшей ещё нет.
const NO_PEAK: [u8; 3] = [0x35, 0x3a, 0x44];
/// Строк в списке событий не больше: каждая — несколько элементов Slint, а событий за 30 дней — сотни; старшие видны
/// отметками на графиках.
const EVENT_ROWS: usize = 500;
/// Ширина показа вокруг события по щелчку, когда узкого показа того же дня не было: 10 мин.
const CENTRE_SPAN: i64 = 600_000;

/// Прочитанный день — ответ на эту просьбу: тот же день, те же блоки по порядку и те же ряды.
fn answers(asked: &DayOrder, day: &Day) -> bool {
    asked.date == day.date
        && asked.quantities == day.quantities
        && asked.blocks.iter().eq(day.blocks.iter().map(|b| &b.name))
}

/// Блоки показа для timeline.rs: подпись и цвет.
fn shown_blocks<'a>(labels: &'a [String], colours: &[u32]) -> Vec<timeline::Shown<'a>> {
    labels
        .iter()
        .zip(colours)
        .map(|(label, &color)| timeline::Shown { label, color })
        .collect()
}

fn tick_rows(ticks: &[(f32, String)]) -> Vec<Tick> {
    ticks
        .iter()
        .map(|(f, t)| Tick {
            f: *f,
            text: t.as_str().into(),
        })
        .collect()
}

/// Событие одной строкой для подсказки графика по времени: время, блок, изменение, что и почему; подробности — второй.
fn event_line(e: &Happened) -> String {
    let mut line = format!(
        "{} {}{}: {}",
        e.time,
        e.block.as_deref().map(|b| format!("{b} ")).unwrap_or_default(),
        e.change,
        e.why
    );
    if !e.details.is_empty() {
        line.push_str("\n    ");
        line.push_str(&e.details);
    }
    line
}

fn rgb([r, g, b]: [u8; 3]) -> Color {
    Color::from_rgb_u8(r, g, b)
}

/// Доля 0..1 яркости канала → байт, с отбрасыванием дробной части, как `(int)(v * 255)` в Theme.Heat.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=1 first, so v * 255 fits a byte"
)]
fn channel_byte(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0) as u8
}

/// 0..1 → цвет от холодного синего к красному (Theme.Heat: оттенок 215° → 0°, светлота 0.28 → 0.38, насыщенность 0.65).
fn heat(k: f64) -> Color {
    let k = if k.is_nan() { 0.0 } else { k.clamp(0.0, 1.0) };
    let hue = (215.0 - 215.0 * k) / 360.0;
    let light = 0.28 + 0.10 * k;
    let sat = 0.65;
    let upper = if light < 0.5 {
        light * (1.0 + sat)
    } else {
        light + sat - light * sat
    };
    let lower = 2.0 * light - upper;
    let channel = |at: f64| {
        let at = (at % 1.0 + 1.0) % 1.0;
        channel_byte(if at < 1.0 / 6.0 {
            lower + (upper - lower) * 6.0 * at
        } else if at < 0.5 {
            upper
        } else if at < 2.0 / 3.0 {
            lower + (upper - lower) * (2.0 / 3.0 - at) * 6.0
        } else {
            lower
        })
    };
    Color::from_rgb_u8(channel(hue + 1.0 / 3.0), channel(hue), channel(hue - 1.0 / 3.0))
}

/// Цвет плитки по температуре: 40 °C — синий, 100 °C — красный (`DieView`).
fn heat_of(temp: f64) -> Color {
    heat((temp - 40.0) / 60.0)
}

/// Номер для Slint (его целые — i32). Движок шлёт номера блоков и ступени множителя — десятки; что не помещается,
/// становится -1 и не совпадёт ни с одной плиткой или пунктом.
fn slint_int(v: impl TryInto<i32>) -> i32 {
    v.try_into().unwrap_or(-1)
}

/// Наибольшее по ядрам блока; None — ни одного нет. Только для показа (DieView.Highest).
fn highest(values: &[Option<f64>], members: &[usize]) -> Option<f64> {
    members
        .iter()
        .filter_map(|&i| values.get(i).copied().flatten())
        .fold(None, |top, v| Some(top.map_or(v, |t: f64| t.max(v))))
}

/// Счёт для скрытой проверки: сколько показанного изменило последнее состояние.
#[derive(Default, Clone, Copy, Debug)]
pub struct Changes {
    pub states: u32,
    pub last: u32,
}

/// Команда, на которую ждём ответ: что сказать при отказе и что сделать при успехе.
#[derive(Debug)]
struct Pending {
    failure: &'static str,
    /// «Сохранить как»: название, которое станет выбранным, когда движок ответит «сделано».
    saved_as: Option<String>,
    /// Число ползунка отсечки в этой команде: отказ — ручка возвращается к выбору программы.
    held: bool,
}

impl Pending {
    fn failing(failure: &'static str) -> Self {
        Self {
            failure,
            saved_as: None,
            held: false,
        }
    }
}

#[expect(
    missing_debug_implementations,
    reason = "holds Slint handles that have no Debug; nothing prints a View"
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts of the window (link up, highlight mode, axes swapped, slider held), not one state machine"
)]
pub struct View {
    ui: slint::Weak<AppWindow>,
    link: Arc<Link>,
    pub hello: Option<Hello>,
    pub limits: Option<Limits>,
    pub state: Option<State>,
    pub profiles: Option<Profiles>,
    pub digest: Option<Arc<Digest>>,
    geometry: Geometry,
    tiles: Rc<VecModel<TileData>>,
    sets: Rc<VecModel<SetRow>>,
    /// Блоки в статистике (номера) по порядку добавления; цвет — у блока (`chart::color_of`).
    pub compared: Vec<usize>,
    /// Вид графика (src/chart.rs): эффективность, над кристаллом или абсолютная; линии, оси местами.
    pub scale: Scale,
    pub curves: Curves,
    /// Линия наибольших (переключатель «макс»).
    pub max: bool,
    /// Кривые модели нагрева (переключатель «модель»).
    pub model: bool,
    pub swap: bool,
    /// Ось x — время (иначе множитель).
    pub timeline: bool,
    /// Раскладка по времени и величина раскладки «блоки».
    pub layout: Layout,
    pub quantity: Quantity,
    pub selected: Vec<Quantity>,
    ranges: BTreeMap<String, Range>,
    range_unit: Unit,
    pub normalized: bool,
    pub time_follow: bool,
    range_parameter: Quantity,
    /// Список событий: все блоки или выбранные.
    pub events_all: bool,
    /// Что список событий показывает: события (указатель сводки), выбранные блоки, переключатель — не пересобирать
    /// тот же; номера показанных событий по строкам.
    events_key: Option<(usize, Vec<usize>, bool)>,
    listed: Vec<usize>,
    /// Событие, которое показать посередине, когда его день прочитан: день и время, мс.
    centre: Option<(String, i64)>,
    /// Выбранный день («ГГГГ-ММ-ДД»); None — самый новый.
    pub day: Option<String>,
    /// Показанный отрезок дня, мс; None — весь день.
    pub span: Option<(i64, i64)>,
    /// День, который поток статистики прочитал по последней просьбе.
    pub day_data: Option<Arc<Day>>,
    /// О чём окно попросило поток статистики (не просить дважды).
    asked: Option<DayOrder>,
    pub shown_time: Option<TimeShown>,
    /// Перетаскивание по полю «по времени»: где мышь была в прошлом событии.
    drag_x: Option<f32>,
    /// Поток статистики: ему уходят блоки и соседи из знакомства — по ним считается модель.
    stats: Option<mpsc::Sender<Order>>,
    /// Подсветка линий (владелец 15.10 не решил, какая удобнее, — переключатель в окне): false — под курсором (плитка
    /// или линия графика), true — закреплённая: последний добавленный блок или линия, по которой щёлкнули.
    pub pin_mode: bool,
    pub pinned: Option<usize>,
    pub hovered: Option<usize>,
    /// Выбранная строка списка наборов: "" — заводские; None — ничего (до первых профилей: тогда — профиль запуска).
    pub chosen: Option<String>,
    shown_profiles: Option<ProfileShown>,
    pub shown_stats: Option<StatsShown>,
    shown_links: Vec<(LinkData, f64)>,
    refused: Option<String>,
    up: bool,
    why: String,
    problem: Option<String>,
    /// Последнее сообщение владельцу от программы (notice): номер и текст; щелчок по строке — прочитано.
    notice: Option<(i64, String)>,
    pending: HashMap<i64, Pending>,
    menu_block: Option<usize>,
    pub changes: Changes,
    /// Выбор автомодуля из темы settings программы у часов; None — его нет (движок C# такой темы не шлёт).
    pub auto: Option<AutoSettings>,
    /// Журнал срезов пишется (тема settings, поле log); None — программа его не сообщает (движок C#, `CoreMapTray` до
    /// 0.5.5): выключателя «запись» в окне нет.
    pub log: Option<bool>,
    /// Отсечка под ручкой ползунка: пока его тянут и после отпускания, пока программа не подтвердила её (темой и
    /// состоянием); None — ручка и линия стоят на выборе программы.
    limit_held: Option<i32>,
    limit_dragging: bool,
    /// То же у аварийной отсечки (подтверждение — тема settings).
    emergency_held: Option<i32>,
    emergency_dragging: bool,
    /// Сколько команд окно послало (скрытой проверке: движение ползунка — одна команда).
    pub sent: u32,
    /// Файл окна: что оно помнит о себе; запоминается при смене выбора и подсветки и при закрытии.
    memory: Remembered,
    /// Сравниваемые блоки из файла окна ждут первого знакомства: только тогда известно, какие блоки есть.
    restore: Option<Vec<String>>,
    /// Программа на том конце сказала «bye»: окно запомнило себя и закрывается (`leave`).
    pub left: bool,
}

impl View {
    /// `memory` — что окно помнит о себе (src/saved.rs, `saved::open`): подсветка — сразу, блоки — при знакомстве.
    pub fn new(ui: &AppWindow, link: Arc<Link>, memory: Remembered) -> View {
        let tiles = Rc::new(VecModel::default());
        ui.set_tiles(ModelRc::from(Rc::clone(&tiles)));
        let sets = Rc::new(VecModel::default());
        ui.set_sets(ModelRc::from(Rc::clone(&sets)));
        let pin_mode = memory.saved.pin.unwrap_or(false);
        let scale = memory.saved.view.unwrap_or_default();
        let curves = memory.saved.curves.unwrap_or_default();
        let max = memory.saved.max.unwrap_or(false);
        let model = memory.saved.model.unwrap_or(true);
        let swap = memory.saved.swap.unwrap_or(false);
        let timeline = memory.saved.timeline.unwrap_or(false);
        let layout = match memory.saved.layout {
            Some(Layout::Params) => Layout::Params,
            _ => Layout::Overlay,
        };
        let quantity = memory.saved.quantity.unwrap_or_default();
        let selected = saved::selected_metrics(&memory.saved);
        let normalized = memory.saved.normalized.unwrap_or(true);
        let time_follow = memory.saved.time_follow.unwrap_or(true);
        let range_parameter = Quantity::of(memory.saved.range_parameter.unwrap_or(quantity.index()));
        let ranges = memory.saved.ranges.clone(); // настройки диапазонов, один раз при открытии
        let events_all = memory.saved.events_all.unwrap_or(false);
        let restore = (!memory.saved.compared.is_empty()).then(|| memory.saved.compared.clone()); // a few names, once
        let problem = memory.trouble.clone(); // one line, once
        let view = View {
            ui: ui.as_weak(),
            link,
            hello: None,
            limits: None,
            state: None,
            profiles: None,
            digest: None,
            geometry: Geometry::default(),
            tiles,
            sets,
            compared: Vec::new(),
            scale,
            curves,
            max,
            model,
            swap,
            timeline,
            layout,
            quantity,
            selected,
            ranges,
            range_unit: quantity.unit(),
            normalized,
            time_follow,
            range_parameter,
            events_all,
            events_key: None,
            listed: Vec::new(),
            centre: None,
            day: None,
            span: None,
            day_data: None,
            asked: None,
            shown_time: None,
            drag_x: None,
            stats: None,
            pin_mode,
            pinned: None,
            hovered: None,
            chosen: None,
            shown_profiles: None,
            shown_stats: None,
            shown_links: Vec::new(),
            refused: None,
            up: false,
            why: "окно запускается".into(),
            problem,
            notice: None,
            pending: HashMap::new(),
            menu_block: None,
            changes: Changes::default(),
            auto: None,
            log: None,
            limit_held: None,
            limit_dragging: false,
            emergency_held: None,
            emergency_dragging: false,
            sent: 0,
            memory,
            restore,
            left: false,
        };
        view.show_link();
        view.show_problem();
        view.show_mode();
        if ui.get_pin_mode() != pin_mode {
            ui.set_pin_mode(pin_mode);
        }
        view
    }

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Up => {
                self.up = true;
                self.refused = None;
                self.show_link();
                self.show_auto();
            }
            Event::Down(why) => {
                self.up = false;
                self.why = why;
                self.pending.clear();
                // Ответов на посланное уже не будет: ручки — на последнем известном выборе программы.
                self.limit_held = None;
                self.limit_dragging = false;
                self.emergency_held = None;
                self.emergency_dragging = false;
                self.show_link();
                self.show_auto();
                self.refresh_stats();
            }
            Event::Message(Message::Hello(hello)) => self.greet(hello),
            Event::Message(Message::State(state)) => self.apply(state),
            Event::Message(Message::Limits(limits)) => {
                self.limits = Some(limits);
                self.refresh(false);
                self.refresh_profiles();
                // Заводские потолки — края оси множителя.
                self.refresh_stats();
            }
            Event::Message(Message::Profiles(profiles)) => {
                // Выбор живёт, пока такой профиль есть; окно открылось или выбранного больше нет — профиль запуска.
                let keep = self
                    .chosen
                    .as_deref()
                    .is_some_and(|c| c.is_empty() || profiles.sets.iter().any(|s| s.name == c));
                if !keep {
                    self.chosen = Some(panels::default_choice(&profiles));
                }
                self.profiles = Some(profiles);
                self.refresh_profiles();
                // Разница на плитках — с выбранным профилем: его потолки могли смениться («сохранить»).
                self.refresh(false);
            }
            Event::Message(Message::Reply(reply)) => {
                if reply.re == 0 {
                    if !reply.ok {
                        self.refused = Some(reply.why.unwrap_or_else(|| "движок отказал".into()));
                        self.show_link();
                    }
                } else if let Some(pending) = self.pending.remove(&reply.re) {
                    self.problem = if reply.ok {
                        None
                    } else {
                        Some(format!("{}: {}", pending.failure, reply.why.unwrap_or_default()))
                    };
                    if !reply.ok && pending.held {
                        self.limit_held = None;
                        self.emergency_held = None;
                        self.show_auto();
                        self.refresh_stats();
                        self.refresh_time();
                    }
                    if reply.ok
                        && let Some(name) = pending.saved_as
                    {
                        self.ui().set_prof_name(SharedString::new());
                        self.chosen = Some(name);
                        self.refresh_profiles();
                        self.refresh(false);
                    }
                    self.show_problem();
                }
            }
            Event::Message(Message::Notice(n)) => {
                if self.notice.as_ref().is_none_or(|(seq, _)| n.seq > *seq) {
                    self.notice = Some((n.seq, n.text));
                    self.show_problem();
                }
            }
            Event::Message(Message::Settings(settings)) => {
                let lines = (self.cutoff(), self.emergency());
                self.auto = settings.auto;
                self.log = settings.log.map(|l| l.on);
                self.settle_holds();
                self.show_auto();
                if (self.cutoff(), self.emergency()) != lines {
                    self.refresh_stats();
                    self.refresh_time();
                }
            }
            Event::Message(Message::Bye) => self.leave(),
            Event::Message(Message::Other) => {}
        }
    }

    /// `CoreMapTray` выходит и сказал «bye» (владелец 15.10: «чтобы трей сначала закрывал окно… а потом закрывался
    /// сам»): окно запоминает себя, как при закрытии, и закрывается — без программы оно ничего не показывает и ничем не
    /// управляет. Программа выходит, когда процесс окна кончился (канал оборвался).
    fn leave(&mut self) {
        if self.left {
            return;
        }
        self.left = true;
        self.closing();
        // Скрыть — окно уходит с экрана сразу; кончить цикл — main.rs выходит из `run` и процесс кончается (окно всё
        // равно уйдёт, даже если скрыть не вышло). Скрытая проверка крутит цикл сама: там цикла Slint нет, и
        // `quit_event_loop` отвечает ошибкой — это не сбой.
        let _ = self.ui().hide(); // see above: the loop's end closes the window anyway
        let _ = slint::quit_event_loop(); // see above: no Slint loop in the hidden check
    }

    /// Поток статистики (store.rs): ему — блоки и соседи из знакомства, сейчас (если оно уже было) и при каждом новом.
    pub fn stats_to(&mut self, stats: mpsc::Sender<Order>) {
        self.stats = Some(stats);
        self.send_die();
    }

    fn send_die(&self) {
        if let (Some(stats), Some(hello)) = (&self.stats, &self.hello) {
            // Поток кончился — статистики и модели нет, окно работает без них.
            let _ = stats.send(Order::Die(Arc::new(Die::of(hello))));
        }
    }

    /// Новая сводка статистики (поток store.rs).
    pub fn on_digest(&mut self, digest: Arc<Digest>) {
        self.digest = Some(digest);
        self.refresh_stats();
        self.refresh_links();
        // Дни могли появиться (полночь), события — дописаться.
        self.ask_day();
        self.refresh_time();
    }

    /// День оси времени прочитан (поток store.rs). Ответ не на последнюю просьбу — не нужен. Тот же день перечитан
    /// (дописан или с другими рядами): показ, который был весь день, растёт с ним; иначе остаётся. Ждёт центр
    /// события — показ вокруг него.
    pub fn on_day(&mut self, day: Arc<Day>) {
        if self.asked.as_ref().is_none_or(|a| !answers(a, &day)) {
            return;
        }
        self.day_data = Some(day);
        self.refresh_ranges();
        self.apply_centre();
        self.refresh_time();
    }

    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("окно")
    }

    fn show_link(&self) {
        let ui = self.ui();
        let text = if self.up {
            self.refused.clone().unwrap_or_default()
        } else {
            self.refused.clone().unwrap_or_else(|| self.why.clone())
        };
        set_text(&ui.get_link(), &text, |t| ui.set_link(t));
        let down = !self.up || self.refused.is_some();
        if ui.get_link_down() != down {
            ui.set_link_down(down);
        }
    }

    /// Строка под верхней полосой: отказ последней команды, иначе сообщение программы владельцу, иначе почему нет
    /// показаний (у программы входа — например, процессором владеет прежнее окно), иначе сработала защита.
    fn show_problem(&self) {
        let ui = self.ui();
        let state = self.state.as_ref();
        let text = self
            .problem
            .clone()
            .or_else(|| self.notice.as_ref().map(|(_, text)| text.clone()))
            .or_else(|| state.and_then(|s| s.sensor_error.clone()))
            .or_else(|| state.and_then(|s| s.trip.clone()))
            .unwrap_or_default();
        set_text(&ui.get_problem(), &text, |t| ui.set_problem(t));
    }

    fn greet(&mut self, hello: Hello) {
        let ui = self.ui();
        ui.set_cpu(format!("{}{}", hello.cpu, if hello.simulated { " · имитация" } else { "" }).into());
        let note = if hello.layout.confirmed {
            String::new()
        } else {
            hello.layout.note.clone().unwrap_or_default()
        };
        ui.set_note(note.into());
        self.geometry = die::geometry(&hello);
        ui.set_die_nat_w(self.geometry.width);
        ui.set_die_nat_h(self.geometry.height);
        // Выбор переживает новое знакомство, пока такие блоки есть; при первом — выбор из файла окна (его блоки,
        // которых больше нет, выпадают); пусто — первое P-ядро.
        self.compared.retain(|&b| b < hello.blocks.len());
        self.pinned = self.pinned.filter(|&b| b < hello.blocks.len());
        self.hovered = None;
        if let Some(restore) = self.restore.take()
            && self.compared.is_empty()
        {
            self.compared = saved::compared(&restore, &hello.blocks);
            self.pinned = self.compared.last().copied();
        }
        if self.compared.is_empty()
            && let Some(first) = hello
                .blocks
                .iter()
                .find(|b| b.kind == "P")
                .or_else(|| hello.blocks.first())
        {
            self.compared.push(first.index);
            self.pinned = Some(first.index);
        }
        self.hello = Some(hello);
        self.send_die();
        self.refresh(true);
        self.shown_stats = None;
        self.refresh_stats();
        self.shown_links.clear();
        self.refresh_links();
        self.refresh_profiles();
    }

    fn apply(&mut self, state: State) {
        self.changes.states += 1;
        self.changes.last = 0;
        let s = &state;
        let ui = self.ui();
        let pkg = s.pkg.map_or("–".to_string(), |v| format!("{v:.0} °C"));
        let watts = s.watts.map_or("– Вт".to_string(), |v| format!("{v:.1} Вт"));
        let fans = match &s.fans.rpm {
            Some(rpm) if !rpm.is_empty() => {
                let shown: Vec<String> = rpm
                    .iter()
                    .map(|v| v.map_or("–".into(), |x| format!("{x:.0}")))
                    .collect();
                format!("вент. {} об/мин", shown.join(" / "))
            }
            _ => String::new(),
        };
        let busy = s.busy;
        let mut changed = 0;
        changed += set_text(&ui.get_pkg(), &pkg, |t| ui.set_pkg(t));
        changed += set_text(&ui.get_watts(), &watts, |t| ui.set_watts(t));
        changed += set_text(&ui.get_fans(), &fans, |t| ui.set_fans(t));
        if ui.get_busy() != busy {
            ui.set_busy(busy);
            changed += 1;
        }
        self.changes.last += changed;
        // Панель профилей зависит от потолков, надстройки автомодуля (и потолков владельца под ней) и хода измерения —
        // больше ни от чего в состоянии.
        let profiles_moved = self.state.as_ref().is_none_or(|old| {
            old.limit != state.limit
                || old.auto != state.auto
                || old.intended != state.intended
                || old.busy != state.busy
        });
        // Статистика зависит от состояния только отсечкой автомодуля (линия на графике).
        let cutoff = self.cutoff();
        let first = self.state.is_none();
        self.state = Some(state);
        self.settle_holds();
        self.show_problem();
        self.refresh(false);
        if profiles_moved {
            self.refresh_profiles();
        }
        if first || self.cutoff() != cutoff {
            self.refresh_stats();
        }
    }

    /// Строки плиток заново; rebuild — после знакомства: раскладка могла смениться, модель строится с нуля.
    fn refresh(&mut self, rebuild: bool) {
        let Some(hello) = self.hello.as_ref() else { return };
        let rows: Vec<TileData> = hello
            .layout
            .tiles
            .iter()
            .enumerate()
            .filter_map(|(i, t)| self.tile(hello, i, t))
            .collect();
        if rebuild || self.tiles.row_count() != rows.len() {
            self.tiles.set_vec(rows);
            return;
        }
        for (i, row) in rows.into_iter().enumerate() {
            if !self.tiles.row_data(i).is_some_and(|shown| same(&shown, &row)) {
                self.tiles.set_row_data(i, row);
                self.changes.last += 1;
            }
        }
    }

    /// Строка плитки; None — раскладка ссылается на блок, которого в знакомстве нет (испорченное сообщение).
    #[expect(
        clippy::too_many_lines,
        reason = "one tile's fields read top to bottom, as in DieView.OnPaint"
    )]
    fn tile(&self, hello: &Hello, index: usize, t: &crate::wire::Tile) -> Option<TileData> {
        let block = hello.blocks.get(t.block)?;
        let members = &block.members;
        let first = block.first;
        let place = self.geometry.tiles.get(index).copied().unwrap_or_default();
        let cluster = t.places.len() > 1;
        let s = self.state.as_ref();
        let thr = |i: usize| s.is_some_and(|s| s.thr.get(i).copied().unwrap_or(false));
        let cur = |i: usize| s.and_then(|s| s.cur.get(i).copied().flatten());
        let mut data = TileData {
            block: slint_int(block.index),
            x: place.x,
            y: place.y,
            w: place.w,
            h: place.h,
            cluster,
            fill: rgb(CELL),
            temp: "–".into(),
            peak: "▲ –".into(),
            mean: "ø –".into(),
            pair_color: rgb(TEXT),
            strip: rgb(NO_PEAK),
            ..Default::default()
        };
        if self.compared.contains(&block.index) {
            data.selected = true;
            data.outline = panels::rgb(chart::color_of(&hello.blocks, block.index));
        }
        if cluster {
            let cells: Vec<CellData> = t
                .places
                .iter()
                .enumerate()
                .filter_map(|(k, &i)| {
                    let core = hello.cores.get(i)?;
                    let temp = cur(i);
                    Some(CellData {
                        text: core.lp.to_string().into(),
                        temp: temp.map_or_else(|| "–".into(), |v| format!("{v:.0}°").into()),
                        fill: temp.map_or(rgb(CELL), heat_of),
                        num_color: rgb(if thr(i) { BAD } else { TEXT }),
                        col: slint_int(k % 2),
                        row: slint_int(k / 2),
                    })
                })
                .collect();
            data.cells = ModelRc::new(VecModel::from(cells));
        } else {
            data.number = hello
                .cores
                .get(first)
                .map(|c| c.lp.to_string())
                .unwrap_or_default()
                .into();
            // Номер белый; красный — ядро сбрасывает частоту (владелец 15.10: жёлтый номер читался как меняющееся число).
            data.number_color = rgb(if thr(first) { BAD } else { TEXT });
        }
        let Some(s) = s else {
            return Some(data);
        };
        let temp = highest(&s.cur, members);
        if let Some(v) = temp {
            data.temp = format!("{v:.0}°").into();
            data.fill = heat_of(v);
        }
        let peak = highest(&s.max, members);
        if let Some(p) = peak {
            data.peak = format!("▲ {p:.0}°").into();
            // Полоска слева — ▲ наибольшая цветом, по той же шкале, что плитка: видно, какое ядро грелось сильнее
            // (владелец 15.10).
            data.strip = heat_of(p);
        }
        let mean = highest(&s.avg, members);
        let mean_ratio = highest(&s.avg_ratio, members);
        let mean_text = mean.map_or_else(|| "ø –".to_string(), |m| format!("ø {}°", m.round()));
        let ratio_text = mean_ratio.map(|r| format!("×{r:.1}"));
        if cluster {
            data.mean = mean_text.into();
            data.mean_ratio = ratio_text.map(|r| format!("ø {r}")).unwrap_or_default().into();
        } else {
            data.mean = match ratio_text {
                Some(r) => format!("{mean_text} · {r}"),
                None => mean_text,
            }
            .into();
        }
        let c = panels::ceiling(
            block,
            s.limit.as_deref(),
            self.chosen_ratios(),
            panels::Overlay::of(Some(s)),
        );
        let ratio = highest(&s.ratio, members);
        let fused = self.limits.as_ref().and_then(|l| l.fused.get(first).copied());
        let chosen_title = self.chosen.as_deref().map(panels::titled);
        if let Some(limit) = c.now {
            data.pair = match ratio {
                Some(r) => format!("{r:.1} / ×{limit}"),
                None => format!("×{limit}"),
            }
            .into();
            if let Some(fused) = fused {
                data.pair_color = rgb(match limit.cmp(&fused) {
                    Ordering::Less => ACCENT,
                    Ordering::Greater => BAD,
                    Ordering::Equal => TEXT,
                });
            }
            // Разница текущего состояния с выбранным профилем (владелец 15.10: «в дельте… разницу между… параметрам
            // профиля и параметрам текущего состояния»).
            data.delta = panels::signed(c.delta()).into();
        }
        if c.auto != 0 {
            data.auto = format!("авто {}", panels::signed(c.auto)).into();
        }
        data.ban = c.ban.is_some();
        let by_ceiling = panels::ceilings_of(block, s.by_ceiling.as_deref());
        data.tip = panels::tile_tip(&panels::TileFacts {
            block,
            cores: &hello.cores,
            cur: &s.cur,
            temp,
            peak,
            mean,
            ratio,
            mean_ratio,
            throttled: members.iter().any(|&i| thr(i)),
            ceiling: c,
            fused,
            profile: chosen_title.as_deref(),
            by_ceiling: &by_ceiling,
        })
        .into();
        Some(data)
    }

    // ----- полосы связи соседей -----

    #[expect(
        clippy::float_cmp,
        reason = "the same computation on the same input gives the same bits: unchanged"
    )]
    fn refresh_links(&mut self) {
        let (Some(hello), Some(digest)) = (self.hello.as_ref(), self.digest.as_ref()) else {
            return;
        };
        let links = panels::links(hello, &self.geometry, digest);
        let same_links = links.len() == self.shown_links.len()
            && links.iter().zip(&self.shown_links).all(|((a, da), (b, db))| {
                da == db && a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h && a.color == b.color && a.tip == b.tip
            });
        if same_links {
            return;
        }
        let ui = self.ui();
        ui.set_link_legend(!links.is_empty());
        ui.set_links(ModelRc::new(VecModel::from(
            links.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        ))); // a dozen links, on a changed digest
        self.shown_links = links;
    }

    /// Перемычки, как они показаны (скрытой проверке): сколько и разница у каждой, °C.
    pub fn links_shown(&self) -> (usize, Vec<f64>) {
        (
            self.shown_links.len(),
            self.shown_links.iter().map(|(_, d)| *d).collect(),
        )
    }

    // ----- статистика -----

    /// Подсвеченный блок: под курсором или закреплённый — по переключателю.
    fn highlight(&self) -> Option<usize> {
        if self.pin_mode { self.pinned } else { self.hovered }
    }

    /// Отсечка на графике: пока автомодуль включён (в состоянии есть его отсечка) — она, а пока ползунок тянут или его
    /// число ещё не подтверждено — число под ручкой. Выключен — линии нет.
    fn cutoff(&self) -> Option<f64> {
        let on = self.state.as_ref().and_then(|s| s.auto_limit)?;
        Some(self.limit_held.map_or(on, f64::from))
    }

    /// Аварийная отсечка на графике: так же — пока автомодуль включён; число — из темы settings или под ручкой.
    fn emergency(&self) -> Option<f64> {
        self.state.as_ref().and_then(|s| s.auto_limit)?;
        let set = self.auto.as_ref().and_then(|a| a.emergency)?;
        Some(f64::from(self.emergency_held.unwrap_or_else(|| slint_int(set))))
    }

    fn refresh_stats(&mut self) {
        self.refresh_events();
        let cutoff = self.cutoff();
        let emergency = self.emergency();
        let Some(hello) = self.hello.as_ref() else { return };
        let shown = chart::stats_panel(&chart::StatsInput {
            blocks: &hello.blocks,
            compared: &self.compared,
            highlight: self.highlight(),
            cutoff,
            emergency,
            digest: self.digest.as_deref(),
            scale: self.scale,
            curves: self.curves,
            max: self.max,
            model: self.model,
            swap: self.swap,
            argument_range: self.ranges.get("ratio").copied(),
            value_range: self.ranges.get(&format!("ratio-value:{}", self.scale.index())).copied(),
        });
        if self.shown_stats.as_ref() == Some(&shown) {
            return;
        }
        let ui = self.ui();
        set_text(&ui.get_stats_note(), &shown.note, |t| ui.set_stats_note(t));
        set_text(&ui.get_chart_empty(), &shown.empty, |t| ui.set_chart_empty(t));
        set_text(&ui.get_chart_cutoff_text(), &shown.cutoff_text, |t| {
            ui.set_chart_cutoff_text(t);
        });
        set_text(&ui.get_chart_emergency_text(), &shown.emergency_text, |t| {
            ui.set_chart_emergency_text(t);
        });
        if ui.get_chart_emergency().to_bits() != shown.emergency.to_bits() {
            ui.set_chart_emergency(shown.emergency);
        }
        set_text(&ui.get_chart_value_title(), &shown.value_title, |t| {
            ui.set_chart_value_title(t);
        });
        // Та же величина из того же расчёта — те же биты.
        if ui.get_chart_cutoff().to_bits() != shown.cutoff.to_bits() {
            ui.set_chart_cutoff(shown.cutoff);
        }
        if ui.get_chart_zero().to_bits() != shown.zero.to_bits() {
            ui.set_chart_zero(shown.zero);
        }
        if ui.get_chart_view() != self.scale.index() {
            ui.set_chart_view(self.scale.index());
        }
        if ui.get_chart_mean() != self.curves.mean() {
            ui.set_chart_mean(self.curves.mean());
        }
        if ui.get_chart_p95() != self.curves.p95() {
            ui.set_chart_p95(self.curves.p95());
        }
        if ui.get_chart_max() != self.max {
            ui.set_chart_max(self.max);
        }
        if ui.get_chart_model() != self.model {
            ui.set_chart_model(self.model);
        }
        if ui.get_chart_swap() != self.swap {
            ui.set_chart_swap(self.swap);
        }
        #[expect(clippy::cast_precision_loss, reason = "a few dozen columns: exact in f32")]
        let width = shown.columns.len().max(1) as f32;
        ui.set_chart_columns(width);
        if ui.get_pin_mode() != self.pin_mode {
            ui.set_pin_mode(self.pin_mode);
        }
        let old = self.shown_stats.as_ref();
        if old.is_none_or(|o| o.series != shown.series) {
            ui.set_chart_series(ModelRc::new(VecModel::from(shown.series_rows())));
        }
        if old.is_none_or(|o| o.columns != shown.columns) {
            ui.set_chart_cols(ModelRc::new(VecModel::from(shown.column_rows())));
        }
        if old.is_none_or(|o| o.ticks != shown.ticks) {
            ui.set_chart_ticks(ModelRc::new(VecModel::from(shown.tick_rows())));
        }
        if old.is_none_or(|o| o.legend != shown.legend) {
            ui.set_stats_legend(ModelRc::new(VecModel::from(shown.legend_rows())));
        }
        self.shown_stats = Some(shown);
        self.show_ranges();
    }

    // ----- профили -----

    /// Потолки выбранного профиля по ядрам (`panels::chosen_ratios`).
    fn chosen_ratios(&self) -> Option<&[i64]> {
        panels::chosen_ratios(self.profiles.as_ref(), self.limits.as_ref(), self.chosen.as_deref())
    }

    fn refresh_profiles(&mut self) {
        let (Some(hello), Some(profiles)) = (self.hello.as_ref(), self.profiles.as_ref()) else {
            return;
        };
        let ui = self.ui();
        let name = ui.get_prof_name();
        let state = self.state.as_ref();
        let shown = panels::profile_panel(&ProfileInput {
            hello,
            profiles,
            limits: self.limits.as_ref(),
            limit: state.and_then(|s| s.limit.as_deref()),
            auto: panels::Overlay::of(state),
            chosen: self.chosen.as_deref(),
            name: name.as_str(),
            busy: state.is_some_and(|s| s.busy),
        });
        if self.shown_profiles.as_ref() == Some(&shown) {
            return;
        }
        if !ui.get_has_profiles() {
            ui.set_has_profiles(true);
        }
        set_text(&ui.get_prof_status(), &shown.status, |t| ui.set_prof_status(t));
        set_text(&ui.get_prof_status_tip(), &shown.status_tip, |t| {
            ui.set_prof_status_tip(t);
        });
        set_text(&ui.get_save_tip(), &shown.save_tip, |t| ui.set_save_tip(t));
        set_text(&ui.get_save_as_text(), &shown.save_as_text, |t| ui.set_save_as_text(t));
        set_text(&ui.get_save_note(), &shown.save_note, |t| ui.set_save_note(t));
        let flags = [
            (
                ui.get_prof_attention(),
                shown.attention,
                AppWindow::set_prof_attention as fn(&AppWindow, bool),
            ),
            (ui.get_can_apply(), shown.can_apply, AppWindow::set_can_apply),
            (ui.get_can_save(), shown.can_save, AppWindow::set_can_save),
            (ui.get_can_save_as(), shown.can_save_as, AppWindow::set_can_save_as),
            (ui.get_can_delete(), shown.can_delete, AppWindow::set_can_delete),
            (ui.get_can_startup(), shown.can_startup, AppWindow::set_can_startup),
            (ui.get_startup_on(), shown.startup_on, AppWindow::set_startup_on),
        ];
        for (now, want, set) in flags {
            if now != want {
                set(&ui, want);
            }
        }
        let old_rows = self
            .shown_profiles
            .as_ref()
            .map(|p| p.rows.as_slice())
            .unwrap_or_default();
        if old_rows.len() == shown.rows.len() && self.sets.row_count() == shown.rows.len() {
            for (i, (old, new)) in old_rows.iter().zip(&shown.rows).enumerate() {
                if old != new {
                    self.sets.set_row_data(i, new.row());
                }
            }
        } else {
            self.sets
                .set_vec(shown.rows.iter().map(panels::SetShown::row).collect::<Vec<_>>());
        }
        self.shown_profiles = Some(shown);
    }

    /// Панель профилей, как она показана (скрытой проверке).
    pub fn profile_shown(&self) -> Option<&ProfileShown> {
        self.shown_profiles.as_ref()
    }

    // ----- команды владельца -----

    pub fn order(&mut self, command: &str, args: Map<String, Value>, failure: &'static str) {
        self.order_then(command, args, Pending::failing(failure));
    }

    /// Команда; false — канала нет (сказано строкой под верхней полосой).
    fn order_then(&mut self, command: &str, args: Map<String, Value>, pending: Pending) -> bool {
        // На эти команды программа может спросить владельца своим окошком (потолок выше заводского): пусть оно выйдет
        // на передний план, а не мигает под окном.
        if matches!(command, "limit.set" | "profile.open") {
            self.link.allow_foreground();
        }
        if let Some(id) = self.link.ask(command, args) {
            self.sent += 1;
            self.pending.insert(id, pending);
            true
        } else {
            self.problem = Some(format!("{}: {}", pending.failure, self.link.peer().unreachable()));
            self.show_problem();
            false
        }
    }

    // ----- автомодуль -----

    /// Выключатели и ползунки — по выбору программы у часов; ручки, которые ждут подтверждения, — на своём числе.
    fn show_auto(&self) {
        let ui = self.ui();
        let has = self.auto.is_some();
        if ui.get_has_auto() != has {
            ui.set_has_auto(has);
        }
        let ready = has && self.up && self.refused.is_none();
        if ui.get_auto_ready() != ready {
            ui.set_auto_ready(ready);
        }
        if ui.get_has_log() != self.log.is_some() {
            ui.set_has_log(self.log.is_some());
        }
        let logging = self.log.unwrap_or(false);
        if ui.get_log_on() != logging {
            ui.set_log_on(logging);
        }
        let Some(a) = &self.auto else { return };
        let ints = [
            (
                ui.get_auto_limit(),
                self.limit_held.unwrap_or_else(|| slint_int(a.limit)),
                AppWindow::set_auto_limit as fn(&AppWindow, i32),
            ),
            (
                ui.get_auto_limit_min(),
                slint_int(a.limit_range[0]),
                AppWindow::set_auto_limit_min,
            ),
            (
                ui.get_auto_limit_max(),
                slint_int(a.limit_range[1]),
                AppWindow::set_auto_limit_max,
            ),
        ];
        for (now, want, set) in ints {
            if now != want {
                set(&ui, want);
            }
        }
        if ui.get_auto_on() != a.on {
            ui.set_auto_on(a.on);
        }
        // Аварийная (с 0.5.6): её промежуток — от цели под ручкой + 1, пока цель тянут (бриф T: цель + 1 … 100).
        let has = a.emergency.is_some();
        if ui.get_has_emergency() != has {
            ui.set_has_emergency(has);
        }
        if let Some(set) = a.emergency {
            let [low, high] = match self.limit_held {
                Some(limit) if a.emergency_range.is_none() => [i64::from(limit) + 1, 100],
                _ => a.emergency_span(),
            };
            let ints = [
                (
                    ui.get_auto_emergency(),
                    self.emergency_held.unwrap_or_else(|| slint_int(set)),
                    AppWindow::set_auto_emergency as fn(&AppWindow, i32),
                ),
                (
                    ui.get_auto_emergency_min(),
                    slint_int(low),
                    AppWindow::set_auto_emergency_min,
                ),
                (
                    ui.get_auto_emergency_max(),
                    slint_int(high),
                    AppWindow::set_auto_emergency_max,
                ),
            ];
            for (now, want, set) in ints {
                if now != want {
                    set(&ui, want);
                }
            }
        }
    }

    /// Ручка отсечки ждала подтверждения: отпускается, когда и тема, и состояние (линия) показывают её число (или
    /// автомодуль выключен — линии нет).
    #[expect(
        clippy::float_cmp,
        reason = "both sides are whole degrees sent as JSON integers: exact in f64"
    )]
    fn settle_holds(&mut self) {
        let Some(a) = &self.auto else { return };
        if !self.emergency_dragging
            && let Some(held) = self.emergency_held
            && a.emergency == Some(i64::from(held))
        {
            self.emergency_held = None;
        }
        if !self.limit_dragging
            && let Some(held) = self.limit_held
            && i64::from(held) == a.limit
            && self
                .state
                .as_ref()
                .and_then(|s| s.auto_limit)
                .is_none_or(|line| line == f64::from(held))
        {
            self.limit_held = None;
        }
    }

    fn set_auto(&mut self, field: &str, value: Value, failure: &'static str, held: bool) -> bool {
        let mut args = Map::new();
        args.insert(field.into(), value);
        self.order_then(
            "auto.set",
            args,
            Pending {
                failure,
                saved_as: None,
                held,
            },
        )
    }

    pub fn auto_toggled(&mut self) {
        if let Some(on) = self.auto.as_ref().map(|a| a.on) {
            self.set_auto("on", (!on).into(), "Автомодуль не переключён", false);
        }
    }

    /// «запись»: журнал срезов включить или выключить — команда log.set; выключатель встаёт, когда программа подтвердит
    /// темой settings.
    pub fn log_toggled(&mut self) {
        if let Some(on) = self.log {
            let mut args = Map::new();
            args.insert("on".into(), (!on).into());
            self.order_then("log.set", args, Pending::failing("Журнал срезов не переключён"));
        }
    }

    /// Ползунок отсечки тянут: число под ручкой — линией на графике (если автомодуль включён). Команды нет.
    pub fn limit_moved(&mut self, value: i32) {
        let cutoff = self.cutoff();
        self.limit_dragging = true;
        self.limit_held = Some(value);
        // Нижний край аварийной идёт за целью.
        self.show_auto();
        if self.cutoff() != cutoff {
            self.refresh_stats();
            self.refresh_time();
        }
    }

    /// Ползунок отсечки отпустили: одна команда, если число другое.
    pub fn limit_released(&mut self, value: i32) {
        let cutoff = self.cutoff();
        self.limit_dragging = false;
        let same = self.auto.as_ref().is_none_or(|a| a.limit == i64::from(value));
        self.limit_held = (!same && self.set_auto("limit", value.into(), "Отсечка не изменена", true)).then_some(value);
        self.show_auto();
        if self.cutoff() != cutoff {
            self.refresh_stats();
            self.refresh_time();
        }
    }

    /// Ползунок аварийной отсечки тянут: число под ручкой — линией на графике. Команды нет.
    pub fn emergency_moved(&mut self, value: i32) {
        let line = self.emergency();
        self.emergency_dragging = true;
        self.emergency_held = Some(value);
        if self.emergency() != line {
            self.refresh_stats();
            self.refresh_time();
        }
    }

    /// Ползунок аварийной отсечки отпустили: одна команда auto.set {emergency}, если число другое.
    pub fn emergency_released(&mut self, value: i32) {
        let line = self.emergency();
        self.emergency_dragging = false;
        let same = self.auto.as_ref().is_none_or(|a| a.emergency == Some(i64::from(value)));
        self.emergency_held =
            (!same && self.set_auto("emergency", value.into(), "Аварийная отсечка не изменена", true)).then_some(value);
        self.show_auto();
        if self.emergency() != line {
            self.refresh_stats();
            self.refresh_time();
        }
    }

    // ----- что окно помнит о себе -----

    /// Пишет файл окна, если запомненное изменилось: сравниваемые блоки (по названиям), подсветка, обычные размер и
    /// место. Развёрнутое или свёрнутое окно своих обычных размера и места не сообщает — остаются прежние.
    fn remember(&mut self) {
        let ui = self.ui();
        let window = ui.window();
        let mut saved = self.memory.saved.clone(); // a few names and numbers, once per click
        if let Some(hello) = &self.hello {
            saved.compared = self
                .compared
                .iter()
                .filter_map(|&b| Some(hello.blocks.get(b)?.name.clone()))
                .collect();
        }
        saved.pin = Some(self.pin_mode);
        saved.view = Some(self.scale);
        saved.curves = Some(self.curves);
        saved.max = Some(self.max);
        saved.model = Some(self.model);
        saved.swap = Some(self.swap);
        saved.timeline = Some(self.timeline);
        saved.layout = Some(self.layout);
        saved.quantity = Some(self.quantity);
        saved.normalized = Some(self.normalized);
        saved.time_follow = Some(self.time_follow);
        saved.range_parameter = Some(self.range_parameter.index());
        saved.metrics = Some(self.selected.iter().map(|q| q.index()).collect());
        saved.ranges.clone_from(&self.ranges);
        saved.events_all = Some(self.events_all);
        saved.maximized = window.is_maximized();
        if !window.is_maximized() && !window.is_minimized() {
            let size = window.size().to_logical(window.scale_factor());
            saved.size = Some((size.width, size.height));
            let at = window.position();
            saved.position = Some((at.x, at.y));
        }
        if saved == self.memory.saved {
            return;
        }
        match saved::write(&self.memory.path, &saved) {
            Ok(()) => self.memory.saved = saved,
            Err(e) => {
                self.problem = Some(format!("Окно не запомнило выбор: {} не записан ({e})", saved::FILE));
                self.show_problem();
            }
        }
    }

    /// Окно закрывают: размер и место — в файл окна (событий о перемещении нет, это единственная минута их узнать).
    pub fn closing(&mut self) {
        self.remember();
    }

    /// Щелчок по плитке: блока нет в статистике — добавить (сколько угодно блоков, у каждого свой цвет), есть —
    /// убрать. Больше щелчок ничего не делает.
    pub fn clicked(&mut self, block: i32) {
        let Ok(block) = usize::try_from(block) else { return };
        if let Some(at) = self.compared.iter().position(|&b| b == block) {
            self.compared.remove(at);
            if self.pinned == Some(block) {
                self.pinned = self.compared.last().copied();
            }
            if self.hovered == Some(block) {
                self.hovered = None;
            }
        } else {
            self.compared.push(block);
            self.pinned = Some(block);
        }
        self.refresh(false);
        self.refresh_stats();
        // Ось x — время: блоки раскладки из выбранных.
        self.ask_day();
        self.refresh_time();
        self.remember();
    }

    /// Курсор над плиткой или ушёл с неё: в подсветке «под курсором» её линия — поверх остальных.
    pub fn tile_hover(&mut self, block: i32, on: bool) {
        let Ok(block) = usize::try_from(block) else { return };
        let hovered = if on {
            self.compared.contains(&block).then_some(block)
        } else if self.hovered == Some(block) {
            None
        } else {
            self.hovered
        };
        self.hover(hovered);
    }

    /// Курсор над полем графика (пиксели от его левого верхнего угла, поле w × h): ближайшая линия — подсвеченная.
    pub fn chart_pointer(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let near = self.near(x, y, w, h);
        self.hover(near);
    }

    pub fn chart_left(&mut self) {
        self.hover(None);
    }

    /// Щелчок по полю графика: в подсветке «закреплённая» ближайшая линия становится закреплённой.
    pub fn chart_click(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if let Some(block) = self.near(x, y, w, h)
            && self.pinned != Some(block)
        {
            self.pinned = Some(block);
            if self.pin_mode {
                self.refresh_stats();
            }
        }
    }

    pub fn mode_changed(&mut self, pin: bool) {
        if self.pin_mode != pin {
            self.pin_mode = pin;
            self.refresh_stats();
            self.remember();
        }
    }

    /// Вид графика: 0 — эффективность, 1 — над кристаллом, 2 — абсолютная (`Scale::of`).
    pub fn view_changed(&mut self, index: i32) {
        let scale = Scale::of(index);
        if self.scale != scale {
            self.scale = scale;
            self.refresh_stats();
            self.remember();
        }
    }

    /// «макс»: линию наибольших показать или скрыть; остальные — как были.
    pub fn max_toggled(&mut self) {
        self.max = !self.max;
        self.refresh_stats();
        self.remember();
    }

    /// «ø»: линии средних показать или скрыть; 95 % — как были (владелец 16.10: два независимых выключателя).
    pub fn mean_toggled(&mut self) {
        self.curves = self.curves.toggled_mean();
        self.refresh_stats();
        self.remember();
    }

    /// «95 %»: линии 95 % показать или скрыть; средние — как были.
    pub fn p95_toggled(&mut self) {
        self.curves = self.curves.toggled_p95();
        self.refresh_stats();
        self.remember();
    }

    /// Кривые модели нагрева: показать или скрыть (свой выключатель, вместе с линиями ø и 95 %).
    pub fn model_toggled(&mut self) {
        self.model = !self.model;
        self.refresh_stats();
        self.remember();
    }

    /// Оси местами: температура по горизонтали (true).
    pub fn axes_changed(&mut self, swap: bool) {
        if self.swap != swap {
            self.swap = swap;
            self.refresh_stats();
            self.refresh_time();
            self.show_ranges();
            self.remember();
        }
    }

    /// Линия под курсором: в «эффективности» — точки и линия между ними, в остальных видах — показанные линии (ø, 95 %,
    /// наибольшие) и кривая модели.
    fn near(&self, x: f32, y: f32, w: f32, h: f32) -> Option<usize> {
        let series = self.shown_stats.as_ref().map(|s| s.series.as_slice())?;
        let own = self.scale == Scale::Efficiency;
        chart::nearest_with(
            series,
            (f64::from(x), f64::from(y)),
            (f64::from(w), f64::from(h)),
            if own { Curves::Mean } else { self.curves },
            self.max && !own,
        )
    }

    // ----- ось x — время -----

    /// Ось x: множитель (false) или время (true).
    pub fn time_changed(&mut self, on: bool) {
        if self.timeline == on {
            return;
        }
        self.timeline = on;
        self.show_mode();
        self.ask_day();
        self.refresh_stats();
        self.refresh_time();
        self.remember();
    }

    /// Раскладка по времени: 0 — «блоки», 1 — «параметры».
    pub fn layout_changed(&mut self, layout: i32) {
        let layout = match layout {
            1 => Layout::Params,
            _ => Layout::Overlay,
        };
        if self.layout == layout {
            return;
        }
        self.layout = layout;
        self.show_mode();
        self.ask_day();
        self.refresh_time();
        self.remember();
    }

    /// Величина раскладки «блоки» (`Quantity::index`).
    pub fn quantity_changed(&mut self, quantity: i32) {
        let quantity = Quantity::of(quantity);
        if self.quantity == quantity {
            return;
        }
        self.quantity = quantity;
        self.selected = vec![quantity];
        self.range_unit = quantity.unit();
        self.show_mode();
        self.ask_day();
        self.refresh_time();
        self.remember();
    }

    /// Ось x, раскладка и величина — в окно (панель статистики от них не зависит, `refresh_stats` их не ставит).
    fn show_mode(&self) {
        let ui = self.ui();
        if ui.get_chart_time() != self.timeline {
            ui.set_chart_time(self.timeline);
        }
        let layout = match self.layout {
            Layout::Blocks => 0,
            Layout::Params => 1,
            Layout::Overlay => 2,
        };
        if ui.get_time_layout() != layout {
            ui.set_time_layout(layout);
        }
        if ui.get_time_quantity() != self.quantity.index() {
            ui.set_time_quantity(self.quantity.index());
        }
        if ui.get_events_all() != self.events_all {
            ui.set_events_all(self.events_all);
        }
        let labels = [
            "ядра: °C",
            "множитель",
            "потолок",
            "нагрузка %",
            "кристалл °C",
            "GPU °C",
            "GPU Вт",
            "GPU %",
            "CPU об/мин",
            "GPU об/мин",
        ];
        ui.set_metric_rows(ModelRc::new(VecModel::from(
            Quantity::ALL
                .iter()
                .zip(labels)
                .map(|(q, text)| crate::MetricChoice {
                    index: q.index(),
                    text: text.into(),
                    on: self.selected.contains(q),
                    tip: self.metric_tip(*q).into(),
                })
                .collect::<Vec<_>>(),
        )));
        self.show_ranges();
    }

    /// Дни со срезами (по возрастанию) и показанный: выбранный, если он ещё есть, иначе самый новый.
    fn days(&self) -> (&[String], Option<&str>) {
        let days = self.digest.as_ref().map_or(&[][..], |d| d.days.as_slice());
        let chosen = self
            .day
            .as_deref()
            .filter(|d| days.iter().any(|x| x == d))
            .or_else(|| days.last().map(String::as_str));
        (days, chosen)
    }

    /// Блоки по времени (номера): «блоки» — все выбранные, «параметры» — первые два.
    fn time_indices(&self) -> Vec<usize> {
        self.compared.clone() // выбранные номера, только при смене вида или запросе дня
    }

    /// Просьба к потоку статистики: день, блоки и их ряды, пока ось x — время; иначе день не нужен. Та же просьба
    /// дважды не идёт; другой день — показ заново (весь день), те же день и другие ряды — показ остаётся.
    fn ask_day(&mut self) {
        let wanted = if self.timeline {
            let (_, chosen) = self.days();
            let blocks: Vec<String> = self
                .time_indices()
                .iter()
                .filter_map(|&b| Some(self.hello.as_ref()?.blocks.get(b)?.name.clone()))
                .collect(); // up to twelve names per request
            let quantities = self.selected.clone(); // запрос владеет набором рядов до ответа потока
            chosen.map(|date| DayOrder {
                date: date.to_string(),
                blocks,
                quantities,
            })
        } else {
            None
        };
        if wanted == self.asked {
            return;
        }
        if wanted
            .as_ref()
            .is_none_or(|w| self.asked.as_ref().is_none_or(|a| a.date != w.date))
        {
            self.span = None;
            self.day_data = None;
        }
        self.asked.clone_from(&wanted);
        if let Some(stats) = &self.stats {
            // Поток кончился — дня не будет, вид скажет, что он читается.
            let _ = stats.send(Order::Day(wanted));
        }
    }

    /// ◀ / ▶: день раньше или позже среди дней со срезами; показ — весь день.
    pub fn day_step(&mut self, step: i32) {
        let (days, chosen) = self.days();
        let Some(at) = chosen.and_then(|c| days.iter().position(|d| d == c)) else {
            return;
        };
        let next = if step < 0 { at.checked_sub(1) } else { Some(at + 1) };
        let Some(date) = next.and_then(|k| days.get(k)).cloned() else {
            return;
        };
        self.time_follow = false;
        self.choose_day(date);
        self.ask_day();
        self.refresh_time();
    }

    /// Выбор дня: самый новый — «самый новый» (растёт с полуночью), не закреплённая дата.
    fn choose_day(&mut self, date: String) {
        let newest = self.digest.as_ref().and_then(|d| d.days.last());
        self.day = (Some(&date) != newest).then_some(date);
    }

    /// «весь день»: показ — от первого среза дня до последнего.
    pub fn day_whole(&mut self) {
        self.span = self.day_data.as_ref().and_then(|d| d.bounds());
        self.refresh_time();
    }

    /// Колесо над полем «по времени»: масштаб вокруг курсора (`timeline::zoom`).
    pub fn time_wheel(&mut self, _x: f32, _width: f32, delta: f32) {
        let (Some(day), Some(span)) = (self.day_data.as_ref(), self.span) else {
            return;
        };
        let Some(bounds) = day.bounds() else { return };
        self.span = Some(timeline::zoom(span, bounds, 1.0, f64::from(delta)));
        self.time_follow = self.span.is_some_and(|s| s.1 == bounds.1);
        self.remember_time_span();
        self.refresh_time();
        self.remember();
    }

    pub fn time_press(&mut self, x: f32) {
        self.drag_x = Some(x);
    }

    /// Перетаскивание по полю «по времени»: показ сдвигается вслед за мышью (`timeline::pan`).
    pub fn time_drag(&mut self, x: f32, width: f32) {
        let (Some(last), Some(day), Some(span)) = (self.drag_x, self.day_data.as_ref(), self.span) else {
            return;
        };
        let Some(bounds) = day.bounds() else { return };
        self.time_follow = false;
        self.span = Some(timeline::pan(
            span,
            bounds,
            f64::from(x - last) / f64::from(width.max(1.0)),
        ));
        self.time_follow = self.span.is_some_and(|s| s.1 == bounds.1);
        self.drag_x = Some(x);
        self.remember_time_span();
        self.refresh_time();
    }

    pub fn time_release(&mut self) {
        self.drag_x = None;
        self.remember();
    }

    /// Подсказка под курсором поля «по времени» (курсор и ширина поля — точки).
    pub fn time_hover(&self, x: f32, width: f32) -> String {
        let (Some(day), Some((from, to))) = (self.day_data.as_ref(), self.span) else {
            return String::new();
        };
        let share = f64::from(x) / f64::from(width.max(1.0));
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "milliseconds of a day"
        )]
        let at = from + (share * (to - from) as f64).round() as i64;
        // «Рядом» — 6 точек поля: отметка события или перерыва под курсором.
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "milliseconds of a day"
        )]
        let near = (6.0 / f64::from(width.max(1.0)) * (to - from) as f64).round() as i64;
        let (labels, colours) = self.time_blocks();
        let shown = shown_blocks(&labels, &colours);
        timeline::hover(
            day,
            &Under {
                x: at,
                near,
                layout: self.layout,
                quantity: self.quantity,
                selected: Some(&self.selected),
                blocks: &shown,
                events: &self.time_acts(),
            },
        )
    }

    /// Блоки по времени: подписи и цвета.
    fn time_blocks(&self) -> (Vec<String>, Vec<u32>) {
        let Some(hello) = self.hello.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        self.time_indices()
            .iter()
            .filter_map(|&b| {
                let block = hello.blocks.get(b)?;
                Some((panels::capital(&panels::whom(block)), chart::color_of(&hello.blocks, b)))
            })
            .unzip()
    }

    /// События показанного дня для оси времени: блоков на графике (с номером среди них — отметкой на линии блока) и без
    /// блока (сброс, пробуждение — отметкой под полосами).
    fn time_acts(&self) -> Vec<Act> {
        let (Some(digest), Some(hello), Some(day)) =
            (self.digest.as_ref(), self.hello.as_ref(), self.day_data.as_ref())
        else {
            return Vec::new();
        };
        let (Some(&first), Some(&last)) = (day.t.first(), day.t.last()) else {
            return Vec::new();
        };
        let indices = self.time_indices();
        let names: Vec<&str> = indices
            .iter()
            .filter_map(|&b| Some(hello.blocks.get(b)?.name.as_str()))
            .collect();
        events::listed(&digest.events, &names, false)
            .into_iter()
            .filter_map(|(i, which)| {
                let e = &digest.events[i];
                let at = e.at.filter(|t| (first..=last).contains(t))?;
                Some(Act {
                    at,
                    color: e.kind.color(),
                    cut: e.kind.cuts(),
                    block: which,
                    tip: event_line(e),
                })
            })
            .collect()
    }

    /// Полосы оси времени заново (день, показ, раскладка, блоки, отсечки, события); в окно — только изменившееся.
    #[expect(clippy::too_many_lines, reason = "the time view's properties, one after another")]
    pub fn refresh_time(&mut self) {
        if !self.timeline {
            return;
        }
        let ui = self.ui();
        let (days, chosen) = self.days();
        let at = chosen.and_then(|c| days.iter().position(|d| d == c));
        let day_text = chosen.map(timeline::short_date).unwrap_or_default();
        set_text(&ui.get_day_text(), &day_text, |t| ui.set_day_text(t));
        let (prev, next) = (at.is_some_and(|k| k > 0), at.is_some_and(|k| k + 1 < days.len()));
        if ui.get_can_day_prev() != prev {
            ui.set_can_day_prev(prev);
        }
        if ui.get_can_day_next() != next {
            ui.set_can_day_next(next);
        }
        let shown = match (self.day_data.as_ref(), self.span) {
            _ if self.compared.is_empty() => TimeShown {
                empty: "Щёлкни по плитке — её день появится здесь.".into(),
                ..TimeShown::default()
            },
            _ if chosen.is_none() => TimeShown {
                empty: "Срезов пока нет — их пишет CoreMapTray, пока включена запись.".into(),
                ..TimeShown::default()
            },
            // Прочитан ли день, о котором просили (тот же день, те же блоки и ряды) — иначе он ещё читается.
            (Some(day), Some(span)) if self.asked.as_ref().is_some_and(|a| answers(a, day)) => {
                let (labels, colours) = self.time_blocks();
                let blocks = shown_blocks(&labels, &colours);
                // Картинки полос — в пикселях экрана: столько столбцов прореживания, сколько их видно.
                let scale = f64::from(ui.window().scale_factor());
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a plot size in pixels, a few thousand"
                )]
                let (columns, height) = (
                    (f64::from(ui.get_time_plot_w().max(100.0)) * scale).round() as usize,
                    (f64::from(ui.get_time_plot_h().max(40.0)) * scale).round() as usize,
                );
                timeline::time_panel(&TimeInput {
                    day,
                    view: span,
                    layout: self.layout,
                    quantity: self.quantity,
                    normalized: self.normalized,
                    parameter_ranges: Some(&self.parameter_ranges()),
                    selected: Some(&self.selected),
                    manual_ranges: Some(&self.time_ranges()),
                    swap: self.swap,
                    columns,
                    height,
                    scale,
                    blocks: &blocks,
                    limit: self.cutoff(),
                    emergency: self.emergency(),
                    events: &self.time_acts(),
                })
            }
            _ => TimeShown {
                empty: "День читается…".into(),
                ..TimeShown::default()
            },
        };
        if self.shown_time.as_ref() == Some(&shown) {
            return;
        }
        set_text(&ui.get_time_head(), &shown.head, |t| ui.set_time_head(t));
        set_text(&ui.get_time_empty(), &shown.empty, |t| ui.set_time_empty(t));
        let old = self.shown_time.as_ref();
        if old.is_none_or(|o| o.strips != shown.strips) {
            let strips: Vec<TimeStrip> = shown
                .strips
                .iter()
                .map(|s| TimeStrip {
                    title: s.title.as_str().into(),
                    ticks: ModelRc::new(VecModel::from(tick_rows(&s.ticks))),
                    right_ticks: ModelRc::new(VecModel::from(tick_rows(&s.right_ticks))),
                    picture: Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                        &s.picture.rgba,
                        s.picture.width,
                        s.picture.height,
                    )),
                    refs: ModelRc::new(VecModel::from(
                        s.refs
                            .iter()
                            .map(|r| TimeRef {
                                f: r.f,
                                color: panels::rgb(r.color),
                                text: r.text.as_str().into(),
                            })
                            .collect::<Vec<_>>(),
                    )),
                })
                .collect(); // up to seven strips, per zoom or pan
            ui.set_time_strips(ModelRc::new(VecModel::from(strips)));
        }
        if old.is_none_or(|o| o.ticks != shown.ticks) {
            ui.set_time_ticks(ModelRc::new(VecModel::from(tick_rows(&shown.ticks))));
        }
        if old.is_none_or(|o| o.marks != shown.marks) {
            ui.set_time_marks(ModelRc::new(VecModel::from(
                shown
                    .marks
                    .iter()
                    .map(|m| TimeMark {
                        f: m.f,
                        color: panels::rgb(m.color),
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        if old.is_none_or(|o| o.dots != shown.dots) {
            ui.set_time_dots(ModelRc::new(VecModel::from(
                shown
                    .dots
                    .iter()
                    .map(|d| TimeDot {
                        x: d.x,
                        y: d.y,
                        color: panels::rgb(d.color),
                        kind: d.kind,
                    })
                    .collect::<Vec<_>>(),
            )));
        }
        if old.is_none_or(|o| o.gaps != shown.gaps) {
            ui.set_time_gaps(ModelRc::new(VecModel::from(shown.gaps.clone()))); // a few gap marks
        }
        self.shown_time = Some(shown);
        self.show_ranges();
    }

    // ----- события -----

    /// «все блоки» (true) или «выбранные блоки» в списке событий.
    pub fn events_filter(&mut self, all: bool) {
        if self.events_all == all {
            return;
        }
        self.events_all = all;
        self.show_mode();
        self.refresh_events();
        self.remember();
    }

    /// Список событий под графиком: новые первыми, все блоки или выбранные; в окно — только когда он другой.
    fn refresh_events(&mut self) {
        let (Some(hello), Some(digest)) = (self.hello.as_ref(), self.digest.as_ref()) else {
            return;
        };
        let key = (
            Arc::as_ptr(&digest.events) as usize,
            self.compared.clone(),
            self.events_all,
        ); // a dozen numbers
        if self.events_key.as_ref() == Some(&key) {
            return;
        }
        let names: Vec<&str> = self
            .compared
            .iter()
            .filter_map(|&b| Some(hello.blocks.get(b)?.name.as_str()))
            .collect();
        let listed = events::listed(&digest.events, &names, self.events_all);
        // Строк в окне — не больше `EVENT_ROWS` (каждая — несколько элементов Slint); старше — только на графике.
        let rows: Vec<LineRow> = listed
            .iter()
            .take(EVENT_ROWS)
            .map(|&(i, which)| {
                let e = &digest.events[i];
                let mark = which
                    .and_then(|w| self.compared.get(w))
                    .map_or(0x0035_3a44, |&b| chart::color_of(&hello.blocks, b));
                LineRow {
                    time: e.time.as_str().into(),
                    head: match &e.block {
                        Some(b) => format!("{b} {}", e.change).trim_end().into(),
                        None => e.change.as_str().into(),
                    },
                    text: e.why.as_str().into(),
                    more: e.details.as_str().into(),
                    color: panels::rgb(e.kind.color()),
                    mark: panels::rgb(mark),
                }
            })
            .collect();
        let empty = if digest.events.is_empty() {
            "событий пока нет: их пишет CoreMapTray с 0.5.6 (отсечки, профили, правки потолков, сбросы)"
        } else if self.events_all {
            "событий нет"
        } else {
            "с выбранными блоками событий не было — «все блоки» покажет остальные"
        };
        let ui = self.ui();
        set_text(&ui.get_events_empty(), empty, |t| ui.set_events_empty(t));
        ui.set_events(ModelRc::new(VecModel::from(rows)));
        self.listed = listed.into_iter().map(|(i, _)| i).take(EVENT_ROWS).collect();
        self.events_key = Some(key);
    }

    /// Щелчок по строке списка событий: ось x — время, день события, показ с моментом события посередине (уже был
    /// узкий показ того же дня — той же ширины, иначе `CENTRE_SPAN`).
    pub fn event_clicked(&mut self, row: i32) {
        let Some(&i) = usize::try_from(row).ok().and_then(|r| self.listed.get(r)) else {
            return;
        };
        let Some(e) = self.digest.as_ref().and_then(|d| d.events.get(i)) else {
            return;
        };
        let (Some(at), Some(date)) = (e.at, e.date.clone()) else {
            return;
        };
        let has_samples = self.digest.as_ref().is_some_and(|d| d.days.contains(&date));
        if !self.timeline {
            self.timeline = true;
            self.show_mode();
        }
        if has_samples {
            self.choose_day(date.clone()); // one date
            self.centre = Some((date, at));
        }
        self.ask_day();
        self.apply_centre();
        self.refresh_stats();
        self.refresh_time();
        self.remember();
    }

    /// Время события первой строки списка (скрытой проверке).
    pub fn first_event_time(&self) -> Option<i64> {
        let i = *self.listed.first()?;
        self.digest.as_ref()?.events.get(i)?.at
    }

    /// Ждущий центр события — на показ, когда его день прочитан.
    fn apply_centre(&mut self) {
        let (Some((date, at)), Some(day)) = (self.centre.as_ref(), self.day_data.as_ref()) else {
            return;
        };
        if self.asked.as_ref().is_none_or(|a| &a.date != date) {
            // Просят уже другой день: центр не нужен.
            self.centre = None;
            return;
        }
        if &day.date != date || !self.asked.as_ref().is_some_and(|a| answers(a, day)) {
            return;
        }
        let (Some(bounds), Some(x)) = (day.bounds(), day.place(*at)) else {
            self.centre = None;
            return;
        };
        let width = match self.span {
            Some(s) if s != bounds => s.1 - s.0,
            _ => CENTRE_SPAN,
        };
        self.time_follow = false;
        self.span = Some(timeline::centred(x, width, bounds));
        self.remember_time_span();
        self.centre = None;
    }

    fn hover(&mut self, block: Option<usize>) {
        if self.hovered != block {
            self.hovered = block;
            if !self.pin_mode {
                self.refresh_stats();
            }
        }
    }

    pub fn reset_max(&mut self) {
        self.order("max.reset", Map::new(), "Максимумы не сброшены");
    }

    /// Правая кнопка: меню потолка; что предлагать, решает движок.
    pub fn menu(&mut self, block: i32) {
        let ui = self.ui();
        let in_profile = |first: usize| self.chosen_ratios().and_then(|r| r.get(first).copied());
        let found = self
            .hello
            .as_ref()
            .zip(self.limits.as_ref())
            .and_then(|(hello, limits)| {
                let b = usize::try_from(block).ok().and_then(|i| hello.blocks.get(i))?;
                let i = b.first;
                Some((b, limits.choices.get(b.index)?, *limits.fused.get(i)?, in_profile(i)))
            });
        let Some((b, choices, fused, profile)) = found else {
            ui.set_menu_entries(ModelRc::default());
            self.menu_block = None;
            return;
        };
        let current = self
            .state
            .as_ref()
            .and_then(|s| s.limit.as_ref())
            .and_then(|l| l.get(b.first).copied());
        let entries: Vec<MenuEntry> = choices
            .iter()
            .map(|&v| {
                // Значки вместо слов (владелец 15.10): ◆ — заводской предел, ○ — в выбранном профиле, красным — выше
                // заводского; расшифровка — строкой под заголовком меню. ↺ шрифт окна не рисует (снимок проверки 15.10).
                let marks: Vec<&str> = [(v == fused, "◆"), (Some(v) == profile, "○")]
                    .into_iter()
                    .filter_map(|(on, mark)| on.then_some(mark))
                    .collect();
                MenuEntry {
                    ratio: slint_int(v),
                    text: format!("×{v}").into(),
                    hint: marks.join(" ").into(),
                    checked: Some(v) == current,
                    above: v.cmp(&fused) == Ordering::Greater,
                }
            })
            .collect();
        ui.set_menu_title(
            format!(
                "Потолок множителя: {}{}",
                if b.kind == "E" {
                    "кластер ядра "
                } else {
                    "ядро "
                },
                b.lp
            )
            .into(),
        );
        ui.set_menu_entries(ModelRc::new(VecModel::from(entries)));
        self.menu_block = Some(b.index);
    }

    pub fn limit_chosen(&mut self, ratio: i32) {
        let Some(block) = self.menu_block.take() else { return };
        let mut args = Map::new();
        args.insert("block".into(), block.into());
        args.insert("ratio".into(), ratio.into());
        self.order("limit.set", args, "Потолок не изменён");
    }

    /// Щелчок по строке под верхней полосой: убрать отказ; сообщение программы — прочитано (notices.seen: новому
    /// окну оно уже не придёт).
    pub fn problem_dismissed(&mut self) {
        if self.problem.take().is_none()
            && let Some((seq, _)) = self.notice.take()
        {
            let mut args = Map::new();
            args.insert("seq".into(), seq.into());
            self.order("notices.seen", args, "Сообщение не отмечено прочитанным");
        }
        self.show_problem();
    }

    pub fn set_clicked(&mut self, row: i32) {
        let key = usize::try_from(row)
            .ok()
            .and_then(|i| self.shown_profiles.as_ref()?.rows.get(i))
            .map(|r| r.key.clone()); // one name per click
        // Выбор ничего не ставит и ничего не шлёт (владелец 15.10: «когда… профиль выбираешь он автоматически не
        // применяется»): меняется только то, с чем сравнивается текущее состояние — строка панели и плитки.
        if key.is_some() && key != self.chosen {
            self.chosen = key;
            self.refresh_profiles();
            self.refresh(false);
        }
    }

    fn named(name: &str) -> Map<String, Value> {
        let mut args = Map::new();
        args.insert("name".into(), name.into());
        args
    }

    /// «Применить»: потолки выбранного профиля — в процессор (команда profile.open; «» — заводские).
    pub fn profile_apply(&mut self) {
        if let Some(name) = self.chosen.clone() {
            self.order("profile.open", Self::named(&name), "Профиль не применён");
        }
    }

    pub fn profile_save(&mut self) {
        if let Some(name) = self.chosen.clone().filter(|n| !n.is_empty()) {
            self.order("profile.save", Self::named(&name), "Профиль не сохранён");
        }
    }

    pub fn profile_save_as(&mut self) {
        let name = self.ui().get_prof_name().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.order_then(
            "profile.save",
            Self::named(&name),
            Pending {
                failure: "Профиль не сохранён",
                saved_as: Some(name),
                held: false,
            },
        );
    }

    /// «Загружать при старте Windows» у выбранного сохранённого профиля: отметить — он ставится при входе в Windows (и
    /// только он); снять у отмеченного — при входе ничего не ставится.
    pub fn startup_toggled(&mut self) {
        let Some(name) = self.chosen.clone().filter(|n| !n.is_empty()) else {
            return;
        };
        let on = !self.shown_profiles.as_ref().is_some_and(|p| p.startup_on);
        let mut args = Self::named(&name);
        args.insert("on".into(), on.into());
        self.order("profile.startup", args, "Загрузка при старте Windows не изменена");
    }

    pub fn profile_delete(&mut self) {
        if let Some(name) = self.chosen.clone().filter(|n| !n.is_empty()) {
            self.order("profile.delete", Self::named(&name), "Профиль не удалён");
        }
    }

    pub fn name_edited(&mut self) {
        self.refresh_profiles();
    }

    /// Строка плитки, как она показана сейчас (скрытой проверке).
    pub fn shown(&self, block: usize) -> Option<TileData> {
        (0..self.tiles.row_count())
            .filter_map(|i| self.tiles.row_data(i))
            .find(|t| t.block == slint_int(block))
    }

    /// Наборы, как они показаны в списке (скрытой проверке): ключ ("" — заводские) по порядку.
    pub fn set_keys(&self) -> Vec<String> {
        self.shown_profiles
            .as_ref()
            .map(|p| p.rows.iter().map(|r| r.key.clone()).collect())
            .unwrap_or_default()
    }
}

/// Одна и та же ли строка плитки. Сравнение Slint для моделей внутри строки — по адресу, а клетки ядер строятся
/// заново на каждое состояние; поэтому они сравниваются по содержимому.
#[expect(
    clippy::float_cmp,
    reason = "the same computation on the same input gives the same bits: unchanged"
)]
fn same(a: &TileData, b: &TileData) -> bool {
    let cells = |t: &TileData| {
        t.cells
            .iter()
            .map(|c| (c.text, c.temp, c.fill, c.num_color, c.col, c.row))
            .collect::<Vec<_>>()
    };
    a.block == b.block
        && a.x == b.x
        && a.y == b.y
        && a.w == b.w
        && a.h == b.h
        && a.cluster == b.cluster
        && a.number == b.number
        && a.number_color == b.number_color
        && a.peak == b.peak
        && a.mean == b.mean
        && a.mean_ratio == b.mean_ratio
        && a.temp == b.temp
        && a.fill == b.fill
        && a.pair == b.pair
        && a.pair_color == b.pair_color
        && a.delta == b.delta
        && a.auto == b.auto
        && a.ban == b.ban
        && a.selected == b.selected
        && a.outline == b.outline
        && a.strip == b.strip
        && a.tip == b.tip
        && cells(a) == cells(b)
}

/// Ставит строку, только если она другая. 1 — поставлена, 0 — та же.
fn set_text(current: &SharedString, text: &str, set: impl FnOnce(SharedString)) -> u32 {
    if current.as_str() == text {
        return 0;
    }
    set(text.into());
    1
}
