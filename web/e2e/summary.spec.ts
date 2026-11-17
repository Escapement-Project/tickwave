// A view whose page writes its summary into it, laid out by tickwave.css before tickwave.js defines
// it: one height and one title width from the first frame to the footer's arrival, and a console
// warning where the page's text is not the recording's.

import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";
import { layout } from "../test/layout.ts";

const PAGE = "/e2e/pages/summary.html";
/** As a page writes them; the view's own have no-break spaces between figure and unit. */
const FACTS = "524 ms; 2 channels: SWCLK, SWDIO; 4.6 KiB";
const OWN_FACTS = "524\u00a0ms; 2 channels: SWCLK, SWDIO; 4.6\u00a0KiB";
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

// With download links, the undefined view keeps their room too: the recording's link, and with
// `annotations` the sidecar's, beside the button.
const LINKS: [string, Record<string, string>][] = [
  ["", {}],
  ["download links", { download: "" }],
  ["download links with the sidecar's", { download: "", annotations: "fixtures/identify-swd.annotations.toml" }],
];

for (const [links, attributes] of LINKS)
for (const style of links ? ["", "verge"] : ["", "verge", "bench"]) {
  for (const width of [1280, 390]) {
    test(`a view keeps one height from its first frame through its definition and its footer: ${style || "defaults"}, ${width} px${links ? `, ${links}` : ""}`, async ({ page }, info) => {
      const warnings: string[] = [];
      page.on("console", (m) => {
        if (m.type() === "warning") warnings.push(m.text());
      });
      await page.setViewportSize({ width, height: 800 });
      await recordFrames(page);
      const tag = `slots-${style}-${width}-${Object.keys(attributes).length}-${info.project.name}-${info.repeatEachIndex}-${Date.now()}`;
      const q = new URLSearchParams({ src: SRC, title: TITLE, facts: FACTS, style, tag, after: "0", stall: "1000", ...attributes });
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
      // The links are there once it is defined, and the button where it was.
      if (links) await expect(page.locator("#v").locator(".download").first()).toBeVisible();
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
  await page.goto(`${PAGE}?${new URLSearchParams({ channels: "SWDIO", facts: "524 ms; 1 of 2 channels: SWDIO; 4.6 KiB" })}`);
  await page.evaluate(() => (window as unknown as { define: () => Promise<unknown> }).define());
  await page.waitForFunction(() => !!(document.getElementById("v") as HTMLElement & { summary?: unknown }).summary);
  expect(await page.evaluate(() => (document.getElementById("v") as HTMLElement & { summary: unknown }).summary)).toEqual({
    title: "identify-swd.tickwave",
    facts: "524\u00a0ms; 1 of 2 channels: SWDIO; 4.6\u00a0KiB",
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

// A bundled recording whose stored view names its channels: its facts count those, so the view
// compares, fills its facts and sends its summary only once the bundle is read.

const BUNDLE = "fixtures/roles.bundle.tickwave";
/** The stored view's channels, in its order, and every channel, in the recording's. */
const STORED_FACTS = "524 ms; 2 channels: SWDIO, SWCLK; 14 KiB";
const ALL_FACTS = "524 ms; 2 channels: SWCLK, SWDIO; 14 KiB";
const own = (facts: string) => facts.replace(/ (ms|KiB)/g, " $1");

/**
 * Holds every read of the bundle record for `ms`, then answers it, or fails it where `fail`. With
 * a first read of 256 bytes, the head comes in reads of its own, so the head is in well before the
 * bundle. Returns how many reads met the bundle.
 */
async function holdBundle(page: Page, ms: number, fail = false): Promise<() => number> {
  const [from, to] = layout(new Uint8Array(readFileSync(BUNDLE))).bundle!;
  let met = 0;
  await page.route("**/roles.bundle.tickwave*", async (route) => {
    const r = /^bytes=(\d+)-(\d+)$/.exec(route.request().headers()["range"] ?? "");
    if (r && Number(r[1]) < to && from <= Number(r[2])) {
      met++;
      await new Promise((go) => setTimeout(go, ms));
      if (fail) return route.fulfill({ status: 500, body: "" });
    }
    await route.continue();
  });
  return () => met;
}

/** Defines the view, recording each summary it sends and each warning. */
async function define(page: Page): Promise<{ summaries: () => Promise<unknown[]>; warnings: string[] }> {
  const warnings: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "warning") warnings.push(m.text());
  });
  await page.evaluate(() => {
    const w = window as unknown as { summaries: unknown[] };
    w.summaries = [];
    document.getElementById("v")!.addEventListener("tickwave-summary", (e) => w.summaries.push((e as CustomEvent).detail));
    return (window as unknown as { define: () => Promise<unknown> }).define();
  });
  return { summaries: () => page.evaluate(() => (window as unknown as { summaries: unknown[] }).summaries), warnings };
}

test("a bundled recording that stores its channels, with the page's facts for them, warns of nothing and matches from its first summary", async ({ page }) => {
  const met = await holdBundle(page, 400);
  await page.goto(`${PAGE}?${new URLSearchParams({ src: BUNDLE, facts: STORED_FACTS, firstRead: "256" })}`);
  const { summaries, warnings } = await define(page);
  await expect.poll(summaries, { timeout: 20_000 }).toEqual([{ title: "roles.bundle.tickwave", facts: own(STORED_FACTS), matches: true }]);
  expect(met()).toBeGreaterThan(0);
  // Opening reads the sidecar again, and says nothing new.
  await page.locator("#v .toggle").click();
  await expect.poll(() => page.locator("#v").evaluate((v) => v.shadowRoot!.querySelector("canvas")!.style.height), { timeout: 20_000 }).not.toBe("");
  expect(await summaries()).toHaveLength(1);
  expect(warnings).toEqual([]);
});

test("with no facts slotted, the view's own name the stored channels from the first it shows", async ({ page }) => {
  const met = await holdBundle(page, 400);
  // Each text the facts' slot shows, in turn, from the page's first frame.
  await page.addInitScript(() => {
    const w = window as unknown as { shown: string[] };
    w.shown = [];
    const tick = () => {
      const text = document.getElementById("v")?.shadowRoot?.querySelector("slot[name=facts]")?.textContent ?? "";
      if (text !== (w.shown.at(-1) ?? "")) w.shown.push(text);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  await page.goto(`${PAGE}?${new URLSearchParams({ src: BUNDLE, firstRead: "256" })}`);
  const { summaries } = await define(page);
  await expect.poll(summaries, { timeout: 20_000 }).toHaveLength(1);
  expect(met()).toBeGreaterThan(0);
  expect(await page.evaluate(() => (window as unknown as { shown: string[] }).shown)).toEqual([own(STORED_FACTS)]);
});

test("a bundle that fails to read settles its facts on every channel", async ({ page }) => {
  const met = await holdBundle(page, 100, true);
  await page.goto(`${PAGE}?${new URLSearchParams({ src: BUNDLE, facts: ALL_FACTS, firstRead: "256" })}`);
  const { summaries, warnings } = await define(page);
  await expect.poll(summaries, { timeout: 20_000 }).toEqual([{ title: "roles.bundle.tickwave", facts: own(ALL_FACTS), matches: true }]);
  expect(met()).toBeGreaterThan(0);
  expect(warnings).toEqual([]);
});

test("a recording with no sidecar named and none bundled has its summary as soon as its head is in, even off screen", async ({ page }) => {
  await page.goto(`${PAGE}?${new URLSearchParams({ facts: FACTS })}`);
  await define(page);
  // A second view of the recording, off screen: it fetches nothing, and has the head the first read.
  await page.evaluate(() => {
    const spacer = Object.assign(document.createElement("div"), { style: "height: 4000px" });
    const w = document.createElement("tickwave-view");
    w.id = "w";
    w.setAttribute("src", document.getElementById("v")!.getAttribute("src")!);
    document.body.append(spacer, w);
  });
  const summary = (id: string) => page.evaluate((id) => (document.getElementById(id) as HTMLElement & { summary: unknown }).summary, id);
  await expect.poll(() => summary("v"), { timeout: 20_000 }).not.toBeNull();
  await expect.poll(() => summary("w")).toEqual({ title: "identify-swd.tickwave", facts: OWN_FACTS, matches: true });
  expect(await page.locator("#w").evaluate((w) => w.getBoundingClientRect().top)).toBeGreaterThan(await page.evaluate(() => innerHeight));
});

test("a stored view read for a sidecar the view has since moved on from counts for nothing", async ({ page }) => {
  // Two sidecars of identify-swd, each storing one channel; the first held until released.
  const sidecar = (channel: string) => `${readFileSync("fixtures/identify-swd.annotations.toml", "utf8")}\n[view]\nchannels = ["${channel}"]\n`;
  let release!: () => void;
  const released = new Promise<void>((r) => (release = r));
  let asked = false;
  await page.route("**/fixtures/first.annotations.toml", async (route) => {
    asked = true;
    await released;
    await route.fulfill({ contentType: "text/plain", body: sidecar("SWDIO") });
  });
  await page.route("**/fixtures/second.annotations.toml", (route) => route.fulfill({ contentType: "text/plain", body: sidecar("SWCLK") }));
  await page.goto(`${PAGE}?${new URLSearchParams({ annotations: "fixtures/first.annotations.toml" })}`);
  const { summaries } = await define(page);
  await expect.poll(() => asked, { timeout: 20_000 }).toBe(true);
  await page.locator("#v").evaluate((v) => v.setAttribute("annotations", "../../fixtures/second.annotations.toml"));
  const second = { title: "identify-swd.tickwave", facts: "524 ms; 1 of 2 channels: SWCLK; 4.6 KiB", matches: true };
  await expect.poll(summaries, { timeout: 20_000 }).toEqual([second]);
  release();
  // The first sidecar's read lands, and changes nothing.
  await page.waitForTimeout(500);
  expect(await summaries()).toEqual([second]);
  expect(await page.evaluate(() => (document.getElementById("v") as HTMLElement & { summary: unknown }).summary)).toEqual(second);
});
