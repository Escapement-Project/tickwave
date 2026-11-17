// A view's download links (the `download` attribute): the recording as served, and with an
// `annotations` attribute its sidecar's hand-written file, beside Open in the summary line, which
// keeps its height and Open's place with them or without, open or closed.

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
  await expect(sidecar).toHaveAttribute("download", "identify-swd.annotations.toml");
  expect(await sidecar.evaluate((a) => (a as HTMLAnchorElement).href)).toMatch(/\/fixtures\/identify-swd\.annotations\.toml$/);
  await expect(file).toHaveAttribute("part", "button download-button");
  await expect(sidecar).toHaveAttribute("part", "button download-button annotations-download-button");
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
  const names = async () => {
    type Node = { role?: { value: string }; name?: { value: string } };
    const cdp = await page.context().newCDPSession(page);
    const { nodes } = (await cdp.send("Accessibility.getFullAXTree")) as unknown as { nodes: Node[] };
    return nodes.filter((n) => n.role?.value === "link" || n.role?.value === "button").map((n) => n.name?.value).filter((n) => n?.startsWith("Download") || n?.startsWith("Open"));
  };
  await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml");
  await expect.poll(names).toEqual(["Download identify-swd.tickwave", "Download the annotations of identify-swd.tickwave", "Open identify-swd.tickwave"]);
  await summary(page, "&download&annotations=fixtures/identify-swd.annotations.toml&title=The first read");
  await expect.poll(names).toEqual(["Download The first read", "Download the annotations of The first read", "Open The first read"]);
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
