// Раздел «Кристалл»: плитка на блок. Щелчок — нагрузка на блок, правая кнопка — меню потолка множителя.
// Как у прежнего окна (src/MainForm.Die.cs, DieView в src/Ui.cs): слева вверху номера ядер, справа вверху средняя под
// нагрузкой и максимум, слева внизу температура, справа внизу множитель / потолок и разница с «было».
import { effect, signal, useComputed } from "@preact/signals";
import { useLayoutEffect, useRef } from "preact/hooks";
import { cores, generation, hello, limits, order, top, type CoreValues } from "./engine.js";
import { highest, signed, tileColor } from "./format.js";
import type { BlockInfo, CoreInfo, Tile } from "./wire.js";

/** Выбранный блок: его плитка обведена (позже — его кривая поверх остальных). */
export const selected = signal(-1);

// Пока владелец ничего не выбрал, выбран первый P-блок — как в прежнем окне.
effect(() => {
  const h = hello.value;
  if (h && selected.peek() < 0) selected.value = (h.blocks.find(b => b.kind === "P") ?? h.blocks[0]).index;
});

const menu = signal<{ block: BlockInfo; x: number; y: number } | null>(null);

export function closeMenu() {
  menu.value = null;
}

export function Die() {
  const h = hello.value;
  if (!h) return <section class="panel die-panel"><h2>Кристалл</h2></section>;
  const L = h.layout, gen = generation.value;
  // Пропорции схемы — как у плиток прежнего окна: 150 × 96 с зазором 8.
  const ratio = (L.cols * 158 - 8) / (L.rows * 104 - 8);
  const noted = !L.confirmed && !!L.note;
  return (
    <section class={noted ? "panel die-panel with-note" : "panel die-panel"}>
      <h2>Кристалл</h2>
      <div class="die" style={{ gridTemplateColumns: `repeat(${L.cols}, 1fr)`, gridTemplateRows: `repeat(${L.rows}, 1fr)`, "--ratio": ratio.toFixed(4) }}>
        {L.tiles.map(t => <TileView key={`${gen}.${t.block}`} tile={t} block={h.blocks[t.block]} info={h.cores} />)}
      </div>
      {noted && <p class="note">{L.note}</p>}
      <LimitMenu />
    </section>
  );
}

function TileView({ tile, block, info }: { tile: Tile; block: BlockInfo; info: CoreInfo[] }) {
  const all = cores.peek();
  const members = block.members.map(i => all[i]);
  const first = all[block.first];

  const temp = useComputed(() => highest(members.map(c => c.cur.value)));
  const style = useComputed(() => `grid-column:${tile.col + 1};grid-row:${tile.row + 1};background:${tileColor(temp.value)}`);
  const tempText = useComputed(() => (temp.value === null ? "–" : temp.value + "°"));
  const stats = useComputed(() => {
    const peak = highest(members.map(c => c.max.value)), mean = highest(members.map(c => c.avg.value));
    return [mean === null ? "" : "⌀ " + Math.round(mean), peak === null ? "" : "▲ " + peak].filter(s => s).join("  ");
  });
  const cls = useComputed(() =>
    "tile" + (members.some(c => c.load.value) ? " loaded" : "") + (selected.value === block.index ? " selected" : ""));
  const pair = useComputed(() => {
    const limit = first.limit.value, ratio = highest(members.map(c => c.ratio.value));
    if (limit === null || limit <= 0) return "";
    return (ratio === null ? "" : ratio.toFixed(1) + " / ") + "×" + limit;
  });
  // Потолок ниже заводского — жёлтым, выше — красным.
  const pairCls = useComputed(() => {
    const limit = first.limit.value, L = limits.value;
    if (limit === null || !L) return "pair";
    const fused = L.fused[block.first];
    return limit < fused ? "pair below" : limit > fused ? "pair above" : "pair";
  });
  const delta = useComputed(() => {
    const limit = first.limit.value, L = limits.value;
    if (limit === null || !L || L.was[block.first] <= 0 || limit === L.was[block.first]) return "";
    return signed(limit - L.was[block.first]);
  });

  return (
    <div class={cls} style={style} data-block={block.index}
      onClick={() => {
        selected.value = block.index;
        order("load.toggle", { block: block.index }, "Нагрузка не изменена");
      }}
      onContextMenu={e => {
        e.preventDefault();
        selected.value = block.index;
        menu.value = { block, x: e.clientX, y: e.clientY };
      }}>
      <div class={block.members.length > 1 ? "nums cluster" : "nums"}>
        {tile.places.map(i => <Num lp={info[i].lp} kind={info[i].kind} core={all[i]} />)}
      </div>
      <div class="stats">{stats}</div>
      <div class="temp">{tempText}</div>
      <div class="corner"><span class={pairCls}>{pair}</span><span class="delta">{delta}</span></div>
    </div>
  );
}

/** Номер ядра: цвет по роду ядра, красный — ядро сбрасывает частоту из-за перегрева. */
function Num({ lp, kind, core }: { lp: number; kind: string; core: CoreValues }) {
  const cls = useComputed(() => (core.thr.value ? "num hot" : "num " + kind));
  return <span class={cls}>{lp}</span>;
}

/** Меню потолка: от нескольких ступеней выше заводского предела вниз; что предлагать, решает движок (choices). */
function LimitMenu() {
  const m = menu.value, L = limits.value;
  const box = useRef<HTMLDivElement>(null);
  // Меню у нижнего или правого края сдвигается внутрь окна.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    if (r.bottom > innerHeight) el.style.top = Math.max(0, innerHeight - r.height) + "px";
    if (r.right > innerWidth) el.style.left = Math.max(0, innerWidth - r.width) + "px";
  });
  if (!m || !L) return null;
  const i = m.block.first, current = cores.value[i].limit.value, busy = top.busy.value;
  return (
    <div class="menu" ref={box} style={{ left: m.x + "px", top: m.y + "px" }} onClick={e => e.stopPropagation()}>
      <div class="head">Потолок множителя: {m.block.kind === "E" ? "кластер ядра " : "ядро "}{m.block.lp}</div>
      {L.choices[m.block.index].map(v => (
        <button class={v === current ? "item checked" : "item"} disabled={busy} data-ratio={v}
          onClick={() => {
            closeMenu();
            order("limit.set", { block: m.block.index, ratio: v }, "Потолок не изменён");
          }}>
          <span class="mark">{v === current ? "✓" : ""}</span>×{v}
          <span class="hint">{v === L.fused[i] ? "заводской предел" : v > L.fused[i] ? "выше заводского" : ""}{v === L.was[i] && v !== L.fused[i] ? " было" : ""}</span>
        </button>
      ))}
    </div>
  );
}
