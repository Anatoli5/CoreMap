// Что окно показывает: из сообщений движка (или программы входа) и сводки статистики — строки и цвета для
// ui/app.slint; из щелчков — команды. Правила показа плиток — те же, что у прежнего окна (DieView в src/Ui.cs).
//
// Плитка — строка модели. Пришло состояние — для каждой плитки считается новая строка, и заменяется только та, что
// отличается от показанной: Slint перерисовывает только её. Панели профилей и статистики пересчитываются только от
// того, что они показывают (профили, потолки, надстройка автомодуля, выбор; сводка статистики, выбранный блок), и
// свойства окна ставятся, только если значение другое.
//
// Жесты (владелец 15.10: «по две функции на одно действие… так не делается»): щелчок по плитке — добавить блок в
// статистику или убрать его оттуда (сравнение до четырёх блоков, владелец 15.10), и только; правая кнопка — меню
// потолка. Нагрузки у окна нет.
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{Map, Value};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::die::{self, Geometry};
use crate::panels::{self, ProfileInput, ProfileShown, StatsShown};
use crate::pipe::{Event, Link};
use crate::store::Digest;
use crate::wire::{Hello, Limits, Message, Profiles, State};
use crate::{AppWindow, CellData, LineRow, LinkData, MenuEntry, SetRow, TileData};

const TEXT: [u8; 3] = [0xe6, 0xe8, 0xec];
const ACCENT: [u8; 3] = [0xff, 0xd1, 0x66];
const BAD: [u8; 3] = [0xff, 0x5d, 0x5d];
const CELL: [u8; 3] = [0x2a, 0x2e, 0x36];
/// Полоска плитки, пока ▲ наибольшей ещё нет.
const NO_PEAK: [u8; 3] = [0x35, 0x3a, 0x44];

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
}

#[expect(
    missing_debug_implementations,
    reason = "holds Slint handles that have no Debug; nothing prints a View"
)]
pub struct View {
    ui: slint::Weak<AppWindow>,
    link: Arc<Link>,
    pub hello: Option<Hello>,
    pub limits: Option<Limits>,
    pub state: Option<State>,
    pub profiles: Option<Profiles>,
    /// Тема «history» программы входа; None — её не было, тогда история — из data\profile-log.jsonl.
    history: Option<Vec<Map<String, Value>>>,
    pub digest: Option<Arc<Digest>>,
    geometry: Geometry,
    tiles: Rc<VecModel<TileData>>,
    sets: Rc<VecModel<SetRow>>,
    /// Блоки в статистике по порядку добавления: (номер блока, номер цвета в `panels::SERIES`).
    pub compared: Vec<(usize, usize)>,
    /// Подсветка линий (владелец 15.10 не решил, какая удобнее, — переключатель в окне): false — под курсором (плитка
    /// или линия графика), true — закреплённая: последний добавленный блок или линия, по которой щёлкнули.
    pub pin_mode: bool,
    pub pinned: Option<usize>,
    pub hovered: Option<usize>,
    /// Выбранная строка списка наборов: "" — заводские; None — ничего.
    chosen: Option<String>,
    shown_profiles: Option<ProfileShown>,
    pub shown_stats: Option<StatsShown>,
    shown_links: Vec<(LinkData, f64)>,
    shown_history: Vec<(String, String)>,
    refused: Option<String>,
    up: bool,
    why: String,
    problem: Option<String>,
    /// Последнее сообщение владельцу от программы (notice): номер и текст; щелчок по строке — прочитано.
    notice: Option<(i64, String)>,
    pending: HashMap<i64, Pending>,
    menu_block: Option<usize>,
    pub changes: Changes,
}

impl View {
    pub fn new(ui: &AppWindow, link: Arc<Link>) -> View {
        let tiles = Rc::new(VecModel::default());
        ui.set_tiles(ModelRc::from(Rc::clone(&tiles)));
        let sets = Rc::new(VecModel::default());
        ui.set_sets(ModelRc::from(Rc::clone(&sets)));
        let view = View {
            ui: ui.as_weak(),
            link,
            hello: None,
            limits: None,
            state: None,
            profiles: None,
            history: None,
            digest: None,
            geometry: Geometry::default(),
            tiles,
            sets,
            compared: Vec::new(),
            pin_mode: false,
            pinned: None,
            hovered: None,
            chosen: None,
            shown_profiles: None,
            shown_stats: None,
            shown_links: Vec::new(),
            shown_history: Vec::new(),
            refused: None,
            up: false,
            why: "окно запускается".into(),
            problem: None,
            notice: None,
            pending: HashMap::new(),
            menu_block: None,
            changes: Changes::default(),
        };
        view.show_link();
        view
    }

    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::Up => {
                self.up = true;
                self.refused = None;
                self.show_link();
            }
            Event::Down(why) => {
                self.up = false;
                self.why = why;
                self.pending.clear();
                self.show_link();
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
                let keep = self
                    .chosen
                    .as_deref()
                    .is_some_and(|c| c.is_empty() || profiles.sets.iter().any(|s| s.name == c));
                if !keep {
                    self.chosen = profiles.standing.clone().or_else(|| profiles.was_named.clone());
                }
                self.profiles = Some(profiles);
                self.refresh_profiles();
            }
            Event::Message(Message::History(history)) => {
                self.history = Some(history.entries);
                self.refresh_history();
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
                    if reply.ok
                        && let Some(name) = pending.saved_as
                    {
                        self.ui().set_prof_name(SharedString::new());
                        self.chosen = Some(name);
                        self.refresh_profiles();
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
            Event::Message(Message::Other) => {}
        }
    }

    /// Новая сводка статистики (поток store.rs).
    pub fn on_digest(&mut self, digest: Arc<Digest>) {
        self.digest = Some(digest);
        self.refresh_stats();
        self.refresh_links();
        if self.history.is_none() {
            self.refresh_history();
        }
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
        // Выбор переживает новое знакомство, пока такие блоки есть; пусто — первое P-ядро.
        self.compared.retain(|&(b, _)| b < hello.blocks.len());
        self.pinned = self.pinned.filter(|&b| b < hello.blocks.len());
        self.hovered = None;
        if self.compared.is_empty()
            && let Some(first) = hello
                .blocks
                .iter()
                .find(|b| b.kind == "P")
                .or_else(|| hello.blocks.first())
        {
            self.compared.push((first.index, 0));
            self.pinned = Some(first.index);
        }
        self.hello = Some(hello);
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
        // Панель профилей зависит от потолков, надстройки автомодуля и хода измерения — больше ни от чего в состоянии.
        let profiles_moved = self
            .state
            .as_ref()
            .is_none_or(|old| old.limit != state.limit || old.auto != state.auto || old.busy != state.busy);
        // Статистика зависит от состояния только отсечкой автомодуля (линия на графике).
        let cutoff_moved = self.state.as_ref().is_none_or(|old| old.auto_limit != state.auto_limit);
        self.state = Some(state);
        self.show_problem();
        self.refresh(false);
        if profiles_moved {
            self.refresh_profiles();
        }
        if cutoff_moved {
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
        if let Some(&(_, slot)) = self.compared.iter().find(|(b, _)| *b == block.index) {
            data.selected = true;
            data.outline = panels::rgb(panels::SERIES[slot % panels::COMPARE]);
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
            self.limits.as_ref(),
            panels::Overlay::of(Some(s)),
        );
        let ratio = highest(&s.ratio, members);
        let fused = self.limits.as_ref().and_then(|l| l.fused.get(first).copied());
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
            data.delta = panels::signed(c.owner_delta()).into();
        }
        if c.auto != 0 {
            data.auto = format!("авто {}", panels::signed(c.auto)).into();
        }
        data.ban = c.ban.is_some();
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
        })
        .into();
        Some(data)
    }

    // ----- перемычки соседей -----

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

    fn refresh_stats(&mut self) {
        let Some(hello) = self.hello.as_ref() else { return };
        let shown = panels::stats_panel(&panels::StatsInput {
            blocks: &hello.blocks,
            compared: &self.compared,
            highlight: self.highlight(),
            fused: self.limits.as_ref().map(|l| l.fused.as_slice()),
            cutoff: self.state.as_ref().and_then(|s| s.auto_limit),
            digest: self.digest.as_deref(),
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
        set_text(&ui.get_actions_empty(), &shown.actions_empty, |t| {
            ui.set_actions_empty(t);
        });
        // Та же величина из того же расчёта — те же биты.
        if ui.get_chart_cutoff().to_bits() != shown.cutoff.to_bits() {
            ui.set_chart_cutoff(shown.cutoff);
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
        if old.is_none_or(|o| o.yticks != shown.yticks) {
            ui.set_chart_yticks(ModelRc::new(VecModel::from(shown.ticks())));
        }
        if old.is_none_or(|o| o.legend != shown.legend) {
            ui.set_stats_legend(ModelRc::new(VecModel::from(shown.legend_rows())));
        }
        if old.is_none_or(|o| o.actions != shown.actions) {
            ui.set_actions(ModelRc::new(VecModel::from(shown.action_rows())));
        }
        self.shown_stats = Some(shown);
    }

    // ----- профили -----

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
        set_text(&ui.get_prof_was(), &shown.was, |t| ui.set_prof_was(t));
        set_text(&ui.get_prof_now(), &shown.now, |t| ui.set_prof_now(t));
        set_text(&ui.get_save_tip(), &shown.save_tip, |t| ui.set_save_tip(t));
        set_text(&ui.get_save_as_text(), &shown.save_as_text, |t| ui.set_save_as_text(t));
        set_text(&ui.get_save_note(), &shown.save_note, |t| ui.set_save_note(t));
        let flags = [
            (
                ui.get_prof_attention(),
                shown.attention,
                AppWindow::set_prof_attention as fn(&AppWindow, bool),
            ),
            (ui.get_can_open(), shown.can_open, AppWindow::set_can_open),
            (ui.get_can_save(), shown.can_save, AppWindow::set_can_save),
            (ui.get_can_save_as(), shown.can_save_as, AppWindow::set_can_save_as),
            (ui.get_can_accept(), shown.can_accept, AppWindow::set_can_accept),
            (ui.get_can_revert(), shown.can_revert, AppWindow::set_can_revert),
            (ui.get_can_delete(), shown.can_delete, AppWindow::set_can_delete),
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

    fn refresh_history(&mut self) {
        let entries = self
            .history
            .as_deref()
            .or_else(|| self.digest.as_ref().and_then(|d| d.history.as_deref()));
        let rows: Vec<LineRow> = entries
            .map(|e| panels::history_rows(e, self.hello.as_ref()))
            .unwrap_or_default();
        let key: Vec<(String, String)> = rows.iter().map(|r| (r.time.to_string(), r.text.to_string())).collect();
        let ui = self.ui();
        let has = entries.is_some();
        if ui.get_has_history() != has {
            ui.set_has_history(has);
        }
        if key != self.shown_history {
            ui.set_history(ModelRc::new(VecModel::from(rows)));
            self.shown_history = key;
        }
    }

    /// Строк истории, как они показаны (скрытой проверке).
    pub fn history_shown(&self) -> usize {
        self.shown_history.len()
    }

    // ----- команды владельца -----

    pub fn order(&mut self, command: &str, args: Map<String, Value>, failure: &'static str) {
        self.order_then(command, args, failure, None);
    }

    fn order_then(&mut self, command: &str, args: Map<String, Value>, failure: &'static str, saved_as: Option<String>) {
        // На эти команды программа может спросить владельца своим окошком (потолок выше заводского): пусть оно выйдет
        // на передний план, а не мигает под окном.
        if matches!(command, "limit.set" | "profile.open" | "profile.revert") {
            self.link.allow_foreground();
        }
        if let Some(id) = self.link.ask(command, args) {
            self.pending.insert(id, Pending { failure, saved_as });
        } else {
            self.problem = Some(format!("{failure}: нет связи с программой у часов"));
            self.show_problem();
        }
    }

    /// Щелчок по плитке: блока нет в статистике — добавить (до `COMPARE` блоков, у каждого свой цвет), есть — убрать.
    /// Больше щелчок ничего не делает.
    pub fn clicked(&mut self, block: i32) {
        let Ok(block) = usize::try_from(block) else { return };
        if let Some(at) = self.compared.iter().position(|(b, _)| *b == block) {
            self.compared.remove(at);
            if self.pinned == Some(block) {
                self.pinned = self.compared.last().map(|(b, _)| *b);
            }
            if self.hovered == Some(block) {
                self.hovered = None;
            }
        } else if self.compared.len() >= panels::COMPARE {
            self.problem = Some(format!(
                "Сравнить можно до {} блоков — сначала убери один щелчком по его плитке",
                panels::COMPARE
            ));
            self.show_problem();
            return;
        } else {
            // Цвет — первый свободный: у оставшихся блоков цвета не меняются.
            let slot = (0..panels::COMPARE)
                .find(|s| self.compared.iter().all(|(_, used)| used != s))
                .unwrap_or(0);
            self.compared.push((block, slot));
            self.pinned = Some(block);
        }
        self.refresh(false);
        self.refresh_stats();
    }

    /// Курсор над плиткой или ушёл с неё: в подсветке «под курсором» её линия — поверх остальных.
    pub fn tile_hover(&mut self, block: i32, on: bool) {
        let Ok(block) = usize::try_from(block) else { return };
        let hovered = if on {
            self.compared.iter().any(|(b, _)| *b == block).then_some(block)
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
        }
    }

    fn near(&self, x: f32, y: f32, w: f32, h: f32) -> Option<usize> {
        let series = self.shown_stats.as_ref().map(|s| s.series.as_slice())?;
        panels::nearest(series, (f64::from(x), f64::from(y)), (f64::from(w), f64::from(h)))
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
        let found = self
            .hello
            .as_ref()
            .zip(self.limits.as_ref())
            .and_then(|(hello, limits)| {
                let b = usize::try_from(block).ok().and_then(|i| hello.blocks.get(i))?;
                let i = b.first;
                Some((
                    b,
                    limits.choices.get(b.index)?,
                    *limits.fused.get(i)?,
                    limits.was.get(i).copied(),
                ))
            });
        let Some((b, choices, fused, was)) = found else {
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
                // Значки вместо слов (владелец 15.10): ◆ — заводской предел, ○ — «было», красным — выше заводского;
                // расшифровка — строкой под заголовком меню. ↺ шрифт окна не рисует (снимок проверки 15.10).
                let marks: Vec<&str> = [(v == fused, "◆"), (Some(v) == was, "○")]
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
        if key.is_some() && key != self.chosen {
            self.chosen = key;
            self.refresh_profiles();
        }
    }

    fn named(name: &str) -> Map<String, Value> {
        let mut args = Map::new();
        args.insert("name".into(), name.into());
        args
    }

    pub fn profile_open(&mut self) {
        if let Some(name) = self.chosen.clone() {
            self.order("profile.open", Self::named(&name), "Профиль не открыт");
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
        self.order_then("profile.save", Self::named(&name), "Профиль не сохранён", Some(name));
    }

    pub fn profile_accept(&mut self) {
        self.order("profile.accept", Map::new(), "Не принято");
    }

    pub fn profile_revert(&mut self) {
        self.order("profile.revert", Map::new(), "Не откачено");
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
