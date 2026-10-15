// Имитация процессора для проверок (как «процессор в памяти» CoreLimits в C# при --simulate): P-ядра ×53, E ×46,
// заводской предел ×54 у логических процессоров 12 и 23, у остальных равен потолку. Запись в ядро ставит потолок
// всему его блоку, как настоящий процессор у кластера E-ядер. Сюда же — то, что проверка делает с «процессором»
// сама: сброс к заводским (сон), чужая правка, глухие ядра.

use crate::HwError;
use crate::mailbox::{Allowed, Cpu, Limit};
use crate::topology::{Kind, Topology};

#[derive(Debug, Clone)]
pub struct SimCpu {
    fused: Vec<u8>,
    current: Vec<u8>,
    boot: Vec<u8>,           // что «процессор» держит после загрузки
    blocks: Vec<Vec<usize>>, // по ядру — ядра его блока
    deaf: Vec<bool>,
    /// Сколько записей потолка дошло до «процессора» — проверка видит, не писала ли программа лишнего.
    pub writes: usize,
    /// Сколько раз «процессор» спросили о потолке — проверка видит, не трогала ли программа его вовсе.
    pub reads: usize,
}

impl SimCpu {
    #[must_use]
    pub fn new(topology: &Topology) -> Self {
        let current: Vec<u8> = topology
            .cores
            .iter()
            .map(|c| if c.kind == Kind::P { 53 } else { 46 })
            .collect();
        let fused = topology
            .cores
            .iter()
            .zip(&current)
            .map(|(c, &now)| if c.lp.0 == 12 || c.lp.0 == 23 { 54 } else { now })
            .collect();
        let blocks = (0..topology.cores.len())
            .map(|i| topology.block_of(i).members.clone())
            .collect();
        Self {
            fused,
            boot: current.clone(), // the start state is also what a reboot returns to
            current,
            blocks,
            deaf: vec![false; topology.cores.len()],
            writes: 0,
            reads: 0,
        }
    }

    /// Сон или перезагрузка: потолки возвращаются к тем, что процессор держит после загрузки. Это не всегда
    /// заводской предел: у 275HX ядра 12 и 23 после загрузки на ×53 при пределе ×54 (замер 12.10, KNOWLEDGE.md).
    pub fn reset_to_factory(&mut self) {
        self.current.clone_from(&self.boot);
    }

    /// Чужая программа поставила блоку потолок (мимо проверок программы).
    pub fn foreign_write(&mut self, core: usize, ratio: u8) {
        for &m in &self.blocks[core] {
            self.current[m] = ratio;
        }
    }

    /// Ядро перестаёт (или снова начинает) отвечать о своём потолке.
    pub fn set_deaf(&mut self, core: usize, deaf: bool) {
        self.deaf[core] = deaf;
    }

    #[must_use]
    pub fn current(&self) -> &[u8] {
        &self.current
    }
}

impl Cpu for SimCpu {
    fn ask(&mut self, core: usize) -> Result<Option<Limit>, HwError> {
        self.reads += 1;
        Ok((!self.deaf[core]).then(|| Limit {
            fused: self.fused[core],
            current: self.current[core],
        }))
    }

    fn tell(&mut self, core: usize, ratio: Allowed) -> Result<Option<i32>, HwError> {
        self.writes += 1;
        for &m in &self.blocks[core] {
            self.current[m] = ratio.get();
        }
        Ok(Some(0))
    }
}
