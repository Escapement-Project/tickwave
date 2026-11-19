// A page loads the elements from a folder on its own site or on another one, as from a release's
// directory on tickwave's site. Either way the decoder's worker starts, and reads the page's own
// recording and sidecar: from its own URL on the page's site, from a module the page makes when
// the files are on another.

import { test, expect } from "@playwright/test";

for (const [where, from, worker] of [
  ["its own site", "../../dist/", /^http:\/\/127\.0\.0\.1:8124\/dist\/tickwave-worker\.js$/],
  ["another site", "http://127.0.0.1:8126/dist/", /^blob:http:\/\/127\.0\.0\.1:8124\//],
] as const) {
  test(`elements loaded from ${where} start their decoder and read the page's recording and sidecar`, async ({ page }) => {
    const errors: string[] = [];
    const workers: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("worker", (w) => workers.push(w.url()));
    await page.goto(`/e2e/pages/origin.html?from=${encodeURIComponent(from)}`);
    await expect(page.locator("#i figcaption")).toHaveText(/debug port's ID register/);
    await expect(page.locator("#i .measure")).toContainText("SWCLK");
    await expect(page.locator("#v")).toContainText("SWDIO");
    expect(errors).toEqual([]);
    expect(workers).toHaveLength(1);
    expect(workers[0]).toMatch(worker);
  });
}
