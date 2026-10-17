// Сохранённый результат экспериментальной модели. Это просмотр уже рассчитанных кривых, а не переобучение
// по новым срезам и не команда процессору. Первая линия и альтернативная подгонка используют одно окружение.
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "conditional-snapshot.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub version: u32,
    pub source: String,
    pub conditions: String,
    pub blocks: Vec<BlockCurve>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BlockCurve {
    pub name: String,
    pub points: Vec<CurvePoint>,
    pub temperature_test_mae: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct CurvePoint {
    pub ratio: f64,
    pub heat: f64,
    pub alternate_heat: f64,
}

impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.blocks.is_empty()
            || self.source.trim().is_empty()
            || self.conditions.trim().is_empty()
        {
            return Err("неподдерживаемая версия или пустой сохранённый расчёт".into());
        }
        let mut names = std::collections::BTreeSet::new();
        for block in &self.blocks {
            if block.name.trim().is_empty()
                || !names.insert(&block.name)
                || block.points.is_empty()
                || !block.temperature_test_mae.is_finite()
                || block.temperature_test_mae < 0.0
            {
                return Err("пустое или повторяющееся ядро, либо неверная ошибка модели".into());
            }
            let mut previous = f64::NEG_INFINITY;
            for point in &block.points {
                if [point.ratio, point.heat, point.alternate_heat]
                    .iter()
                    .any(|v| !v.is_finite() || *v <= 0.0)
                    || point.ratio <= previous
                    || !(point.ratio / point.heat).is_finite()
                    || !(point.ratio / point.alternate_heat).is_finite()
                {
                    return Err(format!(
                        "ядро {}: нужны положительные конечные значения и возрастающие множители",
                        block.name
                    ));
                }
                previous = point.ratio;
            }
        }
        Ok(())
    }
}

pub fn read(path: &Path) -> Result<Option<Snapshot>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("сохранённая модель не прочитана: {error}")),
    };
    let snapshot: Snapshot =
        serde_json::from_str(&text).map_err(|error| format!("сохранённая модель повреждена: {error}"))?;
    snapshot.validate()?;
    Ok(Some(snapshot))
}
