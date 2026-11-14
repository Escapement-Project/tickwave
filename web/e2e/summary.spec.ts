// A view whose page writes its summary into it, laid out by tickwave.css before tickwave.js defines
// it: one height and one title width from the first frame to the footer's arrival, and a console
// warning where the page's text is not the recording's.

import { expect, test, type Page } from "@playwright/test";

const PAGE = "/e2e/pages/summary.html";
/** As a page writes them; the view's own have no-break spaces between figure and unit. */
const FACTS = "524 ms; 2 channels: SWCLK, SWDIO; 4.2 KiB";
const OWN_FACTS = "524\u00a0ms; 2 channels: SWCLK, SWDIO; 4.2\u00a0KiB";
/** A recording without a title of its own takes its file's name, so this is identify-swd's title. */
const TITLE = "SWD reads through the probe at its fastest clock, against the stock firmware.tickwave";
const SRC = `named/fixtures/identify-swd.tickwave/${encodeURIComponent(TITLE)}`;

interface Frame {
  defined: boolean;
  summary: boolean;
  height: number;
  /** The title's grid cell: the page's span while undefined, the view's own cell once defined. */
  titleWidth: number;
  /** Each line box of the page's title and facts text, which are the same nodes in both states. */
  lines: string;
}

/** Records every frame from the first: the view's height and its title cell's width. */
async function recordFrames(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const w = window as unknown as { frames: Frame[] };
    w.frames = [];
    const tick = () => {
      const el = document.getElementById("v") as (HTMLElement & { summary?: unknown }) | null;
      if (el) {
        const defined = el.matches(":defined");
        const cell = defined ? el.shadowRoot!.querySelector(".title") : el.querySelector("[slot=title]");
        const lines = [...el.querySelectorAll("[slot]")].map((span) => {
          const range = document.createRange();
          range.selectNodeContents(span);
          return [...range.getClientRects()].map((r) => `${r.left},${r.right},${r.top},${r.bottom}`).join(" ");
        });
        w.frames.push({ defined, summary: !!el.summary, height: el.getBoundingClientRect().height, titleWidth: cell?.getBoundingClientRect().width ?? -1, lines: lines.join(" | ") });
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

const frames = (page: Page) => page.evaluate(() => (window as unknown as { frames: Frame[] }).frames);

for (const style of ["", "verge", "bench"]) {
  for (const width of [1280, 390]) {
    test(`a view keeps one height from its first frame through its definition and its footer: ${style || "defaults"}, ${width} px`, async ({ page }, info) => {
      const warnings: string[] = [];
      page.on("console", (m) => {
        if (m.type() === "warning") warnings.push(m.text());
      });
      await page.setViewportSize({ width, height: 800 });
      await recordFrames(page);
      const tag = `slots-${style}-${width}-${info.project.name}-${info.repeatEachIndex}-${Date.now()}`;
      const q = new URLSearchParams({ src: SRC, title: TITLE, facts: FACTS, style, tag, after: "0", stall: "1000" });
      await page.goto(`${PAGE}?${q}`);
      await expect.poll(async () => (await frames(page)).length).toBeGreaterThan(20);
      await page.evaluate(() => (window as unknown as { define: () => Promise<unknown> }).define());
      // The server holds the footer a second, so the view is defined for many frames without it.
      await page.waitForFunction(() => !!(document.getElementById("v") as HTMLElement & { summary?: unknown }).summary, null, { timeout: 10_000 });
      const settledAt = (await frames(page)).length;
      await expect.poll(async () => (await frames(page)).length).toBeGreaterThan(settledAt + 10);
      const all = await frames(page);
      expect(all.filter((f) => !f.defined).length).toBeGreaterThan(20);
      expect(all.filter((f) => f.defined && !f.summary).length).toBeGreaterThan(20);
      expect(all.filter((f) => f.summary).length).toBeGreaterThan(10);
      expect(new Set(all.map((f) => f.height))).toEqual(new Set([all[0].height]));
      expect(new Set(all.map((f) => f.titleWidth))).toEqual(new Set([all[0].titleWidth]));
      expect(new Set(all.map((f) => f.lines))).toEqual(new Set([all[0].lines]));
      // The title wraps at the narrow width, so a cell of another width would break it elsewhere.
      const lines = await page.evaluate(() => {
        const t = document.querySelector("#v [slot=title]")!;
        return Math.round(t.getBoundingClientRect().height / parseFloat(getComputedStyle(t).lineHeight));
      });
      expect(lines > 1).toBe(width === 390);
      expect(await page.evaluate(() => (document.getElementById("v") as HTMLElement & { summary: unknown }).summary)).toEqual({ title: TITLE, facts: OWN_FACTS, matches: true });
      expect(warnings).toEqual([]);
    });
  }
}

test("a page's summary that is not the recording's is warned of once; slotted anew, it is compared again", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "warning") warnings.push(m.text());
  });
  await page.goto(`${PAGE}?${new URLSearchParams({ facts: "1 s; 1 channel: SWCLK" })}`);
  await page.evaluate(() => {
    const w = window as unknown as { summaries: unknown[] };
    w.summaries = [];
    document.getElementById("v")!.addEventListener("tickwave-summary", (e) => w.summaries.push((e as CustomEvent).detail));
    return (window as unknown as { define: () => Promise<unknown> }).define();
  });
  const summaries = () => page.evaluate(() => (window as unknown as { summaries: unknown[] }).summaries);
  // Only the facts are slotted: the title is the view's own, and is not compared.
  await expect.poll(summaries).toEqual([{ title: "identify-swd.tickwave", facts: OWN_FACTS, matches: false }]);
  expect(warnings).toEqual([`tickwave-view#v src="../../fixtures/identify-swd.tickwave": the page's facts "1 s; 1 channel: SWCLK" differ from the recording's, "${OWN_FACTS}"`]);
  // Opening and closing renders the summary again; nothing new is said.
  const view = page.locator("#v");
  await view.locator(".toggle").click();
  await view.locator(".toggle").click();
  expect(warnings.length).toBe(1);
  expect((await summaries()).length).toBe(1);
  // The page writes the recording's facts, spaced its own way: any run of spaces, no-break ones
  // included, is one space.
  await page.evaluate((facts) => {
    const span = document.createElement("span");
    span.slot = "facts";
    span.textContent = `\n  ${facts.replaceAll(" ", "  ")}\n`;
    document.querySelector("#v [slot=facts]")!.replaceWith(span);
  }, FACTS);
  await expect.poll(summaries).toEqual([
    { title: "identify-swd.tickwave", facts: OWN_FACTS, matches: false },
    { title: "identify-swd.tickwave", facts: OWN_FACTS, matches: true },
  ]);
  expect(warnings.length).toBe(1);
  // The button names the title the page shows.
  await expect(view.locator(".toggle")).toHaveAttribute("aria-label", "Open identify-swd.tickwave");
});

test("a page's title names the view's button, and a wrong one is warned of", async ({ page }) => {
  const warnings: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "warning") warnings.push(m.text());
  });
  await page.goto(`${PAGE}?${new URLSearchParams({ title: "Reads", facts: FACTS })}`);
  await page.evaluate(() => (window as unknown as { define: () => Promise<unknown> }).define());
  await expect(page.locator("#v .toggle")).toHaveAttribute("aria-label", "Open Reads");
  await page.waitForFunction(() => !!(document.getElementById("v") as HTMLElement & { summary?: unknown }).summary);
  expect(warnings).toEqual([`tickwave-view#v src="../../fixtures/identify-swd.tickwave": the page's title "Reads" differs from the recording's, "identify-swd.tickwave"`]);
});

test("a view of some of the channels names them, out of the recording's", async ({ page }) => {
  await page.goto(`${PAGE}?${new URLSearchParams({ channels: "SWDIO", facts: "524 ms; 1 of 2 channels: SWDIO; 4.2 KiB" })}`);
  await page.evaluate(() => (window as unknown as { define: () => Promise<unknown> }).define());
  await page.waitForFunction(() => !!(document.getElementById("v") as HTMLElement & { summary?: unknown }).summary);
  expect(await page.evaluate(() => (document.getElementById("v") as HTMLElement & { summary: unknown }).summary)).toEqual({
    title: "identify-swd.tickwave",
    facts: "524\u00a0ms; 1 of 2 channels: SWDIO; 4.2\u00a0KiB",
    matches: true,
  });
});

test("an undefined view that is hidden takes no room", async ({ page }) => {
  await page.goto(`${PAGE}?${new URLSearchParams({ title: TITLE, facts: FACTS, hidden: "" })}`);
  expect(await page.locator("#v").evaluate((v) => [v.matches(":defined"), v.getBoundingClientRect().height])).toEqual([false, 0]);
});

test("the Open button is as wide as Close, so it stays where the reader pressed it", async ({ page }) => {
  await page.goto(`${PAGE}?${new URLSearchParams({ title: TITLE, facts: FACTS })}`);
  await page.evaluate(() => (window as unknown as { define: () => Promise<unknown> }).define());
  const toggle = page.locator("#v .toggle");
  await expect(toggle).toHaveText("Open");
  const open = await toggle.boundingBox();
  await toggle.click();
  await expect(toggle).toHaveText("Close");
  expect(await toggle.boundingBox()).toEqual(open);
});
