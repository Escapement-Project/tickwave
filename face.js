// The day/night switch, on every page. The system's face until the switch is used, then the one
// chosen, kept in this browser; the head's inline script applies a kept choice before first paint.
// A page that draws in the face's colours listens for "facechange", which comes once the new face
// applies: while the page is covered when the switch turns it, at once otherwise.
(() => {
  const root = document.documentElement, sw = document.getElementById("faces");
  if (!sw) return;
  const shown = () => getComputedStyle(root).colorScheme === "dark" ? "night" : "day";
  const sync = () => sw.setAttribute("aria-checked", shown() === "night");
  sync();
  const changed = () => document.dispatchEvent(new CustomEvent("facechange"));
  const choose = (face) => {
    root.dataset.face = face;
    try { localStorage.setItem("face", face); } catch { /* the choice lasts this visit only */ }
    sync();
    changed();
  };
  const still = matchMedia("(prefers-reduced-motion: reduce)");
  const ms = (v) => parseFloat(v) * (v.trim().endsWith("ms") ? 1 : 1000) || 0;
  let turning = false;
  sw.addEventListener("click", () => {
    if (turning) return;
    const from = shown(), face = from === "night" ? "day" : "night";
    if (still.matches || !sw.showPopover) { choose(face); return; }
    turning = true;
    // The page dips to the new face's background and back up (base.css's .face-cover). The face
    // changes while the cover is full, which it stays for at least 50 ms: a page's script that
    // redraws on the change draws in the next frame or two (tickwave's canvases take up to ~45 ms
    // on a slow machine).
    const turn = ms(getComputedStyle(root).getPropertyValue("--face-turn"));
    const hold = Math.max(50, turn * 0.2), rise = Math.max(0, (turn - hold) / 2);
    const cover = document.createElement("div");
    cover.className = "face-cover";
    cover.popover = "manual";
    cover.dataset.face = face;
    cover.setAttribute("aria-hidden", "true");
    const wash = document.createElement("i");
    // The switch's copy, where the switch is, in the old face until the cover passes half-way.
    const copy = sw.cloneNode(true);
    copy.removeAttribute("id");
    copy.tabIndex = -1;
    copy.dataset.face = from;
    copy.classList.toggle("focus", sw.matches(":focus-visible"));
    const at = sw.getBoundingClientRect();
    Object.assign(copy.style, { left: `${at.left}px`, top: `${at.top}px`, width: `${at.width}px`, height: `${at.height}px` });
    cover.append(wash, copy);
    document.body.append(cover);
    cover.showPopover();
    // Drawn where it starts, so both thumbs travel from here; the real one under the copy.
    copy.getBoundingClientRect();
    copy.setAttribute("aria-checked", face === "night");
    sw.setAttribute("aria-checked", face === "night");
    const up = wash.animate([{ opacity: 0 }, { opacity: 1 }], { duration: rise, easing: "cubic-bezier(.4, 0, 1, 1)", fill: "forwards" });
    const flip = () => {
      if (Number(getComputedStyle(wash).opacity) >= 0.5 || up.playState !== "running") copy.dataset.face = face;
      else requestAnimationFrame(flip);
    };
    requestAnimationFrame(flip);
    const done = () => { cover.remove(); turning = false; };
    // From a task, not a frame callback: a script that waits a frame on the change (tickwave's
    // face.ts) then draws in the very next one. The hold counts from the change itself, as a timer
    // can fire late.
    setTimeout(() => {
      choose(face);
      wash.animate([
        { opacity: 1 },
        { opacity: 1, offset: hold / (hold + rise), easing: "cubic-bezier(0, 0, .2, 1)" },
        { opacity: 0 },
      ], { duration: hold + rise }).finished.then(done, done);
    }, rise);
  });
  // While nothing is chosen the face follows the system, and the switch and the page's drawings with it.
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    sync();
    changed();
  });
  // The switch moves only once the page has drawn it where it belongs, so a night page loads still.
  requestAnimationFrame(() => requestAnimationFrame(() => sw.classList.add("ready")));
})();
