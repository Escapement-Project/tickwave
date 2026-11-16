// The label at the pointer, and the cursor readout's pulse line, on e2e/pages/view.html. Expected
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
  expect(seen).toContain("low 627 ns\nperiod 1.274 µs\nduty 50.78 %\n784.9 kHz\n1/width 1.595 MHz");
  await page.mouse.move(0, 0);
  await expect(probe(view)).toBeHidden();
});

test("in a taller row the width and the period sit on their spans, and the label under the row keeps the rest", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await settle(view, 376_718_800, 376_721_600);
  await view.locator('[part="row-handle"]').first().focus();
  for (let i = 0; i < 4; i++) await page.keyboard.press("Shift+ArrowDown");
  await settle(view);
  // The low pulse from 376,719,436 to 376,720,063; its period closes at 376,720,710.
  const x = await xOf(view, 376_719_700);
  const plot = (await view.locator(".plot").boundingBox())!;
  let text = "";
  for (let y = plot.y + 4; y < plot.y + plot.height - 20 && !text.startsWith("duty"); y += 6) {
    await page.mouse.move(x, y);
    text = (await probe(view).isVisible()) ? (await probe(view).innerText()).replace(/\u00a0/g, " ") : "";
  }
  expect(text).toBe("duty 50.78 %\n784.9 kHz\n1/width 1.595 MHz");
  // Clear of the row it reads, and centred under the pulse, from its start to the edge closing its period.
  const label = (await probe(view).boundingBox())!;
  const rowEnd = (await view.locator('[part="row-handle"]').first().boundingBox())!;
  expect(label.y).toBeGreaterThanOrEqual(rowEnd.y + rowEnd.height / 2);
  const middle = ((await xOf(view, 376_719_436)) + (await xOf(view, 376_720_710))) / 2;
  expect(Math.abs(label.x + label.width / 2 - middle)).toBeLessThan(1.5);
});

test("the label keeps to the side of the plot away from the row it reads, past the plot's edge where the plot is short", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  const plot = async () => (await view.locator(".plot").boundingBox())!;
  // SWCLK, the upper row: its low pulse from 376,719,436 to 376,720,063. The label goes under it.
  await settle(view, 376_719_200, 376_721_000);
  const [swclkEnd, swdioEnd] = await Promise.all([0, 1].map(async (i) => {
    const b = (await view.locator('[part="row-handle"]').nth(i).boundingBox())!;
    return b.y + b.height / 2;
  }));
  const p = await plot();
  await page.mouse.move(await xOf(view, 376_719_700), (p.y + swclkEnd) / 2 - 4);
  await expect(probe(view)).toContainText("low 627 ns");
  let label = (await probe(view).boundingBox())!;
  expect(label.y).toBeGreaterThanOrEqual(swclkEnd);
  // The plot is shorter than the label: it runs past the plot's lower edge.
  expect(label.y + label.height).toBeGreaterThan(p.y + p.height);
  // SWDIO, the lower row: its high pulse from 376,846,682 to 376,851,176. The label goes above it.
  await settle(view, 376_845_000, 376_853_000);
  await page.mouse.move(await xOf(view, 376_848_000), (swclkEnd + swdioEnd) / 2);
  await expect(probe(view)).toContainText("high 4.494 µs");
  label = (await probe(view).boundingBox())!;
  expect(label.y + label.height).toBeLessThanOrEqual(swclkEnd);
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

test("a pulse being measured goes on measuring as the pointer moves within it, and once measured reads at once anywhere in it", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  // 100 ns of SWDIO's 4.494 µs high pulse, as above; SWDIO is the lower row.
  await settle(view, 376_848_000, 376_848_100);
  const [a, b] = await Promise.all([0, 1].map(async (i) => {
    const h = (await view.locator('[part="row-handle"]').nth(i).boundingBox())!;
    return h.y + h.height / 2;
  }));
  const y = (a + b) / 2;
  const text = async () => ((await probe(view).isVisible()) ? (await probe(view).innerText()).replace(/ /g, " ") : "");
  // The pointer never rests: a step every 40 ms, under the 150 ms the view waits for a rest.
  const x0 = await xOf(view, 376_848_020);
  let seen = "";
  for (let i = 0; i < 50 && !seen.startsWith("high 4.494 µs"); i++) {
    await page.mouse.move(x0 + (i % 20) * 2, y);
    await page.waitForTimeout(40);
    seen = await text();
  }
  expect(seen).toMatch(/^high 4\.494 µs\nperiod /);
  // Elsewhere in the same pulse the label reads at once, without measuring again.
  await page.mouse.move(await xOf(view, 376_848_080), y);
  expect(await text()).toMatch(/^high 4\.494 µs/);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  expect(await text()).toMatch(/^high 4\.494 µs/);
});

test("zoomed out, a pixel with several edges reads their count and its time high, and asks for a closer look", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await settle(view);
  const seen = await labels(page, view, await xOf(view, 380_000_000));
  // SWCLK's row and SWDIO's: each label its pixel's count and share, then the ask.
  const dense = seen.filter((s) => s.endsWith("zoom in to measure"));
  expect(dense.length).toBe(2);
  for (const s of dense) expect(s).toMatch(/^[\d,]+ edges in [\d.]+ µs\nhigh \d+\.\d %\nzoom in to measure$/);
});

test("zoomed out over an overview, a crowded pixel reads the bounds its buckets' densities and duties give", async ({ page }) => {
  // The JTAG capture's whole view draws each channel from its 2^16 level, a few buckets a pixel;
  // 475 ms is in its busiest stretch.
  await page.goto(`${VIEW}?open&src=fixtures/identify-jtag.tickwave`);
  const view = page.locator("#v");
  await settle(view);
  const dense = (await labels(page, view, await xOf(view, 475_000_000))).filter((s) => s.endsWith("zoom in to measure"));
  expect(dense.length).toBeGreaterThan(0);
  // A duty is a share's bounds, so the share is a range, or under or over a bound.
  for (const s of dense) expect(s).toMatch(/^[\d,]+( to [\d,]+)? edges in [\d.]+ µs\nhigh (under [\d.]+|over [\d.]+|[\d.]+ to [\d.]+) %\nzoom in to measure$/);
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
  await expect(readout.locator(".line", { hasText: "SWCLK" })).toHaveText(/^SWCLK low 627\sns 1\/width 1\.595\sMHz period 1\.274\sµs 784\.9\skHz duty 50\.78\s%$/);
  // SWDIO is high from the start to its first fall, a pulse with no period.
  await expect(readout.locator(".line", { hasText: "SWDIO" })).toHaveText(/^SWDIO high at least 376\.845415\sms$/);
  await page.keyboard.press("Enter");
  await expect(readout).toContainText("B 376.719700 ms");
});

test("a channel known only from part way through reads from there: nothing before, and its first pulse from where it is known", async ({ page }) => {
  // crates/format/examples/gaps.rs: A, the upper row, is not known until 300 µs, high there, and
  // falls at 400 µs; B, the lower row, is high from 100 to 200 µs.
  await page.goto(`${VIEW}?open&src=fixtures/leading-gap.tickwave`);
  const view = page.locator("#v");
  await settle(view, 0, 1_000_000);
  const before = await labels(page, view, await xOf(view, 150_000));
  expect(before).toHaveLength(1);
  expect(before[0]).toMatch(/^high 100\.000\sµs/);
  const after = await labels(page, view, await xOf(view, 350_000));
  expect(after[0]).toMatch(/^high at least 100\.000\sµs/);
});
