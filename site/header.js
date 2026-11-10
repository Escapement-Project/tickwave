// The shared header, on every page of the project (design/header.html). It looks the same at every
// scroll position; this script adds what changes: the hairline once the page has scrolled, the mark
// on the section the reader is in, and, where the header scrolls away, its return when the reader
// scrolls up. Without it the header still works.
(() => {
  const header = document.querySelector("header[data-published]");
  if (!header) return;

  // --header, which keeps links into the page and the focus clear of the header, is the header's
  // own height wherever its rows land: base.css's values stand only until this has measured it.
  const root = document.documentElement;
  new ResizeObserver(() => root.style.setProperty("--header", `${Math.ceil(header.getBoundingClientRect().height)}px`)).observe(header);

  // The hairline: a marker at the top of the page says when it has left the screen.
  const marker = document.createElement("div");
  marker.setAttribute("aria-hidden", "true");
  marker.style.cssText = "position: absolute; top: 0; left: 0; width: 1px; height: 24px; pointer-events: none;";
  document.body.prepend(marker);
  new IntersectionObserver(([e]) => root.classList.toggle("scrolled", !e.isIntersecting)).observe(marker);

  // A page built with root-relative links (the measurements page) opened from disk: "/" would be
  // the filesystem's root, so the header's links go to the published site.
  if (location.protocol === "file:") {
    for (const a of header.querySelectorAll('a[href^="/"]')) a.href = header.dataset.published + a.getAttribute("href");
  }

  // A place that is a section of this page (the landing's Verge and Source) shows while the reader
  // is in it, as a place that is this page does: aria-current="location" on the section that
  // crosses a reading line, 30 % down the window or 200 px under the header, whichever is higher (in
  // a tall window 30 % falls past a section the reader has just brought to the top). At the end of
  // the page, where a short last section may never reach the line, the last such section whose top
  // is in the window is the one shown. The mark changes colour and underline only, so nothing moves.
  const page = (u) => u.origin + u.pathname.replace(/index\.html$/, "");
  const spots = new Map();
  for (const a of header.querySelectorAll("nav a")) {
    const u = new URL(a.href, location.href), section = u.hash && page(u) === page(location) && document.getElementById(u.hash.slice(1));
    if (section) spots.set(section, a);
  }
  if (spots.size) {
    const sections = [...spots.keys()];
    const mark = () => {
      const line = Math.min(innerHeight * 0.3, header.getBoundingClientRect().bottom + 200);
      const atEnd = innerHeight + scrollY >= root.scrollHeight - 2;
      const here = atEnd ? sections.filter((s) => s.getBoundingClientRect().top < innerHeight).at(-1)
        : sections.find((s) => { const r = s.getBoundingClientRect(); return r.top <= line && r.bottom > line; });
      for (const [section, a] of spots) {
        if (section === here) a.setAttribute("aria-current", "location");
        else a.removeAttribute("aria-current");
      }
    };
    mark();
    addEventListener("scroll", mark, { passive: true });
    addEventListener("resize", mark);
  }

  // Where the header scrolls away with the page (the sheets set --header-returns: on a phone, and on
  // a doc page narrower than its contents rail), it comes back as one row when the reader scrolls
  // up, and leaves again on the way down. Once it is out of sight it waits fixed above the window,
  // and the page keeps its place with padding the size of the header; back at the top of the page it
  // is in the flow again. A jump, as to a link in the page, sends it away.
  let flow = 0, last = scrollY, moves = 0;
  const has = (name) => root.classList.contains(name);
  const away = (to) => {
    if (to) {
      root.style.setProperty("--header-flow", `${flow}px`);
      root.classList.add("header-away");
      root.style.setProperty("--header-back", `${header.getBoundingClientRect().height}px`);
      // Out of sight first, then moving: the header must not slide up past the reader as it goes.
      // A return before that frame cancels it, or a later going away would slide.
      cancelAnimationFrame(moves);
      moves = requestAnimationFrame(() => root.classList.add("header-moves"));
    } else {
      cancelAnimationFrame(moves);
      root.classList.remove("header-away", "header-back", "header-moves");
    }
  };
  const place = () => {
    if (getComputedStyle(root).getPropertyValue("--header-returns").trim() !== "1") {
      if (has("header-away")) away(false);
      last = scrollY;
      return;
    }
    // Its exact height: a whole pixel would move the page by the fraction as it goes away.
    if (!has("header-away")) flow = header.getBoundingClientRect().height;
    const out = scrollY > flow;
    if (out !== has("header-away")) away(out);
    const moved = scrollY - last;
    if (!out || Math.abs(moved) > innerHeight) {
      root.classList.remove("header-back");
      last = scrollY;
    } else if (Math.abs(moved) > 6) {
      if (moved < 0) root.classList.add("header-back");
      else if (!header.contains(document.activeElement)) root.classList.remove("header-back");
      last = scrollY;
    }
  };
  place();
  addEventListener("scroll", place, { passive: true });
  // A new width, as when a phone turns, can give the header another height in either layout: back
  // in the flow to measure it, then away again if it was, all before the next paint.
  addEventListener("resize", () => {
    const back = has("header-back");
    if (has("header-away")) away(false);
    place();
    if (back && has("header-away")) root.classList.add("header-back");
  });
  header.addEventListener("focusin", () => { if (has("header-away")) root.classList.add("header-back"); });
})();
