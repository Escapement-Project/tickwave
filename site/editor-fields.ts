// What the annotation editor writes into a field of the hand-written file. Apart from the editor,
// so tests run it without the elements.

import { formatInterval } from "../web/src/readouts.ts";
import { parseTime, toTicks } from "../web/src/time.ts";

/** A time for the file: as a decimal at the recording's resolution, which reads back as the same
 * tick, or as ticks where it would not. A plain space before the unit, for whoever edits the file
 * by hand next, where the readouts keep theirs from breaking. */
export function timeText(tick: number, timebase: { num: number; den: number }): string | number {
  const text = formatInterval(tick, timebase).replace(/\u00a0/g, " ");
  const back = parseTime(text);
  return back && toTicks(back, timebase) === tick ? text : tick;
}

/** An id made from a label, unlike every id in `taken`. */
export function idFrom(label: string, taken: ReadonlySet<string>): string {
  const base =
    label
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[\u0300-\u036f]/g, "")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 40)
      .replace(/-+$/, "") || "annotation";
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}
