// Время в файлах профилей и в журнале Windows.
//
// Отметка (`Stamp`) — мировое время в тиках по 100 нс от 1601 года (FILETIME Windows). В файлах профилей C#-часть
// пишет местное время со сдвигом в формате «o» (DateTime.ToString("o"): 2026-10-14T18:33:26.1031323-04:00) — так
// же пишется и здесь, чтобы файлы читались обеими частями. Местное время и сдвиг считает Windows по правилам
// часового пояса этой машины для той даты, о которой речь (летнее время учтено).
#![expect(unsafe_code, reason = "time zone conversion: Win32 calls the standard library lacks")]

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows_sys::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime,
};

const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_MINUTE: i64 = 60 * TICKS_PER_SECOND;

/// Мировое время, тики по 100 нс от 1601-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(pub i64);

/// Поля даты и времени.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: u16,
    month: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
}

impl Stamp {
    /// Сейчас.
    #[must_use]
    pub fn now() -> Self {
        let mut ft = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: `ft` is a writable FILETIME.
        unsafe { GetSystemTimePreciseAsFileTime(&raw mut ft) };
        Self(from_filetime(ft))
    }

    /// Сколько часов от `earlier` до этой отметки.
    #[must_use]
    pub fn hours_since(self, earlier: Self) -> f64 {
        // Разница в тиках на десятилетиях укладывается в f64 точно до долей секунды.
        #[expect(
            clippy::cast_precision_loss,
            reason = "tick differences of years stay exact far below a second"
        )]
        let ticks = (self.0 - earlier.0) as f64;
        ticks / (3600.0 * 1e7)
    }

    /// Сдвиг местного времени от мирового в тиках для этой отметки.
    fn offset(self) -> Option<i64> {
        let utc = civil_of(self.0)?;
        let mut local = blank();
        // SAFETY: null time zone = the current one; both SYSTEMTIMEs are valid for the call.
        if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &raw const utc, &raw mut local) } == 0 {
            return None;
        }
        let back = ticks_of(&utc)?;
        Some(ticks_of(&local)? - back)
    }

    /// Местное время со сдвигом, как DateTime.ToString("o") у местной даты в C#.
    #[must_use]
    pub fn roundtrip(self) -> String {
        let offset = self.offset().unwrap_or(0);
        let local = self.0 + offset;
        let Some(st) = civil_of(local) else {
            return String::new();
        };
        let fraction = local.rem_euclid(TICKS_PER_SECOND);
        let minutes = offset / TICKS_PER_MINUTE;
        let sign = if minutes < 0 { '-' } else { '+' };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{fraction:07}{sign}{:02}:{:02}",
            st.wYear,
            st.wMonth,
            st.wDay,
            st.wHour,
            st.wMinute,
            st.wSecond,
            minutes.abs() / 60,
            minutes.abs() % 60
        )
    }

    /// Разбирает время в формате «o»: дата, время, до 7 знаков долей секунды и сдвиг «±чч:мм», «Z» или ничего
    /// (тогда время местное — так читает `DateTime.Parse` с `RoundtripKind`). None — не разобрано.
    #[must_use]
    pub fn parse_roundtrip(text: &str) -> Option<Self> {
        let num = |s: &str| -> Option<u16> {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            s.parse().ok()
        };
        let (date, rest) = text.split_once('T')?;
        let mut parts = date.split('-');
        let (year, month, day) = (num(parts.next()?)?, num(parts.next()?)?, num(parts.next()?)?);
        if parts.next().is_some() || rest.len() < 8 {
            return None;
        }
        let civil = Civil {
            year,
            month,
            day,
            hour: num(rest.get(0..2)?)?,
            minute: num(rest.get(3..5)?)?,
            second: num(rest.get(6..8)?)?,
        };
        if rest.get(2..3)? != ":" || rest.get(5..6)? != ":" {
            return None;
        }
        let mut tail = rest.get(8..)?;
        let mut fraction = 0i64;
        if let Some(digits) = tail.strip_prefix('.') {
            let len = digits.bytes().take_while(u8::is_ascii_digit).count();
            if len == 0 || len > 7 {
                return None;
            }
            let padded = format!("{:0<7}", &digits[..len]);
            fraction = padded.parse().ok()?;
            tail = &digits[len..];
        }
        let wall = ticks_of(&system(civil))? + fraction;
        match tail {
            "Z" => Some(Self(wall)),
            "" => {
                // Местное время без сдвига: Windows переводит его в мировое по правилам часового пояса.
                let local = civil_of(wall)?;
                let mut utc = blank();
                // SAFETY: null time zone = the current one; both SYSTEMTIMEs are valid for the call.
                if unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &raw const local, &raw mut utc) } == 0 {
                    return None;
                }
                Some(Self(ticks_of(&utc)? + wall.rem_euclid(10_000)))
            }
            zone => {
                let sign = match zone.get(0..1)? {
                    "+" => 1,
                    "-" => -1,
                    _ => return None,
                };
                if zone.len() != 6 || zone.get(3..4)? != ":" {
                    return None;
                }
                let minutes = i64::from(num(zone.get(1..3)?)?) * 60 + i64::from(num(zone.get(4..6)?)?);
                Some(Self(wall - sign * minutes * TICKS_PER_MINUTE))
            }
        }
    }

    /// Местное время по образцу: «dd.MM HH:mm», «HH:mm», «yyyy-MM-dd HH:mm:ss», «yyyy-MM-dd».
    #[must_use]
    pub fn local(self, pattern: Pattern) -> String {
        let local = self.0 + self.offset().unwrap_or(0);
        let Some(st) = civil_of(local) else {
            return String::new();
        };
        match pattern {
            Pattern::DayTime => format!("{:02}.{:02} {:02}:{:02}", st.wDay, st.wMonth, st.wHour, st.wMinute),
            Pattern::Time => format!("{:02}:{:02}", st.wHour, st.wMinute),
            Pattern::Log => format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
            ),
            Pattern::Date => format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay),
        }
    }

    /// Мировое время для запроса к журналу Windows: «yyyy-MM-ddTHH:mm:ss.fffZ» (как в Profile.FirstEvent).
    #[must_use]
    pub fn utc_query(self) -> String {
        let Some(st) = civil_of(self.0) else {
            return String::new();
        };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
        )
    }
}

/// Образец местного времени для текстов владельцу и журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    DayTime,
    Time,
    Log,
    /// Имя дневного файла.
    Date,
}

/// Отметка из FILETIME (журнал Windows отдаёт время событий так).
#[must_use]
pub fn from_filetime(ft: FILETIME) -> i64 {
    i64::try_from(u64::from(ft.dwHighDateTime) << 32 | u64::from(ft.dwLowDateTime)).unwrap_or(i64::MAX)
}

fn blank() -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    }
}

fn system(c: Civil) -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: c.year,
        wMonth: c.month,
        wDayOfWeek: 0,
        wDay: c.day,
        wHour: c.hour,
        wMinute: c.minute,
        wSecond: c.second,
        wMilliseconds: 0,
    }
}

/// Поля даты (до миллисекунд) по тикам.
fn civil_of(ticks: i64) -> Option<SYSTEMTIME> {
    let raw = u64::try_from(ticks).ok()?;
    let ft = FILETIME {
        dwLowDateTime: (raw & 0xFFFF_FFFF).try_into().ok()?,
        dwHighDateTime: (raw >> 32).try_into().ok()?,
    };
    let mut st = blank();
    // SAFETY: both structures are valid for the call.
    (unsafe { FileTimeToSystemTime(&raw const ft, &raw mut st) } != 0).then_some(st)
}

/// Тики по полям даты (доли миллисекунды теряются — их добавляет тот, кто знает их сам).
fn ticks_of(st: &SYSTEMTIME) -> Option<i64> {
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: both structures are valid for the call; Windows rejects impossible dates by returning 0.
    (unsafe { SystemTimeToFileTime(st, &raw mut ft) } != 0).then(|| from_filetime(ft))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_by_csharp_parses_and_writes_back_the_same_instant() {
        let text = "2026-10-14T18:33:26.1031323-04:00";
        let stamp = Stamp::parse_roundtrip(text).expect("C#-written stamp parses");
        let again = Stamp::parse_roundtrip(&stamp.roundtrip()).expect("own output parses");
        assert_eq!(stamp, again);
        // 18:33:26.1031323 at -04:00 is 22:33:26.1031323 UTC.
        assert_eq!(stamp.utc_query(), "2026-10-14T22:33:26.103Z");
    }

    #[test]
    fn utc_and_offset_forms_agree() {
        let a = Stamp::parse_roundtrip("2026-01-02T03:04:05.0000001Z").expect("utc");
        let b = Stamp::parse_roundtrip("2026-01-02T06:04:05.0000001+03:00").expect("offset");
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_what_is_not_a_date() {
        for text in [
            "",
            "2026-10-14",
            "2026-10-14T18:33",
            "2026-10-14T18:33:26.12345678Z",
            "x026-10-14T18:33:26Z",
        ] {
            assert_eq!(Stamp::parse_roundtrip(text), None, "{text}");
        }
    }
}
