// The Open page's annotation editor, and the view's parts it stands on: the cursor and annotation
// events, and a sidecar handed in as text. Expected files are written out by hand from
// ANNOTATIONS.md's layout, not taken from the editor.

import { test, expect, type Locator, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";

const SITE = "/dist/site/";
const IDENTIFY = `${SITE}open.html?src=fixtures/identify-swd.tickwave&annotations=fixtures/identify-swd.annotations.toml`;
const SIDECAR = readFileSync("fixtures/identify-swd.annotations.toml", "utf8");

async function viewReady(page: Page): Promise<Locator> {
  const view = page.locator("tickwave-view");
  await view.evaluate(
    (v) => new Promise<void>((resolve) => (v.shadowRoot!.querySelector("canvas")!.width > 0 ? resolve() : v.addEventListener("tickwave-ready", () => resolve(), { once: true }))),
  );
  return view;
}

/** Drags along the time axis from `from` to `to`, as fractions of the plot's width. */
async function select(page: Page, view: Locator, from: number, to: number): Promise<void> {
  const box = (await view.locator(".plot").boundingBox())!;
  const y = box.y + box.height - 10;
  await page.mouse.move(box.x + box.width * from, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * to, y, { steps: 6 });
  await page.mouse.up();
}

/** The view's hand-written sidecar text, as the editor last handed it in. */
const handed = (view: Locator) => view.evaluate((v) => (v as unknown as { annotationsText: string | null }).annotationsText);

/** Where `el` is on the page, to check it did not move. A scroll is not a move. */
const at = (el: Locator) =>
  el.evaluate((e) => {
    const b = e.getBoundingClientRect();
    return { x: Math.round(b.x + scrollX), y: Math.round(b.y + scrollY) };
  });

async function download(page: Page): Promise<{ name: string; text: string }> {
  const [d] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Download the sidecar" }).click()]);
  return { name: d.suggestedFilename(), text: readFileSync((await d.path())!, "utf8") };
}

test("a new annotation over the selection goes at the end of the sidecar, which keeps every byte it had", async ({ page }) => {
  await page.goto(IDENTIFY);
  const view = await viewReady(page);
  const edit = page.locator("#edit");
  const where = await at(edit);
  await edit.click();
  await expect(edit).toHaveAccessibleName("Stop editing");
  await expect(page.locator("#editor")).toBeVisible();
  expect(await at(edit)).toEqual(where);
  // The sidecar's own annotations, in its order: the hand-written file's first.
  await expect(page.locator("#notes .pick .what").first()).toHaveText(/^first-wait/);

  await select(page, view, 0.3, 0.6);
  const span = await view.evaluate((v) => (v as unknown as { selection: { start: number; end: number } }).selection);
  await page.getByRole("button", { name: "New annotation" }).click();
  await expect(page.locator("#note-title")).toHaveText("New annotation");
  // The selection's ticks at 1 ns: nanoseconds, written in milliseconds to the nanosecond.
  const ms = (t: number) => `${(t / 1e6).toFixed(6)} ms`;
  await expect(page.locator("#note-start")).toHaveValue(ms(span.start));
  await expect(page.locator("#note-end")).toHaveValue(ms(span.end));
  await page.locator("#note-label").fill("A burst of clocks");
  await expect(page.locator("#note-id")).toHaveValue("a-burst-of-clocks");

  const add = page.locator("#note-apply");
  const addAt = await at(add);
  await add.click();
  await expect(page.locator("#note-title")).toHaveText("a-burst-of-clocks");
  await expect(add).toHaveAccessibleName("Apply");
  expect(await at(add)).toEqual(addAt);
  await expect(page.locator("#edit-state")).toHaveText("Changes not saved");

  const table = `\n[[annotation]]\nid = "a-burst-of-clocks"\nstart = "${ms(span.start)}"\nend = "${ms(span.end)}"\nlabel = "A burst of clocks"\n`;
  expect(await handed(view)).toBe(SIDECAR + table);
  const saved = await download(page);
  expect(saved).toEqual({ name: "identify-swd.annotations.toml", text: SIDECAR + table });
  await expect(page.locator("#edit-state")).toHaveText("");
});

test("a change that wouldn't check is refused, and the sidecar stays as it was", async ({ page }) => {
  await page.goto(IDENTIFY);
  const view = await viewReady(page);
  await page.locator("#edit").click();
  await page.locator("#notes .pick", { hasText: /^attach/ }).click();
  await expect(page.locator("#note-start")).toHaveValue("376.719436 ms");
  await page.locator("#note-end").fill("300 ms");
  await page.locator("#note-apply").click();
  const problem = page.locator("#note-problem");
  await expect(problem).toContainText("The sidecar wouldn't check with that change:");
  // attach is the hand-written file's second annotation.
  await expect(problem.locator("li")).toHaveText(["annotation 2: ends before it starts"]);
  expect(await handed(view)).toBe(SIDECAR);
  await expect(page.locator("#edit-state")).toHaveText("");
});

test("a generator's annotation is read, not edited, and naming it adds a hand-written one that refers to it", async ({ page }) => {
  await page.goto(IDENTIFY);
  const view = await viewReady(page);
  await page.locator("#edit").click();
  const pick = page.locator("#notes .pick", { hasText: "from swd" }).first();
  const id = (await pick.locator(".what").textContent())!.split(" · ")[0];
  await pick.scrollIntoViewIfNeeded();
  const pickAt = await at(pick);
  await pick.click();
  // Choosing it changes the form above the list, not the list's place.
  expect(await at(pick)).toEqual(pickAt);
  await expect(page.locator("#note-title")).toHaveText(id);
  await expect(page.locator("#note-label")).toBeDisabled();
  await expect(page.locator("#note-apply")).toBeDisabled();
  await page.getByRole("button", { name: "Name it" }).click();
  await expect(page.locator("#note-title")).toHaveText(`A name for ${id}`);
  await expect(page.locator("#note-start")).toBeDisabled();
  await page.locator("#note-label").fill("The probe's first request");
  await page.locator("#note-apply").click();
  await expect(page.locator("#note-title")).toHaveText("the-probe-s-first-request");
  expect(await handed(view)).toBe(`${SIDECAR}\n[[annotation]]\nid = "the-probe-s-first-request"\nref = "${id}"\nlabel = "The probe's first request"\n`);
});

test("a recording with no sidecar starts one, and its annotation lane is there before the first annotation", async ({ page }) => {
  await page.goto(`${SITE}open.html?src=fixtures/swd50-cut.tickwave`);
  const view = await viewReady(page);
  const plot = view.locator(".plot");
  const before = (await plot.boundingBox())!.height;
  await page.locator("#edit").click();
  await expect(page.locator("#editor")).toBeVisible();
  await expect(page.locator("#notes li")).toHaveCount(0);
  const withLane = (await plot.boundingBox())!.height;
  expect(withLane).toBeGreaterThan(before);

  // A point at cursor A, which Enter places at the middle of the view.
  await plot.focus();
  await page.keyboard.press("Enter");
  await page.getByRole("button", { name: "New annotation" }).click();
  const start = await page.locator("#note-start").inputValue();
  expect(start).toMatch(/^\d+(\.\d+)? (s|ms|µs|ns)$/);
  await expect(page.locator("#note-end")).toHaveValue("");
  await page.locator("#note-label").fill("Reset released");
  await page.locator("#note-apply").click();
  await expect(page.locator("#notes li")).toHaveCount(1);
  expect((await plot.boundingBox())!.height).toBe(withLane);

  const contentId = await page.locator("#facts dd").last().textContent();
  expect(await download(page)).toEqual({
    name: "swd50-cut.annotations.toml",
    text: `format = "tickwave-annotations/1"\nrecording = "swd50-cut.tickwave"\nrecording_id = "${contentId}"\n\n[[annotation]]\nid = "reset-released"\nstart = "${start}"\nlabel = "Reset released"\n`,
  });
});

test("the view says where its cursors are, in ticks and as times, and which annotation's label was clicked", async ({ page }) => {
  await page.goto(IDENTIFY);
  const view = await viewReady(page);
  const next = (type: string) =>
    view.evaluate((v, type) => {
      (v as unknown as { e2eNext: Promise<unknown> }).e2eNext = new Promise((resolve) => v.addEventListener(type, (e) => resolve((e as CustomEvent).detail), { once: true }));
    }, type);
  const detail = () => view.evaluate((v) => (v as unknown as { e2eNext: Promise<unknown> }).e2eNext);

  await view.evaluate((v) => (v as unknown as { setWindow(a: number, b: number): void }).setWindow(376719436, 377140560));
  await next("tickwave-cursor");
  await view.locator(".plot").focus();
  await page.keyboard.press("Enter");
  // The middle of the window: 376,929,998 ticks of 1 ns.
  expect(await detail()).toEqual({ a: 376929998, b: null, aTime: "376.929998 ms", bTime: null });
  await next("tickwave-cursor");
  await page.keyboard.press("Escape");
  expect(await detail()).toEqual({ a: null, b: null, aTime: null, bTime: null });

  // A label in the lane at the plot's top: where the pointer turns to a hand.
  await page.locator("#edit").click();
  const box = (await view.locator(".plot").boundingBox())!;
  const x = box.x + box.width / 2;
  let y = box.y + 2;
  for (; y < box.y + 40; y += 2) {
    await page.mouse.move(x, y);
    if ((await view.locator(".plot").evaluate((p) => (p as HTMLElement).style.cursor)) === "pointer") break;
  }
  expect(y).toBeLessThan(box.y + 40);
  await next("tickwave-annotation");
  await page.mouse.click(x, y);
  const { id } = (await detail()) as { id: string };
  // The editor opens on the annotation the view named.
  await expect(page.locator("#note-title")).toHaveText(id);
});

test("a sidecar's text handed in before the view has loaded is the one it shows", async ({ page }) => {
  await page.goto(`${SITE}open.html`);
  const count = await page.evaluate(async (id) => {
    const view = document.createElement("tickwave-view") as HTMLElement & { annotationsText: string | null };
    view.annotationsText = `format = "tickwave-annotations/1"\nrecording_id = "${id}"\n\n[[annotation]]\nid = "one"\nstart = "1 ms"\nkind = "mark"\n`;
    view.setAttribute("src", "fixtures/identify-swd.tickwave");
    view.setAttribute("annotations", "fixtures/identify-swd.annotations.toml");
    view.setAttribute("open", "");
    document.body.append(view);
    await new Promise((resolve) => view.addEventListener("tickwave-ready", resolve, { once: true }));
    return view.shadowRoot!.querySelector(".steps .count")!.textContent;
  }, "blake3:ccc8b7d8c3e4cb30da7fc6088e54ebeba2858ad402eb3fb5a9aeb50be989accf");
  // The text's one annotation, not the file's sixteen WAITs.
  expect(count).toBe("– of 1");
});

test("served by tickwave open, the sidecar is saved in place, over the version read and no other", async ({ page }) => {
  const puts: { headers: Record<string, string>; body: string }[] = [];
  await page.route("**/fixtures/identify-swd.annotations.toml", async (route) => {
    const r = route.request();
    if (r.method() === "GET") return route.fulfill({ body: SIDECAR, headers: { ETag: '"read"', "Content-Type": "application/toml" } });
    puts.push({ headers: r.headers(), body: r.postData() ?? "" });
    return puts.length === 1 ? route.fulfill({ status: 200, headers: { ETag: '"first"' } }) : route.fulfill({ status: 412, body: "the file changed on disk" });
  });
  await page.goto(`${IDENTIFY}&local&save=fixtures/identify-swd.annotations.toml`);
  const view = await viewReady(page);
  await page.locator("#edit").click();
  const save = page.getByRole("button", { name: "Save the sidecar" });
  await expect(page.locator("#sidecar-name")).toHaveText("to identify-swd.annotations.toml, beside the recording");

  await page.locator("#notes .pick", { hasText: /^attach/ }).click();
  await page.locator("#note-label").fill("Attaching");
  await page.locator("#note-apply").click();
  await expect(page.locator("#edit-state")).toHaveText("Changes not saved");
  await save.click();
  await expect(page.locator("#edit-state")).toHaveText("");
  expect(puts[0].headers["content-type"]).toBe("application/toml");
  expect(puts[0].headers["if-match"]).toBe('"read"');
  expect(puts[0].body).toBe(await handed(view));
  expect(puts[0].body).toContain('label = "Attaching"\n');

  await page.locator("#note-label").fill("Attaching again");
  await page.locator("#note-apply").click();
  await save.click();
  await expect(page.locator("#editor-problem")).toContainText("identify-swd.annotations.toml changed on disk since this page read it, so it wasn't saved over.");
  expect(puts[1].headers["if-match"]).toBe('"first"');
  await expect(page.locator("#edit-state")).toHaveText("Changes not saved");
});
