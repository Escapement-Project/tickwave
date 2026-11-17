// A zoom step eased, on e2e/pages/view.html with identify-swd.tickwave, or identify-jtag.tickwave
// where the rows read as the view zooms in: the window set at once and announced once, what the
// view draws easing there, the rows reading for where it goes, steps and pans during an ease, and
// reduced motion. Then the same in an inset, on e2e/pages/inset.html with identify-swd's attach
// sequence.

import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/";

type Span = { start: number; end: number };

async function ready(page: Page, file = "identify-swd.tickwave"): Promise<Locator> {
  await page.goto(VIEW + file);
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  // What a stretch of the plot box spans, settled, against the window: the box holds the names
  // column too, so the two differ by a factor the layout fixes.
  await view.evaluate((v) => {
    const plot = v.shadowRoot!.querySelector(".plot")!.getBoundingClientRect();
    const e = v as unknown as { e2eFactor: number; window: Span; tickAt(x: number): number };
    const across = (e.tickAt(plot.left + plot.width * 0.9) - e.tickAt(plot.left + plot.width * 0.4)) / 0.5;
    e.e2eFactor = across / (e.window.end - e.window.start);
  });
  await view.locator(".plot").focus();
  await noteKeys(page);
  return view;
}

const win = (view: Locator) => view.evaluate((v) => (v as unknown as { window: Span }).window);

/**
 * Starts recording, each frame for `ms`, the span the view draws (from `tickAt` across its plot)
 * and when, and the windows it announces; resolves to a reader of them.
 */
async function record(view: Locator, ms: number): Promise<() => Promise<{ drawn: [number, number][]; announced: [number, Span][] }>> {
  await view.evaluate((v, ms) => {
    const plot = v.shadowRoot!.querySelector(".plot")!.getBoundingClientRect();
    const at = (x: number) => (v as unknown as { tickAt(x: number): number }).tickAt(x);
    const factor = (v as unknown as { e2eFactor: number }).e2eFactor;
    const out = { drawn: [] as [number, number][], announced: [] as [number, Span][] };
    v.addEventListener("tickwave-window", (e) => out.announced.push([performance.now(), (e as CustomEvent).detail]));
    (v as unknown as { e2eRecord: Promise<typeof out> }).e2eRecord = new Promise((resolve) => {
      const t0 = performance.now();
      const frame = () => {
        // From well inside the plot, so the names column takes no part.
        const [a, b] = [plot.left + plot.width * 0.4, plot.left + plot.width * 0.9];
        out.drawn.push([performance.now(), (at(b) - at(a)) / 0.5 / factor]);
        if (performance.now() - t0 < ms) requestAnimationFrame(frame);
        else resolve(out);
      };
      requestAnimationFrame(frame);
    });
  }, ms);
  return () => view.evaluate((v) => (v as unknown as { e2eRecord: Promise<{ drawn: [number, number][]; announced: [number, Span][] }> }).e2eRecord);
}

/** The window's span the view draws now. */
const drawnNow = (view: Locator) =>
  view.evaluate((v) => {
    const plot = v.shadowRoot!.querySelector(".plot")!.getBoundingClientRect();
    const e = v as unknown as { e2eFactor: number; tickAt(x: number): number };
    return (e.tickAt(plot.left + plot.width * 0.9) - e.tickAt(plot.left + plot.width * 0.4)) / 0.5 / e.e2eFactor;
  });

const span = (w: Span) => w.end - w.start;
const near = (a: number, b: number) => Math.abs(a - b) <= Math.abs(b) * 1e-4;

/** Notes in the page when each key goes down, in its own clock; `keyAt` reads the last. */
const noteKeys = (page: Page) =>
  page.evaluate(() => window.addEventListener("keydown", () => ((window as unknown as { e2eKey: number }).e2eKey = performance.now()), true));
const keyAt = (page: Page) => page.evaluate(() => (window as unknown as { e2eKey: number }).e2eKey);

/**
 * Whether the frames drawn on the way from span `from` to `to`, `[time, span]`, ease out from `t0`
 * over 150 ms, whatever frames the browser drops. A frame shows a window drawn no earlier than the
 * frame before it, so each is at least as far along as that frame's time is of the 150 ms, which a
 * start slower than linear falls behind; and one is well ahead of it, which linear is not.
 */
function quickAtFirst(frames: [number, number][], t0: number, from: number, to: number): boolean {
  let ahead = false;
  for (let i = 1; i < frames.length; i++) {
    const s = frames[i][1];
    if (near(s, from) || near(s, to)) continue;
    const [along, time] = [(from - s) / (from - to), Math.min(1, (frames[i - 1][0] - t0) / 150)];
    if (along < time - 0.02) return false;
    ahead ||= along > time + 0.2;
  }
  return ahead;
}

test("a zoom step sets the window at once and announces it once; what the view draws eases there over about 150 ms, quick at first", async ({ page }) => {
  const view = await ready(page);
  const before = await win(view);
  const recorded = await record(view, 600);
  await page.keyboard.press("+");
  const to = await win(view);
  expect(span(to)).toBeCloseTo(span(before) / 1.6, 0);
  const { drawn, announced } = await recorded();
  expect(announced.map(([, w]) => w)).toEqual([to]);
  const spans = drawn.map(([, s]) => s);
  // Narrowing frame by frame, never past where it goes, and there in the end.
  for (let i = 1; i < spans.length; i++) expect(spans[i]).toBeLessThanOrEqual(spans[i - 1] * (1 + 1e-6));
  expect(near(spans[spans.length - 1], span(to))).toBe(true);
  const moving = drawn.filter(([, s]) => !near(s, span(before)) && !near(s, span(to)));
  expect(moving.length).toBeGreaterThanOrEqual(3);
  const [first, last] = [moving[0][0], moving[moving.length - 1][0]];
  expect(last - first).toBeGreaterThan(60);
  expect(last - first).toBeLessThan(250);
  expect(quickAtFirst(drawn, await keyAt(page), span(before), span(to))).toBe(true);
  // Settled is where the view was going, drawn there.
  await page.keyboard.press("+");
  const again = await win(view);
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  expect(near(await drawnNow(view), span(again))).toBe(true);
});

test("a step during an ease goes on from where the last one was going, and what the view draws never turns back", async ({ page }) => {
  const view = await ready(page);
  const before = await win(view);
  const recorded = await record(view, 700);
  await page.keyboard.press("+");
  await page.waitForTimeout(50);
  await page.keyboard.press("+");
  const to = await win(view);
  expect(span(to)).toBeCloseTo(span(before) / 1.6 / 1.6, 0);
  const spans = (await recorded()).drawn.map(([, s]) => s);
  for (let i = 1; i < spans.length; i++) expect(spans[i]).toBeLessThanOrEqual(spans[i - 1] * (1 + 1e-6));
  expect(near(spans[spans.length - 1], span(to))).toBe(true);
});

test("a pan during an ease moves on from where the ease was going, at once", async ({ page }) => {
  const view = await ready(page);
  await page.keyboard.press("+");
  const to = await win(view);
  const recorded = await record(view, 100);
  await page.keyboard.press("ArrowRight");
  const panned = await win(view);
  expect(panned.start).toBeCloseTo(to.start + span(to) * 0.125, 0);
  expect(span(panned)).toBeCloseTo(span(to), 0);
  const spans = (await recorded()).drawn.map(([, s]) => s);
  // From the first frame after the pan, the view draws where it went.
  expect(spans.slice(-3).every((s) => near(s, span(panned)))).toBe(true);
});

test("under reduced motion a zoom step jumps; the setting is read at each step", async ({ page }) => {
  const view = await ready(page);
  await page.emulateMedia({ reducedMotion: "reduce" });
  let before = await win(view);
  let recorded = await record(view, 200);
  await page.keyboard.press("+");
  let to = await win(view);
  let spans = (await recorded()).drawn.map(([, s]) => s);
  // Where it was, then where it went: nothing between.
  expect(spans.every((s) => near(s, span(before)) || near(s, span(to)))).toBe(true);
  expect(near(spans[spans.length - 1], span(to))).toBe(true);
  // Motion allowed again: the next step eases.
  await page.emulateMedia({ reducedMotion: "no-preference" });
  before = to;
  recorded = await record(view, 400);
  await page.keyboard.press("+");
  to = await win(view);
  spans = (await recorded()).drawn.map(([, s]) => s);
  expect(spans.some((s) => !near(s, span(before)) && !near(s, span(to)))).toBe(true);
});

test("the rows read for where a zoom step goes, as the step is taken, and never for a window on the way there", async ({ page }) => {
  const view = await ready(page, "identify-jtag.tickwave");
  type Posted = { op: string; start?: number; end?: number; at: number };
  await page.evaluate(() => {
    const w = window as unknown as { posted: Posted[] };
    w.posted = [];
    const post = Worker.prototype.postMessage;
    Worker.prototype.postMessage = function (this: Worker, m: unknown, ...rest: unknown[]) {
      const msg = m as { op?: string; requests?: Posted[] };
      for (const r of msg?.op === "batch" ? msg.requests! : [msg as Posted]) {
        if (r?.op === "edges" || r?.op === "overview" || r?.op === "samples") w.posted.push({ op: r.op, start: r.start, end: r.end, at: performance.now() });
      }
      return (post as (...a: unknown[]) => void).call(this, m, ...rest);
    };
  });
  // Three steps in from the whole recording, finer than the rows hold, so they read.
  const recorded = await record(view, 700);
  for (let i = 0; i < 3; i++) await page.keyboard.press("+");
  const { announced, drawn } = await recorded();
  const posted = await page.evaluate(() => (window as unknown as { posted: Posted[] }).posted);
  expect(posted.length).toBeGreaterThan(0);
  // A row reads where the view goes and a width either side.
  const reads = (w: Span) => [Math.floor(w.start - span(w)), Math.ceil(w.end + span(w))];
  const steps = announced.map(([, w]) => reads(w));
  for (const r of posted) {
    const ok = steps.some(([a, b]) => Math.abs(r.start! - a) <= 1 && Math.abs(r.end! - b) <= 1);
    expect(ok, `${r.op} ${r.start}..${r.end} against ${JSON.stringify(steps)}`).toBe(true);
  }
  // The read went out before the window drawn got there.
  const landed = drawn.find(([, s]) => near(s, span(announced[announced.length - 1][1])))![0];
  expect(posted[0].at).toBeLessThan(landed);
});

const INSET = "/e2e/pages/inset.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml&annotation=attach&channels=SWCLK%20SWDIO";

type Inset = { window: Span | null; zoomed: Span | null; settled(): Promise<void> };

/** A sidecar for identify-jtag.tickwave: one annotation over most of it. */
const MOST = [
  'format = "tickwave-annotations/1"',
  'recording = "identify-jtag.tickwave"',
  'recording_id = "blake3:a60f152e38195d3a541668cd17e104d5c690b2ff422d984440e9afa1ef0828a7"',
  "",
  "[[annotation]]",
  'id = "most"',
  'start = "20 ms"',
  'end = "600 ms"',
  'label = "Most of the recording"',
].join("\n");

async function insetReady(page: Page, url = INSET, label = /^Attaching/): Promise<Locator> {
  await page.goto(url);
  const inset = page.locator("#i");
  await expect(inset.locator("figcaption")).toHaveText(label, { timeout: 20_000 });
  await inset.evaluate((e) => (e as unknown as Inset).settled());
  await page.keyboard.press("Tab");
  await noteKeys(page);
  return inset;
}

/** The span an inset shows, and the span it is zoomed to. */
const shown = (inset: Locator) => inset.evaluate((e) => (e as unknown as Inset).window!);
const zoomed = (inset: Locator) => inset.evaluate((e) => (e as unknown as Inset).zoomed);

/** Starts recording, each frame for `ms`, the span the inset shows and when; resolves to a reader
 * of them. */
async function recordInset(inset: Locator, ms: number): Promise<() => Promise<[number, number][]>> {
  await inset.evaluate((e, ms) => {
    const spans: [number, number][] = [];
    (e as unknown as { e2eRecord: Promise<[number, number][]> }).e2eRecord = new Promise((resolve) => {
      const t0 = performance.now();
      const frame = () => {
        const w = (e as unknown as Inset).window!;
        spans.push([performance.now(), w.end - w.start]);
        if (performance.now() - t0 < ms) requestAnimationFrame(frame);
        else resolve(spans);
      };
      requestAnimationFrame(frame);
    });
  }, ms);
  return () => inset.evaluate((e) => (e as unknown as { e2eRecord: Promise<[number, number][]> }).e2eRecord);
}

test("an inset's zoom step sets the span it is zoomed to at once and eases the span shown there; a step during it goes on from there, and a press jumps there", async ({ page }) => {
  const inset = await insetReady(page);
  const whole = await shown(inset);
  let recorded = await recordInset(inset, 600);
  await page.keyboard.press("ArrowUp");
  const to = (await zoomed(inset))!;
  expect(span(to)).toBeCloseTo(span(whole) / 1.6, 0);
  const frames = await recorded();
  let spans = frames.map(([, s]) => s);
  for (let i = 1; i < spans.length; i++) expect(spans[i]).toBeLessThanOrEqual(spans[i - 1] * (1 + 1e-6));
  expect(near(spans[spans.length - 1], span(to))).toBe(true);
  const moving = spans.filter((s) => !near(s, span(whole)) && !near(s, span(to)));
  expect(moving.length).toBeGreaterThanOrEqual(3);
  expect(quickAtFirst(frames, await keyAt(page), span(whole), span(to))).toBe(true);
  // Two steps 50 ms apart: the second from where the first was going.
  recorded = await recordInset(inset, 600);
  await page.keyboard.press("ArrowUp");
  await page.waitForTimeout(50);
  await page.keyboard.press("ArrowUp");
  const twice = (await zoomed(inset))!;
  expect(span(twice)).toBeCloseTo(span(to) / 1.6 / 1.6, 0);
  // A move along the span during the ease goes from where it was going, by an eighth of that.
  await page.keyboard.press("ArrowRight");
  const moved = (await zoomed(inset))!;
  expect(moved.start - twice.start).toBeCloseTo(span(twice) / 8, 0);
  spans = (await recorded()).map(([, s]) => s);
  for (let i = 1; i < spans.length; i++) expect(spans[i]).toBeLessThanOrEqual(spans[i - 1] * (1 + 1e-6));
  expect(near(spans[spans.length - 1], span(twice))).toBe(true);
  // A press during an ease starts from where it was going.
  await page.keyboard.press("ArrowDown");
  const out = (await zoomed(inset))!;
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.5, b.y + 40);
  await page.mouse.down();
  const pressed = await shown(inset);
  await page.mouse.up();
  expect(near(span(pressed), span(out))).toBe(true);
  expect(pressed.start).toBeCloseTo(out.start, 0);
});

test("Ctrl with the wheel during an inset's ease zooms about the tick under the pointer in the span it is going to", async ({ page }) => {
  const inset = await insetReady(page);
  const whole = await shown(inset);
  const b = (await inset.locator("canvas").boundingBox())!;
  const [left, right] = [b.x + b.width * 0.3, b.x + b.width * 0.8];
  // A step about the left point, let land, gives that point's place across the plot.
  await page.mouse.move(left, b.y + 40);
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, -100);
  const first = (await zoomed(inset))!;
  await inset.evaluate((e) => (e as unknown as Inset).settled());
  const f = (first.start - whole.start) / (span(whole) - span(first));
  // A step about the right point, and during its ease one about the left again: the tick at the
  // left point where the second was going stays there.
  await page.mouse.move(right, b.y + 40);
  await page.mouse.wheel(0, -100);
  const second = (await zoomed(inset))!;
  await page.mouse.move(left, b.y + 40);
  await page.mouse.wheel(0, -100);
  await page.keyboard.up("Control");
  const third = (await zoomed(inset))!;
  expect(span(third)).toBeLessThan(span(second) * 0.9);
  expect(third.start + f * span(third)).toBeCloseTo(second.start + f * span(second), -1);
});

test("an inset laid out again during an ease, as when it is resized, goes on easing", async ({ page }) => {
  const inset = await insetReady(page);
  const whole = await shown(inset);
  const recorded = await recordInset(inset, 600);
  await page.keyboard.press("ArrowUp");
  await inset.evaluate((e) => ((e as HTMLElement).style.maxWidth = "420px"));
  const to = (await zoomed(inset))!;
  const spans = (await recorded()).map(([, s]) => s);
  expect(spans.filter((s) => !near(s, span(whole)) && !near(s, span(to))).length).toBeGreaterThanOrEqual(3);
  expect(near(spans[spans.length - 1], span(to))).toBe(true);
});

test("under reduced motion an inset's zoom step jumps; the setting is read at each step", async ({ page }) => {
  const inset = await insetReady(page);
  await page.emulateMedia({ reducedMotion: "reduce" });
  let before = await shown(inset);
  let recorded = await recordInset(inset, 200);
  await page.keyboard.press("ArrowUp");
  let to = (await zoomed(inset))!;
  let spans = (await recorded()).map(([, s]) => s);
  expect(spans.every((s) => near(s, span(before)) || near(s, span(to)))).toBe(true);
  expect(near(spans[spans.length - 1], span(to))).toBe(true);
  await page.emulateMedia({ reducedMotion: "no-preference" });
  before = to;
  recorded = await recordInset(inset, 400);
  await page.keyboard.press("ArrowUp");
  to = (await zoomed(inset))!;
  spans = (await recorded()).map(([, s]) => s);
  expect(spans.some((s) => !near(s, span(before)) && !near(s, span(to)))).toBe(true);
});

test("an inset's rows read for where a zoom step goes, as the step is taken, and never for a span on the way there", async ({ page }) => {
  // identify-jtag, whose rows read as an inset over most of it zooms in, with a sidecar of one
  // annotation over that span.
  await page.route("**/fixtures/jtag.annotations.toml", (route) => route.fulfill({ status: 200, contentType: "text/plain", body: MOST }));
  const inset = await insetReady(page, "/e2e/pages/inset.html?src=fixtures/identify-jtag.tickwave&annotations=fixtures/jtag.annotations.toml&annotation=most", /^Most/);
  type Posted = { op: string; start?: number; end?: number; at: number };
  await page.evaluate(() => {
    const w = window as unknown as { posted: Posted[] };
    w.posted = [];
    const post = Worker.prototype.postMessage;
    Worker.prototype.postMessage = function (this: Worker, m: unknown, ...rest: unknown[]) {
      const msg = m as { op?: string; requests?: Posted[] };
      for (const r of msg?.op === "batch" ? msg.requests! : [msg as Posted]) {
        if (r?.op === "edges" || r?.op === "overview" || r?.op === "samples") w.posted.push({ op: r.op, start: r.start, end: r.end, at: performance.now() });
      }
      return (post as (...a: unknown[]) => void).call(this, m, ...rest);
    };
  });
  const recorded = await recordInset(inset, 1200);
  const targets: Span[] = [];
  for (let i = 0; i < 6; i++) {
    await page.keyboard.press("ArrowUp");
    targets.push((await zoomed(inset))!);
  }
  const frames = await recorded();
  const spans = frames.map(([, s]) => s);
  const posted = await page.evaluate(() => (window as unknown as { posted: Posted[] }).posted);
  expect(posted.length).toBeGreaterThan(0);
  const reads = (w: Span) => [Math.floor(w.start - span(w)), Math.ceil(w.end + span(w))];
  for (const r of posted) {
    const ok = targets.some((w) => Math.abs(r.start! - reads(w)[0]) <= 1 && Math.abs(r.end! - reads(w)[1]) <= 1);
    expect(ok, `${r.op} ${r.start}..${r.end} against ${JSON.stringify(targets.map(reads))}`).toBe(true);
  }
  expect(near(spans[spans.length - 1], span(targets[targets.length - 1]))).toBe(true);
  // The first read went out before the span shown got where its step was going.
  const first = targets.find((w) => Math.abs(posted[0].start! - reads(w)[0]) <= 1)!;
  const landed = frames.find(([, s]) => near(s, span(first)))![0];
  expect(posted[0].at).toBeLessThan(landed);
});
