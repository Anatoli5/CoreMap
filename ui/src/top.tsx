// Верхняя полоса: процессор, температура корпуса, мощность, обороты вентиляторов, связь с движком.
// И строка под ней — почему движок не выполнил последнюю команду.
import { computed } from "@preact/signals";
import { hello, link, problem, refused, top } from "./engine.js";

const cpu = computed(() => {
  const h = hello.value;
  return h ? h.cpu + (h.simulated ? " · имитация" : "") : "";
});
const pkg = computed(() => (top.pkg.value === null ? "–" : top.pkg.value + " °C"));
const watts = computed(() => (top.watts.value === null ? "– Вт" : top.watts.value.toFixed(1) + " Вт"));
const fans = computed(() => {
  const a = top.fan0.value, b = top.fan1.value;
  return a === null && b === null ? "" : "вент. " + [a, b].map(v => (v === null ? "–" : v)).join(" / ") + " об/мин";
});
const linkText = computed(() => (link.value.up ? (refused.value ?? "") : (refused.value ?? link.value.why ?? "нет связи с движком")));
const linkCls = computed(() => (link.value.up && !refused.value ? "link" : "link down"));

export function TopBar() {
  return (
    <header class="top">
      <span class="cpu">{cpu}</span>
      <span title="Температура корпуса процессора">{pkg}</span>
      <span>{watts}</span>
      <span>{fans}</span>
      <span class={linkCls}>{linkText}</span>
    </header>
  );
}

export function Problem() {
  const text = problem.value ?? top.sensorError.value ?? top.trip.value;
  return text ? <div class="problem" onClick={() => (problem.value = null)}>{text}</div> : null;
}
