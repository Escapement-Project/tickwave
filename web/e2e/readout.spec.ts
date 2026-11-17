// A view's readout keeps its lines' heights as they fill (AGENTS.md, "Nothing moves under the
// reader"): the selection's counts, a "…" until the edges are counted, and its periods, nothing
// until they are worked out, then a menu and figures. The figures are in a font of their own,
// here one whose line is taller than the text's, so a line that held only the text's font would
// grow when they came. The worker's counts are held back, so the lines are seen waiting.

import { test, expect, type Locator, type Page } from "@playwright/test";

const FONTS = "--font-text: 'DejaVu Sans', sans-serif; --font-figures: 'C059', serif";

/** Holds each count and period read back by `ms`, so the readout waits for them. */
async function holdCounts(page: Page, ms: number): Promise<void> {
  await page.evaluate((ms) => {
    const post = Worker.prototype.postMessage;
    Worker.prototype.postMessage = function (this: Worker, m: unknown, ...rest: unknown[]) {
      const msg = m as { op?: string; requests?: { op?: string }[] };
      const ops = msg?.op === "batch" ? (msg.requests ?? []).map((r) => r?.op) : [msg?.op];
      const send = () => (post as (...a: unknown[]) => void).call(this, m, ...rest);
      if (ops.some((op) => op === "edgeCount" || op === "periods" || op === "edges")) setTimeout(send, ms);
      else send();
    };
  }, ms);
}

/** Every height the readout and each of its lines take from here on, in order, to the
 * hundredth of a pixel, as a ResizeObserver sees them. */
async function watch(view: Locator): Promise<void> {
  await view.evaluate((v) => {
    const readout = v.shadowRoot!.querySelector(".readout")!;
    const seen: { readout: number[]; lines: number[][]; waited: boolean } = { readout: [], lines: [], waited: false };
    (v as unknown as { e2eSeen: typeof seen }).e2eSeen = seen;
    const round = (x: number) => Math.round(x * 100) / 100;
    const record = () => {
      const r = round(readout.getBoundingClientRect().height);
      if (seen.readout[seen.readout.length - 1] !== r) seen.readout.push(r);
      const lines = [...readout.querySelectorAll(":scope > .line")].map((l) => round(l.getBoundingClientRect().height));
      if (JSON.stringify(seen.lines[seen.lines.length - 1]) !== JSON.stringify(lines)) seen.lines.push(lines);
      seen.waited ||= readout.textContent!.includes("…");
    };
    new MutationObserver(record).observe(readout, { childList: true, subtree: true, characterData: true });
    new ResizeObserver(record).observe(readout);
  });
}

const seen = (view: Locator) => view.evaluate((v) => (v as unknown as { e2eSeen: { readout: number[]; lines: number[][]; waited: boolean } }).e2eSeen);

async function ready(page: Page): Promise<Locator> {
  await page.goto(`/e2e/pages/view.html?open&style=${encodeURIComponent(FONTS)}`);
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await page.evaluate(() => document.fonts.ready);
  return view;
}

/** Drags along the time axis from `from` to `to`, as shares of the plot's width. */
async function select(page: Page, view: Locator, from: number, to: number): Promise<void> {
  const box = (await view.locator(".plot").boundingBox())!;
  const y = box.y + box.height - 10;
  await page.mouse.move(box.x + box.width * from, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * to, y, { steps: 6 });
  await page.mouse.up();
}

test("a selection's readout keeps each line's height from the first it shows, while its counts and periods are worked out and once they are", async ({ page }) => {
  const view = await ready(page);
  await holdCounts(page, 400);
  await watch(view);
  await select(page, view, 0.45, 0.8);
  await expect(view.locator(".readout select")).toHaveCount(1, { timeout: 10_000 });
  await expect(view.locator(".readout")).toContainText("median");
  const all = await seen(view);
  // From the empty readout, hidden, to its first lines: from there, waiting lines, then filled
  // ones, three lines throughout, each as tall as it was, and the readout too.
  expect(all.lines[0]).toEqual([]);
  const [readout, lines] = [all.readout.filter((h) => h > 0), all.lines.slice(1)];
  expect(all.waited, "the lines were seen waiting").toBe(true);
  expect(new Set(lines.map((l) => JSON.stringify(l))).size, JSON.stringify(all.lines)).toBe(1);
  expect(lines[0].length).toBe(3);
  expect(new Set(readout).size, JSON.stringify(all.readout)).toBe(1);
});

test("a second selection's readout, its counts waiting again, keeps the heights the first one's lines took", async ({ page }) => {
  const view = await ready(page);
  await select(page, view, 0.45, 0.8);
  await expect(view.locator(".readout")).toContainText("median");
  await holdCounts(page, 400);
  await watch(view);
  await select(page, view, 0.5, 0.9);
  await expect(view.locator(".readout")).toContainText("…");
  await expect(view.locator(".readout")).toContainText("median", { timeout: 10_000 });
  const { readout, lines, waited } = await seen(view);
  expect(waited).toBe(true);
  expect(new Set(lines.map((l) => JSON.stringify(l))).size, JSON.stringify(lines)).toBe(1);
  expect(new Set(readout).size, JSON.stringify(readout)).toBe(1);
});
