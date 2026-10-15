// Файлы данных: чтение JSON, запись в том же виде, что Json.cs C#-части, и подмена файла целиком.
//
// Пишет программа входа только profile.json (и свои файлы); profiles.json и метку оборванного замера она только
// читает (DECISIONS #31). Запись совпадает с C# байт в байт: разделители «, » и «: », строки с тем же
// экранированием, дробные числа по образцу «0.###», UTF-8 без метки порядка байтов. Тогда файл, записанный одной
// частью, другая читает и переписывает без единого отличия.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

/// Файл есть, но не читается: испорчен или записан не этой программой. Текст — для владельца, как в Json.Spoiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spoiled(pub String);

impl std::fmt::Display for Spoiled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Spoiled {}

/// Файл прочитан, но не разобран. Сохраняет его рядом как .bad (следующая запись заменила бы его новым, а так
/// прежнее содержимое остаётся владельцу) и возвращает ошибку с текстом для него.
#[must_use]
pub fn spoiled(path: &Path, cause: &str) -> Spoiled {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut bad = path.as_os_str().to_owned();
    bad.push(".bad");
    // Копия — если получится: важнее причина, по которой файл не прочитан.
    let kept = if fs::copy(path, &bad).is_ok() {
        format!("; он сохранён рядом как {name}.bad")
    } else {
        String::new()
    };
    Spoiled(format!("файл {name} не читается ({cause}){kept}"))
}

/// Объект из файла JSON. Ok(None) — файла нет.
pub fn read_object(path: &Path) -> Result<Option<serde_json::Map<String, Value>>, Spoiled> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Spoiled(format!("файл {} не прочитан ({e})", path.display()))),
    };
    let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).into_owned();
    if text.trim().is_empty() {
        return Err(spoiled(path, "файл пуст"));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        Ok(_) => Err(spoiled(path, "в файле не объект")),
        Err(e) => Err(spoiled(path, &e.to_string())),
    }
}

/// Подменяет файл целиком: сначала во временный, потом прежний становится .bak, а новый — на его место
/// (как File.Replace в Json.WriteFile): обрыв посреди записи не оставит половину файла на месте целого.
pub fn write_replacing(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut fresh = path.as_os_str().to_owned();
    fresh.push(".tmp");
    fs::write(&fresh, text.as_bytes())?;
    if path.exists() {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak");
        fs::copy(path, &backup)?;
    }
    fs::rename(&fresh, path)
}

/// Дописывает строку в конец файла (журналы автомодуля): папки и файла нет — создаются. Строка уходит одной записью,
/// так что обрыв не оставит в файле половину строки поверх целых.
pub fn append_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut text = String::with_capacity(line.len() + 1);
    text.push_str(line);
    text.push('\n');
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    io::Write::write_all(&mut file, text.as_bytes())
}

/// Удаляет файл и его .bak (метку и профиль запуска после «заводских»). Файла нет — не ошибка.
pub fn delete_with_backup(path: &Path) -> io::Result<()> {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    for file in [path.as_os_str(), backup.as_os_str()] {
        match fs::remove_file(file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Строка в кавычках, как её пишет Json.Quote: экранируются кавычка, обратная косая черта, \n, \r, \t и прочие
/// управляющие знаки (\u00xx, строчные цифры); остальное — как есть.
#[must_use]
pub fn quote(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 2);
    text.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                text.push('\\');
                text.push(c);
            }
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            c if c < ' ' => {
                let _ = write!(text, "\\u{:04x}", u32::from(c)); // writing into a String cannot fail
            }
            c => text.push(c),
        }
    }
    text.push('"');
    text
}

/// Дробное число по образцу «0.###» (не больше трёх знаков после точки, без хвостовых нулей); NaN — null.
///
/// Округление — как у .NET Framework, которым пишет C#-часть: сначала 15 значащих цифр, потом половина — от нуля.
/// Точное двоичное округление расходилось бы с ним на «ничьих»: 2.9995 у C# — «3», а двоичное ближе к 2.999
/// (найдено тестом по спецификации, измерено на .NET 4.8 этой машины).
#[must_use]
pub fn number(value: f64) -> String {
    if !value.is_finite() {
        return "null".into();
    }
    // «d.dddddddddddddde±x»: 15 значащих цифр.
    let sci = format!("{:.14e}", value.abs());
    let Some((mantissa, exponent)) = sci.split_once('e') else {
        return "null".into();
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return "null".into();
    };
    let digits: Vec<u32> = mantissa.chars().filter_map(|c| c.to_digit(10)).collect();
    // Сколько старших цифр образуют целую часть числа × 1000.
    let keep = exponent + 1 + 3;
    let mut scaled: u128 = 0;
    for position in 0..keep.max(0) {
        let digit = usize::try_from(position)
            .ok()
            .and_then(|p| digits.get(p))
            .copied()
            .unwrap_or(0);
        scaled = scaled.saturating_mul(10).saturating_add(u128::from(digit));
    }
    let next = usize::try_from(keep)
        .ok()
        .and_then(|k| digits.get(k))
        .copied()
        .unwrap_or(0);
    if keep >= 0 && next >= 5 {
        scaled += 1;
    }
    let whole = scaled / 1000;
    let fraction = format!("{:03}", scaled % 1000);
    let fraction = fraction.trim_end_matches('0');
    let sign = if value < 0.0 && scaled > 0 { "-" } else { "" };
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

/// Список целых: «[1, 2, 3]».
#[must_use]
pub fn ints<T: std::fmt::Display>(values: &[T]) -> String {
    let mut text = String::from("[");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            text.push_str(", ");
        }
        let _ = write!(text, "{v}"); // writing into a String cannot fail
    }
    text.push(']');
    text
}

/// Список целых из поля JSON; null внутри — 0 (как Json.Ints).
#[must_use]
pub fn read_ints(value: Option<&Value>) -> Option<Vec<i64>> {
    value?
        .as_array()?
        .iter()
        .map(|v| if v.is_null() { Some(0) } else { v.as_i64() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_like_csharp_0_hash_hash_hash() {
        assert_eq!(number(0.0), "0");
        assert_eq!(number(3.0), "3");
        assert_eq!(number(3.25), "3.25");
        assert_eq!(number(1.23456), "1.235");
        assert_eq!(number(f64::NAN), "null");
    }

    #[test]
    fn quoting_like_csharp() {
        assert_eq!(quote("csmart2"), "\"csmart2\"");
        assert_eq!(quote("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
        assert_eq!(quote("профиль"), "\"профиль\"");
    }
}
