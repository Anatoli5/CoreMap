// Что окно помнит о себе между запусками (DECISIONS #36): блоки в сравнении, подсветку, вид графика статистики (вид,
// линии, оси), размер и место. Цвет у блока свой (src/chart.rs, color_of) — его помнить не нужно.
// Это настройки только окна — ими никто, кроме окна, не пользуется, поэтому держит их оно само, в data\window.json;
// программа у часов о них не знает (её файл data\tray.json — настройки автомодуля, которыми пользуется она: «каждая
// настройка живёт у программы, которая ею пользуется»).
//
// Пишется при смене выбора блоков, подсветки и вида графика (щелчок владельца — одна запись) и при закрытии окна: размер и место —
// только тогда, событий о перемещении у Slint нет. Окно развёрнуто или свёрнуто — его обычные размер и место остаются
// прежними (из файла), развёрнутость запоминается признаком.
//
// Негодная часть файла заменяется значением по умолчанию только у себя (владелец: наследовать, умолчание — только
// когда родителя нет): блока больше нет — он выпадает, остальные остаются; место, где заголовок не виден ни на одном
// экране, — окно ставит Windows, размер прежний; размер меньше наименьшего — наименьший.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use slint::{ComponentHandle, LogicalSize, PhysicalPosition};

use crate::AppWindow;
use crate::chart::{Curves, Scale};
use crate::timeline::{Layout, Quantity};
use crate::wire::BlockInfo;

pub const FILE: &str = "window.json";
/// Размер больше этого (логические точки) — испорченное число, а не окно.
const LARGEST: f32 = 16_384.0;
/// Полоса заголовка от левого верхнего угла окна (физические точки): за неё окно берут мышью.
const TITLE_LEFT: i32 = 8;
const TITLE_W: i32 = 160;
const TITLE_H: i32 = 24;
/// Столько от полосы заголовка должно быть на рабочей области какого-то экрана, чтобы окно можно было взять.
const GRAB_W: i32 = 100;
const GRAB_H: i32 = 16;

/// Прямоугольник экрана в физических точках: правый и нижний край не входят.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    fn overlap(self, other: Rect) -> (i32, i32) {
        (
            (self.right.min(other.right) - self.left.max(other.left)).max(0),
            (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0),
        )
    }
}

/// Что лежит в файле. None у поля — его нет или оно негодно: берётся значение по умолчанию.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    /// Блоки в сравнении по порядку добавления: названия (как в знакомстве).
    pub compared: Vec<String>,
    /// Подсветка закреплённая (true) или под курсором (false).
    pub pin: Option<bool>,
    /// Вид графика: эффективность, над кристаллом или абсолютная. Файл сборок до 0.5.6 писал «above» (true — над
    /// кристаллом, false — абсолютная): он читается в тот же вид; нет ни того, ни другого — эффективность (бриф 16.10).
    pub view: Option<Scale>,
    /// Какие линии на графике.
    pub curves: Option<Curves>,
    /// Линия наибольших («макс»).
    pub max: Option<bool>,
    /// Ось x — время (true) или множитель.
    pub timeline: Option<bool>,
    /// Раскладка по времени: «блоки» или «параметры».
    pub layout: Option<Layout>,
    /// Величина раскладки «блоки».
    pub quantity: Option<Quantity>,
    /// Список событий: все блоки (true) или выбранные.
    pub events_all: Option<bool>,
    /// Кривые модели нагрева на графике.
    pub model: Option<bool>,
    /// Оси местами: температура по горизонтали (true).
    pub swap: Option<bool>,
    /// Обычный размер окна, логические точки (не зависят от масштаба экрана).
    pub size: Option<(f32, f32)>,
    /// Левый верхний угол окна с рамкой, физические точки экрана.
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
}

/// Куда ставить окно при открытии; None — как сегодня (размер по содержимому, место выбирает Windows).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Placement {
    pub size: Option<(f32, f32)>,
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
}

/// Разбор файла. Err — это не объект JSON (файл испорчен целиком); поля разбираются каждое само по себе.
pub fn parse(text: &str) -> Result<Saved, String> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        return Err("не объект JSON".into());
    };
    let pair = |key: &str| -> Option<(f64, f64)> {
        let a = map.get(key)?.as_array()?;
        match a.as_slice() {
            [x, y] => Some((x.as_f64()?, y.as_f64()?)),
            _ => None,
        }
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "checked finite and within LARGEST first, so the value fits f32"
    )]
    let size = pair("size")
        .filter(|&(w, h)| {
            w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 && w <= f64::from(LARGEST) && h <= f64::from(LARGEST)
        })
        .map(|(w, h)| (w as f32, h as f32));
    let position = map
        .get("position")
        .and_then(Value::as_array)
        .and_then(|a| match a.as_slice() {
            [x, y] => Some((i32::try_from(x.as_i64()?).ok()?, i32::try_from(y.as_i64()?).ok()?)),
            _ => None,
        });
    // Блок — название строкой; запись вида {"block": …, "color": …} (её писали сборки 0.5.3 до цветов у блоков) —
    // тоже: цвет из неё не нужен.
    let compared = map
        .get("compared")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|e| e.as_str().or_else(|| e.get("block")?.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // Линии: «mean», «p95», «both» — как писали сборки с переключателем «ø / 95 % / обе» (только ø, только 95 %, обе);
    // «none» — оба выключателя выключены (владелец 16.10: два независимых выключателя).
    let curves = match map.get("curves").and_then(Value::as_str) {
        Some("mean") => Some(Curves::Mean),
        Some("p95") => Some(Curves::P95),
        Some("both") => Some(Curves::Both),
        Some("none") => Some(Curves::Neither),
        _ => None,
    };
    Ok(Saved {
        compared,
        pin: map.get("pin").and_then(Value::as_bool),
        view: match map.get("view").and_then(Value::as_str) {
            // «own» — вид «нагрев ядра» сборок 0.5.6–0.5.7, место которого занял вид «эффективность».
            Some("eff" | "own") => Some(Scale::Efficiency),
            Some("above") => Some(Scale::AboveDie),
            Some("abs") => Some(Scale::Absolute),
            _ => match map.get("above").and_then(Value::as_bool) {
                Some(true) => Some(Scale::AboveDie),
                Some(false) => Some(Scale::Absolute),
                None => None,
            },
        },
        curves,
        max: map.get("max").and_then(Value::as_bool),
        timeline: map.get("timeline").and_then(Value::as_bool),
        layout: match map.get("layout").and_then(Value::as_str) {
            Some("blocks") => Some(Layout::Blocks),
            Some("params") => Some(Layout::Params),
            _ => None,
        },
        quantity: match map.get("quantity").and_then(Value::as_str) {
            Some("temp") => Some(Quantity::Temp),
            Some("ratio") => Some(Quantity::Ratio),
            Some("ceiling") => Some(Quantity::Ceiling),
            Some("busy") => Some(Quantity::Busy),
            Some("die") => Some(Quantity::Level),
            _ => None,
        },
        events_all: map.get("eventsAll").and_then(Value::as_bool),
        model: map.get("model").and_then(Value::as_bool),
        swap: map.get("swap").and_then(Value::as_bool),
        size,
        position,
        maximized: map.get("maximized").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Текст файла.
#[must_use]
pub fn text(saved: &Saved) -> String {
    let mut map = Map::new();
    map.insert("compared".into(), json!(saved.compared));
    if let Some(pin) = saved.pin {
        map.insert("pin".into(), pin.into());
    }
    if let Some(view) = saved.view {
        let name = match view {
            Scale::Efficiency => "eff",
            Scale::AboveDie => "above",
            Scale::Absolute => "abs",
        };
        map.insert("view".into(), name.into());
    }
    if let Some(max) = saved.max {
        map.insert("max".into(), max.into());
    }
    if let Some(timeline) = saved.timeline {
        map.insert("timeline".into(), timeline.into());
    }
    if let Some(layout) = saved.layout {
        let name = match layout {
            Layout::Blocks => "blocks",
            Layout::Params => "params",
        };
        map.insert("layout".into(), name.into());
    }
    if let Some(quantity) = saved.quantity {
        let name = match quantity {
            Quantity::Temp => "temp",
            Quantity::Ratio => "ratio",
            Quantity::Ceiling => "ceiling",
            Quantity::Busy => "busy",
            Quantity::Level => "die",
        };
        map.insert("quantity".into(), name.into());
    }
    if let Some(all) = saved.events_all {
        map.insert("eventsAll".into(), all.into());
    }
    if let Some(curves) = saved.curves {
        let name = match curves {
            Curves::Mean => "mean",
            Curves::P95 => "p95",
            Curves::Both => "both",
            Curves::Neither => "none",
        };
        map.insert("curves".into(), name.into());
    }
    if let Some(model) = saved.model {
        map.insert("model".into(), model.into());
    }
    if let Some(swap) = saved.swap {
        map.insert("swap".into(), swap.into());
    }
    if let Some((w, h)) = saved.size {
        map.insert("size".into(), json!([w.round(), h.round()]));
    }
    if let Some((x, y)) = saved.position {
        map.insert("position".into(), json!([x, y]));
    }
    map.insert("maximized".into(), saved.maximized.into());
    Value::Object(map).to_string()
}

/// Блоки из файла (номера), по порядку файла: только те, что есть в знакомстве, без повторов — хоть все.
#[must_use]
pub fn compared(saved: &[String], blocks: &[BlockInfo]) -> Vec<usize> {
    let mut kept: Vec<usize> = Vec::with_capacity(saved.len().min(blocks.len()));
    for name in saved {
        if let Some(block) = blocks.iter().find(|b| &b.name == name)
            && !kept.contains(&block.index)
        {
            kept.push(block.index);
        }
    }
    kept
}

/// Куда ставить окно. `work_area(полоса)` — рабочая область экрана, на котором больше всего этой полосы (None — она
/// ни на одном экране); `least` — наименьший размер окна (логические точки).
pub fn placement(saved: &Saved, least: (f32, f32), work_area: impl Fn(Rect) -> Option<Rect>) -> Placement {
    let size = saved.size.map(|(w, h)| (w.max(least.0), h.max(least.1)));
    let position = saved.position.filter(|&(x, y)| {
        let title = Rect {
            left: x.saturating_add(TITLE_LEFT),
            top: y,
            right: x.saturating_add(TITLE_LEFT).saturating_add(TITLE_W),
            bottom: y.saturating_add(TITLE_H),
        };
        work_area(title).is_some_and(|area| {
            let (w, h) = title.overlap(area);
            w >= GRAB_W && h >= GRAB_H
        })
    });
    Placement {
        size,
        position,
        maximized: saved.maximized,
    }
}

/// Что окно помнит и куда это писать.
#[derive(Debug, Default)]
pub struct Remembered {
    pub path: PathBuf,
    pub saved: Saved,
    /// Файл есть, но не прочитался или испорчен (строка владельцу); None — прочитался или его нет.
    pub trouble: Option<String>,
}

/// Читает data\window.json и ставит окно на запомненное место (до показа — без прыжка на глазах).
pub fn open(ui: &AppWindow, dir: &Path) -> Remembered {
    let path = dir.join(FILE);
    let (saved, trouble) = match std::fs::read_to_string(&path) {
        Ok(text) => match parse(&text) {
            Ok(saved) => (saved, None),
            Err(e) => (Saved::default(), Some(e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Saved::default(), None),
        Err(e) => (Saved::default(), Some(e.to_string())),
    };
    let place = placement(&saved, (ui.get_least_w(), ui.get_least_h()), crate::screen::work_area);
    let window = ui.window();
    if let Some((w, h)) = place.size {
        window.set_size(LogicalSize::new(w, h));
    }
    if let Some((x, y)) = place.position {
        window.set_position(PhysicalPosition::new(x, y));
    }
    if place.maximized {
        window.set_maximized(true);
    }
    Remembered {
        path,
        saved,
        trouble: trouble.map(|e| format!("Окно открылось по умолчанию: {FILE} не прочитан ({e})")),
    }
}

/// Пишет файл целиком (через временный рядом: оборванная запись не портит прежний).
pub fn write(path: &Path, saved: &Saved) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text(saved))?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    // По брифу (DECISIONS #36): что запомнено — восстанавливается; негодная часть — умолчание только у неё.
    use super::*;

    fn block(index: usize, name: &str) -> BlockInfo {
        BlockInfo {
            index,
            members: vec![index],
            first: index,
            lp: i64::try_from(index).unwrap_or(0),
            kind: "P".into(),
            name: name.into(),
        }
    }

    /// Блоки 275HX: 8 P-ядер и 4 кластера E-ядер.
    fn blocks() -> Vec<BlockInfo> {
        [
            "0", "1", "2–5", "6–9", "10", "11", "12", "13", "14–17", "18–21", "22", "23",
        ]
        .iter()
        .enumerate()
        .map(|(i, name)| block(i, name))
        .collect()
    }

    const SCREEN: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    /// Один экран 1920×1080 с панелью задач внизу: рабочая область — у полосы, которая его задевает.
    fn one_screen(title: Rect) -> Option<Rect> {
        let (w, h) = title.overlap(SCREEN);
        (w > 0 && h > 0).then_some(SCREEN)
    }

    #[test]
    fn what_was_written_comes_back() {
        let saved = Saved {
            compared: vec!["2–5".into(), "0".into()],
            pin: Some(true),
            view: Some(Scale::Absolute),
            curves: Some(Curves::P95),
            max: Some(true),
            timeline: Some(false),
            layout: Some(Layout::Params),
            quantity: Some(Quantity::Ceiling),
            events_all: Some(true),
            model: Some(false),
            swap: Some(true),
            size: Some((1500.0, 900.0)),
            position: Some((120, 80)),
            maximized: false,
        };
        let back = parse(&text(&saved)).expect("свой файл читается");
        assert_eq!(back, saved);
        assert_eq!(compared(&back.compared, &blocks()), [2, 0]);
        // Все двенадцать блоков — все двенадцать и вернутся, по порядку файла.
        let every: Vec<String> = blocks().iter().rev().map(|b| b.name.clone()).collect();
        let all = Saved {
            compared: every,
            ..Saved::default()
        };
        let every_back = parse(&text(&all)).expect("свой файл читается");
        assert_eq!(
            compared(&every_back.compared, &blocks()),
            (0..12).rev().collect::<Vec<_>>()
        );
        let place = placement(&back, (900.0, 560.0), one_screen);
        assert_eq!(
            place,
            Placement {
                size: Some((1500.0, 900.0)),
                position: Some((120, 80)),
                maximized: false
            }
        );
    }

    #[test]
    fn blocks_that_are_gone_drop_out_alone() {
        // Блока «99» нет (другой процессор или раскладка), «0» записан дважды.
        let saved: Vec<String> = ["99", "0", "0", "1", "2–5", "6–9", "10"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            compared(&saved, &blocks()),
            [0, 1, 2, 3, 4],
            "порядок — как в файле, без повторов"
        );
        // Ни одного известного — пусто (окно возьмёт умолчание: первое P-ядро).
        assert_eq!(compared(&["99".into()], &blocks()), Vec::<usize>::new());
    }

    #[test]
    fn an_old_scale_maps_to_the_same_raw_view_and_none_to_efficiency() {
        // Бриф 16.10: «an old saved value maps to the same raw view; a missing value maps to» the default view —
        // «эффективность» (бриф 16.10 «эффективность», на месте «нагрева ядра»; его ключ «own» читается в тот же вид).
        // Ловит: прежний файл, открывшийся не в том виде; новый ключ, не переживший запись.
        assert_eq!(parse(r#"{"above":true}"#).map(|s| s.view), Ok(Some(Scale::AboveDie)));
        assert_eq!(parse(r#"{"above":false}"#).map(|s| s.view), Ok(Some(Scale::Absolute)));
        assert_eq!(parse("{}").map(|s| s.view), Ok(None));
        assert_eq!(parse(r#"{"above":"да"}"#).map(|s| s.view), Ok(None));
        assert_eq!(Scale::default(), Scale::Efficiency, "нет записи — эффективность");
        // Новый ключ сильнее прежнего (файл, записанный новой сборкой поверх прежнего); «own» сборок 0.5.6–0.5.7 — тот же вид.
        assert_eq!(
            parse(r#"{"view":"own","above":true}"#).map(|s| s.view),
            Ok(Some(Scale::Efficiency))
        );
        for view in [Scale::Efficiency, Scale::AboveDie, Scale::Absolute] {
            let saved = Saved {
                view: Some(view),
                ..Saved::default()
            };
            assert_eq!(parse(&text(&saved)).map(|s| s.view), Ok(Some(view)));
        }
    }

    #[test]
    fn lines_of_the_three_way_switch_map_onto_the_two_switches() {
        // Файл сборки с «ø / 95 % / обе»: только ø, только 95 %, обе; и новое «обе выключены» — туда и обратно.
        for (word, mean, p95) in [
            ("mean", true, false),
            ("p95", false, true),
            ("both", true, true),
            ("none", false, false),
        ] {
            let saved = parse(&format!(r#"{{"curves":"{word}"}}"#)).expect("объект");
            let curves = saved.curves.expect(word);
            assert_eq!((curves.mean(), curves.p95()), (mean, p95), "{word}");
            assert_eq!(parse(&text(&saved)).expect("свой файл").curves, Some(curves));
        }
    }

    #[test]
    fn a_bad_part_falls_back_alone() {
        // Размер — не пара чисел, место — дробное, в сравнении — число и запись прежнего вида с цветом, линии — незнакомое
        // слово: годное рядом остаётся.
        let saved = parse(
            r#"{"compared":[7,{"block":"0","color":2},"2–5"],"pin":true,"curves":"all","above":false,"size":"big","position":[1.5,2]}"#,
        )
        .expect("объект");
        assert_eq!(saved.compared, ["0".to_string(), "2–5".to_string()]);
        assert_eq!(
            (saved.pin, saved.view, saved.curves),
            (Some(true), Some(Scale::Absolute), None)
        );
        assert_eq!((saved.size, saved.position), (None, None));
        for bad in [
            r#"{"size":[0,700]}"#,
            r#"{"size":[-5,700]}"#,
            r#"{"size":[99999,700]}"#,
            r#"{"size":[1200]}"#,
        ] {
            assert_eq!(parse(bad).map(|s| s.size), Ok(None), "{bad}");
        }
        assert_eq!(parse(r#"{"position":[99999999999,0]}"#).map(|s| s.position), Ok(None));
        // Не объект — файл испорчен целиком.
        assert!(parse("не JSON").is_err());
        assert!(parse("[1,2]").is_err());
        // Пустой объект — всё по умолчанию.
        assert_eq!(parse("{}"), Ok(Saved::default()));
    }

    #[test]
    fn a_position_off_every_screen_is_left_to_windows() {
        let at = |x: i32, y: i32| Saved {
            size: Some((1300.0, 800.0)),
            position: Some((x, y)),
            ..Saved::default()
        };
        // Свёрнутое окно Windows держит на −32000: такого места нет ни на одном экране.
        let place = placement(&at(-32_000, -32_000), (900.0, 560.0), one_screen);
        assert_eq!((place.position, place.size), (None, Some((1300.0, 800.0))));
        // Экран, на котором окно было, отключён: правее всех экранов.
        assert_eq!(placement(&at(2500, 100), (900.0, 560.0), one_screen).position, None);
        // Заголовок ушёл под край так, что взять его нечем: на экране меньше 100×16 точек полосы.
        assert_eq!(placement(&at(1850, 100), (900.0, 560.0), one_screen).position, None);
        assert_eq!(placement(&at(100, 1030), (900.0, 560.0), one_screen).position, None);
        // Чуть левее края, но заголовок виден — место остаётся.
        assert_eq!(
            placement(&at(-60, 0), (900.0, 560.0), one_screen).position,
            Some((-60, 0))
        );
        // Размер меньше наименьшего — наименьший; развёрнутость переносится.
        let small = Saved {
            size: Some((300.0, 200.0)),
            maximized: true,
            ..Saved::default()
        };
        let place = placement(&small, (900.0, 560.0), one_screen);
        assert_eq!((place.size, place.maximized), (Some((900.0, 560.0)), true));
    }
}
