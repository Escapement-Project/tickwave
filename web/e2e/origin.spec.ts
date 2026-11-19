// A page loads the elements from a folder on its own site or on another one, as from a release's
// directory on tickwave's site. Either way the decoder's worker starts, from beside tickwave.js,
// and reads the page's own recording and sidecar.

import { test, expect } from "@playwright/test";

for (const [where, from, port] of [
  ["its own site", "../../dist/", 8124],
  ["another site", "http://127.0.0.1:8126/dist/", 8126],
] as const) {
  test(`elements loaded from ${where} start their decoder and read the page's recording and sidecar`, async ({ page, request }) => {
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.goto(`/e2e/pages/origin.html?from=${encodeURIComponent(from)}`);
    await expect(page.locator("#i figcaption")).toHaveText(/debug port's ID register/);
    await expect(page.locator("#i .measure")).toContainText("SWCLK");
    await expect(page.locator("#v")).toContainText("SWDIO");
    expect(errors).toEqual([]);
    for (const file of ["tickwave-worker.js", "tickwave.wasm"]) {
      const fetched = (await (await request.get(`/__log?tag=${file}`)).json()) as { port: number }[];
      expect(fetched.map((e) => e.port), file).toContain(port);
    }
  });
}
