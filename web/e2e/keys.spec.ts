// Keys held with Alt, Ctrl or Cmd are the browser's: Alt with an arrow goes back or forward, Ctrl
// or Cmd with plus or minus zooms the page. Every element that answers keys takes its own plain,
// and none of these. The keys are dispatched to the focused part as events, so that what the
// browser itself would do with them doesn't take the page away.

import { test, expect, type Locator } from "@playwright/test";

/** Whether `target` took `key`, plain and with each of Alt, Ctrl and Cmd: true where it was
 * prevented. */
async function taken(target: Locator, key: string): Promise<Record<string, boolean>> {
  return target.evaluate((t, key) => {
    const out: Record<string, boolean> = {};
    for (const mod of ["plain", "altKey", "ctrlKey", "metaKey"]) {
      const init: KeyboardEventInit = { key, bubbles: true, composed: true, cancelable: true };
      if (mod !== "plain") Object.assign(init, { [mod]: true });
      out[mod] = !t.dispatchEvent(new KeyboardEvent("keydown", init));
    }
    return out;
  }, key);
}

const PLAIN_ONLY = { plain: true, altKey: false, ctrlKey: false, metaKey: false };

for (const [what, url, selector, keys] of [
  ["the view's plot", "/e2e/pages/view.html?open", "#v .plot", ["ArrowLeft", "ArrowRight", "ArrowUp", "+", "-", "Home"]],
  ["a row's height handle", "/e2e/pages/view.html?open", "#v .row-handle >> nth=0", ["ArrowUp", "ArrowDown"]],
  ["an analog row's scale", "/e2e/pages/view.html?open&src=fixtures/swd50-cut.tickwave", "#v .scale >> nth=0", ["ArrowUp", "+"]],
  ["a channel's grip", "/e2e/pages/view.html?open&shaping", "#v .grip >> nth=1", ["ArrowLeft", "ArrowUp"]],
  ["a channel's trim handle", "/e2e/pages/view.html?open&shaping", "#v .trim >> nth=0", ["ArrowLeft"]],
  ["an input's head grip in a merge", "/e2e/pages/merge.html", "#v .grip.head >> nth=1", ["ArrowLeft", "ArrowRight"]],
  ["a chart", "/e2e/pages/chart.html", "#c1", ["ArrowRight", "Home"]],
  ["an inset", "/e2e/pages/origin.html?from=../../dist/", "#i .hit", ["ArrowLeft", "ArrowUp"]],
] as const) {
  test(`${what} takes its keys plain, and leaves them to the browser with Alt, Ctrl or Cmd`, async ({ page }) => {
    await page.goto(url);
    const target = page.locator(selector);
    await target.waitFor();
    // Each part answers its keys once its recording or data is in.
    await page.locator(selector.split(" ")[0]).evaluate((el) => (el as unknown as { settled?(): Promise<void> }).settled?.());
    for (const key of keys) expect(await taken(target, key), key).toEqual(PLAIN_ONLY);
  });
}
