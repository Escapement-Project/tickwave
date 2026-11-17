// A crowded digital row's fills on the canvas: fixtures/identify-jtag.tickwave whole, where TCK's
// edges crowd closer than a pixel and its row shades. Each layer is drawn a column at a time from
// an image (plot.ts `#drawFills`), so each column's layers are read here row by row of pixels. Then
// identify-swd with its sidecar, whose notes split the crowded rows into stretches.

import { test, expect } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/identify-jtag.tickwave";

/** Row 0's pixel columns right of its words, each as its alphas from the row's top to its bottom. */
async function columns(page: import("@playwright/test").Page): Promise<number[][]> {
  return page.locator("#v").evaluate((v) => {
    const root = v.shadowRoot!;
    const canvas = root.querySelector("canvas")!;
    const handles = [...root.querySelectorAll<HTMLElement>('[part="row-handle"]')];
    const dpr = canvas.width / canvas.getBoundingClientRect().width;
    const top = Math.round((handles[0].offsetTop - 28) * dpr);
    const bottom = Math.round(handles[0].offsetTop * dpr);
    const left = Math.round((handles[0].offsetWidth + 200) * dpr);
    const width = canvas.width - left - Math.round(20 * dpr);
    const d = canvas.getContext("2d")!.getImageData(left, top, width, bottom - top).data;
    return Array.from({ length: width }, (_, x) => Array.from({ length: bottom - top }, (_, y) => d[4 * (y * width + x) + 3]));
  });
}

/** A column's alphas as runs of one value: [alpha, how many rows]. */
const runsOf = (col: number[]) =>
  col.reduce<[number, number][]>((out, a) => {
    const last = out.at(-1);
    if (last && last[0] === a) last[1]++;
    else out.push([a, 1]);
    return out;
  }, []);

/** The first row a column paints, and the row past its last. */
const painted = (c: number[]) => [c.findIndex((a) => a > 0), c.length - [...c].reverse().findIndex((a) => a > 0)];

test("a crowded row's band and rails each draw at one opacity down the column, the rails as deep as the line, the larger at the band's", async ({ page }) => {
  await page.goto(VIEW);
  await page.locator("#v").evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  const cols = await columns(page);
  // A column the line draws at full ink: a level, its depth the line's width.
  const flat = cols.map(runsOf).find((r) => r.length === 3 && r[0][0] === 0 && r[1][0] === 255 && r[2][0] === 0);
  expect(flat).toBeDefined();
  const lw = flat![1][1];
  // A shaded column: part ink, part clear down its middle, painted from the high rail's top to the
  // low rail's bottom.
  const shaded = cols.filter((c) => {
    const mid = c[Math.floor(c.length / 2)];
    return mid > 0 && mid < 255;
  });
  // Not vacuous: TCK's whole row is mostly such columns.
  expect(shaded.length).toBeGreaterThan(100);
  const [top0, bottom0] = painted(shaded[0]);
  let unlike = 0;
  for (const c of shaded) {
    const [top, bottom] = painted(c);
    // Each rail a line's width deep, the band between them, at one depth in every column.
    expect(bottom - top).toBe(bottom0 - top0);
    const [high, band, low] = [c.slice(top, top + lw), c.slice(top + lw, bottom - lw), c.slice(bottom - lw, bottom)];
    for (const part of [high, band, low]) expect(new Set(part).size).toBe(1);
    // The level with the larger share of the time takes the band's opacity; the other a part of
    // it, at least a fifth, within their steps' rounding.
    const [b, h, l] = [band[0], high[0], low[0]];
    expect(Math.abs(Math.max(h, l) - b)).toBeLessThanOrEqual(2);
    expect(Math.min(h, l)).toBeGreaterThanOrEqual(Math.round(0.2 * b) - 2);
    if (Math.abs(h - l) > 4) unlike++;
  }
  // Not vacuous: some columns' levels take unlike shares.
  expect(unlike).toBeGreaterThan(10);
});

test("a crowded row its notes split into stretches puts each piece's fills once a draw, however many stretches take turns at them", async ({ page }) => {
  // 40 ms of identify-swd: SWCLK and SWDIO crowd, and the sidecar paints the probe's and the
  // target's turns, the target's heavier, so each row's stretches take turns at a line width's
  // fills and another's.
  await page.goto(`/e2e/pages/view.html?open&src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml&window=${encodeURIComponent("357ms 397ms")}`);
  const view = page.locator("#v");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  // One draw's puts into a strip and draws from one, a pointer onto a row drawing it.
  await page.evaluate(() => {
    const n = { puts: 0, images: 0, draws: 0 };
    const P = CanvasRenderingContext2D.prototype as unknown as Record<string, (...a: unknown[]) => unknown>;
    for (const [name, key] of [
      ["putImageData", "puts"],
      ["drawImage", "images"],
      ["clearRect", "draws"],
    ] as const) {
      const f = P[name];
      P[name] = function (this: unknown, ...a: unknown[]) {
        n[key]++;
        return f.apply(this, a);
      };
    }
    (window as unknown as { e2eCounts: typeof n }).e2eCounts = n;
  });
  const box = (await view.locator(".plot").boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 30);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  const n = await page.evaluate(() => (window as unknown as { e2eCounts: { puts: number; images: number; draws: number } }).e2eCounts);
  expect(n.draws).toBe(1);
  // Not vacuous: a stretch draws its fills' three layers, and dozens of them do.
  expect(n.images / 3).toBeGreaterThan(50);
  // Two rows, each a piece at two line widths: four sets of fills, put once each.
  expect(n.puts).toBeLessThanOrEqual(4);
});
