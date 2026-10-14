// Связь страницы с движком через окно (src/WebWindow.cs) и всё, что страница о движке знает.
//
// Каждое число ядра — свой сигнал. Пришло состояние — сигналам присваиваются новые числа; сигнал, чьё число не
// изменилось, никого не будит, а изменившийся меняет на странице ровно то место, что его показывает. Так страница
// меняет только изменившееся и только по сообщению движка — своих таймеров у неё нет (AGENTS.md, «rendering
// without redundant work»).
import { batch, signal, type Signal } from "@preact/signals";
import { Protocol, type Hello, type Limits, type Message, type Reply, type Run, type State } from "./wire.js";

const host = window.chrome.webview;

export interface CoreValues {
  cur: Signal<number | null>;
  max: Signal<number | null>;
  ratio: Signal<number | null>;
  avg: Signal<number | null>;
  thr: Signal<boolean>;
  load: Signal<boolean>;
  limit: Signal<number | null>;
}

export const link = signal<{ up: boolean; why: string | null }>({ up: false, why: "окно запускается" });
export const refused = signal<string | null>(null);   // отказ движка в знакомстве
export const problem = signal<string | null>(null);   // отказ на последнюю команду владельца
export const hello = signal<Hello | null>(null);
export const limits = signal<Limits | null>(null);
export const cores = signal<CoreValues[]>([]);
export const generation = signal(0);                  // растёт с каждым знакомством: плитки строятся заново

export const top = {
  pkg: signal<number | null>(null),
  watts: signal<number | null>(null),
  busy: signal(false),
  fan0: signal<number | null>(null),
  fan1: signal<number | null>(null),
  run: signal<Run | null>(null),
  trip: signal<string | null>(null),
  sensorError: signal<string | null>(null),
};

/** Скрытой проверке (check.ts): сколько сигналов изменило последнее состояние; что делать до и после него. */
export const watch = {
  changed: 0,
  before: null as null | (() => void),
  after: null as null | ((changed: number) => void),
  shot: null as null | ((name: string) => void),
};

function put<T>(target: Signal<T>, value: T) {
  if (Object.is(target.peek(), value)) return;
  target.value = value;
  watch.changed++;
}

function greet(m: Hello) {
  batch(() => {
    cores.value = m.cores.map(() => ({
      cur: signal(null), max: signal(null), ratio: signal(null), avg: signal(null),
      thr: signal(false), load: signal(false), limit: signal(null),
    }));
    hello.value = m;
    generation.value++;
  });
}

function apply(m: State) {
  if (watch.before) watch.before();
  watch.changed = 0;
  batch(() => {
    cores.peek().forEach((c, i) => {
      put(c.cur, m.cur[i] ?? null);
      put(c.max, m.max[i] ?? null);
      put(c.ratio, m.ratio[i] ?? null);
      put(c.avg, m.avg[i] ?? null);
      put(c.thr, m.thr[i] ?? false);
      put(c.load, m.load[i] ?? false);
      put(c.limit, m.limit ? m.limit[i] ?? null : null);
    });
    put(top.pkg, m.pkg);
    put(top.watts, m.watts);
    put(top.busy, m.busy);
    put(top.fan0, m.fans.rpm ? m.fans.rpm[0] ?? null : null);
    put(top.fan1, m.fans.rpm ? m.fans.rpm[1] ?? null : null);
    put(top.run, m.run);
    put(top.trip, m.trip);
    put(top.sensorError, m.sensorError);
  });
  if (watch.after) watch.after(watch.changed);
}

let nextId = 1;
const waiting = new Map<number, (reply: Reply) => void>();

/** Команда движку; ответ — когда придёт. Без канала — сразу отказ. */
export function ask(command: string, args: Record<string, unknown> = {}): Promise<Reply> {
  if (!link.peek().up) return Promise.resolve({ t: "reply", re: -1, ok: false, why: "нет связи с движком" });
  const id = nextId++;
  return new Promise(resolve => {
    waiting.set(id, resolve);
    host.postMessage({ ...args, id, c: command });
  });
}

/** Команда от владельца: отказ показывается строкой под верхней полосой, удача её убирает. */
export async function order(command: string, args: Record<string, unknown>, failure: string) {
  const reply = await ask(command, args);
  problem.value = reply.ok ? null : failure + ": " + (reply.why ?? "движок не объяснил");
}

function receive(m: Message) {
  switch (m.t) {
    case "link":
      link.value = { up: m.up, why: m.up ? null : m.why ?? null };
      if (m.up) {
        refused.value = null;
        host.postMessage({ id: 0, c: "hello", proto: Protocol });
      } else {
        for (const resolve of waiting.values()) resolve({ t: "reply", re: -1, ok: false, why: "связь с движком прервалась" });
        waiting.clear();
      }
      break;
    case "reply": {
      if (m.re === 0) {
        if (!m.ok) refused.value = m.why ?? "движок отказал";
        break;
      }
      const resolve = waiting.get(m.re);
      if (resolve) {
        waiting.delete(m.re);
        resolve(m);
      }
      break;
    }
    case "hello":
      greet(m);
      break;
    case "state":
      apply(m);
      break;
    case "limits":
      limits.value = m;
      break;
    case "shot":
      if (watch.shot) watch.shot(m.name);
      break;
  }
}

/** Слушать окно и сказать ему, что страница готова: до этого окно ей ничего не шлёт. */
export function start() {
  host.addEventListener("message", e => receive(e.data as Message));
  host.postMessage("ready");
}
