// Топология процессора из Windows и блоки (как Topology.cs и Block.cs).
//
// Блок — единица, у которой общий потолок множителя: кластер E-ядер с общим L2 целиком либо одно физическое
// ядро. Потолок блока читается и пишется через его первое ядро; процессор сам распространяет запись на кластер.
// В отличие от C#-части, логические процессоры одного физического ядра (гиперпоточность) попадают в один блок —
// иначе на процессоре с ними один потолок писался бы дважды (ошибка B15 описи); на 275HX гиперпоточности нет,
// блоков те же 12. В файлах профилей потолок по-прежнему записан на каждый логический процессор (`lps`).
#![expect(
    unsafe_code,
    reason = "GetLogicalProcessorInformationEx and the registry: Win32 calls"
)]

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows_sys::Win32::System::SystemInformation::{GetLogicalProcessorInformationEx, RelationAll};

use crate::{HwError, Lp};

/// Вид ядра: производительное или энергоэффективное.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    P,
    E,
}

/// Логический процессор.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Core {
    pub lp: Lp,
    /// Номер физического ядра в порядке Windows.
    pub physical: usize,
    pub kind: Kind,
    /// Кластер с общим L2 (только у E-ядер, когда в нём больше одного физического ядра).
    pub cluster: Option<usize>,
}

/// Блок: ядра (позиции в списке ядер, по возрастанию), первое из них, имя «0» или «2–5».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub members: Vec<usize>,
    pub first: usize,
    pub lp: Lp,
    pub kind: Kind,
    pub name: String,
}

/// Ядра по возрастанию номера логического процессора и блоки по порядку первых ядер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub cores: Vec<Core>,
    pub blocks: Vec<Block>,
    by_core: Vec<usize>,
}

const RELATION_CORE: u32 = 0;
const RELATION_CACHE: u32 = 2;

impl Topology {
    /// Топология этой машины.
    pub fn read() -> Result<Self, HwError> {
        let mut size = 0u32;
        // SAFETY: the first call only asks for the needed size; a null buffer with size 0 is allowed.
        unsafe { GetLogicalProcessorInformationEx(RelationAll, null_mut(), &raw mut size) };
        // u64 elements keep the buffer 8-byte aligned, as the records require.
        let mut buffer = vec![0u64; usize::try_from(size).unwrap_or(0).div_ceil(8)];
        // SAFETY: `buffer` is writable for at least `size` bytes and outlives the call.
        let ok = unsafe { GetLogicalProcessorInformationEx(RelationAll, buffer.as_mut_ptr().cast(), &raw mut size) };
        if ok == 0 {
            return Err(HwError("Windows не отдала топологию процессора".into()));
        }
        let bytes: Vec<u8> = buffer.iter().flat_map(|w| w.to_le_bytes()).collect();
        Self::parse(&bytes[..usize::try_from(size).unwrap_or(0).min(bytes.len())])
    }

    /// Топология из ответа GetLogicalProcessorInformationEx(RelationAll) — для проверок на чужой раскладке.
    /// Смещения полей — из описания структур Windows, те же, что в Topology.cs.
    pub fn parse(bytes: &[u8]) -> Result<Self, HwError> {
        let bad = || HwError("топология процессора от Windows не разобрана".into());
        let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |at: usize| {
            bytes
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let u64_at = |at: usize| {
            bytes
                .get(at..at + 8)
                .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        };
        let mut physical: Vec<(u8, u64)> = Vec::new(); // (класс эффективности, маска процессоров ядра)
        let mut l2: Vec<u64> = Vec::new();
        let mut at = 0usize;
        while at < bytes.len() {
            let relation = u32_at(at).ok_or_else(bad)?;
            let size = u32_at(at + 4).and_then(|s| usize::try_from(s).ok()).ok_or_else(bad)?;
            if size == 0 {
                return Err(bad());
            }
            if relation == RELATION_CORE {
                if u16_at(at + 40).ok_or_else(bad)? != 0 {
                    return Err(HwError(
                        "больше 64 логических процессоров пока не поддерживается".into(),
                    ));
                }
                physical.push((*bytes.get(at + 9).ok_or_else(bad)?, u64_at(at + 32).ok_or_else(bad)?));
            } else if relation == RELATION_CACHE
                && *bytes.get(at + 8).ok_or_else(bad)? == 2
                && u16_at(at + 48).ok_or_else(bad)? == 0
            {
                l2.push(u64_at(at + 40).ok_or_else(bad)?);
            }
            at += size;
        }
        Self::build(&physical, &l2)
    }

    fn build(physical: &[(u8, u64)], l2: &[u64]) -> Result<Self, HwError> {
        let top = physical
            .iter()
            .map(|&(class, _)| class)
            .max()
            .ok_or_else(|| HwError("у процессора нет ядер".into()))?;
        let mut cores: Vec<Core> = Vec::new();
        for (index, &(class, mask)) in physical.iter().enumerate() {
            for lp in bits(mask) {
                cores.push(Core {
                    lp: Lp(lp),
                    physical: index,
                    kind: if class == top { Kind::P } else { Kind::E },
                    cluster: None,
                });
            }
        }
        cores.sort_by_key(|c| c.lp);
        // Кластер — L2, общий для двух и больше физических ядер; номера — по первому процессору группы.
        let mut groups: Vec<u64> = l2.to_vec();
        groups.sort_by_key(|&mask| mask.trailing_zeros());
        let mut clusters = 0usize;
        for mask in groups {
            let members: Vec<usize> = (0..cores.len()).filter(|&i| mask >> cores[i].lp.0 & 1 == 1).collect();
            let mut distinct: Vec<usize> = members.iter().map(|&i| cores[i].physical).collect();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() < 2 {
                continue;
            }
            for &i in &members {
                cores[i].cluster = Some(clusters);
            }
            clusters += 1;
        }
        Ok(Self::with_blocks(cores))
    }

    /// Блоки: E-ядра одного кластера — один блок; иначе одно физическое ядро — один блок.
    fn with_blocks(cores: Vec<Core>) -> Self {
        let mut keys: Vec<(bool, usize)> = Vec::new(); // (кластер?, номер кластера или физического ядра)
        let mut members: Vec<Vec<usize>> = Vec::new();
        let mut by_core = vec![0usize; cores.len()];
        for (i, core) in cores.iter().enumerate() {
            let key = match (core.kind, core.cluster) {
                (Kind::E, Some(cluster)) => (true, cluster),
                _ => (false, core.physical),
            };
            let block = if let Some(found) = keys.iter().position(|&k| k == key) {
                found
            } else {
                keys.push(key);
                members.push(Vec::new());
                keys.len() - 1
            };
            members[block].push(i);
            by_core[i] = block;
        }
        let blocks = members
            .into_iter()
            .map(|members| {
                let first = members[0];
                let last = members[members.len() - 1];
                let name = if members.len() == 1 {
                    cores[first].lp.to_string()
                } else {
                    format!("{}–{}", cores[first].lp, cores[last].lp)
                };
                Block {
                    first,
                    lp: cores[first].lp,
                    kind: cores[first].kind,
                    name,
                    members,
                }
            })
            .collect();
        Self { cores, blocks, by_core }
    }

    /// Номера логических процессоров в порядке списка ядер — поле `lps` файлов профилей.
    #[must_use]
    pub fn lps(&self) -> Vec<Lp> {
        self.cores.iter().map(|c| c.lp).collect()
    }

    /// Блок, в который входит ядро (позиция в списке ядер).
    #[must_use]
    pub fn block_of(&self, core: usize) -> &Block {
        &self.blocks[self.by_core[core]]
    }
}

fn bits(mask: u64) -> impl Iterator<Item = u32> {
    (0..64u32).filter(move |&i| mask >> i & 1 == 1)
}

/// Название процессора из реестра (`ProcessorNameString`, без пробелов по краям) — поле `cpu` файлов профилей.
/// Должно совпадать с тем, что пишет C#-часть, иначе файл профиля молча считается чужим.
#[must_use]
pub fn cpu_name() -> String {
    let key: Vec<u16> = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0"
        .encode_utf16()
        .chain([0])
        .collect();
    let value: Vec<u16> = "ProcessorNameString".encode_utf16().chain([0]).collect();
    let mut buffer = [0u16; 256];
    let mut bytes = u32::try_from(size_of_val(&buffer)).unwrap_or(u32::MAX);
    // SAFETY: names are NUL-terminated and outlive the call; `buffer` is writable for `bytes` bytes; RRF_RT_REG_SZ
    // makes the result NUL-terminated.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &raw mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return "процессор".into();
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..len]).to_string_lossy().trim().to_string()
}
