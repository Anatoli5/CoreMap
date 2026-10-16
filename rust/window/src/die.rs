// Схема кристалла: где какая плитка и где полосы тепловой связи соседей — в долях схемы, чтобы ui/app.slint растягивал
// её под окно одним множителем. Раскладка — из знакомства движка (layouts\*.json: столбец и строка блока).
//
// Ряд, где есть кластер E-ядер, выше ряда P-ядер (владелец 15.10: «графически разделить… блоки энергоэффективных
// ядер по 4 ядра… чтобы они отличались от производительных… побольше что ли»): в плитке кластера четыре клетки
// ядер 2 × 2, им нужна высота.
use crate::wire::{Hello, Tile};

/// Размеры схемы при масштабе 1, в логических пикселях: столько окно берёт под неё, открываясь по содержимому.
pub const TILE_W: f32 = 172.0;
pub const P_ROW: f32 = 72.0;
pub const E_ROW: f32 = 104.0;
/// Промежуток между плитками: в нём лежит полоса тепловой связи соседей.
pub const GAP: f32 = 14.0;
/// Полоса связи — в промежутке между соседями, вдоль их общей стороны, на эту долю её длины (владелец 15.10: «в
/// промежутках между тайлами… а не перемычку с тайл на тайл»).
const BAND: f32 = 0.6;
/// Самая толстая полоса — промежуток без пикселя с каждой стороны: полоса не касается плиток.
pub const BAND_MAX: f32 = GAP - 2.0;

/// Место в долях схемы: x, y, ширина, высота.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Place {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Geometry {
    /// Размер схемы при масштабе 1.
    pub width: f32,
    pub height: f32,
    /// Места плиток — в порядке `hello.layout.tiles`.
    pub tiles: Vec<Place>,
    /// Соседние пары: номера плиток (в порядке раскладки), по горизонтали или по вертикали.
    pub links: Vec<Link>,
}

/// Полоса связи соседних плиток `a` и `b` (номера в раскладке): середина промежутка между ними.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    /// Соседи слева-справа (полоса стоит вертикально в промежутке между ними) или сверху-снизу (лежит).
    pub across: bool,
    /// Середина полосы и её длина вдоль общей стороны (в пикселях при масштабе 1).
    pub cx: f32,
    pub cy: f32,
    pub length: f32,
}

impl Link {
    /// Место полосы толщиной `thick` пикселей (при масштабе 1) в долях схемы.
    pub fn place(&self, thick: f32, width: f32, height: f32) -> Place {
        let (w, h) = if self.across {
            (thick, self.length)
        } else {
            (self.length, thick)
        };
        Place {
            x: (self.cx - w / 2.0) / width,
            y: (self.cy - h / 2.0) / height,
            w: w / width,
            h: h / height,
        }
    }
}

fn is_cluster(t: &Tile) -> bool {
    t.places.len() > 1
}

pub fn geometry(hello: &Hello) -> Geometry {
    let layout = &hello.layout;
    let cols = usize::try_from(layout.cols).unwrap_or(0).max(1);
    let rows = usize::try_from(layout.rows).unwrap_or(0).max(1);
    let mut row_h = vec![P_ROW; rows];
    for t in &layout.tiles {
        if is_cluster(t)
            && let Some(h) = row_h.get_mut(usize::from(t.row))
        {
            *h = E_ROW;
        }
    }
    let mut row_y = Vec::with_capacity(rows);
    let mut y = 0.0;
    for h in &row_h {
        row_y.push(y);
        y += h + GAP;
    }
    let height = y - GAP;
    let width = units(cols) * (TILE_W + GAP) - GAP;
    let pixel = |t: &Tile| {
        let row = usize::from(t.row).min(rows - 1);
        let x = f32::from(t.col) * (TILE_W + GAP);
        (x, row_y[row], TILE_W, row_h[row])
    };
    let tiles = layout
        .tiles
        .iter()
        .map(|t| {
            let (x, y, w, h) = pixel(t);
            Place {
                x: x / width,
                y: y / height,
                w: w / width,
                h: h / height,
            }
        })
        .collect();
    let mut links = Vec::new();
    for (a, ta) in layout.tiles.iter().enumerate() {
        for (b, tb) in layout.tiles.iter().enumerate().skip(a + 1) {
            let (dc, dr) = (ta.col.abs_diff(tb.col), ta.row.abs_diff(tb.row));
            if dc + dr != 1 {
                continue;
            }
            // Полоса — в промежутке за левой (или под верхней) плиткой пары: в одном ряду плитки одной высоты.
            let first = if (tb.row, tb.col) < (ta.row, ta.col) { tb } else { ta };
            let (x, y, w, h) = pixel(first);
            links.push(Link {
                a,
                b,
                across: dc == 1,
                cx: if dc == 1 { x + w + GAP / 2.0 } else { x + w / 2.0 },
                cy: if dc == 1 { y + h / 2.0 } else { y + h + GAP / 2.0 },
                length: BAND * if dc == 1 { h } else { w },
            });
        }
    }
    Geometry {
        width,
        height,
        tiles,
        links,
    }
}

/// Соседние блоки (номера в `hello.blocks`) — те же пары, что у полос связи на схеме (`geometry`, `Link`): сбоку или
/// сверху-снизу, по диагонали — не соседи. Ими же пользуется модель нагрева (model.rs) — правило одно.
pub fn neighbours(hello: &Hello) -> Vec<(usize, usize)> {
    let block_of = |tile: usize| hello.layout.tiles.get(tile).map(|t| t.block);
    geometry(hello)
        .links
        .iter()
        .filter_map(|l| Some((block_of(l.a)?, block_of(l.b)?)))
        .collect()
}

/// Число столбцов или рядов (единицы) → пиксели.
#[expect(clippy::cast_precision_loss, reason = "a die has a handful of columns, exact in f32")]
fn units(v: usize) -> f32 {
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Layout;

    fn hello(tiles: Vec<(u16, u16, usize)>) -> Hello {
        Hello {
            cpu: String::new(),
            simulated: true,
            cores: Vec::new(),
            blocks: Vec::new(),
            layout: Layout {
                note: None,
                confirmed: true,
                cols: 2,
                rows: 3,
                tiles: tiles
                    .into_iter()
                    .enumerate()
                    .map(|(i, (col, row, n))| Tile {
                        block: i,
                        col,
                        row,
                        places: (0..n).collect(),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn cluster_rows_are_taller_and_tiles_do_not_overlap() {
        // P P / E E / P P — как на 275HX, только короче.
        let g = geometry(&hello(vec![
            (0, 0, 1),
            (1, 0, 1),
            (0, 1, 4),
            (1, 1, 4),
            (0, 2, 1),
            (1, 2, 1),
        ]));
        assert!((g.height - (2.0 * P_ROW + E_ROW + 2.0 * GAP)).abs() < 1e-3);
        assert!((g.width - (2.0 * TILE_W + GAP)).abs() < 1e-3);
        assert!(g.tiles[2].h > g.tiles[0].h, "плитка кластера выше плитки P-ядра");
        for (i, a) in g.tiles.iter().enumerate() {
            assert!(a.x >= 0.0 && a.y >= 0.0 && a.x + a.w <= 1.0 + 1e-6 && a.y + a.h <= 1.0 + 1e-6);
            for b in g.tiles.iter().skip(i + 1) {
                let apart = a.x + a.w <= b.x + 1e-6
                    || b.x + b.w <= a.x + 1e-6
                    || a.y + a.h <= b.y + 1e-6
                    || b.y + b.h <= a.y + 1e-6;
                assert!(apart, "плитки {a:?} и {b:?} налезают");
            }
        }
    }

    #[test]
    fn neighbours_are_side_by_side_or_one_above_the_other() {
        let g = geometry(&hello(vec![
            (0, 0, 1),
            (1, 0, 1),
            (0, 1, 4),
            (1, 1, 4),
            (0, 2, 1),
            (1, 2, 1),
        ]));
        let pairs: Vec<(usize, usize, bool)> = g.links.iter().map(|l| (l.a, l.b, l.across)).collect();
        // 3 пары слева-справа и 4 сверху-снизу; по диагонали — не соседи.
        assert_eq!(pairs.len(), 7);
        assert!(pairs.contains(&(0, 1, true)) && pairs.contains(&(0, 2, false)) && pairs.contains(&(3, 5, false)));
        assert!(!pairs.iter().any(|&(a, b, _)| (a, b) == (0, 3) || (a, b) == (1, 2)));
        // Полоса — в промежутке между своими плитками и не заходит на них даже самой толстой.
        let eps = 1e-6;
        let l = g.links.iter().find(|l| (l.a, l.b) == (0, 2)).expect("0–2");
        let p = l.place(BAND_MAX, g.width, g.height);
        let (top, below) = (g.tiles[0], g.tiles[2]);
        assert!(
            p.y >= top.y + top.h - eps && p.y + p.h <= below.y + eps,
            "полоса 0–2 в промежутке"
        );
        assert!(
            p.x > top.x && p.x + p.w < top.x + top.w,
            "полоса 0–2 короче общей стороны"
        );
        let l = g.links.iter().find(|l| (l.a, l.b) == (0, 1)).expect("0–1");
        let p = l.place(BAND_MAX, g.width, g.height);
        let (left, right) = (g.tiles[0], g.tiles[1]);
        assert!(
            p.x >= left.x + left.w - eps && p.x + p.w <= right.x + eps,
            "полоса 0–1 в промежутке"
        );
        assert!(p.h < left.h, "полоса 0–1 короче общей стороны");
    }
}
