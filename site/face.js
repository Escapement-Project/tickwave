// The day/night switch, on every page. The system's face until the switch is used, then the one
// chosen, kept in this browser; the head's inline script applies a kept choice before first paint.
// A page that draws in the face's colours listens for "facechange".
(() => {
  const root = document.documentElement, sw = document.getElementById("faces");
  if (!sw) return;
  const shown = () => getComputedStyle(root).colorScheme === "dark" ? "night" : "day";
  const sync = () => sw.setAttribute("aria-checked", shown() === "night");
  sync();
  sw.addEventListener("click", () => {
    const face = shown() === "night" ? "day" : "night";
    root.dataset.face = face;
    try { localStorage.setItem("face", face); } catch { /* the choice lasts this visit only */ }
    sync();
    document.dispatchEvent(new CustomEvent("facechange"));
  });
  // While nothing is chosen the face follows the system, and the switch and the page's drawings with it.
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    sync();
    document.dispatchEvent(new CustomEvent("facechange"));
  });
  // The switch moves only once the page has drawn it where it belongs, so a night page loads still.
  requestAnimationFrame(() => requestAnimationFrame(() => sw.classList.add("ready")));
})();
