// Сообщения движка (src/Feed.cs в C#-части) — как их видит окно. Хозяин формата — движок; здесь только поля,
// которые окно показывает. Лишние поля пропускаются, пропавшее нужное поле ломает разбор сообщения — это видно
// в скрытой проверке.
use serde::Deserialize;

pub const PROTOCOL: i64 = 1;

#[derive(Deserialize, Clone, Debug)]
pub struct CoreInfo {
    pub lp: i64,
    pub kind: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct BlockInfo {
    pub index: usize,
    pub members: Vec<usize>,
    pub first: usize,
    pub lp: i64,
    pub kind: String,
    pub name: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Tile {
    pub block: usize,
    // Место на схеме кристалла — единицы и десятки; u16 переводится в i32 Slint и в f32 координат без потерь.
    pub col: u16,
    pub row: u16,
    pub places: Vec<usize>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Layout {
    pub note: Option<String>,
    pub confirmed: bool,
    pub cols: i32,
    pub rows: i32,
    pub tiles: Vec<Tile>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Hello {
    pub cpu: String,
    pub simulated: bool,
    pub cores: Vec<CoreInfo>,
    pub blocks: Vec<BlockInfo>,
    pub layout: Layout,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Fans {
    pub rpm: Option<Vec<Option<f64>>>,
}

/// Числа без значения (датчик не ответил, ядро спало) приходят как null. Ряды — по ядрам.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub pkg: Option<f64>,
    pub watts: Option<f64>,
    pub busy: bool,
    pub cur: Vec<Option<f64>>,
    pub max: Vec<Option<f64>>,
    pub ratio: Vec<Option<f64>>,
    pub avg: Vec<Option<f64>>,
    #[serde(deserialize_with = "flags")]
    pub thr: Vec<bool>,
    #[serde(deserialize_with = "flags")]
    pub load: Vec<bool>,
    pub limit: Option<Vec<i64>>,
    pub fans: Fans,
    pub trip: Option<String>,
    pub sensor_error: Option<String>,
}

/// Потолки по ядрам: заводские и «было»; choices — что можно поставить блоку, по убыванию, по номеру блока.
#[derive(Deserialize, Clone, Debug)]
pub struct Limits {
    pub fused: Vec<i64>,
    pub was: Vec<i64>,
    pub choices: Vec<Vec<i64>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Reply {
    pub re: i64,
    pub ok: bool,
    pub why: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "t")]
pub enum Message {
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "state")]
    State(State),
    #[serde(rename = "limits")]
    Limits(Limits),
    #[serde(rename = "reply")]
    Reply(Reply),
    #[serde(other)]
    Other,
}

/// Признаки по ядрам (троттлинг, нагрузка): движок пишет их 1 и 0, а не true и false (Out.Row в Wire.cs).
fn flags<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Vec<bool>, D::Error> {
    Ok(Vec::<u8>::deserialize(from)?.into_iter().map(|v| v != 0).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Состояние в том виде, в каком его пишет Feed.State: тот же порядок полей, null вместо NaN, признаки 1/0,
    // обороты целыми, поля, которых окно не читает (n, up, pkgMax, level, avgRatio, run…), на месте.
    const STATE: &str = r#"{"t":"state","n":3,"up":1.5,"pkg":52,"pkgMax":60,"watts":null,"since":12,"busy":false,"level":2,"cur":[48,null,51,50],"max":[55,null,58,57],"ratio":[52.0,null,46.0,45.5],"avg":[50.2,null,49.0,48.1],"avgRatio":[51.0,null,45.0,45.0],"thr":[0,1,0,0],"load":[1,0,0,0],"limit":[57,57,46,46],"fans":{"rpm":[2100,2050],"max":false,"spin":false,"mode":0},"run":null,"trip":null,"sensorError":null}"#;

    #[test]
    fn state_as_the_engine_writes_it() {
        let Ok(Message::State(state)) = serde_json::from_str::<Message>(STATE) else {
            panic!("состояние движка не разобралось");
        };
        assert_eq!(state.thr, [false, true, false, false]);
        assert_eq!(state.load, [true, false, false, false]);
        assert_eq!(state.pkg, Some(52.0));
        assert_eq!(state.watts, None);
        assert_eq!(state.cur[1], None);
        assert_eq!(state.limit.as_deref(), Some(&[57, 57, 46, 46][..]));
        assert_eq!(state.fans.rpm, Some(vec![Some(2100.0), Some(2050.0)]));
    }

    #[test]
    fn unknown_topic_is_skipped_not_an_error() {
        let message = serde_json::from_str::<Message>(r#"{"t":"profiles","sets":[]}"#);
        assert!(matches!(message, Ok(Message::Other)));
    }
}
