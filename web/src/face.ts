// The page's face and fonts: elements re-resolve their colours when the face changes, and
// re-measure text when a web font arrives.

const faceListeners = new Set<() => void>();
const fontListeners = new Set<() => void>();
let installed = false;
let queued = false;

/** The tokens every element draws with, read on <html>. */
const TOKENS = ["--ink", "--ink-2", "--page", "--surface", "--raised", "--hair", "--rule", "--wash", "--font-text", "--font-figures"];
let face = "";

/** The face as <html> gives it: its colour scheme, its text colour and the tokens. */
function currentFace(): string {
  const style = getComputedStyle(document.documentElement);
  return [style.colorScheme, style.color, ...TOKENS.map((t) => style.getPropertyValue(t))].join("\n");
}

/** Tells the elements when the face has changed, in the next frame: a page's CSS takes the new
 * face in that frame's styles, and a canvas redrawn in its callbacks paints with them. A page may
 * switch the face by `data-face`, a class or a style on <html>, or leave it to the system's
 * preference; a change there that leaves the face alone, such as a header marking the page
 * scrolled, redraws nothing. */
function fire(): void {
  const now = currentFace();
  if (now === face) return;
  face = now;
  if (queued) return;
  queued = true;
  requestAnimationFrame(() => {
    queued = false;
    for (const f of faceListeners) f();
  });
}

function install(): void {
  if (installed) return;
  installed = true;
  face = currentFace();
  new MutationObserver(fire).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-face", "class", "style"],
  });
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", fire);
  document.fonts?.addEventListener("loadingdone", () => {
    for (const f of fontListeners) f();
  });
}

export function onFaceChange(f: () => void): () => void {
  install();
  faceListeners.add(f);
  return () => faceListeners.delete(f);
}

export function onFontsLoaded(f: () => void): () => void {
  install();
  fontListeners.add(f);
  return () => fontListeners.delete(f);
}

export const reducedMotion = (): boolean => matchMedia("(prefers-reduced-motion: reduce)").matches;
