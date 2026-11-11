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

test("where one pixel holds several edges, an inset says how to zoom in, and zoomed in there it reads the pulse", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  // The whole attach sequence, hundreds of clocks across the inset.
  await page.goto(INSET.replace("annotation=dpidr-read", "annotation=attach"));
  const inset = page.locator("#i");
  await expect(inset.locator("figcaption")).toHaveText(/^Attaching/, { timeout: 20_000 });
  await inset.evaluate((e) => (e as HTMLElement & { settled(): Promise<void> }).settled());
  const b = (await inset.locator("canvas").boundingBox())!;
  let r: string[] | null = null;
  for (let fx = 0.2; fx < 0.95 && r?.[1] !== "zoom in to measure"; fx += 0.01) {
    await page.mouse.move(b.x + b.width * fx, b.y + 40);
    r = await reading(inset);
  }
  expect(r).toEqual(["SWCLK", "zoom in to measure", expect.stringMatching(/^(Ctrl|Cmd) \+ scroll$/)]);
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

test("on a touch screen, two fingers zoom an inset, and lifting them opens nothing", async ({ browser, browserName }) => {
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
  await fingers("touchStart", 20);
  for (let d = 30; d <= 100; d += 10) await fingers("touchMove", d);
  await fingers("touchEnd", null);
  const zoomed = await win(inset);
  expect(span(zoomed)).toBeLessThan(span(whole) / 4);
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
