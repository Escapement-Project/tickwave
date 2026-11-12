// The view's shaping, as the convert flow uses it: a channel moved by its grip or its keys, trimmed by
// the handles at its data's ends, and rows reordered. A shape goes by the capture's name for a
// channel: identify-swd's SWCLK came from digital_3_0, its SWDIO from digital_3_1.
// identify-swd's SWDIO rises at 376,846,682 ns and falls at 376,851,176 (e2e/probe.spec.ts); its
// SWCLK falls at 376,719,436, rises at 376,720,063 and falls at 376,720,710.

import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&shaping";
type Shape = Record<string, { offset: number; keep: { from: number; to: number } | null }>;
const shape = (view: Locator) => view.evaluate((v) => (v as unknown as { shape: Shape }).shape);
const setShape = (view: Locator, s: Shape) => view.evaluate((v, s) => ((v as unknown as { shape: Shape }).shape = s), s);

async function ready(page: Page, query = ""): Promise<Locator> {
  await page.goto(VIEW + query);
  const view = page.locator("#v");
  await view.evaluate((v) => new Promise<void>((r) => (v.shadowRoot!.querySelector("canvas")!.width > 0 ? r() : v.addEventListener("tickwave-ready", () => r(), { once: true }))));
  return view;
}

async function show(view: Locator, start: number, end: number): Promise<void> {
  await view.evaluate((v, [s, e]) => (v as HTMLElement & { setWindow(s: number, e: number): void }).setWindow(s, e), [start, end]);
  await view.evaluate((v) => (v as HTMLElement & { settled(): Promise<void> }).settled());
}

/** The viewport x of a tick, from the view's own tickAt. */
async function xOf(view: Locator, tick: number): Promise<number> {
  const box = (await view.locator(".plot").boundingBox())!;
  const [xa, xb] = [box.x + box.width * 0.3, box.x + box.width * 0.7];
  const [ta, tb] = await view.evaluate((v, xs) => xs.map((x) => (v as HTMLElement & { tickAt(x: number): number }).tickAt(x)), [xa, xb]);
  return xa + ((tick + 0.5 - ta) * (xb - xa)) / (tb - ta);
}

/** The pointer's reading over the row at `row` (0 the top) at tick `tick`. */
async function readingAt(page: Page, view: Locator, row: number, tick: number): Promise<string> {
  const grip = (await view.locator(".grip").nth(row).boundingBox())!;
  await page.mouse.move(await xOf(view, tick), grip.y + grip.height / 2);
  const reading = view.locator('[part="reading"]');
  await expect(reading).toBeVisible();
  return (await reading.innerText()).replace(/\s+/g, " ");
}

async function drag(page: Page, el: Locator, dx: number, dy = 0): Promise<void> {
  const b = (await el.boundingBox())!;
  const [x, y] = [b.x + b.width / 2, b.y + b.height / 2];
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y + dy, { steps: 8 });
  await page.mouse.up();
}

test("a grip moves its channel in time by the drag, in whole ticks, and its keys by a pixel's worth", async ({ page }) => {
  const view = await ready(page);
  await show(view, 376_840_000, 376_860_000);
  const event = view.evaluate((v) => new Promise((r) => v.addEventListener("tickwave-shape", (e) => r((e as CustomEvent).detail), { once: true })));
  const grip = view.locator(".grip").nth(1);
  const x = (await grip.boundingBox())!.x;
  const perPx = await view.evaluate((v, x) => {
    const t = v as HTMLElement & { tickAt(x: number): number };
    return t.tickAt(x + 101) - t.tickAt(x + 1);
  }, x + 200);
  // Alt: the drag's own ticks, with no snap to SWCLK's edges.
  await page.keyboard.down("Alt");
  await drag(page, grip, 100);
  await page.keyboard.up("Alt");
  const moved = (await shape(view)).digital_3_1.offset;
  expect(Number.isInteger(moved)).toBe(true);
  expect(Math.abs(moved - perPx)).toBeLessThanOrEqual(Math.ceil(perPx / 100) + 1);
  const first = (await event) as { shape: Shape; change: string; channel: string };
  expect([first.change, first.channel, first.shape.digital_3_1.keep]).toEqual(["move", "digital_3_1", null]);
  await grip.focus();
  await page.keyboard.press("ArrowLeft");
  expect((await shape(view)).digital_3_1.offset).toBe(moved - Math.max(1, Math.round(perPx / 100)));
  await page.keyboard.press("Shift+ArrowRight");
  expect((await shape(view)).digital_3_1.offset).toBe(moved + 9 * Math.max(1, Math.round(perPx / 100)));
  await expect(grip).toHaveAttribute("aria-label", /^Move SWDIO, now .* later than recorded\./);
  // The other channel stays where it was.
  expect((await shape(view)).digital_3_0.offset).toBe(0);
});

test("a grip dragged near another row's edge snaps an edge of its channel onto it, and Alt moves it freely", async ({ page }) => {
  const view = await ready(page);
  // SWDIO falls at 376,845,415 and rises at 376,846,682; SWCLK has edges at 376,845,430 and
  // 376,846,704, 15 and 22 ns on: a few pixels at 4 µs across the view.
  await show(view, 376_844_500, 376_848_500);
  const grip = view.locator(".grip").nth(1);
  await drag(page, grip, 2);
  const snapped = (await shape(view)).digital_3_1.offset;
  expect([376_845_415 + snapped, 376_846_682 + snapped].some((t) => [376_845_430, 376_846_704].includes(t))).toBe(true);
  await setShape(view, { digital_3_1: { offset: 0, keep: null } });
  await page.keyboard.down("Alt");
  await drag(page, grip, 2);
  await page.keyboard.up("Alt");
  const free = (await shape(view)).digital_3_1.offset;
  expect([15, 22]).not.toContain(free);
  expect(free).toBeGreaterThan(0);
});

test("a moved channel reads where it is drawn, and a channel moved before the capture's start widens the view", async ({ page }) => {
  const view = await ready(page);
  await setShape(view, { digital_3_1: { offset: 10_000, keep: null } });
  await show(view, 376_840_000, 376_870_000);
  // SWDIO falls at 376,845,415 and rises at 376,846,682 as recorded; moved, its 1.267 µs low pulse is
  // at 376,855,415, where it was high, and its 4.494 µs high pulse follows.
  expect(await readingAt(page, view, 1, 376_856_000)).toMatch(/^low 1\.267 µs/);
  expect(await readingAt(page, view, 1, 376_858_682)).toMatch(/^high 4\.494 µs/);
  // Moved 400 ms earlier, SWCLK's data starts before tick 0, and Fit shows it.
  await setShape(view, { digital_3_0: { offset: -400_000_000, keep: null } });
  await view.locator(".fit").click();
  expect((await view.evaluate((v) => (v as unknown as { window: { start: number } }).window)).start).toBe(-400_000_000);
});

test("the trim handles keep part of a channel, snapping to its edges, and Home and End keep all of it again", async ({ page }) => {
  const view = await ready(page);
  await show(view, 376_715_000, 376_725_000);
  await setShape(view, { digital_3_0: { offset: 0, keep: { from: 376_716_000, to: 376_724_000 } } });
  const [start, end] = [view.locator(".trim").nth(0), view.locator(".trim").nth(1)];
  await expect(start).toBeVisible();
  await expect(start).toHaveAttribute("aria-valuetext", "376.716000 ms");
  // Dragged to within a few pixels of SWCLK's fall at 376,719,436, the start snaps to it.
  const b = (await start.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await page.mouse.down();
  await page.mouse.move((await xOf(view, 376_719_436)) + 2, b.y + b.height / 2, { steps: 8 });
  await page.mouse.up();
  expect((await shape(view)).digital_3_0.keep).toEqual({ from: 376_719_436, to: 376_724_000 });
  await start.focus();
  await page.keyboard.press("Home");
  await end.focus();
  await page.keyboard.press("End");
  expect((await shape(view)).digital_3_0.keep).toBeNull();
});

test("a grip dragged over the row above trades places with it, and the view keeps its place", async ({ page }) => {
  const view = await ready(page);
  await show(view, 376_840_000, 376_860_000);
  const before = await view.evaluate((v) => (v as unknown as { window: unknown }).window);
  await view.evaluate((v) => {
    const w = window as unknown as { orders: string[][] };
    w.orders = [];
    v.addEventListener("tickwave-shape", (e) => w.orders.push((e as CustomEvent<{ channels: string[] }>).detail.channels));
  });
  await drag(page, view.locator(".grip").nth(1), 0, -30);
  // The drag reports as it goes; the last report has the rows' new order.
  expect(await page.evaluate(() => (window as unknown as { orders: string[][] }).orders.at(-1))).toEqual(["SWDIO", "SWCLK"]);
  await expect(view).toHaveAttribute("channels", "SWDIO SWCLK");
  expect(await view.evaluate((v) => (v as unknown as { window: unknown }).window)).toEqual(before);
  // The keys go from grip to grip in the rows' new order.
  await expect(view.locator(".grip").first()).toHaveAttribute("aria-label", /^Move SWDIO/);
  // With the focus on a grip, Down moves its row back.
  const swdio = view.locator('.grip[aria-label^="Move SWDIO"]');
  await swdio.focus();
  await page.keyboard.press("ArrowDown");
  await expect(view).toHaveAttribute("channels", "SWCLK SWDIO");
  // The grip keeps the focus as its row moves, and the keys go from grip to grip in the rows' order.
  await expect(swdio).toBeFocused();
  await expect(view.locator(".grip").first()).toHaveAttribute("aria-label", /^Move SWCLK/);
});

test("without shaping, the view has no grips or trim handles and keeps no room for them, and draws the recording where it was recorded", async ({ page }) => {
  // The names column's width, which each row's height handle spans, in a view that never shaped.
  await page.goto("/e2e/pages/view.html?open");
  const plain = page.locator("#v");
  await plain.evaluate((v) => new Promise<void>((r) => (v.shadowRoot!.querySelector("canvas")!.width > 0 ? r() : v.addEventListener("tickwave-ready", () => r(), { once: true }))));
  const names = (v: Locator) => v.locator(".row-handle").first().evaluate((h) => (h as HTMLElement).style.width);
  const never = await names(plain);
  const view = await ready(page);
  expect(await names(view)).not.toBe(never);
  await setShape(view, { digital_3_1: { offset: 10_000, keep: null } });
  await view.evaluate((v) => ((v as unknown as { shaping: boolean }).shaping = false));
  await expect(view.locator(".grip")).toHaveCount(0);
  await expect(view.locator(".trim")).toHaveCount(0);
  expect(await names(view)).toBe(never);
  await show(view, 376_840_000, 376_870_000);
  expect(await view.evaluate((v) => v.shadowRoot!.querySelectorAll(".grip").length)).toBe(0);
  await view.locator(".plot").hover();
  const box = (await view.locator(".plot").boundingBox())!;
  await page.mouse.move(await xOf(view, 376_848_682), box.y + box.height * 0.55);
  await expect(view.locator('[part="reading"]')).toContainText(/^high 4\.494\s?µs/);
});
