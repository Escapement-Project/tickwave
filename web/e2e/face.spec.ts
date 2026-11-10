// What tickwave does while a page's face turns, on e2e/pages/face.html: the project's switch, which
// changes the face under a cover, beside a chart and an open view.

import { test, expect, type Page } from "@playwright/test";

/** One frame while the face turns: the face on <html>, how opaque the cover is (-1 with none up),
 * and what tickwave shows. Each is read after the frame's drawings, at the frame's animation time. */
interface Frame {
  readonly face: string;
  readonly cover: number;
  readonly ring: string;
  readonly line: string;
  readonly panel: string;
  readonly canvas: number;
  readonly rebuilds: number;
}

async function ready(page: Page): Promise<void> {
  await page.goto("/e2e/pages/face.html");
  await expect(page.locator("#c svg .tw-pt").first()).toBeAttached({ timeout: 20_000 });
  await page.locator("#v").evaluate((v) => (v as HTMLElement & { settled(): Promise<void> }).settled());
  await expect(page.locator("#faces.ready")).toBeAttached();
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

/** Clicks the switch and records what shows just before it acts, then every frame for 700 ms. */
async function turn(page: Page): Promise<Frame[]> {
  await page.evaluate(() => {
    const w = window as unknown as { frames: Frame[]; start: number };
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
    const style = (sel: Element | null, prop: "stroke" | "backgroundColor") => (sel ? getComputedStyle(sel)[prop] : "");
    const snap = (): Frame => {
      const wash = document.querySelector(".face-cover > i");
      return {
        face: document.documentElement.dataset.face ?? "",
        cover: wash ? Number(getComputedStyle(wash).opacity) : -1,
        ring: style(svg.querySelector(".tw-pt"), "stroke"),
        line: style(svg.querySelector(".tw-series path"), "stroke"),
        panel: style(view.querySelector(".viewer"), "backgroundColor"),
        canvas: pixels(),
        rebuilds,
      };
    };
    const read = (now: number) => {
      if (w.start && now - w.start > 700) return;
      if (w.start) w.frames.push(snap());
      requestAnimationFrame(later);
    };
    // After the frame's callbacks, where its drawings are done: its animation time holds until the next.
    const later = (now: number) => setTimeout(() => read(now), 0);
    requestAnimationFrame(later);
    // At the switch itself a capturing listener runs first, before the switch's own.
    document.getElementById("faces")!.addEventListener(
      "click",
      () => {
        w.frames.push(snap());
        w.start = performance.now();
      },
      { capture: true },
    );
  });
  await page.click("#faces");
  await page.waitForTimeout(900);
  return page.evaluate(() => (window as unknown as { frames: Frame[] }).frames);
}

const KEYS = ["ring", "line", "panel", "canvas"] as const;
/** The frames at which `key` shows something other than the frame before. */
const changes = (frames: Frame[], key: (typeof KEYS)[number]) => frames.flatMap((f, i) => (i > 0 && f[key] !== frames[i - 1][key] ? [i] : []));

test("under the switch's cover, a chart and a view each turn once, while the cover is full", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light", reducedMotion: "no-preference" });
  await ready(page);
  const frames = await turn(page);
  expect(frames[0].face).toBe("");
  expect(frames.at(-1)!.face).toBe("night");
  // The chart's colours are the page's CSS: nothing is redrawn.
  expect(frames.at(-1)!.rebuilds).toBe(0);
  const turned = frames.findIndex((f) => f.face === "night");
  for (const key of KEYS) {
    // Old, then new: one change, no colour in between, and never back.
    const at = changes(frames, key);
    expect(at, key).toHaveLength(1);
    expect(frames[at[0]].cover, `${key}: the cover when it turns`).toBe(1);
    // In the frame the face changes, or for the canvas, the one after at most.
    expect(at[0] - turned, key).toBeGreaterThanOrEqual(0);
    expect(at[0] - turned, key).toBeLessThanOrEqual(1);
  }
  // The cover has lifted and gone.
  expect(frames.at(-1)!.cover).toBe(-1);
});

test("without motion, the page, the chart and the view turn at once, with no cover", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light", reducedMotion: "reduce" });
  await ready(page);
  const frames = await turn(page);
  expect(frames.at(-1)!.rebuilds).toBe(0);
  // From the first frame after the click, each shows the night face.
  expect(frames.slice(1).every((f) => f.face === "night" && f.cover === -1)).toBe(true);
  for (const key of KEYS) expect(changes(frames, key), key).toEqual([1]);
});
