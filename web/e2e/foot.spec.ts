// An inset keeps one height whatever its foot shows (AGENTS.md, "Nothing moves under the
// reader"): with nothing in the line, a reading at the pointer, a measure, and the key. The foot's
// figures are in a font of their own, which the page picks: here one whose line is taller than
// the text's, as a host page's may be, so a line that held only the text's font would grow.

import { test, expect, type Page } from "@playwright/test";

const FONTS = "--font-text: 'DejaVu Sans', sans-serif; --font-figures: 'C059', serif";
const INSET = (annotation: string) =>
  `/e2e/pages/inset.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml&annotation=${annotation}`;

/** The inset's, its foot's and its caption's boxes, to the pixel. */
function boxes(page: Page) {
  return page.locator("#i").evaluate((i) => {
    const r = (e: Element) => {
      const b = e.getBoundingClientRect();
      return [b.top, b.height].map((x) => Math.round(x * 100) / 100);
    };
    const root = i.shadowRoot!;
    return { inset: r(i), foot: r(root.querySelector(".foot")!), caption: r(root.querySelector("figcaption")!) };
  });
}

async function open(page: Page, annotation: string, caption: RegExp): Promise<void> {
  await page.goto(INSET(annotation));
  await page.locator("#i").evaluate((i, fonts) => i.setAttribute("style", `${i.getAttribute("style") ?? ""}; ${fonts}`), FONTS);
  await expect(page.locator("#i figcaption")).toHaveText(caption);
  await page.evaluate(() => document.fonts.ready);
}

/** The pointer over the middle of the inset's plot, reading SWDIO there. */
async function point(page: Page): Promise<void> {
  const b = (await page.locator("#i canvas").boundingBox())!;
  await page.mouse.move(b.x + b.width * 0.55, b.y + b.height * 0.75);
  await expect(page.locator("#i .line.reads")).toHaveCount(1);
}

test("an inset with a measure keeps its boxes as the pointer reads a row and leaves it", async ({ page }) => {
  await open(page, "dpidr-read", /debug port's ID register/);
  await expect(page.locator("#i .measure")).toContainText("SWCLK");
  await expect(page.locator("#i .key > span")).toHaveCount(2);
  const measured = await boxes(page);
  await point(page);
  const read = await boxes(page);
  await page.mouse.move(0, 0);
  await expect(page.locator("#i .line.reads")).toHaveCount(0);
  expect([read, await boxes(page)]).toEqual([measured, measured]);
});

test("an inset without a measure keeps its boxes as a reading brings figures in, and its foot is the same height as one with a measure", async ({ page }) => {
  await open(page, "dpidr-read", /debug port's ID register/);
  const withMeasure = (await boxes(page)).foot[1];
  await open(page, "attach", /Attaching/);
  await expect(page.locator("#i .key > span")).toHaveCount(2);
  const nothing = await boxes(page);
  await point(page);
  expect(await boxes(page)).toEqual(nothing);
  expect(nothing.foot[1]).toBe(withMeasure);
});

test("in a 288 px panel, as on a 320 px phone, Open shares the foot's first line: the inset is as tall as one that opens nothing, and stays so as the pointer reads a row", async ({ page }) => {
  const narrow = async (query: string) => {
    await page.goto(INSET("dpidr-read") + query);
    await page.locator("#i").evaluate((i, fonts) => i.setAttribute("style", `max-width: 288px; ${fonts}`), FONTS);
    await expect(page.locator("#i figcaption")).toHaveText(/debug port's ID register/);
    await page.evaluate(() => document.fonts.ready);
    await expect(page.locator("#i .measure")).toContainText("SWCLK");
    await expect(page.locator("#i .key > span")).toHaveCount(2);
  };
  await narrow("");
  await expect(page.locator("#i .open")).toBeHidden();
  const plain = await boxes(page);
  await narrow(`&href=${encodeURIComponent("/dist/site/open.html")}`);
  await expect(page.locator("#i .open")).toBeVisible();
  expect(await boxes(page)).toEqual(plain);
  await point(page);
  expect(await boxes(page)).toEqual(plain);
});
