// Журналы программы входа: data\tray.log — что она делала (постановка профиля, сверка после сна, возврат права
// писать потолки), data\error.log — ошибки, в том же виде, что пишет C#-часть (Program.Log): владелец и агент
// смотрят одно место. tray.log не растёт без конца: больше 256 КБ — остаётся вторая половина.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use machine::clock::{Pattern, Stamp};

const LIMIT: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub struct Journal {
    tray: PathBuf,
    errors: PathBuf,
}

impl Journal {
    #[must_use]
    pub fn new(data: &Path) -> Self {
        Self {
            tray: data.join("tray.log"),
            errors: data.join("error.log"),
        }
    }

    /// Строка в tray.log.
    pub fn note(&self, text: &str) {
        let line = format!("{} {}\r\n", Stamp::now().local(Pattern::Log), text.replace('\n', " / "));
        // Журнал не должен ронять программу: не записался — о нём некому сказать, кроме него самого.
        let _ = append(&self.tray, &line).and_then(|()| trim(&self.tray));
    }

    /// Ошибка: в error.log (как Program.Log в C#) и строкой в tray.log.
    pub fn error(&self, text: &str) {
        let entry = format!("{}\r\n{text}\r\n\r\n", Stamp::now().local(Pattern::Log));
        // Как и выше: ошибку записи журнала сообщить некуда.
        let _ = append(&self.errors, &entry);
        self.note(&format!("ошибка: {text}"));
    }

    #[must_use]
    pub fn tray_path(&self) -> &Path {
        &self.tray
    }
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// Больше предела — остаётся вторая половина, с начала строки.
fn trim(path: &Path) -> std::io::Result<()> {
    if fs::metadata(path)?.len() <= LIMIT {
        return Ok(());
    }
    let bytes = fs::read(path)?;
    let half = bytes.len() / 2;
    let start = bytes[half..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(half, |p| half + p + 1);
    fs::write(path, &bytes[start..])
}
