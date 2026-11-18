// The marks in motion, on every plate that carries one: an element with data-mark="verge" holding
// design/marks/verge.svg, or data-mark="tickwave" holding design/marks/tickwave.svg. A mark plays
// once when its plate is first wholly in view below the header, once any mark the page plays first
// is still, and again when the pointer or a tap reaches it. Both reach the project's step, hold
// there and return, in the same times.
//
// Verge's, in tickwave's grammar of a capture taken again: the V dims to grey as a floor and stems
// grow under it; a beam sweeps the plate, and as it reaches each sample the grey one fades and a new
// one rises in lume from the floor to its place on the project's step, the riser's stem in lume (the
// step's edge); a hold, over whose end the step dims; the next sweep takes the V again the same way.
// Every number comes from the design tokens (design/tokens.toml's [motion.mark] and
// design/marks/verge.svg, built into design/out/mark.json, which the page embeds as #mark), as
// verge-gui's does: this is only how to draw it, into the plate's own 64-unit box. The mark's own
// circles are the lit samples; the lines and the dimmed samples live in a group under them and the
// beam in one over them, both removed at the end, so at rest the plate holds the mark and nothing
// else.
(() => {
  const plates = document.querySelectorAll("[data-mark]");
  if (!plates.length) return;
  const json = (id) => { const el = document.getElementById(id); return el && JSON.parse(el.textContent); };
  const MARK = json("mark"), TW = json("tickwave-mark");
  const reduced = matchMedia("(prefers-reduced-motion: reduce)");
  const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const f1 = (v) => v.toFixed(1), f2 = (v) => v.toFixed(2);
  const clamp01 = (v) => Math.max(0, Math.min(1, v));

  function vergeAt(t) {
    const m = MARK, g = m.geometry, ease = (x) => 1 - (1 - clamp01(x)) ** m.ease;
    const lerp = (a, b, p) => a + (b - a) * p, reach = (i, t0) => t0 + m.grid_x[i] / 64 * m.sweep;
    const dim1 = clamp01(t / m.grow), dim2 = clamp01((t - m.back + m.grow) / m.grow);
    // Each sample: the capture's [height, lit, dimmed], whether it stands on the step, and the
    // previous capture's [height, dimmed], fading once the beam has passed.
    const samples = m.grid_x.map((_, i) => {
      if (t < reach(i, m.grow)) return { now: [g.v[i], 1 - dim1, dim1], step: false, was: null };
      const p1 = ease((t - reach(i, m.grow)) / m.move);
      if (t < reach(i, m.back)) return { now: [lerp(g.floor, g.step[i], p1), p1 * (1 - dim2), p1 * dim2], step: true, was: [g.v[i], 1 - p1] };
      const p2 = ease((t - reach(i, m.back)) / m.move);
      return { now: [lerp(g.floor, g.v[i], p2), p2, 0], step: false, was: [g.step[i], 1 - p2] };
    });
    const across = (t0) => (t - t0) / m.sweep * 64, s1 = m.grow, s2 = m.back;
    const beam = t >= s1 && t < s1 + m.sweep ? across(s1) : t >= s2 && t < s2 + m.sweep ? across(s2) : null;
    // The step's edge, drawn behind the first beam and wiped by the second.
    const [start, finish] = m.floor_run;
    const from = t < s2 ? start : Math.max(start, beam ?? finish);
    const to = t < s1 ? start : t < s1 + m.sweep ? Math.max(start, beam) : t < s2 + m.sweep ? finish : start;
    const lines = clamp01(Math.min(t / m.grow, (m.end - t) / m.grow));
    return { samples, beam, trace: to > from ? [from, to] : null, lines };
  }

  function playVerge(plate) {
    const svg = plate.querySelector("svg"), dots = [...svg.querySelectorAll("circle")];
    if (reduced.matches || plate.dataset.playing) return;
    plate.dataset.playing = "1";
    const NS = "http://www.w3.org/2000/svg", m = MARK, g = m.geometry, x = (grid) => g.x0 + g.k * grid;
    const grey = css("--plate-line"), riser = m.grid_x.indexOf(m.riser);
    // The lines and the dimmed samples under the lit ones, the beam over them.
    const group = (where) => {
      const el = document.createElementNS(NS, "g");
      el.setAttribute("fill", "none"); el.setAttribute("stroke", grey);
      return where(el);
    };
    const under = group((el) => svg.insertBefore(el, dots[0])), over = group((el) => svg.appendChild(el));
    const line = (x1, y1, x2, y2, w, o, paint = "") => o > 0.004 ? `<line x1="${f1(x1)}" y1="${f1(y1)}" x2="${f1(x2)}" y2="${f1(y2)}" stroke-width="${w}" stroke-opacity="${f2(o)}"${paint}/>` : "";
    const dimmed = (cx, cy, o) => o > 0.004 ? `<circle cx="${f1(cx)}" cy="${f1(cy)}" r="${g.radius}" fill="${grey}" stroke="none" fill-opacity="${f2(o * m.line_opacity)}"/>` : "";
    const draw = (t) => {
      const { samples, beam, trace, lines } = vergeAt(t), parts = [], circles = [];
      if (trace) {
        const [a, b] = trace, level = a < m.riser ? g.floor : g.top;
        const rises = a < m.riser && b > m.riser ? `H${f1(x(m.riser))}V${f1(g.top)}` : "";
        parts.push(`<path d="M${f1(x(a))},${f1(level)}${rises}H${f1(x(b))}" stroke-width="${g.line}" stroke-opacity="${m.line_opacity}"/>`);
      }
      if (lines > 0) {
        const [start, finish] = m.floor_run, stem = (cx, cy, o, paint) => line(cx, g.floor, cx, g.floor + (cy - g.floor) * lines, g.stem, o * lines, paint);
        parts.push(line(x(start), g.floor, x(start + (finish - start) * lines), g.floor, g.line, m.line_opacity));
        samples.forEach(({ now: [y, lit, dim], step, was }, i) => {
          const cx = g.x[i];
          if (was) parts.push(stem(cx, was[0], was[1] * m.line_opacity));
          // The riser's stem on the step is the step's edge, in lume as the sample is lit.
          if (step && i === riser) parts.push(stem(cx, y, lit, ' stroke="currentColor"'), stem(cx, y, dim * m.line_opacity));
          else parts.push(stem(cx, y, (lit + dim) * m.line_opacity));
        });
      }
      samples.forEach(({ now: [y, , dim], was }, i) => {
        if (was) circles.push(dimmed(g.x[i], was[0], was[1]));
        circles.push(dimmed(g.x[i], y, dim));
      });
      under.innerHTML = parts.join("") + circles.join("");
      over.innerHTML = beam != null ? line(x(beam), g.beam[0], x(beam), g.beam[1], g.beam_width, m.beam_opacity) : "";
      dots.forEach((d, i) => { const [y, lit] = samples[i].now; d.setAttribute("cy", f1(y)); d.setAttribute("fill-opacity", f2(lit)); });
    };
    const t0 = performance.now();
    const step = (now) => {
      const t = Math.max(0, now - t0) / 1000;
      if (t < m.end) { draw(t); requestAnimationFrame(step); return; }
      under.remove(); over.remove();
      // Back to the mark's own markup, as design/marks/verge.svg writes it.
      dots.forEach((d, i) => { d.setAttribute("cy", String(g.v[i])); d.removeAttribute("fill-opacity"); });
      delete plate.dataset.playing;
    };
    step(t0);
  }

  // tickwave's: still while Verge's samples dim, then the time axis zooms in on one rising edge,
  // which glides to the middle, until it is the only edge in view, the project's step; it holds as
  // long as Verge holds its own, then zooms
  // back out. A tick at each third fades in as the ticks spread, so the ruler stays as dense as at
  // rest. The numbers come from design/tokens.toml's [motion.tickwave-mark], Verge's times and
  // design/marks/tickwave.svg, built into design/out/tickwave-mark.json (#tickwave-mark). Each frame
  // is the mark's own rects placed for the zoom, the wave cut to its run; the plate gets its own
  // markup back at the end.
  function tickwaveAt(t) {
    const m = TW, w = m.wave, k = m.ticks, p = m.ease, f = (v) => v.toFixed(2), parts = [];
    const inout = (x) => (x = clamp01(x)) < 0.5 ? 2 ** (p - 1) * x ** p : 1 - (2 - 2 * x) ** p / 2;
    const since = t - m.wait, a = since < m.zoom ? inout(since / m.zoom) : since < m.zoom + m.hold ? 1 : inout((m.end - t) / m.zoom);
    const scale = m.scale ** a, at = m.edge + (32 - m.edge) * a, c = m.edge - (at - 32) / scale;
    const X = (u) => 32 + (u - c) * scale, half = w.width / 2;
    const rect = (x0, x1, y, h) => {
      x0 = Math.max(x0, w.run[0]); x1 = Math.min(x1, w.run[1]);
      if (x1 > x0) parts.push(`<rect x="${f(x0)}" y="${f(y)}" width="${f(x1 - x0)}" height="${f(h)}"/>`);
    };
    let from = -Infinity, low = w.low_first;
    for (const e of w.edges) {
      const x = X(e);
      rect(from, x + half, low ? w.lo : w.hi, w.width);
      rect(x - half, x + half, w.hi, w.lo + w.width - w.hi);
      from = x - half; low = !low;
    }
    rect(from, Infinity, low ? w.lo : w.hi, w.width);
    const tick = (x, [y, h], line, op) => parts.push(`<rect x="${f(x - k.width / 2)}" y="${f(y)}" width="${f(k.width)}" height="${f(h)}"`
      + `${line ? ' class="line"' : ""}${op < 1 ? ` style="opacity:${f(op)}"` : ""}/>`);
    const third = clamp01((scale - m.thirds[0]) / (m.thirds[1] - m.thirds[0])), reach = (32 + k.width) / scale;
    for (let j = Math.floor((c - reach - k.first) / k.step); k.first + j * k.step <= c + reach; j++) {
      const u = k.first + j * k.step, edge = w.edges.includes(u);
      tick(X(u), edge ? k.major : k.minor, !edge, 1);
      if (third > 0) for (const d of [1, 2]) tick(X(u + d * k.step / 3), k.minor, true, third);
    }
    return parts.join("");
  }

  function playTickwave(plate) {
    const svg = plate.querySelector("svg");
    if (reduced.matches || plate.dataset.playing) return;
    plate.dataset.playing = "1";
    const rest = svg.innerHTML, t0 = performance.now();
    const step = (now) => {
      const t = Math.max(0, now - t0) / 1000;
      // Still on its own markup while Verge's samples dim, then the zoom.
      if (t < TW.end) { if (t >= TW.wait) svg.innerHTML = tickwaveAt(t); requestAnimationFrame(step); return; }
      // Back to the mark's own markup, as design/marks/tickwave.svg writes it.
      svg.innerHTML = rest;
      delete plate.dataset.playing;
    };
    step(t0);
  }

  const players = { verge: MARK && playVerge, tickwave: TW && playTickwave };
  const header = document.querySelector("header")?.offsetHeight ?? 0;
  for (const plate of plates) {
    const play = players[plate.dataset.mark];
    if (!play) continue;
    plate.addEventListener("pointerenter", () => play(plate));
    // The header can cover the plate, so it plays once wholly in view below it, after any mark the
    // page plays first (window.marksWait, the landing's hero).
    new IntersectionObserver((seen, io) => {
      if (!seen.some((s) => s.isIntersecting)) return;
      io.disconnect();
      Promise.resolve(window.marksWait).then(() => setTimeout(() => play(plate), 200));
    }, { threshold: 1, rootMargin: `-${header}px 0px 0px 0px` }).observe(plate);
  }
})();
