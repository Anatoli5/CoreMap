// Память срезов автомодуля (DECISIONS #40, подзапись «две отсечки»; прежде — память перегрева с запретами по времени,
// #32): чтобы не было качелей. Срез записывает блок, потолок, с которого он срезан, и уровень кристалла тогда (среднее
// 2-с медиан остальных блоков). Вернуть блок на этот потолок можно, только пока нынешний уровень кристалла ниже
// записанного: при том же или более горячем кристалле этот потолок уже перегрел блок. Это заменяет прежние запреты на
// 30 минут с удвоением до суток — их числа были выбором агента без замера (DECISIONS #40, поправка).
//
// Новый срез с того же потолка заменяет прежний: последний — самый свежий довод. Срок у записи не выходит: кристалл
// остынет — запись пускает сама.
//
// Запись действует, только пока температура, по которой блок срезан, выше нынешнего целевого предела (то же правило
// было у запретов): предел подняли выше той температуры — прежний срез уже не перегрев.
//
// Своего файла у памяти нет: программа входа восстанавливает её из журнала действий (data\auto-log.jsonl), повторяя
// записанные срезы по порядку, — у факта один владелец. Срезы прежних сборок (строки «lower») уровня кристалла не
// знают: такая запись пускает возврат при любом уровне.

use std::collections::HashMap;

/// Срез блока с потолка `from`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub from: i32,
    /// Температура, по которой срезан (показание или 2-с медиана), °C.
    pub temp: f64,
    /// Уровень кристалла при срезе, °C; None — не известен (строка прежней сборки).
    pub die: Option<f64>,
}

/// Срезы по блокам (блок — по номеру своего первого ядра: он не зависит от порядка блоков) и потолкам.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    cuts: HashMap<(usize, i32), Cut>,
}

impl Memory {
    /// Блок (первое ядро `core`) срезан с потолка `from` при температуре `temp` и уровне кристалла `die`.
    pub fn cut(&mut self, core: usize, from: i32, temp: f64, die: Option<f64>) {
        self.cuts.insert((core, from), Cut { from, temp, die });
    }

    /// Можно ли вернуть блок на потолок `to` при целевом пределе `limit` и нынешнем уровне кристалла `die`.
    #[must_use]
    pub fn allows(&self, core: usize, to: i32, limit: f64, die: Option<f64>) -> bool {
        let Some(cut) = self.cuts.get(&(core, to)) else {
            return true;
        };
        if cut.temp <= limit {
            return true; // срезан ниже нынешнего предела — уже не перегрев
        }
        match (cut.die, die) {
            (None, _) => true,
            // Нынешний уровень не известен — что кристалл остыл, не доказано.
            (Some(_), None) => false,
            (Some(then), Some(now)) => now < then,
        }
    }

    /// Сколько срезов действует при этом пределе (скрытая проверка: из журнала действий восстанавливаются те же).
    #[must_use]
    pub fn holding(&self, limit: f64) -> usize {
        self.cuts.values().filter(|cut| cut.temp > limit).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_return_to_the_cut_ceiling_waits_for_a_cooler_die() {
        let mut m = Memory::default();
        m.cut(1, 50, 88.0, Some(60.0));
        assert!(!m.allows(1, 50, 85.0, Some(60.0)), "the same die: no");
        assert!(!m.allows(1, 50, 85.0, Some(63.0)), "hotter: no");
        assert!(m.allows(1, 50, 85.0, Some(59.9)), "cooler: yes");
        assert!(!m.allows(1, 50, 85.0, None), "die not known: not proven cooler");
        assert!(m.allows(1, 49, 85.0, Some(70.0)), "another ceiling: no record");
        assert!(m.allows(2, 50, 85.0, Some(70.0)), "another block");
    }

    #[test]
    fn the_latest_cut_from_a_ceiling_replaces_the_one_before() {
        let mut m = Memory::default();
        m.cut(1, 50, 88.0, Some(60.0));
        m.cut(1, 50, 90.0, Some(52.0));
        assert!(!m.allows(1, 50, 85.0, Some(55.0)), "now it overheated at a die of 52");
        assert!(m.allows(1, 50, 85.0, Some(51.0)));
        assert_eq!(m.holding(85.0), 1);
    }

    #[test]
    fn an_old_cut_without_a_die_level_allows_any_return() {
        let mut m = Memory::default();
        m.cut(1, 50, 88.0, None);
        assert!(m.allows(1, 50, 85.0, Some(99.0)));
        assert!(m.allows(1, 50, 85.0, None));
    }

    #[test]
    fn a_cut_below_a_raised_limit_no_longer_holds() {
        // Cut at 88 °C under an 85 °C target; the owner moved the target to 90 - 88 °C is no overheat any more.
        let mut m = Memory::default();
        m.cut(1, 50, 88.0, Some(60.0));
        assert!(!m.allows(1, 50, 85.0, Some(60.0)));
        assert!(m.allows(1, 50, 90.0, Some(60.0)));
        assert_eq!((m.holding(85.0), m.holding(90.0)), (1, 0));
    }
}
