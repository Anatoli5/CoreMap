// Временная страница-пробник (13.10.2026): проверяет цепочку «движок → канал → окно → страница» и больше ничего.
// Схема кристалла здесь — плитки по раскладке из hello с температурой и множителем. Настоящая страница придёт на
// её место, когда будет решено, на чём она пишется (DECISIONS #27); правила разговора с окном — в src/WebWindow.cs.
const host = window.chrome.webview;
const $ = id => document.getElementById(id);
let tiles = [];   // по номеру блока: { first, temp, ratio } — первое ядро блока (ряды состояния идут по ядрам) и узлы текста
let drawn = false;

function text(node, value) {
  if (node.textContent !== value) node.textContent = value;   // пишется только изменившееся
}

function hello(m) {
  text($("cpu"), m.cpu + (m.simulated ? " · имитация" : ""));
  const die = $("die");
  die.style.gridTemplateColumns = `repeat(${m.layout.cols}, 1fr)`;
  die.style.gridTemplateRows = `repeat(${m.layout.rows}, 1fr)`;
  die.replaceChildren();
  tiles = [];
  for (const t of m.layout.tiles) {
    const tile = document.createElement("div"), name = document.createElement("span"), temp = document.createElement("b"), ratio = document.createElement("span");
    tile.className = "tile";
    tile.style.gridColumn = t.col + 1;
    tile.style.gridRow = t.row + 1;
    name.textContent = m.blocks[t.block].name;
    tile.append(name, temp, ratio);
    die.append(tile);
    tiles[t.block] = { first: m.blocks[t.block].first, temp, ratio };
  }
}

function state(m) {
  text($("pkg"), `${m.pkg} °C · ${m.watts} Вт`);
  for (const tile of tiles) {
    text(tile.temp, m.cur[tile.first] + "°");
    text(tile.ratio, "×" + m.ratio[tile.first] + (m.limit ? " / ×" + m.limit[tile.first] : ""));
  }
  if (!drawn && tiles.length) {
    drawn = true;
    requestAnimationFrame(() => requestAnimationFrame(() => host.postMessage("drawn")));   // кадр с числами показан
  }
}

host.addEventListener("message", e => {
  const m = e.data;
  if (m.t === "link") {
    $("link").className = m.up ? "" : "down";
    text($("link"), m.up ? "движок на связи" : m.why);
    if (m.up) host.postMessage({ id: 0, c: "hello", proto: 1 });
  }
  else if (m.t === "hello") hello(m);
  else if (m.t === "state") state(m);
});
host.postMessage("ready");
