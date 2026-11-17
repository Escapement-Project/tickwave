// The traces' line weight from the page: `--tickwave-line-weight`, on e2e/pages/view.html with
// identify-swd.tickwave and its annotations, over the first read's data, where SWDIO is painted by
// the target's drive and by the probe's. A level's line is read from the canvas's pixels.

import { test, expect, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml&window=377090000%20377145000";

/** Row 1's (SWDIO's) pixel columns over `[from, to)` ticks, each as its alphas down the row. */
async function columns(page: Page, from: number, to: number): Promise<number[][]> {
  return page.locator("#v").evaluate(
    (v, [from, to]) => {
      const root = v.shadowRoot!;
      const canvas = root.querySelector("canvas")!;
      const box = canvas.getBoundingClientRect();
      const handles = [...root.querySelectorAll<HTMLElement>('[part="row-handle"]')];
      const dpr = canvas.width / box.width;
      const at = (t: number) => {
        // `tickAt` is linear in x: two points place any tick.
        const e = v as unknown as { tickAt(x: number): number };
        const [x0, x1] = [box.left + box.width * 0.5, box.left + box.width * 0.6];
        const [t0, t1] = [e.tickAt(x0), e.tickAt(x1)];
        return Math.round((x0 + ((t - t0) / (t1 - t0)) * (x1 - x0) - box.left) * dpr);
      };
      const [left, right] = [at(from), at(to)];
      const top = Math.round((handles[1].offsetTop - 28) * dpr);
      const bottom = Math.round(handles[1].offsetTop * dpr);
      const d = canvas.getContext("2d")!.getImageData(left, top, right - left, bottom - top).data;
      const w = right - left;
      return Array.from({ length: w }, (_, x) => Array.from({ length: bottom - top }, (_, y) => d[4 * (y * w + x) + 3]));
    },
    [from, to],
  );
}

/** The depth of the line most columns draw as one level alone: the line's weight in device px. */
function lineDepth(cols: number[][]): number {
  const depths = new Map<number, number>();
  for (const c of cols) {
    const runs = c.reduce<[number, number][]>((out, a) => {
      const last = out.at(-1);
      if (last && last[0] === a) last[1]++;
      else out.push([a, 1]);
      return out;
    }, []);
    if (runs.length === 3 && runs[0][0] === 0 && runs[1][0] === 255 && runs[2][0] === 0) depths.set(runs[1][1], (depths.get(runs[1][1]) ?? 0) + 1);
  }
  return [...depths].sort((a, b) => b[1] - a[1])[0][0];
}

/** The view with `weight` set on it, as a page sets the property on the element. */
async function open(page: Page, weight: string | null): Promise<void> {
  await page.goto(weight === null ? VIEW : `${VIEW}&style=${encodeURIComponent(`--tickwave-line-weight:${weight}`)}`);
  await page.locator("#v").evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
}

// The target's drive, ack and data: 377,103,058 to 377,139,060. The probe's request before it.
const TARGET: [number, number] = [377_106_000, 377_138_000];
const PROBE: [number, number] = [377_094_600, 377_102_500];

for (const [weight, depth] of [
  [null, 3],
  ["3px", 6],
  ["3", 6],
  [" 2.5 px ", 5],
  ["0", 3],
  ["-2px", 3],
  ["thick", 3],
  ["2em", 3],
] as const) {
  test(`a line weight of ${JSON.stringify(weight)} draws a level ${depth} device pixels deep, and the target's drive a pixel heavier`, async ({ page }) => {
    await open(page, weight);
    expect(lineDepth(await columns(page, ...PROBE))).toBe(depth);
    expect(lineDepth(await columns(page, ...TARGET))).toBe(depth + 2);
  });
}
