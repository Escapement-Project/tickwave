// An inset shows the channels its recording's stored view names (ANNOTATIONS.md, "The stored
// view"), as a view does, where neither it nor the view its reference cites names any: a bundled
// recording's, and a separate sidecar's. Its span stays its annotation's.

import { readFileSync } from "node:fs";
import { test, expect, type Locator, type Page } from "@playwright/test";

/** roles.annotations.toml bundled into identify-swd: its stored view names SWDIO, then SWCLK, the
 * reverse of the recording's order. */
const BUNDLE = "../../fixtures/roles.bundle.tickwave";
const TURNAROUND = "read-request-turnaround";
const SWD = "../../fixtures/identify-swd.tickwave";

/** identify-swd's sidecar with a stored view naming `channels`, served at fixtures/stored.annotations.toml. */
async function storing(page: Page, channels: string[]): Promise<string> {
  const toml = `${readFileSync("fixtures/identify-swd.annotations.toml", "utf8")}\n[view]\nchannels = ${JSON.stringify(channels)}\n`;
  await page.route("**/fixtures/stored.annotations.toml", (route) => route.fulfill({ contentType: "text/plain", body: toml }));
  return "../../fixtures/stored.annotations.toml";
}

/** Waits until `inset` has drawn its rows whole, its sweep off. */
async function settle(inset: Locator): Promise<void> {
  await expect.poll(() => inset.evaluate((i) => i.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  await inset.evaluate(async (i) => {
    await (i as unknown as { settled(): Promise<void> }).settled();
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    await (i as unknown as { settled(): Promise<void> }).settled();
  });
}

/** `inset`'s canvas: its pixels, and its height in CSS pixels. */
const canvas = (inset: Locator) =>
  inset.evaluate((i) => {
    const c = i.shadowRoot!.querySelector("canvas")!;
    return { pixels: c.toDataURL(), height: c.getBoundingClientRect().height };
  });

/** An inset with `attributes`, 440 px wide, drawn whole: its canvas. It is taken off the page
 * after, so the next one is on screen too. */
async function inset(page: Page, attributes: Record<string, string>): Promise<{ pixels: string; height: number }> {
  await page.evaluate((attributes) => {
    const i = document.createElement("tickwave-inset");
    i.id = "made";
    for (const [k, v] of Object.entries(attributes)) i.setAttribute(k, v);
    i.setAttribute("sweep", "off");
    i.style.width = "440px";
    document.body.prepend(i);
  }, attributes);
  const made = page.locator("#made");
  await settle(made);
  const drawn = await canvas(made);
  await made.evaluate((i) => i.remove());
  return drawn;
}

test("an inset of a bundled recording draws the channels its stored view names, in its order; its own channels win", async ({ page }) => {
  await page.goto("/e2e/pages/view.html");
  const at = { src: BUNDLE, annotation: TURNAROUND };
  const stored = await inset(page, at);
  const named = await inset(page, { ...at, channels: "SWDIO SWCLK" });
  const recorded = await inset(page, { ...at, channels: "SWCLK SWDIO" });
  expect(stored.pixels).toBe(named.pixels);
  // The recording's order draws otherwise, so the comparison can tell them apart.
  expect(recorded.pixels).not.toBe(named.pixels);
  // A channel the inset names wins over the stored ones: one row, not two.
  const one = await inset(page, { ...at, channels: "SWCLK" });
  expect(one.height).toBeLessThan(stored.height);
});

test("an inset whose sidecar stores some of the channels draws those alone", async ({ page }) => {
  await page.goto("/e2e/pages/view.html");
  const annotations = await storing(page, ["SWDIO"]);
  const at = { src: SWD, annotations, annotation: "dpidr-read" };
  const stored = await inset(page, at);
  const named = await inset(page, { ...at, channels: "SWDIO" });
  const every = await inset(page, { src: SWD, annotations: "../../fixtures/identify-swd.annotations.toml", annotation: "dpidr-read" });
  expect(stored.pixels).toBe(named.pixels);
  expect(stored.height).toBeLessThan(every.height);
});

/** The inset a reference to the view of the bundle shows, with `channels` on the view: its canvas. */
async function refInset(page: Page, channels: string | null): Promise<{ pixels: string; height: number }> {
  // A reference's inset sweeps in, unless the reader asks for less motion.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`/e2e/pages/view.html?src=fixtures/roles.bundle.tickwave${channels ? `&channels=${encodeURIComponent(channels)}` : ""}`);
  await page.evaluate((id) => {
    const r = document.createElement("tickwave-ref");
    r.id = "r";
    r.setAttribute("for", "v");
    r.setAttribute("annotation", id);
    r.textContent = "the turnaround";
    document.body.prepend(r);
  }, TURNAROUND);
  const ref = page.locator("#r");
  await ref.hover();
  const shown = ref.locator("tickwave-inset");
  if (channels === null) await expect(shown).not.toHaveAttribute("channels");
  else await expect(shown).toHaveAttribute("channels", channels);
  await settle(shown);
  return canvas(shown);
}

test("a reference's inset of a bundled recording draws the stored channels; its view's channels win", async ({ page }) => {
  const stored = await refInset(page, null);
  const named = await refInset(page, "SWDIO SWCLK");
  const recorded = await refInset(page, "SWCLK SWDIO");
  expect(stored.pixels).toBe(named.pixels);
  expect(recorded.pixels).not.toBe(named.pixels);
  expect((await refInset(page, "SWCLK")).height).toBeLessThan(stored.height);
});

test("a stored channel the recording lacks is reported by an inset as a view reports it", async ({ page }) => {
  // The sidecar's check finds it, so neither element has a stored view naming it: each reports
  // the sidecar's problem.
  const annotations = await storing(page, ["SWDIO", "NOPE"]);
  await page.goto(`/e2e/pages/view.html?open&annotations=fixtures/stored.annotations.toml`);
  const view = page.locator("#v");
  await expect(view.locator(".notice")).toContainText("NOPE", { timeout: 20_000 });
  await view.evaluate((v) => (v as unknown as { settled(): Promise<void> }).settled());
  const said = (await view.locator(".notice").textContent())!;
  expect(said).toBe('view: no channel "NOPE"');
  await page.evaluate((annotations) => {
    const i = document.createElement("tickwave-inset");
    i.id = "made";
    i.setAttribute("src", "../../fixtures/identify-swd.tickwave");
    i.setAttribute("annotations", annotations);
    i.setAttribute("annotation", "dpidr-read");
    i.setAttribute("sweep", "off");
    i.style.width = "440px";
    document.body.prepend(i);
  }, annotations);
  // Its first problem is the view's; a sidecar that doesn't check holds no annotation to show.
  await expect(page.locator("#made .notice")).toHaveText(`${said} (and 1 more problems; see the console)`, { timeout: 20_000 });
});
