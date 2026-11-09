// The shared header, on every page of the project (design/header.html). It looks the same at every
// scroll position; this script adds what changes: the hairline once the page has scrolled, the
// mark on the section the reader is in, and the menu of places on phones. Without it the header
// still works: the places show, and the menu button is only drawn on phones.
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

  // The menu of places, drawn on phones only. Open, the places follow the button in the tab order;
  // Escape closes it and gives the focus back to the button. The focus or a click going anywhere but
  // the button and its places closes it, a page's own controls in the header included, as does
  // following a place.
  const button = header.querySelector(".menu-button"), places = header.querySelector("nav");
  if (!button || !places) return;
  const open = () => header.classList.contains("open");
  const set = (to, refocus = false) => {
    header.classList.toggle("open", to);
    button.setAttribute("aria-expanded", String(to));
    if (!to && refocus) button.focus();
  };
  button.addEventListener("click", () => set(!open()));
  places.addEventListener("click", (e) => { if (e.target.closest("a")) set(false); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && open()) set(false, header.contains(document.activeElement)); });
  const inMenu = (el) => el instanceof Node && (button.contains(el) || places.contains(el));
  document.addEventListener("pointerdown", (e) => { if (open() && !inMenu(e.target)) set(false); });
  // The focus arriving anywhere else, whether it left the menu or was never in it (a click that
  // does not focus the button, as in Safari).
  document.addEventListener("focusin", (e) => { if (open() && !inMenu(e.target)) set(false); });
  header.addEventListener("focusout", (e) => { if (open() && e.relatedTarget === null && inMenu(e.target)) set(false); });
  // Wider than a phone the places show in the row and the menu is not drawn: nothing stays open.
  matchMedia("(max-width: 520px)").addEventListener("change", () => set(false));
})();
