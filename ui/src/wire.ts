// Сообщения движка (src/Feed.cs) и окна (src/WebWindow.cs) — как их видит страница.
// Хозяин формата — движок; здесь его поля переписаны руками. Что пропало или переименовано, ловит скрытая проверка:
// страница рисует по этим полям, и без них не нарисует.

export const Protocol = 1;

export type Kind = "P" | "E";

export interface CoreInfo { lp: number; kind: Kind; cluster: number }

export interface BlockInfo {
  index: number;
  members: number[];   // ядра блока: номера в общем списке ядер
  first: number;       // по первому ядру блок меряется и через него ставится потолок
  lp: number;
  kind: Kind;
  name: string;        // «0» или «2–5»
  title: string;       // «ядро 0» или «блок E-ядер 2–5»
}

export interface Tile { block: number; col: number; row: number; places: number[] }

export interface Hello {
  t: "hello";
  proto: number;
  build: string;
  simulated: boolean;
  cpu: string;
  cores: CoreInfo[];
  blocks: BlockInfo[];
  layout: { name: string; note: string | null; confirmed: boolean; cols: number; rows: number; tiles: Tile[] };
  sensors: boolean;
  sensorProblem: string | null;
  limits: boolean;
  curves: boolean;
}

export interface Run {
  n: number; kind: string; block: number; done: number; total: number;
  blockDone: number; blockTotal: number; stepDone: number; stepTotal: number; elapsed: number; text: string | null;
}

/** Числа без значения (датчик не ответил, ядро спало) приходят как null. Ряды — по ядрам. */
export interface State {
  t: "state";
  n: number;
  pkg: number | null;
  pkgMax: number | null;
  watts: number | null;
  busy: boolean;
  level: number;
  cur: (number | null)[];
  max: (number | null)[];
  ratio: (number | null)[];
  avg: (number | null)[];
  thr: boolean[];
  load: boolean[];
  limit: number[] | null;
  fans: { rpm: number[] | null; max: boolean; spin: boolean; mode: number };
  run: Run | null;
  trip: string | null;
  sensorError: string | null;
}

/** Потолки по ядрам: заводские, «было»; choices — что можно поставить блоку, по убыванию, по номеру блока. */
export interface Limits { t: "limits"; fused: number[]; was: number[]; unread: boolean; choices: number[][] }

export interface Reply { t: "reply"; re: number; ok: boolean; why?: string; note?: string }

/** От окна: есть ли канал к движку. */
export interface Link { t: "link"; up: boolean; why?: string }

/** От окна, только скрытой проверке: картинка снята. */
export interface Shot { t: "shot"; name: string }

export type Message = Hello | State | Limits | Reply | Link | Shot | { t: "profiles" | "notice" | "run.finished" };
