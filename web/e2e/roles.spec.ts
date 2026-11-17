// Paint roles and the stored view (ANNOTATIONS.md, "Roles", "The stored view") in the browser:
// fixtures/roles.annotations.toml over identify-swd declares probe and target as the SWD
// generator does, a third role with its own colours and a dash, and a view of SWDIO over SWCLK,
// 377.09 to 377.25 ms, in #d9480f by day. Its four hand-written annotations paint SWDIO's
// turnarounds; the generator's paint the requests (probe) and the acks and data (target).

import { test, expect, type Locator, type Page } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { layout, partRows } from "../test/layout.ts";

const ROLES = "fixtures/roles.annotations.toml";
const BUNDLE = "fixtures/roles.bundle.tickwave";

async function ready(view: Locator): Promise<void> {
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
}

/** Each swatch of a key: its label, and its line's colour, width and dash. */
function swatches(key: Locator) {
  return key.locator(":scope > span").evaluateAll((items) =>
    items.map((s) => {
      const line = s.querySelector("line")!;
      return { label: s.textContent, stroke: line.getAttribute("stroke"), width: line.getAttribute("stroke-width"), dash: line.getAttribute("stroke-dasharray") };
    }),
  );
}

const window_ = (view: Locator) => view.evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);
const facts = (view: Locator) => view.evaluate((v) => (v as unknown as { summary: { facts: string } | null }).summary?.facts ?? "");

test("a sidecar's stored view sets the channels, the span and the trace's colour where the page gives none, and its roles key in their order and looks", async ({ page }) => {
  await page.goto(`/e2e/pages/view.html?open&annotations=${ROLES}`);
  const view = page.locator("#v");
  await ready(view);
  // A sidecar the page gives applies its view when it lands.
  await expect.poll(() => facts(view)).toContain("2 channels: SWDIO, SWCLK");
  await expect.poll(() => window_(view)).toEqual({ start: 377_090_000, end: 377_250_000 });
  // In the recording's order, not by name: probe takes the trace's colour, the stored one; target
  // ink a pixel heavier; the third its own colour and dash, 2 and 1 line widths of 1.5 px.
  await expect.poll(() => swatches(view.locator(".key"))).toEqual([
    { label: "probe drives", stroke: "#d9480f", width: "1.5", dash: null },
    { label: "target drives", stroke: "#000000", width: "2.5", dash: null },
    { label: "neither drives", stroke: "#8a8a8a", width: "1.5", dash: "3 1.5" },
  ]);
});

test("the page's attributes and tokens win over the stored view and the declared looks, each on its own", async ({ page }) => {
  const style = encodeURIComponent("--series-verge: rgb(0, 128, 0); --tickwave-role-neither: rgb(0, 0, 255); --tickwave-role-neither-dash: none; --tickwave-role-target-weight: 3");
  await page.goto(`/e2e/pages/view.html?open&annotations=${ROLES}&channels=SWCLK&window=${encodeURIComponent("377.1ms 377.2ms")}&series=verge&style=${style}`);
  const view = page.locator("#v");
  await ready(view);
  expect(await facts(view)).toContain("1 of 2 channels: SWCLK");
  expect(await window_(view)).toEqual({ start: 377_100_000, end: 377_200_000 });
  // The key is the recording's, not the window's: SWCLK alone is painted by none of them, and the
  // key lists every role the sidecar declares, in the page's looks.
  await expect.poll(() => swatches(view.locator(".key"))).toEqual([
    { label: "probe drives", stroke: "#008000", width: "1.5", dash: null },
    { label: "target drives", stroke: "#000000", width: "4.5", dash: null },
    { label: "neither drives", stroke: "#0000ff", width: "1.5", dash: null },
  ]);
  // A series the page doesn't set leaves the stored colour.
  await page.goto(`/e2e/pages/view.html?open&annotations=${ROLES}&series=unset`);
  await ready(view);
  await expect.poll(async () => (await swatches(view.locator(".key")))[0]?.stroke).toBe("#d9480f");
});

test("a collapsed view's facts count the stored view's channels", async ({ page }) => {
  await page.goto(`/e2e/pages/view.html?annotations=${ROLES}`);
  const view = page.locator("#v");
  await expect.poll(() => facts(view), { timeout: 20_000 }).toContain("2 channels: SWDIO, SWCLK");
});

/** How many separate runs of columns hold a pixel near `rgb` in the plot's canvas. */
function runsOf(page: Page, rgb: [number, number, number]): Promise<number> {
  return page.locator("#v").evaluate((v, rgb) => {
    const canvas = v.shadowRoot!.querySelector("canvas")!;
    const { data, width, height } = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height);
    let runs = 0;
    let inRun = false;
    for (let x = 0; x < width; x++) {
      let hit = false;
      for (let y = 0; y < height && !hit; y++) {
        const o = 4 * (y * width + x);
        hit = Math.abs(data[o] - rgb[0]) < 40 && Math.abs(data[o + 1] - rgb[1]) < 40 && Math.abs(data[o + 2] - rgb[2]) < 40 && data[o + 3] > 200;
      }
      if (hit && !inRun) runs++;
      inRun = hit;
    }
    return runs;
  }, rgb);
}

test("a third role's lines draw dashed, and a solid role's in one piece", async ({ page }) => {
  // The read's data turnaround, 377.13906 to 377.14056 ms, in green so nothing else matches.
  const style = encodeURIComponent("--tickwave-role-neither: rgb(0, 200, 0); --tickwave-role-target: rgb(0, 0, 230)");
  await page.goto(`/e2e/pages/view.html?open&annotations=${ROLES}&window=${encodeURIComponent("377.1388ms 377.1408ms")}&style=${style}`);
  const view = page.locator("#v");
  await ready(view);
  expect(await runsOf(page, [0, 200, 0])).toBeGreaterThanOrEqual(5);
  // Without the dash, the same span in one stretch of columns.
  await page.goto(`/e2e/pages/view.html?open&annotations=${ROLES}&window=${encodeURIComponent("377.1388ms 377.1408ms")}&style=${encodeURIComponent(`--tickwave-role-neither: rgb(0, 200, 0); --tickwave-role-neither-dash: none`)}`);
  await ready(view);
  expect(await runsOf(page, [0, 200, 0])).toBe(1);
});

test("an inset's key lists the roles its span paints in the recording's order, labelled, each swatch in its look", async ({ page }) => {
  await page.goto(`/e2e/pages/inset.html?src=fixtures/identify-swd.tickwave&annotations=${ROLES}&annotation=swd/read/377094557`);
  const key = page.locator("#i .key");
  // Not by name, which would put neither first.
  await expect.poll(() => swatches(key), { timeout: 20_000 }).toEqual([
    { label: "probe drives", stroke: "#d9480f", width: "1.5", dash: null },
    { label: "target drives", stroke: "#000000", width: "2.5", dash: null },
    { label: "neither drives", stroke: "#8a8a8a", width: "1.5", dash: "3 1.5" },
  ]);
});

test("a downloaded bundle opened with no attributes shows the stored view and the roles' looks before any part is read", async ({ page }) => {
  test.skip(!existsSync(BUNDLE), "needs target/release/tickwave: scripts/generate.ts bundles with it");
  const file = new Uint8Array(readFileSync(BUNDLE));
  const t = layout(file);
  const parts = partRows(file, t.parts!).map((p) => [p.from, p.to] as const);
  expect(parts.length).toBeGreaterThan(0);
  // Every read of a part's frame waits until released.
  let release!: () => void;
  const released = new Promise<void>((r) => (release = r));
  let held = 0;
  await page.route("**/roles.bundle.tickwave*", async (route) => {
    const r = /^bytes=(\d+)-(\d+)$/.exec(route.request().headers()["range"] ?? "");
    if (r && parts.some(([from, to]) => Number(r[1]) < to && from <= Number(r[2]))) {
      held++;
      await released;
    }
    await route.continue();
  });
  await page.goto(`/e2e/pages/view.html?open&firstRead=256&src=${BUNDLE}`);
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  expect(await facts(view)).toContain("2 channels: SWDIO, SWCLK");
  expect(await window_(view)).toEqual({ start: 377_090_000, end: 377_250_000 });
  // Every role the bundle declares keys from the first frame, in its look.
  const all = [
    { label: "probe drives", stroke: "#d9480f", width: "1.5", dash: null },
    { label: "target drives", stroke: "#000000", width: "2.5", dash: null },
    { label: "neither drives", stroke: "#8a8a8a", width: "1.5", dash: "3 1.5" },
  ];
  await expect.poll(() => swatches(view.locator(".key"))).toEqual(all);
  await mark(view.locator(".key"));
  expect(held).toBeGreaterThan(0);
  release();
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  // The trace in the stored colour. Not looked for while the parts are held: a read of a block
  // can share a request with a part's, and be held with it.
  expect(await runsOf(page, [217, 72, 15])).toBeGreaterThan(0);
  // The parts read change nothing in the key: the same entries, not built again.
  expect(await swatches(view.locator(".key"))).toEqual(all);
  expect(await marked(view.locator(".key"))).toEqual([true, true, true]);
});

/** Marks each entry of a key, so `marked` tells one kept from one built again. */
const mark = (key: Locator) => key.locator(":scope > span").evaluateAll((items) => items.forEach((s) => ((s as unknown as { e2eMark: boolean }).e2eMark = true)));
const marked = (key: Locator) => key.locator(":scope > span").evaluateAll((items) => items.map((s) => (s as unknown as { e2eMark?: boolean }).e2eMark === true));

test("the view's key is the recording's: a pan, a zoom, a drag or a wheel builds it again in no frame", async ({ page }) => {
  await page.goto(`/e2e/pages/view.html?open&annotations=${ROLES}`);
  const view = page.locator("#v");
  await ready(view);
  const key = view.locator(".key");
  await expect.poll(async () => (await swatches(key)).length).toBe(3);
  const before = await swatches(key);
  await mark(key);
  // Counts the key's entries built from here on.
  await key.evaluate((k) => {
    const w = window as unknown as { e2eBuilt: number };
    w.e2eBuilt = 0;
    new MutationObserver((records) => (w.e2eBuilt += records.reduce((n, r) => n + r.addedNodes.length, 0))).observe(k, { childList: true });
  });
  const plot = view.locator(".plot");
  await plot.focus();
  for (const k of ["ArrowRight", "ArrowRight", "ArrowLeft", "+", "-", "Home"]) await page.keyboard.press(k);
  const b = (await plot.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 3, b.y + b.height / 2, { steps: 5 });
  await page.mouse.up();
  await page.mouse.wheel(200, 0);
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  expect(await page.evaluate(() => (window as unknown as { e2eBuilt: number }).e2eBuilt)).toBe(0);
  expect(await marked(key)).toEqual([true, true, true]);
  expect(await swatches(key)).toEqual(before);
  // A palette that changes builds it again, once: the page's ink, which target takes.
  await page.evaluate(() => document.documentElement.style.setProperty("--ink", "rgb(10, 10, 10)"));
  await expect.poll(async () => (await swatches(key))[1]?.stroke).toBe("#0a0a0a");
  expect(await page.evaluate(() => (window as unknown as { e2eBuilt: number }).e2eBuilt)).toBe(3);
});

/** The sidecar the in-memory page reads, as JSON: a probe painting SWDIO from 377.01 to 377.09 ms,
 * green; a later paint over its middle and one on its own, from 377.15 to 377.22 ms, in role
 * "stray", declared, in blue, only where `declared` says. Each where SWDIO is quiet, so each column
 * holds the line, not a shade. */
function strayJson(declared: boolean): string {
  const role = [{ name: "probe", colour: "#00c800" }, ...(declared ? [{ name: "stray", colour: "#0000e6" }] : [])];
  const span = (id: string, start: string, end: string, paint: string) => ({ id, start, end, channels: ["SWDIO"], paint });
  return JSON.stringify({
    role,
    annotation: [span("a", "377.01 ms", "377.09 ms", "probe"), span("b", "377.03 ms", "377.05 ms", "stray"), span("c", "377.15 ms", "377.22 ms", "stray")],
  });
}

test("a paint naming a role its sidecar doesn't declare draws as none and keys nothing, where one declared draws its look", async ({ page }) => {
  // Not checked: the in-memory route's loader reads a sidecar as it stands, as a reader meets a
  // bundle another tool wrote.
  let declared = false;
  await page.route("**/fixtures/stray.json", (route) => route.fulfill({ status: 200, contentType: "application/json", body: strayJson(declared) }));
  const url = `/e2e/pages/memory.html?annotations=fixtures/stray.json&window=${encodeURIComponent("377.0ms 377.25ms")}`;
  for (declared of [false, true]) {
    await page.goto(url);
    const view = page.locator("#v");
    await ready(view);
    const key = view.locator(".key");
    // Green: probe; blue: stray, where it is declared.
    const [green, blue] = [await runsOf(page, [0, 200, 0]), await runsOf(page, [0, 0, 230])];
    if (!declared) {
      // probe shows through the later paint, and the paint alone is the trace's: one green run,
      // and no blue; the key names probe alone.
      expect([green, blue]).toEqual([1, 0]);
      await expect.poll(async () => (await swatches(key)).map((s) => s.label)).toEqual(["probe"]);
    } else {
      // Declared, it draws over probe's middle and on its own.
      expect([green, blue]).toEqual([2, 2]);
      await expect.poll(async () => (await swatches(key)).map((s) => s.label)).toEqual(["probe", "stray"]);
    }
  }
});

/** Along pixel row `y` of the plot's canvas, from column `from` to `to`: the runs of columns
 * holding a pixel near `rgb`, and the gaps between them, in device pixels, in order, and the first
 * such column. */
function runsAlong(page: Page, rgb: [number, number, number], y: number, from: number, to: number): Promise<{ runs: number[]; gaps: number[]; first: number }> {
  return page.locator("#v").evaluate(
    (v, [rgb, y, from, to]) => {
      const canvas = v.shadowRoot!.querySelector("canvas")!;
      const { data } = canvas.getContext("2d")!.getImageData(0, y as number, canvas.width, 1);
      const [r, g, b] = rgb as number[];
      const runs: number[] = [];
      const gaps: number[] = [];
      let n = 0;
      let gap = 0;
      let first = -1;
      for (let x = from as number; x < (to as number); x++) {
        const o = 4 * x;
        const hit = Math.abs(data[o] - r) < 40 && Math.abs(data[o + 1] - g) < 40 && Math.abs(data[o + 2] - b) < 40 && data[o + 3] > 200;
        if (hit && first < 0) first = x;
        if (hit) {
          if (n === 0 && runs.length > 0) gaps.push(gap);
          n++;
          gap = 0;
        } else {
          if (n > 0) runs.push(n);
          n = 0;
          gap++;
        }
      }
      if (n > 0) runs.push(n);
      return { runs, gaps, first };
    },
    [rgb, y, from, to] as const,
  );
}

test("a dashed role's dash runs on along each stretch from where it starts, across the paths its line is stroked in", async ({ page }) => {
  // SWDIO is idle high from the capture's start to the first attach at 376.7 ms. A dash of 2 and
  // 2 line widths over three stretches of a 100 to 300 ms window, each starting at its own place
  // in the line's 256-column paths: the first some 290 columns long, past a path's end.
  const spans = [
    ["150 ms", "200 ms"],
    ["207.3 ms", "245 ms"],
    ["251.7 ms", "290 ms"],
  ];
  const toml = [
    'format = "tickwave-annotations/1"',
    'recording_id = "blake3:63773d56fb3420735c864932f8d2441acd6f31a51b557e073bf27755a0534697"',
    "",
    "[[role]]",
    'name = "probe"',
    "dash = [2, 2]",
    ...spans.flatMap(([start, end], i) => ["", "[[annotation]]", `id = "quiet-${i}"`, `start = "${start}"`, `end = "${end}"`, 'channels = ["SWDIO"]', 'paint = "probe"']),
  ].join("\n");
  await page.route("**/fixtures/dash.annotations.toml", (route) => route.fulfill({ status: 200, contentType: "text/plain", body: toml }));
  const style = encodeURIComponent("--tickwave-role-probe: rgb(0, 200, 0)");
  await page.goto(`/e2e/pages/view.html?open&annotations=fixtures/dash.annotations.toml&channels=SWDIO&window=${encodeURIComponent("100ms 300ms")}&style=${style}`);
  const view = page.locator("#v");
  await ready(view);
  // The row of pixels the high level's line is drawn along: the one with the most green.
  const y = await view.evaluate((v) => {
    const canvas = v.shadowRoot!.querySelector("canvas")!;
    const { data, width, height } = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height);
    let best = [0, -1];
    for (let y = 0; y < height; y++) {
      let n = 0;
      for (let x = 0; x < width; x++) if (data[4 * (y * width + x) + 1] > 160 && data[4 * (y * width + x)] < 60) n++;
      if (n > best[0]) best = [n, y];
    }
    return best[1];
  });
  const dpr = await page.evaluate(() => devicePixelRatio);
  const lw = Math.max(1, Math.round(1.5 * dpr));
  // A time's column on the canvas, as the plot rounds it.
  const column = (ms: number) =>
    view.evaluate(
      (v, [ms, dpr]) => {
        const canvas = v.shadowRoot!.querySelector("canvas")!.getBoundingClientRect();
        const e = v as unknown as { tickAt(x: number): number };
        const [x0, x1] = [canvas.left + 100, canvas.left + 600];
        const [t0, t1] = [e.tickAt(x0), e.tickAt(x1)];
        return Math.round((x0 + ((ms * 1e6 - t0) * (x1 - x0)) / (t1 - t0) - canvas.left) * dpr);
      },
      [ms, dpr],
    );
  const firsts: number[] = [];
  for (const [start, end] of spans) {
    const [from, to] = [await column(parseFloat(start)), await column(parseFloat(end))];
    const { runs, gaps, first } = await runsAlong(page, [0, 200, 0], y, from, to);
    expect(runs.length, start).toBeGreaterThan(20);
    // The first dash starts at the stretch's first column: the pattern starts where the stretch
    // does, not wherever the line's own start puts it.
    expect(first, start).toBe(from);
    // Every dash and gap but the last, which the stretch's end cuts: 2 line widths each.
    expect(new Set(runs.slice(0, -1)), start).toEqual(new Set([2 * lw]));
    expect(new Set(gaps), start).toEqual(new Set([2 * lw]));
    firsts.push(from);
  }
  // Not all at one place in the pattern from the line's start, so a pattern started there would
  // put at least one stretch's first dash elsewhere.
  expect(new Set(firsts.map((x) => (x - firsts[0]) % (4 * lw))).size).toBeGreaterThan(1);
});
