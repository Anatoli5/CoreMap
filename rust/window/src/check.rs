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
use crate::store::Digest;
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
/// (`rust/tray/src/check_window.rs`, `WINDOW_LIMIT`) ждут: отсечка, размер перед закрытием.
const LIMIT_TO: i32 = 88;
const CLOSED_SIZE: (u32, u32) = (1500, 900);
/// Жестов с настройками в проходе --settings: включить, отсечка, подтягивание, выключить (глубины нет, DECISIONS #38).
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
    digests: Option<mpsc::Receiver<Arc<Digest>>>,
    stats: Option<mpsc::Sender<()>>,
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
        ok
    }

    fn note(&mut self, what: impl AsRef<str>) {
        self.lines.push(format!("        {}", what.as_ref()));
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
            while let Some(digest) = self.digests.as_ref().and_then(|d| d.try_recv().ok()) {
                self.view.borrow_mut().on_digest(digest);
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
        self.select(&p_block, &e_block, expected.as_ref());
        self.chart_views(&p_block, &e_block, expected.as_ref());
        self.means_and_reset(&p_block);
        self.limit_menu(&limits, &e_block);
        self.profiles(&limits, &e_block);
        self.sizes(&p_block, &e_block);
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
        self.stats = Some(crate::store::start(dir, move |d| {
            // Приёмник живёт до конца проверки.
            let _ = send.send(d);
        }));
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
            // Повторный проход по неизменившимся файлам ничего не присылает (статистика не пересчитывается зря).
            if let Some(stats) = &self.stats {
                let _ = stats.send(()); // поток жив, пока жива проверка
            }
            let again = self.pump(|_| false, 1.0);
            let quiet = !again
                && self
                    .view
                    .borrow()
                    .digest
                    .as_ref()
                    .is_some_and(|d| Arc::ptr_eq(d, &digest));
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
            let actions = self.ui.get_actions().row_count();
            self.check(
                actions == s.actions,
                format!("действий автомодуля с блоком {actions}, в журнале {}", s.actions),
            );
        }
        // Второй блок — к первому: две линии, ось та же.
        self.click(px, py, PointerEventButton::Left);
        let two = self.pump(|v| compared(v) == [e, p], 2.0);
        let series = self.ui.get_chart_series().row_count();
        let same_axis = axis(&self.view.borrow()) == axis_e;
        let now = compared(&self.view.borrow());
        self.check(
            two && series == 2 && same_axis,
            format!(
                "щелчок по {} добавил его: блоков {now:?}, линий {series}; ось та же, что у одного кластера: {same_axis}",
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
        let (lo, hi) = self
            .view
            .borrow()
            .shown_stats
            .as_ref()
            .map(|s| {
                (
                    s.columns.first().map_or(0, |c| c.ratio),
                    s.columns.last().map_or(0, |c| c.ratio),
                )
            })
            .unwrap_or_default();
        let frame = chart::Frame {
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
        // Шкала над кристаллом — по умолчанию: 0 на оси, подписи со знаком.
        let above = self.view.borrow().scale == Scale::AboveDie && self.ui.get_chart_above();
        let first = ticks(self);
        self.check(
            above
                && first.iter().any(|t| t == "0°")
                && first.iter().any(|t| t.starts_with('+'))
                && self.ui.get_chart_value_title().contains("над кристаллом"),
            format!("по умолчанию шкала над кристаллом: деления {first:?}"),
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
            now.first().is_some_and(|t| t == "40°") && now.last().is_some_and(|t| t == "100°") && changed,
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

        // Линии: только ø, только 95 %, обе.
        let drawn = |v: &View| {
            v.shown_stats.as_ref().map_or((false, false), |s| {
                (
                    s.series.iter().any(|l| !l.mean.is_empty()),
                    s.series.iter().any(|l| !l.p95.is_empty()),
                )
            })
        };
        let at = (self.ui.get_mean_chip_x(), self.ui.get_mean_chip_y());
        self.chip(at, |v| v.curves == Curves::Mean, "щелчок по «ø» — только средние");
        let mean_only = drawn(&self.view.borrow()) == (true, false) && self.ui.get_chart_curves() == 0;
        let at = (self.ui.get_p95_chip_x(), self.ui.get_p95_chip_y());
        self.chip(at, |v| v.curves == Curves::P95, "щелчок по «95 %» — только 95 %");
        let p95_only = drawn(&self.view.borrow()) == (false, true) && self.ui.get_chart_curves() == 1;
        let at = (self.ui.get_both_chip_x(), self.ui.get_both_chip_y());
        self.chip(at, |v| v.curves == Curves::Both, "щелчок по «обе» — обе линии");
        let both = drawn(&self.view.borrow()) == (true, true) && self.ui.get_chart_curves() == 2;
        self.check(
            mean_only && p95_only && both,
            format!("нарисованы: только ø {mean_only}, только 95 % {p95_only}, обе {both}"),
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
        self.chip(plain_at, |v| !v.swap, "щелчок по «×» — множитель снова по горизонтали");
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
        let shown = came
            && ui.get_has_auto()
            && ui.get_auto_ready()
            && i64::from(ui.get_auto_limit()) == start.limit
            && ui.get_auto_on() == start.on
            && ui.get_auto_step_up() == start.step_up
            && i64::from(ui.get_auto_limit_min()) == start.limit_range[0]
            && i64::from(ui.get_auto_limit_max()) == start.limit_range[1];
        self.check(
            shown,
            format!(
                "настройки показаны так, как их прислал CoreMapTray: {} {} °C, подтягивание {}, отсечка {}–{} °C",
                if start.on { "вкл" } else { "выкл" },
                start.limit,
                start.step_up,
                start.limit_range[0],
                start.limit_range[1]
            ),
        );
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

        // 3. Подтягивание — щелчок по его выключателю.
        let (x, y) = (self.ui.get_step_switch_x(), self.ui.get_step_switch_y());
        self.click(x, y, PointerEventButton::Left);
        let up = self.pump(|v| v.auto.as_ref().is_some_and(|a| a.step_up), 5.0);
        self.check(up && self.ui.get_auto_step_up(), "щелчок включил подтягивание вверх");

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
        // Вид графика тоже запоминается: оси местами и только 95 % (шкала — абсолютная с настроек автомодуля).
        let (x, y) = (self.ui.get_swap_chip_x(), self.ui.get_swap_chip_y());
        self.click(x, y, PointerEventButton::Left);
        let (x, y) = (self.ui.get_p95_chip_x(), self.ui.get_p95_chip_y());
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
                && clicked.above == Some(false),
            format!(
                "щелчки по плитке {}, «закреплённая», «°C» и «95 %» сразу записаны в файл окна: {:?}, подсветка закреплённая {:?}, оси местами {:?}, линии {:?}, над кристаллом {:?}",
                e.name, clicked.compared, clicked.pin, clicked.swap, clicked.curves, clicked.above
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
                && self.ui.get_chart_curves() == 1,
            format!("вид графика — как оставили: шкала {scale:?}, линии {curves:?}, оси местами {swap}"),
        );
        let came = self.pump(|v| v.auto.is_some(), 5.0);
        let auto = self.view.borrow().auto.clone();
        self.check(
            came && auto
                .as_ref()
                .is_some_and(|a| !a.on && a.limit == i64::from(LIMIT_TO) && a.step_up)
                && self.ui.get_auto_limit() == LIMIT_TO,
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
    actions: usize,
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
        let fan = if read {
            (2500 + 40 * package).to_string()
        } else {
            String::new()
        };
        let _ = write!(text, ",{package},{fan},"); // into a String
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
    header.push('\n');
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

    // Журнал автомодуля: три действия с кластером, одно с другим блоком.
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
    s.actions = 3;
    Ok(s)
}
