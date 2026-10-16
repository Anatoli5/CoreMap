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

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Fans {
    pub rpm: Option<Vec<Option<f64>>>,
}

/// Числа без значения (датчик не ответил, ядро спало) приходят как null. Ряды — по ядрам; ряд целиком null (у
/// программы входа — пока процессором владеет прежнее окно или движок, причина — в sensorError) — пустой ряд.
/// Поля, которых у программы входа может не быть (ватты, ход измерения), — по умолчанию пустые: их отсутствие не
/// ломает разбор состояния.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub pkg: Option<f64>,
    #[serde(default)]
    pub watts: Option<f64>,
    #[serde(default)]
    pub busy: bool,
    #[serde(default, deserialize_with = "row")]
    pub cur: Vec<Option<f64>>,
    #[serde(default, deserialize_with = "row")]
    pub max: Vec<Option<f64>>,
    #[serde(default, deserialize_with = "row")]
    pub ratio: Vec<Option<f64>>,
    /// Средняя температура под нагрузкой (ø) и средний фактический множитель за то же время; null — нагрузки не было.
    #[serde(default, deserialize_with = "row")]
    pub avg: Vec<Option<f64>>,
    #[serde(default, deserialize_with = "row")]
    pub avg_ratio: Vec<Option<f64>>,
    #[serde(default, deserialize_with = "flags")]
    pub thr: Vec<bool>,
    #[serde(default, deserialize_with = "flags")]
    pub load: Vec<bool>,
    #[serde(default)]
    pub limit: Option<Vec<i64>>,
    #[serde(default)]
    pub fans: Fans,
    #[serde(default)]
    pub trip: Option<String>,
    #[serde(default)]
    pub sensor_error: Option<String>,
    /// Надстройка автомодуля по блокам (программа входа; у движка C# её нет). Форма поля — см. `AutoMark`; поле
    /// неожиданной формы не ломает состояние, а пропускается.
    #[serde(default, deserialize_with = "lenient")]
    pub auto: Option<Vec<Option<AutoMark>>>,
    /// Потолки владельца по ядрам — под поправкой автомодуля (программа входа; у движка C# нет): поправка блока b —
    /// auto[b] − intended[первое ядро b].
    #[serde(default, deserialize_with = "lenient")]
    pub intended: Option<Vec<Option<i64>>>,
    /// Отсечка автомодуля, °C (программа входа; null — автомодуль выключен, у движка C# поля нет).
    #[serde(default, deserialize_with = "lenient")]
    pub auto_limit: Option<f64>,
}

/// Что автомодуль держит у блока: потолок, который он поставил, или подробнее — потолок, от какого принятого и
/// запрет поднимать обратно. null у блока — автомодуль его не трогает.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(untagged)]
pub enum AutoMark {
    Ceiling(i64),
    Detail {
        #[serde(alias = "ceiling")]
        to: Option<i64>,
        /// Принятый потолок, от которого считается поправка; нет — считается от «было».
        #[serde(alias = "accepted")]
        base: Option<i64>,
        /// Потолок, на который нельзя возвращаться до `until` (память перегрева).
        ban: Option<i64>,
        until: Option<String>,
    },
}

impl AutoMark {
    pub fn ceiling(&self) -> Option<i64> {
        match self {
            AutoMark::Ceiling(v) => Some(*v),
            AutoMark::Detail { to, .. } => *to,
        }
    }

    pub fn base(&self) -> Option<i64> {
        match self {
            AutoMark::Ceiling(_) => None,
            AutoMark::Detail { base, .. } => *base,
        }
    }

    pub fn ban(&self) -> Option<(i64, Option<&str>)> {
        match self {
            AutoMark::Ceiling(_) => None,
            AutoMark::Detail { ban, until, .. } => ban.map(|b| (b, until.as_deref())),
        }
    }
}

/// Профиль запуска: что ставится при входе в Windows (Feed.Profiles, Profile.Describe/Details в C#-части).
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Startup {
    pub exists: bool,
    pub name: Option<String>,
    pub disabled: bool,
    pub describe: Option<String>,
    pub details: Option<String>,
    pub note: Option<String>,
}

/// Сохранённый набор потолков: потолки по ядрам.
#[derive(Deserialize, Clone, Debug)]
pub struct ProfileSet {
    pub name: String,
    pub ratios: Vec<i64>,
}

/// Профили и что из них стоит. standing / wasNamed: название набора, "" — заводские, null — ни один сохранённый.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Profiles {
    #[serde(default)]
    pub startup: Startup,
    pub sets: Vec<ProfileSet>,
    pub standing: Option<String>,
    #[serde(default)]
    pub was_named: Option<String>,
    #[serde(default)]
    pub can_accept: bool,
}

/// Журнал смен профиля (тема программы входа «history»): записи — объекты; какие поля окно читает, см.
/// `samples::history_line`. Сама тема может прийти списком под любым из названий ниже.
#[derive(Deserialize, Clone, Debug)]
pub struct History {
    #[serde(default, alias = "items", alias = "log", alias = "lines")]
    pub entries: Vec<serde_json::Map<String, serde_json::Value>>,
}

/// Потолки по ядрам: заводские и «было»; choices — что можно поставить блоку, по убыванию, по номеру блока.
#[derive(Deserialize, Clone, Debug)]
pub struct Limits {
    pub fused: Vec<i64>,
    pub was: Vec<i64>,
    pub choices: Vec<Vec<i64>>,
}

/// Выбор владельца для автомодуля (тема программы входа «settings»; у движка C# её нет). Хозяин выбора и
/// промежутков — программа входа (data\tray.json): окно показывает их и шлёт изменения командой auto.set.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AutoSettings {
    pub on: bool,
    pub limit: i64,
    pub depth: i64,
    pub step_up: bool,
    /// Допустимая отсечка, °C, и глубина, ступеней: [от, до].
    pub limit_range: [i64; 2],
    pub depth_range: [i64; 2],
}

/// Тема «settings»: выбор автомодуля; поле неожиданной формы — как будто темы нет (настройки в окне не показываются).
#[derive(Deserialize, Clone, Debug)]
pub struct Settings {
    #[serde(default, deserialize_with = "lenient")]
    pub auto: Option<AutoSettings>,
}

/// Сообщение владельцу (то, что программа сказала бы всплывающим окошком); прочитано — команда notices.seen {seq}.
#[derive(Deserialize, Clone, Debug)]
pub struct Notice {
    pub seq: i64,
    pub text: String,
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
    #[serde(rename = "profiles")]
    Profiles(Profiles),
    #[serde(rename = "history")]
    History(History),
    #[serde(rename = "notice")]
    Notice(Notice),
    #[serde(rename = "settings")]
    Settings(Settings),
    #[serde(other)]
    Other,
}

/// Признаки по ядрам (троттлинг, нагрузка): движок пишет их 1 и 0, а не true и false (Out.Row в Wire.cs); ряд или
/// значение null — «нет» (неизвестно).
fn flags<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Vec<bool>, D::Error> {
    Ok(Option::<Vec<Option<u8>>>::deserialize(from)?
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.is_some_and(|v| v != 0))
        .collect())
}

/// Ряд чисел по ядрам; null вместо ряда — пустой ряд (показаний нет).
fn row<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Vec<Option<f64>>, D::Error> {
    Ok(Option::<Vec<Option<f64>>>::deserialize(from)?.unwrap_or_default())
}

/// Необязательное поле новой формы: не разобралось — как будто его нет (форму задаёт другая программа, и её
/// ошибка не должна гасить показ всего состояния).
fn lenient<'de, D, T>(from: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(from)?;
    Ok(serde_json::from_value(value).ok())
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
    fn state_carries_means_under_load() {
        let Ok(Message::State(state)) = serde_json::from_str::<Message>(STATE) else {
            panic!("состояние движка не разобралось");
        };
        assert_eq!(state.avg[0], Some(50.2));
        assert_eq!(state.avg_ratio, [Some(51.0), None, Some(45.0), Some(45.0)]);
        assert_eq!(state.auto, None);
    }

    #[test]
    fn unknown_topic_is_skipped_not_an_error() {
        let message = serde_json::from_str::<Message>(r#"{"t":"curves","sets":[]}"#);
        assert!(matches!(message, Ok(Message::Other)));
    }

    // Профили в том виде, в каком их пишет Feed.Profiles (Json.Text: поля по порядку, null у «ни один»).
    const PROFILES: &str = r#"{"t":"profiles","startup":{"exists":true,"name":"csmart2","state":"ok","disabled":false,"describe":"при запуске ставится «csmart2»","details":null,"note":null},"sets":[{"name":"csmart2","ratios":[43,53,45,45,45,45]},{"name":"тихий","ratios":[40,40,40,40,40,40]}],"standing":null,"wasNamed":"csmart2","canAccept":true}"#;

    #[test]
    fn profiles_as_the_engine_writes_them() {
        let Ok(Message::Profiles(p)) = serde_json::from_str::<Message>(PROFILES) else {
            panic!("профили не разобрались");
        };
        assert_eq!(p.sets.len(), 2);
        assert_eq!(p.sets[1].name, "тихий");
        assert_eq!(p.standing, None);
        assert_eq!(p.was_named.as_deref(), Some("csmart2"));
        assert!(p.can_accept && p.startup.exists && !p.startup.disabled);
    }

    #[test]
    fn auto_overlay_in_either_shape_and_a_bad_one_ignored() {
        let with = |auto: &str| {
            let text = STATE.replace(r#""trip":null"#, &format!(r#""auto":{auto},"trip":null"#));
            match serde_json::from_str::<Message>(&text) {
                Ok(Message::State(s)) => s.auto,
                other => panic!("состояние с полем auto не разобралось: {other:?}"),
            }
        };
        let plain = with("[null,41,null,null]").expect("потолки по блокам");
        assert_eq!(plain[1].as_ref().and_then(AutoMark::ceiling), Some(41));
        let detail = with(r#"[{"to":41,"base":43,"ban":43,"until":"2026-10-15T02:00:00"},null]"#).expect("подробно");
        let mark = detail[0].as_ref().expect("блок 0");
        assert_eq!((mark.ceiling(), mark.base()), (Some(41), Some(43)));
        assert_eq!(mark.ban(), Some((43, Some("2026-10-15T02:00:00"))));
        assert_eq!(with(r#""что-то другое""#), None);
    }

    #[test]
    fn state_of_the_logon_program_while_another_part_owns_the_cpu() {
        // Программа входа, пока процессором владеет прежнее окно: потолков нет, ряды датчиков — null, причина словами.
        let text = r#"{"t":"state","n":7,"up":3.5,"pkg":null,"pkgMax":null,"since":3,"busy":false,"level":0,"cur":null,"max":null,"ratio":null,"avg":null,"avgRatio":null,"thr":null,"load":null,"limit":null,"fans":{"rpm":null,"max":false,"spin":false,"mode":0},"run":null,"trip":null,"sensorError":"открыто прежнее окно","busyShare":null,"auto":[null,null],"intended":null}"#;
        let Ok(Message::State(state)) = serde_json::from_str::<Message>(text) else {
            panic!("состояние программы входа не разобралось");
        };
        assert!(state.cur.is_empty() && state.load.is_empty() && state.limit.is_none() && state.intended.is_none());
        assert_eq!(state.sensor_error.as_deref(), Some("открыто прежнее окно"));
        // Её обычное состояние: ватт нет, средние и надстройка есть.
        let text = r#"{"t":"state","n":8,"pkg":61,"busy":false,"cur":[60,null],"max":[70,null],"ratio":[45.2,null],"avg":[58.5,null],"avgRatio":[44.9,null],"thr":[0,0],"load":[1,0],"limit":[45,46],"fans":{"rpm":[2400,2100]},"run":null,"trip":null,"sensorError":null,"busyShare":[0.93,null],"auto":[44,null],"intended":[46,46]}"#;
        let Ok(Message::State(state)) = serde_json::from_str::<Message>(text) else {
            panic!("состояние программы входа не разобралось");
        };
        assert_eq!(state.watts, None);
        assert_eq!(state.avg_ratio[0], Some(44.9));
        assert_eq!(state.intended, Some(vec![Some(46), Some(46)]));
        assert_eq!(
            state
                .auto
                .as_ref()
                .and_then(|a| a[0].as_ref())
                .and_then(AutoMark::ceiling),
            Some(44)
        );
    }

    #[test]
    fn notice_and_history_topics() {
        let Ok(Message::Notice(n)) =
            serde_json::from_str::<Message>(r#"{"t":"notice","seq":3,"text":"профиль поставлен"}"#)
        else {
            panic!("сообщение владельцу не разобралось");
        };
        assert_eq!((n.seq, n.text.as_str()), (3, "профиль поставлен"));
        let text = r#"{"t":"history","entries":[{"time":"2026-10-15T01:20:00.0000000+03:00","at":1,"act":"open","name":"csmart2","before":[53],"after":[52]}]}"#;
        let Ok(Message::History(h)) = serde_json::from_str::<Message>(text) else {
            panic!("история не разобралась");
        };
        assert_eq!(h.entries.len(), 1);
    }

    #[test]
    fn settings_topic_as_the_logon_program_writes_it() {
        // Тема в том виде, в каком её пишет программа входа (rust/tray/src/app/auto.rs, Choice::topic).
        let text = r#"{"t":"settings","auto":{"on":true,"limit":82,"depth":4,"stepUp":false,"limitRange":[60,95],"depthRange":[1,8]}}"#;
        let Ok(Message::Settings(s)) = serde_json::from_str::<Message>(text) else {
            panic!("тема settings не разобралась");
        };
        let auto = s.auto.expect("выбор автомодуля");
        assert_eq!((auto.on, auto.limit, auto.depth, auto.step_up), (true, 82, 4, false));
        assert_eq!((auto.limit_range, auto.depth_range), ([60, 95], [1, 8]));
        // Поле другой формы не ломает сообщение: настроек просто нет.
        let Ok(Message::Settings(s)) = serde_json::from_str::<Message>(r#"{"t":"settings","auto":{"on":"да"}}"#)
        else {
            panic!("тема settings другой формы сломала разбор");
        };
        assert!(s.auto.is_none());
    }

    #[test]
    fn state_without_engine_only_fields() {
        // Программа входа может не слать нагрузку, троттлинг, средние и вентиляторы: состояние всё равно читается.
        let text = r#"{"t":"state","pkg":50,"cur":[48],"max":[55],"ratio":[45.0],"limit":[46]}"#;
        let Ok(Message::State(state)) = serde_json::from_str::<Message>(text) else {
            panic!("состояние без полей движка не разобралось");
        };
        assert!(state.load.is_empty() && state.thr.is_empty() && state.avg.is_empty() && !state.busy);
    }
}
