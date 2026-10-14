// Как числа выглядят на странице. Правила те же, что у прежнего окна (src/Ui.cs), чтобы владельцу не переучиваться.

/** Наибольшее из чисел; null — ни одного нет. Только для показа: так считаются числа плитки блока (DieView.Highest). */
export function highest(values: (number | null)[]): number | null {
  let top: number | null = null;
  for (const v of values) if (v !== null && (top === null || v > top)) top = v;
  return top;
}

/** 0..1 → цвет от холодного синего к красному (Theme.Heat). */
export function heat(k: number): string {
  k = Math.max(0, Math.min(1, k));
  return `hsl(${(215 - 215 * k).toFixed(0)} 65% ${(28 + 10 * k).toFixed(1)}%)`;
}

/** Цвет плитки по температуре: 40 °C — синий, 100 °C — красный. */
export function tileColor(temp: number | null): string {
  return temp === null ? "var(--cell)" : heat((temp - 40) / 60);
}

export function signed(n: number): string {
  return (n > 0 ? "+" : "−") + Math.abs(n);
}
