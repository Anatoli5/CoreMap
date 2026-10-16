// Обход ошибки Slint 1.18.1: окно после сворачивания остаётся пустым и прозрачным (владелец 16.10 повторил: свернул
// окно, щёлкнул по значку CoreMapTray — окно «не вернулось», при перетаскивании видны куски без фона).
//
// Причина: свёрнутому окну Windows не хранит содержимое, а рисование процессором (software renderer) по возрасту
// буфера считает прежний кадр на месте — первый кадр после восстановления рисует только изменившийся участок, остальное
// пусто. Исправлено в Slint: github.com/slint-ui/slint/pull/13566 («winit: repaint the whole window after restoring it
// from minimized») и github.com/slint-ui/slint/pull/13568 («winit: repaint what Windows invalidated», в master с
// 28.09.2026); в выпущенной 1.18.1 (21.09.2026, последняя на crates.io) исправления нет, зависимость от git не берём.
//
// Обход: события окна winit смотрим до Slint (`on_winit_window_event`, всё пропускаем дальше). Окно восстановлено —
// размер был 0×0 (свёрнуто) и стал ненулевым, или winit сказал «окно снова видно» (`Occluded(false)`): тогда меняется
// прозрачный слой во всё окно (ui/app.slint, `veil`), изменившимся считается всё окно, и оно рисуется целиком.
// Убрать этот файл, слой и свойство `repaint-epoch`, когда Slint выйдет с #13568.

use slint::ComponentHandle;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};

use crate::AppWindow;

/// Что из событий окна нужно обходу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Новый размер окна, пиксели; 0×0 — окно свёрнуто.
    Resized(u32, u32),
    /// Окно перестало быть закрытым или свёрнутым (`Occluded(false)`).
    Visible,
    Other,
}

/// Было ли окно свёрнуто: только переход «свёрнуто → нет» требует перерисовать всё.
#[derive(Debug, Default)]
pub struct Restore {
    minimized: bool,
}

impl Restore {
    /// Событие окна; true — окно восстановлено, пора перерисовать его целиком.
    pub fn seen(&mut self, event: Seen) -> bool {
        match event {
            Seen::Resized(width, height) => {
                let minimized = width == 0 || height == 0;
                let restored = self.minimized && !minimized;
                self.minimized = minimized;
                restored
            }
            Seen::Visible => true,
            Seen::Other => false,
        }
    }

    /// Событие окна и, если окно восстановлено, полная перерисовка. Тот же путь — у настоящего окна (`watch`) и у
    /// скрытой проверки, которой настоящего сворачивания не устроить (src/check.rs).
    pub fn on(&mut self, ui: &AppWindow, event: Seen) {
        if self.seen(event) {
            repaint_all(ui);
        }
    }
}

/// Прозрачный слой во всё окно меняется: следующий кадр рисует окно целиком.
pub fn repaint_all(ui: &AppWindow) {
    ui.set_repaint_epoch(ui.get_repaint_epoch().wrapping_add(1));
}

/// Настоящее окно: события winit — в `Restore`, все — дальше в Slint. Окно не на winit (скрытая проверка) — ничего.
pub fn watch(ui: &AppWindow) {
    let weak = ui.as_weak();
    let mut restore = Restore::default();
    ui.window().on_winit_window_event(move |_, event| {
        let seen = match event {
            WindowEvent::Resized(size) => Seen::Resized(size.width, size.height),
            WindowEvent::Occluded(false) => Seen::Visible,
            _ => Seen::Other,
        };
        if let Some(ui) = weak.upgrade() {
            restore.on(&ui, seen);
        }
        EventResult::Propagate
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Перерисовать всё — только когда окно вернулось из свёрнутого или снова видно; обычная смена размера и
    // сворачивание — нет.
    #[test]
    fn only_coming_back_from_minimized_repaints_everything() {
        let mut r = Restore::default();
        assert!(!r.seen(Seen::Resized(1500, 900)), "an ordinary resize");
        assert!(!r.seen(Seen::Resized(0, 0)), "minimizing");
        assert!(!r.seen(Seen::Resized(0, 0)), "still minimized");
        assert!(r.seen(Seen::Resized(1500, 900)), "restored");
        assert!(!r.seen(Seen::Resized(1400, 860)), "a resize after the restore");
        assert!(!r.seen(Seen::Resized(1400, 0)), "a zero side is minimized too");
        assert!(r.seen(Seen::Resized(1400, 860)));
        assert!(r.seen(Seen::Visible), "visible again");
        assert!(!r.seen(Seen::Other));
    }
}
