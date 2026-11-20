// What a reader can do with an inset: point at a note or a pulse to read it, zoom in within its span,
// step through the notes by keys, and open the recording at a note. On e2e/pages/inset.html, the
// DPIDR read of identify-swd.tickwave, and through a reference on e2e/pages/view.html.

import { test, expect, type Locator, type Page } from "@playwright/test";

const INSET = "/e2e/pages/inset.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml&annotation=dpidr-read&channels=SWCLK%20SWDIO";
const ACK = ["ack OK", "3.000 µs", "target drives"];
const REQUEST = ["request 0xA5", "8.001 µs", "probe drives"];

async function drawn(page: Page, query = ""): Promise<Locator> {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(INSET + query);
  const inset = page.locator("#i");
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  await inset.evaluate((e) => (e as HTMLElement & { settled(): Promise<void> }).settled());
  return inset;
}

/** The reading's items as shown, or null while the line shows the measure. */
const reading = (inset: Locator) =>
  inset.evaluate((e) => {
    const r = e.shadowRoot!.querySelector(".line.reads .reading");
    return r ? [...r.children].map((c) => c.textContent!.replace(/ /g, " ")) : null;
  });

/** The span the inset shows, in ticks. */
const win = (inset: Locator) => inset.evaluate((e) => (e as HTMLElement & { window: { start: number; end: number } | null }).window!);
const span = (w: { start: number; end: number }) => w.end - w.start;

/** Ctrl and the wheel, by `dy`. */
async function ctrlWheel(page: Page, dy: number): Promise<void> {
  await page.keyboard.down("Control");
  await page.mouse.wheel(0, dy);
  await page.keyboard.up("Control");
}

/** The canvas's box, and a point in it as fractions of its width and pixels from its top. */
async function at(inset: Locator, fx: number, y: number): Promise<{ x: number; y: number }> {
  const b = (await inset.locator("canvas").boundingBox())!;
  return { x: b.x + b.width * fx, y: b.y + y };
}

/** A point on the notes' lane, at fraction `fx` of the canvas's width. */
const onLane = (inset: Locator, fx: number) => at(inset, fx, 10);

/** Moves the pointer along the notes' lane, and returns each distinct reading on the way. */
async function sweepLane(page: Page, inset: Locator): Promise<string[][]> {
  const seen: string[][] = [];
  for (let fx = 0.15; fx < 0.98; fx += 0.01) {
    const p = await at(inset, fx, 10);
    await page.mouse.move(p.x, p.y);
    const r = await reading(inset);
    if (r && !seen.some((s) => s.join() === r.join())) seen.push(r);
  }
  return seen;
}

test("pointing at a note lights it and reads its label, length and driver in place of the measure, and moves nothing", async ({ page }) => {
  const inset = await drawn(page);
  const height = (await inset.boundingBox())!.height;
  const seen = await sweepLane(page, inset);
  // The ack's label does not fit over its note: pointing at it names it.
  expect(seen).toContainEqual(REQUEST);
  expect(seen).toContainEqual(ACK);
  expect((await inset.boundingBox())!.height).toBe(height);
  await page.mouse.move(0, 0);
  expect(await reading(inset)).toBeNull();
  await expect(inset.locator(".measure")).toBeVisible();
});

test("pointing at a trace reads the pulse there: its level and width, then where it starts in the span, as many items as fit", async ({ page }) => {
  const inset = await drawn(page);
  const height = (await inset.boundingBox())!.height;
  // SWCLK's row, in the middle of the data phase, where it runs at 1 MHz.
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.6, b.y + 40);
  await expect.poll(() => reading(inset)).not.toBeNull();
  const r = (await reading(inset))!;
  expect(r[0]).toMatch(/^SWCLK (high|low) 500 ns$/);
  expect(r[1]).toMatch(/^starts \d+\.\d+ µs in$/);
  // Whole items only, on one line.
  expect(await inset.evaluate((e) => { const l = e.shadowRoot!.querySelector(".line")!; return l.scrollWidth <= l.clientWidth; })).toBe(true);
  expect((await inset.boundingBox())!.height).toBe(height);
});

test("a pulse's level is as wide as \"high\" either way, so the figures after it hold still as the pointer crosses edges", async ({ page }) => {
  const inset = await drawn(page);
  const b = (await inset.locator("canvas").boundingBox())!;
  // Along SWCLK's row, a few pixels a pulse, until both levels have been read.
  const left = new Map<string, number>();
  for (let fx = 0.55; fx < 0.65 && left.size < 2; fx += 0.002) {
    await page.mouse.move(b.x + b.width * fx, b.y + 40);
    const r = await inset.evaluate((e) => {
      const first = e.shadowRoot!.querySelector(".line.reads .reading > span");
      const level = first?.querySelector(".level")?.textContent;
      const figure = first?.querySelector(".figures");
      return level && figure ? ([level, figure.getBoundingClientRect().left] as const) : null;
    });
    if (r) left.set(r[0], r[1]);
  }
  expect([...left.keys()].sort()).toEqual(["high", "low"]);
  expect(left.get("low")).toBe(left.get("high"));
});

test("an inset is one Tab stop, named by its label; Shift with the arrow keys steps through its notes and says each, and Escape clears", async ({ page }) => {
  const inset = await drawn(page);
  await page.keyboard.press("Tab");
  const hit = inset.locator(".hit");
  await expect(hit).toBeFocused();
  await expect(hit).toHaveAttribute("aria-label", "Reading the debug port's ID register");
  await expect(hit).toHaveAttribute("aria-description", /Shift with left and right steps through its parts/);
  await page.keyboard.press("Shift+ArrowRight");
  expect(await reading(inset)).toEqual(REQUEST);
  await expect(inset.locator(".said")).toHaveText("request 0xA5, 8.001 µs, probe drives".replace(/ µs/, " µs"));
  await page.keyboard.press("Shift+ArrowRight");
  expect(await reading(inset)).toEqual(ACK);
  await page.keyboard.press("Shift+ArrowLeft");
  expect(await reading(inset)).toEqual(REQUEST);
  await page.keyboard.press("Escape");
  expect(await reading(inset)).toBeNull();
  await expect(inset.locator(".said")).toHaveText("");
});

test("with href, the inset links to that page at the lit note, or its own annotation, keeping that address's other parameters", async ({ page }) => {
  const inset = await drawn(page, `&href=${encodeURIComponent("/dist/site/open.html?from=page&annotation=old")}`);
  const hit = inset.locator(".hit");
  const params = async () => Object.fromEntries(new URL((await hit.getAttribute("href"))!, page.url()).searchParams);
  expect(await params()).toEqual({
    from: "page",
    src: "/fixtures/identify-swd.tickwave",
    annotations: "/fixtures/identify-swd.annotations.toml",
    annotation: "dpidr-read",
  });
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  expect((await params()).annotation).toBe("dpidr-ack");
  // Enter follows the link: the Open page, at the ack.
  await page.keyboard.press("Enter");
  await page.waitForURL(/open\.html/);
  const view = page.locator("#view tickwave-view");
  await expect.poll(() => view.evaluate((v) => (v as HTMLElement & { window: { start: number } | null }).window?.start ?? null), { timeout: 20_000 }).toBe(377102308);
  expect(new URL(page.url()).searchParams.get("annotation")).toBe("dpidr-ack");
});

test("an inset that opens somewhere marks where: on the note the pointer or the keys light, else with the keys' focus at the span shown, and it moves nothing", async ({ page }) => {
  const inset = await drawn(page, `&href=${encodeURIComponent("/dist/site/open.html")}`);
  const cue = inset.locator(".opens-at");
  const canvas = (await inset.locator("canvas").boundingBox())!;
  const height = (await inset.boundingBox())!.height;
  const left = async () => {
    const b = (await cue.boundingBox())!;
    expect(b.x).toBeGreaterThanOrEqual(canvas.x);
    expect(b.x + b.width).toBeLessThanOrEqual(canvas.x + canvas.width + 0.5);
    expect(b.y + b.height).toBeLessThanOrEqual(canvas.y + canvas.height + 0.5);
    return b.x;
  };
  await expect(cue).toBeHidden();
  // The pointer along the notes: the mark sits on each one lit, and moves on with it.
  const at: Record<string, number> = {};
  for (let fx = 0.15; fx < 0.98 && !(at.request && at.ack); fx += 0.01) {
    const p = await onLane(inset, fx);
    await page.mouse.move(p.x, p.y);
    const r = await reading(inset);
    if (r?.[0] === REQUEST[0] && !at.request) at.request = await left();
    if (r?.[0] === ACK[0] && !at.ack) at.ack = await left();
  }
  expect(at.request).toBeDefined();
  expect(at.ack).toBeGreaterThan(at.request);
  await expect(cue).toHaveText("Opens here");
  await page.mouse.move(0, 0);
  await expect(cue).toBeHidden();
  // The keys' focus with no note lit: the span shown, from its start. A note stepped to: there.
  await page.keyboard.press("Tab");
  await expect(cue).toBeVisible();
  const whole = await left();
  expect(whole).toBeLessThan(at.request);
  await page.keyboard.press("Shift+ArrowRight");
  expect(await reading(inset)).toEqual(REQUEST);
  expect(Math.abs((await left()) - at.request)).toBeLessThan(1);
  await page.keyboard.press("Escape");
  expect(await left()).toBe(whole);
  await page.keyboard.press("Shift+Tab");
  await expect(cue).toBeHidden();
  expect((await inset.boundingBox())!.height).toBe(height);
});

test("an inset that opens nothing marks nothing", async ({ page }) => {
  const inset = await drawn(page);
  await sweepLane(page, inset);
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+ArrowRight");
  expect(await reading(inset)).toEqual(REQUEST);
  await expect(inset.locator(".opens-at")).toBeHidden();
});

test("with for, a click on a note opens the page's view at it, and for wins over href", async ({ page }) => {
  const inset = await drawn(page, `&for&href=${encodeURIComponent("/dist/site/open.html")}`);
  await expect(inset.locator(".hit")).not.toHaveAttribute("href");
  await expect(inset.locator(".hit")).toHaveAttribute("role", "button");
  const seen = await sweepLane(page, inset);
  expect(seen).toContainEqual(ACK);
  // Back to the ack, found by sweeping again to it.
  for (let fx = 0.15; fx < 0.98; fx += 0.01) {
    const p = await at(inset, fx, 10);
    await page.mouse.move(p.x, p.y);
    if ((await reading(inset))?.[0] === "ack OK") break;
  }
  await page.mouse.down();
  await page.mouse.up();
  const view = page.locator("#v");
  await expect(view).toHaveAttribute("open", "");
  await expect.poll(() => view.evaluate((v) => (v as HTMLElement & { window: { start: number } | null }).window?.start ?? null), { timeout: 20_000 }).toBe(377102308);
  expect(page.url()).toContain("inset.html");
});

test("on a touch screen, a tap lights a note and a tap on the lit note opens at it", async ({ browser }) => {
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const inset = await drawn(page, "&for");
  // The ack, found with the mouse's help, then tapped.
  let ack: { x: number; y: number } | null = null;
  for (let fx = 0.15; fx < 0.98 && !ack; fx += 0.01) {
    const p = await at(inset, fx, 10);
    await page.mouse.move(p.x, p.y);
    if ((await reading(inset))?.[0] === "ack OK") ack = p;
  }
  await page.mouse.move(0, 0);
  await page.touchscreen.tap(ack!.x, ack!.y);
  expect(await reading(inset)).toEqual(ACK);
  await expect(page.locator("#v")).not.toHaveAttribute("open", "");
  await page.touchscreen.tap(ack!.x, ack!.y);
  await expect(page.locator("#v")).toHaveAttribute("open", "");
  await context.close();
});

test("a reference's popover stays open as the mouse moves into it; a note there opens the view at that note, and leaving closes it", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/e2e/pages/view.html?annotations=fixtures/identify-swd.annotations.toml");
  const ref = page.locator("tickwave-ref");
  await ref.scrollIntoViewIfNeeded();
  const rb = (await ref.boundingBox())!;
  await page.mouse.move(rb.x + 4, rb.y + rb.height / 2);
  const inset = ref.locator("tickwave-inset");
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  const pop = ref.locator(".pop");
  // Across the gap to the popover, then along its lane to the ack.
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.15, b.y + 10, { steps: 10 });
  for (let fx = 0.15; fx < 0.98; fx += 0.01) {
    await page.mouse.move(b.x + b.width * fx, b.y + 10);
    if ((await reading(inset))?.[0] === "ack OK") break;
  }
  expect(await pop.evaluate((p) => p.matches(":popover-open"))).toBe(true);
  expect(await reading(inset)).toEqual(ACK);
  await page.mouse.down();
  await page.mouse.up();
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => (v as HTMLElement & { window: { start: number } | null }).window?.start ?? null), { timeout: 20_000 }).toBe(377102308);
  // Out of the popover and the text: it closes.
  await ref.scrollIntoViewIfNeeded();
  await page.mouse.move(rb.x + 4, rb.y + rb.height / 2);
  await expect.poll(() => pop.evaluate((p) => p.matches(":popover-open"))).toBe(true);
  await page.mouse.move(1, 1);
  await expect.poll(() => pop.evaluate((p) => p.matches(":popover-open"))).toBe(false);
});

test("with the focus on a reference, Shift with the arrow keys steps through its popover's notes, and Enter opens the view at the one lit", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/e2e/pages/view.html?annotations=fixtures/identify-swd.annotations.toml");
  const ref = page.locator("tickwave-ref");
  await ref.scrollIntoViewIfNeeded();
  await ref.focus();
  const inset = ref.locator("tickwave-inset");
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  // The popover's inset is no Tab stop: the reference keeps the focus.
  await expect(inset.locator(".hit")).toHaveAttribute("tabindex", "-1");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  expect(await reading(inset)).toEqual(ACK);
  await expect(ref).toBeFocused();
  await page.keyboard.press("Enter");
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => (v as HTMLElement & { window: { start: number } | null }).window?.start ?? null), { timeout: 20_000 }).toBe(377102308);
});

test("Ctrl or Cmd with the wheel zooms an inset about the pointer, and out to its span but no further; zoomed in, Shift with the wheel moves along it", async ({ page }) => {
  const inset = await drawn(page);
  const whole = await win(inset);
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.6, b.y + 40);
  await expect.poll(() => reading(inset)).not.toBeNull();
  const before = await reading(inset);
  // At its whole span, the inset has nowhere to move.
  await page.keyboard.down("Shift");
  await page.mouse.wheel(0, 200);
  await page.keyboard.up("Shift");
  expect(await win(inset)).toEqual(whole);
  await ctrlWheel(page, -400);
  const zoomed = await win(inset);
  expect(span(zoomed)).toBeLessThan(span(whole) / 2);
  expect(zoomed.start).toBeGreaterThan(whole.start);
  expect(zoomed.end).toBeLessThan(whole.end);
  // The pulse under the pointer stays under it.
  expect(await reading(inset)).toEqual(before);
  await page.keyboard.down("Shift");
  await page.mouse.wheel(0, 100);
  await page.keyboard.up("Shift");
  const moved = await win(inset);
  expect(moved.start).toBeGreaterThan(zoomed.start);
  expect(span(moved)).toBeCloseTo(span(zoomed), 3);
  // As far as the span's end, and no further.
  await page.keyboard.down("Shift");
  await page.mouse.wheel(0, 20_000);
  await page.keyboard.up("Shift");
  const end = await win(inset);
  expect(end.end).toBeCloseTo(whole.end, 3);
  expect(span(end)).toBeCloseTo(span(zoomed), 3);
  await ctrlWheel(page, 4000);
  expect(await win(inset)).toEqual(whole);
  // The plain wheel scrolls the page and leaves the inset be.
  await page.evaluate(() => (document.body.style.height = "4000px"));
  await page.mouse.wheel(0, 200);
  await expect.poll(() => page.evaluate(() => scrollY)).toBeGreaterThan(0);
  expect(await win(inset)).toEqual(whole);
});

test("where one pixel holds several edges, an inset reads their count and time high and says how to zoom in, and zoomed in there it reads the pulse", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  // The whole attach sequence, hundreds of clocks across the inset.
  await page.goto(INSET.replace("annotation=dpidr-read", "annotation=attach"));
  const inset = page.locator("#i");
  await expect(inset.locator("figcaption")).toHaveText(/^Attaching/, { timeout: 20_000 });
  await inset.evaluate((e) => (e as HTMLElement & { settled(): Promise<void> }).settled());
  const b = (await inset.locator("canvas").boundingBox())!;
  let r: string[] | null = null;
  for (let fx = 0.2; fx < 0.95 && !/ edges in /.test(r?.[1] ?? ""); fx += 0.01) {
    await page.mouse.move(b.x + b.width * fx, b.y + 40);
    r = await reading(inset);
  }
  expect(r).toEqual([
    "SWCLK",
    expect.stringMatching(/^[\d,]+ edges in [\d.]+ µs$/),
    expect.stringMatching(/^high \d+\.\d %$/),
    expect.stringMatching(/^(Ctrl|Cmd) \+ scroll to measure$/),
  ]);
  await ctrlWheel(page, -1500);
  await expect.poll(async () => (await reading(inset))?.[0] ?? "").toMatch(/^SWCLK (high|low) \d/);
});

test("with an inset focused, up and down zoom and left and right move along it; zoomed in, a note stepped to comes into sight", async ({ page }) => {
  const inset = await drawn(page);
  const whole = await win(inset);
  await page.keyboard.press("Tab");
  await page.keyboard.press("ArrowLeft");
  expect(await win(inset)).toEqual(whole);
  await page.keyboard.press("ArrowUp");
  const zoomed = await win(inset);
  expect(span(zoomed)).toBeCloseTo(span(whole) / 1.6, 0);
  expect(zoomed.start + zoomed.end).toBeCloseTo(whole.start + whole.end, 0);
  await page.keyboard.press("ArrowRight");
  const moved = await win(inset);
  expect(moved.start - zoomed.start).toBeCloseTo(span(zoomed) / 8, 0);
  // Closer in on the middle, the request, at the span's start, is out of sight; stepping to it
  // brings it in whole.
  for (let i = 0; i < 4; i++) await page.keyboard.press("ArrowUp");
  const close = await win(inset);
  expect(close.start).toBeGreaterThan(377_094_557);
  await page.keyboard.press("Shift+ArrowRight");
  expect(await reading(inset)).toEqual(REQUEST);
  const shown = await win(inset);
  expect(shown.start).toBeLessThanOrEqual(377_094_557);
  expect(shown.end).toBeGreaterThanOrEqual(377_094_557 + 8001);
  // With a note lit, the keys zoom about it, off the middle: it stays where it is.
  await page.keyboard.press("ArrowLeft");
  const mid = 377_094_557 + 8001 / 2;
  const at = (w: { start: number; end: number }) => (mid - w.start) / span(w);
  const off = await win(inset);
  expect(at(off)).toBeCloseTo(0.625, 6);
  await page.keyboard.press("ArrowUp");
  expect(at(await win(inset))).toBeCloseTo(0.625, 6);
  for (let i = 0; i < 12; i++) await page.keyboard.press("ArrowDown");
  expect(await win(inset)).toEqual(whole);
  expect(await reading(inset)).toEqual(REQUEST);
});

test("on a touch screen, two fingers zoom an inset, the finger left down moves nothing, and lifting them opens nothing", async ({ browser, browserName }) => {
  test.skip(browserName !== "chromium", "two touch points at once need Chromium's DevTools protocol");
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const inset = await drawn(page, "&for");
  const whole = await win(inset);
  const b = (await inset.locator("canvas").boundingBox())!;
  const x = b.x + b.width * 0.6;
  const y = b.y + 40;
  const cdp = await context.newCDPSession(page);
  const fingers = (type: "touchStart" | "touchMove" | "touchEnd", d: number | null) =>
    cdp.send("Input.dispatchTouchEvent", {
      type,
      touchPoints: d === null ? [] : [{ x: x - d, y, id: 0 }, { x: x + d, y, id: 1 }],
    });
  // Zoomed in a little first, so that the first finger down begins a drag.
  await page.keyboard.press("Tab");
  await page.keyboard.press("ArrowUp");
  await fingers("touchStart", 20);
  for (let d = 30; d <= 100; d += 10) await fingers("touchMove", d);
  // One finger lifts, and the other moves on: it points, and moves nothing. A touchEnd that lists a
  // point lifts that one alone, after the moves Chromium holds for the next frame.
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [{ x: x + 100, y, id: 1 }] });
  const frame = () => page.evaluate(() => new Promise((r) => requestAnimationFrame(r)));
  await frame();
  const zoomed = await win(inset);
  expect(span(zoomed)).toBeLessThan(span(whole) / 4);
  for (let dx = 10; dx <= 60; dx += 10) await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: x - 100 + dx, y, id: 0 }] });
  await fingers("touchEnd", null);
  await frame();
  expect(await win(inset)).toEqual(zoomed);
  await page.waitForTimeout(300);
  await expect(page.locator("#v")).not.toHaveAttribute("open", "");
  await context.close();
});

test("with the focus on a reference, up and down zoom its popover's inset, and the popover shown again later shows the whole span", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/e2e/pages/view.html?annotations=fixtures/identify-swd.annotations.toml");
  const ref = page.locator("tickwave-ref");
  await ref.scrollIntoViewIfNeeded();
  await ref.focus();
  const inset = ref.locator("tickwave-inset");
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  await inset.evaluate((e) => (e as HTMLElement & { settled(): Promise<void> }).settled());
  const whole = await win(inset);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  expect(span(await win(inset))).toBeCloseTo(span(whole) / 1.6 ** 2, 0);
  await page.keyboard.press("ArrowRight");
  await expect(ref).toBeFocused();
  const pop = ref.locator(".pop");
  await page.keyboard.press("Escape");
  await expect.poll(() => pop.evaluate((p) => p.matches(":popover-open"))).toBe(false);
  await page.waitForTimeout(900);
  await ref.evaluate((e) => (e as HTMLElement).blur());
  await ref.focus();
  await expect.poll(() => pop.evaluate((p) => p.matches(":popover-open"))).toBe(true);
  await expect.poll(() => win(inset)).toEqual(whole);
});

/** The span a view shows, in ticks, once it has one. */
const viewWindow = (view: Locator) => view.evaluate((v) => (v as HTMLElement & { window: { start: number; end: number } }).window);

test("zoomed in, a drag moves an inset along its span and opens nothing; a click then opens the view over the span shown", async ({ page }) => {
  const inset = await drawn(page, "&for");
  const p = await at(inset, 0.6, 40);
  await page.mouse.move(p.x, p.y);
  await ctrlWheel(page, -400);
  const zoomed = await win(inset);
  const before = await reading(inset);
  expect(before).not.toBeNull();
  await page.mouse.down();
  await page.mouse.move(p.x - 80, p.y, { steps: 8 });
  // The pulse taken hold of stays under the pointer.
  expect(await reading(inset)).toEqual(before);
  await page.mouse.up();
  const moved = await win(inset);
  expect(moved.start).toBeGreaterThan(zoomed.start);
  expect(span(moved)).toBeCloseTo(span(zoomed), 3);
  // Off the trace, across the label above it and the line of text under it, and let go there: the
  // drag goes on outside the trace, selects no text, and ends with the button.
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.mouse.down();
  await page.mouse.move(p.x - 100, b.y - 30, { steps: 8 });
  await page.mouse.move(p.x + 150, b.y + b.height + 12, { steps: 8 });
  await page.mouse.up();
  const out = await win(inset);
  expect(out.start).toBeLessThan(moved.start);
  await page.mouse.move(p.x - 40, p.y, { steps: 4 });
  expect(await win(inset)).toEqual(out);
  await page.waitForTimeout(300);
  await expect(page.locator("#v")).not.toHaveAttribute("open", "");
  expect(await page.evaluate(() => getSelection()?.toString() ?? "")).toBe("");
  await page.mouse.down();
  await page.mouse.up();
  const view = page.locator("#v");
  await expect(view).toHaveAttribute("open", "");
  await expect.poll(async () => (await viewWindow(view)).start, { timeout: 20_000 }).toBeCloseTo(out.start, 3);
  expect((await viewWindow(view)).end).toBeCloseTo(out.end, 3);
});

test("zoomed in, an inset's link takes the span it shows to the Open page, which opens over it", async ({ page }) => {
  // The page's own window gives way to the annotation's span.
  const inset = await drawn(page, `&href=${encodeURIComponent("/dist/site/open.html?window=1 2")}`);
  const hit = inset.locator(".hit");
  const params = async () => new URL((await hit.getAttribute("href"))!, page.url()).searchParams;
  expect((await params()).has("window")).toBe(false);
  await page.keyboard.press("Tab");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  const shown = (z: { start: number; end: number }) => `${Math.floor(z.start)} ${Math.ceil(z.end)}`;
  expect((await params()).get("window")).toBe(shown(await win(inset)));
  // Shown again later, the inset shows its whole span, and its link names none.
  await inset.evaluate((e) => ((e as HTMLElement).style.display = "none"));
  await page.waitForTimeout(900);
  await inset.evaluate((e) => ((e as HTMLElement).style.display = ""));
  await expect.poll(async () => (await params()).has("window")).toBe(false);
  await hit.focus();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  expect((await params()).get("window")).toBe(shown(await win(inset)));
  // A drag on the link moves the span, the pulse taken hold of staying under the pointer, and the
  // address goes with it: the link's own drag does not take over.
  const p = await at(inset, 0.6, 40);
  await page.mouse.move(p.x, p.y);
  const before = await win(inset);
  const held = await reading(inset);
  expect(held).not.toBeNull();
  await page.mouse.down();
  await page.mouse.move(p.x - 60, p.y, { steps: 6 });
  expect(await reading(inset)).toEqual(held);
  await page.mouse.up();
  const z = await win(inset);
  expect(z.start).toBeGreaterThan(before.start);
  expect((await params()).get("window")).toBe(shown(z));
  expect(page.url()).toContain("inset.html");
  await hit.focus();
  await page.keyboard.press("Enter");
  await page.waitForURL(/open\.html/);
  const view = page.locator("#view tickwave-view");
  const [s, e] = shown(z).split(" ").map(Number);
  await expect.poll(async () => (await viewWindow(view)).start, { timeout: 20_000 }).toBe(s);
  expect((await viewWindow(view)).end).toBe(e);
  // The page's own link keeps the span; a space written as %20 reads the same as +.
  const url = new URL(page.url());
  expect(url.searchParams.get("window")).toBe(shown(z));
  expect(url.searchParams.get("annotation")).toBe("dpidr-read");
  await page.goto(page.url().replace(/window=(\d+)\+(\d+)/, "window=$1%20$2"));
  await expect.poll(async () => (await viewWindow(page.locator("#view tickwave-view"))).start, { timeout: 20_000 }).toBe(s);
  // Without an annotation, the page opens over the span all the same.
  await page.goto(page.url().replace(/&annotation=[^&]*/, ""));
  await expect.poll(async () => (await viewWindow(page.locator("#view tickwave-view"))).start, { timeout: 20_000 }).toBe(s);
  expect((await viewWindow(page.locator("#view tickwave-view"))).end).toBe(e);
  expect(new URL(page.url()).searchParams.get("window")).toBe(shown(z));
});

test("not zoomed in, a press that moves is a click: the inset moves nowhere and opens the view", async ({ page }) => {
  const inset = await drawn(page, "&for");
  const whole = await win(inset);
  const p = await at(inset, 0.6, 40);
  await page.mouse.move(p.x, p.y);
  await page.mouse.down();
  await page.mouse.move(p.x - 80, p.y, { steps: 8 });
  expect(await win(inset)).toEqual(whole);
  await page.mouse.up();
  await expect(page.locator("#v")).toHaveAttribute("open", "");
});

test("on a touch screen, one finger moves a zoomed-in inset along its span, and lifting it opens nothing", async ({ browser, browserName }) => {
  test.skip(browserName !== "chromium", "a touch that moves needs Chromium's DevTools protocol");
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const inset = await drawn(page, "&for");
  await page.keyboard.press("Tab");
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  const zoomed = await win(inset);
  const p = await at(inset, 0.6, 40);
  const cdp = await context.newCDPSession(page);
  const finger = (type: "touchStart" | "touchMove" | "touchEnd", dx: number | null) =>
    cdp.send("Input.dispatchTouchEvent", { type, touchPoints: dx === null ? [] : [{ x: p.x + dx, y: p.y, id: 0 }] });
  await finger("touchStart", 0);
  for (let dx = -10; dx >= -80; dx -= 10) await finger("touchMove", dx);
  await finger("touchEnd", null);
  const moved = await win(inset);
  expect(moved.start).toBeGreaterThan(zoomed.start);
  expect(span(moved)).toBeCloseTo(span(zoomed), 3);
  await page.waitForTimeout(300);
  await expect(page.locator("#v")).not.toHaveAttribute("open", "");
  await context.close();
});

test("a reference opens its view over the span its popover is zoomed to; a drag there moves it and opens nothing; once the popover has closed, Enter opens at the annotation", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/e2e/pages/view.html?annotations=fixtures/identify-swd.annotations.toml");
  const ref = page.locator("tickwave-ref");
  const inset = ref.locator("tickwave-inset");
  const pop = ref.locator(".pop");
  const view = page.locator("#v");
  await ref.scrollIntoViewIfNeeded();
  const rb = (await ref.evaluate((e) => e.getClientRects()[0].toJSON())) as { x: number; y: number; height: number };
  await page.mouse.move(rb.x + 4, rb.y + rb.height / 2);
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  await inset.evaluate((e) => (e as HTMLElement & { settled(): Promise<void> }).settled());
  const b = (await inset.locator("canvas").boundingBox())!;
  const p = { x: b.x + b.width * 0.6, y: b.y + 40 };
  await page.mouse.move(p.x, p.y, { steps: 10 });
  await ctrlWheel(page, -400);
  const zoomed = await win(inset);
  await page.mouse.down();
  await page.mouse.move(p.x - 60, p.y, { steps: 6 });
  await page.mouse.up();
  const moved = await win(inset);
  expect(moved.start).toBeGreaterThan(zoomed.start);
  await page.waitForTimeout(300);
  expect(await pop.evaluate((x) => x.matches(":popover-open"))).toBe(true);
  await expect(view).not.toHaveAttribute("open", "");
  await page.mouse.down();
  await page.mouse.up();
  await expect.poll(async () => (await viewWindow(view)).start, { timeout: 20_000 }).toBeCloseTo(moved.start, 3);
  expect((await viewWindow(view)).end).toBeCloseTo(moved.end, 3);

  // Zoomed by the keys, then closed with Escape: Enter opens at the annotation's own span.
  await page.goto("/e2e/pages/view.html?annotations=fixtures/identify-swd.annotations.toml");
  await ref.scrollIntoViewIfNeeded();
  await ref.focus();
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  // The popover's first frame shows the whole span: keys pressed before it would be undone by it.
  await inset.evaluate((e) => (e as HTMLElement & { settled(): Promise<void> }).settled());
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  expect(await inset.evaluate((e) => (e as HTMLElement & { zoomed: object | null }).zoomed)).not.toBeNull();
  await page.keyboard.press("Escape");
  await expect.poll(() => pop.evaluate((x) => x.matches(":popover-open"))).toBe(false);
  await page.keyboard.press("Enter");
  const a = (await inset.evaluate((e) => (e as HTMLElement & { annotation: { start: number; end: number } }).annotation))!;
  const pad = (a.end - a.start) * 0.25;
  await expect.poll(async () => (await viewWindow(view)).start, { timeout: 20_000 }).toBeCloseTo(a.start - pad, 3);
  expect((await viewWindow(view)).end).toBeCloseTo(a.end + pad, 3);
});

test("an inset that opens somewhere has an Open at the end of its foot's first line, which goes where the plot does and is no Tab stop of its own; one that opens nothing has none", async ({ page }) => {
  const plain = await drawn(page);
  await expect(plain.locator(".open")).toBeHidden();
  const inset = await drawn(page, `&href=${encodeURIComponent("/dist/site/open.html")}`);
  const open = inset.locator(".open");
  await expect(open).toBeVisible();
  const hrefs = () => inset.evaluate((e) => [".hit", ".open"].map((s) => e.shadowRoot!.querySelector(s)!.getAttribute("href")));
  const [hit, link] = await hrefs();
  expect(link).toBe(hit);
  await page.keyboard.press("Tab");
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Shift+ArrowRight");
  const [litHit, litLink] = await hrefs();
  expect(litLink).toBe(litHit);
  expect(new URL(litLink!, page.url()).searchParams.get("annotation")).toBe("dpidr-ack");
  await page.keyboard.press("Tab");
  await expect(open).not.toBeFocused();
});

test("on a touch screen, Open opens the view at the note a tap lit", async ({ browser }) => {
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const inset = await drawn(page, "&for");
  let ack: { x: number; y: number } | null = null;
  for (let fx = 0.15; fx < 0.98 && !ack; fx += 0.01) {
    const p = await at(inset, fx, 10);
    await page.mouse.move(p.x, p.y);
    if ((await reading(inset))?.[0] === "ack OK") ack = p;
  }
  await page.mouse.move(0, 0);
  await page.touchscreen.tap(ack!.x, ack!.y);
  expect(await reading(inset)).toEqual(ACK);
  const view = page.locator("#v");
  await expect(view).not.toHaveAttribute("open", "");
  await inset.locator(".open").tap();
  await expect(view).toHaveAttribute("open", "");
  await expect.poll(() => view.evaluate((v) => (v as HTMLElement & { window: { start: number } | null }).window?.start ?? null), { timeout: 20_000 }).toBe(377102308);
  await context.close();
});

test("on a touch screen, a tap where no note is opens nothing, and Open then opens the view over the inset's annotation", async ({ browser }) => {
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const inset = await drawn(page, "&for");
  const b = (await inset.locator("canvas").boundingBox())!;
  await page.touchscreen.tap(b.x + b.width / 2, b.y + b.height * 0.75);
  const view = page.locator("#v");
  await page.waitForTimeout(300);
  await expect(view).not.toHaveAttribute("open", "");
  await inset.locator(".open").tap();
  await expect(view).toHaveAttribute("open", "");
  // dpidr-read runs from 377,094,557 to 377,140,560 ticks.
  await expect
    .poll(() => view.evaluate((v) => {
      const w = (v as HTMLElement & { window: { start: number; end: number } | null }).window;
      return w !== null && w.start <= 377094557 && w.end >= 377140560;
    }), { timeout: 20_000 })
    .toBe(true);
  await context.close();
});

/** The reference on view.html, its text's left end, and whether its popover is open. */
async function reference(page: Page, query = "") {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`/e2e/pages/view.html?annotations=fixtures/identify-swd.annotations.toml${query}`);
  const ref = page.locator("tickwave-ref");
  await ref.scrollIntoViewIfNeeded();
  const rb = (await ref.boundingBox())!;
  const shown = () => ref.locator(".pop").evaluate((p) => p.matches(":popover-open"));
  return { ref, text: { x: rb.x + 4, y: rb.y + rb.height / 2 }, shown };
}

test("on a touch screen, a tap on a reference shows its figure and opens nothing, the next tap closes it, and the figure's Open opens the view", async ({ browser }) => {
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const { ref, text, shown } = await reference(page);
  const view = page.locator("#v");
  await page.touchscreen.tap(text.x, text.y);
  await expect.poll(shown).toBe(true);
  await page.waitForTimeout(300);
  await expect(view).not.toHaveAttribute("open", "");
  await page.touchscreen.tap(text.x, text.y);
  await expect.poll(shown).toBe(false);
  await page.touchscreen.tap(text.x, text.y);
  await expect.poll(shown).toBe(true);
  const inset = ref.locator("tickwave-inset");
  await expect(inset.locator(".measure")).toHaveText(/kHz/, { timeout: 20_000 });
  await inset.locator(".open").tap();
  await expect(view).toHaveAttribute("open", "");
  await expect.poll(shown).toBe(false);
  await context.close();
});

test("on a touch screen, a finger held on a reference that turns into a scroll closes its figure", async ({ browser, browserName }) => {
  test.skip(browserName !== "chromium", "a touch that scrolls is sent through Chromium's DevTools protocol");
  const context = await browser.newContext({ hasTouch: true });
  const page = await context.newPage();
  const { text, shown } = await reference(page, "&below");
  const cdp = await context.newCDPSession(page);
  const touch = (type: "touchStart" | "touchMove" | "touchEnd", touchPoints: { x: number; y: number }[]) => cdp.send("Input.dispatchTouchEvent", { type, touchPoints });
  await touch("touchStart", [text]);
  await expect.poll(shown).toBe(true);
  for (let k = 1; k <= 12; k++) await touch("touchMove", [{ x: text.x, y: text.y + 15 * k }]);
  await touch("touchEnd", []);
  await expect.poll(shown).toBe(false);
  await context.close();
});

test("over Open, with no note lit, the plot marks where it opens: the span shown", async ({ page }) => {
  const inset = await drawn(page, "&for");
  const cue = inset.locator(".opens-at");
  await expect(cue).toBeHidden();
  await inset.locator(".open").hover();
  await expect(cue).toBeVisible();
  await page.mouse.move(0, 0);
  await expect(cue).toBeHidden();
});
