// <tickwave-chart> in a browser, on Figure 1 of the landing page (fixtures/figure1.chart.json). The
// first chart is 620 px wide with end labels, so its plot runs from x 52 to 556 and y 28 to 286 of
// its SVG, as in test/chart.test.ts. Expected figures are the fixture's, written out by hand.

import { test, expect, type Locator, type Page } from "@playwright/test";

const PAGE = "/e2e/pages/chart.html";
const log50 = (v: number) => 52 + (504 * Math.log(v)) / Math.log(50);

async function settled(chart: Locator): Promise<void> {
  await chart.evaluate(async (c) => {
    await customElements.whenDefined("tickwave-chart");
    await (c as HTMLElement & { settled(): Promise<void> }).settled();
  });
}

/** Records the page's tickwave-pick events, from the chart they came from. */
async function recordPicks(page: Page): Promise<() => Promise<{ chart: string; x: string | null; series: string | null }[]>> {
  await page.evaluate(() => {
    const w = window as unknown as { picks: unknown[] };
    w.picks = [];
    document.addEventListener("tickwave-pick", (e) => w.picks.push({ chart: (e.target as Element).id, ...(e as CustomEvent).detail }));
  });
  return () => page.evaluate(() => (window as unknown as { picks: { chart: string; x: string | null; series: string | null }[] }).picks);
}

test.describe("at rest", () => {
  test.beforeEach(async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce", colorScheme: "light" });
    await page.goto(PAGE);
    await settled(page.locator("#c1"));
  });

  test("Figure 1 is a chart named by its caption and described by its table", async ({ page }) => {
    const chart = page.getByRole("group", { name: "Figure 1. 4 KiB writes against the SWD clock asked for." });
    await expect(chart).toHaveAttribute("aria-roledescription", "chart");
    await expect(chart).toHaveAttribute("id", "c1");
    await expect(chart).toHaveAccessibleDescription(/Figure 1's figures/);
    // No point of Figure 1 names a recording, so Enter has nothing to open.
    await expect(chart).not.toHaveAccessibleDescription(/Enter opens/);
  });

  test("draws the shown series in the page's colours, as attributes, and nothing by reference", async ({ page }) => {
    const c1 = page.locator("#c1");
    const clang = c1.locator('g.tw-series[data-series="clang"]');
    const stock = c1.locator('g.tw-series[data-series="stock"]');
    await expect(clang.locator("circle.tw-pt")).toHaveCount(7);
    await expect(clang.locator("circle.tw-pt").first()).toHaveAttribute("fill", "#0f8a6c");
    // NXP's 30 MHz run failed and one of its two 50 MHz runs did, so its line ends at 20 MHz.
    await expect(stock.locator("circle.tw-pt")).toHaveCount(5);
    await expect(stock.locator("circle.tw-hollow")).toHaveCount(0);
    await expect(stock.locator("path")).toHaveCount(1);
    await expect(stock.locator("path")).not.toHaveAttribute("stroke-dasharray", /./);
    // GCC is hidden until the page shows it, and NXP does not mark its failures.
    await expect(c1.locator('g.tw-series[data-series="gcc"]')).toHaveCount(0);
    await expect(c1.locator("svg text", { hasText: "failed" })).toHaveCount(0);
    await expect(c1.locator("text.tw-end")).toHaveText(["Verge", "NXP"]);
    await expect(c1.locator(".tw-legend li")).toHaveText(["Verge, Clang build", "NXP's firmware V3.174", "NXP's firmware reads right up to 20 MHz"]);
    expect(await c1.evaluate((c) => c.innerHTML.includes("url(#"))).toBe(false);
    // Had both its 50 MHz runs given a figure, read wrong, the point would stand alone, hollow,
    // its middle in the page's surface colour.
    await c1.evaluate(async (c) => {
      const data = await (await fetch("/fixtures/figure1.chart.json")).json();
      data.series[1].points[6] = { value: "744.6", verdict: "wrong" };
      (c as HTMLElement & { data: unknown }).data = data;
    });
    const hollow = stock.locator("circle.tw-hollow");
    await expect(hollow).toHaveCount(1);
    await expect(hollow).toHaveAttribute("stroke", "#2a78d6");
    const surface = await page.evaluate(() => {
      const probe = document.body.appendChild(document.createElement("i"));
      probe.style.color = "var(--surface)";
      const colour = getComputedStyle(probe).color;
      probe.remove();
      return colour;
    });
    expect(await hollow.evaluate((e) => getComputedStyle(e).fill)).toBe(surface);
  });

  test("the keys step through the settings, the readout keeps every digit, and the group follows", async ({ page }) => {
    const picks = await recordPicks(page);
    const c1 = page.locator("#c1");
    const rows = c1.locator(".tw-readout > div");
    const c2rows = page.locator("#c2 .tw-readout > div");
    await page.keyboard.press("Tab");
    await expect(c1).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(c1.locator(".tw-readout b")).toHaveText("1 MHz");
    await expect(rows).toHaveText(["Verge 81.0 KiB/s", "NXP 76.7 KiB/s"]);
    await page.keyboard.press("End");
    await expect(rows).toHaveText(["Verge 2,257.8 KiB/s", "NXP failed in 1 of 2 runs (744.6 KiB/s in the other), reads wrong"]);
    await page.keyboard.press("ArrowLeft");
    await expect(rows).toHaveText(["Verge 1,779.8 KiB/s", "NXP failed"]);
    // The second chart reads below its plot, by full names, and shows GCC.
    await expect(page.locator("#c2 .tw-readout b")).toHaveText("30 MHz");
    await expect(c2rows).toHaveText(["Verge, Clang build 1,779.8 KiB/s", "Verge, GCC build 1,641.9 KiB/s"]);
    await page.keyboard.press("Escape");
    await expect(rows).toHaveCount(0);
    await expect(c2rows).toHaveCount(0);
    expect(await picks()).toEqual([
      { chart: "c1", x: "1", series: null },
      { chart: "c1", x: "50", series: null },
      { chart: "c1", x: "30", series: null },
      { chart: "c1", x: null, series: null },
    ]);
  });

  test("pointing at a setting picks it, and leaving the plot lets it go", async ({ page }) => {
    const picks = await recordPicks(page);
    const c1 = page.locator("#c1");
    const box = (await c1.locator("svg.tw-plot").boundingBox())!;
    await page.mouse.move(box.x + log50(20) + 3, box.y + 150);
    await expect(c1.locator(".tw-readout > div")).toHaveText(["Verge 1,238.6 KiB/s", "NXP 666.7 KiB/s"]);
    await expect(page.locator("#c2 .tw-readout b")).toHaveText("20 MHz");
    // A rule marks the picked setting.
    await expect(c1.locator(`line[x1="${log50(20).toFixed(1).replace(/\.0$/, "")}"][stroke-opacity=".35"]`)).toHaveCount(1);
    await page.mouse.move(box.x + box.width + 40, box.y + 150);
    await expect(c1.locator(".tw-readout > div")).toHaveCount(0);
    expect(await picks()).toEqual([
      { chart: "c1", x: "20", series: null },
      { chart: "c1", x: null, series: null },
    ]);
  });

  test("the tooltip readout stays inside the chart at any width, wrapping a long figure", async ({ page }) => {
    const outside = await page.locator("#c1").evaluate(async (chart) => {
      const c = chart as HTMLElement & { pick(x?: string | null): void };
      const figure = chart.closest("figure")!;
      const out: string[] = [];
      for (let width = 280; width <= 1240; width += 40) {
        figure.style.width = `${width}px`;
        await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
        for (const x of ["1", "5", "10", "20", "30", "50"]) {
          c.pick(x);
          const tip = chart.querySelector(".tw-readout")!.getBoundingClientRect();
          const box = chart.getBoundingClientRect();
          if (tip.left < box.left - 0.5 || tip.right > box.right + 0.5 || tip.top < box.top - 0.5 || tip.bottom > box.bottom + 0.5) {
            out.push(`${width} px, ${x} MHz: ${Math.round(tip.left - box.left)} to ${Math.round(tip.right - box.left)} of ${Math.round(box.width)}`);
          }
        }
      }
      c.pick();
      return out;
    });
    expect(outside).toEqual([]);
    // NXP's figure at 50 MHz runs to two lines, the second under the first's text, not its swatch.
    await page.locator("#c1").evaluate((c) => (c as HTMLElement & { pick(x: string): void }).pick("50"));
    const nxp = page.locator("#c1 .tw-readout > div").filter({ hasText: "NXP" });
    await expect(nxp).toContainText("NXP failed in 1 of 2 runs (744.6 KiB/s in the other), reads wrong");
    const lines = await nxp.evaluate((row) => {
      const range = document.createRange();
      range.selectNodeContents(row);
      const rects = [...range.getClientRects()].filter((r) => r.width > 0);
      const swatch = row.querySelector(".tw-swatch")!.getBoundingClientRect();
      return { tops: new Set(rects.map((r) => Math.round(r.top))).size, last: rects.at(-1)!.left - swatch.left };
    });
    expect(lines.tops).toBeGreaterThan(1);
    expect(lines.last).toBeCloseTo(28, 0);
  });

  test("a legend entry dims the other series, and the limit's entry strengthens its line", async ({ page }) => {
    const c1 = page.locator("#c1");
    await c1.locator(".tw-legend li", { hasText: "NXP's firmware V3.174" }).hover();
    await expect(c1.locator('g.tw-series[data-series="clang"]')).toHaveAttribute("opacity", "0.2");
    await expect(c1.locator('g.tw-series[data-series="stock"]')).not.toHaveAttribute("opacity", /./);
    await c1.locator(".tw-legend li", { hasText: "reads right up to 20 MHz" }).hover();
    await expect(c1.locator('g.tw-series[data-series="clang"]')).not.toHaveAttribute("opacity", /./);
    await expect(c1.locator("line.tw-limit")).toHaveAttribute("stroke-width", "2");
  });

  test("the keyboard reaches each legend entry, which keeps the focus as it picks its series", async ({ page }) => {
    const c1 = page.locator("#c1");
    const entry = (name: string) => c1.locator(".tw-legend li", { hasText: name });
    await page.keyboard.press("Tab");
    await page.keyboard.press("Tab");
    await expect(entry("Verge, Clang build")).toBeFocused();
    await expect(c1.locator('g.tw-series[data-series="stock"]')).toHaveAttribute("opacity", "0.2");
    await page.keyboard.press("Tab");
    await expect(entry("NXP's firmware V3.174")).toBeFocused();
    await expect(c1.locator('g.tw-series[data-series="clang"]')).toHaveAttribute("opacity", "0.2");
    await page.keyboard.press("Tab");
    await expect(entry("reads right up to 20 MHz")).toBeFocused();
    await expect(c1.locator("line.tw-limit")).toHaveAttribute("stroke-width", "2");
  });

  test("new data keeps the reader's setting where it has one, and else takes the page's", async ({ page }) => {
    const chart = page.locator("#c1");
    const readout = chart.locator(".tw-readout");
    /** Figure 1 with Verge's 5 MHz figure changed, or without 10 MHz. */
    const slice = (without10: boolean) =>
      chart.evaluate(async (c, without10) => {
        const data = await (await fetch("/fixtures/figure1.chart.json")).json();
        data.series[0].points[2].value = "400.0";
        if (without10) {
          data.x.values.splice(3, 1);
          for (const s of data.series) s.points.splice(3, 1);
        }
        (c as HTMLElement & { data: unknown }).data = data;
      }, without10);
    await chart.focus();
    await page.keyboard.press("Home");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await expect(readout).toContainText("5 MHz");
    // Another slice of the same settings: the reader stays at 5 MHz and reads its new figure.
    await slice(false);
    await expect(readout).toContainText("5 MHz");
    await expect(readout).toContainText("Verge 400.0 KiB/s");
    // A slice without 10 MHz falls back to the page's selected setting.
    await chart.evaluate((c) => c.setAttribute("selected", "20"));
    await page.keyboard.press("ArrowLeft");
    await expect(readout).toContainText("10 MHz");
    await slice(true);
    await expect(readout).toContainText("20 MHz");
  });

  test("the legend lists every series the page shows, a hidden one included", async ({ page }) => {
    const c1 = page.locator("#c1");
    await c1.evaluate((c) => c.setAttribute("show", "clang stock gcc"));
    await expect(c1.locator(".tw-legend li")).toHaveText(["Verge, Clang build", "NXP's firmware V3.174", "Verge, GCC build", "NXP's firmware reads right up to 20 MHz"]);
  });

  test("pick() moves the group, as a reader's pick does, and fires nothing", async ({ page }) => {
    const picks = await recordPicks(page);
    await page.locator("#c1").evaluate((c) => (c as HTMLElement & { pick(x: string): void }).pick("20"));
    await expect(page.locator("#c2 .tw-readout b")).toHaveText("20 MHz");
    expect(await picks()).toEqual([]);
  });

  test("a chart file that fails after another has loaded says nothing over it; the failed one is tried again", async ({ page }) => {
    let calls = 0;
    await page.route("**/slow.chart.json", async (route) => {
      calls++;
      await new Promise((r) => setTimeout(r, 600));
      await route.fulfill({ status: 500 });
    });
    const c3 = page.locator("#c3");
    await c3.evaluate((c) => {
      c.setAttribute("src", "/fixtures/slow.chart.json");
      c.setAttribute("src", "/fixtures/figure1.chart.json");
    });
    await page.waitForTimeout(900);
    await expect(c3.locator(".tw-notice")).toBeHidden();
    // Asked for again after it failed, it is fetched again.
    await c3.evaluate((c) => c.setAttribute("src", "/fixtures/slow.chart.json"));
    await expect(c3.locator(".tw-notice")).toContainText("HTTP 500");
    await c3.evaluate((c) => {
      c.setAttribute("src", "/fixtures/figure1.chart.json");
      c.setAttribute("src", "/fixtures/slow.chart.json");
    });
    await expect.poll(() => calls).toBe(3);
  });

  test("a page's reset that makes every svg a block leaves the swatches in their lines", async ({ page }) => {
    // The project's shared sheet has `.chart svg { display: block }`.
    await page.addStyleTag({ content: "svg { display: block; }" });
    const c1 = page.locator("#c1");
    await c1.focus();
    // The first setting, where every row is short enough for one line.
    await page.keyboard.press("Home");
    const rows = c1.locator(".tw-readout > div");
    await expect(rows).toHaveCount(2);
    for (const row of await rows.all()) {
      const [swatch, box] = await Promise.all([row.locator(".tw-swatch").boundingBox(), row.boundingBox()]);
      // One line: the swatch beside its text, not above it.
      expect(box!.height).toBeLessThan(26);
      expect(swatch!.y).toBeGreaterThanOrEqual(box!.y);
    }
  });

  test("highlight rings its points, and `limits` strengthens the limit as its legend entry does", async ({ page }) => {
    const c1 = page.locator("#c1");
    const limit = c1.locator("line.tw-limit");
    await expect(limit).toHaveAttribute("stroke-width", "1");
    await c1.evaluate((c) => c.setAttribute("highlight", "clang@50 limits"));
    await expect(limit).toHaveAttribute("stroke-width", "2");
    await expect(c1.locator('svg circle[r="9"]')).toHaveCount(1);
    await c1.evaluate((c) => c.setAttribute("highlight", "clang@50"));
    await expect(limit).toHaveAttribute("stroke-width", "1");
  });

  test("a hidden series draws while the page picks it", async ({ page }) => {
    const c1 = page.locator("#c1");
    await c1.evaluate((c) => (c as HTMLElement & { pick(x: string, s: string): void }).pick("20", "gcc"));
    const gcc = c1.locator('g.tw-series[data-series="gcc"]');
    await expect(gcc.locator("rect.tw-pt")).toHaveCount(7);
    await expect(gcc.locator("path")).toHaveAttribute("stroke-dasharray", "6 3");
    await expect(c1.locator(".tw-readout > div")).toHaveText(["Verge 1,238.6 KiB/s", "NXP 666.7 KiB/s", "GCC 1,184.2 KiB/s"]);
    await expect(c1.locator('g.tw-series[data-series="clang"]')).toHaveAttribute("opacity", "0.2");
  });

  test("the face redraws the series in its colours", async ({ page }) => {
    const line = page.locator('#c1 g.tw-series[data-series="clang"] path').first();
    await expect(line).toHaveAttribute("stroke", "#0f8a6c");
    await page.evaluate(() => (document.documentElement.dataset.face = "night"));
    await expect(line).toHaveAttribute("stroke", "#1aa384");
  });

  test("a change on <html> that leaves the face alone redraws nothing; a class that switches it does", async ({ page }) => {
    const line = page.locator('#c1 g.tw-series[data-series="clang"] path').first();
    await page.evaluate(async () => {
      await document.fonts.ready;
      const w = window as unknown as { redraws: number };
      w.redraws = 0;
      new MutationObserver(() => w.redraws++).observe(document.querySelector("#c1 svg.tw-plot")!, { childList: true });
    });
    const frames = () => page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
    // What the project's header does as the page scrolls and as it wraps.
    await page.evaluate(() => {
      document.documentElement.classList.add("scrolled");
      document.documentElement.style.setProperty("--header", "64px");
    });
    await frames();
    await frames();
    expect(await page.evaluate(() => (window as unknown as { redraws: number }).redraws)).toBe(0);
    // A host page whose night face is a class.
    await page.evaluate(() => {
      const style = document.head.appendChild(document.createElement("style"));
      style.textContent = "html.night { --ink: #f2f2ee; --series-verge: #1aa384; }";
      document.documentElement.classList.add("night");
    });
    await expect(line).toHaveAttribute("stroke", "#1aa384");
  });

  test("a chart that cannot draw says why, and tells the page", async ({ page }) => {
    const errors = await page.evaluate(async () => {
      const seen: string[] = [];
      const add = (setup: (c: HTMLElement) => void) =>
        new Promise<void>((resolve) => {
          const c = document.createElement("tickwave-chart");
          c.addEventListener("tickwave-error", (e) => (seen.push((e as CustomEvent).detail.message), resolve()));
          document.body.append(c);
          setup(c);
        });
      await add((c) => c.setAttribute("src", "/fixtures/missing.chart.json"));
      await add((c) => ((c as HTMLElement & { data: unknown }).data = { format: "tickwave-chart/2" }));
      return seen;
    });
    expect(errors[0]).toBe("/fixtures/missing.chart.json: HTTP 404");
    expect(errors[1].split("\n")[0]).toBe('format: "tickwave-chart/2", not "tickwave-chart/1"');
    await expect(page.locator("body > tickwave-chart .tw-notice")).toHaveText(["/fixtures/missing.chart.json: HTTP 404", 'format: "tickwave-chart/2", not "tickwave-chart/1"']);
  });
});

test("a chart below the fold is drawn whole, hidden by CSS alone until its sweep plays", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 900, height: 700 });
  await page.goto(PAGE);
  const c3 = page.locator("#c3");
  await expect(c3).toHaveAttribute("data-pending", "");
  // The whole figure is in the markup, for a reader that drops the page's CSS.
  await expect(c3.locator('g.tw-series[data-series="clang"] circle.tw-pt')).toHaveCount(7);
  await expect(c3.locator("text.tw-end")).toHaveText(["Verge", "NXP"]);
  await expect(c3.locator(".tw-data")).toBeHidden();
  await c3.scrollIntoViewIfNeeded();
  await expect(c3).not.toHaveAttribute("data-pending", "");
  await settled(c3);
  await expect(c3.locator(".tw-data")).toBeVisible();
  await expect(c3.locator('g.tw-series[data-series="clang"] circle.tw-pt')).toHaveCount(7);
  await expect(c3.locator("text.tw-end")).toHaveText(["Verge", "NXP"]);
});

test("a chart a fifth on screen waits; its sweep plays once 60 % of it shows", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 900, height: 700 });
  await page.goto(PAGE);
  const c3 = page.locator("#c3");
  const box = (await c3.boundingBox())!;
  // Its top fifth inside the window's foot.
  await page.evaluate((y) => scrollTo(0, y), box.y - 700 + box.height * 0.2);
  await page.waitForTimeout(300);
  await expect(c3).toHaveAttribute("data-pending", "");
  await page.evaluate((y) => scrollTo(0, y), box.y - 700 + box.height * 0.7);
  await expect(c3).not.toHaveAttribute("data-pending", "");
});

test("a highlight set while the sweep plays ends it at once", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.setViewportSize({ width: 900, height: 700 });
  await page.goto(PAGE);
  const c1 = page.locator("#c1");
  await expect(c1).not.toHaveAttribute("data-pending", "");
  const elapsed = await c1.evaluate(async (c) => {
    const t = performance.now();
    c.setAttribute("highlight", "clang@50 limits");
    await (c as HTMLElement & { settled(): Promise<void> }).settled();
    return performance.now() - t;
  });
  // The sweep takes 750 ms and the end labels 200 ms more; a highlight leaves neither to play.
  expect(elapsed).toBeLessThan(150);
  await expect(c1.locator('g.tw-series[data-series="clang"] circle.tw-pt')).toHaveCount(7);
  await expect(c1.locator("text.tw-end")).toHaveText(["Verge", "NXP"]);
});

test("data set on the element before the module defines it still draws", async ({ page }) => {
  await page.goto(PAGE);
  const data = await page.evaluate(async () => (await fetch("/fixtures/figure1.chart.json")).json());
  await page.setContent(`<link rel="stylesheet" href="/dist/site/tokens.css"><tickwave-chart id="early" sweep="off"></tickwave-chart>`);
  await page.evaluate((data) => ((document.getElementById("early") as HTMLElement & { data: unknown }).data = data), data);
  await page.addScriptTag({ url: "/dist/tickwave-chart.js", type: "module" });
  await expect(page.locator('#early g.tw-series[data-series="clang"] circle.tw-pt')).toHaveCount(7);
});

test.describe("on a touch screen", () => {
  test.use({ hasTouch: true });

  test("a tap picks a setting, lifting the finger keeps it, and a tap outside clears it", async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto(PAGE);
    const c1 = page.locator("#c1");
    await settled(c1);
    const box = (await c1.locator("svg.tw-plot").boundingBox())!;
    await page.touchscreen.tap(box.x + log50(5), box.y + 200);
    await expect(c1.locator(".tw-readout b")).toHaveText("5 MHz");
    await expect(c1.locator(".tw-readout > div")).toHaveText(["Verge 395.3 KiB/s", "NXP 297.8 KiB/s"]);
    // The tap has lifted: the pick stays until a tap lands outside the chart.
    await page.touchscreen.tap(box.x + box.width + 30, box.y + 200);
    await expect(c1.locator(".tw-readout > div")).toHaveCount(0);
  });
});

test("Chromium's accessibility tree: a chart named by its caption, described by its table, with a live readout", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "read through Chromium's DevTools protocol");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(PAGE);
  await settled(page.locator("#c1"));
  await page.locator("#c1").focus();
  await page.keyboard.press("End");
  await expect(page.locator("#c1 .tw-readout b")).toHaveText("50 MHz");
  type Node = { nodeId: string; parentId?: string; role?: { value: string }; name?: { value: string }; description?: { value: string }; properties?: { name: string; value: { value: unknown } }[] };
  const cdp = await page.context().newCDPSession(page);
  const { nodes } = (await cdp.send("Accessibility.getFullAXTree")) as unknown as { nodes: Node[] };
  const prop = (n: Node, name: string) => n.properties?.find((p) => p.name === name)?.value.value;
  const charts = nodes.filter((n) => prop(n, "roledescription") === "chart");
  expect(charts.map((n) => [n.role?.value, n.name?.value])).toEqual([
    ["group", "Figure 1. 4 KiB writes against the SWD clock asked for."],
    ["group", "Figure 2. The two builds."],
    ["group", "Figure 3. Below the fold."],
  ]);
  const chart = charts[0];
  expect(chart.description?.value).toBe("Figure 1's figures");
  expect([prop(chart, "focusable"), prop(chart, "focused")]).toEqual([true, true]);
  // The readout is a polite live region inside the chart.
  const byId = new Map(nodes.map((n) => [n.nodeId, n]));
  const inside = (n: Node) => {
    for (let p = n.parentId; p; p = byId.get(p)?.parentId) if (p === chart.nodeId) return true;
    return false;
  };
  const live = nodes.filter((n) => prop(n, "live") === "polite" && inside(n));
  expect(live.length).toBeGreaterThan(0);
  const text = nodes.filter((n) => n.role?.value === "StaticText" && live.some((l) => l.nodeId === n.parentId || inside(n))).map((n) => n.name?.value);
  expect(text).toEqual(expect.arrayContaining(["50 MHz", "Verge 2,257.8 KiB/s", "NXP failed in 1 of 2 runs (744.6 KiB/s in the other), reads wrong"]));
});

test.describe("evidence", () => {
  const EVIDENCE = "/e2e/pages/evidence.html";
  const ORIGIN = "http://127.0.0.1:8124";
  const RECORDING = `${ORIGIN}/fixtures/identify-swd.tickwave`;
  const SIDECAR = `${ORIGIN}/fixtures/identify-swd.annotations.toml`;

  test.beforeEach(async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto(EVIDENCE);
    await settled(page.locator("#c"));
    await page.evaluate(() => {
      const w = window as unknown as { evidence: unknown[] };
      w.evidence = [];
      document.addEventListener("tickwave-evidence", (e) => w.evidence.push((e as CustomEvent).detail));
    });
  });

  /** Where series A's point at setting `i` is drawn, on the page. */
  async function point(page: Page, i: number): Promise<{ x: number; y: number }> {
    const circle = page.locator('#c g.tw-series[data-series="a"] circle.tw-pt').nth(i);
    const box = (await page.locator("#c svg.tw-plot").boundingBox())!;
    return { x: box.x + Number(await circle.getAttribute("cx")), y: box.y + Number(await circle.getAttribute("cy")) };
  }
  const evidence = (page: Page) => page.evaluate(() => (window as unknown as { evidence: unknown[] }).evidence);
  /** Points the first and last points at these recordings, so a test can find its own requests. */
  const setRecordings = (page: Page, first: string, last: string) =>
    page.locator("#c").evaluate(
      async (c, [first, last]) => {
        const data = await (await fetch("/fixtures/evidence.chart.json")).json();
        const [a, , b] = data.series[0].points;
        Object.assign(a.evidence, { recording: first, annotations: "/fixtures/identify-swd.annotations.toml" });
        Object.assign(b.evidence, { recording: last, annotations: "/fixtures/identify-swd.annotations.toml" });
        (c as HTMLElement & { data: unknown }).data = data;
      },
      [first, last],
    );
  /** The requests the e2e server logged whose URL holds `tag`. */
  const requests = async (tag: string) => (await (await fetch(`${ORIGIN}/__log?tag=${tag}`)).json()) as unknown[];
  const viewWindow = (page: Page) => page.locator("#v").evaluate((v) => (v as unknown as { window: { start: number; end: number } }).window);

  test("pointing at a point that names a recording loads the elements then, and shows its annotation beside it", async ({ page }) => {
    const loaded = () => page.evaluate(() => [customElements.get("tickwave-inset") !== undefined, performance.getEntriesByType("resource").some((e) => e.name.endsWith("/dist/tickwave.js"))]);
    expect(await loaded()).toEqual([false, false]);
    const pop = page.locator("#c .tw-pop");
    const first = await point(page, 0);
    await page.mouse.move(first.x + 3, first.y + 3);
    await expect(pop).toBeVisible();
    expect(await loaded()).toEqual([true, true]);
    const inset = pop.locator("tickwave-inset");
    await expect(inset).toHaveAttribute("src", RECORDING);
    await expect(inset).toHaveAttribute("annotations", SIDECAR);
    await expect(inset).toHaveAttribute("annotation", "attach");
    await expect(inset.locator("figcaption")).toHaveText("Attaching: line reset, select, line reset, then the first read");
    // Below the chart, across the point's x: clear of the plot and its readout.
    const box = (await pop.boundingBox())!;
    const chart = (await page.locator("#c").boundingBox())!;
    expect(box.y).toBeGreaterThanOrEqual(chart.y + chart.height);
    expect(box.x).toBeLessThanOrEqual(first.x);
    expect(box.x + box.width).toBeGreaterThanOrEqual(first.x);
    // The middle point names no recording, so pointing at it closes the popover.
    const middle = await point(page, 1);
    await page.mouse.move(middle.x, middle.y);
    await expect(pop).toBeHidden();
    expect(await evidence(page)).toEqual([]);
  });

  test("a click fires tickwave-evidence, and the page's view of that recording opens at the annotation", async ({ page }) => {
    const last = await point(page, 2);
    await page.mouse.move(last.x, last.y);
    await expect(page.locator("#c .tw-pop tickwave-inset")).toHaveAttribute("annotation", "dpidr-read");
    await page.mouse.click(last.x, last.y);
    expect(await evidence(page)).toEqual([{ x: "3", series: "a", recording: RECORDING, annotations: SIDECAR, annotation: "dpidr-read" }]);
    await expect(page.locator("#v")).toHaveAttribute("open", "");
    await expect(page.locator("#c .tw-pop")).toBeHidden();
    // dpidr-read runs from 377,094,557 to 377,140,560 ticks; the view shows it with room either side.
    await expect.poll(async () => {
      const w = await viewWindow(page);
      return w.start < 377094557 && w.end > 377140560 && w.end - w.start < 100_000;
    }).toBe(true);
  });

  test("a page that cancels the event takes over; with no view of the recording, the popover stays", async ({ page }) => {
    await page.evaluate(() => document.addEventListener("tickwave-evidence", (e) => e.preventDefault(), { once: true }));
    const last = await point(page, 2);
    await page.mouse.move(last.x, last.y);
    await page.mouse.click(last.x, last.y);
    expect(await evidence(page)).toHaveLength(1);
    await expect(page.locator("#c .tw-pop")).toBeHidden();
    await expect(page.locator("#v")).not.toHaveAttribute("open", "");
    await page.locator("#v").evaluate((v) => v.remove());
    const first = await point(page, 0);
    await page.mouse.move(first.x, first.y);
    await page.mouse.click(first.x, first.y);
    expect(await evidence(page)).toHaveLength(2);
    await expect(page.locator("#c .tw-pop tickwave-inset")).toHaveAttribute("annotation", "attach");
    await expect(page.locator("#c .tw-pop")).toBeVisible();
  });

  test("the keys reach a point's evidence: Escape closes it, Enter opens the view there", async ({ page }) => {
    const pop = page.locator("#c .tw-pop");
    const inset = pop.locator("tickwave-inset");
    await page.locator("#c").focus();
    await page.keyboard.press("ArrowRight");
    await expect(inset).toHaveAttribute("annotation", "attach");
    await expect(pop).toBeVisible();
    await page.keyboard.press("ArrowRight");
    await expect(pop).toBeHidden();
    await page.keyboard.press("End");
    await expect(inset).toHaveAttribute("annotation", "dpidr-read");
    await expect(pop).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(pop).toBeHidden();
    await page.keyboard.press("End");
    await expect(pop).toBeVisible();
    await page.keyboard.press("Enter");
    expect(await evidence(page)).toEqual([{ x: "3", series: "a", recording: RECORDING, annotations: SIDECAR, annotation: "dpidr-read" }]);
    await expect(page.locator("#v")).toHaveAttribute("open", "");
    await expect(pop).toBeHidden();
  });

  test("a mouse passing over the points loads nothing; one that rests on a point opens it", async ({ page }, info) => {
    const tag = `pass-${info.project.name}-${Date.now()}`;
    await setRecordings(page, `/fixtures/identify-swd.tickwave?tag=${tag}`, `/fixtures/identify-swd.tickwave?tag=${tag}`);
    // The three points lie on one line: pass along it, from before the first to past the last.
    const [first, last] = [await point(page, 0), await point(page, 2)];
    const dx = (last.x - first.x) / 10;
    const dy = (last.y - first.y) / 10;
    await page.mouse.move(first.x - dx, first.y - dy);
    await page.mouse.move(last.x + dx, last.y + dy, { steps: 48 });
    await page.mouse.move(last.x + dx, last.y + 80);
    await page.waitForTimeout(400);
    await expect(page.locator("#c .tw-pop")).toBeHidden();
    expect(await requests(tag)).toEqual([]);
    expect(await page.evaluate(() => customElements.get("tickwave-inset") === undefined)).toBe(true);
    await page.mouse.move(first.x, first.y);
    await expect(page.locator("#c .tw-pop")).toBeVisible();
    await expect.poll(async () => (await requests(tag)).length).toBeGreaterThan(0);
  });

  test("a recording the inset has moved on from cannot fail it later", async ({ page }, info) => {
    const tag = `stale-${info.project.name}-${Date.now()}`;
    // The first point's recording fails, but only after 1.5 s; the last point's loads at once.
    await setRecordings(page, `/fixtures/identify-swd.tickwave?tag=${tag}&after=0&stall=1500&fail=500`, `/fixtures/identify-swd.tickwave?tag=${tag}-b`);
    await page.evaluate(() => {
      const w = window as unknown as { errors: string[] };
      w.errors = [];
      document.addEventListener("tickwave-error", (e) => w.errors.push((e as CustomEvent).detail.message));
    });
    const inset = page.locator("#c .tw-pop tickwave-inset");
    const [first, last] = [await point(page, 0), await point(page, 2)];
    await page.mouse.move(first.x, first.y);
    await expect(inset).toHaveAttribute("annotation", "attach");
    await page.mouse.move(last.x, last.y);
    await expect(inset).toHaveAttribute("annotation", "dpidr-read");
    await expect(inset.locator("figcaption")).toHaveText("Reading the debug port's ID register");
    // The first recording's failure lands after the inset moved on.
    await expect.poll(async () => (await requests(tag)).some((r) => (r as { status: number }).status === 500), { timeout: 5000 }).toBe(true);
    await page.waitForTimeout(200);
    expect(await page.evaluate(() => (window as unknown as { errors: string[] }).errors)).toEqual([]);
    await expect(inset.locator('[part="notice"]')).toBeHidden();
    await expect(inset.locator("figcaption")).toHaveText("Reading the debug port's ID register");
  });

  test("the description says, once, that Enter opens a point's recording", async ({ page }) => {
    await expect(page.locator("#c")).toHaveAccessibleDescription("Enter opens the recording behind the picked point.");
    // Nothing in the live readout repeats it as the reader moves.
    await page.locator("#c").focus();
    await page.keyboard.press("End");
    await expect(page.locator("#c .tw-readout")).not.toContainText("Enter");
  });
});

test.describe("a wire-clock chart", () => {
  // fixtures/wire.chart.json: the 2026-11-04 request clock, Verge's two runs drawn on one another
  // from 30 MHz, NXP's failure at 20 MHz marked and naming a recording.
  test.beforeEach(async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce", colorScheme: "light" });
    await page.goto("/e2e/pages/wire.html");
    await settled(page.locator("#w"));
  });

  /** The middle of the last element `selector` finds, whose parent's text is `text` if given. Found
   * and measured in one step in the page: a pick redraws the markup, and a locator's element can be
   * replaced between finding it and measuring it. */
  const centre = (page: Page, selector: string, text?: string) =>
    page.evaluate(
      ([selector, text]) => {
        const all = [...document.querySelectorAll(selector)].filter((e) => text === undefined || e.parentElement?.textContent === text);
        const b = all[all.length - 1].getBoundingClientRect();
        return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
      },
      [selector, text] as const,
    );
  /** Verge's last point, where the capturing run's lies too. */
  const verge50 = (page: Page) => centre(page, '#w g.tw-series[data-series="gcc"] rect');
  const failMark = (page: Page) => centre(page, "#w g.tw-data > g:not(.tw-series) > path", "NXP failed");

  test("the y=x line is named in the legend, and the two runs' end labels stand apart inside the chart", async ({ page }) => {
    await expect(page.locator("#w .tw-legend li[data-reference]")).toHaveText("as asked");
    const verge = page.locator('#w .tw-end:text-is("Verge")');
    const capturing = page.locator('#w .tw-end:text-is("Verge, capturing")');
    expect(Number(await capturing.getAttribute("y")) - Number(await verge.getAttribute("y"))).toBeGreaterThanOrEqual(14);
    const label = (await capturing.boundingBox())!;
    const svg = (await page.locator("#w svg.tw-plot").boundingBox())!;
    expect(label.x + label.width).toBeLessThanOrEqual(svg.x + svg.width);
  });

  test("of two points drawn on one another, the first series' opens, or the picked one's; the popover names it", async ({ page }) => {
    const pop = page.locator("#w .tw-pop");
    const p = await verge50(page);
    await page.mouse.move(p.x, p.y);
    await expect(pop.locator("tickwave-inset")).toHaveAttribute("annotation", "dpidr-read");
    await expect(pop.locator(".tw-pop-head")).toHaveText("Verge, GCC build");
    await page.mouse.move(p.x - 60, p.y + 60);
    await expect(pop).toBeHidden();
    await page.locator("#w").evaluate((c) => (c as HTMLElement & { pick(x: string, s: string): void }).pick("50", "capture"));
    await page.mouse.move(p.x, p.y);
    await expect(pop.locator("tickwave-inset")).toHaveAttribute("annotation", "first-wait");
    await expect(pop.locator(".tw-pop-head")).toHaveText("Verge, GCC build, capturing energy at 100,000 pairs/s");
  });

  test("a failure's mark opens the failed run's recording, from the pointer and the keys; an unmarked failure opens nothing", async ({ page }) => {
    const pop = page.locator("#w .tw-pop");
    const inset = pop.locator("tickwave-inset");
    const mark = await failMark(page);
    await page.mouse.move(mark.x, mark.y);
    await expect(inset).toHaveAttribute("annotation", "dpidr-request");
    await expect(pop.locator(".tw-pop-head")).toHaveText("NXP's firmware V3.174");
    await page.mouse.move(mark.x, mark.y - 120);
    await expect(pop).toBeHidden();
    const chart = page.locator("#w");
    await chart.focus();
    await page.keyboard.press("Home");
    await expect(inset).toHaveAttribute("annotation", "attach");
    await page.keyboard.press("ArrowRight");
    await expect(inset).toHaveAttribute("annotation", "dpidr-request");
    await expect(chart.locator(".tw-readout")).toContainText("NXP failed");
    // Without its mark, the failure has nothing to point at, and the keys open nothing there either.
    await chart.evaluate(async (c) => {
      const data = await (await fetch("/fixtures/wire.chart.json")).json();
      delete data.series[2].fails;
      for (const s of data.series) for (const p of s.points) if (p?.evidence) p.evidence = { ...p.evidence, recording: "/fixtures/identify-swd.tickwave", annotations: "/fixtures/identify-swd.annotations.toml" };
      (c as HTMLElement & { data: unknown }).data = data;
    });
    await page.keyboard.press("Home");
    await expect(inset).toHaveAttribute("annotation", "attach");
    await page.keyboard.press("ArrowRight");
    await expect(pop).toBeHidden();
    await expect(chart.locator(".tw-readout")).toContainText("NXP failed");
  });

  test("the popover opens above the chart without room below it, and below it with room, across the point's x", async ({ page }) => {
    const pop = page.locator("#w .tw-pop");
    let p = await verge50(page);
    await page.mouse.move(p.x, p.y);
    await expect(pop.locator("tickwave-inset")).toHaveAttribute("annotation", "dpidr-read");
    let chart = (await page.locator("#w").boundingBox())!;
    let box = (await pop.boundingBox())!;
    expect(box.y + box.height).toBeLessThanOrEqual(chart.y);
    expect(box.x).toBeLessThanOrEqual(p.x);
    expect(box.x + box.width).toBeGreaterThanOrEqual(p.x);
    await page.mouse.move(p.x - 60, p.y + 60);
    await expect(pop).toBeHidden();
    await page.evaluate((top) => scrollBy(0, top - 10), chart.y);
    p = await verge50(page);
    await page.mouse.move(p.x, p.y);
    await expect(pop).toBeVisible();
    chart = (await page.locator("#w").boundingBox())!;
    box = (await pop.boundingBox())!;
    expect(box.y).toBeGreaterThanOrEqual(chart.y + chart.height);
    expect(box.x).toBeLessThanOrEqual(p.x);
    expect(box.x + box.width).toBeGreaterThanOrEqual(p.x);
  });

  test("the tooltip readout keeps off the points: at 50 MHz, below Verge's line, and between the lines", async ({ page }) => {
    const chart = page.locator("#w");
    /** Where the readout is, and how many points it hides, read in one step: a pick redraws. */
    const readoutAt = () =>
      chart.evaluate((chart) => {
        const readout = chart.querySelector(".tw-readout")!.getBoundingClientRect();
        const plot = chart.querySelector("svg.tw-plot")!.getBoundingClientRect();
        const hidden = [...chart.querySelectorAll("g.tw-series :is(rect, circle)")].filter((m) => {
          const b = m.getBoundingClientRect();
          const x = b.x + b.width / 2;
          const y = b.y + b.height / 2;
          return x >= readout.left && x <= readout.right && y >= readout.top && y <= readout.bottom;
        }).length;
        return { top: readout.top - plot.top, hidden };
      });
    await chart.focus();
    await page.keyboard.press("End");
    await expect(chart.locator(".tw-readout")).toContainText("50 MHz");
    // At the plot's top it would hide Verge's line from 37.5 MHz up.
    let at = await readoutAt();
    expect(at.top).toBeGreaterThan(28 + 20);
    expect(at.hidden).toBe(0);
    // The read-data clock's figures: at the foot it would hide NXP's 25 MHz point, so it sits between.
    await chart.evaluate(async (c) => {
      const data = await (await fetch("/fixtures/wire.chart.json")).json();
      const set = (i: number, values: (string | null)[]) => values.forEach((v, j) => v !== null && (data.series[i].points[j].value = v));
      set(0, ["10.0", null, null, "30.0", "37.5", "37.5"]);
      set(1, [null, null, null, "30.0", "30.0", "30.0"]);
      set(2, ["6.9", null, "10.0", null, null, "10.0"]);
      (c as HTMLElement & { data: unknown }).data = data;
    });
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("End");
    at = await readoutAt();
    expect(at.hidden).toBe(0);
  });
});
