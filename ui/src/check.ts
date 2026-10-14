// Скрытая проверка страницы. Только с ?check в адресе: так её открывает окно, запущенное с --snapshot
// (src/WebWindow.cs), против движка на имитации. Итог — строками окну, оно кладёт их в отчёт проверки движка.
//
// Что проверяется: страница нарисовала то, что прислал движок; на состояние, где ни одно число не изменилось, она не
// правит ничего; щелчок по плитке даёт нагрузку, правая кнопка открывает меню, пункт меню ставит потолок.
// Как страница выглядит, проверка не судит — для этого картинки, их смотрит агент.
import { effect, signal } from "@preact/signals";
import { ask, cores, hello, limits, top, watch } from "./engine.js";

const host = window.chrome.webview;
const lines: string[] = [];

function check(ok: boolean, what: string): boolean {
  lines.push((ok ? "ok      " : "FAILED  ") + what);
  return ok;
}

/** Ждёт условия по изменениям сигналов, не по таймеру; false — не дождался за seconds. */
function until(condition: () => boolean, seconds: number): Promise<boolean> {
  return new Promise(resolve => {
    let done = false;
    let dispose: (() => void) | null = null;
    const finish = (ok: boolean) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      queueMicrotask(() => dispose && dispose());
      resolve(ok);
    };
    const timer = setTimeout(() => finish(false), seconds * 1000);
    dispose = effect(() => {
      if (condition()) finish(true);
    });
  });
}

/** Два кадра: всё, что изменено, нарисовано. */
function frames(): Promise<void> {
  return new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
}

/** Просит окно снять картинку страницы и ждёт, пока снимет. */
async function shot(name: string): Promise<boolean> {
  await frames();
  return new Promise(resolve => {
    const timer = setTimeout(() => {
      watch.shot = null;
      resolve(false);
    }, 10000);
    watch.shot = taken => {
      if (taken !== name) return;
      clearTimeout(timer);
      watch.shot = null;
      resolve(true);
    };
    host.postMessage("shot:" + name);
  });
}

function tile(block: number): HTMLElement {
  return document.querySelector(`[data-block="${block}"]`) as HTMLElement;
}

export async function run() {
  try {
    await steps();
  } catch (e) {
    check(false, "проверка страницы упала: " + String(e));
  }
  host.postMessage("done:" + lines.join("\n"));
}

async function steps() {
  if (!check(await until(() => hello.value !== null && limits.value !== null && top.pkg.value !== null, 15),
    "знакомство, потолки и первое состояние пришли")) return;
  const h = hello.value!, L = limits.value!;
  check(document.querySelectorAll(".tile").length === h.layout.tiles.length,
    `плиток ${document.querySelectorAll(".tile").length}, в раскладке ${h.layout.tiles.length}`);
  check(await shot("main"), "картинка страницы");

  // 1. Правки страницы на каждое состояние движка: счёт по наблюдателю за всем документом.
  const observer = new MutationObserver(() => {});
  observer.observe(document.body, { subtree: true, childList: true, attributes: true, characterData: true });
  const samples: { changed: number; writes: number }[] = [];
  let pending = -1;
  watch.before = () => {
    const writes = observer.takeRecords().length;
    if (pending >= 0) samples.push({ changed: pending, writes });
  };
  const seen = signal(0);
  watch.after = changed => {
    pending = changed;
    seen.value++;
  };
  await until(() => seen.value > 8, 10);
  watch.before = watch.after = null;
  observer.disconnect();
  const sum = (list: typeof samples, key: "changed" | "writes") => list.reduce((s, x) => s + x[key], 0);
  const quiet = samples.filter(s => s.changed === 0);
  check(samples.length >= 8, `состояний подряд без действий: ${samples.length}`);
  check(quiet.every(s => s.writes === 0), `из них без изменившихся чисел ${quiet.length}, правок страницы на них ${sum(quiet, "writes")} (должно быть 0)`);
  lines.push(`        за эти состояния изменилось чисел ${sum(samples, "changed")}, правок страницы ${sum(samples, "writes")}, ` +
    `больше всего за одно состояние ${Math.max(0, ...samples.map(s => s.writes))}`);

  // 2. Щелчок по плитке P-блока — нагрузка; второй щелчок её снимает.
  const p = h.blocks.find(b => b.kind === "P") ?? h.blocks[0];
  tile(p.index).click();
  check(await until(() => cores.value[p.first].load.value, 5), `щелчок по плитке ${p.name} дал нагрузку`);
  await frames();
  check(tile(p.index).classList.contains("loaded"), "плитка под нагрузкой обведена");
  check(await shot("load"), "картинка с нагрузкой");
  tile(p.index).click();
  check(await until(() => !cores.value[p.first].load.value, 5), "второй щелчок снял нагрузку");

  // 3. Правая кнопка по плитке E-блока — меню; пункт на ступень ниже заводского ставит потолок.
  const e = h.blocks.find(b => b.kind === "E") ?? p;
  const box = tile(e.index).getBoundingClientRect();
  tile(e.index).dispatchEvent(new MouseEvent("contextmenu",
    { bubbles: true, cancelable: true, button: 2, clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 }));
  await frames();
  const items = [...document.querySelectorAll<HTMLButtonElement>(".menu .item")];
  check(items.length === L.choices[e.index].length, `меню потолка ${e.name}: пунктов ${items.length}, у движка ${L.choices[e.index].length}`);
  check(await shot("menu"), "картинка с меню");
  const before = cores.value[e.first].limit.value;
  const target = L.fused[e.first] - 1;
  const item = items.find(i => i.dataset.ratio === String(target));
  if (!check(item !== undefined && before !== null, `в меню есть ×${target}, потолок сейчас ×${before}`)) return;
  item!.click();
  check(await until(() => cores.value[e.first].limit.value === target, 5), `пункт ×${target} поставил потолок`);
  await frames();
  check(document.querySelector(".menu") === null, "меню закрылось");
  const delta = target - L.was[e.first];
  const shown = tile(e.index).querySelector(".delta")?.textContent ?? "";
  check(delta === 0 ? shown === "" : shown === (delta > 0 ? "+" : "−") + Math.abs(delta), `на плитке разница с «было»: «${shown}»`);
  const back = await ask("limit.set", { block: e.index, ratio: before });
  check(back.ok && await until(() => cores.value[e.first].limit.value === before, 5), `потолок возвращён на ×${before}`);
}
