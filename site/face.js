// The day/night switch, on every page. The system's face until the switch is used, then the one
// chosen, kept in this browser; the head's inline script applies a kept choice before first paint.
// A page that draws in the face's colours listens for "facechange", which comes as the page's
// colours pass half-way while they turn (base.css's .face-turning), at once otherwise.
(() => {
  const root = document.documentElement, sw = document.getElementById("faces");
  if (!sw) return;
  const shown = () => getComputedStyle(root).colorScheme === "dark" ? "night" : "day";
  const sync = () => sw.setAttribute("aria-checked", shown() === "night");
  sync();
  // The colours follow the thumb over its travel (base.css's .face-turning). The class goes on with
  // the new face, in the same style change, so the change itself is what transitions.
  const still = matchMedia("(prefers-reduced-motion: reduce)");
  const changed = () => document.dispatchEvent(new CustomEvent("facechange"));
  let turning = 0, half = 0;
  sw.addEventListener("click", () => {
    const face = shown() === "night" ? "day" : "night";
    if (!still.matches) {
      root.classList.add("face-turning");
      clearTimeout(turning); clearTimeout(half);
      // Long enough for a drawing changed at the half-way point to finish its own fade.
      turning = setTimeout(() => root.classList.remove("face-turning"), 360);
    }
    root.dataset.face = face;
    try { localStorage.setItem("face", face); } catch { /* the choice lasts this visit only */ }
    sync();
    if (still.matches) changed();
    else half = setTimeout(changed, 91); // base.css's half-way point, where the thumb takes the new face
  });
  // While nothing is chosen the face follows the system, and the switch and the page's drawings with it.
  // That change is instant: the event comes after the new face is drawn, too late to transition it.
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    sync();
    changed();
  });
  // The switch moves only once the page has drawn it where it belongs, so a night page loads still.
  requestAnimationFrame(() => requestAnimationFrame(() => sw.classList.add("ready")));
})();
