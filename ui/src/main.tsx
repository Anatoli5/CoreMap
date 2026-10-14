// Страница нового окна. Разделы появляются по одному (DECISIONS #24, этап 2); сейчас — «Кристалл» и верхняя полоса.
import { render } from "preact";
import { closeMenu, Die } from "./die.js";
import { start } from "./engine.js";
import { Problem, TopBar } from "./top.js";

function App() {
  return (
    <>
      <TopBar />
      <Problem />
      <main>
        <Die />
      </main>
    </>
  );
}

// Меню закрывается щелчком мимо него и клавишей Esc.
addEventListener("pointerdown", e => {
  if (!(e.target as Element).closest(".menu")) closeMenu();
});
addEventListener("keydown", e => {
  if (e.key === "Escape") closeMenu();
});

render(<App />, document.getElementById("app")!);
start();
// Скрытая проверка: так страницу открывает окно, запущенное с --snapshot (src/WebWindow.cs).
if (new URLSearchParams(location.search).has("check")) import("./check.js").then(m => m.run());
