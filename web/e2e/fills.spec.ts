// A crowded digital row's fills on the canvas: fixtures/identify-jtag.tickwave whole, where TCK's
// edges crowd closer than a pixel and its row shades. Each layer is drawn a column at a time from
// an image (plot.ts `#drawFills`), so each column's layers are read here row by row of pixels.

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

test("a crowded row's band and rails each draw at one opacity down the column, the rails as deep as the line", async ({ page }) => {
  await page.goto(VIEW);
  await page.locator("#v").evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  const cols = await columns(page);
  // A column the line draws at full ink: a level, its depth the line's width.
  const flat = cols.map(runsOf).find((r) => r.length === 3 && r[0][0] === 0 && r[1][0] === 255 && r[2][0] === 0);
  expect(flat).toBeDefined();
  const lw = flat![1][1];
  // A shaded column with its band and rails at three opacities: from the top, nothing, the high
  // rail, the band, the low rail, nothing.
  const shaded = cols.map(runsOf).filter((r) => r.length === 5 && r[0][0] === 0 && r[4][0] === 0 && r[1][0] !== r[2][0] && r[2][0] !== r[3][0] && r[2][0] > 0 && r[2][0] < 255);
  // Not vacuous: TCK's whole row is mostly such columns.
  expect(shaded.length).toBeGreaterThan(100);
  for (const r of shaded) {
    expect([r[1][1], r[3][1]]).toEqual([lw, lw]);
    // The band fills between them, at one depth in every column.
    expect(r[2][1]).toBe(shaded[0][2][1]);
  }
  // The rails take the shares of time at each level, which sum to one: clear of the floor (0.2,
  // an alpha of 52), their alphas sum to 255, within their steps' rounding.
  const shares = shaded.filter((r) => r[1][0] > 60 && r[3][0] > 60);
  expect(shares.length).toBeGreaterThan(50);
  for (const r of shares) expect(Math.abs(r[1][0] + r[3][0] - 255)).toBeLessThanOrEqual(6);
});
