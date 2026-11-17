// A zoom step eased, on e2e/pages/view.html with identify-swd.tickwave, or identify-jtag.tickwave
// where the rows read as the view zooms in: the window set at once and announced once, what the
// view draws easing there, the rows reading for where it goes, steps and pans during an ease, and
// reduced motion. Then the same in an inset, on e2e/pages/inset.html with identify-swd's attach
// sequence.

import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/";

type Span = { start: number; end: number };

/**
 * Draws the plot under `box` a few times with the pointer, then takes the pointer off it. A page's
 * first draws run its code cold, and a step after three draws longer than a frame jumps (plot.ts
 * `slow`) where these tests look for an ease.
 */
async function warm(page: Page, box: Locator): Promise<void> {
  const b = (await box.boundingBox())!;
  const frames = () => page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  for (let i = 0; i < 8; i++) {
    await page.mouse.move(b.x + b.width * (0.3 + 0.05 * i), b.y + b.height / 2);
    await frames();
  }
  await page.mouse.move(0, 0);
  await frames();
}

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
  await warm(page, view.locator(".plot"));
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

test("an arrow key during a zoom step's ease pans from where the step was going, and the view glides there from the window drawn", async ({ page }) => {
  const view = await ready(page);
  await learnMiddle(view);
  const before = await win(view);
  await page.keyboard.press("+");
  const to = await win(view);
  const recorded = await record(view, 400);
  const at = await recordAt(view, 400);
  await page.keyboard.press("ArrowRight");
  const panned = await win(view);
  expect(panned.start).toBeCloseTo(to.start + span(to) * 0.125, 0);
  expect(span(panned)).toBeCloseTo(span(to), 0);
  const spans = (await recorded()).drawn.map(([, s]) => s);
  // The zoom goes on narrowing to the step's span, never back, and lands there.
  for (let i = 1; i < spans.length; i++) expect(spans[i]).toBeLessThanOrEqual(spans[i - 1] * (1 + 1e-6));
  expect(near(spans[spans.length - 1], span(panned))).toBe(true);
  expect(span(before)).toBeGreaterThan(span(panned));
  // What the middle of the plot shows moves on to the panned window's, by way of frames between.
  const ticks = (await at()).map(([, t]) => t);
  expect(near(ticks[ticks.length - 1], middleOf(panned))).toBe(true);
  expect(ticks.some((t) => !near(t, ticks[0]) && !near(t, middleOf(panned)))).toBe(true);
});

/** Starts recording, each frame for `ms`, the tick the view draws at the middle of its plot, and
 * when; resolves to a reader of them. A pan moves it by what it pans. */
async function recordAt(view: Locator, ms: number): Promise<() => Promise<[number, number][]>> {
  await view.evaluate((v, ms) => {
    const plot = v.shadowRoot!.querySelector(".plot")!.getBoundingClientRect();
    const e = v as unknown as { tickAt(x: number): number; e2eAt: Promise<[number, number][]> };
    const out: [number, number][] = [];
    e.e2eAt = new Promise((resolve) => {
      const t0 = performance.now();
      const frame = () => {
        out.push([performance.now(), e.tickAt(plot.left + plot.width * 0.6)]);
        if (performance.now() - t0 < ms) requestAnimationFrame(frame);
        else resolve(out);
      };
      requestAnimationFrame(frame);
    });
  }, ms);
  return () => view.evaluate((v) => (v as unknown as { e2eAt: Promise<[number, number][]> }).e2eAt);
}

/** The tick at the middle of the plot box (its names column included) once the view draws `w`. */
let middleOf: (w: Span) => number = () => NaN;

/** Learns `middleOf` from the view as it stands, settled: the plot box's 0.6 against the window. */
async function learnMiddle(view: Locator): Promise<void> {
  const [w, t] = await view.evaluate((v) => {
    const plot = v.shadowRoot!.querySelector(".plot")!.getBoundingClientRect();
    const e = v as unknown as { window: Span; tickAt(x: number): number };
    return [e.window, e.tickAt(plot.left + plot.width * 0.6)] as const;
  });
  const f = (t - w.start) / span(w);
  middleOf = (x: Span) => x.start + f * span(x);
}

test("an arrow key's pan sets the window at once and announces it once; what the view draws glides there over about 150 ms, quick at first", async ({ page }) => {
  const view = await ready(page);
  await page.keyboard.press("+");
  await page.keyboard.press("+");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await learnMiddle(view);
  for (const [key, share] of [
    ["ArrowRight", 0.125],
    ["ArrowLeft", -0.125],
    ["Shift+ArrowRight", 0.5],
  ] as const) {
    const before = await win(view);
    const recorded = await record(view, 500);
    const at = await recordAt(view, 500);
    await page.keyboard.press(key);
    const to = await win(view);
    expect(to.start - before.start).toBeCloseTo(span(before) * share, 0);
    expect(span(to)).toBeCloseTo(span(before), 0);
    const { announced } = await recorded();
    expect(announced.map(([, w]) => w)).toEqual([to]);
    const frames = await at();
    const ticks = frames.map(([, t]) => t);
    // Moving one way frame by frame, never past where it goes, and there in the end.
    const dir = Math.sign(share);
    for (let i = 1; i < ticks.length; i++) expect((ticks[i] - ticks[i - 1]) * dir).toBeGreaterThanOrEqual(-1e-6 * span(to));
    expect(near(ticks[ticks.length - 1], middleOf(to))).toBe(true);
    const moving = frames.filter(([, t]) => !near(t, middleOf(before)) && !near(t, middleOf(to)));
    expect(moving.length, key).toBeGreaterThanOrEqual(3);
    expect(moving[moving.length - 1][0] - moving[0][0]).toBeLessThan(250);
    expect(quickAtFirst(frames, await keyAt(page), middleOf(before), middleOf(to)), key).toBe(true);
    await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  }
});

test("a held arrow key glides on, never back and never stopping, and lands where the last press goes", async ({ page }) => {
  const view = await ready(page);
  // Zoomed in about the middle far enough that ten presses stay inside the capture.
  for (let i = 0; i < 4; i++) await page.keyboard.press("+");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await learnMiddle(view);
  const before = await win(view);
  const at = await recordAt(view, 900);
  // A held key: a keydown every 33 ms, as a keyboard's repeat sends, ten of them, sent in the page
  // so their times are the page's.
  const presses = 10;
  await view.evaluate(
    (v, presses) =>
      new Promise<void>((resolve) => {
        const plot = v.shadowRoot!.querySelector(".plot")!;
        let n = 0;
        const press = () => {
          (window as unknown as { e2eKey: number }).e2eKey = performance.now();
          plot.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", repeat: n > 0, bubbles: true, composed: true }));
          if (++n < presses) setTimeout(press, 33);
          else resolve();
        };
        press();
      }),
    presses,
  );
  const to = await win(view);
  expect(to.start - before.start).toBeCloseTo(span(before) * 0.125 * presses, 0);
  const frames = await at();
  const ticks = frames.map(([, t]) => t);
  const step = span(before) * 0.125;
  // In presses' pans from where it started, frame by frame.
  const along = ticks.map((t) => (t - ticks[0]) / step);
  for (let i = 1; i < along.length; i++) expect(along[i], `frame ${i}`).toBeGreaterThanOrEqual(along[i - 1] - 1e-6);
  expect(near(ticks[ticks.length - 1], middleOf(to))).toBe(true);
  // Glides: frames between whole presses' pans, not a jump a press.
  expect(along.filter((x) => Math.abs(x - Math.round(x)) > 0.03).length).toBeGreaterThanOrEqual(3);
  // While the key is held it keeps moving: over any two frames, a tenth of a press's pan at
  // least. Two, since a browser under load can draw a frame late.
  const lastKey = await keyAt(page);
  const first = along.findIndex((x) => x > 0.03);
  const held = frames.map((_, i) => i).filter((i) => i >= first + 2 && frames[i][0] <= lastKey);
  expect(held.length).toBeGreaterThanOrEqual(8);
  for (const i of held) expect(along[i] - along[i - 2], `frames ${i - 2} to ${i}`).toBeGreaterThan(0.1);
});

test("a second press before the view has drawn the first glides from the window drawn, not from where the first was going", async ({ page }) => {
  const view = await ready(page);
  for (let i = 0; i < 4; i++) await page.keyboard.press("+");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await learnMiddle(view);
  const before = await win(view);
  const step = span(before) * 0.125;
  // Two presses in one task, so the view draws nothing between them; then, in the frame the view
  // next draws, after its draw, the tick it draws at the middle and how long after the presses.
  const [drawn, ms] = await view.evaluate(
    (v) =>
      new Promise<[number, number]>((resolve) => {
        const plot = v.shadowRoot!.querySelector(".plot")!;
        const box = plot.getBoundingClientRect();
        const press = () => plot.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, composed: true }));
        const t0 = performance.now();
        press();
        press();
        // Asked for after the view's own frame, which the first press asked for: it runs after
        // that frame's draw.
        requestAnimationFrame(() => resolve([(v as unknown as { tickAt(x: number): number }).tickAt(box.left + box.width * 0.6), performance.now() - t0]));
      }),
  );
  expect(ms).toBeLessThan(40);
  const moved = (drawn - middleOf(before)) / step;
  // From where it was drawn, two presses' pans away, the first frame's ease, quick at first, has
  // covered 1 - (1 - t / 150 ms)^3 of them: under a press's pan for any frame within 40 ms. From
  // where the first press was going, it would be past a whole press's pan already.
  const k = Math.min(1, ms / 150);
  expect(moved).toBeLessThanOrEqual(2 * (1 - (1 - k) ** 3) + 0.02);
  expect(moved).toBeLessThan(1);
  expect(near((await win(view)).start, before.start + 2 * step)).toBe(true);
});

test("under reduced motion an arrow key's pan jumps; so do a drag and Shift with the wheel always", async ({ page }) => {
  const view = await ready(page);
  await page.keyboard.press("+");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await learnMiddle(view);
  const jumps = async (act: () => Promise<void>) => {
    const before = await win(view);
    const at = await recordAt(view, 300);
    await act();
    const to = await win(view);
    expect(near(middleOf(to), middleOf(before))).toBe(false);
    const ticks = (await at()).map(([, t]) => t);
    // Where it was, then where it went: nothing between.
    expect(ticks.every((t) => near(t, middleOf(before)) || near(t, middleOf(to)))).toBe(true);
    expect(near(ticks[ticks.length - 1], middleOf(to))).toBe(true);
    await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  };
  await page.emulateMedia({ reducedMotion: "reduce" });
  await jumps(() => page.keyboard.press("ArrowRight"));
  await page.emulateMedia({ reducedMotion: "no-preference" });
  const b = (await view.locator(".plot").boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await jumps(async () => {
    await page.keyboard.down("Shift");
    await page.mouse.wheel(0, 120);
    await page.keyboard.up("Shift");
  });
  await jumps(async () => {
    await page.mouse.move(b.x + b.width * 0.6, b.y + b.height / 2);
    await page.mouse.down();
    await page.mouse.move(b.x + b.width * 0.4, b.y + b.height / 2);
    await page.mouse.up();
  });
});

test("a page that sets the window an arrow key's glide is going to leaves it gliding; another window ends it there", async ({ page }) => {
  const view = await ready(page);
  await page.keyboard.press("+");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  await learnMiddle(view);
  let before = await win(view);
  let at = await recordAt(view, 400);
  await page.keyboard.press("ArrowRight");
  let to = await win(view);
  await view.evaluate((v, w) => (v as unknown as { setWindow(a: number, b: number): void }).setWindow(w.start, w.end), to);
  let ticks = (await at()).map(([, t]) => t);
  expect(ticks.filter((t) => !near(t, middleOf(before)) && !near(t, middleOf(to))).length).toBeGreaterThanOrEqual(3);
  expect(near(ticks[ticks.length - 1], middleOf(to))).toBe(true);
  // Another window, set during a glide: drawn at once.
  before = to;
  at = await recordAt(view, 400);
  await page.keyboard.press("ArrowRight");
  const other = { start: before.start - span(before) / 4, end: before.end - span(before) / 4 };
  await view.evaluate((v, w) => (v as unknown as { setWindow(a: number, b: number): void }).setWindow(w.start, w.end), other);
  to = await win(view);
  expect(to).toEqual(other);
  const frames = (await at()).map(([, t]) => t);
  // At most the frame the key's glide drew before the page's window came, then the page's.
  expect(frames.filter((t) => !near(t, middleOf(before)) && !near(t, middleOf(other))).length).toBeLessThanOrEqual(1);
  expect(near(frames[frames.length - 1], middleOf(other))).toBe(true);
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

test("a step after three draws longer than a frame at 60 Hz jumps, an ease whose frames overrun ends there, and a step after a draw inside such a frame eases, as after draws longer than a frame at 240 Hz", async ({ page }) => {
  const view = await ready(page);
  // Every draw clears the canvas first: a wait there of `ms` makes each draw take that long, 20 ms
  // past a frame at 60 Hz, 8 ms past one at 240 Hz but inside one at 60. `e2eSlow` counts the
  // draws made slow.
  const slow = (ms: number) =>
    page.evaluate((ms) => {
      const c = CanvasRenderingContext2D.prototype as unknown as { clearRect: (...a: number[]) => void; e2eClear?: (...a: number[]) => void };
      const w = window as unknown as { e2eSlow: number };
      c.e2eClear ??= c.clearRect;
      const clear = c.e2eClear;
      w.e2eSlow = 0;
      c.clearRect =
        ms > 0
          ? function (this: CanvasRenderingContext2D, ...a: number[]) {
              w.e2eSlow++;
              const t = performance.now();
              while (performance.now() - t < ms);
              clear.apply(this, a);
            }
          : clear;
    }, ms);
  // The frames drawn between where a key's step or pan starts and where it goes: by the span
  // drawn, or for a pan by the tick drawn at a place on the plot.
  const step = async (key: string, by: "span" | "position" = "span") => {
    if (by === "position") await learnMiddle(view);
    const w0 = await win(view);
    const recorded = await record(view, 400);
    const at = await recordAt(view, 400);
    await page.keyboard.press(key);
    const w1 = await win(view);
    const [before, to, drawn] =
      by === "span" ? [span(w0), span(w1), (await recorded()).drawn.map(([, s]) => s)] : [middleOf(w0), middleOf(w1), (await at()).map(([, t]) => t)];
    expect(near(drawn[drawn.length - 1], to)).toBe(true);
    return drawn.filter((s) => !near(s, before) && !near(s, to)).length;
  };
  // The last draws inside a frame: the step eases, its first three frames overrun, the ease ends.
  await slow(20);
  expect(await step("+")).toBeLessThanOrEqual(3);
  // The last three draws over a frame: the next step jumps.
  expect(await step("+")).toBe(0);
  // Fast again: the step after the slow draws jumps, and draws inside a frame; the next eases.
  await slow(0);
  expect(await step("-")).toBe(0);
  expect(await step("-")).toBeGreaterThanOrEqual(3);
  // The last three draws over a frame again: an arrow key's pan jumps as a step does.
  await slow(20);
  await step("+");
  await step("-");
  expect(await step("ArrowRight", "position")).toBe(0);
  await slow(0);
  await step("+");
  expect(await step("ArrowLeft", "position")).toBeGreaterThanOrEqual(3);
  // Draws past a frame at 240 Hz but inside one at 60: a step eases, and so does the next.
  await slow(8);
  expect(await step("+")).toBeGreaterThanOrEqual(3);
  expect(await step("-")).toBeGreaterThanOrEqual(3);
  expect(await page.evaluate(() => (window as unknown as { e2eSlow: number }).e2eSlow)).toBeGreaterThanOrEqual(6);
  await slow(0);
  // One draw over a frame, and then two, as a pointer moving from row to row draws: the next step
  // still eases.
  const box = (await view.locator(".plot").boundingBox())!;
  for (const ys of [[0.25], [0.75, 0.25]]) {
    await slow(20);
    for (const y of ys) {
      await page.mouse.move(box.x + box.width / 2, box.y + box.height * y);
      await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    }
    expect(await page.evaluate(() => (window as unknown as { e2eSlow: number }).e2eSlow)).toBe(ys.length);
    await slow(0);
    expect(await step(ys.length === 1 ? "+" : "-")).toBeGreaterThanOrEqual(3);
  }
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

test("a zoom step's first read goes out as the key goes down, before the next frame", async ({ page }) => {
  const view = await ready(page, "identify-jtag.tickwave");
  // Each read posted, and whether a frame has run since the last key: the page's frame, asked for
  // as the key goes down, comes before the view's.
  await page.evaluate(() => {
    const w = window as unknown as { posted: boolean[]; e2eFramed: boolean };
    w.posted = [];
    w.e2eFramed = true;
    window.addEventListener(
      "keydown",
      () => {
        w.e2eFramed = false;
        requestAnimationFrame(() => (w.e2eFramed = true));
      },
      true,
    );
    const post = Worker.prototype.postMessage;
    Worker.prototype.postMessage = function (this: Worker, m: unknown, ...rest: unknown[]) {
      const msg = m as { op?: string; requests?: { op?: string }[] };
      for (const r of msg?.op === "batch" ? msg.requests! : [msg]) {
        if (r?.op === "edges" || r?.op === "overview" || r?.op === "samples") w.posted.push(w.e2eFramed);
      }
      return (post as (...a: unknown[]) => void).call(this, m, ...rest);
    };
  });
  // Steps in from the whole recording, each settled before the next: those finer than the rows
  // hold read.
  let reads = 0;
  for (let i = 0; i < 6; i++) {
    await page.evaluate(() => ((window as unknown as { posted: boolean[] }).posted = []));
    await page.keyboard.press("+");
    await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
    const posted = await page.evaluate(() => (window as unknown as { posted: boolean[] }).posted);
    if (posted.length === 0) continue;
    reads++;
    expect(posted[0], `step ${i + 1}'s first read went out after a frame`).toBe(false);
  }
  // Not vacuous: one at least did.
  expect(reads).toBeGreaterThan(0);
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
  await warm(page, inset.locator("canvas"));
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

test("a note stepped to during an inset's ease comes into the span the inset is going to, at that span's width", async ({ page }) => {
  // identify-swd's DPIDR read: zoomed in twice about the middle, the request at the span's start is
  // out of where the inset is going; stepping to it at once moves that span to it.
  const inset = await insetReady(page, INSET.replace("annotation=attach", "annotation=dpidr-read"), /^Reading/);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  const to = (await zoomed(inset))!;
  const drawn = await shown(inset);
  expect(span(drawn)).toBeGreaterThan(span(to) * 1.05);
  await page.keyboard.press("Shift+ArrowRight");
  const stepped = (await zoomed(inset))!;
  const [start, end] = [377_094_557, 377_094_557 + 8001];
  expect(stepped.start).toBeLessThanOrEqual(start);
  expect(stepped.end).toBeGreaterThanOrEqual(end);
  expect(span(stepped)).toBeCloseTo(Math.max(span(to), (end - start) * 1.25), 0);
});

test("an inset laid out again during an ease to its span's edge goes on easing", async ({ page }) => {
  // A step about the window's left side goes to a span held at the window's start; the channels,
  // written another way, lay the inset out again while it eases there.
  const inset = await insetReady(page);
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.08, b.y + 40);
  const recorded = await recordInset(inset, 600);
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, -200);
  await page.keyboard.up("Control");
  const to = (await zoomed(inset))!;
  await inset.evaluate((e) => e.setAttribute("channels", "SWCLK  SWDIO"));
  const whole = (await recorded()).map(([, s]) => s);
  expect(whole.filter((s) => !near(s, whole[0]) && !near(s, span(to))).length).toBeGreaterThanOrEqual(3);
  expect(near(whole[whole.length - 1], span(to))).toBe(true);
});

/** Starts recording, each frame for `ms`, the span an inset shows; resolves to a reader of them,
 * each with its time. */
async function recordInsetWindows(inset: Locator, ms: number): Promise<() => Promise<[number, Span][]>> {
  await inset.evaluate((e, ms) => {
    const out: [number, Span][] = [];
    (e as unknown as { e2eWindows: Promise<[number, Span][]> }).e2eWindows = new Promise((resolve) => {
      const t0 = performance.now();
      const frame = () => {
        out.push([performance.now(), { ...(e as unknown as Inset).window! }]);
        if (performance.now() - t0 < ms) requestAnimationFrame(frame);
        else resolve(out);
      };
      requestAnimationFrame(frame);
    });
  }, ms);
  return () => inset.evaluate((e) => (e as unknown as { e2eWindows: Promise<[number, Span][]> }).e2eWindows);
}

test("an inset's arrow keys glide along its span from where it is drawn, never out of it, and jump under reduced motion", async ({ page }) => {
  const inset = await insetReady(page);
  const whole = await shown(inset);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await inset.evaluate((e) => (e as unknown as Inset).settled());
  // Ticks apart by less than a thousandth of the span shown: an inset's span is short, so its
  // ticks' own size would make a whole pan look like none.
  const zoomedSpan = span(await shown(inset));
  const at = (a: number, b: number) => Math.abs(a - b) <= zoomedSpan * 1e-3;
  // Right, then left, each an eighth of the span shown, eased there.
  for (const [key, dir] of [
    ["ArrowRight", 1],
    ["ArrowLeft", -1],
  ] as const) {
    const before = await shown(inset);
    const recorded = await recordInsetWindows(inset, 500);
    await page.keyboard.press(key);
    const to = (await zoomed(inset))!;
    expect(to.start - before.start).toBeCloseTo((dir * span(before)) / 8, 0);
    const frames = await recorded();
    const starts = frames.map(([, w]) => w.start);
    for (let i = 1; i < starts.length; i++) expect((starts[i] - starts[i - 1]) * dir).toBeGreaterThanOrEqual(-1e-6 * span(to));
    expect(at(starts[starts.length - 1], to.start)).toBe(true);
    expect(starts.filter((s) => !at(s, before.start) && !at(s, to.start)).length, key).toBeGreaterThanOrEqual(3);
    for (const [, w] of frames) expect(at(span(w), span(to))).toBe(true);
    await inset.evaluate((e) => (e as unknown as Inset).settled());
  }
  // Pressed on past the span's end, it glides to the end and no further: every window drawn on
  // the way lies inside the span.
  const recorded = await recordInsetWindows(inset, 900);
  for (let i = 0; i < 30; i++) await page.keyboard.press("ArrowRight");
  const frames = await recorded();
  expect(at((await zoomed(inset))!.end, whole.end)).toBe(true);
  for (const [, w] of frames) {
    expect(w.start).toBeGreaterThanOrEqual(whole.start - 1e-6 * span(whole));
    expect(w.end).toBeLessThanOrEqual(whole.end + 1e-6 * span(whole));
  }
  // Under reduced motion, a press jumps there.
  await inset.evaluate((e) => (e as unknown as Inset).settled());
  await page.emulateMedia({ reducedMotion: "reduce" });
  const before = await shown(inset);
  const jumped = await recordInsetWindows(inset, 300);
  await page.keyboard.press("ArrowLeft");
  const to = (await zoomed(inset))!;
  const starts = (await jumped()).map(([, w]) => w.start);
  expect(at(to.start, before.start)).toBe(false);
  expect(starts.every((s) => at(s, before.start) || at(s, to.start))).toBe(true);
  expect(at(starts[starts.length - 1], to.start)).toBe(true);
});
