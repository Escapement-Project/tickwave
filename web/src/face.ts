// The page's face and fonts: elements re-resolve their colours when `data-face` on <html> or the
// system's preference changes, and re-measure text when a web font arrives.

const faceListeners = new Set<() => void>();
const fontListeners = new Set<() => void>();
let installed = false;
let queued = false;

function fire(): void {
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
