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

test.describe("the viewer", () => {
  test("opens a recording and its sidecar from ?src= and ?annotations=", async ({ page }) => {
    await page.goto(`${SITE}index.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml`);
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
    await page.goto(`${SITE}index.html`);
    await page.locator("#src").fill("fixtures/swd50-cut.tickwave");
    await page.getByRole("button", { name: "Open" }).click();
    const f = await facts(page);
    expect(f.Channels).toBe("SWCLK (analog, V), SWDIO (analog, V), NRST");
    await viewReady(page);
    expect(new URL(page.url()).searchParams.get("src")).toBe("fixtures/swd50-cut.tickwave");
  });

  test("opens a local .tickwave through the file input", async ({ page }) => {
    await page.goto(`${SITE}index.html`);
    await page.locator("#files").setInputFiles("fixtures/identify-swd.4k.tickwave");
    const f = await facts(page);
    // The same capture in nine blocks: the same content id, a file of 6635 bytes.
    expect(f).toMatchObject({ Recording: "identify-swd.4k.tickwave", Size: "6.5 KiB", "Content id": IDENTIFY_ID });
    await viewReady(page);
    await expect(page.locator("#share")).toHaveText("Files from this computer can't be shared by link.");
  });

  test("opens a local .tickwave with its sidecar's files", async ({ page }) => {
    await page.goto(`${SITE}index.html`);
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
    await page.goto(`${SITE}index.html`);
    // The sidecar names identify-swd.tickwave; this file is identify-swd.4k.tickwave.
    await page.locator("#files").setInputFiles(["fixtures/identify-swd.4k.tickwave", "fixtures/identify-swd.annotations.toml", "dist/site/fixtures/identify-swd.swd.jsonl"]);
    const problem = page.locator("#problem");
    await expect(problem).toContainText("The sidecar has problems, so the recording shows without its annotations:");
    await expect(problem.locator("li")).toContainText(['names "identify-swd.tickwave", not "identify-swd.4k.tickwave"']);
    await expect(page.locator("#stage")).toBeVisible();
  });

  test("says a file that is not a recording isn't one", async ({ page }) => {
    await page.goto(`${SITE}index.html`);
    await page.locator("#files").setInputFiles("../site/favicon.ico");
    await expect(page.locator("#problem")).toHaveText("favicon.ico isn't a tickwave recording. The viewer reads .tickwave files, which the converter makes from a capture.");
    await expect(page.locator("#stage")).toBeHidden();
  });

  test("explains a server that refuses a read from another site", async ({ page }) => {
    // Port 8125 is another origin, and sends no Access-Control-Allow-Origin.
    const src = "http://127.0.0.1:8125/dist/site/fixtures/identify-swd.tickwave";
    await page.goto(`${SITE}index.html?src=${encodeURIComponent(src)}`);
    await expect(page.locator("#problem")).toContainText("127.0.0.1:8125 didn't send the recording.");
    await expect(page.locator("#problem")).toContainText("(CORS)");
  });

  test("explains an address with nothing there", async ({ page }) => {
    await page.goto(`${SITE}index.html?src=fixtures/nothing.tickwave`);
    await expect(page.locator("#problem")).toContainText("There's no file at");
    await expect(page.locator("#problem")).toContainText("the server answered 404.");
  });
});

test("the demo's references, inset and views work", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${SITE}demo.html`);
  const inset = page.locator("figure tickwave-inset");
  await expect(inset.locator(".measure")).toHaveText("SWCLK 999.93 kHz mean of 45 periods");
  await expect(inset.locator(".key > span")).toHaveText(["probe drives", "target drives"]);
  const ref = page.locator("tickwave-ref[annotation=line-reset]");
  await ref.hover();
  await expect(ref.locator(".pop tickwave-inset figcaption")).toHaveText("Line reset: SWDIO held high for 51 clocks");
  await expect(ref.locator(".pop tickwave-inset .measure")).toHaveText("SWCLK 54.644 µs first edge to last");
  await page.mouse.move(0, 0);
  const capture = page.locator("#capture");
  await expect(capture.locator(".summary .facts")).toHaveText("524 ms · 2 channels: SWCLK, SWDIO · 4.7 KiB");
  await page.locator("tickwave-ref[annotation=dpidr-ack]").click();
  await expect(capture).toHaveAttribute("open", "");
  await expect(page.locator("#analog").locator(".toolbar .title")).toHaveText("swd50-cut.tickwave");
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
  // The six pages, each reached by a link.
  expect([...pages.keys()].map((u) => u.slice(u.indexOf(SITE) + SITE.length)).sort()).toEqual(
    ["demo.html", "docs/annotations.html", "docs/converter.html", "docs/elements.html", "docs/format.html", "index.html"],
  );
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
  ["viewer", "index.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml"],
  ["demo", "demo.html"],
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
          await page.waitForTimeout(150);
          const [scroll, client] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
          expect(scroll, `${name} scrolls sideways at ${width} px`).toBeLessThanOrEqual(client);
          await page.screenshot({ path: `screenshots/site-${info.project.name}-${name}-${face}-${width}.png`, fullPage: true });
        }
      });
    }
  }
});
