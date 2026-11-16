// Analog samples whose places fall between ticks, on e2e/pages/view.html: fixtures/phased.tickwave
// (crates/format/examples/phased.rs). A's samples sit a phase of 3/4 of a tick past a 5/2-tick
// grid from tick 100, at 100.75, 103.25, ...; after a break at 140, a quarter tick past it, at
// 140.25, 142.75, ...; B's at 100, 102.5, ... A sample's value holds from the tick its place maps
// to, the tick at or before it.

import { test, expect, type Locator } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?open&src=fixtures/phased.tickwave";

/** Each analog channel's value at cursor A, placed at `tick`, the middle of a window about it. */
async function at(view: Locator, tick: number): Promise<Record<string, string>> {
  await view.evaluate(
    (v, t) => {
      const view = v as HTMLElement & { setWindow(a: number, b: number): void; settled(): Promise<void> };
      view.setWindow(t - 50, t + 50);
      return view.settled();
    },
    tick,
  );
  const plot = view.locator(".plot");
  await plot.focus();
  await plot.press("Escape");
  await plot.press("Enter");
  const lines = view.locator(".readout .line");
  await expect(lines.first()).toContainText("A ");
  const out: Record<string, string> = {};
  for (const t of await lines.allTextContents()) {
    const m = /^(\S+) A (.+)$/.exec(t.replace(/ /g, " "));
    if (m) out[m[1]] = m[2];
  }
  return out;
}

test("a sample whose place falls between ticks holds from the tick its place maps to, a break's own phase included", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  await view.evaluate((v) => (v as HTMLElement & { settled(): Promise<void> }).settled());
  // At 102 A's first sample still holds, its second lying at 103.25; B's second lies at 102.5.
  expect(await at(view, 102)).toEqual({ A: "1.000 V", B: "2.001 V" });
  expect(await at(view, 103)).toEqual({ A: "1.001 V", B: "2.001 V" });
  // After the break, A's twelfth sample lies at 142.75, its own phase of a quarter tick: not at
  // 143.25, where the declared phase would put it.
  expect(await at(view, 142)).toEqual({ A: "1.011 V", B: "2.017 V" });
});
