// Keys the elements leave to the browser.

/** A key pressed with Alt, Ctrl or Cmd is the browser's, as Alt with an arrow goes back or forward
 * and Ctrl or Cmd with plus or minus zooms the page: no element answers one. */
export function browserKey(e: KeyboardEvent): boolean {
  return e.altKey || e.ctrlKey || e.metaKey;
}
