// The label beside the pointer, and the cursor readout's pulse line, on e2e/pages/view.html. Expected
// values come from the fixtures' own edges: identify-swd's SWCLK starts high and falls at
// 376,719,436 ns, rises at 376,720,063 and falls at 376,720,710 (crates/import/tests/saleae.rs), and
// its SWDIO rises at 376,846,682 and falls at 376,851,176.

import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html";

async function settle(view: Locator, start?: number | string, end?: number | string): Promise<void> {
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  if (start !== undefined) await view.evaluate((v, [s, e]) => (v as HTMLElement & { setWindow(s: unknown, e: unknown): void }).setWindow(s, e), [start, end]);
  await view.evaluate((v) => (v as HTMLElement & { settled(): Promise<void> }).settled());
}

/** The viewport x of a tick, from the view's own linear tickAt. */
async function xOf(view: Locator, tick: number): Promise<number> {
  const box = (await view.locator(".plot").boundingBox())!;
  const [xa, xb] = [box.x + box.width * 0.3, box.x + box.width * 0.7];
  const [ta, tb] = await view.evaluate((v, xs) => xs.map((x) => (v as HTMLElement & { tickAt(x: number): number }).tickAt(x)), [xa, xb]);
  return xa + ((tick + 0.5 - ta) * (xb - xa)) / (tb - ta);
}

const probe = (view: Locator) => view.locator('[part="reading"]');

/** The label's text at each row the pointer crosses going down the plot at `x`, after `wait` ms. */
async function labels(page: Page, view: Locator, x: number, wait = 0): Promise<string[]> {
  const box = (await view.locator(".plot").boundingBox())!;
  const out: string[] = [];
  for (let y = box.y + 4; y < box.y + box.height - 20; y += 6) {
    await page.mouse.move(x, y);
    if (wait) await page.waitForTimeout(wait);
    if (await probe(view).isVisible()) out.push((await probe(view).innerText()).replace(/ /g, " "));
  }
  return [...new Set(out)];
}

test("a digital pulse beside the pointer: its level and width, its period, frequency and duty", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await settle(view, 376_719_200, 376_721_000);
  const seen = await labels(page, view, await xOf(view, 376_719_700));
  expect(seen).toContain("low 627 ns\nperiod 1.274 µs · 784.9 kHz · duty 50.78 %");
  await page.mouse.move(0, 0);
  await expect(probe(view)).toBeHidden();
});

test("a pulse whose edges lie past the data drawn is measured once the pointer rests", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  // 100 ns of SWDIO's 4.494 µs high pulse: its edges are far outside what the row reads.
  await settle(view, 376_848_000, 376_848_100);
  const seen = await labels(page, view, await xOf(view, 376_848_050), 400);
  expect(seen.some((s) => s.startsWith("high 4.494 µs\nperiod "))).toBe(true);
  // Before SWCLK's first edge the pulse runs from the capture's start: no period to give.
  await settle(view, 100_000_000, 100_001_000);
  expect(await labels(page, view, await xOf(view, 100_000_500), 400)).toContain("high at least 376.719436 ms");
});

test("zoomed out, a pixel with several edges asks for a closer look", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await settle(view);
  expect(await labels(page, view, await xOf(view, 380_000_000))).toContain("zoom in to measure");
});

test("an analog channel: the exact value where a pixel holds one sample, the cursor's value; a range zoomed out", async ({ page }) => {
  await page.goto(`${VIEW}?open&src=fixtures/swd50-cut.tickwave`);
  const view = page.locator("#v");
  await settle(view, 10_000, 10_050);
  const middle = 10_025;
  const seen = await labels(page, view, await xOf(view, middle));
  // Enter places cursor A at the middle of the view: its readout gives each analog channel's value there.
  await view.locator(".plot").focus();
  await page.keyboard.press("Enter");
  const lines = view.locator(".readout .line");
  await expect(lines.first()).toContainText("A ");
  const values = (await lines.allTextContents()).map((t) => /^(\S+) A (.+)$/.exec(t.replace(/ /g, " "))?.[2]).filter(Boolean);
  expect(values.length).toBe(2);
  expect(seen.filter((s) => !s.includes(" to ") && !s.startsWith("high") && !s.startsWith("low")).sort()).toEqual([...values].sort());
  await settle(view, 0, 11_000_000);
  expect((await labels(page, view, await xOf(view, 20_000))).some((s) => / to .*V$/.test(s))).toBe(true);
});

test("Enter places cursor A at the middle, and its readout carries each digital channel's pulse", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await settle(view, 376_719_200, 376_720_200);
  await view.locator(".plot").focus();
  await page.keyboard.press("Enter");
  const readout = view.locator(".readout");
  await expect(readout).toContainText("A 376.719700 ms");
  await expect(readout.locator(".line", { hasText: "SWCLK" })).toHaveText(/^SWCLK low 627\sns period 1\.274\sµs 784\.9\skHz duty 50\.78\s%$/);
  // SWDIO is high from the start to its first fall, a pulse with no period.
  await expect(readout.locator(".line", { hasText: "SWDIO" })).toHaveText(/^SWDIO high at least 376\.845415\sms$/);
  await page.keyboard.press("Enter");
  await expect(readout).toContainText("B 376.719700 ms");
});
