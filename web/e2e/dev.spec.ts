// The dev page in Chromium and Firefox: screenshots for reading by eye, and the interactions.
// Expected figures are hand-derived from the export (see test/), never read back from the code.

import { test, expect, type Page, type Locator } from "@playwright/test";
import { execFileSync } from "node:child_process";
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

/** The requests the e2e server logged for the recording URLs holding `tag`. */
async function served(tag: string): Promise<{ port: number; url: string; range: string | null; status: number }[]> {
  const log = (await (await fetch(`http://127.0.0.1:8124/__log?tag=${tag}`)).json()) as { port: number; url: string; range: string | null; status: number }[];
  return log.filter((e) => new URL(e.url, "http://x").pathname.endsWith(".chrono"));
}

const tagged = (info: { project: { name: string } }, what: string) => `${what}-${info.project.name}-${Date.now()}`;

test("a collapsed view fetches its head, and only its head, once it scrolls into view", async ({ page }, info) => {
  const tag = tagged(info, "collapsed");
  // A small first read, so the head (1,742 bytes, the converter says) and the block are separate.
  await page.goto(`/e2e/pages/view.html?below&firstRead=512&tag=${tag}`);
  const view = page.locator("#v");
  await expect(view.locator(".summary .title")).toHaveText("identify-swd.chrono");
  await page.waitForTimeout(300);
  expect(await served(tag)).toEqual([]);
  await view.scrollIntoViewIfNeeded();
  await expect(view.locator(".summary .facts")).toHaveText("524\u00a0ms \u00b7 2 channels: SWCLK, SWDIO \u00b7 4.7\u00a0KiB");
  await page.waitForTimeout(300);
  expect((await served(tag)).map((e) => [e.range, e.status])).toEqual([
    ["bytes=0-511", 206],
    ["bytes=512-1741", 206],
  ]);
  // Opened at the whole capture, it draws from the head's overview: still no block.
  await view.locator(".open").click();
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  expect((await served(tag)).map((e) => e.range)).toEqual(["bytes=0-511", "bytes=512-1741"]);
  // Zoomed to the first read, it needs edges, and reads the block by its range: 1,742 to 4,828.
  await view.evaluate((v) => {
    const view = v as unknown as { setWindow(a: number, b: number): void; settled(): Promise<void> };
    view.setWindow(377094000, 377141000);
    return view.settled();
  });
  expect((await served(tag)).map((e) => e.range)).toEqual(["bytes=0-511", "bytes=512-1741", "bytes=1742-4827"]);
});

test("a server that ignores Range: one whole-file response serves the view", async ({ page }, info) => {
  const tag = tagged(info, "norange");
  await recordReady(page);
  await page.goto(`http://127.0.0.1:8125/e2e/pages/view.html?open&firstRead=512&tag=${tag}`);
  const view = page.locator("#v");
  await waitReady(page, ["v"]);
  const box = (await plotOf(view).boundingBox())!;
  const y = box.y + box.height - 10;
  const selected = view.evaluate(
    (v) => new Promise((resolve) => v.addEventListener("chrono-select", (e) => resolve((e as CustomEvent).detail), { once: true })),
  );
  await page.mouse.move(box.x + 70, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width - 12, y, { steps: 6 });
  await page.mouse.up();
  expect(((await selected) as { edges: Record<string, number> }).edges).toEqual({ SWCLK: 15286, SWDIO: 1874 });
  expect((await served(tag)).map((e) => [e.port, e.status])).toEqual([[8125, 200]]);
});

test("a finer overview level comes from overview blocks by range, not from the data", async ({ page }, info) => {
  const tag = tagged(info, "levels");
  await recordReady(page);
  // The JTAG identify capture: head bytes 0 to 1,251, its data block to 10,378, its three overview
  // blocks of 2^12 ticks to 13,678 (the converter's report).
  await page.goto(`/e2e/pages/view.html?open&src=fixtures/identify-jtag.chrono&firstRead=1024&tag=${tag}`);
  await waitReady(page, ["v"]);
  const view = page.locator("#v");
  // At the whole capture a column is about 400,000 ticks: the head's 2^16 level draws it.
  expect((await served(tag)).map((e) => e.range)).toEqual(["bytes=0-1023", "bytes=1024-1250"]);
  // 20 ms over the view's columns is about 13,000 ticks a column: the 2^12 level, from blocks.
  // 465 to 485 ms is the capture's busiest stretch.
  await view.evaluate((v) => {
    const view = v as unknown as { setWindow(a: number, b: number): void; settled(): Promise<void> };
    view.setWindow(465e6, 485e6);
    return view.settled();
  });
  const ranges = (await served(tag)).map((e) => e.range!);
  expect(ranges.slice(0, 2)).toEqual(["bytes=0-1023", "bytes=1024-1250"]);
  expect(ranges.length).toBe(3);
  const [, from, to] = /^bytes=(\d+)-(\d+)$/.exec(ranges[2])!.map(Number);
  expect(from).toBeGreaterThanOrEqual(10378);
  expect(to).toBeLessThan(13678);
  mkdirSync("screenshots", { recursive: true });
  await view.screenshot({ path: `screenshots/${info.project.name}-overview-blocks.png` });
});

test("analog through the WASM path: the head's overview, the samples, and readouts to one code step", async ({ page }, info) => {
  const tag = tagged(info, "analog");
  await page.setViewportSize({ width: 700, height: 600 });
  await recordReady(page);
  // swd50-cut: 50,000 samples a channel at 1 GS/s in an 11 ms capture; its head is 1,152 bytes.
  await page.goto(`/e2e/pages/view.html?open&src=fixtures/swd50-cut.chrono&firstRead=1024&tag=${tag}`);
  await waitReady(page, ["v"]);
  const view = page.locator("#v");
  // At the whole capture a column is several thousand ticks: the head's 2^12 level draws the
  // analog rows, and no block is read.
  expect((await served(tag)).map((e) => e.range)).toEqual(["bytes=0-1023", "bytes=1024-1151"]);
  const drawn = await view.evaluate((v) => {
    const canvas = v.shadowRoot!.querySelector("canvas")!;
    const data = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, Math.round(canvas.height / 2)).data;
    let n = 0;
    for (let i = 3; i < data.length; i += 4) if (data[i] > 200) n++;
    return n;
  });
  expect(drawn).toBeGreaterThan(200);
  // Zoomed to 100 samples, it reads the data block by range, and draws a line through them.
  const win = (a: number, b: number) =>
    view.evaluate(
      (v, [a, b]) => {
        const view = v as unknown as { setWindow(a: number, b: number): void; settled(): Promise<void> };
        view.setWindow(a, b);
        return view.settled();
      },
      [a, b],
    );
  await win(1000, 1100);
  const ranges = (await served(tag)).map((e) => e.range!);
  expect(ranges.length).toBe(3);
  expect(ranges[2]).toMatch(/^bytes=1152-\d+$/);
  // Cursors at ticks 1020 and 1060, on the SWCLK row.
  const box = (await plotOf(view).boundingBox())!;
  const tickAt = (x: number) => view.evaluate((v, x) => (v as unknown as { tickAt(x: number): number }).tickAt(x), x);
  const t0 = await tickAt(box.x + 100);
  const t1 = await tickAt(box.x + 600);
  const xOf = (t: number) => Math.round(box.x + 100 + ((t - t0) * 500) / (t1 - t0));
  await page.mouse.click(xOf(1020), box.y + 40);
  await page.mouse.click(xOf(1060), box.y + 40);
  // From the export's own f32 volts, rounded to the millivolt: one code step is 2.49 mV.
  const readout = view.locator(".readout");
  await expect(readout.locator(".line").first().locator(":scope > span").first()).toHaveText("A 1.020\u00a0\u00b5s");
  const values = (name: string) => readout.locator(".line", { hasText: name }).locator(":scope > span");
  await expect(values("SWCLK")).toHaveText(["A 3.269\u00a0V", "B 3.267\u00a0V", "\u0394 \u22122\u00a0mV"]);
  await expect(values("SWDIO")).toHaveText(["A 3.289\u00a0V", "B 3.284\u00a0V", "\u0394 \u22125\u00a0mV"]);
  mkdirSync("screenshots", { recursive: true });
  await view.screenshot({ path: `screenshots/${info.project.name}-analog-samples.png` });
  await win(0, 50000);
  await view.screenshot({ path: `screenshots/${info.project.name}-analog-50us.png` });
});

test("an inset's duration: first edge to last of the edges in [start, end)", async ({ page }) => {
  // Expected values from the edges, read by a process of the test's own through the decoder.
  const edges = (ch: number, s: number, e: number) =>
    JSON.parse(execFileSync(process.execPath, ["scripts/edges.ts", "fixtures/identify-swd.chrono", String(ch), String(s), String(e)], { encoding: "utf8" })) as { ticks: number[] };
  const ms = (ticks: number[]) => `${((ticks[ticks.length - 1] - ticks[0]) / 1000).toFixed(3)}\u00a0\u00b5s`;
  for (const [id, s, e, want] of [
    // The line reset: SWCLK from its first fall to the 51st rise.
    ["line-reset", 376719436, 376774581, "54.644\u00a0\u00b5s"],
    // The same clock, ending exactly on that 51st rise, which is left out: the last edge is the
    // fall at 376773579.
    ["reset-clock", 376719436, 376774080, "54.143\u00a0\u00b5s"],
  ] as const) {
    expect(ms(edges(0, s, e).ticks)).toBe(want);
    await recordReady(page);
    await page.goto(`/e2e/pages/inset.html?src=fixtures/identify-swd.chrono&annotations=fixtures/identify-swd.annotations.toml&annotation=${id}`);
    await waitReady(page, ["i"]);
    await expect(page.locator("#i").locator(".measure")).toHaveText(`SWCLK ${want} first edge to last`);
  }
});

test("a local file reads through slice", async ({ page }) => {
  await recordReady(page);
  await page.goto("/e2e/pages/file.html");
  await page.locator("#pick").setInputFiles("fixtures/identify-swd.4k.chrono");
  const view = page.locator("#v");
  await waitReady(page, ["v"]);
  await expect(view.locator(".toolbar .title")).toHaveText("identify-swd.4k.chrono");
  const w = await view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
  expect(w).toEqual({ start: 0, end: 523583100 });
});

test("a bundled sidecar serves when the view names none", async ({ page }) => {
  test.skip(!existsSync("fixtures/identify-swd.bundle.chrono"), "needs target/release/chrono: scripts/generate.ts bundles with it");
  await recordReady(page);
  await page.goto("/e2e/pages/view.html?open&src=fixtures/identify-swd.bundle.chrono");
  const view = page.locator("#v");
  await waitReady(page, ["v"]);
  await view.evaluate((v) => (v as unknown as { show(id: string): Promise<void> }).show("first-wait"));
  // first-wait names swd/wait/379065159, a 3000-tick acknowledge: shown with a quarter either side.
  expect(await view.evaluate((v) => (v as unknown as { window: unknown }).window)).toEqual({ start: 379064409, end: 379068909 });
  await expect(view.locator(".steps .count")).toHaveText("1 of 16");
  await expect(view.locator(".notice")).toBeHidden();
});

test("a sidecar's problems show in the element and the console; the trace still draws", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "warning") warnings.push(m.text());
  });
  // The sidecar names identify-swd.chrono; this file is identify-swd.4k.chrono.
  await recordReady(page);
  await page.goto("/e2e/pages/view.html?open&src=fixtures/identify-swd.4k.chrono&annotations=fixtures/identify-swd.annotations.toml");
  const view = page.locator("#v");
  await waitReady(page, ["v"]);
  const notice = view.locator(".notice");
  await expect(notice).toBeVisible();
  await expect(notice).toContainText('names "identify-swd.chrono", not "identify-swd.4k.chrono"');
  expect(warnings.some((w) => w.includes('not "identify-swd.4k.chrono"'))).toBe(true);
  expect(await view.evaluate((v) => (v as unknown as { window: unknown }).window)).toEqual({ start: 0, end: 523583100 });
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
    // The axis labels stay readable over a selection: in the labels' band, within the selection
    // (200 to 600) and clear of cursor B's flag (420 to about 436), almost no pixel is ink. A fill
    // behind them would make nearly every pixel ink.
    const inked = await view.evaluate((v) => {
      const canvas = v.shadowRoot!.querySelector("canvas")!;
      const dpr = canvas.width / canvas.clientWidth;
      const hex = getComputedStyle(document.documentElement).getPropertyValue("--ink").trim();
      const ink = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
      const y0 = Math.round((canvas.clientHeight - 18) * dpr);
      const data = canvas.getContext("2d")!.getImageData(Math.round(445 * dpr), y0, Math.round(145 * dpr), Math.round(14 * dpr)).data;
      let n = 0;
      for (let i = 0; i < data.length; i += 4) if (data[i] === ink[0] && data[i + 1] === ink[1] && data[i + 2] === ink[2] && data[i + 3] > 200) n++;
      return n / (data.length / 4);
    });
    expect(inked).toBeLessThan(0.05);
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

  test("Ctrl and the wheel zoom about the pointer; Shift pans; the plain wheel scrolls the page", async ({ page }) => {
    const view = page.locator("#identify");
    const box = (await plotOf(view).boundingBox())!;
    // Pointer events carry whole pixels here; a fractional x would be a third of a pixel off, which
    // at the fit scale is a hundred thousand ticks.
    const x = Math.round(box.x + box.width * 0.7);
    const at = (x: number) => view.evaluate((v, x) => (v as unknown as { tickAt(x: number): number }).tickAt(x), x);
    const win = () => view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
    const before = await at(x);
    const window0 = await win();
    // Bounded, so a zoom that never happens fails here instead of at the test's timeout.
    const announced = view.evaluate(
      (v) =>
        new Promise((resolve, reject) => {
          v.addEventListener("chrono-window", (e) => resolve((e as CustomEvent).detail), { once: true });
          setTimeout(() => reject(new Error("no chrono-window within 5 s")), 5000);
        }),
    );
    await page.mouse.move(x, box.y + 60);
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, -400);
    await page.keyboard.up("Control");
    await page.waitForTimeout(100);
    expect(await announced).toEqual(await win());
    const window1 = await win();
    expect(window1.end - window1.start).toBeLessThan(window0.end - window0.start);
    expect(Math.abs((await at(x)) - before)).toBeLessThanOrEqual(1);
    // Shift and the wheel pan.
    await page.keyboard.down("Shift");
    await page.mouse.wheel(0, 200);
    await page.keyboard.up("Shift");
    await page.waitForTimeout(100);
    const window2 = await win();
    expect(window2.start).toBeGreaterThan(window1.start);
    expect(window2.end - window2.start).toBeCloseTo(window1.end - window1.start, 3);
    // The plain wheel scrolls the page, leaves the trace, and says how to zoom.
    const scrolled = await page.evaluate(() => scrollY);
    await page.mouse.wheel(0, 200);
    await page.waitForTimeout(200);
    expect(await page.evaluate(() => scrollY)).toBeGreaterThan(scrolled);
    expect(await win()).toEqual(window2);
    await expect(view.locator(".hint")).toHaveText(/^(Ctrl|Cmd) \+ scroll to zoom$/);
    await expect(view.locator(".hint")).toHaveCSS("opacity", "1");
    // The keys still pan and zoom.
    await plotOf(view).focus();
    await page.keyboard.press("ArrowRight");
    const window3 = await win();
    expect(window3.start).toBeGreaterThan(window2.start);
    await page.keyboard.press("-");
    const window4 = await win();
    expect(window4.end - window4.start).toBeGreaterThan(window3.end - window3.start);
  });

  test("stepping through one kind reads n of N", async ({ page }) => {
    const view = page.locator("#identify");
    // Opened at no annotation, stepping takes the first annotation's kind: the TOML lists
    // first-wait first.
    await expect(view.locator(".kind")).toHaveValue("wait");
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
  // The paint roles' key, as the landing page words it.
  await expect(page.locator("#inset-dpidr").locator(".key > span")).toHaveText(["probe drives", "target drives"]);
});

test.describe("a reference", () => {
  test("hover opens an inset beside the text and moves nothing; Escape closes it", async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 900 });
    await open(page, { reduced: true });
    const ref = page.locator("chrono-ref[annotation=line-reset]");
    const figure = page.locator("figure").first();
    // Figure 1 grows when its annotation's text arrives; measure it after that.
    await waitReady(page, ["inset-dpidr"]);
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
    // Stepping takes the kind of the annotation the view opened at.
    await expect(view.locator(".kind")).toHaveValue("ack");
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
