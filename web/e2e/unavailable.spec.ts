// A recording with channels this version can't read (FORMAT.md, "Unavailable channels"), on
// e2e/pages/view.html: fixtures/unavailable.tickwave, whose V, E, P and Q are unavailable and
// whose I, J, D and S read.

import { test, expect, type Locator } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/unavailable.tickwave";

/** The device pixels of row `i`'s plot area, right of where its words go, that hold any ink. */
async function inked(view: Locator, i: number): Promise<number> {
  return view.evaluate((v, i) => {
    const root = v.shadowRoot!;
    const canvas = root.querySelector("canvas")!;
    const handles = [...root.querySelectorAll<HTMLElement>('[part="row-handle"]')];
    const dpr = canvas.width / canvas.getBoundingClientRect().width;
    const top = i === 0 ? handles[0].offsetTop - 28 : handles[i - 1].offsetTop;
    const bottom = handles[i].offsetTop;
    const left = handles[i].offsetWidth + 200;
    const ctx = canvas.getContext("2d")!;
    const data = ctx.getImageData(Math.round(left * dpr), Math.round(top * dpr) + 2, Math.round((canvas.width / dpr - left - 20) * dpr), Math.round((bottom - top) * dpr) - 4).data;
    let n = 0;
    for (let k = 3; k < data.length; k += 4) if (data[k] > 0) n++;
    return n;
  }, i);
}

const LEFT_OUT = (id: string, channel: string) => `${id}: names channel "${channel}", which this version can't read`;

test("a channel this version can't read keeps its row in its place and draws nothing there; annotations that name one are left out, and say so", async ({ page }) => {
  const warned: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "warning" && m.text().startsWith('<tickwave-view id="v">')) warned.push(m.text());
  });
  await page.goto(VIEW);
  const view = page.locator("#v");
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  const labels = await view.locator('[part="row-handle"]').evaluateAll((hs) => hs.map((h) => h.getAttribute("aria-label")));
  expect(labels).toEqual(["V", "I", "J", "D", "E", "P", "Q", "S"].map((n) => `Height of ${n}`));
  // The rows of V, E, P and Q hold no trace past their words; I's, D's and S's do.
  const ink = await Promise.all([0, 1, 3, 4, 5, 6, 7].map((i) => inked(view, i)));
  expect(ink.map((n) => n > 0)).toEqual([false, true, true, false, false, false, true]);
  // The notice gives the first, the annotation past the length, and the console each.
  await expect(view.locator(".notice")).toHaveText("verge/z: starts at or past the length, 100000 (and 2 more problems; see the console)");
  expect(warned.filter((w) => w.includes("which this version can't read"))).toEqual([
    `<tickwave-view id="v">: ${LEFT_OUT("verge/a", "V")}`,
    `<tickwave-view id="v">: ${LEFT_OUT("verge/b", "E")}`,
  ]);
});
