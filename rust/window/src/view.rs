// Что окно показывает: из сообщений движка — строки и цвета для ui/app.slint; из щелчков — команды движку.
// Правила показа — те же, что у прежнего окна (DieView в src/Ui.cs C#-части).
//
// Плитка — строка модели. Пришло состояние — для каждой плитки считается новая строка, и заменяется только та,
// что отличается от показанной: Slint перерисовывает только её. Строки верхней полосы — так же.
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{Map, Value};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::pipe::{Event, Link};
use crate::wire::{Hello, Limits, Message, State};
use crate::{AppWindow, MenuEntry, NumData, TileData};

const TEXT: [u8; 3] = [0xe6, 0xe8, 0xec];
const ACCENT: [u8; 3] = [0xff, 0xd1, 0x66];
const BAD: [u8; 3] = [0xff, 0x5d, 0x5d];
const CELL: [u8; 3] = [0x2a, 0x2e, 0x36];
const P_CORE: [u8; 3] = [0xf0, 0xb8, 0x60];
const E_CORE: [u8; 3] = [0x6f, 0xd0, 0xc8];

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
    tiles: Rc<VecModel<TileData>>,
    selected: i32,
    refused: Option<String>,
    up: bool,
    why: String,
    problem: Option<String>,
    pending: HashMap<i64, &'static str>,
    menu_block: Option<usize>,
    pub changes: Changes,
}

impl View {
    pub fn new(ui: &AppWindow, link: Arc<Link>) -> View {
        let tiles = Rc::new(VecModel::default());
        ui.set_tiles(ModelRc::from(Rc::clone(&tiles)));
        let view = View {
            ui: ui.as_weak(),
            link,
            hello: None,
            limits: None,
            state: None,
            tiles,
            selected: -1,
            refused: None,
            up: false,
            why: "окно запускается".into(),
            problem: None,
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
            }
            Event::Message(Message::Reply(reply)) => {
                if reply.re == 0 {
                    if !reply.ok {
                        self.refused = Some(reply.why.unwrap_or_else(|| "движок отказал".into()));
                        self.show_link();
                    }
                } else if let Some(failure) = self.pending.remove(&reply.re) {
                    self.problem = if reply.ok {
                        None
                    } else {
                        Some(format!("{failure}: {}", reply.why.unwrap_or_default()))
                    };
                    self.show_problem();
                }
            }
            Event::Message(Message::Other) => {}
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

    fn show_problem(&self) {
        let ui = self.ui();
        let state = self.state.as_ref();
        let text = self
            .problem
            .clone()
            .or_else(|| state.and_then(|s| s.sensor_error.clone()))
            .or_else(|| state.and_then(|s| s.trip.clone()))
            .unwrap_or_default();
        set_text(&ui.get_problem(), &text, |t| ui.set_problem(t));
    }

    fn greet(&mut self, hello: Hello) {
        let ui = self.ui();
        ui.set_cpu(format!("{}{}", hello.cpu, if hello.simulated { " · имитация" } else { "" }).into());
        ui.set_cols(hello.layout.cols);
        ui.set_rows(hello.layout.rows);
        let note = if hello.layout.confirmed {
            String::new()
        } else {
            hello.layout.note.clone().unwrap_or_default()
        };
        ui.set_note(note.into());
        if self.selected < 0
            && let Some(first) = hello
                .blocks
                .iter()
                .find(|b| b.kind == "P")
                .or_else(|| hello.blocks.first())
        {
            self.selected = slint_int(first.index);
        }
        self.hello = Some(hello);
        self.refresh(true);
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
        self.state = Some(state);
        self.show_problem();
        self.refresh(false);
    }

    /// Строки плиток заново; rebuild — после знакомства: раскладка могла смениться, модель строится с нуля.
    fn refresh(&mut self, rebuild: bool) {
        let Some(hello) = self.hello.as_ref() else { return };
        let rows: Vec<TileData> = hello.layout.tiles.iter().filter_map(|t| self.tile(hello, t)).collect();
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
    fn tile(&self, hello: &Hello, t: &crate::wire::Tile) -> Option<TileData> {
        let block = hello.blocks.get(t.block)?;
        let members = &block.members;
        let first = block.first;
        let mut data = TileData {
            block: slint_int(block.index),
            col: i32::from(t.col),
            row: i32::from(t.row),
            fill: rgb(CELL),
            temp: "–".into(),
            pair_color: rgb(TEXT),
            selected: self.selected == slint_int(block.index),
            ..Default::default()
        };
        let empty: Vec<bool> = Vec::new();
        let thr = self.state.as_ref().map_or(&empty, |s| &s.thr);
        let grid = t.places.len() > 1;
        let nums: Vec<NumData> = t
            .places
            .iter()
            .enumerate()
            .filter_map(|(k, &i)| {
                let core = hello.cores.get(i)?;
                Some(NumData {
                    text: core.lp.to_string().into(),
                    color: rgb(if thr.get(i).copied().unwrap_or(false) {
                        BAD
                    } else if core.kind == "P" {
                        P_CORE
                    } else {
                        E_CORE
                    }),
                    col: if grid { slint_int(k % 2) } else { 0 },
                    row: if grid { slint_int(k / 2) } else { 0 },
                })
            })
            .collect();
        data.nums = ModelRc::new(VecModel::from(nums));
        let Some(s) = self.state.as_ref() else {
            return Some(data);
        };
        let temp = highest(&s.cur, members);
        if let Some(v) = temp {
            data.temp = format!("{v:.0}°").into();
            data.fill = heat((v - 40.0) / 60.0);
        }
        let peak = highest(&s.max, members);
        let mean = highest(&s.avg, members);
        let parts: Vec<String> = [
            mean.map(|m| format!("⌀ {}", m.round())),
            peak.map(|p| format!("▲ {p:.0}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        data.stats = parts.join("  ").into();
        data.loaded = members.iter().any(|&i| s.load.get(i).copied().unwrap_or(false));
        if let Some(limit) = s.limit.as_ref().and_then(|l| l.get(first).copied()).filter(|&l| l > 0) {
            let ratio = highest(&s.ratio, members);
            data.pair = match ratio {
                Some(r) => format!("{r:.1} / ×{limit}"),
                None => format!("×{limit}"),
            }
            .into();
            if let Some(l) = self.limits.as_ref() {
                if let Some(&fused) = l.fused.get(first) {
                    data.pair_color = rgb(match limit.cmp(&fused) {
                        Ordering::Less => ACCENT,
                        Ordering::Greater => BAD,
                        Ordering::Equal => TEXT,
                    });
                }
                if let Some(&was) = l.was.get(first)
                    && was > 0
                    && limit != was
                {
                    let d = limit - was;
                    data.delta = format!("{}{}", if d > 0 { "+" } else { "−" }, d.abs()).into();
                }
            }
        }
        Some(data)
    }

    // ----- команды владельца -----

    pub fn order(&mut self, command: &str, args: Map<String, Value>, failure: &'static str) {
        if let Some(id) = self.link.ask(command, args) {
            self.pending.insert(id, failure);
        } else {
            self.problem = Some(format!("{failure}: нет связи с движком"));
            self.show_problem();
        }
    }

    pub fn clicked(&mut self, block: i32) {
        self.selected = block;
        self.refresh(false);
        let mut args = Map::new();
        args.insert("block".into(), block.into());
        self.order("load.toggle", args, "Нагрузка не изменена");
    }

    /// Правая кнопка: меню потолка от нескольких ступеней выше заводского предела вниз; что предлагать, решает движок.
    pub fn menu(&mut self, block: i32) {
        self.selected = block;
        self.refresh(false);
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
                let mut hint = match v.cmp(&fused) {
                    Ordering::Equal => "заводской предел",
                    Ordering::Greater => "выше заводского",
                    Ordering::Less => "",
                }
                .to_string();
                if Some(v) == was && v != fused {
                    hint.push_str(if hint.is_empty() { "было" } else { " · было" });
                }
                MenuEntry {
                    ratio: slint_int(v),
                    text: format!("×{v}").into(),
                    hint: hint.into(),
                    checked: Some(v) == current,
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

    pub fn problem_dismissed(&mut self) {
        self.problem = None;
        self.show_problem();
    }

    /// Строка плитки, как она показана сейчас (скрытой проверке).
    pub fn shown(&self, block: usize) -> Option<TileData> {
        (0..self.tiles.row_count())
            .filter_map(|i| self.tiles.row_data(i))
            .find(|t| t.block == slint_int(block))
    }
}

/// Одна и та же ли строка плитки. Сравнение Slint для моделей внутри строки — по адресу, а номера ядер строятся
/// заново на каждое состояние; поэтому они сравниваются по содержимому.
fn same(a: &TileData, b: &TileData) -> bool {
    let nums = |t: &TileData| {
        t.nums
            .iter()
            .map(|n| (n.text, n.color, n.col, n.row))
            .collect::<Vec<_>>()
    };
    a.block == b.block
        && a.col == b.col
        && a.row == b.row
        && a.stats == b.stats
        && a.temp == b.temp
        && a.fill == b.fill
        && a.pair == b.pair
        && a.pair_color == b.pair_color
        && a.delta == b.delta
        && a.loaded == b.loaded
        && a.selected == b.selected
        && nums(a) == nums(b)
}

/// Ставит строку, только если она другая. 1 — поставлена, 0 — та же.
fn set_text(current: &SharedString, text: &str, set: impl FnOnce(SharedString)) -> u32 {
    if current.as_str() == text {
        return 0;
    }
    set(text.into());
    1
}
