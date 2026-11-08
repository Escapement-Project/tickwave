// The annotation lane: spans as brackets, points as ticks, each with its label where it fits.
// Brackets take lanes first, in order of start, so a long label never pushes a later bracket out.
// Then each label goes in the room its lane leaves around its bracket: whole if it fits, cut with
// an ellipsis if not, and left off when fewer than six characters would show. A bracket with no free lane is not drawn.

export interface NoteIn {
  readonly id: string;
  readonly start: number;
  readonly end: number | null;
  readonly label: string | null;
}

export interface NoteOut {
  readonly id: string;
  readonly lane: number;
  /** The bracket, clipped to the plot; x0 === x1 for a point. */
  readonly x0: number;
  readonly x1: number;
  readonly point: boolean;
  /** The label as drawn, or null. */
  readonly label: string | null;
  /** The label's left edge. */
  readonly labelX: number;
  /** The extent the note takes in its lane: bracket and label. */
  readonly left: number;
  readonly right: number;
}

const ELLIPSIS = "…";

/** The longest prefix of `text` that, with an ellipsis, fits `room`; null below `least` characters. */
export function elide(text: string, room: number, measure: (s: string) => number, least = 6): string | null {
  if (measure(text) <= room) return text;
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (measure(text.slice(0, mid).trimEnd() + ELLIPSIS) <= room) lo = mid;
    else hi = mid - 1;
  }
  return lo >= least ? text.slice(0, lo).trimEnd() + ELLIPSIS : null;
}

export function layoutNotes(
  notes: readonly NoteIn[],
  toX: (tick: number) => number,
  left: number,
  right: number,
  lanes: number,
  measure: (s: string) => number,
  gap = 8,
): NoteOut[] {
  // Pass 1: brackets into lanes.
  const lastRight = new Array<number>(lanes).fill(-Infinity);
  const placed: { n: NoteIn; lane: number; x0: number; x1: number; point: boolean }[] = [];
  for (const n of notes) {
    const a = toX(n.start);
    const b = n.end === null ? a : toX(n.end);
    if (b < left || a > right) continue;
    const point = n.end === null;
    const x0 = Math.max(a, left);
    const x1 = Math.min(b, right);
    const lo = point ? x0 - 2 : x0;
    // Brackets may touch: one span often ends at the tick the next starts. Of the free lanes,
    // take the one free longest, so the labels to come get the most room.
    let best = -1;
    for (let lane = 0; lane < lanes; lane++) {
      if (lastRight[lane] <= lo + 0.5 && (best < 0 || lastRight[lane] < lastRight[best])) best = lane;
    }
    if (best >= 0) {
      lastRight[best] = point ? x0 + 2 : x1;
      placed.push({ n, lane: best, x0, x1, point });
    }
  }
  // Pass 2: labels into the room between neighbours in the same lane.
  const out: NoteOut[] = [];
  for (let lane = 0; lane < lanes; lane++) {
    const row = placed.filter((p) => p.lane === lane);
    let used = left - gap;
    row.forEach((p, i) => {
      const nextLeft = i + 1 < row.length ? (row[i + 1].point ? row[i + 1].x0 - 2 : row[i + 1].x0) : right + gap;
      const lo = used + gap;
      const hi = nextLeft - gap;
      const bracketLeft = p.point ? p.x0 - 2 : p.x0;
      const bracketRight = p.point ? p.x0 + 2 : p.x1;
      let label: string | null = null;
      let labelX = p.x0;
      if (p.n.label) {
        if (p.point) {
          // A point's label sits to its right, or to its left where there is no room.
          const w = measure(p.n.label);
          if (p.x0 + 4 + w <= Math.min(hi, right)) [label, labelX] = [p.n.label, p.x0 + 4];
          else if (p.x0 - 4 - w >= Math.max(lo, left)) [label, labelX] = [p.n.label, p.x0 - 4 - w];
          else {
            const cut = elide(p.n.label, Math.min(hi, right) - p.x0 - 4, measure);
            if (cut) [label, labelX] = [cut, p.x0 + 4];
          }
        } else {
          const room = Math.min(hi, right) - Math.max(lo, left);
          const cut = elide(p.n.label, room, measure);
          if (cut) {
            const w = measure(cut);
            const centre = (p.x0 + p.x1) / 2;
            labelX = Math.min(Math.max(centre - w / 2, Math.max(lo, left)), Math.min(hi, right) - w);
            label = cut;
          }
        }
      }
      const w = label ? measure(label) : 0;
      const l = label ? Math.min(bracketLeft, labelX) : bracketLeft;
      const r = label ? Math.max(bracketRight, labelX + w) : bracketRight;
      used = r;
      out.push({ id: p.n.id, lane, x0: p.x0, x1: p.x1, point: p.point, label, labelX, left: l, right: r });
    });
  }
  return out;
}
