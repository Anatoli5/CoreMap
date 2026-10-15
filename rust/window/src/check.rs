// Скрытая проверка окна (--snapshot F): ни окна на экране, ни кнопки на панели задач. Окно рисуется программно
// (программный отрисовщик Slint в буфер), щелчки и нажатия клавиш — настоящие события по координатам плиток, кнопок и
// строк. Против движка на имитации; запускает её проверка движка (Tray.cs C#-части). Итог — F.txt рядом с картинкой
// F, первая строка ok / FAILED; картинки F, F-select.png, F-pin.png, F-tip.png, F-profiles.png, F-menu.png, F-1280.png, F-natural.png
// смотрит агент — как выглядит окно, проверка не судит.
//
// Статистика — из своей временной папки: придуманные срезы за сегодня (полный день, 43 200 строк — цена разбора дня),
// вчера и 40 дней назад (не должны войти), обрезанная последняя строка, журнал автомодуля и журнал профилей. Что
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
    data: Option<PathBuf>,
    own_data: bool,
}

pub fn run(pipe: String, snapshot: &str, size: (u32, u32), data: Option<PathBuf>) -> i32 {
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
    let own_data = data.is_none();
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
        data,
        own_data,
    };
    let started = Instant::now();
    check.steps();
    // Своя временная папка статистики не остаётся после проверки; не удалилась — сказано в отчёте.
    if check.own_data
        && let Some(dir) = &check.data
        && let Err(e) = std::fs::remove_dir_all(dir)
    {
        check
            .lines
            .push(format!("        временная папка {} не удалена: {e}", dir.display()));
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
        self.means_and_reset(&p_block);
        self.limit_menu(&limits, &e_block);
        self.profiles(&limits, &e_block);
        self.sizes();
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

    /// Придуманные срезы и журналы во временной папке; поток статистики над ней; первая сводка.
    fn statistics(&mut self, hello: &Hello, limits: &Limits, e_block: &BlockInfo) -> Option<Synthetic> {
        let expected = if self.data.is_none() {
            let dir = std::env::temp_dir().join(format!("coremap-window-check-{}", std::process::id()));
            let made = synthesize(&dir, hello, limits, e_block);
            self.data = Some(dir);
            match made {
                Ok(s) => Some(s),
                Err(e) => {
                    self.check(false, format!("придуманные срезы не записаны: {e}"));
                    None
                }
            }
        } else {
            None
        };
        let dir = self.data.clone().unwrap_or_default();
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
                "        из них сегодняшний полный день: {} строк, {:.1} МБ",
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
        let compared = |v: &View| v.compared.iter().map(|(b, _)| *b).collect::<Vec<_>>();
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
        self.check(
            outline == Some((true, crate::panels::rgb(crate::panels::SERIES[0]))),
            "выбранная плитка обведена цветом своей линии",
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
                .map(|b| b.iter().map(|(r, n)| (*r, *n)).collect())
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
                format!("точки графика (множитель, срезов) {got:?}, у генератора {want:?}"),
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
            let history = self.view.borrow().history_shown();
            self.check(
                history == s.history,
                format!("строк истории профилей {history}, в журнале {}", s.history),
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
        self.check(
            p_outline == Some(crate::panels::rgb(crate::panels::SERIES[1])),
            "у второго блока — второй цвет",
        );
        let shot = self.shot("select");
        self.check(shot, "картинка сравнения двух блоков");

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

        // Больше четырёх блоков не сравнивается: пятый щелчок не добавляет и говорит почему.
        let others: Vec<usize> = self
            .view
            .borrow()
            .hello
            .as_ref()
            .map(|h| {
                h.blocks
                    .iter()
                    .map(|b| b.index)
                    .filter(|&b| b != p && b != e)
                    .take(3)
                    .collect()
            })
            .unwrap_or_default();
        if others.len() == 3 {
            for &b in &others {
                let (x, y) = self.tile_center(b);
                self.click(x, y, PointerEventButton::Left);
            }
            let refused = self.pump(|v| v.compared.len() == crate::panels::COMPARE, 1.0);
            let said = self.ui.get_problem().to_string();
            let count = self.view.borrow().compared.len();
            self.check(
                refused && said.contains("до 4"),
                format!("пятый блок не добавлен: блоков {count}, строка «{said}»"),
            );
            self.view.borrow_mut().problem_dismissed();
            // Убрать лишние: сравниваются снова кластер и P-ядро, их цвета прежние.
            for &b in others.iter().take(2) {
                let (x, y) = self.tile_center(b);
                self.click(x, y, PointerEventButton::Left);
            }
            let back = self.pump(|v| compared(v) == [e, p], 1.0);
            let colours = self.view.borrow().compared.clone();
            self.check(
                back && colours == [(e, 0), (p, 1)],
                format!("лишние убраны, цвета оставшихся прежние: {colours:?}"),
            );
        }
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
        // Поле графика: Chart стоит в stats-x + pad - 4, поле — 40 px от его левого края и 8 px от верха, справа 10 px,
        // снизу ось 34 px (ui/app.slint, Chart).
        let ui = &self.ui;
        let chart_x = ui.get_stats_x() + ui.get_pad() - 4.0;
        let chart_w = ui.get_stats_w() - 2.0 * ui.get_pad() + 4.0;
        let (plot_x, plot_y) = (chart_x + 40.0, ui.get_chart_y() + 8.0);
        let (plot_w, plot_h) = (chart_w - 40.0 - 10.0, ui.get_chart_h() - 8.0 - 34.0);
        Some((plot_x + point.x * plot_w, plot_y + (1.0 - point.mean) * plot_h))
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
        let diff = target - limits.was[e_block.first];
        let expected = crate::panels::signed(diff);
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

    /// 5. Профили щелчками: сохранить как, открыть заводские, откатить, открыть сохранённый; затем всё как было.
    #[expect(
        clippy::too_many_lines,
        reason = "one scripted round trip, step by step, read top to bottom"
    )]
    fn profiles(&mut self, limits: &Limits, e_block: &BlockInfo) {
        const NAME: &str = "проверка окна";
        let arrived = self.pump(|v| v.profiles.is_some(), 5.0);
        if !self.check(arrived && self.ui.get_has_profiles(), "профили пришли, панель показана")
        {
            return;
        }
        let first = e_block.first;
        let fused = limits.fused[first];
        let lowered = fused - 1;
        // Потолок кластера на ступень ниже заводского — то, что будет сохранено.
        let mut args = Map::new();
        args.insert("block".into(), e_block.index.into());
        args.insert("ratio".into(), lowered.into());
        self.view.borrow_mut().order("limit.set", args, "Потолок не изменён");
        let set = self.pump(|v| limit(v, first) == Some(lowered), 5.0);
        self.check(set, format!("потолок {} ×{lowered} для сохранения", e_block.name));

        // Сохранить как: щелчок в поле, название с клавиатуры, кнопка.
        let field = (
            self.ui.get_prof_x() + self.ui.get_pad() + 40.0,
            self.ui.get_row_c_y() + self.ui.get_btn_h() / 2.0,
        );
        self.press(field);
        self.type_text(NAME);
        let typed = self.ui.get_prof_name().to_string();
        self.check(
            typed == NAME && self.ui.get_can_save_as(),
            format!("в поле названия «{typed}», «сохранить как» доступна"),
        );
        let save_as = (
            self.ui.get_prof_x() + self.ui.get_prof_w() - self.ui.get_pad() - 64.0,
            self.ui.get_row_c_y() + self.ui.get_btn_h() / 2.0,
        );
        self.press(save_as);
        let saved = self.pump(
            |v| {
                v.profiles
                    .as_ref()
                    .is_some_and(|p| p.standing.as_deref() == Some(NAME) && p.sets.iter().any(|s| s.name == NAME))
            },
            5.0,
        );
        let cleared = self.pump(|_| false, 0.5) || self.ui.get_prof_name().is_empty();
        self.check(
            saved && cleared,
            format!("«сохранить как» записал «{NAME}», он стоит сейчас, поле очищено"),
        );
        let delta = self
            .view
            .borrow()
            .shown(e_block.index)
            .map(|t| t.delta.to_string())
            .unwrap_or_default();
        self.check(
            delta.is_empty(),
            format!("после сохранения разницы с «было» нет: «{delta}»"),
        );

        // Открыть заводские: строка списка, кнопка «открыть».
        let Some(row) = self.set_row("") else {
            self.check(false, "строки «заводские» в списке нет");
            return;
        };
        self.press(row);
        let open = self.button(self.ui.get_row_b_y(), 0);
        let enabled = self.ui.get_can_open();
        self.press(open);
        let opened = self.pump(|v| limit(v, first) == Some(fused), 5.0);
        let delta = self
            .view
            .borrow()
            .shown(e_block.index)
            .map(|t| t.delta.to_string())
            .unwrap_or_default();
        self.check(
            enabled && opened && delta == "+1",
            format!("«открыть» заводские: ×{fused}, на плитке «{delta}»"),
        );
        let shot = self.shot("profiles");
        self.check(shot, "картинка: открыты заводские, «было» — сохранённый");

        // Откатить к «было».
        let revert = self.button(self.ui.get_row_a_y(), 1);
        self.press(revert);
        let reverted = self.pump(|v| limit(v, first) == Some(lowered), 5.0);
        self.check(reverted, format!("«откатить» вернул ×{lowered}"));

        // Заводские, затем открыть сохранённый.
        self.press(row);
        self.press(open);
        self.pump(|v| limit(v, first) == Some(fused), 5.0);
        if let Some(mine) = self.set_row(NAME) {
            self.press(mine);
            self.press(open);
        }
        let back = self.pump(
            |v| {
                limit(v, first) == Some(lowered)
                    && v.profiles.as_ref().is_some_and(|p| p.standing.as_deref() == Some(NAME))
            },
            5.0,
        );
        self.check(back, format!("«открыть» «{NAME}» вернул ×{lowered}"));

        // Как было: заводские, «принять», удалить сохранённый.
        self.press(row);
        self.press(open);
        self.pump(|v| limit(v, first) == Some(fused), 5.0);
        let accept = self.button(self.ui.get_row_a_y(), 0);
        self.press(accept);
        let accepted = self.pump(
            |v| v.limits.as_ref().is_some_and(|l| l.was.get(first) == Some(&fused)),
            5.0,
        );
        self.check(accepted, "«принять» сделал заводские «было»");
        if let Some(mine) = self.set_row(NAME) {
            self.press(mine);
            let delete = self.button(self.ui.get_row_b_y(), 2);
            self.press(delete);
        }
        let deleted = self.pump(
            |v| {
                v.profiles
                    .as_ref()
                    .is_some_and(|p| !p.sets.iter().any(|s| s.name == NAME))
            },
            5.0,
        );
        self.check(deleted, format!("«удалить» убрал «{NAME}»"));
    }

    /// 6. Окно 1280×800 и по содержимому: разделы не налезают друг на друга и не выходят за окно.
    fn sizes(&mut self) {
        for (name, size) in [("1280", Some((1280, 800))), ("natural", None)] {
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
            let shot = self.shot(name);
            self.check(shot, format!("картинка {name}"));
        }
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
    /// По блоку: множитель → число занятых срезов, посчитанное генератором.
    bins: HashMap<String, BTreeMap<i32, u32>>,
    rows: u64,
    today_rows: u64,
    today_bytes: u64,
    actions: usize,
    history: usize,
}

/// Строки одного файла: `count` срезов с шагом 2 с от полуночи дня `date`; `from` — номер первого среза (узор
/// занятости непрерывен между файлами). s — куда считать ожидаемое; None — эти строки не должны войти.
#[expect(
    clippy::cast_precision_loss,
    clippy::too_many_arguments,
    reason = "small synthetic numbers; a test generator"
)]
fn rows(
    text: &mut String,
    date: &str,
    from: u64,
    count: u64,
    hello: &Hello,
    limits: &Limits,
    neighbours: &[Vec<usize>],
    s: Option<&mut Synthetic>,
) {
    let n = hello.blocks.len();
    let mut busy = vec![false; n];
    let mut counted = s;
    for k in 0..count {
        let i = from + k;
        let t = k * 2;
        let _ = write!(text, "{date} {:02}:{:02}:{:02}", t / 3600 % 24, t / 60 % 60, t % 60); // into a String
        for (j, b) in busy.iter_mut().enumerate() {
            let period = 15 + 7 * j as u64;
            *b = !(i / period).is_multiple_of(3);
        }
        let package = 50 + i % 20;
        let fan = if i.is_multiple_of(10) {
            String::new()
        } else {
            (2500 + 40 * package).to_string()
        };
        let _ = write!(text, ",{package},{fan},"); // into a String
        for (j, b) in hello.blocks.iter().enumerate() {
            let fused = limits.fused.get(b.first).copied().unwrap_or(40);
            let step = i64::try_from((i / (11 + j as u64)) % 4).unwrap_or(0);
            let ratio = fused - step;
            let ratio_text = format!("{}.7", ratio - 1); // ×(ratio−0.3): корзина ratio
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
            // Изредка пустая температура: неизвестно, не ноль — такой срез не считается.
            let temp_text = if (i + j as u64).is_multiple_of(53) {
                String::new()
            } else {
                format!("{temp:.0}")
            };
            let _ = write!(text, ",{fused},{ratio_text},{share},{temp_text}"); // into a String
            if is_busy
                && !temp_text.is_empty()
                && let Some(s) = counted.as_deref_mut()
            {
                *s.bins
                    .entry(b.name.clone())
                    .or_default()
                    .entry(i32::try_from(ratio).unwrap_or(0))
                    .or_default() += 1;
            }
        }
        text.push('\n');
        if let Some(s) = counted.as_deref_mut() {
            s.rows += 1;
        }
    }
}

fn synthesize(dir: &Path, hello: &Hello, limits: &Limits, e_block: &BlockInfo) -> std::io::Result<Synthetic> {
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
    let mut s = Synthetic::default();
    let now = SystemTime::now();
    let day = |back| samples::date_before(now, back);

    // Сегодня: полный день по 2 с и обрезанная строка в конце (программа входа дописывает её как раз сейчас).
    let mut text = header.clone();
    rows(&mut text, &day(0), 0, 43_200, hello, limits, &neighbours, Some(&mut s));
    s.today_rows = s.rows;
    let _ = write!(text, "{} 23:59:59,61,3000,,", day(0)); // into a String
    s.today_bytes = text.len() as u64;
    std::fs::write(samples_dir.join(format!("{}.csv", day(0))), &text)?;
    // Вчера — входит.
    let mut text = header.clone();
    rows(
        &mut text,
        &day(1),
        50_000,
        600,
        hello,
        limits,
        &neighbours,
        Some(&mut s),
    );
    std::fs::write(samples_dir.join(format!("{}.csv", day(1))), &text)?;
    // 40 дней назад — за окном в 30 дней, не входит.
    let mut text = header;
    rows(&mut text, &day(40), 90_000, 300, hello, limits, &neighbours, None);
    std::fs::write(samples_dir.join(format!("{}.csv", day(40))), &text)?;

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
    // Журнал смен профиля: две записи в том виде, в каком их пишет программа входа (потолки по ядрам до и после).
    let before = serde_json::Value::from(limits.fused.clone()).to_string(); // a dozen numbers, once
    let mut lowered = limits.fused.clone();
    for &i in &e_block.members {
        if let Some(v) = lowered.get_mut(i) {
            *v -= 2;
        }
    }
    let after = serde_json::Value::from(lowered).to_string();
    let history = [
        format!(
            r#"{{"time":"{date}T09:00:00.0000000+03:00","at":1,"act":"open","name":"тихий","before":{before},"after":{after}}}"#
        ),
        format!(
            r#"{{"time":"{date}T09:10:00.0000000+03:00","at":2,"act":"save","name":"тихий","before":{after},"after":{after}}}"#
        ),
    ];
    std::fs::write(dir.join("profile-log.jsonl"), history.join("\n") + "\n")?;
    s.history = 2;
    Ok(s)
}
