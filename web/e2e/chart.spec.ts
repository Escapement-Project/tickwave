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
  });

  test("draws the shown series in the page's colours, as attributes, and nothing by reference", async ({ page }) => {
    const c1 = page.locator("#c1");
    const clang = c1.locator('g.tw-series[data-series="clang"]');
    const stock = c1.locator('g.tw-series[data-series="stock"]');
    await expect(clang.locator("circle.tw-pt")).toHaveCount(7);
    await expect(clang.locator("circle.tw-pt").first()).toHaveAttribute("fill", "#0f8a6c");
    // NXP's 30 MHz run failed, so its line ends at 20 MHz; its 50 MHz point reads wrong and
    // stands alone, hollow.
    await expect(stock.locator("circle.tw-pt")).toHaveCount(5);
    await expect(stock.locator("circle.tw-hollow")).toHaveCount(1);
    await expect(stock.locator("path")).toHaveCount(1);
    await expect(stock.locator("path")).not.toHaveAttribute("stroke-dasharray", /./);
    // GCC is hidden until the page shows it, and NXP does not mark its failures.
    await expect(c1.locator('g.tw-series[data-series="gcc"]')).toHaveCount(0);
    await expect(c1.locator("svg text", { hasText: "failed" })).toHaveCount(0);
    await expect(c1.locator("text.tw-end")).toHaveText(["Verge", "NXP"]);
    await expect(c1.locator(".tw-legend li")).toHaveText(["Verge, Clang build", "NXP's firmware V3.174", "NXP's firmware reads right up to 20 MHz"]);
    expect(await c1.evaluate((c) => c.innerHTML.includes("url(#"))).toBe(false);
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
    await expect(rows).toHaveText(["Verge 2,257.8 KiB/s", "NXP 744.6 KiB/s, reads wrong"]);
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
  expect(text).toEqual(expect.arrayContaining(["50 MHz", "Verge 2,257.8 KiB/s", "NXP 744.6 KiB/s, reads wrong"]));
});
