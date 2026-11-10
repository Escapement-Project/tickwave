// Row heights the reader sets through each row's handle, on e2e/pages/view.html.

import { test, expect, type Locator } from "@playwright/test";

const VIEW = "/e2e/pages/view.html";

async function ready(view: Locator): Promise<void> {
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
}

const height = (view: Locator) => view.evaluate((v) => (v as HTMLElement).offsetHeight);

test("a row's handle sets its height from the keys, within its limits, and the view grows with it", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await ready(view);
  const handles = view.locator('[part="row-handle"]');
  await expect(handles).toHaveCount(2);
  const swclk = handles.first();
  await expect(swclk).toHaveAttribute("aria-label", "Height of SWCLK");
  await expect(swclk).toHaveAttribute("aria-valuenow", "28");
  const before = await height(view);
  await swclk.focus();
  await page.keyboard.press("ArrowDown");
  await expect(swclk).toHaveAttribute("aria-valuenow", "32");
  await page.keyboard.press("Shift+ArrowDown");
  await expect(swclk).toHaveAttribute("aria-valuenow", "48");
  await expect.poll(() => height(view)).toBe(before + 20);
  await page.keyboard.press("End");
  await expect(swclk).toHaveAttribute("aria-valuenow", "400");
  await page.keyboard.press("Home");
  await expect(swclk).toHaveAttribute("aria-valuenow", "24");
  // Enter puts the default back, and places no cursor.
  await page.keyboard.press("Enter");
  await expect(swclk).toHaveAttribute("aria-valuenow", "28");
  await expect(view.locator(".readout")).toHaveText("");
  await expect.poll(() => height(view)).toBe(before);
  await expect(handles.nth(1)).toHaveAttribute("aria-valuenow", "28");
});

test("dragging a handle sets its row's height without panning or placing a cursor; a double click puts the default back", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await ready(view);
  const swclk = view.locator('[part="row-handle"]').first();
  const window = await view.evaluate((v) => (v as HTMLElement & { window: unknown }).window);
  const box = (await swclk.boundingBox())!;
  const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y + 30, { steps: 3 });
  await page.mouse.up();
  await expect(swclk).toHaveAttribute("aria-valuenow", "58");
  expect(await view.evaluate((v) => (v as HTMLElement & { window: unknown }).window)).toEqual(window);
  await expect(view.locator(".readout")).toHaveText("");
  await swclk.dblclick();
  await expect(swclk).toHaveAttribute("aria-valuenow", "28");
});

test("another recording starts with every row at its default height", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await ready(view);
  await view.locator('[part="row-handle"]').first().focus();
  await page.keyboard.press("End");
  await view.evaluate((v) => v.setAttribute("src", "../../fixtures/swd50-cut.tickwave"));
  const handles = view.locator('[part="row-handle"]');
  await expect(handles).toHaveCount(3);
  for (const [i, h] of ["72", "72", "28"].entries()) await expect(handles.nth(i)).toHaveAttribute("aria-valuenow", h);
});

test("a row dragged taller never leaves the canvas blank when the page paints", async ({ page }) => {
  await page.goto(`${VIEW}?open&src=fixtures/swd50-cut.tickwave`);
  const view = page.locator("#v");
  await ready(view);
  await view.evaluate((v) => (v as HTMLElement & { settled(): Promise<void> }).settled());
  // An observer made after the view's runs after its layout, the last thing before the paint.
  await view.evaluate((v) => {
    const canvas = v.shadowRoot!.querySelector("canvas")!;
    const ctx = canvas.getContext("2d")!;
    const w = window as Window & { blank?: number; seen?: number };
    w.blank = 0;
    w.seen = 0;
    new ResizeObserver(() => {
      const d = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
      let ink = false;
      for (let i = 3; i < d.length && !ink; i += 4 * 97) ink = d[i] > 0;
      w.seen!++;
      if (!ink) w.blank!++;
    }).observe(canvas);
  });
  const box = (await view.locator('[part="row-handle"]').first().boundingBox())!;
  const [x, y] = [box.x + box.width / 2, box.y + box.height / 2];
  await page.mouse.move(x, y);
  await page.mouse.down();
  for (let k = 1; k <= 10; k++) await page.mouse.move(x, y + k * 4);
  await page.mouse.up();
  const { blank, seen } = await page.evaluate(() => {
    const w = window as Window & { blank?: number; seen?: number };
    return { blank: w.blank, seen: w.seen };
  });
  expect(seen).toBeGreaterThan(5);
  expect(blank).toBe(0);
});
