// Потолки множителя по ядрам: заводской предел, действующий потолок и его изменение (как CoreLimits в Tuning.cs).
//
// Что можно поставить ядру — `choices`: от четырёх ступеней выше заводского предела до четырнадцати ниже, не ниже
// ×8. Любая запись проходит эту проверку здесь, до почтового ящика. Принята ли запись, видно только обратным
// чтением: после записи блок перечитывается (в C# перечитывались все 24 ядра — лишняя работа, B16 описи).
// Ядро, о котором процессор не ответил, остаётся неизвестным: его потолок не показывается и не меняется.

use crate::HwError;
use crate::mailbox::{Allowed, Cpu, Limit};
use crate::topology::Topology;

const SPAN: i32 = 14; // на столько ступеней ниже заводского предела можно опустить потолок
/// На столько ступеней выше заводского предела потолок можно поднять вручную (как в Omen Gaming Hub).
pub const OVER: i32 = 4;
const LOWEST: i32 = 8;
const ATTEMPTS: usize = 3; // столько раз ядро спрашивается о потолке, прежде чем чтение считается неудачным
/// Заводской предел вне этих границ — не ответ о потолке (чужой запрос в ящике, другой процессор): от него считался бы
/// весь допустимый диапазон (ревизия, outputs/review-write-path-2026-10-14.md п. 6). У 275HX — ×46…×54.
const PLAUSIBLE: std::ops::RangeInclusive<u8> = 8..=80;

fn plausible(limit: Limit) -> bool {
    PLAUSIBLE.contains(&limit.fused)
}

/// Что программа знает о потолках после последнего обращения к процессору.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    fused: Vec<i32>,   // 0 — процессор не ответил ни разу
    current: Vec<i32>, // действующий потолок; у неответившего ядра — прежний
    silent: Vec<bool>, // при последнем чтении ядро не ответило: current устарел
}

impl Limits {
    /// Первое чтение всех ядер. Второе значение — о каких ядрах процессор не ответил (None — ответили все).
    pub fn read(cpu: &mut dyn Cpu, topology: &Topology) -> Result<(Self, Option<String>), HwError> {
        let count = topology.cores.len();
        let mut limits = Self {
            fused: vec![0; count],
            current: vec![0; count],
            silent: vec![true; count],
        };
        let unread = limits.read_cores(cpu, topology, &(0..count).collect::<Vec<_>>())?;
        Ok((limits, unread))
    }

    /// Перечитывает действующие потолки: их мог сбросить сон или изменить другая программа.
    /// None — прочитаны все; иначе причина, и тогда часть потолков — прежние, а не из процессора.
    pub fn refresh(&mut self, cpu: &mut dyn Cpu, topology: &Topology) -> Option<String> {
        let all: Vec<usize> = (0..self.current.len()).collect();
        match self.read_cores(cpu, topology, &all) {
            Ok(unread) => unread,
            Err(e) => Some(e.0),
        }
    }

    /// Перечитывает только первое ядро каждого блока — потолок у блока общий. Опрос автомодуля сравнивает только
    /// их: так он не будит раз в 2 с кластеры E-ядер, которые Windows держит спящими (ревизия, L10).
    pub fn refresh_blocks(&mut self, cpu: &mut dyn Cpu, topology: &Topology) -> Option<String> {
        let firsts: Vec<usize> = topology.blocks.iter().map(|b| b.first).collect();
        match self.read_cores(cpu, topology, &firsts) {
            Ok(unread) => unread,
            Err(e) => Some(e.0),
        }
    }

    /// Читает данные ядра. Неудачный ответ не стирает известное: заводской предел читается один раз, действующий
    /// потолок остаётся прежним — но ядро помечается, и чтение не выдаёт себя за удачное.
    fn read_cores(
        &mut self,
        cpu: &mut dyn Cpu,
        topology: &Topology,
        cores: &[usize],
    ) -> Result<Option<String>, HwError> {
        let mut unread = Vec::new();
        for &i in cores {
            let mut answer = None;
            for _ in 0..ATTEMPTS {
                match cpu.ask(i) {
                    Ok(Some(limit)) if plausible(limit) => {
                        answer = Some(limit);
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // Чтение оборвалось: про все ядра известно только то, что было раньше.
                        self.silent.fill(true);
                        return Err(e);
                    }
                }
            }
            if let Some(limit) = answer {
                self.silent[i] = false;
                if self.fused[i] == 0 {
                    self.fused[i] = i32::from(limit.fused);
                }
                self.current[i] = i32::from(limit.current);
            } else {
                self.silent[i] = true;
                unread.push(topology.cores[i].lp.to_string());
            }
        }
        Ok((!unread.is_empty()).then(|| format!("процессор не ответил о потолке ядер: {}", unread.join(", "))))
    }

    /// При последнем чтении ядро ответило: его `current` — из процессора, а не из памяти программы.
    #[must_use]
    pub fn answered(&self, core: usize) -> bool {
        !self.silent[core]
    }

    /// Процессор хоть раз ответил о потолке ядра.
    #[must_use]
    pub fn known(&self, core: usize) -> bool {
        self.fused[core] > 0
    }

    #[must_use]
    pub fn fused(&self) -> &[i32] {
        &self.fused
    }

    #[must_use]
    pub fn current(&self) -> &[i32] {
        &self.current
    }

    /// Значения, которые можно поставить ядру, по убыванию: от `OVER` ступеней выше заводского предела вниз.
    #[must_use]
    pub fn choices(&self, core: usize) -> Vec<i32> {
        let fused = self.fused[core];
        (-OVER..=SPAN).map(|k| fused - k).filter(|&r| r >= LOWEST).collect()
    }

    /// Действующие потолки совпадают с набором (ядра, о которых процессор не ответил, не в счёт).
    #[must_use]
    pub fn same(&self, ratios: &[i32]) -> bool {
        (0..self.current.len()).all(|i| !self.known(i) || ratios.get(i) == Some(&self.current[i]))
    }

    /// В наборе есть потолок выше заводского предела у известного ядра.
    #[must_use]
    pub fn above_factory(&self, ratios: &[i32]) -> bool {
        (0..self.fused.len()).any(|i| self.known(i) && ratios.get(i).is_some_and(|&r| r > self.fused[i]))
    }

    /// Ставит каждому блоку потолок из набора (по первому ядру блока: потолок у блока общий). None — все приняты;
    /// иначе причина последнего отказа.
    pub fn apply(&mut self, cpu: &mut dyn Cpu, topology: &Topology, ratios: &[i32]) -> Option<String> {
        // Сверка с процессором, а не с памятью программы: потолки могли измениться с прошлого чтения. Чтение
        // оборвалось (драйвер, занятый ящик) — писать вслепую не за чем: каждая запись упёрлась бы в то же
        // (ревизия, п. 7).
        let all: Vec<usize> = (0..self.current.len()).collect();
        if let Err(e) = self.read_cores(cpu, topology, &all) {
            return Some(e.0);
        }
        let mut refusal = None;
        for block in &topology.blocks {
            let i = block.first;
            let Some(&ratio) = ratios.get(i) else { continue };
            // Потолок, о котором процессор не ответил, пишется и тогда, когда по памяти программы он уже такой.
            if self.known(i) && (self.silent[i] || self.current[i] != ratio) {
                refusal = self.set(cpu, topology, i, ratio).or(refusal);
            }
        }
        refusal
    }

    /// Ставит потолок ядру, а с ним — всему его блоку. None — принято; иначе причина отказа.
    pub fn set(&mut self, cpu: &mut dyn Cpu, topology: &Topology, core: usize, ratio: i32) -> Option<String> {
        if !self.known(core) {
            return Some("процессор не сообщил потолок этого ядра".into());
        }
        let choices = self.choices(core);
        let (Some(&highest), Some(&lowest)) = (choices.first(), choices.last()) else {
            return Some("для этого ядра нет допустимых потолков".into());
        };
        // Проверка диапазона — до записи и единственная дорога к ней: в почтовый ящик идёт только байт из него.
        let Ok(byte) = u8::try_from(ratio) else {
            return Some(format!("потолок {ratio} вне допустимого: от ×{lowest} до ×{highest}"));
        };
        if !choices.contains(&ratio) {
            return Some(format!("потолок {ratio} вне допустимого: от ×{lowest} до ×{highest}"));
        }
        let lp = topology.cores[core].lp;
        let answered = match cpu.tell(core, Allowed::checked(byte)) {
            Ok(Some(answered)) => answered,
            Ok(None) => {
                return Some(format!(
                    "не удалось привязать поток к процессору {lp}: потолок не записан"
                ));
            }
            Err(e) => return Some(e.0),
        };
        let block = topology.block_of(core).members.clone(); // members of one block, a handful of indexes
        let reread = self.read_cores(cpu, topology, &block);
        // Ответа на запись не дождались: запрос мог остаться в ящике и выполниться позже. Сказать «не записан» было
        // бы неправдой (ревизия, п. 5).
        if answered == -1 && (reread.is_err() || self.silent[core]) {
            return Some(format!(
                "потолок {ratio} отправлен ядру {lp}, но процессор не ответил вовремя — проверь потолок блока"
            ));
        }
        if let Err(e) = reread {
            return Some(e.0);
        }
        if self.silent[core] {
            return Some(format!(
                "потолок {ratio} для ядра {lp} записан, но процессор не ответил, принял ли он его"
            ));
        }
        if self.current[core] == ratio {
            return None;
        }
        Some(format!(
            "процессор не принял потолок {ratio} для ядра {lp}: код ответа {answered}, действующий потолок остался {}",
            self.current[core]
        ))
    }
}
