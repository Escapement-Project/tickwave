// The dev page in Chromium and Firefox: screenshots for reading by eye, and the interactions.
// Expected figures are hand-derived from the export (see test/), never read back from the code.

import { test, expect, type Page, type Locator } from "@playwright/test";
import { existsSync, mkdirSync } from "node:fs";

const LOCAL = existsSync("fixtures-local/gcc-swd-reads.chronofx") && existsSync("fixtures-local/stock-reads.chronofx");
const VENDOR = existsSync("dev/vendor/tokens.css");

/** Records every chrono-ready by element id, from before the page's own scripts run. */
async function recordReady(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const w = window as unknown as { ready: Set<string>; errors: string[] };
    w.ready = new Set();
    w.errors = [];
    document.addEventListener("chrono-ready", (e) => w.ready.add((e.target as Element).id));
    document.addEventListener("chrono-error", (e) => w.errors.push(`${(e.target as Element).id}: ${(e as CustomEvent).detail.message}`));
  });
}

async function waitReady(page: Page, ids: string[]): Promise<void> {
  await page.waitForFunction((ids) => ids.every((id) => (window as unknown as { ready: Set<string> }).ready.has(id)), ids, { timeout: 30_000 });
}

const errors = (page: Page) => page.evaluate(() => (window as unknown as { errors: string[] }).errors);

/**
 * Loads the dev page. Unless `local`, the stress fixtures are refused: a test that does not need
 * them should not spend its time decoding twenty million edges.
 */
async function open(page: Page, opts: { reduced?: boolean; local?: boolean } = {}): Promise<void> {
  await recordReady(page);
  if (!opts.local) await page.route("**/fixtures-local/**", (r) => r.abort());
  if (opts.reduced) await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/dev/");
  await page.evaluate(() => document.fonts.ready);
}

const OPEN_VIEWS = LOCAL ? ["gcc", "stock", "energy", "slow"] : ["slow"];
const INSETS = LOCAL ? ["inset-dpidr", "inset-ours", "inset-stock"] : ["inset-dpidr"];

test.describe("screenshots", () => {
  test.skip(!LOCAL, "needs the local fixtures: node scripts/fixtures.ts");
  for (const face of ["day", "night"] as const) {
    for (const width of [1100, 360]) {
      test(`${face} at ${width} px`, async ({ page }, info) => {
        await page.setViewportSize({ width, height: 900 });
        await open(page, { reduced: true, local: true });
        await page.evaluate((face) => (document.documentElement.dataset.face = face), face);
        await waitReady(page, [...OPEN_VIEWS, ...INSETS]);
        await page.waitForTimeout(150);
        expect(await errors(page)).toEqual([]);
        mkdirSync("screenshots", { recursive: true });
        await page.screenshot({ path: `screenshots/${info.project.name}-${face}-${width}.png`, fullPage: true });
      });
    }
  }
});

/** The element's own plot box, for pointer coordinates. */
const plotOf = (view: Locator) => view.locator(".plot");

async function settle(view: Locator): Promise<void> {
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
}

test.describe("stress fixtures zoomed fully out and fully in", () => {
  test.skip(!LOCAL, "needs the local fixtures: node scripts/fixtures.ts");
  // An edge to zoom in on: the first DPIDR read's first SWCLK edge, and stock's first TCK edge.
  for (const [id, edge] of [["gcc", 378584398], ["stock", 379722028]] as const) {
    test(`${id}`, async ({ page }, info) => {
      await page.setViewportSize({ width: 1100, height: 900 });
      await open(page, { local: true });
      await waitReady(page, [id]);
      const view = page.locator(`#${id}`);
      await view.scrollIntoViewIfNeeded();
      await view.locator(".fit").click();
      await settle(view);
      mkdirSync("screenshots", { recursive: true });
      await view.screenshot({ path: `screenshots/${info.project.name}-${id}-out.png` });
      await view.evaluate((v, e) => (v as unknown as { setWindow(a: number, b: number): void }).setWindow(e - 1000, e + 1000), edge);
      await plotOf(view).focus();
      for (let i = 0; i < 20; i++) await page.keyboard.press("+");
      await settle(view);
      const w = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
      // Fully in, the view is 40 px a tick; the edge stays inside it.
      expect(w.end - w.start).toBeLessThan(40);
      expect(w.start).toBeLessThan(edge);
      expect(w.end).toBeGreaterThan(edge);
      await view.screenshot({ path: `screenshots/${info.project.name}-${id}-in.png` });
    });
  }
});

test("a collapsed view fetches nothing until a reference to it scrolls into view", async ({ page }) => {
  const fetched: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/fixtures/")) fetched.push(new URL(r.url()).pathname);
  });
  await page.goto("/e2e/pages/collapsed.html");
  const view = page.locator("#v");
  await expect(view.locator(".summary .title")).toHaveText("identify-swd.chronofx");
  await page.waitForTimeout(300);
  expect(fetched).toEqual([]);
  await page.locator("#far").scrollIntoViewIfNeeded();
  // The head and the sidecar arrive; the summary line then says what the recording is.
  await expect(view.locator(".summary .facts")).toHaveText("523.583100 ms · 2 channels: SWCLK, SWDIO · 3.6 KiB");
  expect(fetched).toContain("/fixtures/identify-swd.chronofx");
  expect(fetched).toContain("/fixtures/identify-swd.annotations.json");
});

test.describe("the identify view", () => {
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 900 });
    await open(page, { reduced: true });
    const view = page.locator("#identify");
    await view.locator(".open").click();
    await waitReady(page, ["identify"]);
    await view.scrollIntoViewIfNeeded();
  });

  test("a drag on the axis selects a span and counts every edge", async ({ page }) => {
    const view = page.locator("#identify");
    const box = (await plotOf(view).boundingBox())!;
    const selected = view.evaluate(
      (v) => new Promise((resolve) => v.addEventListener("chrono-select", (e) => resolve((e as CustomEvent).detail), { once: true })),
    );
    // Every edge is between 376.7 ms and 389.8 ms of 523.6 ms; drag across nearly the whole axis.
    const y = box.y + box.height - 10;
    await page.mouse.move(box.x + 70, y);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2, y, { steps: 4 });
    await page.mouse.move(box.x + box.width - 12, y, { steps: 4 });
    await page.mouse.up();
    const detail = (await selected) as { edges: Record<string, number>; periods: { count: number } };
    // (122348 - 60) / 8 and (15052 - 60) / 8: the export's own edge counts.
    expect(detail.edges).toEqual({ SWCLK: 15286, SWDIO: 1874 });
    // 7643 rising SWCLK edges, so 7642 periods.
    expect(detail.periods.count).toBe(7642);
    await expect(view.locator(".readout")).toContainText("15,286 edges");
    await expect(view.locator(".readout")).toContainText("1,874 edges");
    // For reading by eye: zoomed to the DPIDR read, a selection, two cursors, and the pointer on
    // SWDIO's row.
    await view.evaluate((v) => (v as unknown as { setWindow(a: number, b: number): void }).setWindow(377090000, 377145000));
    await settle(view);
    await page.mouse.move(box.x + 200, y);
    await page.mouse.down();
    await page.mouse.move(box.x + 600, y, { steps: 4 });
    await page.mouse.up();
    const row = box.y + box.height - 26 - 14;
    await page.mouse.click(box.x + 250, row);
    await page.mouse.click(box.x + 420, row);
    await page.mouse.move(box.x + 700, row);
    await settle(view);
    mkdirSync("screenshots", { recursive: true });
    await view.screenshot({ path: `screenshots/${test.info().project.name}-identify-readouts.png` });
  });

  test("two clicks place cursors A and B and read the interval", async ({ page }) => {
    const view = page.locator("#identify");
    const box = (await plotOf(view).boundingBox())!;
    // The last row's middle: above the 24 px axis, half a 28 px row up.
    const row = box.y + box.height - 26 - 14;
    await page.mouse.click(box.x + 300, row);
    await page.mouse.click(box.x + 500, row);
    const readout = view.locator(".readout");
    await expect(readout).toContainText("A ");
    await expect(readout).toContainText("B ");
    await expect(readout).toContainText("Δt ");
    await plotOf(view).focus();
    await page.keyboard.press("Escape");
    await expect(readout).toBeEmpty();
  });

  test("the wheel zooms about the pointer; keys pan and zoom", async ({ page }) => {
    const view = page.locator("#identify");
    const box = (await plotOf(view).boundingBox())!;
    // Pointer events carry whole pixels here; a fractional x would be a third of a pixel off, which
    // at the fit scale is a hundred thousand ticks.
    const x = Math.round(box.x + box.width * 0.7);
    const before = await view.evaluate((v, x) => (v as unknown as { tickAt(x: number): number }).tickAt(x), x);
    const window0 = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    const announced = view.evaluate(
      (v) => new Promise((resolve) => v.addEventListener("chrono-window", (e) => resolve((e as CustomEvent).detail), { once: true })),
    );
    await page.mouse.move(x, box.y + 60);
    await page.mouse.wheel(0, -400);
    await page.waitForTimeout(100);
    expect(await announced).toEqual(await view.evaluate((v) => (v as unknown as { window: unknown }).window));
    const after = await view.evaluate((v, x) => (v as unknown as { tickAt(x: number): number }).tickAt(x), x);
    const window1 = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    expect(window1.end - window1.start).toBeLessThan(window0.end - window0.start);
    expect(Math.abs(after - before)).toBeLessThanOrEqual(1);
    await plotOf(view).focus();
    await page.keyboard.press("ArrowRight");
    const window2 = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    expect(window2.start).toBeGreaterThan(window1.start);
    await page.keyboard.press("-");
    const window3 = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    expect(window3.end - window3.start).toBeGreaterThan(window2.end - window2.start);
    // A trackpad pinch arrives as a wheel with ctrlKey; it zooms the trace, not the page.
    await page.mouse.move(x, box.y + 60);
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, -100);
    await page.keyboard.up("Control");
    await page.waitForTimeout(100);
    const window4 = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    expect(window4.end - window4.start).toBeLessThan(window3.end - window3.start);
  });

  test("stepping through one kind reads n of N", async ({ page }) => {
    const view = page.locator("#identify");
    await view.locator(".kind").selectOption("wait");
    await view.locator(".next").click();
    // The decode found 16 WAIT acknowledges.
    await expect(view.locator(".steps .count")).toHaveText("1 of 16");
    await view.locator(".next").click();
    await expect(view.locator(".steps .count")).toHaveText("2 of 16");
  });
});

test("the inset's measure reads the rate from the edges", async ({ page }) => {
  await open(page, { reduced: true });
  await waitReady(page, ["inset-dpidr"]);
  // 46 rising SWCLK edges, 377095057 to 377140060: 45 periods over 45003 ticks, 999,933 Hz, give
  // or take 22 Hz, shown to 10 Hz.
  await expect(page.locator("#inset-dpidr").locator(".measure")).toHaveText("SWCLK 999.93 kHz mean of 45 periods");
});

test.describe("a reference", () => {
  test("hover opens an inset beside the text and moves nothing; Escape closes it", async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 900 });
    await open(page, { reduced: true });
    const ref = page.locator("chrono-ref[annotation=line-reset]");
    const figure = page.locator("figure").first();
    const before = await figure.boundingBox();
    await ref.hover();
    const pop = ref.locator(".pop");
    await expect(pop).toBeVisible();
    await expect(pop.locator("chrono-inset figcaption")).toHaveText("Line reset: SWDIO held high for 51 clocks");
    expect(await figure.boundingBox()).toEqual(before);
    // Beside the text: below the reference, starting near it.
    const r = (await ref.boundingBox())!;
    const p = (await pop.boundingBox())!;
    expect(p.y).toBeGreaterThan(r.y);
    expect(p.y).toBeLessThan(r.y + r.height + 40);
    await page.keyboard.press("Escape");
    await expect(pop).toBeHidden();
  });

  test("focus opens it, blur closes it, Enter opens the view at the annotation", async ({ page }) => {
    await open(page, { reduced: true });
    const ref = page.locator("chrono-ref[annotation=dpidr-ack]");
    await ref.focus();
    await expect(ref.locator(".pop")).toBeVisible();
    await page.keyboard.press("Enter");
    const view = page.locator("#identify");
    await expect(view).toHaveAttribute("open", "");
    await expect(ref.locator(".pop")).toBeHidden();
    // ack/377103058 runs to 377106058; the view shows it with room either side.
    const w = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    expect(w.start).toBeLessThan(377103058);
    expect(w.end).toBeGreaterThan(377106058);
    expect(w.end - w.start).toBeLessThan(10_000);
  });

  test("the sweep plays once per appearance, not on a quick reopen", async ({ page }) => {
    await open(page);
    const ref = page.locator("chrono-ref[annotation=dpidr-read]");
    // A sweep lasts 420 ms, shorter than a poll's gaps, so the page samples it every frame.
    const watch = () =>
      ref.evaluate((r) => {
        const w = window as unknown as { swept: boolean; watching: number };
        w.swept = false;
        cancelAnimationFrame(w.watching);
        const frame = () => {
          const inset = r.shadowRoot!.querySelector("chrono-inset") as unknown as { sweeping: boolean } | null;
          if (inset?.sweeping) w.swept = true;
          w.watching = requestAnimationFrame(frame);
        };
        frame();
      });
    const swept = () => page.evaluate(() => (window as unknown as { swept: boolean }).swept);
    await watch();
    await ref.hover();
    await expect.poll(swept, { timeout: 5000 }).toBe(true);
    await page.waitForTimeout(600);
    await page.mouse.move(0, 0);
    await expect(ref.locator(".pop")).toBeHidden();
    // Back within 800 ms: already drawn.
    await watch();
    await ref.hover();
    await page.waitForTimeout(600);
    expect(await swept()).toBe(false);
    await page.mouse.move(0, 0);
    await page.waitForTimeout(900);
    await watch();
    await ref.hover();
    await expect.poll(swept, { timeout: 5000 }).toBe(true);
  });
});

test("the canvas follows the face", async ({ page }) => {
  test.skip(!VENDOR, "needs dev/vendor: node scripts/vendor.ts");
  await open(page, { reduced: true });
  await page.evaluate(() => (document.documentElement.dataset.face = "day"));
  await waitReady(page, ["inset-dpidr"]);
  const count = async (face: "day" | "night") => {
    await page.evaluate((face) => (document.documentElement.dataset.face = face), face);
    await page.waitForTimeout(200);
    return page.locator("#inset-dpidr").evaluate((el) => {
      const hex = getComputedStyle(document.documentElement).getPropertyValue("--series-verge").trim();
      const want = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
      const canvas = el.shadowRoot!.querySelector("canvas")!;
      const data = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height).data;
      let n = 0;
      for (let i = 0; i < data.length; i += 4) {
        if (data[i + 3] === 255 && data[i] === want[0] && data[i + 1] === want[1] && data[i + 2] === want[2]) n++;
      }
      return { hex, n };
    });
  };
  const day = await count("day");
  const night = await count("night");
  expect(day.hex).not.toBe(night.hex);
  expect(day.n).toBeGreaterThan(100);
  expect(night.n).toBeGreaterThan(100);
});
