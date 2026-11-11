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

  // The project's mark in motion: its edge is a signal through the ring. Lock (arrive) as the
  // landing's hero comes into view: from the low run alone, a chirp passes through the ring at one
  // speed, its pulses widening, and slows to rest as one rising edge reaches the centre, the mark. Again on the logo,
  // when a mouse or pen reaches it (a tap follows the link instead): from the mark the signal moves
  // on through a new chirp and locks again, gathering speed as Lock sheds it. tickwave's ruler runs
  // under the signal while it moves, raised ticks on its edges, and fades once it stops.
  //
  // Every number is a --project-mark-* property in tokens.css, built from design/tokens.toml's
  // [motion.project-mark] and the mark's own geometry; a page whose tokens.css lacks them keeps the
  // mark still. The mark's own ring and edge paths stay as the page drew them: the ruler goes under
  // the ring, and the moving trace replaces the edge only while it moves, with the clips it needs,
  // so at rest the svg is as the page made it and refers to nothing by id (reader view breaks such
  // references). Every write is inside the svg, never on <html>, which tickwave's pages watch.
  const ProjectMark = (() => {
    const still = matchMedia("(prefers-reduced-motion: reduce)"), f = (v) => v.toFixed(2);
    const NAMES = ["inner", "outer", "width", "riser", "low", "high", "low-bottom", "speed", "again", "ramp", "densest",
      "dense", "growth", "fill", "lead", "tick", "spacing", "raised", "lower", "line", "clear", "cut", "gap-px", "tick-px",
      "spacing-px", "raised-px", "lower-px", "fade-in", "fade-out", "lower-opacity"];
    const tokens = () => {
      const style = getComputedStyle(root), t = {};
      for (const n of NAMES) t[n] = parseFloat(style.getPropertyValue(`--project-mark-${n}`));
      return NAMES.every((n) => Number.isFinite(t[n])) ? t : null;
    };
    const animate = (ms, frame, done) => {
      const t0 = performance.now(); let raf;
      const tick = (now) => { const t = Math.min(1, (now - t0) / ms); frame(t); if (t < 1) raf = requestAnimationFrame(tick); else done?.(); };
      raf = requestAnimationFrame(tick);
      return () => cancelAnimationFrame(raf);
    };
    let uid = 0;

    // The measures at the size drawn: below a pixel's worth each keeps its floor, so the 24 px logo
    // still shows its pulses apart and its ticks.
    function fit(t, svg) {
      const u = (px) => px / ((svg.getBoundingClientRect().width || 64) / 64), raised = Math.max(t.raised, u(t["raised-px"]));
      return {
        densest: Math.max(t.densest, t.width + u(t["gap-px"])), tick: Math.max(t.tick, u(t["tick-px"])),
        spacing: Math.max(t.spacing, u(t["spacing-px"])), raised, lower: Math.max(t.lower, u(t["lower-px"])),
        line: Math.max(t.line, t["low-bottom"] + t.clear + raised),
      };
    }
    // The chirp's runs: some of the densest half-period, then each longer by the growth until a low
    // reaches the fill, the last run; the levels alternate back from it, then high for good.
    function chirp(t, m) {
      const L = Array(t.dense).fill(m.densest);
      for (let h = m.densest; L.at(-1) < t.fill; ) L.push(h *= t.growth);
      if (L.length % 2) L.unshift(m.densest);
      return signal(L);
    }
    function signal(L) { let u = 0; const edges = L.map((l) => (u += l)); return { L, edges, end: u }; }
    // Lock's signal: the low run alone for the lead, then the chirp. The levels count back from the
    // end, so the run in front is low and the chirp's levels stay as they were.
    function lock(t, m) { return signal([t.lead, ...chirp(t, m).L]); }
    // The trace with `s` units of the signal at the riser's place.
    function trace(t, sig, s) {
      const n = sig.L.length, level = (i) => (i >= n || (n - 1 - i) % 2 ? t.high : t.low);
      let u = 0, i = 0;
      while (i < n && u + sig.L[i] <= s - t.riser) u += sig.L[i++];
      let d = `M0,${level(i)}`;
      for (; i < n; i++) {
        u += sig.L[i];
        const x = t.riser + u - s;
        if (x >= 64) break;
        d += `H${f(x)}V${level(i + 1)}`;
      }
      return d + "H64";
    }
    // The ruler: raised ticks on the edges, lower ones splitting each run (and 60 units beyond
    // either end) at about the spacing, standing on the line and running on under the ring.
    function ruler(t, m, sig, s) {
      const near = (u) => Math.abs(u - s) < t.outer + 2;
      const tick = (u, top) => {
        const dx = u - s; if (Math.abs(dx) >= t.outer) return "";
        const floor = Math.min(m.line, 32 + Math.sqrt(t.outer ** 2 - dx ** 2));
        return floor > top + 0.3 ? `M${f(t.riser + dx)},${f(top)}V${f(floor)}` : "";
      };
      const pts = [sig.edges[0] - 60, ...sig.edges, sig.edges.at(-1) + 60];
      let raised = "", lower = "";
      for (const e of sig.edges) if (near(e)) raised += tick(e, m.line - m.raised);
      for (let i = 0; i < pts.length - 1; i++) {
        const a = pts[i], b = pts[i + 1], n = Math.max(1, Math.round((b - a) / m.spacing));
        for (let j = 1; j < n; j++) { const u = a + ((b - a) * j) / n; if (near(u)) lower += tick(u, m.line - m.lower); }
      }
      return [raised, lower];
    }

    function mount(svg) {
      const [ring, edge] = svg.querySelectorAll(":scope > path"), id = `project-mark-${++uid}`;
      if (!ring || !edge) return null;
      // The ruler under the ring, its fade an inline style on a group of its own, so no page rule
      // overrides it.
      ring.insertAdjacentHTML("beforebegin", `<g class="pm-ruler"><g class="pm-fade" fill="none" style="opacity: 0">`
        + `<path style="stroke: var(--project-mark-lower-colour)"/><path style="stroke: var(--project-mark-raised-colour)"/></g></g>`);
      const group = svg.querySelector(".pm-ruler"), fade = group.firstElementChild, [lowerPath, raisedPath] = fade.children;
      let t = null, m = null, stop = null, fading = null;
      // While it moves: the trace clipped to the ring's inside, the ruler to a circle inside its
      // outside, so a tick's width never shows past the ring.
      const live = () => {
        let p = svg.querySelector(".pm-live path");
        if (p) return p;
        edge.insertAdjacentHTML("afterend", `<clipPath id="${id}-in"><circle cx="32" cy="32" r="${t.inner}"/></clipPath>`
          + `<clipPath id="${id}-out"><circle cx="32" cy="32" r="${t.cut}"/></clipPath>`
          + `<g class="pm-live" clip-path="url(#${id}-in)"><path fill="none" stroke="currentColor" stroke-width="${t.width}" stroke-linejoin="miter" stroke-miterlimit="4"/></g>`);
        group.setAttribute("clip-path", `url(#${id}-out)`);
        edge.style.display = "none";
        return svg.querySelector(".pm-live path");
      };
      const rest = () => { svg.querySelector(".pm-live")?.remove(); svg.querySelector(`#${id}-in`)?.remove(); edge.style.display = ""; };
      const unclip = () => { svg.querySelector(`#${id}-out`)?.remove(); group.removeAttribute("clip-path"); };
      const draw = (sig, s) => {
        live().setAttribute("d", trace(t, sig, s));
        const [raised, lower] = ruler(t, m, sig, s);
        raisedPath.setAttribute("d", raised); lowerPath.setAttribute("d", lower);
      };
      const fadeTo = (to, ms, done) => {
        fading?.();
        const from = Number(fade.style.opacity) || 0;
        fading = animate(ms, (k) => { fade.style.opacity = f(from + (to - from) * (1 - (1 - k) ** 3)); }, () => { fading = null; done?.(); });
      };
      // Each play measures afresh, as the size or the tokens may have changed since the last.
      const begin = () => {
        if (still.matches || stop || !(t = tokens())) return false;
        m = fit(t, svg);
        fade.setAttribute("stroke-width", f(m.tick));
        lowerPath.setAttribute("stroke-opacity", t["lower-opacity"]);
        return true;
      };
      // Move `sig` from `from` to its end at `v` units a ms: from rest over the first `start` units,
      // to rest over the last ramp, each at a constant rate, so the speed never jumps. Then the
      // mark's own edge, and the ruler fades and goes with its clip.
      const run = (sig, from, start, v) => {
        const D = sig.end - from, T = Math.min(t.ramp, D), A = Math.min(start, D - T);
        const ta = (2 * A) / v, tc = (D - A - T) / v, tt = (2 * T) / v, total = ta + tc + tt;
        const at = (ms) => {
          if (ms <= ta) return (v / (2 * ta)) * ms ** 2;
          if (ms <= ta + tc) return A + v * (ms - ta);
          const k = ms - ta - tc;
          return D - T + v * k - (v / (2 * tt)) * k ** 2;
        };
        stop = animate(total, (k) => draw(sig, from + (k >= 1 ? D : at(k * total))), () => {
          stop = null; rest();
          fadeTo(0, t["fade-out"] * 1000, () => { raisedPath.removeAttribute("d"); lowerPath.removeAttribute("d"); unclip(); });
        });
      };
      const first = (sig) => { draw(sig, t.riser); fading?.(); fade.style.opacity = "1"; };
      return {
        // Lock's first frame, held until it plays: the low run alone, with the ruler.
        ready() { if (begin()) first(lock(t, m)); },
        arrive() { if (!begin()) return; const sig = lock(t, m); first(sig); run(sig, t.riser, 0, t.speed / 1000); },
        // From the mark on: its last low, a high as long (so the first frame is the mark), one dense
        // run so the levels carry on, then a new chirp, ending on the mark again.
        again() {
          if (!begin()) return;
          const c = chirp(t, m), last = c.L.at(-1), sig = signal([last, last, m.densest, ...c.L]);
          fadeTo(1, t["fade-in"] * 1000);
          run(sig, last, t.ramp, t.again / 1000);
        },
      };
    }
    return { mount };
  })();
  window.ProjectMark = ProjectMark;
  const brand = header.querySelector(".brand"), logo = brand?.querySelector(":scope > svg");
  if (logo) {
    let mark;
    brand.addEventListener("pointerenter", (e) => { if (e.pointerType !== "touch") (mark ??= ProjectMark.mount(logo))?.again(); });
  }
})();
