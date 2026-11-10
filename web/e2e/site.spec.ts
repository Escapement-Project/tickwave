// tickwave's site, built into dist/site/ and served under that subpath, as Pages will serve it under
// /tickwave/. Expected values come from the converter's own report or by hand, not from the site.

import { test, expect, type Page } from "@playwright/test";
import { mkdirSync, readFileSync } from "node:fs";

const SITE = "/dist/site/";
const IDENTIFY_ID = "blake3:930db20d55ecede1084b73ebbdc7edd306268b900746c08abd0bdfdb0f121623";

/** The facts list beside the view, as [term, value] pairs. */
async function facts(page: Page): Promise<Record<string, string>> {
  await expect(page.locator("#stage")).toBeVisible();
  return page.locator("#facts").evaluate((dl) => {
    const out: Record<string, string> = {};
    const items = [...dl.children];
    for (let i = 0; i + 1 < items.length; i += 2) out[items[i].textContent!] = items[i + 1].textContent!;
    return out;
  });
}

async function viewReady(page: Page): Promise<void> {
  await page.locator("tickwave-view").evaluate(
    (v) => new Promise<void>((resolve) => (v.shadowRoot!.querySelector("canvas")!.width > 0 ? resolve() : v.addEventListener("tickwave-ready", () => resolve(), { once: true }))),
  );
}

test.describe("the project's header", () => {
  test("puts tickwave's pages beside its name among the project's places, and marks where the reader is", async ({ page }) => {
    const places = page.getByRole("navigation", { name: "Escapement Project" });
    const link = (name: string) => places.getByRole("link", { name, exact: true });
    // The group's rule has a heavier stretch under the page shown, or the place it sits inside.
    const segment = (name: string) => link(name).evaluate((a) => getComputedStyle(a, "::after").borderTopWidth);
    await page.goto(`${SITE}docs/format.html`);
    await expect(places.getByRole("link")).toHaveText(["Verge", "tickwave", "Open", "Docs", "Source"]);
    await expect(link("Verge")).toHaveAttribute("href", "https://escapement-project.github.io/verge/");
    await expect(link("tickwave")).toHaveAttribute("href", "../index.html");
    // A doc sits inside tickwave and its docs.
    await expect(link("tickwave")).toHaveAttribute("aria-current", "true");
    await expect(link("Docs")).toHaveAttribute("aria-current", "true");
    await expect(places.locator("[aria-current]")).toHaveCount(2);
    expect(await segment("Docs")).toBe("3px");
    expect(await segment("tickwave")).not.toBe("3px");
    // The overview is tickwave's main page: the name is the page.
    await page.goto(`${SITE}index.html`);
    await expect(link("tickwave")).toHaveAttribute("aria-current", "page");
    await expect(places.locator("[aria-current]")).toHaveCount(1);
    expect(await segment("tickwave")).toBe("3px");
    await page.goto(`${SITE}open.html`);
    await expect(link("Open")).toHaveAttribute("aria-current", "page");
    await expect(link("tickwave")).toHaveAttribute("aria-current", "true");
    expect(await segment("Open")).toBe("3px");
    await page.goto(`${SITE}docs/index.html`);
    await expect(link("Docs")).toHaveAttribute("aria-current", "page");
    await expect(page.locator("footer")).toContainText("tickwave is free software under the MIT or Apache-2.0 licence.");
  });

  test("the switch turns the face, and the next page keeps it", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "light" });
    await page.goto(`${SITE}open.html`);
    const faces = page.getByRole("switch", { name: "Night face" });
    await expect(faces).toHaveAttribute("aria-checked", "false");
    await faces.click();
    await expect(faces).toHaveAttribute("aria-checked", "true");
    expect(await page.evaluate(() => [document.documentElement.dataset.face, getComputedStyle(document.documentElement).colorScheme, localStorage.getItem("face")])).toEqual(["night", "dark", "night"]);
    await page.getByRole("navigation", { name: "Escapement Project" }).getByRole("link", { name: "tickwave", exact: true }).click();
    await expect(page).toHaveURL(/index\.html$/);
    await expect(page.getByRole("switch", { name: "Night face" })).toHaveAttribute("aria-checked", "true");
    expect(await page.evaluate(() => document.documentElement.dataset.face)).toBe("night");
  });

  test("on a phone, the header scrolls away, and comes back on the way up without moving the page", async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 700 });
    await page.goto(`${SITE}docs/format.html`);
    const places = page.getByRole("navigation", { name: "Escapement Project" });
    await expect(places.getByRole("link", { name: "Docs" })).toBeVisible();
    const at = () => page.evaluate(() => ({ top: document.querySelector("header")!.getBoundingClientRect().top, bottom: document.querySelector("header")!.getBoundingClientRect().bottom, main: document.querySelector("main")!.getBoundingClientRect().top + scrollY }));
    // Each scroll its own event: two in one frame would read as one jump.
    const to = (y: number) => page.evaluate((y) => new Promise((r) => { scrollTo({ top: y, behavior: "instant" }); requestAnimationFrame(() => requestAnimationFrame(r)); }), y);
    const start = await at();
    await to(1200);
    await expect.poll(async () => (await at()).bottom).toBeLessThanOrEqual(0);
    await to(1000);
    await expect.poll(async () => (await at()).top).toBe(0);
    await expect(places.getByRole("link", { name: "Docs" })).toBeVisible();
    expect((await at()).main).toBeCloseTo(start.main, 2);
    await to(1600);
    await expect.poll(async () => (await at()).bottom).toBeLessThanOrEqual(0);
    expect((await at()).main).toBeCloseTo(start.main, 2);
  });
});

test.describe("the viewer", () => {
  test("opens a recording and its sidecar from ?src= and ?annotations=", async ({ page }) => {
    await page.goto(`${SITE}open.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml`);
    const f = await facts(page);
    // From the converter's report: no title, 0.523583100 s at 1 GS/s, two channels, 4830 bytes.
    expect(f).toMatchObject({
      Recording: "identify-swd.tickwave",
      Length: "523.583100 ms",
      "One tick": "1 ns",
      Channels: "SWCLK, SWDIO",
      Size: "4.7 KiB",
      "Content id": IDENTIFY_ID,
    });
    await viewReady(page);
    // The sidecar's first annotation is a WAIT, and there are 16 to step through.
    const view = page.locator("tickwave-view");
    await expect(view.locator(".kind")).toHaveValue("wait");
    await view.locator(".next").click();
    await expect(view.locator(".steps .count")).toHaveText("1 of 16");
    await expect(page.locator("#share a")).toHaveAttribute("href", /\?src=fixtures%2Fidentify-swd\.tickwave&annotations=/);
    await expect(page.locator("#problem")).toBeHidden();
  });

  test("opens a URL from the field, and puts it in the address", async ({ page }) => {
    await page.goto(`${SITE}open.html`);
    await page.locator("#src").fill("fixtures/swd50-cut.tickwave");
    await page.getByRole("button", { name: "Open" }).click();
    const f = await facts(page);
    expect(f.Channels).toBe("SWCLK (analog, V), SWDIO (analog, V), NRST");
    await viewReady(page);
    expect(new URL(page.url()).searchParams.get("src")).toBe("fixtures/swd50-cut.tickwave");
  });

  test("opens a local .tickwave through the file input", async ({ page }) => {
    await page.goto(`${SITE}open.html`);
    await page.locator("#files").setInputFiles("fixtures/identify-swd.4k.tickwave");
    const f = await facts(page);
    // The same capture in nine blocks: the same content id, a file of 6635 bytes.
    expect(f).toMatchObject({ Recording: "identify-swd.4k.tickwave", Size: "6.5 KiB", "Content id": IDENTIFY_ID });
    await viewReady(page);
    await expect(page.locator("#share")).toHaveText("Files from this computer can't be shared by link.");
  });

  test("opens a local .tickwave with its sidecar's files", async ({ page }) => {
    await page.goto(`${SITE}open.html`);
    await page.locator("#files").setInputFiles(["fixtures/identify-swd.tickwave", "fixtures/identify-swd.annotations.toml", "dist/site/fixtures/identify-swd.swd.jsonl"]);
    await facts(page);
    await viewReady(page);
    const view = page.locator("tickwave-view");
    await expect(view.locator(".kind")).toHaveValue("wait");
    await view.locator(".next").click();
    await expect(view.locator(".steps .count")).toHaveText("1 of 16");
    await expect(page.locator("#problem")).toBeHidden();
  });

  test("says what is wrong with a sidecar, and still shows the recording", async ({ page }) => {
    await page.goto(`${SITE}open.html`);
    // The sidecar names identify-swd.tickwave; this file is identify-swd.4k.tickwave.
    await page.locator("#files").setInputFiles(["fixtures/identify-swd.4k.tickwave", "fixtures/identify-swd.annotations.toml", "dist/site/fixtures/identify-swd.swd.jsonl"]);
    const problem = page.locator("#problem");
    await expect(problem).toContainText("The sidecar has problems, so the recording shows without its annotations:");
    await expect(problem.locator("li")).toContainText(['names "identify-swd.tickwave", not "identify-swd.4k.tickwave"']);
    await expect(page.locator("#stage")).toBeVisible();
  });

  test("says a file that is not a recording isn't one, and one that is neither recording nor capture is nothing to open", async ({ page }) => {
    await page.goto(`${SITE}open.html`);
    await page.locator("#files").setInputFiles({ name: "notes.tickwave", mimeType: "application/octet-stream", buffer: Buffer.from("not a recording") });
    await expect(page.locator("#problem")).toHaveText("notes.tickwave isn't a tickwave recording. The viewer reads .tickwave files, and converts a capture into one first.");
    await expect(page.locator("#stage")).toBeHidden();
    await page.locator("#files").setInputFiles("../site/favicon-tickwave.ico");
    await expect(page.locator("#problem")).toHaveText(/^There's nothing here to open or convert\./);
    await expect(page.locator("#settings")).toBeHidden();
  });

  test("explains a server that refuses a read from another site", async ({ page }) => {
    // Port 8125 is another origin, and sends no Access-Control-Allow-Origin.
    const src = "http://127.0.0.1:8125/dist/site/fixtures/identify-swd.tickwave";
    await page.goto(`${SITE}open.html?src=${encodeURIComponent(src)}`);
    await expect(page.locator("#problem")).toContainText("127.0.0.1:8125 didn't send the recording.");
    await expect(page.locator("#problem")).toContainText("(CORS)");
  });

  test("explains an address with nothing there", async ({ page }) => {
    await page.goto(`${SITE}open.html?src=fixtures/nothing.tickwave`);
    await expect(page.locator("#problem")).toContainText("There's no file at");
    await expect(page.locator("#problem")).toContainText("the server answered 404.");
  });
});

test("the overview: tickwave on its plate, the ways on, a capture beside them, and a section a doc", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${SITE}index.html`);
  await expect(page.locator(".product-id h1")).toHaveText("tickwave");
  await expect(page.locator(".product-id .plate svg")).toHaveCount(1);
  await expect(page.locator(".product-head .ways a")).toHaveText(["Open a recording", "See it in a page", "All docs"]);
  // Beside them, a capture at one annotation, its clock read from the capture's edges.
  const inset = page.locator(".product-figure tickwave-inset");
  await expect(inset.locator("figcaption")).toHaveText("Reading the debug port's ID register");
  await expect(inset.locator(".measure")).toHaveText("SWCLK 999.93 kHz mean of 45 periods");
  // Each section's heading leads to its doc, and the docs' rows are the docs.
  const heads = page.locator("section .head h2 a");
  await expect(heads).toHaveText(["Reading a capture", "Convert", "In your page", "Docs"]);
  expect(await heads.evaluateAll((as) => as.map((a) => a.getAttribute("href")))).toEqual(["docs/viewer.html", "docs/converter.html", "docs/elements.html", "docs/index.html"]);
  await expect(page.locator(".doc-rows dt a")).toHaveText(["Viewer", "Elements", "Charts", "Format", "Sidecar", "Converter"]);
  // The sections' rules run the page's width, as on the project's pages.
  expect(await page.evaluate(() => document.querySelector("section")!.getBoundingClientRect().width)).toBe(await page.evaluate(() => document.documentElement.clientWidth));
});

test("the overview's inset keeps its height while it loads, so nothing below it moves", async ({ page }) => {
  // The elements' script and the recording are held back, then let through one at a time.
  const held = (pattern: string) => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    void page.route(pattern, async (route) => {
      await gate;
      await route.continue();
    });
    return release;
  };
  for (const width of [1280, 700, 360]) {
    await page.unroute("**/*");
    await page.setViewportSize({ width, height: 900 });
    const script = held("**/tickwave.js");
    const data = held("**/fixtures/identify-swd.tickwave");
    // A module script holds DOMContentLoaded back, so the page is ready to measure once parsed, its
    // sheets and fonts in.
    await page.goto(`${SITE}index.html`, { waitUntil: "commit" });
    await page.waitForFunction(() => document.readyState !== "loading" && [...document.querySelectorAll<HTMLLinkElement>("link[rel=stylesheet]")].every((l) => l.sheet));
    // In Firefox, fonts.ready does not settle while the script is held.
    await page.evaluate(() => Promise.all([...document.fonts].map((f) => f.load())));
    const below = () => page.locator("#reading").evaluate((s) => s.getBoundingClientRect().top + scrollY);
    const undefinedYet = await below();
    script();
    await page.waitForFunction(() => customElements.get("tickwave-inset") !== undefined);
    const noData = await below();
    data();
    await page.locator(".product-figure tickwave-inset").evaluate((i) => (i as HTMLElement & { settled(): Promise<void> }).settled());
    await expect(page.locator(".product-figure tickwave-inset .measure")).not.toBeEmpty();
    expect([undefinedYet, noData], `${width} px`).toEqual([await below(), await below()]);
    // And the inset, drawn, fills the room kept for it: nothing cut off, and no gap under it.
    const inset = page.locator(".product-figure tickwave-inset");
    const drawn = await inset.locator("figure").evaluate((f) => f.getBoundingClientRect().height);
    const kept = await inset.evaluate((i) => {
      const s = getComputedStyle(i);
      return i.getBoundingClientRect().height - parseFloat(s.paddingTop) - parseFloat(s.paddingBottom);
    });
    expect(drawn, `${width} px`).toBeCloseTo(kept, 0);
  }
});

test("without scripts, the overview's figure says it needs them, offers its files, and keeps no room", async ({ browser, baseURL }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(new URL(`${SITE}index.html`, baseURL).href);
  const notice = page.locator(".product-figure noscript > p");
  await expect(notice).toContainText("The figure needs JavaScript.");
  await expect(notice.locator("a[download]")).toHaveCount(2);
  expect(await page.locator(".product-figure tickwave-inset").evaluate((i) => i.getBoundingClientRect().height)).toBe(0);
  await context.close();
});

test("the overview's references, inset and views work", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${SITE}index.html`);
  const inset = page.locator(".product-figure tickwave-inset");
  await expect(inset.locator(".measure")).toHaveText("SWCLK 999.93 kHz mean of 45 periods");
  await expect(inset.locator(".key > span")).toHaveText(["probe drives", "target drives"]);
  const ref = page.locator("tickwave-ref[annotation=line-reset]");
  await ref.hover();
  await expect(ref.locator(".pop tickwave-inset figcaption")).toHaveText("Line reset: SWDIO held high for 51 clocks");
  await expect(ref.locator(".pop tickwave-inset .measure")).toHaveText("SWCLK 54.644 µs first edge to last");
  await page.mouse.move(0, 0);
  const capture = page.locator("#capture");
  await expect(capture.locator(".summary .facts")).toHaveText("524 ms; 2 channels: SWCLK, SWDIO; 4.7 KiB");
  // A closed view's focus goes to its Open button.
  await capture.evaluate((v) => v.focus());
  await expect(capture.locator(".open")).toBeFocused();
  await page.locator("tickwave-ref[annotation=dpidr-ack]").click();
  await expect(capture).toHaveAttribute("open", "");
  await expect(page.locator("#analog").locator(".toolbar .title")).toHaveText("swd50-cut.tickwave");
});

test("on a phone, an example opens with its view in sight and focused, the forms folded and the details closed", async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${SITE}open.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml`);
  const view = page.locator("#stage tickwave-view");
  await expect(view.locator(".plot")).toBeFocused();
  const box = await view.boundingBox();
  expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.y + box!.height).toBeLessThanOrEqual(801);
  await expect(page.locator("#opener")).not.toHaveAttribute("open");
  await expect(page.getByText("Open another recording")).toBeVisible();
  await expect(page.locator("#facts")).toBeHidden();
  await page.getByText("Recording details").click();
  await expect(page.locator("#facts")).toContainText("blake3:930db20d");
  // Unfolded, the forms open another.
  await page.getByText("Open another recording").click();
  await page.locator("#src").fill("fixtures/swd50-cut.tickwave");
  await page.locator("#annotations").fill("");
  await page.getByRole("button", { name: "Open", exact: true }).click();
  await expect(page.locator("#stage tickwave-view .plot")).toBeFocused();
  await expect(page.locator("#opener")).not.toHaveAttribute("open");
});

test("the overview without scripts says what needs them, and offers its recordings", async ({ browser, request, baseURL }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(new URL(`${SITE}index.html`, baseURL).href);
  // Playwright's text search skips <noscript>, so the notices are found by where they are.
  await expect(page.locator("noscript > p")).toHaveText([/^The figure needs JavaScript\./, /^The view needs JavaScript\./, /^The capture needs JavaScript\./]);
  await expect(page.getByText("Point at one, or reach it with Tab")).toBeHidden();
  await expect(page.getByText("Here is the whole capture, closed")).toBeHidden();
  const downloads = await page.locator("a[download]").evaluateAll((as) => as.map((a) => (a as HTMLAnchorElement).href));
  expect(downloads).toHaveLength(5);
  for (const href of downloads) expect((await request.get(href)).ok(), href).toBe(true);
  await context.close();
});

test("the docs sit in the project's doc frame: every doc in a rail, this one's sections, and a bar on a phone", async ({ page }) => {
  // The frame takes the page's column, as wide as on any other page: the docs' sheet narrows only the text.
  const column = () => page.locator("main").evaluate((m) => m.getBoundingClientRect().width);
  await page.goto(`${SITE}open.html`);
  const wide = await column();
  await page.goto(`${SITE}docs/format.html`);
  expect(await column()).toBe(wide);
  const rail = page.getByRole("navigation", { name: "tickwave's docs" });
  // Every link, a subsection's included: those show while the reader is in their section.
  await expect(rail.locator("a")).toHaveText(["Viewer", "Elements", "Charts", "Format", "Times", "Layout", "Preamble", "Header", "Index", "Blocks", "Overview", "Content id", "Sidecar", "Converter"]);
  await expect(rail.getByRole("link", { name: "Format" })).toHaveAttribute("aria-current", "page");
  // The section the reader is in is marked as they go.
  // At the top of the window, past the reading line the rail marks by.
  await page.evaluate(() => document.getElementById("blocks")!.scrollIntoView({ block: "start" }));
  await expect(rail.locator('[aria-current="location"]')).toHaveText("Blocks");
  await page.setViewportSize({ width: 360, height: 800 });
  const bar = page.locator(".doc-bar");
  await expect(bar).toBeVisible();
  // The text reflows at the new width; the section goes back to the top.
  await page.evaluate(() => document.getElementById("blocks")!.scrollIntoView({ block: "start" }));
  await expect(bar.locator(".where")).toHaveText("Blocks");
  await bar.click();
  await expect(bar).toHaveAttribute("aria-expanded", "true");
  await expect(rail.getByRole("link", { name: "Converter" })).toBeVisible();
});

test("the docs' index lists each doc with what it covers, in the project's markup", async ({ page }) => {
  await page.goto(`${SITE}docs/index.html`);
  await expect(page.locator(".docs-index .docs-head h1")).toHaveText("tickwave's docs");
  await expect(page.locator(".docs-index .intro")).toHaveText("Each doc is written in tickwave's repository.");
  // One list: one group with no heading.
  await expect(page.locator(".doc-groups > div")).toHaveCount(1);
  await expect(page.locator(".doc-groups .group")).toHaveCount(0);
  const items = page.locator(".doc-groups li");
  await expect(items.locator("a")).toHaveText(["Viewer", "Elements", "Charts", "Format", "Sidecar", "Converter"]);
  await expect(items.first().locator("p")).toHaveText("Opening a recording, moving and zooming, reading its channels, cursors and annotations.");
  await items.getByRole("link", { name: "Format" }).click();
  await expect(page).toHaveURL(/docs\/format\.html$/);
});

test("each page is titled by its name, tickwave's and the project's", async ({ page }) => {
  for (const [path, title] of [
    ["index.html", "tickwave – Escapement Project"],
    ["open.html", "Open – tickwave – Escapement Project"],
    ["docs/index.html", "Docs – tickwave – Escapement Project"],
    ["docs/annotations.html", "Sidecar – tickwave – Escapement Project"],
  ]) {
    await page.goto(`${SITE}${path}`);
    await expect(page).toHaveTitle(title);
  }
});

test("a doc's headings link to themselves from h2 down, and its source line names the file it comes from", async ({ page, request }) => {
  await page.goto(`${SITE}docs/format.html`);
  await expect(page.locator(".doc h1 .anchor")).toHaveCount(0);
  const times = page.locator(".doc h2#times .anchor");
  await expect(times).toHaveAttribute("href", "#times");
  await expect(times).toHaveAttribute("aria-label", "Link to this section");
  await expect(times).not.toHaveAttribute("aria-hidden", /./);
  // Every doc's source line links a file that is in the repository, at the published address.
  const REPO = "https://github.com/Escapement-Project/tickwave/blob/HEAD/";
  for (const path of ["viewer", "elements", "charts", "format", "annotations", "converter"]) {
    const html = await (await request.get(`${SITE}docs/${path}.html`)).text();
    const href = /<p class="doc-source">From <a href="([^"]+)">tickwave<\/a>, where changes to this page go\.<\/p><\/article>/.exec(html)?.[1];
    expect(href, path).toMatch(new RegExp(`^${REPO.replace(/[./]/g, "\\$&")}`));
    const [file, section] = href!.slice(REPO.length).split("#");
    const text = readFileSync(`../${file}`, "utf8");
    if (section) expect(text, href).toMatch(new RegExp(`^#+ ${section.replace(/-/g, " ")}$`, "im"));
  }
});

test("the search's index holds each doc's sections, as the project defines one", async ({ request }) => {
  const index = (await (await request.get(`${SITE}docs/search.json`)).json()) as { docs: [string, string][]; sections: [number, string, string, string][] };
  expect(index.docs).toEqual([["Viewer", "viewer.html"], ["Elements", "elements.html"], ["Charts", "charts.html"], ["Format", "format.html"], ["Sidecar", "annotations.html"], ["Converter", "converter.html"]]);
  const format = index.sections.filter(([d]) => d === 3);
  // The text before the first heading, under the doc's name; then each h2 and h3 in page order.
  expect(format.slice(0, 3).map(([, id, heading]) => [id, heading])).toEqual([["", "Format"], ["times", "Times"], ["layout", "Layout"]]);
  expect(format[0][3]).toMatch(/^A tickwave recording holds digital and analog channels/);
  // Plain text: no heading link, no source line, no doubled spaces.
  for (const [, , heading, text] of index.sections) {
    expect(heading).not.toMatch(/#$/);
    expect(text).not.toContain("where changes to this page go");
    expect(text).not.toMatch(/\s{2}/);
  }
});

test("the docs' search finds a section from a doc and from the index, and opens it marked", async ({ page }) => {
  await page.goto(`${SITE}docs/converter.html`);
  const field = page.getByRole("combobox", { name: "Search tickwave's docs" });
  await field.fill("magic");
  const found = page.locator("#found");
  await expect(found).toBeVisible();
  const preamble = found
    .locator('[role="group"]', { has: page.locator(".doc-name", { hasText: /^Format$/ }) })
    .locator('a[role="option"]', { has: page.locator(".h", { hasText: /^Preamble$/ }) });
  await expect(preamble).toBeVisible();
  await preamble.click();
  await expect(page).toHaveURL(/docs\/format\.html\?q=magic#preamble$/);
  await expect(page.locator(".doc mark.hit").first()).toHaveText(/magic/i);
  // On the index, the results take the list's place.
  await page.goto(`${SITE}docs/index.html`);
  await page.getByRole("combobox", { name: "Search tickwave's docs" }).fill("sidecar");
  await expect(page.locator(".doc-groups")).toBeHidden();
  await expect(page.locator("#found .doc-name").first()).toBeVisible();
});

test("a command in the docs has a Copy button, and a command's output has none", async ({ page }) => {
  await page.goto(`${SITE}docs/converter.html`);
  // copy.js wraps each block it gives a button in a .codeblock.
  const commands = await page.locator(".doc pre:has(> code.language-sh)").count();
  expect(commands).toBeGreaterThan(3);
  expect(await page.locator(".doc pre:has(> code.language-text)").count()).toBeGreaterThan(2);
  await expect(page.locator(".doc .codeblock > pre:has(> code.language-sh)")).toHaveCount(commands);
  await expect(page.locator(".doc .codeblock > pre:has(> code.language-sh)").first().locator("xpath=..").getByRole("button", { name: /copy/i })).toHaveCount(1);
  await expect(page.locator(".doc .codeblock > pre:has(> code.language-text)")).toHaveCount(0);
});

test("every internal link resolves, fragments included", async ({ request, baseURL }) => {
  const start = new URL(`${SITE}index.html`, baseURL).href;
  const seen = new Set<string>([start]);
  const queue = [start];
  const pages = new Map<string, string>();
  const failures: string[] = [];
  let links = 0;
  while (queue.length > 0) {
    const url = queue.shift()!;
    const res = await request.get(url);
    if (!res.ok()) {
      failures.push(`${url}: HTTP ${res.status()}`);
      continue;
    }
    const html = await res.text();
    pages.set(url.split("#")[0].split("?")[0], html);
    const hrefs = [...html.matchAll(/<a [^>]*href="([^"]+)"/g)].map((m) => m[1].replace(/&amp;/g, "&"));
    for (const href of hrefs) {
      if (/^[a-z]+:/i.test(href)) continue;
      links++;
      const target = new URL(href, url);
      // A viewer link names its recording and sidecar: those must exist too.
      for (const key of ["src", "annotations"]) {
        const v = target.searchParams.get(key);
        if (v) {
          const r = await request.get(new URL(v, target).href);
          if (!r.ok()) failures.push(`${url}: ${href}: ${key} ${v}: HTTP ${r.status()}`);
        }
      }
      const pageUrl = target.href.split("#")[0].split("?")[0];
      if (!seen.has(pageUrl) && pageUrl.endsWith(".html")) {
        seen.add(pageUrl);
        queue.push(pageUrl);
      }
      if (target.hash) {
        let body = pages.get(pageUrl);
        if (!body) {
          const r = await request.get(pageUrl);
          body = await r.text();
          pages.set(pageUrl, body);
        }
        if (!body.includes(`id="${decodeURIComponent(target.hash.slice(1))}"`)) failures.push(`${url}: ${href}: no ${target.hash}`);
      }
    }
  }
  expect(failures).toEqual([]);
  // The nine pages, each reached by a link.
  expect([...pages.keys()].map((u) => u.slice(u.indexOf(SITE) + SITE.length)).sort()).toEqual([
    "docs/annotations.html",
    "docs/charts.html",
    "docs/converter.html",
    "docs/elements.html",
    "docs/format.html",
    "docs/index.html",
    "docs/viewer.html",
    "index.html",
    "open.html",
  ]);
  expect(links).toBeGreaterThan(40);
});

test("the docs carry FORMAT.md's and ANNOTATIONS.md's headings", async ({ page }) => {
  for (const [file, path] of [
    ["../FORMAT.md", "docs/format.html"],
    ["../ANNOTATIONS.md", "docs/annotations.html"],
  ]) {
    const headings = [...readFileSync(file, "utf8").matchAll(/^(#{1,4}) (.*)$/gm)].map((m) => [m[1].length, m[2].replace(/`/g, "")] as const);
    expect(headings.length).toBeGreaterThan(8);
    await page.goto(`${SITE}${path}`);
    const shown = await page.locator("main :is(h1, h2, h3, h4)").evaluateAll((hs) => hs.map((h) => [Number(h.tagName[1]), h.textContent!.replace(/\s*#$/, "")] as const));
    expect(shown, path).toEqual(headings);
  }
});

const PAGES = [
  ["overview", "index.html"],
  ["viewer", "open.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml"],
  ["docs", "docs/index.html"],
  ["elements", "docs/elements.html"],
  ["format", "docs/format.html"],
  ["sidecar", "docs/annotations.html"],
  ["converter", "docs/converter.html"],
] as const;

test.describe("screenshots", () => {
  for (const face of ["day", "night"] as const) {
    for (const width of [1100, 360]) {
      test(`${face} at ${width} px, with no sideways scroll`, async ({ page }, info) => {
        await page.setViewportSize({ width, height: 900 });
        await page.emulateMedia({ reducedMotion: "reduce" });
        await page.addInitScript((face) => localStorage.setItem("face", face), face);
        mkdirSync("screenshots", { recursive: true });
        for (const [name, path] of PAGES) {
          await page.goto(`${SITE}${path}`);
          await page.evaluate(() => document.fonts.ready);
          if (name === "viewer") await viewReady(page);
          if (name === "overview") await page.locator("tickwave-inset").evaluate((i) => (i as HTMLElement & { settled(): Promise<void> }).settled());
          await page.waitForTimeout(150);
          const [scroll, client] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
          expect(scroll, `${name} scrolls sideways at ${width} px`).toBeLessThanOrEqual(client);
          await page.screenshot({ path: `screenshots/site-${info.project.name}-${name}-${face}-${width}.png`, fullPage: true });
        }
      });
    }
  }
});
