// Разгонный почтовый ящик процессора (регистр 0x150) — только две команды: прочитать потолок множителя ядра и
// записать его (как OcMailbox в Tuning.cs). Других команд здесь нет намеренно: ошибка в номере команды отправила
// бы процессору чужой запрос (напряжение, частота шины), а не потолок.
//
// Запрос: бит 63 — «выполнить», биты 32–39 — команда, 40–47 и 48… — параметры (здесь нули), 0–31 — данные.
// Ответ приходит в тот же регистр: бит 63 снят — готово; биты 32–39 — код ошибки (0 — принято), 0–31 — данные.
// Ответ на чтение потолка (команда 0x1C), проверено на 275HX: байт 1 — заводской предел, байт 0 — действующий
// потолок. Запись (0x1D) — потолок в байте 0; принята ли она, видно только обратным чтением (limits.rs).

use std::time::{Duration, Instant};

use crate::named::{self, Lease};
use crate::pawnio::Msr;
use crate::topology::Topology;
use crate::{HwError, Lp, affinity};

/// Регистр почтового ящика — единственный, в который пишет эта библиотека (pawnio.rs отказывает в остальных).
pub const REGISTER: u32 = 0x150;
const READ_CORE_LIMIT: u8 = 0x1C;
const WRITE_CORE_LIMIT: u8 = 0x1D;
const RUN: u64 = 1 << 63;
/// Сколько ждать ответа (и освобождения ящика перед запросом). Ответ приходит за микросекунды; срок — по часам, а не
/// числом опросов: длительность паузы между опросами на этом процессоре не мерена (ревизия, outputs/review-write-path
/// -2026-10-14.md п. 10).
const PATIENCE: Duration = Duration::from_millis(20);
const GATE_MS: u32 = 2000;

/// Потолок, прошедший проверку допустимого диапазона (`Limits::set`). Создать его можно только внутри библиотеки:
/// другой дороги к записи в процессор нет (ревизия, п. 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowed(u8);

impl Allowed {
    pub(crate) fn checked(ratio: u8) -> Self {
        Self(ratio)
    }

    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

/// Запрос к почтовому ящику: команда и данные, параметры — нули.
#[must_use]
pub fn request(command: u8, data: u32) -> u64 {
    RUN | u64::from(command) << 32 | u64::from(data)
}

/// Код ошибки (биты 32–39) и данные (биты 0–31) из ответа.
#[must_use]
pub fn reply(answer: u64) -> (u8, u32) {
    let b = answer.to_le_bytes();
    (b[4], u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Ответ процессора на запрос. `error`: 0 — принято; иначе код отказа; None — ответа не дождались.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub error: Option<u8>,
    pub data: u32,
}

/// Один запрос: записать, дождаться снятия бита 63, разобрать ответ. Перед записью — ящик должен быть свободен
/// (бит 63 снят): иначе наш запрос лёг бы поверх чужого. Вызывать только с привязанным потоком и под замком ящика.
fn exchange(msr: &mut dyn Msr, command: u8, data: u32) -> Result<Answer, HwError> {
    if !settled(msr)? {
        return Err(HwError(
            "почтовый ящик процессора занят чужим запросом — запрос не отправлен".into(),
        ));
    }
    msr.write(REGISTER, request(command, data))?;
    let deadline = Instant::now() + PATIENCE;
    loop {
        let answer = msr.read(REGISTER)?;
        if answer & RUN == 0 {
            let (error, data) = reply(answer);
            return Ok(Answer {
                error: Some(error),
                data,
            });
        }
        if Instant::now() >= deadline {
            return Ok(Answer { error: None, data: 0 });
        }
        pause();
    }
}

/// Ящик свободен (бит 63 снят), подождав не дольше, чем ждём ответа.
fn settled(msr: &mut dyn Msr) -> Result<bool, HwError> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if msr.read(REGISTER)? & RUN == 0 {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        pause();
    }
}

/// Короткая пауза между чтениями ответа (как `Thread.SpinWait` в C#).
fn pause() {
    for _ in 0..200 {
        std::hint::spin_loop();
    }
}

/// Потолки в настоящем процессоре: драйвер, замок ящика, привязка потока к ядру на время запроса.
#[derive(Debug)]
pub struct MailboxCpu {
    pub(crate) msr: Box<dyn MsrDebug>,
    gate: named::Mutex,
    pub(crate) lps: Vec<Lp>,
    /// Датчики автомодуля (thermal.rs): счётчики ядер с прошлого опроса, `TjMax` и опорный множитель.
    pub(crate) sensors: crate::thermal::Counters,
}

/// `Msr` с `Debug` — чтобы `MailboxCpu` можно было показать в отладке.
pub trait MsrDebug: Msr + std::fmt::Debug {}
impl<T: Msr + std::fmt::Debug> MsrDebug for T {}

impl MailboxCpu {
    /// `gate` — имя замка почтового ящика (`Names::mailbox`): общий с C#-частью, чтобы её запрос не лёг между
    /// нашей записью и чтением ответа.
    pub fn new(msr: Box<dyn MsrDebug>, gate: &str, topology: &Topology) -> Result<Self, HwError> {
        let (gate, _) =
            named::create(gate, false).map_err(|e| HwError(format!("замок почтового ящика не создан: {e}")))?;
        Ok(Self {
            msr,
            gate,
            lps: topology.lps(),
            sensors: crate::thermal::Counters::new(topology.cores.len()),
        })
    }

    /// Запрос на ядре `core` (позиция в списке ядер). Ok(None) — поток не привязался к ядру, запрос не отправлен.
    fn query(&mut self, core: usize, command: u8, data: u32) -> Result<Option<Answer>, HwError> {
        let lp = self.lps[core];
        // Замок ящика держится весь запрос. Не дождались — отказ, а не запрос без замка (в C# запрос шёл и без
        // него — ошибка B3 описи, outputs/resident-inventory-2026-10-14.md).
        let Some(_gate) = Lease::take(&self.gate, GATE_MS) else {
            return Err(HwError(
                "почтовый ящик процессора занят другой частью CoreMap дольше 2 с — запрос не отправлен".into(),
            ));
        };
        let Some(_pinned) = affinity::pin(lp) else {
            return Ok(None);
        };
        exchange(self.msr.as_mut(), command, data).map(Some)
    }
}

/// Что ядро сказало о своём потолке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    pub fused: u8,
    pub current: u8,
}

/// Где стоят потолки: настоящий процессор (`MailboxCpu`) или имитация (`sim::SimCpu`). Ядро — позиция в списке
/// ядер топологии. Зовут только `Limits` (limits.rs): там проверяется, что значение допустимо.
pub trait Cpu: std::fmt::Debug {
    /// Потолок ядра. Ok(None) — ядро не ответило (поток не привязался или процессор отказал).
    fn ask(&mut self, core: usize) -> Result<Option<Limit>, HwError>;
    /// Пишет потолок ядру (у кластера E-ядер процессор сам распространяет его на кластер). Значение — только
    /// прошедшее проверку диапазона (`Allowed`). Ok(None) — поток не привязался, запись не отправлена;
    /// Ok(Some(code)) — код ответа на команду (-1 — ответа не дождались).
    fn tell(&mut self, core: usize, ratio: Allowed) -> Result<Option<i32>, HwError>;
    /// Конец действия: отпустить драйвер до следующего (программа входа не держит его между действиями).
    fn release(&mut self) {}
}

impl Cpu for MailboxCpu {
    fn ask(&mut self, core: usize) -> Result<Option<Limit>, HwError> {
        Ok(self
            .query(core, READ_CORE_LIMIT, 0)?
            .and_then(|answer| match answer.error {
                Some(0) => {
                    let [current, fused, ..] = answer.data.to_le_bytes();
                    Some(Limit { fused, current })
                }
                _ => None,
            }))
    }

    fn tell(&mut self, core: usize, ratio: Allowed) -> Result<Option<i32>, HwError> {
        Ok(self
            .query(core, WRITE_CORE_LIMIT, u32::from(ratio.get()))?
            .map(|answer| answer.error.map_or(-1, i32::from)))
    }

    fn release(&mut self) {
        self.msr.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_request_puts_ratio_in_low_byte_and_command_in_bits_32_to_39() {
        // ×45 → данные 0x2D, команда 0x1D, бит «выполнить».
        assert_eq!(request(WRITE_CORE_LIMIT, 45), 0x8000_001D_0000_002D);
        assert_eq!(request(READ_CORE_LIMIT, 0), 0x8000_001C_0000_0000);
    }

    /// Поддельный регистр 0x150: что в нём лежит, сколько чтений после записи он ещё «занят», не отвечает ли вовсе.
    #[derive(Debug, Default)]
    struct Register {
        value: u64,
        busy_reads: usize,
        busy_after_write: usize,
        stuck: bool,
        broken_after_write: bool,
        written: Vec<(u32, u64)>,
        reply: u64,
    }

    impl Msr for Register {
        fn read(&mut self, _msr: u32) -> Result<u64, HwError> {
            if self.broken_after_write && !self.written.is_empty() {
                return Err(HwError("драйвер отказал".into()));
            }
            if self.stuck {
                return Ok(RUN);
            }
            if self.busy_reads > 0 {
                self.busy_reads -= 1;
                return Ok(self.value | RUN);
            }
            Ok(self.value)
        }

        fn write(&mut self, msr: u32, value: u64) -> Result<(), HwError> {
            self.written.push((msr, value));
            self.value = self.reply;
            self.busy_reads = self.busy_after_write;
            Ok(())
        }
    }

    #[test]
    fn a_busy_mailbox_gets_no_request_on_top() {
        let mut register = Register {
            stuck: true,
            ..Register::default()
        };
        let refused = exchange(&mut register, WRITE_CORE_LIMIT, 45);
        assert!(refused.is_err(), "busy before the write must refuse");
        assert!(
            register.written.is_empty(),
            "nothing may be written over a pending request"
        );
    }

    #[test]
    fn an_answer_that_never_comes_is_reported_as_none_not_as_accepted() {
        let mut register = Register {
            busy_after_write: usize::MAX,
            ..Register::default()
        };
        let answer = exchange(&mut register, WRITE_CORE_LIMIT, 45).expect("the write itself went through");
        assert_eq!(answer.error, None);
        assert_eq!(register.written, vec![(REGISTER, 0x8000_001D_0000_002D)]);
    }

    #[test]
    fn a_slow_answer_is_waited_for_and_decoded() {
        let mut register = Register {
            busy_after_write: 3,
            reply: 0x0000_0000_0000_3635,
            ..Register::default()
        };
        let answer = exchange(&mut register, READ_CORE_LIMIT, 0).expect("answered");
        assert_eq!(
            answer,
            Answer {
                error: Some(0),
                data: 0x3635
            }
        );
    }

    #[test]
    fn a_driver_failure_after_the_write_is_an_error_not_an_answer() {
        let mut register = Register {
            broken_after_write: true,
            ..Register::default()
        };
        assert!(exchange(&mut register, WRITE_CORE_LIMIT, 45).is_err());
        assert_eq!(register.written.len(), 1);
    }

    fn cpu_on(lp: u32, gate: &str) -> MailboxCpu {
        let (gate, _) = named::create(gate, false).expect("test gate");
        MailboxCpu {
            msr: Box::new(Register::default()),
            gate,
            lps: vec![Lp(lp)],
            sensors: crate::thermal::Counters::new(1),
        }
    }

    fn written(cpu: &MailboxCpu) -> String {
        format!("{:?}", cpu.msr)
    }

    #[test]
    fn a_core_the_thread_cannot_be_pinned_to_gets_no_write() {
        // 63 is outside this process's processors on any machine with fewer: SetThreadAffinityMask refuses.
        let mut cpu = cpu_on(63, &format!("Local\\CoreMap.Mailbox.Test.{}.pin", std::process::id()));
        assert_eq!(cpu.tell(0, Allowed::checked(45)).expect("no driver error"), None);
        assert!(written(&cpu).contains("written: []"), "{}", written(&cpu));
    }

    #[test]
    fn a_gate_held_elsewhere_means_refusal_not_a_request_without_it() {
        let name = format!("Local\\CoreMap.Mailbox.Test.{}.gate", std::process::id());
        let (held, release) = std::sync::mpsc::channel::<()>();
        let (stop, wait) = std::sync::mpsc::channel::<()>();
        let holder = {
            let name = name.clone();
            std::thread::spawn(move || {
                let gate = named::create(&name, true).expect("holder gate");
                held.send(()).expect("test alive");
                wait.recv().expect("test alive");
                gate.0.release();
            })
        };
        release.recv().expect("holder started");
        let mut cpu = cpu_on(0, &name);
        let started = Instant::now();
        assert!(cpu.tell(0, Allowed::checked(45)).is_err());
        assert!(
            started.elapsed() >= Duration::from_millis(1900),
            "waited for the gate first"
        );
        assert!(written(&cpu).contains("written: []"), "{}", written(&cpu));
        stop.send(()).expect("holder alive");
        holder.join().expect("holder ends");
    }

    #[test]
    fn reply_of_the_275hx_reads_fused_in_byte_1_and_current_in_byte_0() {
        // Ответы, измеренные на процессоре владельца (KNOWLEDGE.md): P-ядра 12 и 23 — 0x3635, другие P — 0x3535.
        let (error, data) = reply(0x0000_0000_0000_3635);
        assert_eq!(error, 0);
        assert_eq!(data.to_le_bytes()[..2], [0x35, 0x36]);
        let (error, _) = reply(0x0000_0007_0000_0000);
        assert_eq!(error, 7);
    }
}
