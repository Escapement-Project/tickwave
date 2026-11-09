// The elements as a page changes them under the reader: a new recording or sidecar, a call made
// before the head has arrived, a load that fails, and the focus as views open and close. On
// e2e/pages/view.html, one view of identify-swd.tickwave and a reference to it.

import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html";
const SIDECAR = "../../fixtures/identify-swd.annotations.toml";

/** The view's canvas height once the plot has laid it out: a canvas is 150 px before then. */
async function drawn(view: Locator): Promise<number> {
  await expect.poll(() => view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  return view.evaluate((v) => v.shadowRoot!.querySelector("canvas")!.getBoundingClientRect().height);
}
const readout = (view: Locator) => view.locator(".readout");
const notice = (view: Locator) => view.locator('[part="notice"]');

/** A click in the first row of the plot, `x` CSS pixels from its left. */
async function clickRow(page: Page, view: Locator, x: number): Promise<void> {
  const box = (await view.locator(".plot").boundingBox())!;
  await page.mouse.click(box.x + x, box.y + 60);
}

test("another sidecar for the same recording draws its own lane", async ({ page }) => {
  await page.goto(`${VIEW}?open&annotations=fixtures/identify-swd.annotations.toml`);
  const view = page.locator("#v");
  // The view lays its plot out once the recording and its sidecar are both in: the lane is there.
  const withLane = await drawn(view);
  await view.evaluate((v) => v.removeAttribute("annotations"));
  // identify-swd.tickwave carries no sidecar of its own: the lane goes, and the plot with it.
  await expect.poll(() => drawn(view)).toBeLessThan(withLane - 40);
  await view.evaluate((v, s) => v.setAttribute("annotations", s), SIDECAR);
  await expect.poll(() => drawn(view)).toBe(withLane);
});

test("a new recording starts without the last one's cursors or readout", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await drawn(view);
  await clickRow(page, view, 300);
  await expect(readout(view)).toContainText("A ");
  await view.evaluate((v) => v.setAttribute("src", "../../fixtures/identify-jtag.tickwave"));
  await expect(view.locator(".toolbar .title")).not.toHaveText(/swd/i, { timeout: 20_000 });
  await expect(readout(view)).toBeEmpty();
});

test("a click on a channel's name places no cursor", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const view = page.locator("#v");
  await drawn(view);
  await clickRow(page, view, 8);
  await page.waitForTimeout(200);
  await expect(readout(view)).toBeEmpty();
});

test("setWindow before the head arrives shows that window once it has", async ({ page }) => {
  await page.goto(`${VIEW}?open`);
  const window = await page.evaluate(async () => {
    const v = document.createElement("tickwave-view") as HTMLElement & { setWindow(a: string, b: string): void; window: { start: number; end: number } };
    v.setAttribute("src", "../../fixtures/identify-swd.tickwave");
    v.setAttribute("open", "");
    document.body.append(v);
    v.setWindow("380 ms", "381 ms");
    await new Promise((r) => v.addEventListener("tickwave-ready", r, { once: true }));
    return v.window;
  });
  // 1 ns ticks.
  expect(window).toEqual({ start: 380_000_000, end: 381_000_000 });
});

test("a failed load is tried again, and a new recording's view drops the old notice", async ({ page }, info) => {
  const tag = `retry-${info.project.name}-${Date.now()}`;
  await page.goto(`${VIEW}?open&src=fixtures/missing.tickwave&tag=${tag}`);
  const view = page.locator("#v");
  await expect(notice(view)).toContainText("404");
  const requests = async () => ((await (await fetch(`http://127.0.0.1:8124/__log?tag=${tag}`)).json()) as unknown[]).length;
  const before = await requests();
  await view.locator(".close").click();
  await view.locator(".open").click();
  await expect.poll(requests).toBeGreaterThan(before);
  await view.evaluate((v) => v.setAttribute("src", "../../fixtures/identify-swd.tickwave"));
  await drawn(view);
  await expect(notice(view)).toBeHidden();
});

test("Open and Close move the focus where the reader went, and each names its recording", async ({ page }) => {
  await page.goto(VIEW);
  const view = page.locator("#v");
  const openButton = view.locator(".open");
  await expect(openButton).toHaveAttribute("aria-label", /^Open .+/);
  await openButton.focus();
  await page.keyboard.press("Enter");
  await drawn(view);
  expect(await view.evaluate((v) => v.shadowRoot!.activeElement?.className)).toBe("plot");
  const close = view.locator(".close");
  await expect(close).toHaveAttribute("aria-label", /^Close .+/);
  await close.focus();
  await page.keyboard.press("Enter");
  expect(await view.evaluate((v) => v.shadowRoot!.activeElement?.className)).toBe("open");
});

test("with the page giving the view its height, a readout appearing keeps the plot that tall", async ({ page }) => {
  await page.goto(`${VIEW}?open&src=fixtures/swd50-cut.tickwave`);
  const view = page.locator("#v");
  await view.evaluate((v) => {
    v.style.display = "flex";
    v.style.flexDirection = "column";
    v.style.height = "520px";
  });
  await drawn(view);
  await expect.poll(() => drawn(view)).toBeGreaterThan(300);
  const tall = await drawn(view);
  await clickRow(page, view, 300);
  await expect(readout(view)).toContainText("A ");
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  // The readout takes its lines from the plot; the plot does not fall back to its own height.
  expect(await drawn(view)).toBeGreaterThan(tall - 120);
});

test("a reference answers Space as a button does, and its annotation describes it", async ({ page }) => {
  await page.goto(`${VIEW}?annotations=fixtures/identify-swd.annotations.toml`);
  const ref = page.locator("tickwave-ref");
  await ref.scrollIntoViewIfNeeded();
  // The label, then the text: what the inset shows, which a screen reader cannot reach.
  await expect(ref).toHaveAttribute("aria-description", /^Reading the debug port's ID register\. A DP read of DPIDR\./, { timeout: 20_000 });
  await ref.focus();
  await page.keyboard.press(" ");
  await expect(page.locator("#v")).toHaveAttribute("open", "");
});

test("a URL that is not a .tickwave file is refused once the page has loaded, not left waiting", async ({ page }) => {
  await page.goto(`${VIEW}?open&src=fixtures/figure1.chart.json`);
  await expect(notice(page.locator("#v"))).toContainText("no loader reads it", { timeout: 10_000 });
  // The extension in capitals is still one: this file is missing, so the server says so.
  await page.goto(`${VIEW}?open&src=fixtures/IDENTIFY-SWD.TICKWAVE`);
  await expect(notice(page.locator("#v"))).toContainText("404", { timeout: 10_000 });
});
