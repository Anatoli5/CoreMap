//! Железо и правила защиты `CoreMap` (DECISIONS #30, #31): доступ к процессору через драйвер `PawnIO`, разгонный
//! почтовый ящик (только чтение и запись потолка множителя), потолки ядер с допустимым диапазоном, топология,
//! файлы профилей в том же виде, что пишет C#-часть, правила постановки профиля при запуске, имитация процессора.
//!
//! Библиотеку берёт программа входа в Windows (`tray`, `CoreMapTray.exe`); движок настройки возьмёт её при своём
//! переносе на Rust. Окна здесь нет, потоков своих нет: всё вызывается из потока того, кто взял библиотеку.

pub mod auto;
pub mod clock;
pub mod events;
pub mod files;
pub mod limits;
pub mod mailbox;
pub mod marker;
pub mod memory;
pub mod named;
pub mod pawnio;
pub mod profile;
pub mod sim;
pub mod thermal;
pub mod topology;

mod affinity;

use std::fmt;

/// Отказ железа или драйвера — текстом для владельца (по-русски, как `SensorException` в C#-части).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwError(pub String);

impl fmt::Display for HwError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HwError {}

/// Номер логического процессора Windows. Отдельный тип, чтобы его не спутать с номером ядра в списке
/// (позицией в `Topology::cores`) и с номером блока.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Lp(pub u32);

impl fmt::Display for Lp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
