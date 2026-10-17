// Скрытый запуск нового расчёта на сохранённых данных: без окна, канала и доступа к процессору.
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::conditional::{self, Metadata, Prediction, Surface};
use crate::conditional_data::{self, Prepared};

#[derive(Debug, Deserialize)]
struct Request {
    data: Prepared,
    model: Option<Surface>,
    metadata: Option<Metadata>,
    #[serde(default)]
    bands: Vec<[i64; 2]>,
    train: Option<Vec<bool>>,
    ridge: Option<f64>,
    iterations: Option<usize>,
}

#[derive(Debug, Serialize)]
struct Response {
    model: Surface,
    predictions: Vec<Option<Prediction>>,
    elapsed_seconds: f64,
}

fn execute(input: &Path, output: &Path) -> Result<(), String> {
    let started = Instant::now();
    let request: Request =
        serde_json::from_slice(&std::fs::read(input).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let surface = if let Some(model) = request.model {
        model
    } else {
        let metadata = request.metadata.ok_or("нет описания базиса модели")?;
        let train = request.train.ok_or("нет отметок обучающих строк")?;
        let template = Surface::template(metadata, request.bands);
        let ridge = request.ridge.ok_or("нет выбранного штрафа")?;
        if let Some(iterations) = request.iterations {
            conditional::fit_iterations(&request.data, template, &train, ridge, iterations)
        } else {
            conditional::fit(&request.data, template, &train, ridge)
        }
        .map_err(|e| format!("расчёт модели: {e:?}"))?
    };
    let predictions = surface.predict(&request.data).map_err(|e| format!("прогноз: {e:?}"))?;
    let response = Response {
        model: surface,
        predictions,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    };
    let bytes = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
    std::fs::write(output, bytes).map_err(|e| e.to_string())
}

/// Результат — JSON рядом с исходным запросом; при отказе причина пишется в отдельный файл ошибки.
pub fn run(input: &Path, output: &Path) -> i32 {
    match execute(input, output) {
        Ok(()) => 0,
        Err(message) => {
            // Если недоступна даже папка результата, код выхода всё равно показывает отказ.
            let _ = std::fs::write(output.with_extension("error.txt"), message);
            2
        }
    }
}

fn prepare_data(folder: &Path, output: &Path, block: &str) -> Result<(), String> {
    let folder = if folder.join("samples").is_dir() {
        folder.join("samples")
    } else {
        folder.to_path_buf()
    };
    let (logs, trouble) = crate::store::read_all(folder);
    if let Some(message) = trouble {
        return Err(message);
    }
    let names = logs
        .iter()
        .find(|l| l.rows() > 0)
        .map(crate::model::Log::names)
        .ok_or("нет строк срезов")?;
    let die = crate::model_report::die_for(names)?;
    let index = die
        .names
        .iter()
        .position(|n| n == block)
        .ok_or("нет выбранного блока")?;
    let refs: Vec<_> = logs.iter().collect();
    let prepared = conditional_data::prepare(&refs, &die, index);
    let bytes = serde_json::to_vec(&prepared).map_err(|e| e.to_string())?;
    std::fs::write(output, bytes).map_err(|e| e.to_string())
}

pub fn prepare(folder: &Path, output: &Path, block: &str) -> i32 {
    match prepare_data(folder, output, block) {
        Ok(()) => 0,
        Err(message) => {
            // Аналогично run: отказ записи не выдаём за удачный расчёт.
            let _ = std::fs::write(output.with_extension("error.txt"), message);
            2
        }
    }
}
