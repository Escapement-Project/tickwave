// A page loads the elements from a folder on its own site or on another one, as from a release's
// directory on tickwave's site. Either way the decoder's worker starts, and reads the page's own
// recording and sidecar: from its own URL on the page's site, from a module the page makes when
// the files are on another. And a page shows a recording kept on another site that serves it as
// GitHub Pages does.

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

// Port 8126 refuses the preflight a read from the end needs and hides Content-Range, as GitHub
// Pages does to a page on another site. The decoder reads the size from a read from the start,
// then the end and the blocks by explicit ranges, none of which needs a preflight.
for (const [file, size] of [["swd50-cut", 74766], ["identify-swd", 4715]] as const) {
  test(`a recording on another site that serves as GitHub Pages does shows: ${file}`, async ({ page }, info) => {
    const tag = `site-${file}-${info.project.name}-${info.repeatEachIndex}-${Date.now()}`;
    await page.goto(`/e2e/pages/view.html?open&site=http://127.0.0.1:8126&src=fixtures/${file}.tickwave&tag=${tag}`);
    const view = page.locator("#v");
    await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")?.style.height ?? ""), { timeout: 20_000 }).not.toBe("");
    await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
    await expect(view.locator('[part="notice"]')).toBeHidden();
    const log = (await (await fetch(`http://127.0.0.1:8124/__log?tag=${tag}`)).json()) as { port: number; url: string; range: string | null }[];
    const reads = log.filter((e) => e.url.startsWith(`/fixtures/${file}.tickwave`));
    expect(reads.every((e) => e.port === 8126 && e.range !== null && !e.range.startsWith("bytes=-"))).toBe(true);
    // Longer than the first read: its end next. No longer: the first read held it all.
    expect(reads.slice(0, 2).map((e) => e.range)).toEqual(size > 32 * 1024 ? ["bytes=0-", `bytes=${size - 32 * 1024}-${size - 1}`] : ["bytes=0-"]);
  });
}
