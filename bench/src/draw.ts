// The inset stand-in: each channel of one block as a polyline across the canvas. Edges that land
// in one pixel column collapse into that column's vertical stroke, as any renderer at this
// width would, so the path holds at most a few points a column.

import type { Block } from "./format.ts";

const ROW = 40;

export function drawBlock(canvas: HTMLCanvasElement, b: Block): void {
  canvas.height = ROW * b.channels.length;
  const ctx = canvas.getContext("2d", { willReadFrequently: false })!;
  const w = canvas.width;
  let last = b.start + 1;
  for (const c of b.channels) if (c.kind === "digital" && c.edges.length) last = Math.max(last, c.edges[c.edges.length - 1]);
  const end = Number.isFinite(b.end) && b.end < 1e300 ? b.end : last + 1;
  const scale = w / (end - b.start);
  ctx.clearRect(0, 0, w, canvas.height);
  ctx.lineWidth = 1;
  ctx.strokeStyle = "#000";
  b.channels.forEach((c, k) => {
    const lo = k * ROW + ROW - 5;
    const hi = k * ROW + 5;
    ctx.beginPath();
    if (c.kind === "digital") {
      let level = c.initial;
      ctx.moveTo(0, level ? hi : lo);
      let col = -1;
      let dirty = false;
      for (let j = 0; j < c.edges.length; j++) {
        const x = Math.floor((c.edges[j] - b.start) * scale);
        if (x !== col) {
          if (dirty) ctx.lineTo(col, level ? hi : lo);
          ctx.lineTo(x, level ? hi : lo);
          level = !level;
          ctx.lineTo(x, level ? hi : lo);
          col = x;
          dirty = false;
        } else {
          level = !level;
          dirty = true;
        }
      }
      if (dirty) ctx.lineTo(col, level ? hi : lo);
      ctx.lineTo(w, level ? hi : lo);
    } else {
      const v = c.values;
      let min = Infinity;
      let max = -Infinity;
      for (let j = 0; j < v.length; j++) {
        if (v[j] < min) min = v[j];
        if (v[j] > max) max = v[j];
      }
      const ys = (x: number) => lo - ((x - min) / (max - min || 1)) * (lo - hi);
      const per = v.length / w;
      for (let x = 0; x < w; x++) {
        let a = Infinity;
        let z = -Infinity;
        const to = Math.min(v.length, Math.floor((x + 1) * per));
        for (let j = Math.floor(x * per); j < to; j++) {
          if (v[j] < a) a = v[j];
          if (v[j] > z) z = v[j];
        }
        if (a <= z) {
          ctx.moveTo(x + 0.5, ys(a));
          ctx.lineTo(x + 0.5, ys(z));
        }
      }
    }
    ctx.stroke();
  });
}
