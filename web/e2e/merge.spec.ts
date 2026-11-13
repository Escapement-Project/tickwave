// A merge's preview in the view: the energy fixture (96 MHz: V, I, range, under_range, DIO0, and P
// derived from V and I) and middle-gap (1 GHz: A and B) on one axis at 12 GHz, each input a group
// under a head row whose grip moves all of it.

import { test, expect, type Locator, type Page } from "@playwright/test";

type Shape = Record<string, { offset: number; keep: { from: number; to: number } | null }>;
type Shaped = { shape: Shape; aligns: Record<string, number> };
const state = (view: Locator) => view.evaluate((v) => ({ shape: (v as unknown as Shaped).shape, aligns: (v as unknown as Shaped).aligns }));

async function ready(page: Page, query = ""): Promise<Locator> {
  await page.goto(`/e2e/pages/merge.html${query}`);
  const view = page.locator("#v");
  await view.evaluate((v) => new Promise<void>((r) => (v.shadowRoot!.querySelector("canvas")!.width > 0 ? r() : v.addEventListener("tickwave-ready", () => r(), { once: true }))));
  return view;
}

async function drag(page: Page, el: Locator, dx: number, dy = 0): Promise<void> {
  const b = (await el.boundingBox())!;
  const [x, y] = [b.x + b.width / 2, b.y + b.height / 2];
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y + dy, { steps: 8 });
  await page.mouse.up();
}

test("each input heads its rows, and its head's grip moves every channel of it, apart from the channels' own offsets", async ({ page }) => {
  const view = await ready(page);
  const heads = view.locator(".grip.head");
  await expect(heads).toHaveCount(2);
  // P, derived, has no grip of its own.
  await expect(view.locator(".grip:not(.head)")).toHaveCount(7);
  const head = view.locator('.grip.head[aria-label^="Move middle-gap"]');
  await expect(head).toHaveAttribute("aria-label", /^Move middle-gap, now where it was recorded\. Left and right move all its channels in time/);
  // The head's grip comes before its rows' in the keys' order.
  const order = await view.evaluate((v) => [...v.shadowRoot!.querySelectorAll(".grip")].map((g) => g.getAttribute("aria-label")!.split(",")[0]));
  expect(order).toEqual(["Move energy", "Move V", "Move I", "Move range", "Move under_range", "Move DIO0", "Move middle-gap", "Move A", "Move B"]);
  const startOfA = view.locator('.trim[aria-label="Start of what is kept of A"]');
  const before = await startOfA.getAttribute("aria-valuetext");
  await view.evaluate((v) => {
    const w = window as unknown as { shapes: unknown[] };
    w.shapes = [];
    v.addEventListener("tickwave-shape", (e) => w.shapes.push((e as CustomEvent).detail));
  });
  await head.focus();
  await page.keyboard.press("ArrowRight");
  const detail = (await page.evaluate(() => (window as unknown as { shapes: unknown[] }).shapes.at(-1))) as Shaped & { change: string; group: string };
  expect([detail.change, detail.group]).toEqual(["align", "middle-gap"]);
  const step = detail.aligns["middle-gap"];
  expect(step).toBeGreaterThan(0);
  // The whole input moved; no channel's own offset did.
  const s = await state(view);
  expect(s.aligns).toEqual({ energy: 0, "middle-gap": step });
  expect(Object.values(s.shape).map((c) => c.offset)).toEqual([0, 0, 0, 0, 0, 0, 0, 0]);
  await expect(startOfA).not.toHaveAttribute("aria-valuetext", before!);
  await expect(head).toHaveAttribute("aria-label", /^Move middle-gap, now .* later than recorded\./);
  // A channel's own move adds to its input's alignment and leaves it as it was.
  const grip = view.locator('.grip[aria-label^="Move A,"]');
  await grip.focus();
  await page.keyboard.press("ArrowLeft");
  const t = await state(view);
  expect(t.aligns["middle-gap"]).toBe(step);
  expect(t.shape.A.offset).toBeLessThan(0);
  expect(t.shape.B.offset).toBe(0);
  // Alt drags the input freely, by the drag's own ticks.
  await page.keyboard.down("Alt");
  await drag(page, head, 100);
  await page.keyboard.up("Alt");
  const moved = (await state(view)).aligns["middle-gap"];
  expect(Number.isInteger(moved)).toBe(true);
  expect(moved).toBeGreaterThan(step);
  // Setting the alignments moves the groups they name.
  await view.evaluate((v) => ((v as unknown as Shaped).aligns = { energy: -12_000 }));
  expect((await state(view)).aligns).toEqual({ energy: -12_000, "middle-gap": moved });
});

test("a channel's grip moves its row only among its own input's rows", async ({ page }) => {
  const view = await ready(page);
  const b = view.locator('.grip[aria-label^="Move B,"]');
  // Up over the energy rows: B goes to the top of its own group, under its head.
  await drag(page, b, 0, -400);
  await expect(view).toHaveAttribute("channels", "V I range under_range DIO0 P B A");
  await b.focus();
  await page.keyboard.press("ArrowUp");
  await expect(view).toHaveAttribute("channels", "V I range under_range DIO0 P B A");
  await page.keyboard.press("ArrowDown");
  await expect(view).toHaveAttribute("channels", "V I range under_range DIO0 P A B");
});

test("a derived channel moves with its first operand", async ({ page }) => {
  const view = await ready(page);
  // P is I times V, placed on I's samples: it moves with I, and an offset of its own is not set.
  await view.evaluate((v) => ((v as unknown as Shaped).shape = { I: { offset: 1_000, keep: null }, P: { offset: 5, keep: null } }));
  const s = await state(view);
  expect([s.shape.I.offset, s.shape.P.offset, s.shape.V.offset]).toEqual([1_000, 1_000, 0]);
});

test("groups show their heads without shaping, with no grips", async ({ page }) => {
  const view = await ready(page, "?plain");
  await expect(view.locator(".grip")).toHaveCount(0);
  // Each head's band sits above its group's first row: the row handles' tops step over it.
  const tops = await view.locator(".row-handle").evaluateAll((hs) => hs.map((h) => parseFloat((h as HTMLElement).style.top)));
  const unheaded = await view.evaluate((v) => {
    (v as unknown as { groups: unknown[] }).groups = [];
    return new Promise<number[]>((r) => requestAnimationFrame(() => r([...v.shadowRoot!.querySelectorAll(".row-handle")].map((h) => parseFloat((h as HTMLElement).style.top)))));
  });
  // Eight rows: the first group's head pushes every row down, the second's the last two again.
  const shift = tops.map((t, i) => Math.round(t - unheaded[i]));
  expect(new Set(shift.slice(0, 6)).size).toBe(1);
  expect(shift[6]).toBe(shift[7]);
  expect(shift[6]).toBeGreaterThan(shift[0]);
  expect(shift[0]).toBeGreaterThan(0);
});
