// Что окно помнит о себе между запусками (DECISIONS #36): блоки в сравнении с их цветами, подсветку, размер и место.
// Это настройки только окна — ими никто, кроме окна, не пользуется, поэтому держит их оно само, в data\window.json;
// программа у часов о них не знает (её файл data\tray.json — настройки автомодуля, которыми пользуется она: «каждая
// настройка живёт у программы, которая ею пользуется»).
//
// Пишется при смене выбора блоков и подсветки (щелчок владельца — одна запись) и при закрытии окна: размер и место —
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
use crate::panels::COMPARE;
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
    /// Блоки в сравнении по порядку добавления: название блока (как в знакомстве) и номер цвета.
    pub compared: Vec<(String, usize)>,
    /// Подсветка закреплённая (true) или под курсором (false).
    pub pin: Option<bool>,
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
    let compared = map
        .get("compared")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|e| {
                    let name = e.get("block")?.as_str()?.to_string();
                    let color = usize::try_from(e.get("color")?.as_u64()?).ok()?;
                    Some((name, color))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Saved {
        compared,
        pin: map.get("pin").and_then(Value::as_bool),
        size,
        position,
        maximized: map.get("maximized").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Текст файла.
#[must_use]
pub fn text(saved: &Saved) -> String {
    let compared: Vec<Value> = saved
        .compared
        .iter()
        .map(|(name, color)| json!({ "block": name, "color": color }))
        .collect();
    let mut map = Map::new();
    map.insert("compared".into(), Value::Array(compared));
    if let Some(pin) = saved.pin {
        map.insert("pin".into(), pin.into());
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

/// Блоки из файла: только те, что есть в знакомстве, без повторов, не больше `COMPARE`; цвет вне палитры или уже
/// занятый — первый свободный (цвета остальных не трогаются).
#[must_use]
pub fn compared(saved: &[(String, usize)], blocks: &[BlockInfo]) -> Vec<(usize, usize)> {
    let mut kept: Vec<(usize, Option<usize>)> = Vec::new();
    for (name, color) in saved {
        let Some(block) = blocks.iter().find(|b| &b.name == name) else {
            continue;
        };
        if kept.len() >= COMPARE || kept.iter().any(|(b, _)| *b == block.index) {
            continue;
        }
        let free = *color < COMPARE && kept.iter().all(|(_, c)| *c != Some(*color));
        kept.push((block.index, free.then_some(*color)));
    }
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(kept.len());
    for (i, &(block, color)) in kept.iter().enumerate() {
        let slot = color.unwrap_or_else(|| {
            (0..COMPARE)
                .find(|s| kept.iter().all(|(_, c)| *c != Some(*s)) && out.iter().all(|(_, used)| used != s))
                .unwrap_or(i % COMPARE)
        });
        out.push((block, slot));
    }
    out
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

    fn blocks() -> Vec<BlockInfo> {
        vec![
            block(0, "0"),
            block(1, "1"),
            block(2, "2–5"),
            block(3, "6–9"),
            block(4, "10"),
        ]
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
            compared: vec![("2–5".into(), 0), ("0".into(), 1)],
            pin: Some(true),
            size: Some((1500.0, 900.0)),
            position: Some((120, 80)),
            maximized: false,
        };
        let back = parse(&text(&saved)).expect("свой файл читается");
        assert_eq!(back, saved);
        assert_eq!(compared(&back.compared, &blocks()), [(2, 0), (0, 1)]);
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
        // Блока «12» нет (другой процессор или раскладка), «0» записан дважды, пятый лишний, цвет 9 вне палитры,
        // цвет 1 занят дважды.
        let saved = vec![
            ("12".to_string(), 0),
            ("0".to_string(), 1),
            ("0".to_string(), 2),
            ("1".to_string(), 1),
            ("2–5".to_string(), 9),
            ("6–9".to_string(), 3),
            ("10".to_string(), 0),
        ];
        let got = compared(&saved, &blocks());
        // Порядок — как в файле; не больше четырёх; цвета без повторов, у годных — свои.
        assert_eq!(got.iter().map(|(b, _)| *b).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert_eq!(got[0].1, 1);
        assert_eq!(got[3].1, 3);
        let mut colors: Vec<usize> = got.iter().map(|(_, c)| *c).collect();
        colors.sort_unstable();
        colors.dedup();
        assert_eq!(colors.len(), 4, "colours stay distinct: {got:?}");
        assert!(got.iter().all(|(_, c)| *c < COMPARE));
        // Ни одного известного — пусто (окно возьмёт умолчание: первое P-ядро).
        assert_eq!(compared(&[("99".into(), 0)], &blocks()), []);
    }

    #[test]
    fn a_bad_part_falls_back_alone() {
        // Размер — не пара чисел, место — дробное, сравнение — со строкой без цвета: годное рядом остаётся.
        let saved =
            parse(r#"{"compared":[{"block":"1"},{"block":"0","color":2}],"pin":true,"size":"big","position":[1.5,2]}"#)
                .expect("объект");
        assert_eq!(saved.compared, [("0".to_string(), 2)]);
        assert_eq!(saved.pin, Some(true));
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
