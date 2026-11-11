// An analog row's scale, whose values the reader zooms and moves, on e2e/pages/view.html with
// swd50-cut.tickwave: SWCLK and SWDIO analog, NRST digital.

import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/swd50-cut.tickwave";

async function ready(view: Locator): Promise<void> {
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
}

const win = (view: Locator) => view.evaluate((v) => (v as HTMLElement & { window: { start: number; end: number } }).window);

const SWDIO: [number, number] = [2.5, 4.5];

const PREFIX: Record<string, number> = { "": 1, m: 1e-3, µ: 1e-6, n: 1e-9, k: 1e3 };

/** The two values a scale labels, bottom and top, in volts, from its accessible name. */
async function labels(scale: Locator): Promise<[number, number]> {
  const name = (await scale.getAttribute("aria-label"))!;
  const m = /, (.+?) to (.+?)\. /.exec(name);
  if (!m) throw new Error(`no range in "${name}"`);
  const volts = (s: string) => {
    const v = /^(-?[\d.]+)([mµnk]?)V$/.exec(s.replace(/\s/g, "").replace("−", "-"));
    if (!v) throw new Error(`not a voltage: "${s}"`);
    return Number(v[1]) * PREFIX[v[2]];
  };
  return [volts(m[1]), volts(m[2])];
}

/** Ctrl and three turns of the wheel by `dy`: Chromium sends a smaller step than Firefox for one. */
async function ctrlWheel(page: Page, dy: number): Promise<void> {
  await page.keyboard.down("Control");
  for (let i = 0; i < 3; i++) await page.mouse.wheel(0, dy);
  await page.keyboard.up("Control");
}

async function middle(scale: Locator): Promise<{ x: number; y: number }> {
  const b = (await scale.boundingBox())!;
  return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
}

test("each analog row has a scale named by its channel and the values it labels; a digital row has none", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const scales = view.locator(".scale");
  await expect(scales).toHaveCount(2);
  await expect(scales.first()).toHaveAttribute("aria-label", /^Scale of SWCLK, −2\u00a0V to 6\u00a0V\. /);
  expect(await labels(scales.nth(1))).toEqual(SWDIO);
});

test("Ctrl or Cmd with the wheel over a scale zooms its values about the pointer, and nothing else; Fit shows them whole again", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const [swclk, swdio] = [view.locator(".scale").first(), view.locator(".scale").nth(1)];
  const window = await win(view);
  const other = await labels(swdio);
  // About a third of the way down the 8 V between its rules, on a whole pixel, as Firefox reports
  // the pointer. Zoomed about it, that value stays a third of the way down: inside the labels,
  // which sit within a quarter of the range of its edges, and nearer the top one.
  const b = (await swclk.boundingBox())!;
  const y = Math.round(b.y + 6 + (b.height - 12) / 3);
  const at = 6 - ((y - (b.y + 6)) / (b.height - 12)) * 8;
  await page.mouse.move(b.x + b.width / 2, y);
  await ctrlWheel(page, -300);
  const [bottom, top] = await labels(swclk);
  expect(top - bottom).toBeLessThan(4);
  expect(bottom).toBeLessThan(at);
  expect(top).toBeGreaterThan(at);
  expect((at - bottom) / (top - bottom)).toBeGreaterThan(0.5);
  expect(await win(view)).toEqual(window);
  expect(await labels(swdio)).toEqual(other);
  await view.locator(".fit").click();
  expect(await labels(swclk)).toEqual([-2, 6]);
});

test("a drag on a scale moves its values with the pointer, places no cursor and moves along nothing", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  const window = await win(view);
  const p = await middle(swclk);
  await page.mouse.move(p.x, p.y);
  await ctrlWheel(page, -300);
  const [bottom, top] = await labels(swclk);
  // Down by a quarter of the row's height: the trace follows, so higher values come into the row.
  const b = (await swclk.boundingBox())!;
  await page.mouse.down();
  await page.mouse.move(p.x, p.y + (b.height - 12) / 4, { steps: 4 });
  await page.mouse.up();
  const [bottom2, top2] = await labels(swclk);
  // A quarter of the range is two steps of its labels or more: both move up.
  expect(bottom2).toBeGreaterThan(bottom);
  expect(top2).toBeGreaterThan(top);
  expect(await win(view)).toEqual(window);
  await expect(view.locator(".readout")).toHaveText("");
});

test("a double click on a scale fits its values to what the view shows", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  // Before the first clock: SWCLK idles high, within a few tenths of a volt.
  await view.evaluate((v) => (v as unknown as { setWindow(a: number, b: number): void }).setWindow(0, 1500));
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await swclk.dblclick();
  const [bottom, top] = await labels(swclk);
  expect(top - bottom).toBeLessThan(1);
  expect(bottom).toBeGreaterThan(2);
  await expect(view.locator(".readout")).toHaveText("");
});

test("with a scale focused, up and down move its values, plus and minus zoom them, Enter fits them and Home shows them whole", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  const window = await win(view);
  await swclk.focus();
  // Each key is the scale's alone: on the plot, plus and up zoom along the capture, Home fits it
  // and Enter places a cursor.
  await page.keyboard.press("+");
  const [bottom, top] = await labels(swclk);
  expect(top - bottom).toBeLessThan(8);
  expect(await win(view)).toEqual(window);
  // Up moves the trace up: the values in the row move down.
  await page.keyboard.press("ArrowUp");
  const [bottom2] = await labels(swclk);
  expect(bottom2).toBeLessThan(bottom);
  expect(await win(view)).toEqual(window);
  await page.keyboard.press("-");
  await page.keyboard.press("Home");
  expect(await labels(swclk)).toEqual([-2, 6]);
  await page.keyboard.press("Enter");
  const fitted = await labels(swclk);
  expect(fitted).not.toEqual([-2, 6]);
  expect(fitted[0]).toBeGreaterThanOrEqual(-2);
  expect(fitted[1]).toBeLessThanOrEqual(6);
  expect(await win(view)).toEqual(window);
  await expect(view.locator(".readout")).toHaveText("");
});

test("Ctrl or Cmd with the wheel over the traces still zooms along the capture, and the plain wheel over a scale says how to zoom it", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  const window = await win(view);
  const b = (await view.locator(".plot").boundingBox())!;
  const p = await middle(swclk);
  await page.mouse.move(b.x + b.width / 2, p.y);
  await ctrlWheel(page, -300);
  expect((await win(view)).end - (await win(view)).start).toBeLessThan(window.end - window.start);
  expect(await labels(swclk)).toEqual([-2, 6]);
  await page.mouse.move(p.x, p.y);
  await page.mouse.wheel(0, 100);
  await expect(view.locator(".hint")).toHaveText(/ \+ scroll to zoom the scale$/);
});

test("on a touch screen, two fingers on a scale zoom its values and one moves them", async ({ browser, browserName }) => {
  test.skip(browserName !== "chromium", "two touch points at once need Chromium's DevTools protocol");
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  const window = await win(view);
  const { x, y } = await middle(swclk);
  const cdp = await context.newCDPSession(page);
  const fingers = (type: "touchStart" | "touchMove" | "touchEnd", d: number | null) =>
    cdp.send("Input.dispatchTouchEvent", {
      type,
      touchPoints: d === null ? [] : [{ x, y: y - d, id: 0 }, { x, y: y + d, id: 1 }],
    });
  await fingers("touchStart", 10);
  for (let d = 15; d <= 40; d += 5) await fingers("touchMove", d);
  await fingers("touchEnd", null);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(r)));
  const [bottom, top] = await labels(swclk);
  expect(top - bottom).toBeLessThan(4);
  // One finger down the scale moves the trace down with it. (Whether the page would scroll instead
  // without the scale's touch-action, these synthetic touches don't show: they never scroll it.)
  const finger = (type: "touchStart" | "touchMove" | "touchEnd", dy: number | null) =>
    cdp.send("Input.dispatchTouchEvent", { type, touchPoints: dy === null ? [] : [{ x, y: y + dy, id: 0 }] });
  await finger("touchStart", 0);
  for (let dy = 5; dy <= 30; dy += 5) await finger("touchMove", dy);
  await finger("touchEnd", null);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(r)));
  const [bottom2, top2] = await labels(swclk);
  expect(bottom2).toBeGreaterThan(bottom);
  expect(top2).toBeGreaterThan(top);
  expect(await win(view)).toEqual(window);
  await context.close();
});

/** A press in SWCLK's row, over the traces, moved by `dx` and `dy` with the mouse. */
async function dragRow(page: Page, view: Locator, dx: number, dy: number): Promise<void> {
  const plot = (await view.locator(".plot").boundingBox())!;
  const row = (await view.locator(".scale").first().boundingBox())!;
  const [x, y] = [plot.x + plot.width / 2, row.y + row.height / 2];
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y + dy, { steps: 5 });
  await page.mouse.up();
}

test("zoomed in, a drag in an analog row moves along the capture and moves its values with the pointer", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  // In along the capture first: the whole of it has nowhere to move.
  await view.locator(".zoom-in").click();
  await swclk.focus();
  await page.keyboard.press("+");
  await page.keyboard.press("+");
  const [bottom, top] = await labels(swclk);
  const window = await win(view);
  // 24 px down is about a volt here, two steps of the labels: the trace follows, so both rise.
  await dragRow(page, view, -100, 24);
  const [bottom2, top2] = await labels(swclk);
  expect(bottom2).toBeGreaterThan(bottom);
  expect(top2).toBeGreaterThan(top);
  expect((await win(view)).start).toBeGreaterThan(window.start);
  await expect(view.locator(".readout")).toHaveText("");
});

test("showing its whole range, an analog row's drag moves along the capture alone", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  await view.locator(".zoom-in").click();
  const window = await win(view);
  await dragRow(page, view, -100, 24);
  expect(await labels(swclk)).toEqual([-2, 6]);
  expect((await win(view)).start).toBeGreaterThan(window.start);
});

test("on a touch screen, a finger's drag over a zoomed-in analog row moves along the capture alone", async ({ browser, browserName }) => {
  test.skip(browserName !== "chromium", "touch drags need Chromium's DevTools protocol");
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  await page.goto(VIEW);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator(".scale").first();
  // In along the capture first: the whole of it has nowhere to move.
  await view.locator(".zoom-in").click();
  await swclk.focus();
  await page.keyboard.press("+");
  await page.keyboard.press("+");
  const zoomed = await labels(swclk);
  const window = await win(view);
  const plot = (await view.locator(".plot").boundingBox())!;
  const row = (await swclk.boundingBox())!;
  const [x, y] = [plot.x + plot.width / 2, row.y + row.height / 2];
  const cdp = await context.newCDPSession(page);
  const finger = (type: "touchStart" | "touchMove" | "touchEnd", d: number | null) =>
    cdp.send("Input.dispatchTouchEvent", { type, touchPoints: d === null ? [] : [{ x: x - 4 * d, y: y + d, id: 0 }] });
  await finger("touchStart", 0);
  for (let d = 4; d <= 24; d += 4) await finger("touchMove", d);
  await finger("touchEnd", null);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(r)));
  expect(await labels(swclk)).toEqual(zoomed);
  expect((await win(view)).start).toBeGreaterThan(window.start);
  await context.close();
});
