// What tickwave does while a page's face turns, on e2e/pages/fade.html: a switch that fades the
// page's colours, as the project's does, beside a chart and an open view.

import { test, expect, type Page } from "@playwright/test";

/** One frame while the face turns: <html>'s colour as a share of the way to the new face, and what
 * tickwave shows. Each is read after the frame's drawings, at the frame's animation time. */
interface Frame {
  readonly turned: number;
  readonly ring: string;
  readonly line: string;
  readonly panel: string;
  readonly canvas: number;
  readonly rebuilds: number;
}

async function ready(page: Page): Promise<void> {
  await page.goto("/e2e/pages/fade.html");
  await expect(page.locator("#c svg .tw-pt").first()).toBeAttached({ timeout: 20_000 });
  await page.locator("#v").evaluate((v) => (v as HTMLElement & { settled(): Promise<void> }).settled());
  // The chart's first sweep has played: nothing redraws for half a second.
  await page.evaluate(
    () =>
      new Promise<void>((done) => {
        let timer = setTimeout(done, 500);
        new MutationObserver(() => {
          clearTimeout(timer);
          timer = setTimeout(done, 500);
        }).observe(document.querySelector("#c svg")!, { childList: true });
      }),
  );
}

/** Clicks the switch and records every frame for half a second. */
async function turn(page: Page): Promise<Frame[]> {
  await page.evaluate(() => {
    const w = window as unknown as { frames: unknown[]; start: number };
    w.frames = [];
    w.start = 0;
    const svg = document.querySelector("#c svg")!;
    let rebuilds = 0;
    new MutationObserver(() => rebuilds++).observe(svg, { childList: true });
    const view = document.querySelector("#v")!.shadowRoot!;
    const canvas = view.querySelector("canvas")!;
    const ctx = canvas.getContext("2d")!;
    const pixels = () => {
      const d = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
      let h = 0;
      for (let i = 0; i < d.length; i += 4 * 53) h = (h * 31 + d[i] + d[i + 1] * 7 + d[i + 2] * 13 + d[i + 3]) | 0;
      return h;
    };
    const red = () => Number(/\d+/.exec(getComputedStyle(document.documentElement).color)![0]);
    const from = red();
    const style = (sel: Element | null, prop: "stroke" | "backgroundColor") => (sel ? getComputedStyle(sel)[prop] : "");
    const read = (now: number) => {
      if (w.start && now - w.start > 500) return;
      if (w.start)
        w.frames.push({
          turned: red(),
          ring: style(svg.querySelector(".tw-pt"), "stroke"),
          line: style(svg.querySelector(".tw-series path"), "stroke"),
          panel: style(view.querySelector(".viewer"), "backgroundColor"),
          canvas: pixels(),
          rebuilds,
          from,
        });
      requestAnimationFrame(later);
    };
    // After the frame's callbacks, where its drawings are done: its animation time holds until the next.
    const later = (now: number) => setTimeout(() => read(now), 0);
    requestAnimationFrame(later);
    document.getElementById("faces")!.addEventListener("click", () => (w.start = performance.now()), { capture: true });
  });
  await page.click("#faces");
  await page.waitForTimeout(700);
  const raw = await page.evaluate(() => (window as unknown as { frames: (Frame & { turned: number; from: number })[] }).frames);
  const to = raw.at(-1)!.turned;
  return raw.map((f) => ({ ...f, turned: (f.turned - f.from) / (to - f.from) }));
}

const between = (frames: Frame[], key: "ring" | "line" | "panel") => new Set(frames.slice(1, -1).map((f) => f[key])).size;

test("a chart and a view's panel fade with the page, and the view's canvas turns once, half-way", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light", reducedMotion: "no-preference" });
  await ready(page);
  const frames = await turn(page);
  // The chart's colours are the page's CSS: nothing is redrawn, so each colour passes through the fade.
  expect(frames.at(-1)!.rebuilds).toBe(0);
  for (const key of ["ring", "line", "panel"] as const) expect(between(frames, key), key).toBeGreaterThan(4);
  // A canvas cannot fade: it turns once, on the first frame past half-way.
  const turns = frames.flatMap((f, i) => (i > 0 && f.canvas !== frames[i - 1].canvas ? [i] : []));
  expect(turns).toHaveLength(1);
  expect(frames[turns[0]].turned).toBeGreaterThanOrEqual(0.5);
  expect(frames[turns[0] - 1].turned).toBeLessThan(0.5);
});

test("without motion, the page, the chart and the view turn at once", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light", reducedMotion: "reduce" });
  await ready(page);
  const frames = await turn(page);
  expect(frames.at(-1)!.rebuilds).toBe(0);
  // From the first frame after the click, each shows the night face: no colour in between.
  for (const key of ["ring", "line", "panel", "canvas"] as const) expect(new Set(frames.map((f) => f[key])).size, key).toBe(1);
  expect(frames[0].turned).toBe(1);
});
