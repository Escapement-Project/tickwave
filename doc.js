// A doc page's contents, shared by the project's docs: the markup it expects is in design/README.md,
// and the site's is templates/verge-doc.html. It marks the section the reader is in, in the contents
// and in the bar narrow screens keep at the top, and opens and closes the bar's sheet of contents
// (and search, where the page has it). Without it the contents still work: every link is there, the
// reader's group open.
(() => {
  const wrap = document.querySelector(".doc-wrap");
  if (!wrap) return;
  const rail = wrap.querySelector(".doc-rail"), list = rail.querySelector(".doc-list"), nav = rail.querySelector(".doc-nav");
  const bar = wrap.querySelector(".doc-bar"), where = bar.querySelector(".where");
  const title = where.textContent;

  // Each section link and its heading, in the page's order.
  const spots = [...nav.querySelectorAll(".toc a")]
    .map((a) => [a, document.getElementById(decodeURIComponent(a.hash.slice(1)))])
    .filter(([, h]) => h);
  const narrow = matchMedia("(max-width: 999px)");
  const toc = nav.querySelector(".toc"), page = nav.querySelector('[aria-current="page"]'), pulse = document.createElement("span");
  pulse.className = "pulse";
  pulse.setAttribute("aria-hidden", "true");
  if (toc) toc.append(pulse);

  // The section in view is the last one whose heading has passed a reading line: a little under
  // whatever stays at the top of the window (the header wide, the bar narrow).
  const mark = () => {
    const top = narrow.matches ? bar.getBoundingClientRect().bottom
      : document.querySelector("header").getBoundingClientRect().bottom;
    const line = Math.max(0, top) + Math.min(120, innerHeight * 0.2);
    let here = null;
    for (const [a, h] of spots) {
      if (h.getBoundingClientRect().top <= line) here = a;
      else break;
    }
    for (const [a] of spots) {
      if (a === here) a.setAttribute("aria-current", "location");
      else a.removeAttribute("aria-current");
    }
    // A subsection's section opens its list, so the subsection shows.
    for (const li of nav.querySelectorAll(".toc li.here")) li.classList.remove("here");
    const section = here && here.closest(".toc > li");
    if (section) section.classList.add("here");
    where.textContent = here ? here.textContent : title;
    // The pulse beside the section, or above the first section beside the doc itself, so it moves
    // into the sections rather than appearing; none while the contents are hidden.
    const at = here || page;
    pulse.classList.toggle("on", Boolean(at) && at.offsetHeight > 0);
    if (at && at.offsetHeight > 0) {
      const r = at.getBoundingClientRect();
      pulse.style.top = `${r.top - toc.getBoundingClientRect().top + 4}px`;
      pulse.style.height = `${r.height - 8}px`;
    }
    // Keep the mark in sight in a rail taller than the window, without moving the page.
    if (here && here.offsetHeight > 0 && !narrow.matches && list.scrollHeight > list.clientHeight) {
      const r = here.getBoundingClientRect(), n = list.getBoundingClientRect();
      if (r.top < n.top + 40 || r.bottom > n.bottom - 48) list.scrollTop += r.top - n.top - list.clientHeight / 3;
    }
  };
  // A rail taller than the window fades at an edge with more past it.
  const edges = () => {
    list.classList.toggle("more-above", list.scrollTop > 2);
    list.classList.toggle("more-below", list.scrollTop + list.clientHeight < list.scrollHeight - 2);
  };
  list.addEventListener("scroll", edges, { passive: true });
  addEventListener("resize", edges);
  // A group opening or closing, or the search's results taking the contents' place, changes what
  // the list holds. The mark waits a frame: placing it can open a section, which resizes the list.
  let marking = 0;
  const changed = () => {
    edges();
    cancelAnimationFrame(marking);
    if (spots.length) marking = requestAnimationFrame(mark);
  };
  const sizes = new ResizeObserver(changed);
  for (const el of rail.querySelectorAll(".doc-nav, .found")) sizes.observe(el);
  edges();
  if (spots.length) {
    mark();
    // Placed first where it belongs, then moving from there.
    requestAnimationFrame(() => nav.classList.add("moving"));
    addEventListener("scroll", mark, { passive: true });
    addEventListener("resize", mark);
  }

  // The sheet, narrow only. It opens under the bar, wherever the bar is; a link, Escape, or a
  // click or focus outside the bar and the sheet closes it. It opens on the reader's place: the
  // section, or the search's result, the page shows.
  const open = () => wrap.classList.contains("open");
  const set = (to, refocus = false) => {
    if (to) wrap.style.setProperty("--bar-bottom", `${Math.ceil(bar.getBoundingClientRect().bottom)}px`);
    wrap.classList.toggle("open", to);
    bar.setAttribute("aria-expanded", String(to));
    if (to) {
      nav.classList.remove("moving");
      mark();
      requestAnimationFrame(() => nav.classList.add("moving"));
      const here = [...rail.querySelectorAll('.found .current, [aria-current="location"], [aria-current="page"]')].find((el) => el.offsetHeight > 0);
      if (here) list.scrollTop += here.getBoundingClientRect().top - list.getBoundingClientRect().top - list.clientHeight / 3;
    }
    if (!to && refocus) bar.focus();
  };
  bar.addEventListener("click", () => set(!open()));
  rail.addEventListener("click", (e) => { if (open() && e.target.closest("a")) set(false); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && open()) set(false, true); });
  const inside = (el) => el instanceof Node && (bar.contains(el) || rail.contains(el));
  document.addEventListener("pointerdown", (e) => { if (open() && !inside(e.target)) set(false); });
  document.addEventListener("focusin", (e) => { if (open() && !inside(e.target)) set(false); });
  narrow.addEventListener("change", () => { set(false); mark(); });
})();
