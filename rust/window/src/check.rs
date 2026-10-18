// Скрытая проверка окна (--snapshot F): ни окна на экране, ни кнопки на панели задач. Окно рисуется программно
// (программный отрисовщик Slint в буфер), щелчки, перетаскивания и нажатия клавиш — настоящие события по координатам
// плиток, кнопок, ползунков и строк. Итог — F.txt рядом с картинкой F, первая строка ok / FAILED; картинки смотрит
// агент — как выглядит окно, проверка не судит.
// Три прохода (`Mode`):
// • полный — против движка на имитации, его запускает проверка движка (Tray.cs C#-части): картинки F, F-select.png,
//   F-pin.png, F-tip.png, F-abs.png, F-all.png, F-chart-tip.png, F-swap.png, F-swap-tip.png, F-profiles.png,
//   F-menu.png, F-1280.png, F-1280-tip.png, F-1280-swap.png, F-1500.png, F-natural.png;
// • --settings — против программы у часов, её запускает её скрытая проверка (rust/tray/src/check_window.rs): настройки
//   автомодуля выключателями и ползунком, профили щелчками (с отметкой «загружать при старте Windows»), затем выбор
//   блоков, подсветка, размер — и закрытие окна; картинки F, F-profiles.png;
// • --reopened — второй запуск с той же папкой (--data): окно открылось таким, каким его закрыли; картинка F.
// • --lost — CoreMapTray пропадает без «bye» (его проверка рвёт свой канал, как при падении): окно остаётся и одной
//   строкой говорит, что CoreMapTray не отвечает; картинка F.
// • --bye — CoreMapTray выходит («Выход» в меню) и говорит «bye»: окно записывает себя в файл окна и закрывается.
//
// Статистика — из своей временной папки: придуманные срезы за сегодня (полный день, 43 200 строк — цена разбора дня),
// вчера и 40 дней назад (не должны войти), обрезанная последняя строка и журнал автомодуля. Что
// должно получиться, генератор считает сам, не разбором файлов: по нему сверяются точки графика. Папка удаляется в конце.
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Map;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, PhysicalSize, Rgb8Pixel};

use crate::AppWindow;
use crate::chart::{self, Curves, E_COLORS, P_COLORS, Scale};
use crate::panels::{self, f};
use crate::pipe::{Event, Link};
use crate::samples;
use crate::store::{Delivery, Order};
use crate::timeline::{Layout, Quantity};
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

/// Какой проход проверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Full,
    Settings,
    Reopened,
    Lost,
    Bye,
}

/// Что проход --settings делает с настройками и окном, а --reopened и проверка программы у часов
/// (`rust/tray/src/check_window.rs`, `WINDOW_LIMIT`) ждут: отсечка, аварийная, размер перед закрытием.
const LIMIT_TO: i32 = 88;
const EMERGENCY_TO: i32 = 97;
const CLOSED_SIZE: (u32, u32) = (1500, 900);
/// Жестов с настройками в проходе --settings: включить, отсечка, аварийная, выключить (глубины и подтягивания нет,
/// DECISIONS #38, #40).
const GESTURES: u32 = 4;
/// Профиль, который проверка сохраняет, применяет, перезаписывает и удаляет щелчками (его же ищет проверка программы у
/// часов в истории профилей, `WINDOW_PROFILE`).
const PROFILE: &str = "проверка окна";
/// Размер окна в проходе --bye: в файле окна его нет (там `CLOSED_SIZE`), записать его может только «bye». Его же ждёт
/// проверка программы у часов (`rust/tray/src/check_window.rs`, `WINDOW_LEFT_SIZE`).
const LEFT_SIZE: (u32, u32) = (1400, 860);
/// Строка о связи, когда `CoreMapTray` пропал без «bye» (бриф владельца 15.10: `CoreMapTray`, а не «программа у часов»).
const MISSING: &str = "CoreMapTray не отвечает — автоматика не работает";

struct Check {
    ui: AppWindow,
    window: Rc<MinimalSoftwareWindow>,
    view: Rc<RefCell<View>>,
    events: mpsc::Receiver<Event>,
    digests: Option<mpsc::Receiver<Delivery>>,
    stats: Option<mpsc::Sender<Order>>,
    size: (u32, u32),
    buffer: Vec<Rgb8Pixel>,
    snapshot: String,
    lines: Vec<String>,
    sampling: bool,
    samples: Vec<(u32, bool)>, // на каждое состояние: сколько показанного изменилось, перерисовалось ли окно
    /// Папка статистики и файла окна: своя временная (удаляется в конце) или данная ключом --data.
    data: PathBuf,
    own_data: bool,
    mode: Mode,
    /// Начало проверки — для отчёта по ходу.
    started: Instant,
}

pub fn run(pipe: String, snapshot: &str, size: (u32, u32), data: Option<PathBuf>, mode: Mode) -> i32 {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless {
        window: Rc::clone(&window),
    }))
    .expect("платформа проверки");
    let ui = AppWindow::new().expect("окно");
    ui.window().set_size(PhysicalSize::new(size.0, size.1));
    let own_data = data.is_none();
    let dir = data.unwrap_or_else(|| std::env::temp_dir().join(format!("coremap-window-check-{}", std::process::id())));
    if own_data {
        let _ = std::fs::remove_dir_all(&dir); // остатки проверки с тем же номером процесса — не наши данные
    }
    let created = std::fs::create_dir_all(&dir);
    // Окно открывается тем же путём, что настоящее (main.rs): размер и место из файла окна — до показа.
    let memory = crate::saved::open(&ui, &dir);
    // Проход --bye: размер не тот, что в файле окна; щелчков в проходе нет — в файл его запишет только «bye».
    if mode == Mode::Bye {
        ui.window().set_size(PhysicalSize::new(LEFT_SIZE.0, LEFT_SIZE.1));
    }
    ui.show().expect("окно");
    let shown = ui.window().size();
    let size = (shown.width, shown.height);
    let (send, events) = mpsc::channel();
    let link = Link::start(pipe, move |event| {
        // Приёмник живёт до конца проверки; после неё событие некому принять.
        let _ = send.send(event);
    });
    let view = Rc::new(RefCell::new(View::new(&ui, link, memory)));
    crate::connect(&ui, &view);
    let mut check = Check {
        ui,
        window,
        view,
        events,
        digests: None,
        stats: None,
        size,
        buffer: vec![Rgb8Pixel::default(); (size.0 * size.1) as usize],
        snapshot: snapshot.to_string(),
        lines: Vec::new(),
        sampling: false,
        samples: Vec::new(),
        data: dir,
        own_data,
        mode,
        started: Instant::now(),
    };
    if let Err(e) = created {
        check.check(false, format!("папка {} не создана: {e}", check.data.display()));
    }
    let started = Instant::now();
    match mode {
        Mode::Full => check.steps(),
        Mode::Settings => check.settings_steps(),
        Mode::Reopened => check.reopened_steps(),
        Mode::Lost => check.lost_steps(),
        Mode::Bye => check.bye_steps(),
    }
    // Своя временная папка статистики не остаётся после проверки; не удалилась — сказано в отчёте.
    if check.own_data
        && let Err(e) = std::fs::remove_dir_all(&check.data)
    {
        check.lines.push(format!(
            "        временная папка {} не удалена: {e}",
            check.data.display()
        ));
    }
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
        self.progress();
        ok
    }

    fn note(&mut self, what: impl AsRef<str>) {
        self.lines.push(format!("        {}", what.as_ref()));
        self.progress();
    }

    /// Отчёт по ходу проверки: проверку, которую запустивший её прервал по сроку, видно до места, где она встала (в
    /// конце отчёт пишется целиком заново). Не записался — не беда: в конце он пишется ещё раз.
    fn progress(&self) {
        let text = format!(
            "FAILED\nпроверка не дошла до конца ({:.1} с) — строки до места, где её прервали:\n{}\n",
            self.started.elapsed().as_secs_f64(),
            self.lines.join("\n")
        );
        let _ = std::fs::write(format!("{}.txt", self.snapshot), text); // see the comment above
    }

    fn draw(&mut self) -> bool {
        let width = self.size.0 as usize;
        let buffer = &mut self.buffer;
        self.window.draw_if_needed(|renderer| {
            renderer.render(buffer, width);
        })
    }

    /// Принимает события канала и сводки статистики и рисует, пока условие не выполнится; false — не дождался.
    fn pump(&mut self, until: impl Fn(&View) -> bool, seconds: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            while let Some(delivery) = self.digests.as_ref().and_then(|d| d.try_recv().ok()) {
                match delivery {
                    Delivery::Digest(digest) => self.view.borrow_mut().on_digest(digest),
                    Delivery::Day(day) => self.view.borrow_mut().on_day(day),
                }
            }
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

    fn resize(&mut self, size: (u32, u32)) {
        self.size = size;
        self.buffer = vec![Rgb8Pixel::default(); (size.0 * size.1) as usize];
        self.ui.window().set_size(PhysicalSize::new(size.0, size.1));
        self.pump(|_| true, 0.0);
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
        // Курсор уходит из окна: подсказка под ним не появится на картинках.
        window.dispatch_event(WindowEvent::PointerExited);
        self.pump(|_| true, 0.0);
    }

    fn type_text(&mut self, text: &str) {
        let window = self.ui.window();
        for c in text.chars() {
            let text: slint::SharedString = c.to_string().into();
            window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window.dispatch_event(WindowEvent::KeyReleased { text });
        }
        self.pump(|_| true, 0.0);
    }

    fn tile_center(&self, block: usize) -> (f32, f32) {
        let tile = self.view.borrow().shown(block).expect("плитка блока");
        let (w, h) = (self.ui.get_die_w(), self.ui.get_die_h());
        (
            self.ui.get_die_x() + (tile.x + tile.w / 2.0) * w,
            self.ui.get_die_y() + (tile.y + tile.h / 2.0) * h,
        )
    }

    fn steps(&mut self) {
        let Some((hello, limits)) = self.arrival() else { return };
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
        let e_block = hello.blocks.iter().find(|b| b.kind == "E").unwrap_or(&p_block).clone();
        let expected = self.statistics(&hello, &limits, &e_block);
        let shot = self.shot("main");
        self.check(
            shot,
            "картинка окна (выбран первый P-блок, статистика из придуманных срезов)",
        );
        self.idle();
        self.restore_repaint();
        self.own_view(&p_block, &e_block);
        self.select(&p_block, &e_block, expected.as_ref());
        self.chart_views(&p_block, &e_block, expected.as_ref());
        self.model_curves(&p_block, &e_block);
        self.curve_switches(&e_block);
        self.max_and_cuts(&p_block, &e_block);
        self.means_and_reset(&p_block);
        self.limit_menu(&limits, &e_block);
        self.profiles(&limits, &e_block);
        self.sizes(&p_block, &e_block);
        self.time_view(&p_block, &e_block, expected.as_ref());
        self.conditional_preview(&p_block);
    }

    fn conditional_preview(&mut self, block: &BlockInfo) {
        if !self.own_data {
            return;
        }
        let points: Vec<_> = (25..=50)
            .map(|r| {
                let heat = 3.0 + f64::from(r - 25) * 0.25;
                serde_json::json!({"ratio":r,"heat":heat,"alternate_heat":heat * 1.2})
            })
            .collect();
        let snapshot = serde_json::json!({
            "version":1,"source":"Проверочный вымышленный журнал","conditions":"GPU 60 °C; вентилятор 3000 об/мин; две подгонки",
            "blocks":[{"name":block.name,"temperature_test_mae":0.4,"points":points}]
        });
        if let Err(error) = std::fs::write(self.data.join(crate::conditional_snapshot::FILE), snapshot.to_string()) {
            self.check(false, format!("сохранённая проверочная модель: {error}"));
            return;
        }
        if let Some(stats) = &self.stats {
            let _ = stats.send(Order::Scan);
        } // Поток жив, пока жива проверка.
        let arrived = self.pump(|v| v.digest.as_ref().is_some_and(|d| d.conditional.is_some()), 10.0);
        self.check(arrived, "сохранённая условная модель прочитана в потоке статистики");
        let blocks = (self.ui.get_blocks_chip_x(), self.ui.get_blocks_chip_y());
        self.chip(
            blocks,
            |v| !v.timeline,
            "выход из вида по времени перед условной моделью",
        );
        if !self.view.borrow().compared.contains(&block.index) {
            let (x, y) = self.tile_center(block.index);
            self.click(x, y, PointerEventButton::Left);
        }
        if !self.view.borrow().model {
            self.chip(
                (self.ui.get_model_chip_x(), self.ui.get_model_chip_y()),
                |v| v.model,
                "модель включена для условного вида",
            );
        }
        let efficiency = (self.ui.get_conditional_chip_x(), self.ui.get_conditional_chip_y());
        self.chip(
            efficiency,
            |v| v.scale == Scale::ConditionalEfficiency,
            "условная эффективность открывается щелчком",
        );
        self.view.borrow_mut().axes_changed(true);
        self.pump(|_| true, 0.0);
        let drawn = self.view.borrow().shown_stats.as_ref().is_some_and(|s| {
            s.series
                .iter()
                .any(|line| !line.model.is_empty() && !line.sensitivity.is_empty())
        });
        self.check(
            drawn && self.ui.get_chart_swap(),
            "основная линия и область чувствительности видны при множителе по вертикали",
        );
        let shot = self.shot("conditional");
        self.check(shot, "картинка условной эффективности по вымышленной модели");
        let heat = (
            self.ui.get_conditional_heat_chip_x(),
            self.ui.get_conditional_heat_chip_y(),
        );
        self.chip(
            heat,
            |v| v.scale == Scale::ConditionalHeat,
            "собственный нагрев открывается щелчком",
        );
        let shot = self.shot("conditional-heat");
        self.check(shot, "картинка условного собственного нагрева");
    }

    /// Обход ошибки Slint 1.18.1 (src/restore.rs): путь восстановления из свёрнутого меняет прозрачный слой во всё окно —
    /// следующий кадр рисует окно целиком; цвет слоя остаётся полностью прозрачным. Настоящего сворачивания здесь нет
    /// (окно проверки не на winit, `on_winit_window_event` для него ничего не делает) — тот же путь проходят события,
    /// которые дал бы winit; что настоящий winit их шлёт и что окно после этого рисуется целиком, видно только на экране.
    fn restore_repaint(&mut self) {
        use crate::restore::{Restore, Seen};
        let mut restore = Restore::default();
        let state = |c: &Check| (c.ui.get_repaint_epoch(), c.ui.get_repaint_veil());
        let before = state(self);
        restore.on(&self.ui, Seen::Resized(0, 0));
        let minimized = state(self) == before;
        restore.on(&self.ui, Seen::Resized(self.size.0, self.size.1));
        let restored = state(self);
        restore.on(&self.ui, Seen::Resized(self.size.0 + 10, self.size.1));
        let resized = state(self) == restored;
        restore.on(&self.ui, Seen::Visible);
        let visible = state(self);
        let drawn = self.draw();
        self.check(
            minimized
                && restored.0 != before.0
                && restored.1 != before.1
                && resized
                && visible.1 != restored.1
                && [before.1, restored.1, visible.1].iter().all(|c| c.alpha() == 0)
                && drawn,
            format!(
                "после сворачивания слой во всё окно меняется ({:?} → {:?} → {:?}), всегда прозрачный; свернуть и просто сменить размер — не меняют",
                before.1, restored.1, visible.1
            ),
        );
    }

    /// Знакомство, потолки и первое состояние пришли; плиток столько, сколько в раскладке.
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
        // Кластер E-ядер отличается от P-плитки: рамка с четырьмя клетками ядер и ряд выше.
        let tiles: Vec<_> = self.ui.get_tiles().iter().collect();
        let p = tiles.iter().find(|t| !t.cluster);
        let e = tiles.iter().find(|t| t.cluster);
        if let (Some(p), Some(e)) = (p, e) {
            self.check(
                e.cells.row_count() == 4 && p.cells.row_count() == 0 && e.h > p.h,
                format!(
                    "кластер — четыре клетки ядер на подложке, P-плитка — одна; высота кластера {:.0} px, P {:.0} px",
                    e.h * self.ui.get_die_h(),
                    p.h * self.ui.get_die_h()
                ),
            );
        }
        Some((hello, limits))
    }

    /// Придуманные срезы и журналы в папке проверки; поток статистики над ней; первая сводка. Полный проход пишет
    /// полный день (цена разбора), проход --settings — короткий (график для линии отсечки), --reopened — читает то,
    /// что записал проход --settings.
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario: files, first digest, the model's digest, a quiet pass, links - read top to bottom"
    )]
    fn statistics(&mut self, hello: &Hello, limits: &Limits, e_block: &BlockInfo) -> Option<Synthetic> {
        let synthesized = match self.mode {
            Mode::Full => self.own_data,
            Mode::Settings => !self.data.join("samples").exists(),
            Mode::Reopened | Mode::Lost | Mode::Bye => false,
        };
        let expected = if synthesized {
            match synthesize(&self.data, hello, limits, e_block, self.mode != Mode::Full) {
                Ok(s) => Some(s),
                Err(e) => {
                    self.check(false, format!("придуманные срезы не записаны: {e}"));
                    None
                }
            }
        } else {
            None
        };
        let dir = self.data.clone();
        let (send, digests) = mpsc::channel();
        self.digests = Some(digests);
        let started = Instant::now();
        let stats = crate::store::start(dir, move |d| {
            // Приёмник живёт до конца проверки.
            let _ = send.send(d);
        });
        // Блоки и соседи для модели — тем же путём, что у окна (знакомство уже пришло).
        self.view.borrow_mut().stats_to(stats.clone());
        self.stats = Some(stats);
        let arrived = self.pump(|v| v.digest.is_some(), 30.0);
        self.check(
            arrived,
            format!("сводка статистики пришла за {:.1} с", started.elapsed().as_secs_f64()),
        );
        let Some(digest) = self.view.borrow().digest.clone() else {
            return expected;
        };
        self.note(format!(
            "цена: первый проход — файлов {}, {:.1} МБ, строк {}, {:.0} мс (разбор, счёт и сводка, в потоке статистики)",
            digest.files,
            to_mb(digest.parsed_bytes),
            digest.parsed_rows,
            digest.parse_time.as_secs_f64() * 1000.0
        ));
        if let Some(s) = &expected {
            self.note(format!(
                "        из них сегодняшний день (по 2 с, перерыв, по 0,25 с): {} строк, {:.1} МБ",
                s.today_rows,
                to_mb(s.today_bytes)
            ));
            self.check(
                digest.tally.rows == s.rows && digest.files == 2,
                format!(
                    "строк в сводке {}, должно {} (сегодня без обрезанной строки и вчера; файл 40-дневной давности не входит), файлов {}",
                    digest.tally.rows, s.rows, digest.files
                ),
            );
            // Время — по времени строк: прежняя строка — 2 с, новая — 0,25 с, перерыв между ними в счёт не идёт.
            #[expect(clippy::cast_precision_loss, reason = "milliseconds of a synthetic day and a half")]
            let want = s.ms as f64 / 1000.0;
            self.check(
                (digest.tally.secs - want).abs() < 1e-6,
                format!(
                    "время в сводке {:.2} с, должно {want:.2} с (строки по 2 с и по 0,25 с, перерыв не в счёт)",
                    digest.tally.secs
                ),
            );
            // Модель нагрева: окно отдало потоку блоки и соседей — следующая сводка несёт модель по всем строкам.
            let modelled = self.pump(|v| v.digest.as_ref().is_some_and(|d| d.model.is_some()), 60.0);
            let latest = self.view.borrow().digest.clone();
            let model = latest.as_ref().and_then(|d| d.model.clone());
            let usable: Vec<String> = model
                .as_ref()
                .map(|m| {
                    m.blocks
                        .iter()
                        .map(|b| format!("{} {:.0} мин", b.name, b.minutes))
                        .collect()
                })
                .unwrap_or_default();
            self.check(
                modelled && model.as_ref().is_some_and(|m| m.blocks.iter().all(crate::model::BlockModel::drawn)),
                format!(
                    "модель нагрева посчитана по {} строкам за {:.0} мс (в потоке статистики), кривая есть у каждого блока; годных срезов: {}; строки для модели держат {:.1} МБ",
                    model.as_ref().map_or(0, |m| m.rows),
                    model.as_ref().map_or(0.0, |m| m.time.as_secs_f64() * 1000.0),
                    usable.join(", "),
                    to_mb(latest.as_ref().map_or(0, |d| d.kept_bytes) as u64)
                ),
            );
            // Повторный проход по неизменившимся файлам ничего не присылает (статистика и модель не пересчитываются
            // зря).
            if let Some(stats) = &self.stats {
                let _ = stats.send(Order::Scan); // поток жив, пока жива проверка
            }
            let again = self.pump(|_| false, 1.0);
            let quiet = !again
                && latest
                    .as_ref()
                    .is_some_and(|l| self.view.borrow().digest.as_ref().is_some_and(|d| Arc::ptr_eq(d, l)));
            self.check(quiet, "повторный проход по тем же файлам новой сводки не дал");
        }
        let (count, deltas) = self.view.borrow().links_shown();
        let all = self.ui.get_links().row_count();
        let close = deltas.iter().all(|d| (d - LINK_EFFECT).abs() < 1.5);
        self.check(
            expected.is_none() || (count > 0 && count == all && close),
            format!(
                "полос связи соседей {count}; разница «сосед занят / свободен» {} °C (в придуманных срезах {LINK_EFFECT} °C)",
                deltas.iter().map(|d| format!("{d:.1}")).collect::<Vec<_>>().join(", ")
            ),
        );
        expected
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

    /// Шаг 2: щелчки по плиткам — сравнение блоков в статистике (владелец 15.10): щелчок добавляет блок, повторный убирает,
    /// у каждого свой цвет линии и обводки; ось множителя одна на всех; подсветка линии — под курсором или
    /// закреплённая. И ничего больше: нагрузки щелчок не даёт.
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario of clicks and hovers, read top to bottom"
    )]
    fn select(&mut self, p_block: &BlockInfo, e_block: &BlockInfo, expected: Option<&Synthetic>) {
        let (p, e) = (p_block.index, e_block.index);
        let compared = |v: &View| v.compared.clone();
        let first = compared(&self.view.borrow()) == [p];
        self.check(
            first,
            format!("при открытии в статистике первый P-блок {}", p_block.name),
        );
        // Повторный щелчок убирает блок: статистика пуста и говорит, как выбрать.
        let (px, py) = self.tile_center(p);
        self.click(px, py, PointerEventButton::Left);
        let emptied = self.pump(|v| v.compared.is_empty(), 2.0);
        let hint = self.ui.get_chart_empty().to_string();
        let unframed = !self.view.borrow().shown(p).is_some_and(|t| t.selected);
        self.check(
            emptied && hint.contains("Щёлкни по плитке") && unframed,
            format!("повторный щелчок убрал блок {}; вместо графика: «{hint}»", p_block.name),
        );
        // Кластер — один в статистике.
        let (ex, ey) = self.tile_center(e);
        self.click(ex, ey, PointerEventButton::Left);
        let selected = self.pump(|v| compared(v) == [e], 2.0);
        self.check(selected, format!("щелчок выбрал блок {}", e_block.name));
        let outline = self.view.borrow().shown(e).map(|t| (t.selected, t.outline));
        let colour = self.colour(e);
        self.check(
            outline == Some((true, crate::panels::rgb(colour))) && E_COLORS.contains(&colour),
            "выбранная плитка обведена цветом своей линии (кластер — холодным)",
        );
        self.pump(|_| false, 1.0);
        let loaded = self
            .view
            .borrow()
            .state
            .as_ref()
            .is_some_and(|s| e_block.members.iter().any(|&i| s.load.get(i).copied().unwrap_or(false)));
        self.check(!loaded, "щелчок не дал нагрузки");
        let legend: Vec<String> = self.ui.get_stats_legend().iter().map(|l| l.name.to_string()).collect();
        self.check(
            legend.len() == 1 && legend[0].contains(&e_block.name),
            format!("в заголовке статистики: {legend:?}"),
        );
        let axis = |v: &View| {
            v.shown_stats
                .as_ref()
                .map(|s| s.columns.iter().map(|c| c.ratio).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let axis_e = axis(&self.view.borrow());
        if let Some(s) = expected {
            let want: Vec<(i32, u32)> = s
                .bins
                .get(&e_block.name)
                .map(|b| b.iter().map(|(r, (n, _))| (*r, *n)).collect())
                .unwrap_or_default();
            let got: Vec<(i32, u32)> = self
                .view
                .borrow()
                .shown_stats
                .as_ref()
                .and_then(|st| st.series.first())
                .map(|l| l.points.iter().map(|p| (p.ratio, p.samples)).collect())
                .unwrap_or_default();
            self.check(
                got == want && !got.is_empty(),
                format!("точки графика (множитель, строк) {got:?}, у генератора {want:?} — при любой нагрузке"),
            );
            let want_ms: Vec<(i32, u64)> = s
                .bins
                .get(&e_block.name)
                .map(|b| b.iter().map(|(r, (_, ms))| (*r, *ms)).collect())
                .unwrap_or_default();
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "seconds of a synthetic day, multiples of 0.25 s, rounded"
            )]
            let got_ms: Vec<(i32, u64)> = self
                .view
                .borrow()
                .digest
                .as_ref()
                .and_then(|d| d.tally.blocks.get(&e_block.name))
                .map(|t| {
                    t.bins
                        .iter()
                        .map(|(r, b)| (*r, (b.secs * 1000.0).round() as u64))
                        .collect()
                })
                .unwrap_or_default();
            self.check(
                got_ms == want_ms,
                format!("время корзин (множитель, мс) {got_ms:?}, у генератора {want_ms:?}"),
            );
            let loads: Vec<f32> = self
                .view
                .borrow()
                .shown_stats
                .as_ref()
                .and_then(|st| st.series.first())
                .map(|l| l.points.iter().map(|p| p.load).collect())
                .unwrap_or_default();
            self.check(
                !loads.is_empty() && loads.iter().all(|l| (0.1..=0.9).contains(l)),
                format!(
                    "у каждой точки ø нагрузка (в срезах 0,1…0,9): {}",
                    loads
                        .iter()
                        .map(|l| format!("{:.0}%", l * 100.0))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            );
            let covered = want.iter().all(|(r, _)| axis_e.contains(r))
                && axis_e.windows(2).all(|w| w[1] == w[0] + 1)
                && axis_e.len() == self.ui.get_chart_cols().row_count();
            self.check(
                covered,
                format!(
                    "ось множителя ×{}…×{} — подряд, каждый множитель столбцом, точки на ней",
                    axis_e.first().copied().unwrap_or_default(),
                    axis_e.last().copied().unwrap_or_default()
                ),
            );
            let events = self.ui.get_events().row_count();
            self.check(
                events == s.events_mine,
                format!(
                    "событий с блоком и без блока в списке {events}, должно быть {} (прежний журнал автомодуля до первого события и файл событий дня)",
                    s.events_mine
                ),
            );
        }
        // Второй блок — к первому: общая ось охватывает оба набора данных.
        self.click(px, py, PointerEventButton::Left);
        let two = self.pump(|v| compared(v) == [e, p], 2.0);
        let series = self.ui.get_chart_series().row_count();
        let combined = axis(&self.view.borrow());
        let selected_axis = axis_e.iter().all(|r| combined.contains(r)) && combined.len() >= axis_e.len();
        let now = compared(&self.view.borrow());
        self.check(
            two && series == 2 && selected_axis,
            format!(
                "щелчок по {} добавил его: блоков {now:?}, линий {series}; общая ось сохранила множители кластера и охватила оба блока: {selected_axis}",
                p_block.name
            ),
        );
        let p_outline = self.view.borrow().shown(p).map(|t| t.outline);
        let p_colour = self.colour(p);
        self.check(
            p_outline == Some(crate::panels::rgb(p_colour)) && P_COLORS.contains(&p_colour),
            "у второго блока — свой цвет (P-ядро — тёплым)",
        );
        let shot = self.shot("select");
        self.check(shot, "картинка сравнения двух блоков");
        // Подсказки столбцов: подсказка есть ровно у столбцов с точками (у множителей обоих блоков — в придуманных
        // срезах они не пересекаются: P ×50…54, кластер ×43…46); в каждой по каждому блоку — время, ø температура и ø
        // нагрузка, ▲ — с фактическим множителем, нагрузкой и временем своей строки; числа строк в ней нет.
        let (tips, point_ratios) = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .map(|st| {
                let tips: Vec<(i32, String)> = st
                    .columns
                    .iter()
                    .filter(|c| !c.tip.is_empty())
                    .map(|c| (c.ratio, c.tip.clone()))
                    .collect();
                let mut ratios: Vec<i32> = st
                    .series
                    .iter()
                    .flat_map(|s| s.points.iter().map(|p| p.ratio))
                    .collect();
                ratios.sort_unstable();
                ratios.dedup();
                (tips, ratios)
            })
            .unwrap_or_default();
        for tip in [tips.first(), tips.last()].into_iter().flatten() {
            self.note(format!("подсказка столбца: {}", tip.1.replace('\n', " / ")));
        }
        let well_formed = |tip: &str| {
            // Пробелы внутри чисел и единиц в подсказке неразрывные — для счёта они обычные.
            let tip = tip.replace('\u{a0}', " ");
            let peaks = tip.matches('▲').count();
            peaks >= 1
                && tip.matches(", нагрузка ø ").count() == peaks
                && tip.matches("95 % ниже").count() == peaks
                && tip.matches(" °C: ×").count() == peaks
                && tip.contains("над остальным кристаллом")
                && !tip.contains("срезов")
        };
        let tip_ratios: Vec<i32> = tips.iter().map(|(r, _)| *r).collect();
        self.check(
            !tips.is_empty() && tip_ratios == point_ratios && tips.iter().all(|(_, t)| well_formed(t)),
            format!(
                "подсказки столбцов: есть у множителей {tip_ratios:?}, точки у {point_ratios:?}; у каждого блока время, \
                 ø нагрузка, ø и 95 % над кристаллом, ▲ со множителем, нагрузкой и временем"
            ),
        );

        // Подсветка под курсором: курсор на плитке P — её линия последней (поверх), линия кластера бледнее.
        let order = |v: &View| {
            v.shown_stats
                .as_ref()
                .map(|s| s.series.iter().map(|l| (l.block, l.faded)).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        self.ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(px, py),
        });
        let lit = self.pump(|v| order(v) == [(e, true), (p, false)], 1.0);
        let drawn = order(&self.view.borrow());
        self.check(
            lit,
            format!("курсор на плитке {} — её линия поверх: {drawn:?}", p_block.name),
        );
        // Подсказка плитки: курсор стоит на ней — через полсекунды подсказка в окне.
        let shown = self.pump(|_| false, 0.7) || self.ui.get_tip_shown();
        self.check(shown, "курсор на плитке — подсказка показана");
        let shot = self.shot("tip");
        self.check(shot, "картинка с подсказкой плитки");
        self.ui.window().dispatch_event(WindowEvent::PointerExited);
        self.pump(|_| true, 0.0);
        let gone = !self.ui.get_tip_shown() && order(&self.view.borrow()) == [(e, false), (p, false)];
        self.check(gone, "курсор ушёл — подсказки нет, линии снова одинаковы");
        // Курсор у точки линии кластера на графике — подсвечена она.
        if let Some((x, y)) = self.chart_point(e) {
            self.ui.window().dispatch_event(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, y + 3.0),
            });
            let near = self.pump(|v| v.hovered == Some(e), 1.0);
            self.check(
                near,
                format!("курсор у линии {} на графике — подсвечена она", e_block.name),
            );
            self.ui.window().dispatch_event(WindowEvent::PointerExited);
            self.pump(|_| true, 0.0);
        } else {
            self.check(false, "у линии кластера нет точки на графике");
        }
        // Закреплённая подсветка: последний добавленный (P) поверх, без курсора.
        // Переключатель — щелчком по его кнопке, как владелец: кнопка должна быть в окне и попадать под щелчок.
        let (pin_x, pin_y) = (self.ui.get_pin_chip_x(), self.ui.get_pin_chip_y());
        let inside =
            pin_x > 0.0 && pin_y > 0.0 && pin_x < units(self.size.0 as usize) && pin_y < units(self.size.1 as usize);
        self.check(inside, format!("кнопка «закреплённая» в окне: {pin_x:.0}, {pin_y:.0}"));
        self.click(pin_x, pin_y, PointerEventButton::Left);
        let pinned = self.pump(|v| order(v) == [(e, true), (p, false)], 1.0);
        self.check(
            pinned && self.ui.get_pin_mode(),
            format!("закреплённая подсветка — последний добавленный {} поверх", p_block.name),
        );
        let shot = self.shot("pin");
        self.check(shot, "картинка закреплённой подсветки");
        // Щелчок по линии кластера закрепляет её.
        if let Some((x, y)) = self.chart_point(e) {
            self.click(x, y + 3.0, PointerEventButton::Left);
            let moved = self.pump(|v| order(v) == [(p, true), (e, false)], 1.0);
            self.check(
                moved,
                format!("щелчок по линии {} на графике закрепил её", e_block.name),
            );
        }
        let (hover_x, hover_y) = (self.ui.get_hover_chip_x(), self.ui.get_hover_chip_y());
        self.click(hover_x, hover_y, PointerEventButton::Left);
        let back = self.pump(|v| !v.pin_mode, 1.0);
        self.check(back, "щелчок по «под курсором» вернул подсветку под курсором");
    }

    /// Точка ø средней линии блока на графике — координаты окна; None — у блока нет точек.
    fn chart_point(&self, block: usize) -> Option<(f32, f32)> {
        let view = self.view.borrow();
        let point = view
            .shown_stats
            .as_ref()?
            .series
            .iter()
            .find(|l| l.block == block)?
            .points
            .first()?
            .clone();
        // Поле графика — как его кладёт окно (ui/app.slint, plot-*); места точек уже повёрнуты (оси местами).
        let ui = &self.ui;
        Some((
            ui.get_plot_x() + point.x * ui.get_plot_w(),
            ui.get_plot_y() + (1.0 - point.y) * ui.get_plot_h(),
        ))
    }

    /// Цвет блока, как его даёт окно (src/chart.rs): у блока свой.
    fn colour(&self, block: usize) -> u32 {
        self.view
            .borrow()
            .hello
            .as_ref()
            .map_or(0, |h| chart::color_of(&h.blocks, block))
    }

    /// Щелчок по кнопке переключателя вида и ожидание, что окно его приняло.
    fn chip(&mut self, at: (f32, f32), done: impl Fn(&View) -> bool, what: &str) -> bool {
        self.click(at.0, at.1, PointerEventButton::Left);
        let ok = self.pump(done, 1.0);
        self.check(ok, what)
    }

    /// Курсор — в столбец множителя номер `i` по счёту на оси (при осях местами — в строку), на доле `along` поля
    /// поперёк столбца; через полсекунды — подсказка. Она должна лечь в поле графика, рядом с курсором, не на шкалы
    /// (подписи слева и снизу) и не на столбец под курсором (владелец 15.10: «закрывает саму скалу… не вижу, где я
    /// стою»).
    fn chart_tip(&mut self, i: usize, along: f32, what: &str) -> bool {
        let ui = &self.ui;
        let (px, py, pw, ph) = (ui.get_plot_x(), ui.get_plot_y(), ui.get_plot_w(), ui.get_plot_h());
        let swap = ui.get_chart_swap();
        // Столбец под курсором — по геометрии графика (chart::Frame::band, доли поля, y снизу вверх) в пиксели окна.
        let (lo, hi, argument_range) = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .map(|s| {
                (
                    s.columns.first().map_or(0, |c| c.ratio),
                    s.columns.last().map_or(0, |c| c.ratio),
                    s.argument_range,
                )
            })
            .unwrap_or_default();
        let frame = chart::Frame {
            argument_range,
            lo,
            hi,
            t0: 0.0,
            t1: 1.0,
            swap,
        };
        let (bx, by, bw, bh) = frame.band(lo + i32::try_from(i).unwrap_or(0));
        let column = (px + f(bx) * pw, py + (1.0 - f(by + bh)) * ph, f(bw) * pw, f(bh) * ph);
        let cursor = if swap {
            (px + along * pw, column.1 + column.3 / 2.0)
        } else {
            (column.0 + column.2 / 2.0, py + (1.0 - along) * ph)
        };
        self.ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(cursor.0, cursor.1),
        });
        self.pump(|_| false, 0.7);
        let shown = self.ui.get_tip_shown();
        let tip = (
            self.ui.get_tip_box_x(),
            self.ui.get_tip_box_y(),
            self.ui.get_tip_box_w(),
            self.ui.get_tip_box_h(),
        );
        let chart_x = self.ui.get_chart_x();
        let scales = [
            (chart_x, py, px - chart_x, ph),                 // подписи слева
            (chart_x, py + ph, self.ui.get_chart_w(), 34.0), // подписи снизу
        ];
        let meets = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| {
            a.0 < b.0 + b.2 - 0.5 && b.0 < a.0 + a.2 - 0.5 && a.1 < b.1 + b.3 - 0.5 && b.1 < a.1 + a.3 - 0.5
        };
        let clear = !meets(tip, column) && scales.iter().all(|&r| !meets(tip, r));
        let inside =
            tip.0 >= px - 0.5 && tip.0 + tip.2 <= px + pw + 0.5 && tip.1 >= 0.0 && tip.1 + tip.3 <= py + ph + 0.5;
        self.check(
            shown && clear && inside,
            format!(
                "{what}: подсказка {:.0},{:.0} {:.0}×{:.0}; {} {:.0},{:.0} {:.0}×{:.0}; поле {px:.0},{py:.0} {pw:.0}×{ph:.0}; курсор {:.0},{:.0} — не на шкалах и не на {}, внутри поля",
                tip.0,
                tip.1,
                tip.2,
                tip.3,
                if swap { "строка" } else { "столбец" },
                column.0,
                column.1,
                column.2,
                column.3,
                cursor.0,
                cursor.1,
                if swap { "строке" } else { "столбце" },
            ),
        )
    }

    /// Номера столбцов (по счёту на оси) с подсказкой — где у выбранных блоков есть срезы.
    fn tipped_columns(&self) -> Vec<usize> {
        self.view
            .borrow()
            .shown_stats
            .as_ref()
            .map(|s| {
                s.columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !c.tip.is_empty())
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Курсор уходит из окна: подсказки и подсветки нет.
    fn leave(&mut self) {
        self.ui.window().dispatch_event(WindowEvent::PointerExited);
        self.pump(|_| true, 0.0);
    }

    /// Щелчки по плиткам, пока в сравнении не станут ровно `want` (по порядку: недостающие добавляются в конец).
    fn compare_exactly(&mut self, want: &[usize]) -> bool {
        let now = self.view.borrow().compared.clone();
        for b in now.iter().filter(|b| !want.contains(b)) {
            let (x, y) = self.tile_center(*b);
            self.click(x, y, PointerEventButton::Left);
        }
        for b in want {
            if !self.view.borrow().compared.contains(b) {
                let (x, y) = self.tile_center(*b);
                self.click(x, y, PointerEventButton::Left);
            }
        }
        let want = want.to_vec();
        self.pump(move |v| v.compared == want, 2.0)
    }

    /// Шаг 2б: вид графика (владелец 15.10). Шкала над кристаллом по умолчанию и абсолютная переключателем; линии ø /
    /// 95 % / обе; сравнение всех блоков сразу — двенадцать цветов двумя семьями; подсказка столбца не ложится на шкалы
    /// и на столбец под курсором; оси местами — подсветка линии и подсказка работают так же. В конце — снова кластер
    /// и P-ядро, обычные оси, обе линии, шкала над кристаллом.
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario of switches, hovers and pictures, read top to bottom"
    )]
    fn chart_views(&mut self, p_block: &BlockInfo, e_block: &BlockInfo, expected: Option<&Synthetic>) {
        let (p, e) = (p_block.index, e_block.index);
        let ticks = |c: &Check| {
            c.ui.get_chart_ticks()
                .iter()
                .map(|t| t.text.to_string())
                .collect::<Vec<_>>()
        };
        // Шкала над кристаллом (её выбрал шаг `own_view`): 0 на оси, подписи со знаком.
        let above = self.view.borrow().scale == Scale::AboveDie && self.ui.get_chart_view() == 1;
        let first = ticks(self);
        self.check(
            above
                && !first.is_empty()
                && first.iter().all(|t| t.ends_with('°'))
                && self.ui.get_chart_value_title().contains("над кристаллом"),
            format!("шкала над кристаллом: деления {first:?}"),
        );
        if let Some(s) = expected {
            let digest = self.view.borrow().digest.clone();
            let tally = digest.as_ref().and_then(|d| d.tally.blocks.get(&e_block.name).cloned());
            // Средние над кристаллом — как у генератора (он считает их сам, по строкам, которые пишет).
            let got: Vec<(i32, f64)> = tally
                .as_ref()
                .map(|t| {
                    t.above
                        .iter()
                        .map(|(r, b)| (*r, b.mean().unwrap_or(f64::NAN)))
                        .collect()
                })
                .unwrap_or_default();
            #[expect(clippy::cast_precision_loss, reason = "milliseconds of a synthetic day")]
            let want: Vec<(i32, f64)> = s.above.iter().map(|(r, (sum, ms))| (*r, sum / *ms as f64)).collect();
            let same = got.len() == want.len()
                && got
                    .iter()
                    .zip(&want)
                    .all(|(a, b)| a.0 == b.0 && (a.1 - b.1).abs() < 1e-6);
            self.check(
                same,
                format!(
                    "ø над кристаллом у {} по корзинам — как у генератора: {}",
                    e_block.name,
                    got.iter()
                        .map(|(r, m)| format!("×{r} {m:+.2}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            );
            // 95 %: процентиль гистограммы — не дальше половины ячейки от точного по строкам генератора.
            let mut worst = 0.0f64;
            let mut all_there = true;
            for (ratio, rows) in &s.above_rows {
                let exact = exact_quantile(rows, samples::P95);
                let got = tally
                    .as_ref()
                    .and_then(|t| t.above_hist.get(ratio))
                    .and_then(|h| h.quantile(samples::P95));
                match (exact, got) {
                    (Some(x), Some(g)) => worst = worst.max((x - g).abs()),
                    _ => all_there = false,
                }
            }
            self.check(
                all_there && worst <= samples::ABOVE_CELL / 2.0 + 1e-9,
                format!(
                    "95 % над кристаллом у {} — от точного по строкам не дальше {worst:.3} °C (половина ячейки — {:.3})",
                    e_block.name,
                    samples::ABOVE_CELL / 2.0
                ),
            );
        }
        // Абсолютная — щелчком: 40–100 °C, отсечка снова возможна.
        let before = self.view.borrow().shown_stats.clone();
        let abs = (self.ui.get_abs_chip_x(), self.ui.get_abs_chip_y());
        self.chip(
            abs,
            |v| v.scale == Scale::Absolute,
            "щелчок по «абсолютная» — шкала абсолютной температуры",
        );
        let now = ticks(self);
        let changed = before != self.view.borrow().shown_stats;
        self.check(
            !now.is_empty()
                && now.iter().all(|t| t.ends_with('°'))
                && changed
                && self
                    .view
                    .borrow()
                    .shown_stats
                    .as_ref()
                    .and_then(|s| s.value_range)
                    .is_some(),
            format!("деления абсолютной шкалы {now:?}; нарисованное изменилось: {changed}"),
        );
        let shot = self.shot("abs");
        self.check(shot, "картинка абсолютной шкалы");
        let above_at = (self.ui.get_above_chip_x(), self.ui.get_above_chip_y());
        self.chip(
            above_at,
            |v| v.scale == Scale::AboveDie,
            "щелчок по «над кристаллом» вернул шкалу над кристаллом",
        );

        // Линии ø и 95 % — два независимых выключателя (подробно — шаг 2г, `curve_switches`): скрыть одну и вернуть.
        let drawn = |v: &View| {
            v.shown_stats.as_ref().map_or((false, false), |s| {
                (
                    s.series.iter().any(|l| !l.mean.is_empty()),
                    s.series.iter().any(|l| !l.p95.is_empty()),
                )
            })
        };
        let at = (self.ui.get_p95_chip_x(), self.ui.get_p95_chip_y());
        self.chip(
            at,
            |v| v.curves == Curves::Mean,
            "щелчок по «95 %» — 95 % скрыты, ø остались",
        );
        let mean_only =
            drawn(&self.view.borrow()) == (true, false) && self.ui.get_chart_mean() && !self.ui.get_chart_p95();
        self.chip(
            at,
            |v| v.curves == Curves::Both,
            "второй щелчок по «95 %» — снова обе линии",
        );
        let both = drawn(&self.view.borrow()) == (true, true) && self.ui.get_chart_mean() && self.ui.get_chart_p95();
        self.check(
            mean_only && both,
            format!("нарисованы: только ø {mean_only}, обе {both}"),
        );

        // Все блоки сразу: двенадцать цветов, P — тёплые, E — холодные, обводка плитки — цвет линии.
        let blocks: Vec<BlockInfo> = self
            .view
            .borrow()
            .hello
            .as_ref()
            .map(|h| h.blocks.clone())
            .unwrap_or_default();
        let mut every: Vec<usize> = vec![e, p];
        every.extend(blocks.iter().map(|b| b.index).filter(|&b| b != e && b != p));
        let all = self.compare_exactly(&every);
        let legend = self.ui.get_stats_legend().row_count();
        let series = self.ui.get_chart_series().row_count();
        let outlines: Vec<(bool, bool)> = blocks
            .iter()
            .map(|b| {
                let colour = self.colour(b.index);
                let family = if b.kind == "E" {
                    E_COLORS.contains(&colour)
                } else {
                    P_COLORS.contains(&colour)
                };
                let framed = self
                    .view
                    .borrow()
                    .shown(b.index)
                    .is_some_and(|t| t.selected && t.outline == crate::panels::rgb(colour));
                (family, framed)
            })
            .collect();
        let mut colours: Vec<u32> = blocks.iter().map(|b| self.colour(b.index)).collect();
        colours.sort_unstable();
        colours.dedup();
        self.check(
            all && legend == blocks.len()
                && series == blocks.len()
                && colours.len() == blocks.len()
                && outlines.iter().all(|&(f, o)| f && o),
            format!(
                "в сравнении все {} блоков: в легенде {legend}, линий {series}, разных цветов {}; цвет по семье и обводка плитки — у {} из {}",
                blocks.len(),
                colours.len(),
                outlines.iter().filter(|&&(f, o)| f && o).count(),
                blocks.len()
            ),
        );
        let shot = self.shot("all");
        self.check(shot, "картинка: все блоки, шкала над кристаллом, обе линии");

        // Подсказка столбца с двенадцатью блоками: у левого края, в середине и у правого края оси.
        let tipped = self.tipped_columns();
        if let (Some(&left), Some(&right)) = (tipped.first(), tipped.last()) {
            let middle = tipped[tipped.len() / 2];
            self.chart_tip(left, 0.6, "подсказка левого столбца");
            self.chart_tip(middle, 0.3, "подсказка среднего столбца");
            self.chart_tip(right, 0.8, "подсказка правого столбца");
            let shot = self.shot("chart-tip");
            self.check(shot, "картинка с подсказкой столбца у правого края");
            self.leave();
        } else {
            self.check(false, "у столбцов нет подсказок");
        }

        // Оси местами: множитель по вертикали; подсветка линии и подсказка — так же.
        let swap_at = (self.ui.get_swap_chip_x(), self.ui.get_swap_chip_y());
        self.chip(swap_at, |v| v.swap, "щелчок по «°C» — температура по горизонтали");
        let point = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .and_then(|s| s.series.iter().find(|l| l.block == e))
            .and_then(|l| l.points.first().cloned());
        let turned = point.as_ref().is_some_and(|q| {
            let frame_ok = self.ui.get_chart_swap();
            // Точка ø у кластера при осях местами: по вертикали — множитель (доля оси множителя), по горизонтали —
            // температура.
            let columns = self
                .view
                .borrow()
                .shown_stats
                .as_ref()
                .and_then(|s| s.columns.iter().find(|c| c.ratio == q.ratio).map(|c| c.at));
            frame_ok && columns.is_some_and(|at| (at - q.y).abs() < 1e-6)
        });
        self.check(
            turned,
            "оси местами: точка кластера по вертикали стоит на своём множителе",
        );
        let shot = self.shot("swap");
        self.check(shot, "картинка: оси местами, все блоки");
        let tipped = self.tipped_columns();
        if let (Some(&low), Some(&high)) = (tipped.first(), tipped.last()) {
            self.chart_tip(low, 0.5, "оси местами: подсказка нижней строки");
            self.chart_tip(high, 0.5, "оси местами: подсказка верхней строки");
            let shot = self.shot("swap-tip");
            self.check(shot, "картинка: оси местами, подсказка строки");
            self.leave();
        }
        // Подсветка линии при осях местами — на двух блоках, как при обычных осях: среди двенадцати линии лежат
        // друг на друге, и ближайшей к курсору может честно оказаться чужая.
        let back = self.compare_exactly(&[e, p]);
        self.check(back, "в сравнении снова кластер и P-ядро");
        if let Some((x, y)) = self.chart_point(e) {
            self.ui.window().dispatch_event(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x + 3.0, y + 2.0),
            });
            let near = self.pump(|v| v.hovered == Some(e), 1.0);
            self.check(
                near,
                format!("оси местами: курсор у линии {} — подсвечена она", e_block.name),
            );
            self.leave();
        } else {
            self.check(false, "у линии кластера нет точки на графике");
        }
        let plain_at = (self.ui.get_plain_chip_x(), self.ui.get_plain_chip_y());
        self.chip(
            plain_at,
            |v| !v.swap,
            "повторный щелчок «Поменять оси» возвращает множитель по горизонтали",
        );
        self.direct_chart_zoom();
    }

    fn scale_scroll(&mut self, argument: bool, delta: f32) {
        let (x, y) = if argument {
            (self.ui.get_range_arg_wheel_x(), self.ui.get_range_arg_wheel_y())
        } else {
            (self.ui.get_range_value_wheel_x(), self.ui.get_range_value_wheel_y())
        };
        self.ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(x, y),
            delta_x: 0.0,
            delta_y: delta,
        });
        self.pump(|_| true, 0.0);
    }

    fn scale_auto_click(&mut self, argument: bool) {
        let (x, y) = if argument {
            (self.ui.get_range_arg_auto_x(), self.ui.get_range_arg_auto_y())
        } else {
            (self.ui.get_range_value_auto_x(), self.ui.get_range_value_auto_y())
        };
        self.click(x, y, PointerEventButton::Left);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "independent axis gestures and preserved sibling state in one UI scenario"
    )]
    fn direct_chart_zoom(&mut self) {
        let ranges = |check: &Self| {
            check
                .view
                .borrow()
                .shown_stats
                .as_ref()
                .and_then(|s| s.argument_range.zip(s.value_range))
        };
        let span = |r: crate::plot::Range| r.max - r.min;
        let close = |a: f64, b: f64| (a - b).abs() <= b.abs().max(1.0) * 1e-9;
        if !self.ui.get_range_auto_arg() {
            self.scale_auto_click(true);
        }
        if !self.ui.get_range_auto_value() {
            self.scale_auto_click(false);
        }
        let Some((argument, value)) = ranges(self) else {
            self.check(false, "нет данных для проверки прямого зума");
            return;
        };
        self.scale_scroll(true, 0.5);
        let small = ranges(self);
        self.check(
            small.is_some_and(|(a, v)| span(a) < span(argument) && v == value)
                && !self.ui.get_range_auto_arg()
                && self.ui.get_range_auto_value(),
            "дробное колесо над множителем меняет только его масштаб и его Авто",
        );
        self.scale_scroll(true, 0.0);
        self.check(ranges(self) == small, "нулевое движение колеса не меняет масштаб");
        self.scale_scroll(true, -0.5);
        self.check(
            ranges(self).is_some_and(|(a, v)| close(span(a), span(argument)) && v == value),
            "обратное движение колеса возвращает прежний масштаб",
        );
        self.scale_auto_click(true);
        self.scale_scroll(true, 2.0);
        let larger = ranges(self);
        self.check(
            larger
                .zip(small)
                .is_some_and(|((a, v), (s, _))| span(a) < span(s) && v == value),
            "величина движения колеса учитывается плавно, без округления до делений",
        );
        self.scale_auto_click(true);
        for _ in 0..4 {
            self.scale_scroll(true, 0.5);
        }
        self.check(
            ranges(self)
                .zip(larger)
                .is_some_and(|((a, v), (b, _))| close(span(a), span(b)) && v == value),
            "несколько малых движений дают тот же масштаб, что одно суммарное",
        );
        self.scale_auto_click(true);
        self.scale_scroll(false, 0.5);
        self.check(
            ranges(self).is_some_and(|(a, v)| a == argument && span(v) < span(value))
                && self.ui.get_range_auto_arg()
                && !self.ui.get_range_auto_value(),
            "колесо над значением меняет только его масштаб и его Авто",
        );
        self.click(
            self.ui.get_range_arg_plus_x(),
            self.ui.get_range_arg_wheel_y(),
            PointerEventButton::Left,
        );
        let manual = ranges(self);
        self.check(
            manual.is_some_and(|(a, v)| span(a) < span(argument) && span(v) < span(value))
                && !self.ui.get_range_auto_arg()
                && !self.ui.get_range_auto_value(),
            "кнопка плюс приближает выбранную ось",
        );
        let swap = (self.ui.get_swap_chip_x(), self.ui.get_swap_chip_y());
        self.click(swap.0, swap.1, PointerEventButton::Left);
        self.check(
            manual == ranges(self) && self.ui.get_chart_swap(),
            "перестановка осей сохраняет оба масштаба",
        );
        self.scale_scroll(true, 0.5);
        self.check(
            ranges(self)
                .zip(manual)
                .is_some_and(|((a, v), (old, other))| span(a) < span(old) && v == other),
            "после перестановки колесо множителя меняет ту же логическую ось",
        );
        let before_value = ranges(self);
        self.scale_scroll(false, 0.5);
        self.check(
            ranges(self)
                .zip(before_value)
                .is_some_and(|((a, v), (other, old))| a == other && span(v) < span(old)),
            "после перестановки колесо значения независимо меняет его ось",
        );
        self.click(swap.0, swap.1, PointerEventButton::Left);
        let before_minus = ranges(self);
        self.click(
            self.ui.get_range_value_minus_x(),
            self.ui.get_range_value_wheel_y(),
            PointerEventButton::Left,
        );
        self.check(
            ranges(self)
                .zip(before_minus)
                .is_some_and(|((a, v), (other, old))| a == other && span(v) > span(old)),
            "кнопка минус отдаляет выбранную ось, сохраняя другую",
        );
        let fit = (self.ui.get_range_value_fit_x(), self.ui.get_range_value_fit_y());
        self.click(fit.0, fit.1, PointerEventButton::Left);
        self.check(
            ranges(self).is_some_and(|(_, v)| v == value) && !self.ui.get_range_auto_value(),
            "Вписать подбирает масштаб один раз и оставляет Авто выключенным",
        );
        self.scale_auto_click(true);
        self.scale_auto_click(false);
        self.check(
            ranges(self) == Some((argument, value)) && self.ui.get_range_auto_arg() && self.ui.get_range_auto_value(),
            "Авто возвращает масштабы по выбранным данным",
        );
        let shot = self.shot("zoom-controls");
        self.check(shot, "картинка: прямой масштаб без ручных границ");
        self.leave();
    }
    /// Шаг 2в: кривые модели нагрева (DECISIONS #39). У каждого выбранного блока — кривая его цвета: сплошная над
    /// наблюдёнными множителями, пунктир за ними, температура не падает с множителем; в подсказке блока в легенде —
    /// модель, условия и ошибка (без ватт — бриф 16.10 «эффективность»); выключатель «модель» прячет и возвращает
    /// кривые, не трогая линий ø и 95 %. Картинка F-model.png.
    fn model_curves(&mut self, p_block: &BlockInfo, e_block: &BlockInfo) {
        let pair = [p_block.index, e_block.index];
        let paths = |view: &View, block: usize| {
            view.shown_stats
                .as_ref()
                .and_then(|shown| shown.series.iter().find(|l| l.block == block))
                .map(|l| (l.model.clone(), l.model_dash.clone(), !l.mean.is_empty())) // two path texts, a few times
                .unwrap_or_default()
        };
        let shown = |view: &View| {
            pair.iter().all(|&block| {
                let (solid, dash, _) = paths(view, block);
                !solid.is_empty() && !dash.is_empty()
            })
        };
        let on = self.pump(shown, 2.0) && self.ui.get_chart_model();
        let (solid, dash, _) = paths(&self.view.borrow(), p_block.index);
        // Точки путей в рамке 1000 × 1000 (y вниз): при обычных осях вдоль кривой x растёт, а y не растёт —
        // температура не падает с множителем.
        let numbers = |d: &str| -> Vec<(f64, f64)> {
            let v: Vec<f64> = d.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            v.as_chunks::<2>().0.iter().map(|c| (c[0], c[1])).collect()
        };
        let line = numbers(&solid);
        let monotone = line.len() >= 2 && line.windows(2).all(|w| w[1].0 >= w[0].0 && w[1].1 <= w[0].1 + 1e-6);
        self.check(
            on && monotone,
            format!(
                "кривые модели у {} и {}: сплошная ({} точек) и пунктир ({} штрихов); температура вдоль сплошной не падает: {monotone}",
                p_block.name,
                e_block.name,
                line.len(),
                dash.matches('M').count()
            ),
        );
        let tips: Vec<String> = self.ui.get_stats_legend().iter().map(|l| l.tip.to_string()).collect();
        let figure = tips.iter().all(|t| {
            t.contains("Модель нагрева: регрессия")
                && t.contains("условия кривой")
                && t.contains("ошибка на отложенных данных")
                && !t.contains("ватт")
                && !t.contains("°C на ступень")
        });
        let e_note = tips.iter().any(|t| t.contains("по первому ядру, за четыре ядра"));
        self.check(
            figure && e_note,
            format!(
                "в подсказке каждого блока в легенде — модель нагрева, условия кривой и ошибка, без ватт; у кластера — что нагрузка по первому ядру: {}",
                tips.first().map(|t| t.lines().skip(3).collect::<Vec<_>>().join(" / ")).unwrap_or_default()
            ),
        );
        let shot = self.shot("model");
        self.check(
            shot,
            "картинка: кривые модели у кластера и P-ядра, шкала над кристаллом",
        );
        let at = (self.ui.get_model_chip_x(), self.ui.get_model_chip_y());
        self.chip(at, |v| !v.model, "щелчок по «модель» — кривые модели скрыты");
        let hidden = {
            let view = self.view.borrow();
            pair.iter().all(|&block| {
                let (solid, dash, mean) = paths(&view, block);
                solid.is_empty() && dash.is_empty() && mean
            })
        };
        self.check(
            hidden && !self.ui.get_chart_model(),
            "кривых модели нет, линии ø остались",
        );
        self.chip(at, |v| v.model, "второй щелчок по «модель» — кривые снова видны");
        let back = shown(&self.view.borrow());
        self.check(back, "кривые модели вернулись");
    }

    /// Шаг 2г: линии «ø», «95 %» и «модель» — три независимых выключателя (владелец 16.10: «зачем эта кнопка оба…
    /// сделать два независимых выключателя»; «модель» и была своим). Каждый щелчок настоящим событием мыши показывает
    /// или скрывает только свою линию; все выключены — пустой график с осями; подсветка под курсором работает с тем, что
    /// видно: при одной модели — по её кривой. Картинки F-curves-model.png и F-curves-none.png.
    fn curve_switches(&mut self, e_block: &BlockInfo) {
        let e = e_block.index;
        let lines = |v: &View| {
            v.shown_stats.as_ref().map_or((false, false, false), |s| {
                (
                    s.series.iter().any(|l| !l.mean.is_empty()),
                    s.series.iter().any(|l| !l.p95.is_empty()),
                    s.series.iter().any(|l| !l.model.is_empty()),
                )
            })
        };
        let switches = |c: &Check| (c.ui.get_chart_mean(), c.ui.get_chart_p95(), c.ui.get_chart_model());
        let start = lines(&self.view.borrow());
        self.check(
            start == (true, true, true) && switches(self) == (true, true, true),
            format!("перед шагом видны все три линии и включены их выключатели: {start:?}"),
        );
        let places = [
            ("ø", (self.ui.get_mean_chip_x(), self.ui.get_mean_chip_y())),
            ("95 %", (self.ui.get_p95_chip_x(), self.ui.get_p95_chip_y())),
            ("модель", (self.ui.get_model_chip_x(), self.ui.get_model_chip_y())),
        ];
        let flip = |state: (bool, bool, bool), k: usize| match k {
            0 => (!state.0, state.1, state.2),
            1 => (state.0, !state.1, state.2),
            _ => (state.0, state.1, !state.2),
        };
        // ø, 95 % — осталась одна модель; модель — пусто; потом в том же порядке обратно.
        let mut state = start;
        for (step, k) in [0, 1, 2, 0, 1, 2].into_iter().enumerate() {
            let (name, at) = places[k];
            let want = flip(state, k);
            self.click(at.0, at.1, PointerEventButton::Left);
            let took = self.pump(|v| (v.curves.mean(), v.curves.p95(), v.model) == want, 1.0);
            let (got, ui) = (lines(&self.view.borrow()), switches(self));
            self.check(
                took && got == want && ui == want,
                format!("щелчок по «{name}»: линии (ø, 95 %, модель) {state:?} → {got:?}, выключатели {ui:?} — изменилась только своя"),
            );
            state = got;
            if step == 1 {
                // Видна одна модель: курсор на кривой модели кластера — подсвечена она.
                let aim = {
                    let view = self.view.borrow();
                    view.shown_stats
                        .as_ref()
                        .and_then(|s| s.series.iter().find(|l| l.block == e))
                        .and_then(|l| l.model_line.iter().max_by_key(|run| run.len()).cloned())
                        .and_then(|run| run.get(run.len() / 2).copied())
                };
                if let Some((x, y)) = aim {
                    let ui = &self.ui;
                    let at = LogicalPosition::new(
                        ui.get_plot_x() + x * ui.get_plot_w(),
                        ui.get_plot_y() + (1.0 - y) * ui.get_plot_h(),
                    );
                    self.ui
                        .window()
                        .dispatch_event(WindowEvent::PointerMoved { position: at });
                    let lit = self.pump(|v| v.hovered == Some(e), 1.0);
                    self.check(
                        lit,
                        format!(
                            "ø и 95 % скрыты: курсор на кривой модели {} — подсвечена она",
                            e_block.name
                        ),
                    );
                    let shot = self.shot("curves-model");
                    self.check(shot, "картинка: видна одна модель, кривая кластера подсвечена");
                    self.ui.window().dispatch_event(WindowEvent::PointerExited);
                    self.pump(|_| true, 0.0);
                } else {
                    self.check(
                        false,
                        format!("у кластера {} нет кривой модели на графике", e_block.name),
                    );
                }
            }
            if step == 2 {
                // Всё выключено: пустой график с осями — деления и столбцы те же, надписи «нет данных» нет.
                let ui = &self.ui;
                let empty = ui.get_chart_empty().is_empty()
                    && ui.get_chart_ticks().row_count() > 0
                    && ui.get_chart_cols().row_count() > 0
                    && ui
                        .get_chart_series()
                        .iter()
                        .all(|s| s.mean.row_count() == 0 && s.p95.row_count() == 0);
                self.check(
                    empty,
                    format!(
                        "все выключены: пустой график с осями — делений {}, столбцов {}",
                        ui.get_chart_ticks().row_count(),
                        ui.get_chart_cols().row_count()
                    ),
                );
                let shot = self.shot("curves-none");
                self.check(shot, "картинка: все линии выключены — пустой график с осями");
            }
        }
        self.check(state == start, format!("после шага — снова все три линии: {state:?}"));
    }

    /// Шаг 3: ø под нагрузкой на плитке; кнопка «▲ ø» сбрасывает наибольшую и средние. Нагрузку даёт команда
    /// движку напрямую (не жест окна: у окна нагрузки нет) — только чтобы движок на имитации посчитал средние.
    fn means_and_reset(&mut self, p_block: &BlockInfo) {
        let first = p_block.first;
        let toggle = |check: &mut Check| {
            let mut args = Map::new();
            args.insert("block".into(), p_block.index.into());
            check
                .view
                .borrow_mut()
                .order("load.toggle", args, "Нагрузка не изменена");
        };
        toggle(self);
        let mean_shown = |v: &View| {
            v.shown(p_block.index)
                .is_some_and(|t| t.mean.starts_with("ø ") && !t.mean.starts_with("ø –") && t.mean.contains('×'))
        };
        let means = self.pump(mean_shown, 8.0);
        let text = self
            .view
            .borrow()
            .shown(p_block.index)
            .map(|t| t.mean.to_string())
            .unwrap_or_default();
        self.check(
            means,
            format!("плитка {} показывает ø под нагрузкой: «{text}»", p_block.name),
        );
        toggle(self);
        let off = self.pump(
            |v| {
                v.state
                    .as_ref()
                    .is_some_and(|s| !s.load.get(first).copied().unwrap_or(false))
            },
            5.0,
        );
        self.check(off, "нагрузка снята (командой движку)");
        // Остыть: максимум должен быть выше того, что стоит сейчас, иначе сброс не виден.
        self.pump(|_| false, 3.0);
        let peak = |v: &View| v.state.as_ref().and_then(|s| s.max.get(first).copied().flatten());
        let before = peak(&self.view.borrow()).unwrap_or(0.0);
        let die_panel_w = self.ui.get_die_w() + 2.0 * self.ui.get_pad();
        let (x, y) = (
            self.ui.get_margin() + die_panel_w - self.ui.get_pad() - 32.0,
            self.ui.get_body_y() + 16.0,
        );
        self.click(x, y, PointerEventButton::Left);
        let reset = self.pump(
            |v| {
                peak(v).is_some_and(|p| p < before) && v.shown(p_block.index).is_some_and(|t| t.mean.starts_with("ø –"))
            },
            5.0,
        );
        let after = peak(&self.view.borrow()).unwrap_or(0.0);
        self.check(
            reset,
            format!("кнопка «▲ ø» послала max.reset: наибольшая {before:.0} → {after:.0} °C, ø сброшена"),
        );
    }

    /// 4. Правая кнопка по плитке E-блока — меню; пункт на ступень ниже заводского ставит потолок, затем он возвращается.
    fn limit_menu(&mut self, limits: &Limits, e_block: &BlockInfo) {
        let (x, y) = self.tile_center(e_block.index);
        self.click(x, y, PointerEventButton::Right);
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
        self.pump(|v| v.profiles.is_some(), 5.0);
        let expected = self
            .profile_value(e_block.first)
            .map_or_else(String::new, |p| panels::signed(target - p));
        let delta = self.delta(e_block.index);
        self.check(
            delta == expected,
            format!("на плитке разница с выбранным профилем: «{delta}»"),
        );
        let mut args = Map::new();
        args.insert("block".into(), e_block.index.into());
        args.insert("ratio".into(), before.into());
        self.view.borrow_mut().order("limit.set", args, "Потолок не изменён");
        let back = self.pump(|v| limit(v, e_block.first) == Some(before), 5.0);
        self.check(back, format!("потолок возвращён на ×{before}"));
    }

    fn set_row(&self, key: &str) -> Option<(f32, f32)> {
        let keys = self.view.borrow().set_keys();
        let i = keys.iter().position(|k| k == key)?;
        Some((
            self.ui.get_prof_x() + self.ui.get_pad() + 80.0,
            self.ui.get_sets_y() + units(i) * self.ui.get_set_h() + self.ui.get_set_h() / 2.0,
        ))
    }

    /// Кнопка панели профилей: ряд (y) и место в ряду.
    fn button(&self, y: f32, place: usize) -> (f32, f32) {
        let (w, gap) = (self.ui.get_btn_w(), self.ui.get_btn_gap());
        (
            self.ui.get_prof_x() + self.ui.get_pad() + units(place) * (w + gap) + w / 2.0,
            y + self.ui.get_btn_h() / 2.0,
        )
    }

    fn press(&mut self, (x, y): (f32, f32)) {
        self.click(x, y, PointerEventButton::Left);
    }

    /// Щелчок по строке списка: выбор профиля.
    fn choose(&mut self, key: &str) -> bool {
        let Some(row) = self.set_row(key) else {
            self.check(false, format!("строки «{key}» в списке нет"));
            return false;
        };
        self.press(row);
        self.view.borrow().chosen.as_deref() == Some(key)
    }

    /// Потолок выбранного профиля у ядра; None — профиль не выбран или его потолков нет.
    fn profile_value(&self, first: usize) -> Option<i64> {
        let v = self.view.borrow();
        panels::chosen_ratios(v.profiles.as_ref(), v.limits.as_ref(), v.chosen.as_deref())
            .and_then(|r| r.get(first).copied())
    }

    /// Разница на плитке блока, как она показана.
    fn delta(&self, block: usize) -> String {
        self.view
            .borrow()
            .shown(block)
            .map(|t| t.delta.to_string())
            .unwrap_or_default()
    }

    /// Строка панели профилей и доступность кнопок, как они показаны: (строка, применить, сохранить, отметка доступна,
    /// отметка стоит).
    fn panel(&self) -> (String, bool, bool, bool, bool) {
        let ui = &self.ui;
        (
            ui.get_prof_status().to_string(),
            ui.get_can_apply(),
            ui.get_can_save(),
            ui.get_can_startup(),
            ui.get_startup_on(),
        )
    }

    /// Профиль запуска по теме профилей: название; None — его нет.
    fn startup_name(&self) -> Option<String> {
        let v = self.view.borrow();
        let p = v.profiles.as_ref()?;
        p.startup.exists.then(|| p.startup.name.clone()).flatten()
    }

    /// Шаг 5 — профили щелчками (DECISIONS #38): окно открылось на профиле запуска; выбор строки ничего не ставит и ничего не
    /// шлёт, а строка и плитки показывают разницу выбранного с текущим; «сохранить как», «применить» и «сохранить»
    /// делают их одинаковыми и сереют; «удалить». Против программы у часов — ещё отметка «загружать при старте Windows»:
    /// доступна только у сохранённого профиля, переходит на другой, снимается; в конце — как была. Потолки — как были.
    #[expect(
        clippy::too_many_lines,
        reason = "one scripted round trip, step by step, read top to bottom"
    )]
    fn profiles(&mut self, limits: &Limits, e_block: &BlockInfo) {
        let arrived = self.pump(|v| v.profiles.is_some(), 5.0);
        if !self.check(arrived && self.ui.get_has_profiles(), "профили пришли, панель показана")
        {
            return;
        }
        let (chosen, wanted, logon) = {
            let v = self.view.borrow();
            (
                v.chosen.clone(),
                v.profiles.as_ref().map(panels::default_choice),
                self.startup_name(),
            )
        };
        self.check(
            chosen.is_some() && chosen == wanted,
            format!("выбран профиль запуска (или совпадающий с текущим, или заводские): {chosen:?}, профиль запуска {logon:?}"),
        );
        let first = e_block.first;
        let fused = limits.fused[first];
        let Some(start) = limit(&self.view.borrow(), first) else {
            self.check(false, format!("потолок {} неизвестен", e_block.name));
            return;
        };
        let target = if start == fused - 1 { fused - 2 } else { fused - 1 };
        let name = &e_block.name;

        // Текущее состояние: потолок кластера на ступень ниже — то, что будет сохранено.
        let mut args = Map::new();
        args.insert("block".into(), e_block.index.into());
        args.insert("ratio".into(), target.into());
        self.view.borrow_mut().order("limit.set", args, "Потолок не изменён");
        let lowered = self.pump(|v| limit(v, first) == Some(target), 5.0);
        self.check(lowered, format!("текущее состояние: {name} ×{target}"));

        // Сохранить как: щелчок в поле, название с клавиатуры, кнопка — новый профиль выбран и совпадает с текущим.
        let field = (
            self.ui.get_prof_x() + self.ui.get_pad() + 40.0,
            self.ui.get_row_c_y() + self.ui.get_btn_h() / 2.0,
        );
        self.press(field);
        self.type_text(PROFILE);
        let typed = self.ui.get_prof_name().to_string();
        self.check(
            typed == PROFILE && self.ui.get_can_save_as(),
            format!("в поле названия «{typed}», «сохранить как» доступна"),
        );
        let save_as_button = (
            self.ui.get_prof_x() + self.ui.get_prof_w() - self.ui.get_pad() - 64.0,
            self.ui.get_row_c_y() + self.ui.get_btn_h() / 2.0,
        );
        self.press(save_as_button);
        let saved = self.pump(
            |v| {
                v.chosen.as_deref() == Some(PROFILE)
                    && v.profiles.as_ref().is_some_and(|p| {
                        p.standing.as_deref() == Some(PROFILE) && p.sets.iter().any(|s| s.name == PROFILE)
                    })
            },
            5.0,
        );
        let (status, apply, save, ..) = self.panel();
        self.check(
            saved && self.ui.get_prof_name().is_empty() && status == format!("«{PROFILE}» — совпадает с текущим") && !apply && !save,
            format!("«сохранить как» записал «{PROFILE}» и выбрал его: «{status}», применить {apply}, сохранить {save} — обе серые"),
        );
        let delta = self.delta(e_block.index);
        self.check(delta.is_empty(), format!("на плитке разницы нет: «{delta}»"));

        // Выбор строки ничего не ставит и не шлёт: «заводские» — только другая строка и плитки.
        let sent = self.view.borrow().sent;
        let picked = self.choose("");
        self.pump(|_| false, 0.6);
        let (status, apply, save, can_mark, _) = self.panel();
        let delta = self.delta(e_block.index);
        let still = limit(&self.view.borrow(), first) == Some(target) && self.view.borrow().sent == sent;
        self.check(
            picked
                && still
                && status.starts_with("заводские — отличается:")
                && apply
                && !save
                && !can_mark
                && delta == panels::signed(target - fused),
            format!(
                "щелчок по «заводские»: ничего не поставлено и не послано ({still}); «{status}»; на плитке «{delta}»; применить {apply}, сохранить {save}, отметка при старте {can_mark}"
            ),
        );
        let shot = self.shot("profiles");
        self.check(shot, "картинка: выбраны заводские, разница с текущим");

        // Текущее состояние меняется — выбранный профиль показывает разницу, обе кнопки доступны.
        self.choose(PROFILE);
        let mut args = Map::new();
        args.insert("block".into(), e_block.index.into());
        args.insert("ratio".into(), start.into());
        self.view.borrow_mut().order("limit.set", args, "Потолок не изменён");
        self.pump(|v| limit(v, first) == Some(start), 5.0);
        let (status, apply, save, ..) = self.panel();
        let delta = self.delta(e_block.index);
        let expected = format!("«{PROFILE}» — отличается: {name} ×{target} → сейчас ×{start}");
        self.check(
            status == expected && apply && save && delta == panels::signed(start - target),
            format!("текущее изменилось: «{status}», на плитке «{delta}»; применить {apply}, сохранить {save}"),
        );

        // «Применить»: потолки профиля — в процессор; совпадает, обе серые.
        let apply_at = self.button(self.ui.get_row_b_y(), 0);
        self.press(apply_at);
        let applied = self.pump(|v| limit(v, first) == Some(target), 5.0);
        self.pump(|_| false, 0.3);
        let (status, apply, save, ..) = self.panel();
        self.check(
            applied && status == format!("«{PROFILE}» — совпадает с текущим") && !apply && !save,
            format!("«применить» поставил ×{target}: «{status}», применить {apply}, сохранить {save}"),
        );

        // «Сохранить»: текущее — в профиль; совпадает, обе серые.
        let mut args = Map::new();
        args.insert("block".into(), e_block.index.into());
        args.insert("ratio".into(), start.into());
        self.view.borrow_mut().order("limit.set", args, "Потолок не изменён");
        self.pump(
            |v| limit(v, first) == Some(start) && v.profile_shown().is_some_and(|p| p.can_save),
            5.0,
        );
        let save_button = self.button(self.ui.get_row_b_y(), 1);
        let enabled = self.ui.get_can_save();
        self.press(save_button);
        let rewritten = self.pump(
            |v| {
                v.profiles.as_ref().is_some_and(|p| {
                    p.sets
                        .iter()
                        .any(|s| s.name == PROFILE && s.ratios.get(first) == Some(&start))
                })
            },
            5.0,
        );
        self.pump(|_| false, 0.3);
        let (status, apply, save, ..) = self.panel();
        self.check(
            enabled && rewritten && status == format!("«{PROFILE}» — совпадает с текущим") && !apply && !save,
            format!("«сохранить» (была доступна: {enabled}) записал ×{start} в «{PROFILE}»: «{status}», обе серые"),
        );

        // Отметка «загружать при старте Windows».
        self.choose("");
        let factory = self.panel().3;
        self.choose(PROFILE);
        let mine = self.panel().3;
        self.check(
            !factory && mine,
            format!(
                "отметка «при старте Windows»: у «заводские» недоступна ({factory}), у сохранённого — доступна ({mine})"
            ),
        );
        if self.mode == Mode::Settings {
            self.startup_marks(logon.as_deref());
        }

        // «Удалить».
        self.choose(PROFILE);
        let delete = self.button(self.ui.get_row_b_y(), 2);
        self.press(delete);
        let deleted = self.pump(
            |v| {
                v.profiles
                    .as_ref()
                    .is_some_and(|p| !p.sets.iter().any(|s| s.name == PROFILE))
            },
            5.0,
        );
        let back = limit(&self.view.borrow(), first) == Some(start);
        self.check(
            deleted && back,
            format!("«удалить» убрал «{PROFILE}»; потолок {name} — как до шага (×{start}): {back}"),
        );
    }

    /// Отметка «загружать при старте Windows» щелчками (против программы у часов): ставится у «проверка окна», переходит
    /// на другой сохранённый профиль, снимается; в конце — профиль запуска, какой был (`logon`).
    fn startup_marks(&mut self, logon: Option<&str>) {
        let marked = |v: &View| {
            v.profile_shown()
                .map(|p| {
                    p.rows
                        .iter()
                        .filter(|r| r.startup)
                        .map(|r| r.key.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let mark_at = (
            self.ui.get_prof_x() + self.ui.get_pad() + 7.0,
            self.ui.get_startup_y() + 11.0,
        );
        self.choose(PROFILE);
        self.press(mark_at);
        let on = self.pump(
            |v| marked(v) == [PROFILE] && v.profile_shown().is_some_and(|p| p.startup_on),
            5.0,
        );
        self.check(
            on && self.startup_name().as_deref() == Some(PROFILE),
            format!(
                "щелчок по отметке: «{PROFILE}» загружается при старте, знак ► только у него ({:?})",
                marked(&self.view.borrow())
            ),
        );
        let other = self
            .view
            .borrow()
            .profiles
            .as_ref()
            .and_then(|p| p.sets.iter().map(|s| s.name.clone()).find(|n| n != PROFILE));
        if let Some(other) = other {
            self.choose(&other);
            let before = self.panel().4;
            self.press(mark_at);
            let moved = self.pump(|v| marked(v) == [other.as_str()], 5.0);
            self.check(
                !before && moved && self.startup_name().as_deref() == Some(other.as_str()),
                format!(
                    "отметка у «{other}»: перешла на него, у «{PROFILE}» снята ({:?})",
                    marked(&self.view.borrow())
                ),
            );
            self.press(mark_at);
            let cleared = self.pump(|v| marked(v).is_empty(), 5.0);
            self.check(
                cleared && self.startup_name().is_none() && !self.panel().4,
                "снять отметку у отмеченного: при старте ничего не загружается, знаков ► нет",
            );
        } else {
            self.note("других сохранённых профилей нет — перенос отметки не проверен");
            self.choose(PROFILE);
            self.press(mark_at);
            let cleared = self.pump(|v| marked(v).is_empty(), 5.0);
            self.check(cleared && self.startup_name().is_none(), "снять отметку: знаков ► нет");
        }
        // Как было.
        if let Some(logon) = logon {
            self.choose(logon);
            self.press(mark_at);
            let again = self.pump(|v| marked(v) == [logon], 5.0);
            self.check(again, format!("профиль запуска снова «{logon}», как до шага"));
        }
    }

    /// Шаг 6: окно 1280×800, 1500×900 и по содержимому — разделы не налезают друг на друга и не выходят за окно;
    /// кнопки вида графика — в панели статистики, между легендой и графиком. При 1280 — несколько P- и E-блоков,
    /// подсказка и оси местами на картинках.
    fn sizes(&mut self, p_block: &BlockInfo, e_block: &BlockInfo) {
        // Несколько блоков: два P-ядра и два кластера, если есть.
        let blocks: Vec<BlockInfo> = self
            .view
            .borrow()
            .hello
            .as_ref()
            .map(|h| h.blocks.clone())
            .unwrap_or_default();
        let mut some = vec![e_block.index, p_block.index];
        some.extend(
            blocks
                .iter()
                .filter(|b| b.kind == "P" && b.index != p_block.index)
                .take(2)
                .map(|b| b.index),
        );
        some.extend(
            blocks
                .iter()
                .filter(|b| b.kind == "E" && b.index != e_block.index)
                .take(1)
                .map(|b| b.index),
        );
        let several = self.compare_exactly(&some);
        self.check(several, format!("для картинок размеров в сравнении блоки {some:?}"));
        for (name, size) in [
            ("1280", Some((1280, 800))),
            ("1500", Some((1500, 900))),
            ("natural", None),
        ] {
            let size = size.unwrap_or_else(|| {
                let (w, h) = (self.ui.get_natural_w(), self.ui.get_natural_h());
                (rounded(w), rounded(h))
            });
            self.resize(size);
            let ui = &self.ui;
            let (w, h) = (units_u32(size.0), units_u32(size.1));
            let die_right = ui.get_margin() + ui.get_die_w() + 2.0 * ui.get_pad();
            let fits = die_right <= ui.get_prof_x()
                && ui.get_prof_x() + ui.get_prof_w() <= ui.get_stats_x()
                && ui.get_stats_x() + ui.get_stats_w() <= w + 0.5
                && ui.get_die_y() + ui.get_die_h() <= h
                && ui.get_chart_h() >= 140.0
                && ui.get_row_c_y() + ui.get_btn_h() <= h;
            let line = format!(
                "окно {}×{}: схема ×{:.2} ({:.0}×{:.0}), профили {:.0} px, статистика {:.0} px, график {:.0} px высотой — разделы не налезают",
                size.0,
                size.1,
                ui.get_scale(),
                ui.get_die_w(),
                ui.get_die_h(),
                ui.get_prof_w(),
                ui.get_stats_w(),
                ui.get_chart_h()
            );
            self.check(fits, line);
            self.header_fits(name);
            let shot = self.shot(name);
            self.check(shot, format!("картинка {name}"));
            if name == "1280" {
                let tipped = self.tipped_columns();
                if let Some(&middle) = tipped.get(tipped.len() / 2) {
                    self.chart_tip(middle, 0.5, "1280×800: подсказка столбца");
                    let shot = self.shot("1280-tip");
                    self.check(shot, "картинка 1280 с подсказкой столбца");
                    self.leave();
                }
                let swap_at = (self.ui.get_swap_chip_x(), self.ui.get_swap_chip_y());
                self.chip(swap_at, |v| v.swap, "1280×800: оси местами");
                let shot = self.shot("1280-swap");
                self.check(shot, "картинка 1280, оси местами");
                let plain_at = (self.ui.get_plain_chip_x(), self.ui.get_plain_chip_y());
                self.chip(plain_at, |v| !v.swap, "1280×800: оси обратно");
            }
        }
        let back = self.compare_exactly(&[e_block.index, p_block.index]);
        self.check(back, "в сравнении снова кластер и P-ядро");
    }

    /// Шапка статистики помещается (владелец 15.10 видел при 1280 подпись линий поверх переключателя подсветки): группы
    /// кнопок вида — внутри панели, ниже легенды блоков и выше графика, друг на друга не налезают.
    fn header_fits(&mut self, name: &str) {
        let ui = &self.ui;
        let groups = [
            (
                "шкала",
                ui.get_scale_group_x(),
                ui.get_scale_group_y(),
                ui.get_scale_group_w(),
            ),
            (
                "линии",
                ui.get_curve_group_x(),
                ui.get_curve_group_y(),
                ui.get_curve_group_w(),
            ),
            (
                "оси",
                ui.get_axes_group_x(),
                ui.get_axes_group_y(),
                ui.get_axes_group_w(),
            ),
            (
                "подсветка",
                ui.get_mode_group_x(),
                ui.get_mode_group_y(),
                ui.get_mode_group_w(),
            ),
        ];
        let height = 22.0;
        let (left, right) = (ui.get_stats_x(), ui.get_stats_x() + ui.get_stats_w());
        let (top, bottom) = (ui.get_key_bottom(), ui.get_chart_y());
        let outside: Vec<&str> = groups
            .iter()
            .filter(|&&(_, x, y, w)| x < left || x + w > right || y < top - 0.5 || y + height > bottom + 0.5)
            .map(|&(n, ..)| n)
            .collect();
        let mut crowded = Vec::new();
        for (i, a) in groups.iter().enumerate() {
            for b in &groups[i + 1..] {
                let meet = a.1 < b.1 + b.3 && b.1 < a.1 + a.3 && a.2 < b.2 + height && b.2 < a.2 + height;
                if meet {
                    crowded.push(format!("{}/{}", a.0, b.0));
                }
            }
        }
        let mut rows: Vec<f32> = groups.iter().map(|g| g.2).collect();
        rows.sort_by(f32::total_cmp);
        rows.dedup_by(|a, b| (*a - *b).abs() < 1.0);
        self.check(
            outside.is_empty() && crowded.is_empty(),
            format!(
                "{name}: кнопки вида графика — {} строк(и) в панели статистики между легендой ({top:.0}) и графиком ({bottom:.0}); вне места: {outside:?}, налезли: {crowded:?}",
                rows.len()
            ),
        );
    }
}

impl Check {
    /// Первый P-блок и первый кластер E-ядер знакомства — их выбирают оба прохода с памятью окна.
    fn p_and_e(&mut self, hello: &Hello) -> Option<(BlockInfo, BlockInfo)> {
        let p = hello.blocks.iter().find(|b| b.kind == "P").cloned();
        let e = hello.blocks.iter().find(|b| b.kind == "E").cloned();
        let found = p.zip(e);
        if found.is_none() {
            self.check(false, "в знакомстве нет P-блока или кластера E-ядер");
        }
        found
    }

    /// Проход --settings: настройки автомодуля, профили щелчками, потом память окна до закрытия.
    fn settings_steps(&mut self) {
        let Some((hello, limits)) = self.arrival() else { return };
        let Some((p, e)) = self.p_and_e(&hello) else { return };
        self.statistics(&hello, &limits, &e);
        self.auto_settings();
        self.profiles(&limits, &e);
        self.memory_before_close(&p, &e);
    }

    /// То же для ползунка аварийной отсечки.
    fn emergency_x(&self, value: i32) -> (f32, f32) {
        let ui = &self.ui;
        let (x, y, w, low, high) = (
            ui.get_emergency_track_x(),
            ui.get_emergency_track_y(),
            ui.get_emergency_track_w(),
            ui.get_auto_emergency_min(),
            ui.get_auto_emergency_max(),
        );
        #[expect(clippy::cast_precision_loss, reason = "slider values are tens: exact in f32")]
        let share = (value - low) as f32 / (high - low).max(1) as f32;
        (x + share * w, y)
    }

    /// Перетаскивание ползунка аварийной — как `drag`.
    fn emergency_drag(&mut self, from: i32, to: i32, mut during: impl FnMut(&mut Check, i32)) {
        let (x0, y) = self.emergency_x(from);
        let at = LogicalPosition::new(x0, y);
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position: at });
        window.dispatch_event(WindowEvent::PointerPressed {
            position: at,
            button: PointerEventButton::Left,
        });
        self.pump(|_| true, 0.0);
        let step = if to >= from { 1 } else { -1 };
        let (mut under, mut x) = (from, x0);
        while under != to {
            under += step;
            x = self.emergency_x(under).0;
            self.ui.window().dispatch_event(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, y),
            });
            self.pump(|_| true, 0.0);
            during(self, under);
        }
        self.ui.window().dispatch_event(WindowEvent::PointerReleased {
            position: LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        });
        self.ui.window().dispatch_event(WindowEvent::PointerExited);
        self.pump(|_| true, 0.0);
    }

    /// Мышь на дорожку ползунка отсечки: число `value` — координаты окна (как Slint считает число под мышью,
    /// ValueSlider.at).
    fn slider_x(&self, value: i32) -> (f32, f32) {
        let ui = &self.ui;
        let (x, y, w, low, high) = (
            ui.get_limit_track_x(),
            ui.get_limit_track_y(),
            ui.get_limit_track_w(),
            ui.get_auto_limit_min(),
            ui.get_auto_limit_max(),
        );
        #[expect(clippy::cast_precision_loss, reason = "slider values are tens: exact in f32")]
        let share = (value - low) as f32 / (high - low).max(1) as f32;
        (x + share * w, y)
    }

    /// Перетаскивание ползунка настоящими событиями мыши: нажать на ручке `from`, вести через середины всех чисел до
    /// `to` (середина — чтобы число под мышью не зависело от округления на краю), отпустить. После каждого шага —
    /// `during(проверка, число под мышью)`.
    fn drag(&mut self, from: i32, to: i32, mut during: impl FnMut(&mut Check, i32)) {
        let (x0, y) = self.slider_x(from);
        let at = LogicalPosition::new(x0, y);
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position: at });
        self.ui.window().dispatch_event(WindowEvent::PointerPressed {
            position: at,
            button: PointerEventButton::Left,
        });
        self.pump(|_| true, 0.0);
        let step = if to >= from { 1 } else { -1 };
        let mut under = from;
        let mut x = x0;
        while under != to {
            under += step;
            x = self.slider_x(under).0;
            self.ui.window().dispatch_event(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, y),
            });
            self.pump(|_| true, 0.0);
            during(self, under);
        }
        self.ui.window().dispatch_event(WindowEvent::PointerReleased {
            position: LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        });
        self.ui.window().dispatch_event(WindowEvent::PointerExited);
        self.pump(|_| true, 0.0);
    }

    /// Настройки автомодуля (DECISIONS #36, #38): выключатель, ползунок отсечки, подтягивание — щелчками и
    /// перетаскиванием; команда уходит одна на жест, программа у часов подтверждает темой settings и состоянием, линия
    /// отсечки на графике идёт за ползунком и встаёт на подтверждённое. Глубины нет.
    #[expect(
        clippy::too_many_lines,
        reason = "one scripted round of gestures, read top to bottom"
    )]
    fn auto_settings(&mut self) {
        let came = self.pump(|v| v.auto.is_some(), 5.0);
        let Some(start) = self.view.borrow().auto.clone() else {
            self.check(false, "тема settings от CoreMapTray не пришла — настроек в окне нет");
            return;
        };
        let ui = &self.ui;
        let span = start.emergency_span();
        let shown = came
            && ui.get_has_auto()
            && ui.get_auto_ready()
            && i64::from(ui.get_auto_limit()) == start.limit
            && ui.get_auto_on() == start.on
            && i64::from(ui.get_auto_limit_min()) == start.limit_range[0]
            && i64::from(ui.get_auto_limit_max()) == start.limit_range[1]
            && ui.get_has_emergency() == start.emergency.is_some()
            && start.emergency.is_none_or(|e| {
                i64::from(ui.get_auto_emergency()) == e
                    && i64::from(ui.get_auto_emergency_min()) == span[0]
                    && i64::from(ui.get_auto_emergency_max()) == span[1]
            });
        self.check(
            shown,
            format!(
                "настройки показаны так, как их прислал CoreMapTray: {} {} °C, отсечка {}–{} °C, аварийная {:?} ({}–{} °C)",
                if start.on { "вкл" } else { "выкл" },
                start.limit,
                start.limit_range[0],
                start.limit_range[1],
                start.emergency,
                span[0],
                span[1]
            ),
        );
        if start.emergency.is_none() {
            self.check(
                false,
                "CoreMapTray не прислал аварийную отсечку (тема settings до 0.5.6)",
            );
            return;
        }
        if start.on || i64::from(LIMIT_TO) == start.limit {
            self.check(false, "проверка ждёт выключенный автомодуль с другой отсечкой");
            return;
        }
        // Отсечка — абсолютная температура: её линия — на абсолютной шкале (на шкале над кристаллом её места нет).
        let abs = (self.ui.get_abs_chip_x(), self.ui.get_abs_chip_y());
        self.chip(
            abs,
            |v| v.scale == Scale::Absolute,
            "щелчок по «абсолютная» — шкала, на которой рисуется отсечка",
        );
        // Проверка ползунков требует видимых отсечек; автодиапазоны проверяются отдельным сценарием.
        self.view.borrow_mut().range_set(
            false,
            crate::plot::Range::new(40.0, 100.0).expect("границы теста отсечек"),
        );
        let sent = self.view.borrow().sent;
        let start_limit = i32::try_from(start.limit).unwrap_or(LIMIT_TO);

        // 1. Включить — щелчок по выключателю.
        let (x, y) = (self.ui.get_auto_switch_x(), self.ui.get_auto_switch_y());
        self.click(x, y, PointerEventButton::Left);
        let on = self.pump(
            |v| {
                v.auto.as_ref().is_some_and(|a| a.on)
                    && v.state.as_ref().and_then(|s| s.auto_limit) == Some(f64::from(start_limit))
            },
            5.0,
        );
        let line_on = self.ui.get_chart_cutoff();
        let text_on = self.ui.get_chart_cutoff_text().to_string();
        self.check(
            on && self.ui.get_auto_on() && line_on >= 0.0 && text_on == format!("{start_limit}°"),
            format!(
                "щелчок по выключателю включил автомодуль: линия отсечки на графике, «{text_on}» на {line_on:.3} высоты"
            ),
        );
        // На шкале над кристаллом места абсолютной отсечке нет — линии нет; на абсолютной она снова там же.
        let above = (self.ui.get_above_chip_x(), self.ui.get_above_chip_y());
        self.chip(above, |v| v.scale == Scale::AboveDie, "щелчок по «над кристаллом»");
        let hidden = self.ui.get_chart_cutoff() < 0.0;
        self.chip(abs, |v| v.scale == Scale::Absolute, "щелчок по «абсолютная»");
        let again = self.ui.get_chart_cutoff().to_bits() == line_on.to_bits();
        self.check(
            hidden && again,
            format!("линия отсечки: над кристаллом нет ({hidden}), на абсолютной снова на месте ({again})"),
        );

        // 2. Ползунок отсечки — до LIMIT_TO: пока тянут, команды нет, а ручка и линия идут за мышью.
        let mut during: Vec<(i32, i32, String, u32)> = Vec::new();
        self.drag(start_limit, LIMIT_TO, |c, under| {
            during.push((
                under,
                c.ui.get_limit_shown(),
                c.ui.get_chart_cutoff_text().to_string(),
                c.view.borrow().sent,
            ));
        });
        let quiet = during.iter().all(|(_, _, _, s)| *s == sent + 1);
        let followed = during
            .iter()
            .all(|(under, shown, text, _)| shown == under && *text == format!("{under}°"));
        self.check(
            quiet && followed,
            format!(
                "ползунок отсечки тянут: команд нет, ручка и линия идут за мышью ({})",
                during
                    .iter()
                    .map(|(u, s, t, _)| format!("{u}:{s}/{t}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        );
        let sent_after = self.view.borrow().sent;
        let confirmed = self.pump(
            |v| {
                v.auto.as_ref().is_some_and(|a| a.limit == i64::from(LIMIT_TO))
                    && v.state.as_ref().and_then(|s| s.auto_limit) == Some(f64::from(LIMIT_TO))
            },
            5.0,
        );
        let line = self.ui.get_chart_cutoff();
        let text = self.ui.get_chart_cutoff_text().to_string();
        self.check(
            sent_after == sent + 2
                && confirmed
                && self.ui.get_auto_limit() == LIMIT_TO
                && text == format!("{LIMIT_TO}°")
                && line > line_on,
            format!(
                "отпустили на {LIMIT_TO} °C — одна команда ({}), CoreMapTray подтвердил, линия отсечки поднялась {line_on:.3} → {line:.3} («{text}»)",
                sent_after - sent - 1
            ),
        );
        let shot = self.shot("main");
        self.check(shot, "картинка: настройки автомодуля под графиком, отсечка на графике");

        // 3. Аварийная — тем же перетаскиванием до EMERGENCY_TO: команд нет, пока тянут, одна — когда отпустили; линия
        // аварийной на «абсолютной» идёт за ручкой; нижний край её ползунка — цель + 1.
        let sent_before = self.view.borrow().sent;
        let from = self.ui.get_auto_emergency();
        let low = self.ui.get_auto_emergency_min();
        let mut followed = true;
        self.emergency_drag(from, EMERGENCY_TO, |c, under| {
            followed &= c.ui.get_emergency_shown() == under
                && c.ui.get_chart_emergency_text() == format!("{under}°")
                && c.view.borrow().sent == sent_before;
        });
        let confirmed = self.pump(
            |v| {
                v.auto
                    .as_ref()
                    .is_some_and(|a| a.emergency == Some(i64::from(EMERGENCY_TO)))
            },
            5.0,
        );
        self.check(
            followed
                && confirmed
                && self.view.borrow().sent == sent_before + 1
                && self.ui.get_auto_emergency() == EMERGENCY_TO
                && low == LIMIT_TO + 1,
            format!(
                "аварийная {from} → {EMERGENCY_TO} °C перетаскиванием: пока тянут — команд нет, ручка и линия за мышью ({followed}); отпустили — одна команда, CoreMapTray подтвердил ({confirmed}); нижний край ползунка {low} (цель {LIMIT_TO} + 1)"
            ),
        );
        let shot = self.shot("emergency");
        self.check(shot, "картинка: обе отсечки на «абсолютной», ползунки цели и аварийной");

        // 4. Выключить: линии отсечки нет.
        let (x, y) = (self.ui.get_auto_switch_x(), self.ui.get_auto_switch_y());
        self.click(x, y, PointerEventButton::Left);
        let off = self.pump(
            |v| v.auto.as_ref().is_some_and(|a| !a.on) && v.state.as_ref().is_some_and(|s| s.auto_limit.is_none()),
            5.0,
        );
        let sent_total = self.view.borrow().sent - sent;
        self.check(
            off && self.ui.get_chart_cutoff() < 0.0 && sent_total == GESTURES,
            format!("щелчок выключил автомодуль — линии отсечки нет; команд за {GESTURES} жестов: {sent_total}"),
        );

        // 5. «запись» — журнал срезов, свой выключатель (владелец 16.10): выключить и включить; автомодуль не меняется.
        let came = self.pump(|v| v.log.is_some(), 1.0);
        let shown = came && self.ui.get_has_log() && self.ui.get_log_on();
        let (x, y) = (self.ui.get_log_switch_x(), self.ui.get_log_switch_y());
        let auto = self.view.borrow().auto.clone();
        self.click(x, y, PointerEventButton::Left);
        let off = self.pump(|v| v.log == Some(false), 5.0) && !self.ui.get_log_on();
        let shot = self.shot("log-off");
        self.click(x, y, PointerEventButton::Left);
        let on = self.pump(|v| v.log == Some(true), 5.0) && self.ui.get_log_on();
        let untouched = self.view.borrow().auto == auto;
        self.check(
            shown && off && on && untouched && shot,
            format!(
                "«запись» в шапке статистики: был включён ({shown}), щелчок выключил ({off}), второй включил ({on}) — по команде log.set на щелчок, CoreMapTray подтвердил; автомодуль не тронут ({untouched}); картинка F-log-off.png"
            ),
        );
    }

    /// Память окна до закрытия: щелчки по плитке и подсветке сразу пишутся в файл окна, закрытие дописывает размер.
    fn memory_before_close(&mut self, p: &BlockInfo, e: &BlockInfo) {
        let names = |v: &View| {
            v.compared
                .iter()
                .filter_map(|&b| Some(v.hello.as_ref()?.blocks.get(b)?.name.clone()))
                .collect::<Vec<_>>()
        };
        let wanted = vec![p.name.clone(), e.name.clone()];
        let first = names(&self.view.borrow());
        let (x, y) = self.tile_center(e.index);
        self.click(x, y, PointerEventButton::Left);
        let (x, y) = (self.ui.get_pin_chip_x(), self.ui.get_pin_chip_y());
        self.click(x, y, PointerEventButton::Left);
        // Вид графика тоже запоминается: оси местами и только 95 % — щелчком по «ø» (шкала — абсолютная с настроек
        // автомодуля).
        let (x, y) = (self.ui.get_swap_chip_x(), self.ui.get_swap_chip_y());
        self.click(x, y, PointerEventButton::Left);
        let (x, y) = (self.ui.get_mean_chip_x(), self.ui.get_mean_chip_y());
        self.click(x, y, PointerEventButton::Left);
        let file = self.data.join(crate::saved::FILE);
        let read = || {
            std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| crate::saved::parse(&t).ok())
                .unwrap_or_default()
        };
        let clicked = read();
        self.check(
            first == wanted[..1]
                && names(&self.view.borrow()) == wanted
                && clicked.compared == wanted
                && clicked.pin == Some(true)
                && clicked.swap == Some(true)
                && clicked.curves == Some(Curves::P95)
                && clicked.view == Some(Scale::Absolute),
            format!(
                "щелчки по плитке {}, «закреплённая», «°C» и «ø» (ø скрыты — остались 95 %) сразу записаны в файл окна: {:?}, подсветка закреплённая {:?}, оси местами {:?}, линии {:?}, вид {:?}",
                e.name, clicked.compared, clicked.pin, clicked.swap, clicked.curves, clicked.view
            ),
        );
        self.resize(CLOSED_SIZE);
        self.ui.window().dispatch_event(WindowEvent::CloseRequested);
        self.pump(|_| true, 0.0);
        let closed = read();
        #[expect(clippy::cast_precision_loss, reason = "window sizes, exact in f32")]
        let size = (CLOSED_SIZE.0 as f32, CLOSED_SIZE.1 as f32);
        self.check(
            closed.size == Some(size) && closed.compared == wanted && closed.pin == Some(true),
            format!(
                "закрытие окна записало размер {:?} (окно {}×{}) — файл окна {}",
                closed.size,
                CLOSED_SIZE.0,
                CLOSED_SIZE.1,
                file.display()
            ),
        );
    }

    /// Проход --reopened: окно открылось таким, каким его закрыли, — блоки в сравнении с их цветами, подсветка, размер;
    /// настройки автомодуля — те, что держит программа у часов (их окно не помнит, только показывает).
    fn reopened_steps(&mut self) {
        let Some((hello, limits)) = self.arrival() else { return };
        let Some((p, e)) = self.p_and_e(&hello) else { return };
        let size = self.ui.window().size();
        self.check(
            (size.width, size.height) == CLOSED_SIZE,
            format!(
                "окно открылось размером, с каким его закрыли: {}×{}",
                size.width, size.height
            ),
        );
        let compared = self.view.borrow().compared.clone();
        self.check(
            compared == [p.index, e.index],
            format!("в сравнении — те же блоки {} и {}: {compared:?}", p.name, e.name),
        );
        let pinned = self.ui.get_pin_mode() && self.view.borrow().pin_mode;
        self.check(pinned, "подсветка — закреплённая, как была");
        let (scale, curves, swap) = {
            let v = self.view.borrow();
            (v.scale, v.curves, v.swap)
        };
        self.check(
            scale == Scale::Absolute
                && curves == Curves::P95
                && swap
                && self.ui.get_chart_swap()
                && !self.ui.get_chart_mean()
                && self.ui.get_chart_p95(),
            format!("вид графика — как оставили: шкала {scale:?}, линии {curves:?}, оси местами {swap}"),
        );
        let came = self.pump(|v| v.auto.is_some(), 5.0);
        let auto = self.view.borrow().auto.clone();
        self.check(
            came && auto.as_ref().is_some_and(|a| {
                !a.on && a.limit == i64::from(LIMIT_TO) && a.emergency == Some(i64::from(EMERGENCY_TO))
            }) && self.ui.get_auto_limit() == LIMIT_TO
                && self.ui.get_auto_emergency() == EMERGENCY_TO,
            format!("настройки автомодуля — от CoreMapTray, какими их оставили: {auto:?}"),
        );
        self.statistics(&hello, &limits, &e);
        let shot = self.shot("main");
        self.check(shot, "картинка: окно открылось таким, каким его закрыли");
    }

    /// Проход --lost: проверка `CoreMapTray` рвёт свой канал без «bye», как при падении программы. Окно остаётся и
    /// одной строкой о связи говорит, что `CoreMapTray` не отвечает и автоматика не работает.
    fn lost_steps(&mut self) {
        let came = self.pump(|v| v.hello.is_some(), 15.0);
        if !self.check(came, "знакомство пришло") {
            return;
        }
        let ui = self.ui.clone_strong();
        // Строка — после второй попытки открыть канал: через 1 с, если связь жила не меньше 2 с, иначе через 5 (pipe.rs).
        let said = self.pump(|_| ui.get_link().as_str() == MISSING, 10.0);
        let line = self.ui.get_link().to_string();
        let stays = self.ui.window().is_visible() && !self.view.borrow().left;
        self.check(
            said && stays && self.ui.get_link_down(),
            format!("канал пропал без «bye» — окно осталось ({stays}), строка о связи: «{line}»"),
        );
        let shot = self.shot("main");
        self.check(shot, "картинка: окно без CoreMapTray");
    }

    /// Проход --bye: `CoreMapTray` выходит («Выход» в меню значка) и говорит «bye». Окно записывает себя в файл окна —
    /// размер этого прохода (`LEFT_SIZE`), которого там не было, — и закрывается.
    fn bye_steps(&mut self) {
        let file = self.data.join(crate::saved::FILE);
        let read = || {
            std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| crate::saved::parse(&t).ok())
                .unwrap_or_default()
        };
        #[expect(clippy::cast_precision_loss, reason = "window sizes, exact in f32")]
        let size = (LEFT_SIZE.0 as f32, LEFT_SIZE.1 as f32);
        let before = read();
        // «bye» приходит сразу после знакомства: проверка CoreMapTray шлёт «Выход», как только окно подключилось.
        let left = self.pump(|v| v.left, 10.0);
        let after = read();
        let hello = self.view.borrow().hello.is_some();
        let hidden = !self.ui.window().is_visible();
        self.check(
            hello && left && hidden,
            format!("«bye» от CoreMapTray: окно закрылось (знакомство {hello}, «bye» {left}, скрыто {hidden})"),
        );
        self.check(
            before.size != Some(size) && after.size == Some(size) && after.compared == before.compared,
            format!(
                "и записало себя в файл окна: размер {:?} → {:?}, блоки {:?}",
                before.size, after.size, after.compared
            ),
        );
    }
}

fn limit(v: &View, first: usize) -> Option<i64> {
    v.state
        .as_ref()
        .and_then(|s| s.limit.as_ref())
        .and_then(|l| l.get(first).copied())
}

#[expect(clippy::cast_precision_loss, reason = "byte counts of a few megabytes")]
fn to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

#[expect(clippy::cast_precision_loss, reason = "window sizes, exact in f32")]
fn units_u32(v: u32) -> f32 {
    v as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window size in pixels, positive and small"
)]
fn rounded(v: f32) -> u32 {
    v.max(1.0).ceil() as u32
}

// ----- эффективность, «макс», отсечки, вид «по времени» (бриф W) -----

impl Check {
    /// Шаг 1б: вид «эффективность» (бриф 16.10) — по умолчанию (файла окна нет): у блока — точки по целым множителям
    /// (производительность на градус) с линией между соседними и усами разброса, ø, 95 % и «макс» недоступны (щелчок по
    /// «ø» ничего не меняет), ось — «производительность на °C» от нуля; в подсказке столбца — эффективность с разбросом,
    /// свой нагрев и данные, в подсказке блока — что влияет на температуру ядра. Картинки F-own.png, F-own-tip.png. В
    /// конце — «над кристаллом» и снова один P-блок: дальше идут шаги сырых корзин.
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario of clicks, hovers and pictures, read top to bottom"
    )]
    fn own_view(&mut self, p_block: &BlockInfo, e_block: &BlockInfo) {
        let (p, e) = (p_block.index, e_block.index);
        let default = self.view.borrow().scale == Scale::Efficiency && self.ui.get_chart_view() == 0;
        let ready = |v: &View| {
            v.shown_stats
                .as_ref()
                .and_then(|s| s.series.iter().find(|l| l.block == p))
                .is_some_and(|l| !l.points.is_empty() && !l.sensitivity.is_empty())
        };
        let drawn = self.pump(ready, 30.0);
        let series = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .and_then(|s| s.series.iter().find(|l| l.block == p).cloned());
        let plain = series
            .as_ref()
            .is_some_and(|l| l.p95.is_empty() && l.max.is_empty() && l.model.is_empty() && !l.mean.is_empty());
        let ticks: Vec<String> = self.ui.get_chart_ticks().iter().map(|t| t.text.to_string()).collect();
        let fitted_range = self.view.borrow().shown_stats.as_ref().and_then(|s| s.value_range);
        let fits_data = fitted_range.is_some_and(|r| r.min > 0.0 && r.max > r.min);
        self.check(
            default
                && drawn
                && plain
                && fits_data
                && self.ui.get_chart_value_title().contains("производительность на °C"),
            format!(
                "по умолчанию — «эффективность»: у {} {} точек с областью чувствительности и линия между соседними множителями, линий 95 %, наибольших и кривой модели нет; автодиапазон выбранных данных: {ticks:?}",
                p_block.name,
                series.as_ref().map_or(0, |l| l.points.len())
            ),
        );
        // Область замкнута и обрезана по полю; основной расчёт сохраняется отдельной линией.
        let sensitivity_ok = series.as_ref().is_some_and(|line| {
            let values: Vec<f64> = line
                .sensitivity
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            line.sensitivity.contains('Z')
                && values.len() >= 6
                && values.iter().all(|v| v.is_finite() && (0.0..=1000.0).contains(v))
                && !line.mean.is_empty()
        });
        self.check(
            sensitivity_ok,
            "эффективность: полупрозрачная замкнутая область и отдельная основная линия",
        );
        // ø, 95 % и «макс» в этом виде недоступны: настоящий щелчок по «ø» ничего не меняет.
        let curves = self.view.borrow().curves;
        let (x, y) = (self.ui.get_mean_chip_x(), self.ui.get_mean_chip_y());
        self.click(x, y, PointerEventButton::Left);
        let (x, y) = (self.ui.get_max_chip_x(), self.ui.get_max_chip_y());
        self.click(x, y, PointerEventButton::Left);
        let still = self.view.borrow().curves == curves && !self.view.borrow().max;
        self.check(
            still,
            "щелчки по серым «ø» и «макс» в «эффективности» ничего не изменили",
        );
        // Второй блок — кластер: две линии.
        let (ex, ey) = self.tile_center(e);
        self.click(ex, ey, PointerEventButton::Left);
        let two = self.pump(|v| v.compared == [p, e], 2.0);
        let lines = self.ui.get_chart_series().row_count();
        self.check(
            two && lines == 2,
            format!("кластер {} добавлен: линий {lines}", e_block.name),
        );
        let shot = self.shot("own");
        self.check(shot, "картинка: эффективность у P-ядра и кластера");
        // Подсказка столбца: у столбца с точкой P-ядра — эффективность с разбросом, свой нагрев, данные и нагрузка.
        let (column, tip) = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .and_then(|s| {
                s.columns
                    .iter()
                    .enumerate()
                    .find(|(_, c)| c.tip.contains("на °C") && c.tip.contains(&p_block.name))
                    .map(|(i, c)| (i, c.tip.replace('\u{a0}', " ")))
            })
            .unwrap_or_default();
        let tip_ok = tip.contains("производительность на градус")
            && tip.contains("на °C")
            && tip.contains("свой нагрев")
            && tip.contains("данных")
            && tip.contains("нагрузка ø");
        self.check(
            tip_ok,
            format!("подсказка столбца эффективности: «{}»", tip.replace('\n', " / ")),
        );
        self.chart_tip(column, 0.5, "эффективность: подсказка столбца");
        let shot = self.shot("own-tip");
        self.check(shot, "картинка: подсказка столбца эффективности");
        self.leave();
        let legend: Vec<String> = self.ui.get_stats_legend().iter().map(|l| l.tip.to_string()).collect();
        let influence = legend.iter().all(|t| {
            t.contains("Что влияет на температуру ядра")
                && t.contains("кристалл")
                && t.contains("своя работа")
                && t.contains("соседи")
                && t.contains("предыдущие секунды")
                && t.contains("действуют через кристалл")
                && t.contains("эффективность: множитель / свой нагрев")
        });
        self.check(
            influence,
            format!(
                "в подсказке блока — что влияет на температуру ядра и что такое эффективность: {}",
                legend
                    .first()
                    .map(|t| t
                        .lines()
                        .filter(|l| l.starts_with("• "))
                        .collect::<Vec<_>>()
                        .join(" / "))
                    .unwrap_or_default()
            ),
        );
        // Дальше — сырые корзины: «над кристаллом» и снова один P-блок.
        let above = (self.ui.get_above_chip_x(), self.ui.get_above_chip_y());
        self.chip(above, |v| v.scale == Scale::AboveDie, "щелчок по «над кристаллом»");
        self.click(ex, ey, PointerEventButton::Left);
        let back = self.pump(|v| v.compared == [p], 2.0);
        self.check(back, "кластер убран: в сравнении снова один P-блок");
    }

    /// Шаг 2д: «макс» — третий независимый выключатель на сырых видах: линия наибольших, в подсказке столбца — условия
    /// строки наибольшей (время, множитель и потолок, нагрузка, кристалл, соседи, видеокарта, вентиляторы); отсечки
    /// автомодуля (аварийная и по цели) — отметками на линии блока и строкой в подсказке столбца. Картинки F-max.png,
    /// F-max-tip.png.
    fn max_and_cuts(&mut self, p_block: &BlockInfo, e_block: &BlockInfo) {
        let e = e_block.index;
        let one = self.compare_exactly(&[e]);
        let abs = (self.ui.get_abs_chip_x(), self.ui.get_abs_chip_y());
        self.chip(abs, |v| v.scale == Scale::Absolute, "«абсолютная» для «макс»");
        let at = (self.ui.get_max_chip_x(), self.ui.get_max_chip_y());
        self.chip(at, |v| v.max, "щелчок по «макс» — линия наибольших видна");
        let (line, others) = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .and_then(|s| s.series.first())
            .map(|l| (!l.max.is_empty(), !l.mean.is_empty() && !l.p95.is_empty()))
            .unwrap_or_default();
        let peaks: Vec<f64> = self
            .view
            .borrow()
            .digest
            .as_ref()
            .and_then(|d| d.tally.blocks.get(&e_block.name))
            .map(|t| t.bins.values().map(|b| b.peak.temp).collect())
            .unwrap_or_default();
        self.check(
            one && line && others && self.ui.get_chart_max(),
            format!(
                "«макс»: у {} линия наибольших ({} точек), ø и 95 % не тронуты",
                e_block.name,
                peaks.len()
            ),
        );
        let tips: Vec<(usize, String)> = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .map(|s| {
                s.columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.tip.contains("макс"))
                    .map(|(i, c)| (i, c.tip.replace('\u{a0}', " ")))
                    .collect()
            })
            .unwrap_or_default();
        let conditions = !tips.is_empty()
            && tips.iter().all(|(_, t)| {
                t.contains("потолок ×") && t.contains("кристалл") && t.contains("соседи") && t.contains("нагрузка")
            });
        let readings = tips
            .iter()
            .any(|(_, t)| t.contains("видеокарта") && t.contains("вент."));
        self.check(
            conditions && readings,
            format!(
                "в подсказке столбца — условия строки наибольшей: {}",
                tips.first()
                    .map(|(_, t)| t.lines().nth(2).unwrap_or_default().to_string())
                    .unwrap_or_default()
            ),
        );
        // Отсечки из журнала автомодуля (придуманные: аварийная и по цели на ×(заводской − 1)) — на линии блока.
        let cuts: Vec<i32> = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .and_then(|s| s.series.first())
            .map(|l| l.cuts.iter().map(|c| c.kind).collect())
            .unwrap_or_default();
        let listed = tips
            .iter()
            .any(|(_, t)| t.contains("аварийная отсечка") && t.contains("отсечка: 2-с медиана"));
        self.check(
            cuts.contains(&1) && cuts.contains(&2) && listed,
            format!(
                "отсечки на линии {}: виды {cuts:?}; в подсказке столбца — обе: {listed}",
                e_block.name
            ),
        );
        let shot = self.shot("max");
        self.check(shot, "картинка: «макс» и отметки отсечек, абсолютная шкала");
        if let Some(&(column, _)) = tips.first() {
            self.chart_tip(column, 0.5, "«макс»: подсказка столбца с условиями строки");
            let shot = self.shot("max-tip");
            self.check(shot, "картинка: подсказка столбца с условиями наибольшей");
            self.leave();
        }
        self.chip(at, |v| !v.max, "второй щелчок по «макс» — линии наибольших нет");
        let gone = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .is_some_and(|s| s.series.iter().all(|l| l.max.is_empty()));
        self.check(gone, "линия наибольших скрыта");
        let above = (self.ui.get_above_chip_x(), self.ui.get_above_chip_y());
        self.chip(above, |v| v.scale == Scale::AboveDie, "снова «над кристаллом»");
        let back = self.compare_exactly(&[e, p_block.index]);
        self.check(back, "в сравнении снова кластер и P-ядро");
    }

    fn append_navigation_day(&mut self, delta: i64) -> Option<(i64, i64)> {
        let mut day = self.view.borrow().day_data.as_deref()?.clone();
        *day.x.last_mut()? += delta;
        *day.t.last_mut()? += delta;
        if let Some(stretch) = day.stretches.last_mut() {
            stretch.t1 += delta;
        }
        let bounds = day.bounds();
        self.view.borrow_mut().on_day(Arc::new(day));
        bounds
    }

    fn time_navigation(&mut self) {
        let original = self.view.borrow().day_data.clone();
        let bounds = original.as_ref().and_then(|d| d.bounds()).unwrap_or_default();
        let cursor = LogicalPosition::new(
            self.ui.get_time_plot_x() + 0.3 * self.ui.get_time_plot_w(),
            self.ui.get_time_plot_y() + 0.5 * self.ui.get_time_plot_h(),
        );
        for _ in 0..3 {
            self.ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: cursor,
                delta_x: 0.0,
                delta_y: 60.0,
            });
            self.pump(|_| true, 0.0);
        }
        let width = self.view.borrow().span.map(|s| s.1 - s.0).unwrap_or_default();
        let track = LogicalPosition::new(
            self.ui.get_time_scroll_x() + 0.35 * self.ui.get_time_scroll_w(),
            self.ui.get_time_scroll_y() + 0.5 * self.ui.get_time_scroll_h(),
        );
        self.click(track.x, track.y, PointerEventButton::Left);
        let scrolled = self.view.borrow().span.unwrap_or_default();
        self.check(
            !self.view.borrow().time_follow
                && scrolled.1 - scrolled.0 == width
                && self.ui.get_time_scroll_start() > 0.0
                && self.ui.get_time_scroll_end() <= 1.0,
            "полоса обзора перемещает окно, сохраняет ширину и показывает положение в дне",
        );
        self.scale_scroll(true, 0.5);
        let inward = self.view.borrow().span.unwrap_or_default();
        self.check(
            inward.1 == scrolled.1 && inward.0 > scrolled.0 && !self.view.borrow().time_follow,
            "колесо над шкалой времени приближает историческое окно от выбранного конца",
        );
        self.scale_scroll(true, -0.5);
        let outward = self.view.borrow().span.unwrap_or_default();
        self.check(
            outward.1 == scrolled.1 && outward.0 < inward.0 && !self.view.borrow().time_follow,
            "обратное колесо отдаляет историческое окно, сохраняя конец и отрыв от новых данных",
        );
        let width = outward.1 - outward.0;
        let end = (self.ui.get_time_end_x(), self.ui.get_time_end_y());
        self.click(end.0, end.1, PointerEventButton::Left);
        let at_end = self.view.borrow().span.unwrap_or_default();
        self.check(
            at_end.1 == bounds.1 && at_end.1 - at_end.0 == width && self.view.borrow().time_follow,
            "В конец: окно у последних данных с прежней шириной",
        );
        self.ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: cursor,
            delta_x: 0.0,
            delta_y: 60.0,
        });
        self.pump(|_| true, 0.0);
        let zoomed = self.view.borrow().span.unwrap_or_default();
        self.check(
            self.view.borrow().time_follow && zoomed.1 == bounds.1 && zoomed.1 - zoomed.0 < width,
            "масштаб при слежении сохраняет последние данные у правого края",
        );
        let grown = self.append_navigation_day(1000).unwrap_or_default();
        let followed = self.view.borrow().span.unwrap_or_default();
        self.check(
            followed.1 == grown.1 && followed.1 - followed.0 == zoomed.1 - zoomed.0,
            "новая порция данных сдвигает окно слежения, сохраняя длительность",
        );
        self.click(
            self.ui.get_time_scroll_x() + 2.0,
            self.ui.get_time_scroll_y() + 0.5 * self.ui.get_time_scroll_h(),
            PointerEventButton::Left,
        );
        let frozen = self.view.borrow().span;
        self.append_navigation_day(1000);
        self.check(
            self.view.borrow().span == frozen && !self.ui.get_time_follow(),
            "при выключенном слежении новые данные не двигают ручное окно",
        );
        let shot = self.shot("time-overview");
        self.check(shot, "картинка: полоса обзора с ручным окном");
        let swap = (self.ui.get_swap_chip_x(), self.ui.get_swap_chip_y());
        self.click(swap.0, swap.1, PointerEventButton::Left);
        let shot = self.shot("time-overview-swapped");
        self.check(
            self.ui.get_chart_swap() && self.ui.get_time_scroll_h() > self.ui.get_time_scroll_w() && shot,
            "перестановка осей поворачивает обзор вместе с осью времени",
        );
        let before = self.view.borrow().span.unwrap_or_default();
        self.scale_scroll(true, 0.5);
        let after = self.view.borrow().span.unwrap_or_default();
        self.check(
            after.1 == before.1 && after.0 > before.0 && !self.view.borrow().time_follow,
            "вертикальная шкала времени после перестановки также масштабируется от конца",
        );
        self.click(swap.0, swap.1, PointerEventButton::Left);
        if let Some(day) = original {
            self.view.borrow_mut().on_day(day);
        }
        let auto = (self.ui.get_range_arg_auto_x(), self.ui.get_range_arg_auto_y());
        if !self.ui.get_range_auto_arg() {
            self.click(auto.0, auto.1, PointerEventButton::Left);
        }
    }

    /// Шаг 5: ось x — время (бриф W с поправкой 16.10). Щелчок по «по времени» — день читается потоком статистики;
    /// раскладка «блоки»: одна полоса, выбранные блоки своими цветами, события блока — на его линии, сброс — под полосой,
    /// перерыв записи 08:00–08:30 сжат в отметку (подсказка — его длительность); величина «потолок» — ступени; колесо —
    /// масштаб вокруг курсора, перетаскивание — сдвиг, «весь день» — назад; ◀ — вчера, ▶ — сегодня; «параметры» — семь
    /// полос первых двух блоков. Список событий: «все блоки» — все строки; щелчок по строке из вида «по множителю» — ось x
    /// время и момент события посередине. Цена: пересчёт полос и перерисовка окна. Картинки F-time.png, F-time-zoom.png,
    /// F-time-tip.png, F-time-gap.png, F-time-params.png, F-time-1280.png.
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario of clicks, wheel, drag and pictures, read top to bottom"
    )]
    fn time_view(&mut self, p_block: &BlockInfo, e_block: &BlockInfo, expected: Option<&Synthetic>) {
        // В сравнении кластер и P-ядро: «выбранные» — ещё две строки, если P-ядро — тот «другой» блок журнала.
        let (want_mine, want_all) = expected.map_or((0, 0), |s| {
            (
                s.events_mine + if s.events_other == p_block.name { 2 } else { 0 },
                s.events_all,
            )
        });
        let size = (1920, 1040);
        self.resize(size);
        let at = (self.ui.get_time_chip_x(), self.ui.get_time_chip_y());
        self.chip(at, |v| v.timeline, "щелчок по «по времени»");
        let began = Instant::now();
        let read = self.pump(
            |v| v.day_data.is_some() && v.shown_time.as_ref().is_some_and(|t| !t.strips.is_empty()),
            30.0,
        );
        let arrived = began.elapsed().as_secs_f64();
        // Цена показа: пересчёт полос (прореживание, картинки) и одна перерисовка окна целиком.
        let started = Instant::now();
        self.view.borrow_mut().shown_time = None;
        self.view.borrow_mut().refresh_time();
        let panel = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        self.window.request_redraw();
        self.draw();
        let drawn = started.elapsed().as_secs_f64() * 1000.0;
        self.note(format!(
            "день прочитан и показан за {arrived:.1} с; пересчёт полос {panel:.0} мс, перерисовка окна {drawn:.0} мс"
        ));
        let (strips, dots, marks, gaps, inked, head) = self
            .view
            .borrow()
            .shown_time
            .as_ref()
            .map(|t| {
                (
                    t.strips.len(),
                    t.dots.len(),
                    t.marks.len(),
                    t.gaps.len(),
                    t.strips
                        .first()
                        .map_or(0, |s| s.picture.rgba.chunks(4).filter(|p| p[3] > 0).count()),
                    t.head.clone(),
                )
            })
            .unwrap_or_default();
        let rows = self.view.borrow().day_data.as_ref().map_or(0, |d| d.t.len());
        let names: Vec<String> = self
            .view
            .borrow()
            .day_data
            .as_ref()
            .map(|d| d.blocks.iter().map(|b| b.name.clone()).collect())
            .unwrap_or_default();
        self.check(
            read && strips == 1
                && self.ui.get_chart_time()
                && self.ui.get_time_strips().row_count() == 1
                && names == [e_block.name.clone(), p_block.name.clone()]
                && dots + marks > 0
                && marks > 0
                && gaps > 0
                && inked > 0,
            format!(
                "«блоки»: день прочитан ({rows} строк), полоса {strips}, блоки {names:?}, событий на линиях {dots}, под полосой {marks}, перерывов {gaps}, закрашено пикселей {inked}; «{head}»"
            ),
        );
        let temp_at = (self.ui.get_q_temp_x(), self.ui.get_q_temp_y());
        self.click(temp_at.0, temp_at.1, PointerEventButton::Left);
        let at = (self.ui.get_q_ceiling_x(), self.ui.get_q_ceiling_y());
        self.chip(at, |v| v.quantity == Quantity::Ceiling, "щелчок по «потолок»");
        let ceilings = self.pump(
            |v| {
                v.day_data.as_ref().is_some_and(|d| d.quantities == [Quantity::Ceiling])
                    && v.shown_time
                        .as_ref()
                        .is_some_and(|t| t.strips.first().is_some_and(|s| s.title.contains("потолок")))
            },
            30.0,
        );
        self.check(
            ceilings,
            "«потолок»: день перечитан с потолками, полоса — ступени потолка",
        );
        let shot = self.shot("time");
        self.check(shot, "картинка: «блоки» × потолок, весь день");
        // Колесо: курсор на 30 % ширины поля, ступень колеса от себя — показ уже, время под курсором то же.
        let ui = &self.ui;
        let (px, py, pw, ph) = (
            ui.get_time_plot_x(),
            ui.get_time_plot_y(),
            ui.get_time_plot_w(),
            ui.get_time_plot_h(),
        );
        let span0 = self.view.borrow().span.unwrap_or_default();
        let at_time = |span: (i64, i64), share: f64| {
            #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
            let t = span.0 as f64 + share * (span.1 - span.0) as f64;
            t
        };
        let cursor = LogicalPosition::new(px + 0.3 * pw, py + ph * 0.5);
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position: cursor });
        for _ in 0..3 {
            self.ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: cursor,
                delta_x: 0.0,
                delta_y: 60.0,
            });
            self.pump(|_| true, 0.0);
        }
        let span1 = self.view.borrow().span.unwrap_or_default();
        #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
        let narrower = (span1.1 - span1.0) as f64 / (span0.1 - span0.0).max(1) as f64;
        let fixed = (at_time(span1, 1.0) - at_time(span0, 1.0)).abs();
        self.check(
            (narrower - 0.512).abs() < 0.01 && fixed <= 2.0,
            format!(
                "три ступени колеса над 30 % ширины: показ уже в {narrower:.3} раза (0,8³), правая граница сдвинулась на {fixed:.0} мс"
            ),
        );
        let shot = self.shot("time-zoom");
        self.check(shot, "картинка: после колеса");
        // Подсказка у курсора: время строки и потолок каждого блока.
        self.ui.window().dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(cursor.x + 1.0, cursor.y),
        });
        self.pump(|_| false, 0.7);
        let shot = self.shot("time-tip");
        let hover = self.view.borrow().time_hover(cursor.x + 1.0 - px, pw);
        self.check(
            self.ui.get_tip_shown() && hover.matches("потолок ×").count() == 2 && shot,
            format!("подсказка курсора: «{}»", hover.replace('\n', " / ")),
        );
        // Перетаскивание: нажать, вести на четверть ширины вправо, отпустить — показ сдвинут на четверть раньше.
        let from = LogicalPosition::new(px + 0.5 * pw, py + ph * 0.5);
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position: from });
        window.dispatch_event(WindowEvent::PointerPressed {
            position: from,
            button: PointerEventButton::Left,
        });
        self.pump(|_| true, 0.0);
        for k in 1..=5 {
            let x = from.x + 0.05 * f32::from(u8::try_from(k).unwrap_or(0)) * pw;
            self.ui.window().dispatch_event(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, from.y),
            });
            self.pump(|_| true, 0.0);
        }
        self.ui.window().dispatch_event(WindowEvent::PointerReleased {
            position: LogicalPosition::new(from.x + 0.25 * pw, from.y),
            button: PointerEventButton::Left,
        });
        self.ui.window().dispatch_event(WindowEvent::PointerExited);
        self.pump(|_| true, 0.0);
        let span2 = self.view.borrow().span.unwrap_or_default();
        #[expect(clippy::cast_precision_loss, reason = "milliseconds of a day")]
        let moved = (span1.0 - span2.0) as f64 / (span1.1 - span1.0).max(1) as f64;
        self.check(
            (moved - 0.25).abs() < 0.01 && span2.1 - span2.0 == span1.1 - span1.0,
            format!("перетаскивание на четверть ширины вправо: показ сдвинут раньше на {moved:.3} своей ширины, ширина та же"),
        );
        let whole = (self.ui.get_whole_chip_x(), self.ui.get_whole_chip_y());
        let bounds = self.view.borrow().day_data.as_ref().and_then(|d| d.bounds());
        self.click(whole.0, whole.1, PointerEventButton::Left);
        let whole_day = self.view.borrow().span == bounds;
        self.check(whole_day, "«весь день» — показ снова от первого среза до последнего");
        self.time_navigation();
        // Перерыв записи: курсор на его отметке — подсказка с началом, концом и длительностью.
        self.pump(|v| v.shown_time.is_some(), 1.0);
        let gap = self
            .view
            .borrow()
            .shown_time
            .as_ref()
            .and_then(|t| t.gaps.first().copied());
        if let Some(g) = gap {
            let x = px + g * pw;
            self.ui.window().dispatch_event(WindowEvent::PointerMoved {
                position: LogicalPosition::new(x, py + ph * 0.5),
            });
            self.pump(|_| false, 0.7);
            let shot = self.shot("time-gap");
            let hover = self.view.borrow().time_hover(x - px, pw);
            self.check(
                hover.starts_with("Перерыв записи 07:59:58–08:30:00") && hover.contains("30 мин") && shot,
                format!("подсказка на отметке перерыва: «{}»", hover.replace('\n', " / ")),
            );
            self.ui.window().dispatch_event(WindowEvent::PointerExited);
            self.pump(|_| true, 0.0);
        } else {
            self.check(false, "отметки перерыва нет");
        }
        // ◀ — вчера (в придуманных срезах вчерашний вечер есть), ▶ — снова сегодня.
        let today = self.view.borrow().day_data.as_ref().map(|d| d.date.clone());
        let (x, y) = (self.ui.get_day_prev_x(), self.ui.get_day_prev_y());
        self.click(x, y, PointerEventButton::Left);
        let yesterday = self.pump(
            |v| {
                v.day_data
                    .as_ref()
                    .is_some_and(|d| Some(&d.date) != today.as_ref() && !d.t.is_empty())
            },
            30.0,
        );
        let day_text = self.ui.get_day_text().to_string();
        let (x, y) = (self.ui.get_day_next_x(), self.ui.get_day_next_y());
        self.click(x, y, PointerEventButton::Left);
        let again = self.pump(|v| v.day_data.as_ref().map(|d| d.date.clone()) == today, 30.0);
        self.check(
            yesterday && again && !self.ui.get_can_day_next(),
            format!("◀ — вчерашний день («{day_text}»), ▶ — снова сегодня; дальше сегодня некуда"),
        );
        // «параметры»: семь полос первых двух блоков, события — под полосами.
        for index in 0_u8..10 {
            if index == 2 {
                continue;
            }
            let columns = f32::from(u8::try_from(self.ui.get_metric_columns().max(1)).unwrap_or(1));
            let x = self.ui.get_metric_grid_x() + (f32::from(index) % columns + 0.3) * self.ui.get_metric_cell_width();
            let y = self.ui.get_metric_grid_y()
                + (f32::from(index) / columns).floor() * self.ui.get_metric_cell_height()
                + 10.0;
            self.click(x, y, PointerEventButton::Left);
        }
        let overlay_ready = self.pump(
            |v| {
                v.shown_time
                    .as_ref()
                    .is_some_and(|t| t.parameter_ranges.len() == 10 && t.strips.len() == 1)
            },
            30.0,
        );
        self.check(
            overlay_ready,
            "относительный вид: все десять величин на общей панели, у каждой собственный диапазон",
        );
        let cursor = LogicalPosition::new(self.ui.get_metric_grid_x() + 30.0, self.ui.get_metric_grid_y() + 10.0);
        let before = self
            .view
            .borrow()
            .shown_time
            .as_ref()
            .map(|t| t.parameter_ranges.clone())
            .unwrap_or_default();
        self.ui
            .window()
            .dispatch_event(WindowEvent::PointerMoved { position: cursor });
        self.ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: cursor,
            delta_x: 0.0,
            delta_y: 60.0,
        });
        self.pump(|_| true, 0.0);
        let after = self
            .view
            .borrow()
            .shown_time
            .as_ref()
            .map(|t| t.parameter_ranges.clone())
            .unwrap_or_default();
        self.check(
            before.get(&Quantity::Temp) != after.get(&Quantity::Temp)
                && before.get(&Quantity::Ratio) == after.get(&Quantity::Ratio),
            "колесо над температурой меняет только её масштаб",
        );
        let shot = self.shot("time-relative");
        self.check(shot, "картинка: относительные шкалы");
        let at = (self.ui.get_lay_params_x(), self.ui.get_lay_params_y());
        self.chip(at, |v| v.layout == Layout::Params, "щелчок по «параметры»");
        let params = self.pump(
            |v| {
                v.day_data.as_ref().is_some_and(|d| d.quantities.len() == 10)
                    && v.shown_time.as_ref().is_some_and(|t| t.strips.len() == 10)
            },
            30.0,
        );
        let marks = self.view.borrow().shown_time.as_ref().map_or(0, |t| t.marks.len());
        self.check(
            params && self.ui.get_time_strips().row_count() == 10 && marks > 0,
            format!("подписанные панели выбранных величин: полос 10, событий под полосами {marks}"),
        );
        let shot = self.shot("time-params");
        self.check(shot, "картинка: «параметры», весь день");
        self.resize((1280, 800));
        self.pump(|v| v.shown_time.is_some(), 1.0);
        self.view.borrow_mut().refresh_time();
        self.pump(|_| true, 0.0);
        let shot = self.shot("time-1280");
        self.check(shot, "картинка: «параметры» при 1280×800");
        self.resize(size);
        let at = (self.ui.get_lay_blocks_x(), self.ui.get_lay_blocks_y());
        self.chip(at, |v| v.layout == Layout::Overlay, "снова совместный график");
        // Список событий: «все блоки» — все строки; снова «выбранные блоки».
        let mine = self.ui.get_events().row_count();
        let at = (self.ui.get_ev_all_x(), self.ui.get_ev_all_y());
        self.chip(at, |v| v.events_all, "щелчок по «все блоки»");
        let all = self.ui.get_events().row_count();
        let at = (self.ui.get_ev_mine_x(), self.ui.get_ev_mine_y());
        self.chip(at, |v| !v.events_all, "щелчок по «выбранные блоки»");
        self.check(
            all == want_all && mine == want_mine,
            format!("список событий: выбранные блоки {mine}, все {all} (должно {want_mine} и {want_all})"),
        );
        // Щелчок по строке из вида «по множителю»: ось x — время, момент события посередине.
        let at = (self.ui.get_blocks_chip_x(), self.ui.get_blocks_chip_y());
        self.chip(at, |v| !v.timeline, "щелчок по «по множителю»");
        let event_at = self.view.borrow().first_event_time();
        let (x, y) = (self.ui.get_events_x() + 120.0, self.ui.get_events_y() + 9.0);
        self.click(x, y, PointerEventButton::Left);
        let centred = self.pump(
            |v| {
                v.timeline
                    && v.day_data
                        .as_ref()
                        .zip(v.span)
                        .zip(event_at)
                        .and_then(|((d, s), t)| Some((d.place(t)? - i64::midpoint(s.0, s.1)).abs() <= 1000))
                        .unwrap_or(false)
            },
            30.0,
        );
        let span = self.view.borrow().span.map(|s| s.1 - s.0);
        self.check(
            centred && span == Some(600_000),
            format!("щелчок по первой строке списка: ось x — время, событие посередине, показ {span:?} мс (10 мин)"),
        );
        let at = (self.ui.get_blocks_chip_x(), self.ui.get_blocks_chip_y());
        self.chip(at, |v| !v.timeline, "снова «по множителю»");
        let file = self.data.join(crate::saved::FILE);
        let saved = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| crate::saved::parse(&t).ok())
            .unwrap_or_default();
        self.check(
            saved.timeline == Some(false)
                && saved.view.is_some()
                && saved.layout == Some(Layout::Overlay)
                && saved.metrics.as_ref().is_some_and(|m| m.len() == 10)
                && saved.events_all == Some(false),
            format!(
                "файл окна помнит вид: по времени {:?}, вид {:?}, раскладка {:?}, величина {:?}, события все {:?}",
                saved.timeline, saved.view, saved.layout, saved.quantity, saved.events_all
            ),
        );
    }
}

// ----- придуманные срезы -----

/// На сколько градусов блок горячее за каждого занятого соседа (в придуманных срезах).
const LINK_EFFECT: f64 = 4.0;

#[derive(Debug, Default)]
struct Synthetic {
    /// По блоку: множитель → (строк, мс), посчитанное генератором: каждая строка с температурой и множителем, при любой
    /// нагрузке; мс — её шаг (строки идут без пропусков, кроме перерыва перед первой новой — та тоже покрывает шаг).
    bins: HashMap<String, BTreeMap<i32, (u32, u64)>>,
    /// Блок, у которого генератор считает и шкалу над кристаллом (кластер проверки).
    focus: String,
    /// У него: множитель → (сумма «над кристаллом» × мс, мс) — средняя над кристаллом по строкам.
    above: BTreeMap<i32, (f64, u64)>,
    /// У него: множитель → (над кристаллом, мс) каждой строки — для точного процентиля.
    above_rows: BTreeMap<i32, Vec<(f64, u64)>>,
    rows: u64,
    /// Сколько мс покрывают все строки.
    ms: u64,
    today_rows: u64,
    today_bytes: u64,
    /// Строк списка событий: с выбранным кластером («выбранные блоки») и всех.
    events_mine: usize,
    events_all: usize,
    /// Блок двух строк журнала «не кластера» (прежнее снижение и событие незнакомого вида).
    events_other: String,
}

/// Отрезок строк одного файла: `count` срезов дня `date` с шагом `step_ms` от `start_ms` после полуночи; `from` — номер
/// первого среза (узор занятости непрерывен между отрезками). Шаг 2 с — время до секунды и обороты почти в каждой
/// строке (прежний журнал); шаг 0,25 с — время до миллисекунды и обороты в каждой восьмой (с 0.5.2).
#[derive(Debug, Clone, Copy)]
struct Stretch<'a> {
    date: &'a str,
    from: u64,
    count: u64,
    start_ms: u64,
    step_ms: u64,
}

/// Строки отрезка. s — куда считать ожидаемое; None — эти строки не должны войти.
#[expect(clippy::cast_precision_loss, reason = "small synthetic numbers; a test generator")]
#[expect(
    clippy::many_single_char_names,
    reason = "a synthetic generator: s, n, i, j, k, b are the row, block and counters"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic row, field by field in the file's order"
)]
fn rows(
    text: &mut String,
    stretch: Stretch,
    hello: &Hello,
    limits: &Limits,
    neighbours: &[Vec<usize>],
    s: Option<&mut Synthetic>,
) {
    let n = hello.blocks.len();
    let mut busy = vec![false; n];
    // Значения блоков текущей строки: (заводской, корзина множителя, доля работы, записанная температура).
    let mut cells: Vec<(i64, i64, &str, Option<f64>)> = Vec::with_capacity(n);
    let mut counted = s;
    let old = stretch.step_ms == samples::OLD_STEP_MS.unsigned_abs();
    for k in 0..stretch.count {
        let i = stretch.from + k;
        let ms = stretch.start_ms + k * stretch.step_ms;
        let t = ms / 1000;
        let _ = write!(
            text,
            "{} {:02}:{:02}:{:02}",
            stretch.date,
            t / 3600 % 24,
            t / 60 % 60,
            t % 60
        ); // into a String
        if !old {
            let _ = write!(text, ".{:03}", ms % 1000); // into a String
        }
        for (j, b) in busy.iter_mut().enumerate() {
            let period = 15 + 7 * j as u64;
            *b = !(i / period).is_multiple_of(3);
        }
        let package = 50 + i % 20;
        let read = if old {
            !i.is_multiple_of(10)
        } else {
            k.is_multiple_of(8)
        };
        let (fan, fan_gpu) = if read {
            ((2500 + 40 * package).to_string(), (2700 + 40 * package).to_string())
        } else {
            (String::new(), String::new())
        };
        let _ = write!(text, ",{package},{fan},{fan_gpu}"); // into a String
        // Сначала значения всех блоков (уровень кристалла строки — по ним всем), потом запись и счёт.
        cells.clear();
        for (j, b) in hello.blocks.iter().enumerate() {
            let fused = limits.fused.get(b.first).copied().unwrap_or(40);
            let step = i64::try_from((i / (11 + j as u64)) % 4).unwrap_or(0);
            let ratio = fused - step;
            // Доля работы: 0.9 / 0.1, изредка ровно на границе — 0.5 (занят) и 0.49 (свободен).
            let share = if i.is_multiple_of(97) {
                "0.5"
            } else if i.is_multiple_of(89) {
                "0.49"
            } else if busy[j] {
                "0.9"
            } else {
                "0.1"
            };
            let is_busy = share == "0.5" || (share == "0.9");
            let hot = neighbours[j].iter().filter(|&&o| busy[o]).count();
            let temp = if is_busy {
                35.0 + (ratio as f64 - 25.0) * 1.2 + LINK_EFFECT * hot as f64 + (i % 5) as f64
            } else {
                45.0
            };
            // Изредка пустая температура: неизвестно, не ноль — такой срез не считается. Записывается целым, как
            // у датчиков; дальше считается то, что записано.
            let known = !(i + j as u64).is_multiple_of(53);
            let written = known.then(|| format!("{temp:.0}").parse::<f64>().unwrap_or(temp));
            cells.push((fused, ratio, share, written));
        }
        let (sum, count) = cells
            .iter()
            .filter_map(|c| c.3)
            .fold((0.0, 0usize), |(s, k), t| (s + t, k + 1));
        for (b, &(fused, ratio, share, written)) in hello.blocks.iter().zip(cells.iter()) {
            let ratio_text = format!("{}.7", ratio - 1); // ×(ratio−0.3): корзина ratio
            let temp_text = written.map(|t| format!("{t}")).unwrap_or_default();
            let _ = write!(text, ",{fused},{ratio_text},{share},{temp_text}"); // into a String
            // В график идёт каждая строка с температурой и множителем — при любой нагрузке (владелец 15.10).
            if let Some(temp) = written
                && let Some(s) = counted.as_deref_mut()
            {
                let bin = s
                    .bins
                    .entry(b.name.clone())
                    .or_default()
                    .entry(i32::try_from(ratio).unwrap_or(0))
                    .or_default();
                bin.0 += 1;
                bin.1 += stretch.step_ms;
                // Над кристаллом: минус средняя остальных известных, если их не меньше половины остальных.
                let others = count - 1;
                if b.name == s.focus && others >= 1 && others * 2 >= n - 1 {
                    let up = temp - (sum - temp) / others as f64;
                    let key = i32::try_from(ratio).unwrap_or(0);
                    let at = s.above.entry(key).or_default();
                    at.0 += up * stretch.step_ms as f64;
                    at.1 += stretch.step_ms;
                    s.above_rows.entry(key).or_default().push((up, stretch.step_ms));
                }
            }
        }
        // Напряжение блоков (с 0.5.4) — в конце строки: растёт с множителем; изредка пусто (не прочиталось) — там модель
        // берёт кубический закон. За ним — мощность ядер: 4 Вт + по блоку его p (нагрузка × r/45 × (V/V45)², записанный
        // множитель на 0,3 ниже корзины) × 9 Вт у P-ядра и × 4 × 2,5 Вт у кластера.
        let mut power = 4.0;
        for (j, (b, &(_, ratio, share, _))) in hello.blocks.iter().zip(cells.iter()).enumerate() {
            let volts = 0.55 + 0.0135 * ratio as f64;
            if (i + j as u64).is_multiple_of(61) {
                text.push(',');
            } else {
                let _ = write!(text, ",{volts:.3}"); // into a String
            }
            let written = ratio as f64 - 0.3;
            let v45 = 0.55 + 0.0135 * 45.3;
            let p = share.parse::<f64>().unwrap_or(0.0) * written / 45.0 * (volts / v45).powi(2);
            power += if b.kind == "E" { 4.0 * 2.5 * p } else { 9.0 * p };
        }
        let _ = write!(text, ",{power:.1}"); // into a String
        // Видеокарта — в той же строке, что вентиляторы (раз в 2 с у нового журнала): «макс» берёт ближайшее чтение.
        if read {
            let _ = write!(text, ",{},{}.5,0.{}", 60 + i % 10, 100 + i % 50, 40 + i % 50); // into a String
        } else {
            text.push_str(",,,");
        }
        text.push('\n');
        if let Some(s) = counted.as_deref_mut() {
            s.rows += 1;
            s.ms += stretch.step_ms;
        }
    }
}

/// Процентиль по времени точно, по строкам (значение, мс): наименьшее значение, у которого время не выше него — не
/// меньше доли `share` всего времени (то же определение, что у гистограммы окна, без ячеек).
fn exact_quantile(rows: &[(f64, u64)], share: (u64, u64)) -> Option<f64> {
    let mut sorted = rows.to_vec(); // a few thousand rows of one bin, once per check
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: u64 = sorted.iter().map(|r| r.1).sum();
    let mut below = 0;
    for (v, ms) in sorted {
        below += ms;
        if below * share.1 >= total * share.0 {
            return Some(v);
        }
    }
    None
}

#[expect(
    clippy::too_many_lines,
    reason = "the synthetic files and the auto log, one after another"
)]
fn synthesize(
    dir: &Path,
    hello: &Hello,
    limits: &Limits,
    e_block: &BlockInfo,
    short: bool,
) -> std::io::Result<Synthetic> {
    let samples_dir = dir.join("samples");
    std::fs::create_dir_all(&samples_dir)?;
    // Соседи — по раскладке, как у окна: столбец или ряд отличаются на один.
    let tile_of = |b: usize| hello.layout.tiles.iter().find(|t| t.block == b);
    let neighbours: Vec<Vec<usize>> = (0..hello.blocks.len())
        .map(|a| {
            (0..hello.blocks.len())
                .filter(|&b| {
                    tile_of(a)
                        .zip(tile_of(b))
                        .is_some_and(|(x, y)| x.col.abs_diff(y.col) + x.row.abs_diff(y.row) == 1)
                })
                .collect()
        })
        .collect();
    let mut header = String::from("time,package,fan_cpu,fan_gpu");
    for b in &hello.blocks {
        let n = &b.name;
        let _ = write!(header, ",{n}.ceiling,{n}.ratio,{n}.busy,{n}.temp"); // into a String
    }
    for b in &hello.blocks {
        let _ = write!(header, ",{}.volts", b.name); // into a String
    }
    header.push_str(",power_cores,temp_gpu,power_gpu,busy_gpu\n");
    let mut s = Synthetic {
        focus: e_block.name.clone(),
        ..Synthetic::default()
    };
    let now = SystemTime::now();
    let day = |back| samples::date_before(now, back);
    let (old, new) = (samples::OLD_STEP_MS.unsigned_abs(), samples::STEP_MS.unsigned_abs());

    // Сегодня — день, когда поставлена новая сборка: 00:00–08:00 прежний журнал по 2 с, перерыв, 08:30–14:30 новый по
    // 0,25 с (6 часов игры), и обрезанная строка в конце (программа входа дописывает её как раз сейчас). Короткий
    // вариант (проход --settings: нужен только график) — в 20 раз меньше строк.
    let (morning_rows, game_rows) = if short { (720, 4_320) } else { (14_400, 86_400) };
    let today = day(0);
    let mut text = header.clone();
    let morning = Stretch {
        date: &today,
        from: 0,
        count: morning_rows,
        start_ms: 0,
        step_ms: old,
    };
    rows(&mut text, morning, hello, limits, &neighbours, Some(&mut s));
    let game = Stretch {
        date: &today,
        from: morning_rows,
        count: game_rows,
        start_ms: 8 * 3_600_000 + 30 * 60_000,
        step_ms: new,
    };
    rows(&mut text, game, hello, limits, &neighbours, Some(&mut s));
    s.today_rows = s.rows;
    let _ = write!(text, "{today} 23:59:59.750,61,3000,,"); // into a String
    s.today_bytes = text.len() as u64;
    std::fs::write(samples_dir.join(format!("{today}.csv")), &text)?;
    // Вчера — прежний журнал, входит.
    let yesterday = day(1);
    let mut text = header.clone();
    let evening = Stretch {
        date: &yesterday,
        from: 200_000,
        count: 600,
        start_ms: 20 * 3_600_000,
        step_ms: old,
    };
    rows(&mut text, evening, hello, limits, &neighbours, Some(&mut s));
    std::fs::write(samples_dir.join(format!("{yesterday}.csv")), &text)?;
    // 40 дней назад — за окном в 30 дней, не входит.
    let long_ago = day(40);
    let mut text = header;
    let stale = Stretch {
        date: &long_ago,
        from: 300_000,
        count: 300,
        start_ms: 0,
        step_ms: old,
    };
    rows(&mut text, stale, hello, limits, &neighbours, None);
    std::fs::write(samples_dir.join(format!("{long_ago}.csv")), &text)?;

    // Прежний журнал автомодуля (до 0.5.6): три действия с кластером, одно с другим блоком — до первого события файла
    // событий, поэтому все в списке.
    let name = &e_block.name;
    let fused = limits.fused.get(e_block.first).copied().unwrap_or(40);
    let other = hello
        .blocks
        .iter()
        .find(|b| b.name != *name)
        .map_or("0", |b| b.name.as_str());
    let date = day(0);
    let log = [
        format!(
            r#"{{"time":"{date}T10:00:00.0000000+03:00","act":"lower","block":"{name}","from":{fused},"to":{},"mean":88.4,"max":93.0,"ban":{fused},"until":"{date}T10:30:00.0000000+03:00","strikes":1}}"#,
            fused - 1
        ),
        format!(
            r#"{{"time":"{date}T10:05:00.0000000+03:00","act":"lower","block":"{other}","from":50,"to":49,"mean":90.0,"max":95.0}}"#
        ),
        format!(
            r#"{{"time":"{date}T10:40:00.0000000+03:00","act":"raise","block":"{name}","from":{},"to":{fused},"cool":120}}"#,
            fused - 1
        ),
        format!(
            r#"{{"time":"{date}T11:00:00.0000000+03:00","act":"back","why":"выход","blocks":["{name} ×{fused}"]}}"#
        ),
    ];
    std::fs::write(dir.join("auto-log.jsonl"), log.join("\n") + "\n")?;
    // Файл событий дня (0.5.6): две отсечки и возврат на ×(заводской − 1) — корзине кластера с данными; сброс без блока;
    // возврат потолков профиля; событие незнакомого вида с другим блоком.
    let events = [
        format!(
            r#"{{"time":"{date}T11:20:00.0000000+03:00","kind":"emergency","block":"{name}","from":{fused},"to":{},"period":{{"ceiling":{fused},"secs":600,"max":96,"maxTime":"{date}T11:19:58.0000000+03:00","mean":81.2}},"conditions":{{"reading":96.0,"die":71.4,"medians":[80.1],"ratio":{}.2,"busy":0.9,"gpu":{{"temp":70,"power":120.5,"busy":0.93}},"fans":[4100,4300]}}}}"#,
            fused - 1,
            fused - 1
        ),
        format!(
            r#"{{"time":"{date}T11:30:00.0000000+03:00","kind":"target","block":"{name}","from":{fused},"to":{},"conditions":{{"median":86.5,"die":70.1,"ratio":{}.3,"busy":0.88}}}}"#,
            fused - 1,
            fused - 1
        ),
        format!(
            r#"{{"time":"{date}T11:40:00.0000000+03:00","kind":"return","block":"{name}","from":{},"to":{fused},"conditions":{{"median":80.0,"die":65.0,"ratio":{}.1,"busy":0.7}}}}"#,
            fused - 1,
            fused - 1
        ),
        format!(r#"{{"time":"{date}T11:45:00.0000000+03:00","kind":"reset"}}"#),
        format!(
            r#"{{"time":"{date}T11:50:00.0000000+03:00","kind":"putback","reason":"settings","block":"{name}","from":{},"to":{fused}}}"#,
            fused - 1
        ),
        format!(r#"{{"time":"{date}T11:55:00.0000000+03:00","kind":"mystery","block":"{other}"}}"#),
    ];
    std::fs::create_dir_all(dir.join("events"))?;
    std::fs::write(
        dir.join("events").join(format!("{date}.jsonl")),
        events.join("\n") + "\n",
    )?;
    s.events_mine = 8;
    s.events_all = 10;
    s.events_other = other.to_string();
    Ok(s)
}
