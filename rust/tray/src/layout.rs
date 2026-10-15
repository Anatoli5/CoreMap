// Схема кристалла для окна: где на ней плитка каждого блока (DieLayout.Pick в src/Topology.cs C#-части). Схемы лежат
// в layouts\*.json рядом с программой; подходит схема, у которой совпали раскладка ядер (строка P и E по порядку
// процессоров Windows) и опознавательная строка процессора, и чьи плитки — ровно блоки процессора. Не подошла ни одна —
// плитки в порядке нумерации Windows, до шести в ряд.
//
// Окно получает схему в знакомстве (hello.layout), как от движка на C#: тот же вид, те же правила.

use std::path::Path;

use machine::topology::{Block, Core, Kind};
use serde_json::{Value, json};

/// Плитка: блок, место на схеме, ядра в порядке мест внутри плитки (позиции в списке ядер).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub block: usize,
    pub col: i64,
    pub row: i64,
    pub places: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub name: String,
    pub note: String,
    pub confirmed: bool,
    pub cols: i64,
    pub rows: i64,
    pub tiles: Vec<Tile>,
}

impl Layout {
    /// Схема в том виде, в каком её шлёт движок на C# (Feed.Hello, поле layout).
    #[must_use]
    pub fn json(&self) -> Value {
        json!({
            "name": self.name,
            "note": self.note,
            "confirmed": self.confirmed,
            "cols": self.cols,
            "rows": self.rows,
            "tiles": self.tiles.iter().map(|t| json!({
                "block": t.block, "col": t.col, "row": t.row, "places": t.places,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Схема для этого процессора из папки `folder` (файлы по алфавиту, первый подошедший). Ошибки чтения файла — в
/// `skipped` (для журнала), схема тогда берётся следующая или по порядку нумерации.
#[must_use]
pub fn pick(cores: &[Core], blocks: &[Block], identifier: &str, folder: &Path, skipped: &mut Vec<String>) -> Layout {
    let mut files: Vec<_> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let doc = match std::fs::read_to_string(&path).map(|text| serde_json::from_str::<Value>(&text)) {
            Ok(Ok(doc)) => doc,
            Ok(Err(e)) => {
                skipped.push(format!("схема {name} не прочитана: {e}"));
                continue;
            }
            Err(e) => {
                skipped.push(format!("схема {name} не прочитана: {e}"));
                continue;
            }
        };
        match from_doc(&doc, cores, blocks, identifier) {
            Fit::Yes(layout) => return layout,
            Fit::Other => {}
            Fit::Mismatch => skipped.push(format!(
                "схема {name} не подходит: её плитки не совпадают с блоками процессора"
            )),
        }
    }
    linear(blocks)
}

enum Fit {
    Yes(Layout),
    /// Схема другого процессора.
    Other,
    /// Процессор тот, а плитки не те блоки (схема показывала бы одно, а потолок ставился бы другому).
    Mismatch,
}

fn from_doc(doc: &Value, cores: &[Core], blocks: &[Block], identifier: &str) -> Fit {
    let pattern: String = cores
        .iter()
        .map(|c| if c.kind == Kind::P { 'P' } else { 'E' })
        .collect();
    let matched = &doc["match"];
    let ours = matched["pattern"].as_str() == Some(pattern.as_str())
        && matched["identifier"].as_str().is_some_and(|id| identifier.contains(id));
    if !ours {
        return Fit::Other;
    }
    let mut tiles = Vec::new();
    for unit in doc["units"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let places: Option<Vec<usize>> = unit["lps"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|lp| {
                let lp = lp.as_u64()?;
                cores.iter().position(|c| u64::from(c.lp.0) == lp)
            })
            .collect();
        let Some(places) = places.filter(|p| !p.is_empty()) else {
            return Fit::Mismatch;
        };
        let Some(block) = blocks.iter().position(|b| b.members.contains(&places[0])) else {
            return Fit::Mismatch;
        };
        tiles.push(Tile {
            block,
            col: unit["col"].as_i64().unwrap_or(0),
            row: unit["row"].as_i64().unwrap_or(0),
            places,
        });
    }
    // Плитки — ровно блоки: каждому блоку одна плитка с теми же ядрами (DieLayout.Matches).
    let exact = tiles.len() == blocks.len()
        && tiles.iter().all(|t| {
            let mut sorted = t.places.clone(); // a few core indexes per tile, once per start
            sorted.sort_unstable();
            sorted == blocks[t.block].members
        })
        && (0..blocks.len()).all(|b| tiles.iter().filter(|t| t.block == b).count() == 1);
    if !exact {
        return Fit::Mismatch;
    }
    Fit::Yes(Layout {
        name: doc["name"].as_str().unwrap_or_default().to_string(),
        note: doc["note"].as_str().unwrap_or_default().to_string(),
        confirmed: doc["confirmed"].as_bool().unwrap_or(false),
        cols: doc["cols"].as_i64().unwrap_or(1),
        rows: doc["rows"].as_i64().unwrap_or(1),
        tiles,
    })
}

/// Схемы нет: блоки в порядке нумерации Windows, до шести в ряд (DieLayout.Linear).
fn linear(blocks: &[Block]) -> Layout {
    let count = i64::try_from(blocks.len()).unwrap_or(1).max(1);
    let cols = count.min(6);
    Layout {
        name: "порядок нумерации Windows".into(),
        note: "Для этого процессора схемы кристалла нет: ядра показаны в порядке нумерации Windows.".into(),
        confirmed: false,
        cols,
        rows: (count + cols - 1) / cols,
        tiles: blocks
            .iter()
            .enumerate()
            .map(|(b, block)| {
                let index = i64::try_from(b).unwrap_or(0);
                Tile {
                    block: b,
                    col: index % cols,
                    row: index / cols,
                    places: block.members.clone(),
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use machine::Lp;

    /// 275HX: P P E×8 P×4 E×8 P P, кластеры E по четыре (как Windows отдаёт его на машине владельца).
    fn hx() -> (Vec<Core>, Vec<Block>) {
        let kinds = "PPEEEEEEEEPPPPEEEEEEEEPP";
        let cores: Vec<Core> = kinds
            .chars()
            .enumerate()
            .map(|(i, k)| Core {
                lp: Lp(u32::try_from(i).unwrap_or(0)),
                physical: i,
                kind: if k == 'P' { Kind::P } else { Kind::E },
                cluster: None,
            })
            .collect();
        let groups: [&[usize]; 12] = [
            &[0],
            &[1],
            &[2, 3, 4, 5],
            &[6, 7, 8, 9],
            &[10],
            &[11],
            &[12],
            &[13],
            &[14, 15, 16, 17],
            &[18, 19, 20, 21],
            &[22],
            &[23],
        ];
        let blocks = groups
            .iter()
            .map(|members| Block {
                members: members.to_vec(),
                first: members[0],
                lp: cores[members[0]].lp,
                kind: cores[members[0]].kind,
                name: String::new(),
            })
            .collect();
        (cores, blocks)
    }

    fn the_layout_file() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../layouts/arrowlake_8p16e.json");
        let text = std::fs::read_to_string(path).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    #[test]
    fn the_arrow_lake_file_fits_the_275hx() {
        let (cores, blocks) = hx();
        let Fit::Yes(layout) = from_doc(
            &the_layout_file(),
            &cores,
            &blocks,
            "Intel64 Family 6 Model 198 Stepping 2",
        ) else {
            panic!("layouts/arrowlake_8p16e.json must fit the 275HX");
        };
        assert_eq!((layout.cols, layout.rows, layout.tiles.len()), (2, 6, 12));
        // The cluster 2-5 is drawn in the file's order of places: 5, 4, 3, 2.
        let cluster = layout.tiles.iter().find(|t| t.block == 2).map(|t| t.places.clone());
        assert_eq!(cluster, Some(vec![5, 4, 3, 2]));
    }

    #[test]
    fn another_cpu_gets_windows_order() {
        let (cores, blocks) = hx();
        assert!(matches!(
            from_doc(&the_layout_file(), &cores, &blocks, "Intel64 Family 6 Model 183"),
            Fit::Other
        ));
        let layout = linear(&blocks);
        assert_eq!((layout.cols, layout.rows), (6, 2));
        assert_eq!(layout.tiles[7].col, 1);
        assert_eq!(layout.tiles[7].row, 1);
    }

    #[test]
    fn tiles_that_are_not_the_blocks_do_not_fit() {
        let (cores, mut blocks) = hx();
        // A CPU whose first cluster is split in two: the file's tile 2-5 no longer is one block.
        blocks[2].members = vec![2, 3];
        blocks.insert(3, blocks[2].clone());
        blocks[3].members = vec![4, 5];
        blocks[3].first = 4;
        assert!(matches!(
            from_doc(&the_layout_file(), &cores, &blocks, "Intel64 Family 6 Model 198"),
            Fit::Mismatch
        ));
    }
}
