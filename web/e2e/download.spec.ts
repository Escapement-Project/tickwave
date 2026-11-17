// A view's download links (the `download` attribute): the recording as served, and with an
// `annotations` attribute a button that lists its sidecar's files over the page, beside Open in
// the summary line, which keeps its height and Open's place with them or without, open or closed.

import { readFileSync } from "node:fs";
import { test, expect, type Locator, type Page } from "@playwright/test";

const VIEW = "/e2e/pages/view.html?src=fixtures/identify-swd.tickwave";

/** The summary's box and Open's, to the pixel. */
function boxes(view: Locator) {
  return view.evaluate((v) => {
    const r = (e: Element) => {
      const b = e.getBoundingClientRect();
      return [b.left, b.top, b.width, b.height].map((x) => Math.round(x * 100) / 100);
    };
    const root = v.shadowRoot!;
    return { summary: r(root.querySelector(".summary")!), open: r(root.querySelector(".toggle")!) };
  });
}

/** The links in the Annotations list once every one is in: each one's text, which is its download
 * name, its path, and its part. */
async function links(view: Locator, count = 2): Promise<{ name: string; href: string; part: string | null }[]> {
  const list = view.locator(".files a");
  await expect(list).toHaveCount(count);
  return list.evaluateAll((as) =>
    as.map((a) => {
      const l = a as HTMLAnchorElement;
      if (l.download !== l.textContent) throw new Error(`${l.textContent} downloads as ${l.download}`);
      return { name: l.download, href: l.href.startsWith("blob:") ? "blob:" : new URL(l.href).pathname, part: l.getAttribute("part") };
    }),
  );
}

async function summary(page: Page, query: string): Promise<Locator> {
  await page.goto(`${VIEW}${query}`);
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => (v as unknown as { summary: unknown }).summary !== null), { timeout: 20_000 }).toBe(true);
  return view;
}

test("download links: the recording and its sidecar, by their names, only with the attribute", async ({ page }) => {
  let view = await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  const [file, sidecar] = [view.locator(".download").first(), view.locator(".download.annotations")];
  await expect(file).toBeVisible();
  await expect(file).toHaveText("Download");
  await expect(file).toHaveAttribute("download", "identify-swd.tickwave");
  expect(await file.evaluate((a) => (a as HTMLAnchorElement).href)).toMatch(/\/fixtures\/identify-swd\.tickwave$/);
  await expect(sidecar).toHaveText("Annotations");
  await sidecar.click();
  expect(await links(view)).toEqual([
    { name: "identify-swd.annotations.toml", href: "/fixtures/identify-swd.annotations.toml", part: "annotations-download-link" },
    { name: "identify-swd.swd.jsonl", href: "/fixtures/identify-swd.swd.jsonl", part: "annotations-download-link" },
  ]);
  await expect(file).toHaveAttribute("part", "button download-button");
  await expect(sidecar).toHaveAttribute("part", "button download-button annotations-download-button");
  await expect(view.locator(".files")).toHaveAttribute("part", "annotations-download-list");
  // No sidecar named: the recording's link alone.
  view = await summary(page, "&download");
  await expect(view.locator(".download").first()).toBeVisible();
  await expect(view.locator(".download.annotations")).toBeHidden();
  // Without the attribute, neither.
  view = await summary(page, "&annotations=fixtures/identify-swd.annotations.toml");
  await expect(view.locator(".download").first()).toBeHidden();
  await expect(view.locator(".download.annotations")).toBeHidden();
});

test("the summary line keeps its height and Open its place with the links or without, open or closed", async ({ page }) => {
  for (const open of ["", "&open"]) {
    const without = await boxes(await summary(page, `&annotations=fixtures/identify-swd.annotations.toml${open}`));
    const view = await summary(page, `&download&annotations=fixtures/identify-swd.annotations.toml${open}`);
    await expect(view.locator(".download").first()).toBeVisible();
    expect(await boxes(view)).toEqual(without);
  }
});

test("Chromium's accessibility tree: each link named for the recording, as Open is, by the page's title where it slots one", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "read through Chromium's DevTools protocol");
  type Node = { role?: { value: string }; name?: { value: string }; properties?: { name: string; value: { value: unknown } }[] };
  const nodes = async () => {
    const cdp = await page.context().newCDPSession(page);
    return ((await cdp.send("Accessibility.getFullAXTree")) as unknown as { nodes: Node[] }).nodes;
  };
  // Each link and button's role and name, and a button's expanded state where it has one.
  const names = async () =>
    (await nodes())
      .filter((n) => (n.role?.value === "link" || n.role?.value === "button") && /^(Download|Open)/.test(n.name?.value ?? ""))
      .map((n) => {
        const expanded = n.properties?.find((p) => p.name === "expanded")?.value.value;
        return `${n.role!.value}: ${n.name!.value}${expanded === undefined ? "" : `, expanded ${expanded}`}`;
      });
  const view = await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  await expect
    .poll(names)
    .toEqual(["link: Download identify-swd.tickwave", "button: Download the annotations of identify-swd.tickwave, expanded false", "button: Open identify-swd.tickwave"]);
  await view.locator(".download.annotations").click();
  // The links the list adds come at the tree's end, whatever their place in the document.
  await expect.poll(async () => (await names()).sort()).toEqual([
    "button: Download the annotations of identify-swd.tickwave, expanded true",
    "button: Open identify-swd.tickwave",
    "link: Download identify-swd.annotations.toml, from the annotations of identify-swd.tickwave",
    "link: Download identify-swd.swd.jsonl, from the annotations of identify-swd.tickwave",
    "link: Download identify-swd.tickwave",
  ]);
  // The list is a group named for the recording.
  await expect.poll(async () => (await nodes()).filter((n) => n.role?.value === "group").map((n) => n.name?.value)).toContain("The annotations of identify-swd.tickwave");
  await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml&title=The first read");
  await expect
    .poll(names)
    .toEqual(["link: Download The first read", "button: Download the annotations of The first read, expanded false", "button: Open The first read"]);
});

test("a local file the page registers links its blob under its own name; a merge preview links nothing", async ({ page }) => {
  await page.goto(VIEW);
  const got = await page.evaluate(async () => {
    // A path the page resolves, not one the compiler can.
    const at = "/dist/tickwave.js";
    const tw = (await import(at)) as unknown as {
      registerFile(f: File): string;
      previewMerge(inputs: { src: string; stem: string }[]): Promise<{ src: string }>;
    };
    const bytes = await (await fetch("/fixtures/identify-swd.tickwave")).arrayBuffer();
    const src = tw.registerFile(new File([bytes], "my capture.tickwave"));
    const merged = await tw.previewMerge([
      { src: "/fixtures/identify-swd.tickwave", stem: "a" },
      { src: "/fixtures/energy.tickwave", stem: "b" },
    ]);
    const links = async (s: string) => {
      const v = document.createElement("tickwave-view");
      v.setAttribute("src", s);
      v.setAttribute("download", "");
      document.body.append(v);
      for (let i = 0; i < 100 && !(v as unknown as { summary: unknown }).summary; i++) await new Promise((r) => setTimeout(r, 50));
      const a = v.shadowRoot!.querySelector<HTMLAnchorElement>(".download")!;
      return { hidden: a.hidden, href: a.href, name: a.download, src: s };
    };
    return [await links(src), await links(merged.src)];
  });
  expect(got[0]).toEqual({ hidden: false, href: got[0].src, name: "my capture.tickwave", src: got[0].src });
  expect(got[0].src.startsWith("blob:")).toBe(true);
  expect(got[1].hidden).toBe(true);
});

// The Annotations button's list: the sidecar's hand-written file, then each file it includes.

const ID = "blake3:63773d56fb3420735c864932f8d2441acd6f31a51b557e073bf27755a0534697";

/** Serves `toml` at fixtures/`name`, a 404 where it is null, and counts its requests. */
async function serve(page: Page, name: string, toml: string | null): Promise<() => number> {
  let n = 0;
  await page.route(`**/fixtures/${name}`, (route) => {
    n++;
    return toml === null ? route.fulfill({ status: 404, body: "" }) : route.fulfill({ contentType: "text/plain", body: toml });
  });
  return () => n;
}

/** The element in the view's shadow root that has the focus: its text. */
const focused = (view: Locator) => view.evaluate((v) => v.shadowRoot!.activeElement?.textContent ?? null);

test("the Annotations list: the hand-written file, then each it includes in its order, read when the list first opens", async ({ page }) => {
  // Out of name order, so the list's order can only be the include's.
  const reads = await serve(page, "two.annotations.toml", `format = "tickwave-annotations/1"\nrecording_id = "${ID}"\ninclude = ["z-first.jsonl", "a-second.jsonl"]\n`);
  // A page that names the channels: the collapsed view reads nothing of its sidecar for its facts.
  const view = await summary(page, "&download&channels=SWDIO&annotations=fixtures/two.annotations.toml");
  const button = view.locator(".download.annotations");
  await expect(button).toBeVisible();
  await page.waitForTimeout(500);
  expect(reads()).toBe(0);
  await button.click();
  expect(await links(view, 3)).toEqual(
    ["two.annotations.toml", "z-first.jsonl", "a-second.jsonl"].map((name) => ({ name, href: `/fixtures/${name}`, part: "annotations-download-link" })),
  );
  expect(reads()).toBe(1);
  // Closed and opened again, the same list, read once.
  await button.click();
  await button.click();
  await expect(view.locator(".files a")).toHaveCount(3);
  expect(reads()).toBe(1);
});

test("a view that has read its sidecar lists its files without reading the hand-written one again", async ({ page }) => {
  const reads = await serve(page, "identify-swd.annotations.toml", readFileSync("fixtures/identify-swd.annotations.toml", "utf8"));
  // Collapsed and on screen, the view reads its sidecar for the channels its facts count.
  const view = await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  await expect.poll(reads).toBe(1);
  await view.locator(".download.annotations").click();
  expect((await links(view)).map((l) => l.name)).toEqual(["identify-swd.annotations.toml", "identify-swd.swd.jsonl"]);
  expect(reads()).toBe(1);
});

test("pressed again, or with Escape, the list closes; the button keeps its width and place", async ({ page }) => {
  const view = await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  const [button, list] = [view.locator(".download.annotations"), view.locator(".files")];
  const box = () => button.evaluate((b) => JSON.stringify(b.getBoundingClientRect()));
  const closed = await box();
  await expect(button).toHaveAttribute("aria-expanded", "false");
  await button.click();
  await expect(list).toBeVisible();
  await expect(button).toHaveAttribute("aria-expanded", "true");
  expect(await box()).toBe(closed);
  await button.click();
  await expect(list).toBeHidden();
  await expect(button).toHaveAttribute("aria-expanded", "false");
  expect(await box()).toBe(closed);
  await button.click();
  await expect(list).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(list).toBeHidden();
  await expect(button).toHaveAttribute("aria-expanded", "false");
  expect(await focused(view)).toBe("Annotations");
  // A press inside the list but on no link leaves it open; one anywhere else closes it.
  await button.click();
  await expect(view.locator(".files a")).toHaveCount(2);
  const at = await list.evaluate((l) => l.getBoundingClientRect().toJSON());
  await page.mouse.click(at.left + at.width / 2, at.top + 2);
  await expect(list).toBeVisible();
  await page.mouse.click(5, 5);
  await expect(list).toBeHidden();
});

test("the list takes the focus at its first link, and gives it back to the button", async ({ page }) => {
  const view = await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  const [button, list] = [view.locator(".download.annotations"), view.locator(".files")];
  await button.focus();
  await page.keyboard.press("Enter");
  await expect(list).toBeVisible();
  expect(await focused(view)).toBe("identify-swd.annotations.toml");
  await expect(view.locator(".files a")).toHaveCount(2);
  await page.keyboard.press("Tab");
  expect(await focused(view)).toBe("identify-swd.swd.jsonl");
  // Escape in the list closes it and gives the button the focus.
  await page.keyboard.press("Escape");
  await expect(list).toBeHidden();
  expect(await focused(view)).toBe("Annotations");
  // Back to the button, the list stays open; Escape closes it there.
  await page.keyboard.press("Enter");
  expect(await focused(view)).toBe("identify-swd.annotations.toml");
  await page.keyboard.press("Shift+Tab");
  expect(await focused(view)).toBe("Annotations");
  await expect(list).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(list).toBeHidden();
  expect(await focused(view)).toBe("Annotations");
  // Tab past the last link: on to Open, and the list closes behind it.
  await page.keyboard.press("Enter");
  await page.keyboard.press("Tab");
  await page.keyboard.press("Tab");
  expect(await focused(view)).toBe("Open");
  await expect(list).toBeHidden();
});

test("with the list open, the summary and the page around the view stay where they were", async ({ page }) => {
  const view = await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  const around = () =>
    page.evaluate(() => {
      const r = (e: Element) => JSON.stringify(e.getBoundingClientRect());
      return { host: r(document.getElementById("v")!), far: r(document.getElementById("far")!), height: document.documentElement.scrollHeight };
    });
  const [boxesClosed, aroundClosed] = [await boxes(view), await around()];
  await view.locator(".download.annotations").click();
  await expect(view.locator(".files a")).toHaveCount(2);
  // Over the page: below the button, and painted above what is there.
  const [list, button] = await Promise.all([".files", ".download.annotations"].map((s) => view.locator(s).evaluate((e) => e.getBoundingClientRect().toJSON())));
  expect(list.top).toBeGreaterThan(button.bottom);
  expect(await boxes(view)).toEqual(boxesClosed);
  expect(await around()).toEqual(aroundClosed);
});

test("a hand-written file that doesn't read is listed alone, with why its includes aren't", async ({ page }) => {
  await serve(page, "gone.annotations.toml", null);
  const view = await summary(page, "&download&channels=SWDIO&annotations=fixtures/gone.annotations.toml");
  await view.locator(".download.annotations").click();
  await expect(view.locator(".files .note")).toHaveText("The files it includes can't be listed: /fixtures/gone.annotations.toml: HTTP 404");
  await expect(view.locator(".files .note")).toHaveAttribute("part", "annotations-download-note");
  expect(await links(view, 1)).toEqual([{ name: "gone.annotations.toml", href: "/fixtures/gone.annotations.toml", part: "annotations-download-link" }]);
});

test("a local file registered with its sidecar lists the sidecar's files, as their blobs under their names", async ({ page }) => {
  await page.goto(VIEW);
  await page.evaluate(async () => {
    const at = "/dist/tickwave.js";
    const tw = (await import(at)) as unknown as { registerFile(f: File, sidecar: File[]): string };
    const file = async (path: string, name: string) => new File([await (await fetch(path)).arrayBuffer()], name);
    // Given out of order: the list puts the hand-written file first.
    const src = tw.registerFile(await file("/fixtures/identify-swd.tickwave", "my capture.tickwave"), [
      await file("/fixtures/identify-swd.swd.jsonl", "identify-swd.swd.jsonl"),
      await file("/fixtures/identify-swd.annotations.toml", "identify-swd.annotations.toml"),
    ]);
    const v = document.createElement("tickwave-view");
    v.id = "local";
    v.setAttribute("src", src);
    v.setAttribute("download", "");
    document.body.prepend(v);
  });
  const view = page.locator("#local");
  await view.locator(".download.annotations").click();
  expect(await links(view)).toEqual(["identify-swd.annotations.toml", "identify-swd.swd.jsonl"].map((name) => ({ name, href: "blob:", part: "annotations-download-link" })));
  const [saved] = await Promise.all([page.waitForEvent("download"), view.locator(".files a").nth(1).click()]);
  expect(saved.suggestedFilename()).toBe("identify-swd.swd.jsonl");
  expect(readFileSync((await saved.path())!, "utf8")).toBe(readFileSync("fixtures/identify-swd.swd.jsonl", "utf8"));
});

test("a bundled recording the page names no sidecar for offers no list", async ({ page }) => {
  await page.goto("/e2e/pages/view.html?src=fixtures/identify-swd.bundle.tickwave&download");
  const view = page.locator("#v");
  await expect.poll(() => view.evaluate((v) => (v as unknown as { summary: unknown }).summary !== null), { timeout: 20_000 }).toBe(true);
  await expect(view.locator(".download").first()).toBeVisible();
  await expect(view.locator(".download.annotations")).toBeHidden();
});
