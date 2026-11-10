// The page's face and fonts: elements re-resolve their colours when the face changes, and
// re-measure text when a web font arrives.

const faceListeners = new Set<() => void>();
const fontListeners = new Set<() => void>();
let installed = false;
let queued = false;

/** The tokens every element draws with, read on <html>. */
const TOKENS = ["--ink", "--ink-2", "--page", "--surface", "--raised", "--hair", "--rule", "--wash", "--font-text", "--font-figures"];
let face = "";

/** The transition fading <html>'s text colour, while a page fades its colours as its face turns. */
function fading(): CSSTransition | null {
  for (const a of document.documentElement.getAnimations()) {
    if (a instanceof CSSTransition && a.transitionProperty === "color" && a.playState === "running") return a;
  }
  return null;
}

/** A fade's colours, from and to. */
function ends(fade: CSSTransition): [string, string] {
  const frames = (fade.effect as KeyframeEffect).getKeyframes();
  return [String(frames[0]?.color ?? ""), String(frames.at(-1)?.color ?? "")];
}

const rgb = (c: string): number[] | null => {
  const m = /^rgba?\(([^)]*)\)$/.exec(c.trim());
  const v = m?.[1].split(/[\s,/]+/).slice(0, 3).map(Number);
  return v && v.length === 3 && v.every(Number.isFinite) ? v : null;
};
const apart = (a: number[], b: number[]) => a.reduce((s, x, i) => s + (x - b[i]) ** 2, 0);

/** The face as <html> gives it: its colour scheme, its text colour and the tokens. Mid-fade, the
 * text colour it is fading to, so the fade's end is not taken for another change. */
function currentFace(): string {
  const style = getComputedStyle(document.documentElement);
  const fade = fading();
  const color = fade ? ends(fade)[1] || style.color : style.color;
  return [style.colorScheme, color, ...TOKENS.map((t) => style.getPropertyValue(t))].join("\n");
}

/** Calls `then` on the first frame where <html>'s colour is nearer the face it is fading to than
 * the one it left: a canvas cannot fade, so it turns there, as a page's drawings made by script do. */
function halfway(fade: CSSTransition, then: () => void): void {
  const [from, to] = ends(fade).map(rgb);
  const step = () => {
    const now = rgb(getComputedStyle(document.documentElement).color);
    if (!from || !to || !now || fade.playState !== "running" || apart(now, to) <= apart(now, from)) then();
    else requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

/** Tells the elements when the face has changed. A page may switch it by `data-face`, a class or a
 * style on <html>, or leave it to the system's preference; a change there that leaves the face
 * alone, such as a header marking the page scrolled, redraws nothing. Where the page fades its
 * colours, the elements hear of it half-way through the fade. */
function fire(): void {
  const now = currentFace();
  if (now === face) return;
  face = now;
  if (queued) return;
  queued = true;
  const tell = () => {
    queued = false;
    for (const f of faceListeners) f();
  };
  const fade = fading();
  if (fade) halfway(fade, tell);
  else requestAnimationFrame(tell);
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
