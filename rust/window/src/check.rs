// Скрытая проверка окна (--snapshot F): ни окна на экране, ни кнопки на панели задач. Окно рисуется программно
// (программный отрисовщик Slint в буфер), щелчки — настоящие события мыши по координатам плиток и меню. Против
// движка на имитации; запускает её проверка движка (Tray.cs C#-части). Итог — F.txt рядом с картинкой F, первая
// строка ok / FAILED; картинки F, F-load.png, F-menu.png смотрит агент — как выглядит окно, проверка не судит.
use std::cell::RefCell;
use std::fs::File;
use std::io::BufWriter;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Map;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, PhysicalSize, Rgb8Pixel};

use crate::AppWindow;
use crate::pipe::{Event, Link};
use crate::view::View;
use crate::wire::{BlockInfo, Hello, Limits};

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(Rc::<MinimalSoftwareWindow>::clone(&self.window))
    }
}

struct Check {
    ui: AppWindow,
    window: Rc<MinimalSoftwareWindow>,
    view: Rc<RefCell<View>>,
    events: mpsc::Receiver<Event>,
    size: (u32, u32),
    buffer: Vec<Rgb8Pixel>,
    snapshot: String,
    lines: Vec<String>,
    sampling: bool,
    samples: Vec<(u32, bool)>, // на каждое состояние: сколько показанного изменилось, перерисовалось ли окно
}

pub fn run(pipe: String, snapshot: &str, size: (u32, u32)) -> i32 {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: Rc::clone(&window),
    }))
    .expect("платформа проверки");
    let ui = AppWindow::new().expect("окно");
    ui.window().set_size(PhysicalSize::new(size.0, size.1));
    ui.show().expect("окно");
    let (send, events) = mpsc::channel();
    let link = Link::start(pipe, move |event| {
        // Приёмник живёт до конца проверки; после неё событие некому принять.
        let _ = send.send(event);
    });
    let view = Rc::new(RefCell::new(View::new(&ui, link)));
    crate::connect(&ui, &view);
    let mut check = Check {
        ui,
        window,
        view,
        events,
        size,
        buffer: vec![Rgb8Pixel::default(); (size.0 * size.1) as usize],
        snapshot: snapshot.to_string(),
        lines: Vec::new(),
        sampling: false,
        samples: Vec::new(),
    };
    let started = Instant::now();
    check.steps();
    let failed = check.lines.iter().any(|l| l.starts_with("FAILED"));
    let mut report = vec![if failed { "FAILED".to_string() } else { "ok".to_string() }];
    report.push(format!(
        "CoreMapWindow {} on Slint (version pinned in rust\\window\\Cargo.toml), software renderer, window {}x{}, {:.1} s",
        env!("CARGO_PKG_VERSION"),
        size.0,
        size.1,
        started.elapsed().as_secs_f64()
    ));
    report.extend(check.lines);
    if std::fs::write(format!("{snapshot}.txt"), report.join("\n") + "\n").is_err() {
        return 3; // отчёта нет — проверка движка увидит это по его отсутствию
    }
    i32::from(failed)
}

/// Номер пункта меню или места в сетке → координата: их десятки, f32 держит их точно.
#[expect(clippy::cast_precision_loss, reason = "menu rows number in tens, exact in f32")]
fn units(v: usize) -> f32 {
    v as f32
}

impl Check {
    fn check(&mut self, ok: bool, what: impl AsRef<str>) -> bool {
        self.lines
            .push(format!("{}{}", if ok { "ok      " } else { "FAILED  " }, what.as_ref()));
        ok
    }

    fn draw(&mut self) -> bool {
        let width = self.size.0 as usize;
        let buffer = &mut self.buffer;
        self.window.draw_if_needed(|renderer| {
            renderer.render(buffer, width);
        })
    }

    /// Принимает события канала и рисует, пока условие не выполнится; false — не дождался.
    fn pump(&mut self, until: impl Fn(&View) -> bool, seconds: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            while let Ok(event) = self.events.try_recv() {
                let before = self.view.borrow().changes.states;
                self.view.borrow_mut().on_event(event);
                slint::platform::update_timers_and_animations();
                let drawn = self.draw();
                let changes = self.view.borrow().changes;
                if self.sampling && changes.states != before {
                    self.samples.push((changes.last, drawn));
                }
            }
            slint::platform::update_timers_and_animations();
            self.draw();
            if until(&self.view.borrow()) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn shot(&mut self, name: &str) -> bool {
        self.window.request_redraw();
        self.draw();
        let path = if name == "main" {
            self.snapshot.clone()
        } else {
            format!("{}-{name}.png", self.snapshot.trim_end_matches(".png"))
        };
        let Ok(file) = File::create(&path) else { return false };
        let mut encoder = png::Encoder::new(BufWriter::new(file), self.size.0, self.size.1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let bytes: Vec<u8> = self.buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        encoder
            .write_header()
            .and_then(|mut w| w.write_image_data(&bytes))
            .is_ok()
    }

    fn click(&mut self, x: f32, y: f32, button: PointerEventButton) {
        let position = LogicalPosition::new(x, y);
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    fn tile_center(&self, hello: &Hello, block: usize) -> (f32, f32) {
        let tile = hello.layout.tiles.iter().find(|t| t.block == block).expect("плитка");
        let (w, h, gap) = (self.ui.get_tile_w(), self.ui.get_tile_h(), self.ui.get_gap());
        (
            self.ui.get_die_x() + f32::from(tile.col) * (w + gap) + w / 2.0,
            self.ui.get_die_y() + f32::from(tile.row) * (h + gap) + h / 2.0,
        )
    }

    fn steps(&mut self) {
        let Some((hello, limits)) = self.arrival() else { return };
        self.idle();
        let Some(p_block) = hello
            .blocks
            .iter()
            .find(|b| b.kind == "P")
            .or_else(|| hello.blocks.first())
            .cloned()
        else {
            self.check(false, "в знакомстве нет блоков");
            return;
        };
        self.load_click(&hello, &p_block);
        let e_block = hello.blocks.iter().find(|b| b.kind == "E").unwrap_or(&p_block).clone();
        self.limit_menu(&hello, &limits, &e_block);
    }

    /// Знакомство, потолки и первое состояние пришли; плиток столько, сколько в раскладке; первая картинка.
    fn arrival(&mut self) -> Option<(Hello, Limits)> {
        let arrived = self.pump(|v| v.hello.is_some() && v.limits.is_some() && v.state.is_some(), 15.0);
        if !self.check(arrived, "знакомство, потолки и первое состояние пришли")
        {
            // Почему: что успело прийти и что окно пишет о связи (там же причина, если сообщение не разобралось).
            let view = self.view.borrow();
            let line = format!(
                "        пришло: знакомство {}, потолки {}, состояние {}; связь: «{}»",
                view.hello.is_some(),
                view.limits.is_some(),
                view.state.is_some(),
                self.ui.get_link()
            );
            drop(view);
            self.lines.push(line);
            return None;
        }
        let (Some(hello), Some(limits)) = (self.view.borrow().hello.clone(), self.view.borrow().limits.clone()) else {
            return None;
        };
        let rows = self.ui.get_tiles().row_count();
        self.check(
            rows == hello.layout.tiles.len(),
            format!("плиток {rows}, в раскладке {}", hello.layout.tiles.len()),
        );
        let shot = self.shot("main");
        self.check(shot, "картинка окна");
        Some((hello, limits))
    }

    /// 1. Перерисовки на каждое состояние движка без действий владельца.
    fn idle(&mut self) {
        self.samples.clear();
        self.sampling = true;
        let from = self.view.borrow().changes.states;
        self.pump(|v| v.changes.states >= from + 8, 10.0);
        self.sampling = false;
        let samples = std::mem::take(&mut self.samples);
        let quiet: Vec<_> = samples.iter().filter(|(changed, _)| *changed == 0).collect();
        self.check(
            samples.len() >= 8,
            format!("состояний подряд без действий: {}", samples.len()),
        );
        self.check(
            quiet.iter().all(|(_, drawn)| !drawn),
            format!(
                "из них без изменений показанного {}, перерисовок окна на них {} (должно быть 0)",
                quiet.len(),
                quiet.iter().filter(|(_, d)| *d).count()
            ),
        );
        self.lines.push(format!(
            "        за эти состояния изменилось показанного {}, перерисовок окна {}",
            samples.iter().map(|(c, _)| c).sum::<u32>(),
            samples.iter().filter(|(_, d)| *d).count()
        ));
    }

    /// 2. Щелчок по плитке P-блока — нагрузка; второй щелчок её снимает.
    fn load_click(&mut self, hello: &Hello, p_block: &BlockInfo) {
        let (x, y) = self.tile_center(hello, p_block.index);
        let loaded = |v: &View, first: usize| {
            v.state
                .as_ref()
                .is_some_and(|s| s.load.get(first).copied().unwrap_or(false))
        };
        self.click(x, y, PointerEventButton::Left);
        let on = self.pump(|v| loaded(v, p_block.first), 5.0);
        self.check(on, format!("щелчок по плитке {} дал нагрузку", p_block.name));
        let framed = self.view.borrow().shown(p_block.index).is_some_and(|t| t.loaded);
        self.check(framed, "плитка под нагрузкой обведена");
        let shot = self.shot("load");
        self.check(shot, "картинка с нагрузкой");
        self.click(x, y, PointerEventButton::Left);
        let off = self.pump(|v| !loaded(v, p_block.first), 5.0);
        self.check(off, "второй щелчок снял нагрузку");
    }

    /// 3. Правая кнопка по плитке E-блока — меню; пункт на ступень ниже заводского ставит потолок, затем он возвращается.
    fn limit_menu(&mut self, hello: &Hello, limits: &Limits, e_block: &BlockInfo) {
        let (x, y) = self.tile_center(hello, e_block.index);
        self.click(x, y, PointerEventButton::Right);
        self.pump(|_| true, 0.0);
        let shown = self.ui.get_menu_shown();
        self.check(shown, "правая кнопка открыла меню");
        let entries = self.ui.get_menu_entries();
        let wanted = limits.choices[e_block.index].len();
        self.check(
            entries.row_count() == wanted,
            format!(
                "меню потолка {}: пунктов {}, у движка {wanted}",
                e_block.name,
                entries.row_count()
            ),
        );
        let shot = self.shot("menu");
        self.check(shot, "картинка с меню");
        let limit = |v: &View, first: usize| {
            v.state
                .as_ref()
                .and_then(|s| s.limit.as_ref())
                .and_then(|l| l.get(first).copied())
        };
        let before = limit(&self.view.borrow(), e_block.first);
        let target = limits.fused[e_block.first] - 1;
        let index = entries.iter().position(|m| i64::from(m.ratio) == target);
        let (Some(index), Some(before)) = (index, before) else {
            self.check(
                false,
                format!("в меню есть ×{target}, потолок сейчас ×{}", before.unwrap_or(0)),
            );
            return;
        };
        self.check(true, format!("в меню есть ×{target}, потолок сейчас ×{before}"));
        let (mx, my) = (
            self.ui.get_menu_left() + 125.0,
            self.ui.get_menu_top()
                + self.ui.get_menu_head_h()
                + units(index) * self.ui.get_entry_h()
                + self.ui.get_entry_h() / 2.0,
        );
        self.click(mx, my, PointerEventButton::Left);
        let set = self.pump(|v| limit(v, e_block.first) == Some(target), 5.0);
        self.check(set, format!("пункт ×{target} поставил потолок"));
        let closed = !self.ui.get_menu_shown();
        self.check(closed, "меню закрылось");
        let diff = target - limits.was[e_block.first];
        let expected = if diff == 0 {
            String::new()
        } else {
            format!("{}{}", if diff > 0 { "+" } else { "−" }, diff.abs())
        };
        let delta = self
            .view
            .borrow()
            .shown(e_block.index)
            .map(|t| t.delta.to_string())
            .unwrap_or_default();
        self.check(delta == expected, format!("на плитке разница с «было»: «{delta}»"));
        let mut args = Map::new();
        args.insert("block".into(), e_block.index.into());
        args.insert("ratio".into(), before.into());
        self.view.borrow_mut().order("limit.set", args, "Потолок не изменён");
        let back = self.pump(|v| limit(v, e_block.first) == Some(before), 5.0);
        self.check(back, format!("потолок возвращён на ×{before}"));
    }
}
