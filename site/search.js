// A product's docs search, shared by the project's docs (design/README.md has the markup and the
// index's format). The sections that mention every word typed take the contents' place while there
// is a query, grouped by doc. A result links to its heading with the query, so the page it opens
// shows the same results, that one marked, and the words marked in the text. The index, at the
// field's data-index, is written by each product's build and loads the first time the field is
// used.
(() => {
  const find = document.querySelector(".find");
  const found = document.getElementById("found");
  if (!find || !found) return;
  const field = find.querySelector("input"), clear = find.querySelector(".clear"), count = find.querySelector(".count");
  const holder = find.closest(".doc-rail, .docs-index"), list = holder.querySelector(".doc-list");
  const bar = document.querySelector(".doc-bar"), label = bar && bar.querySelector(".label");
  const article = document.querySelector("article.doc");
  const indexUrl = new URL(find.dataset.index, location.href);
  const SHOWN = 5; // a doc's sections shown before its "more"

  // The index, loaded once. A failed load is forgotten, so the next word typed tries again.
  let index = null, loading = null;
  const load = () => loading ||= fetch(indexUrl, { cache: "no-cache" })
    .then((r) => { if (!r.ok) throw new Error(`${r.status}`); return r.json(); })
    .then(({ docs, sections }) => {
      const d = docs.map(([title, path]) => ({ title, low: title.toLowerCase(), url: new URL(path, indexUrl) }));
      index = sections.map(([doc, id, heading, text], n) => ({ n, doc: d[doc], id, heading, text, hl: heading.toLowerCase(), tl: text.toLowerCase() }));
      return index;
    })
    .catch((e) => { loading = null; throw e; });

  // A word typed matches where a word in the text starts, so "capture" finds "captures", and a
  // plural typed finds its singular. A hyphen or a slash parts words as a space does, so
  // "under-range" finds "under range" too. Words under two characters are left out.
  const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const stem = (w) => (w.length > 3 && w.endsWith("s") && !w.endsWith("ss") ? w.slice(0, -1) : w);
  const words = (q) => [...new Set(q.toLowerCase().split(/[\s\-\/]+/).filter((w) => w.length > 1 && /[\p{L}\p{N}]/u.test(w)))].slice(0, 8);
  const start = "(?<![\\p{L}\\p{N}])";
  const hits = (re, s) => { re.lastIndex = 0; let n = 0; while (re.exec(s) && n < 20) n++; return n; };
  // Every word typed, each to the end of the word it starts, for marking.
  const marker = (ws) => new RegExp(`${start}(?:${ws.map(stem).sort((a, b) => b.length - a.length).map(esc).join("|")})[\\p{L}\\p{N}_]*`, "giu");

  // A section scores by where each word is: its heading counts most, its doc's title a little, and
  // its text by how often, with diminishing weight. Every word must be somewhere, and at least one
  // in the section itself.
  const search = (q) => {
    const ws = words(q);
    if (!ws.length) return null;
    const res = ws.map((w) => new RegExp(start + esc(stem(w)), "gu"));
    const phrase = ws.length > 1 ? q.toLowerCase().trim().replace(/\s+/g, " ") : null;
    const out = [];
    for (const s of index) {
      let score = 0, own = false;
      for (const re of res) {
        const h = hits(re, s.hl), b = hits(re, s.tl), d = hits(re, s.doc.low) > 0;
        if (!h && !b && !d) { score = 0; break; }
        if (h || b) own = true;
        score += h * 12 + (d ? 3 : 0) + Math.log2(1 + b) * 2;
      }
      if (!own || !score) continue;
      if (phrase && (s.tl.includes(phrase) || s.hl.includes(phrase))) score += 6;
      out.push({ s, score });
    }
    // By doc, the doc with the best section first; within a doc, its best sections in page order.
    const docs = new Map();
    for (const r of out) {
      const g = docs.get(r.s.doc) || { doc: r.s.doc, best: 0, all: [] };
      g.best = Math.max(g.best, r.score);
      g.all.push(r);
      docs.set(r.s.doc, g);
    }
    const groups = [...docs.values()].sort((a, b) => b.best - a.best);
    for (const g of groups) {
      g.top = new Set([...g.all].sort((a, b) => b.score - a.score).slice(0, SHOWN).map((r) => r.s));
      g.all.sort((a, b) => a.s.n - b.s.n);
    }
    return { ws, groups, sections: out.length };
  };

  // Text with every match in a mark.
  const marked = (text, re) => {
    const f = document.createDocumentFragment();
    let at = 0;
    re.lastIndex = 0;
    for (let m; (m = re.exec(text)); ) {
      if (m.index > at) f.append(text.slice(at, m.index));
      const mk = document.createElement("mark");
      mk.textContent = m[0];
      f.append(mk);
      at = m.index + m[0].length;
    }
    if (at < text.length) f.append(text.slice(at));
    return f;
  };
  // A line of the section's text around its first match, cut at words.
  const snippet = (text, re) => {
    re.lastIndex = 0;
    const m = re.exec(text);
    let a = 0;
    if (m && m.index > 60) a = text.indexOf(" ", m.index - 50) + 1;
    let b = Math.min(text.length, a + 160);
    if (b < text.length) b = text.lastIndexOf(" ", b);
    return (a > 0 ? "… " : "") + text.slice(a, b) + (b < text.length ? " …" : "");
  };

  const here = (s) => s.doc.url.pathname === location.pathname;
  const hrefOf = (s, q) => {
    const u = new URL(s.doc.url);
    u.searchParams.set("q", q);
    u.hash = s.id;
    return u.href;
  };

  // The results, one group a doc. The option on the section the page shows is the current one, and
  // the pulse marks the option chosen with the arrows, or else the current one.
  let last = null, active = null;
  const open = new Set();
  const pulse = document.createElement("span");
  pulse.className = "pulse";
  pulse.setAttribute("aria-hidden", "true");
  const render = (q, r) => {
    const re = marker(r.ws);
    const id = decodeURIComponent(location.hash.slice(1));
    const groups = r.groups.map((g, gi) => {
      const box = document.createElement("div");
      box.setAttribute("role", "group");
      box.setAttribute("aria-labelledby", `found-doc-${gi}`);
      const name = document.createElement("p");
      name.className = "doc-name";
      name.id = `found-doc-${gi}`;
      name.textContent = g.doc.title;
      box.append(name);
      const all = open.has(g.doc);
      for (const { s } of g.all) {
        if (!all && !g.top.has(s)) continue;
        const o = document.createElement("a");
        o.setAttribute("role", "option");
        o.setAttribute("aria-selected", "false");
        o.tabIndex = -1;
        o.id = `found-${s.n}`;
        o.href = hrefOf(s, q);
        if (here(s) && s.id === id) o.classList.add("current");
        if (s.id) {
          const h = document.createElement("span");
          h.className = "h";
          h.append(marked(s.heading, re));
          o.append(h);
        }
        const t = document.createElement("span");
        t.className = "snip";
        t.append(marked(snippet(s.text, re), re));
        o.append(t);
        box.append(o);
      }
      const rest = g.all.length - g.top.size;
      if (!all && rest > 0) {
        const more = document.createElement("div");
        more.setAttribute("role", "option");
        more.setAttribute("aria-selected", "false");
        more.className = "more";
        more.id = `found-more-${gi}`;
        more.textContent = `${rest} more in this doc`;
        more.addEventListener("click", () => { open.add(g.doc); show(); });
        box.append(more);
      }
      return box;
    });
    found.replaceChildren(...groups, pulse);
    const n = r.sections, d = r.groups.length;
    count.textContent = n
      ? `${n} section${n === 1 ? "" : "s"} in ${d} doc${d === 1 ? "" : "s"}`
      : `No section mentions ${r.ws.length > 1 ? "all of " : ""}“${q}”.`;
    choose(null);
  };

  const options = () => [...found.querySelectorAll('[role="option"]')];
  const place = () => {
    const o = active || found.querySelector(".current");
    pulse.classList.toggle("on", Boolean(o));
    if (o) {
      pulse.style.top = `${o.offsetTop + 4}px`;
      pulse.style.height = `${o.offsetHeight - 8}px`;
    }
  };
  const choose = (o) => {
    if (active) active.setAttribute("aria-selected", "false");
    active = o;
    if (o) {
      o.setAttribute("aria-selected", "true");
      field.setAttribute("aria-activedescendant", o.id);
      o.scrollIntoView({ block: "nearest" });
    } else field.removeAttribute("aria-activedescendant");
    place();
  };

  // The words marked in the page's own text, after following a result to it, for the query they
  // came from; another query typed takes them away.
  let markedFor = null;
  const unmark = () => {
    markedFor = null;
    if (!article) return;
    for (const m of article.querySelectorAll("mark.hit")) {
      const p = m.parentNode;
      m.replaceWith(...m.childNodes);
      p.normalize();
    }
  };
  const markText = (q, ws) => {
    if (!article) return;
    unmark();
    markedFor = q;
    const re = marker(ws);
    const skip = "svg, .anchor, .doc-source, button, mark";
    const walk = document.createTreeWalker(article, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) => (n.parentElement.closest(skip) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    const nodes = [];
    while (walk.nextNode()) nodes.push(walk.currentNode);
    for (const n of nodes) {
      re.lastIndex = 0;
      if (!re.test(n.data)) continue;
      const f = marked(n.data, re);
      for (const m of f.querySelectorAll("mark")) m.className = "hit";
      n.replaceWith(f);
    }
  };

  // The results in the contents' place, or the contents back.
  const showing = (on) => {
    found.hidden = !on;
    holder.classList.toggle("searching", on);
    field.setAttribute("aria-expanded", String(on));
    if (label) label.textContent = on ? "Results" : "Contents";
  };
  const show = () => {
    const q = field.value.trim();
    clear.hidden = !field.value;
    find.classList.toggle("typed", Boolean(field.value));
    if (!q) {
      last = null;
      open.clear();
      showing(false);
      count.textContent = "";
      unmark();
      return;
    }
    if (!index) {
      count.textContent = "Loading the docs' index…";
      load().then(show, () => {
        if (field.value.trim()) count.textContent = "The search index didn't load. Type again to retry.";
      });
      return;
    }
    if (q !== last) open.clear();
    if (q !== markedFor) unmark();
    last = q;
    const r = search(q);
    if (!r) {
      showing(false);
      count.textContent = "Type two or more letters.";
      return;
    }
    render(q, r);
    showing(true);
    place();
  };

  // The page moves to the result's first word marked in its section, the one its line shows; to
  // the section's heading instead where the heading at the top of the window shows that word too.
  const reveal = (id) => {
    if (!article) return;
    const h = id ? document.getElementById(id) : null;
    let after = !h, word = null;
    for (const el of article.querySelectorAll("mark.hit, h2[id], h3[id]")) {
      if (el === h) { after = true; continue; }
      if (!after || (h && h.contains(el))) continue;
      if (el.tagName === "MARK") word = el;
      break;
    }
    if (word && !(h && word.getBoundingClientRect().bottom - h.getBoundingClientRect().top < innerHeight * 0.6)) {
      scrollTo(0, scrollY + word.getBoundingClientRect().top - innerHeight / 3);
    } else if (h) h.scrollIntoView();
    else scrollTo(0, 0);
  };

  // The results' place, kept across a result's page load for the same query, so the list stays
  // where the reader left it: its scroll, and the docs opened past their first few.
  const KEY = "docs-search";
  const keep = () => {
    try {
      sessionStorage.setItem(KEY, JSON.stringify({ q: field.value.trim(), top: list ? list.scrollTop : null, open: [...open].map((d) => d.title) }));
    } catch { /* not kept: the list starts at its top */ }
  };
  const kept = () => {
    try {
      const k = JSON.parse(sessionStorage.getItem(KEY));
      sessionStorage.removeItem(KEY);
      return k;
    } catch { return null; }
  };

  // Following a result to the page already shown: no load, the page moves to the result.
  const follow = (o) => {
    const u = new URL(o.href);
    if (u.pathname !== location.pathname) { keep(); location.href = u.href; return; }
    history.pushState(null, "", u.href);
    for (const c of found.querySelectorAll(".current")) c.classList.remove("current");
    o.classList.add("current");
    choose(null);
    const q = field.value.trim(), r = search(q);
    if (r) markText(q, r.ws);
    reveal(decodeURIComponent(u.hash.slice(1)));
  };

  field.addEventListener("input", show);
  field.addEventListener("focus", () => load().catch(() => {}), { once: true });
  field.addEventListener("keydown", (e) => {
    const os = options();
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (!os.length || found.hidden) return;
      e.preventDefault();
      const i = os.indexOf(active), step = e.key === "ArrowDown" ? 1 : -1;
      choose(os[i < 0 ? (step > 0 ? 0 : os.length - 1) : (i + step + os.length) % os.length]);
    } else if (e.key === "Enter") {
      const o = active || (found.hidden ? null : os[0]);
      if (!o) return;
      e.preventDefault();
      if (o.classList.contains("more")) {
        // The doc's sections, all of them now, the first chosen.
        const group = o.parentElement.getAttribute("aria-labelledby");
        o.click();
        choose(found.querySelector(`[aria-labelledby="${group}"] a[role="option"]`));
      } else o.click();
    } else if (e.key === "Escape") {
      // The first Escape clears the query; the next leaves the field (and closes the sheet).
      if (field.value) {
        e.preventDefault();
        e.stopPropagation();
        field.value = "";
        show();
      } else field.blur();
    }
  });
  found.addEventListener("click", (e) => {
    const o = e.target.closest('a[role="option"]');
    if (!o || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
    e.preventDefault();
    follow(o);
  });
  clear.addEventListener("click", () => {
    field.value = "";
    show();
    field.focus();
  });
  // The pulse follows its option wherever the list is laid out again: shown, in a sheet opened, wider.
  new ResizeObserver(place).observe(found);

  // "/" goes to the field from anywhere on the page; where the field is in the closed sheet, the
  // sheet opens first.
  document.addEventListener("keydown", (e) => {
    if (e.key !== "/" || e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
    if (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable]")) return;
    e.preventDefault();
    if (!field.offsetParent && bar && bar.getAttribute("aria-expanded") === "false") bar.click();
    field.focus();
    field.select();
  });

  // Arriving from a result: the words marked in the text and the page at the result's word, then
  // the same results, where the list was left. The marks need only the query, so the page moves
  // before the index loads, once the page has loaded and the browser has gone to the heading.
  const q = (new URLSearchParams(location.search).get("q") || "").trim();
  if (q) {
    const k = kept();
    field.value = q;
    const ws = words(q);
    if (ws.length) {
      markText(q, ws);
      const go = () => requestAnimationFrame(() => reveal(decodeURIComponent(location.hash.slice(1))));
      if (document.readyState === "complete") go();
      else addEventListener("load", go, { once: true });
    }
    show();
    load().then(() => {
      if (field.value.trim() !== q) return;
      const same = k && k.q === q;
      if (same) {
        const docs = new Map(index.map((s) => [s.doc.title, s.doc]));
        for (const t of k.open || []) if (docs.has(t)) open.add(docs.get(t));
        show();
      }
      // Arrived from elsewhere, the list opens on the result the page shows.
      if (!list) return;
      const c = found.querySelector(".current");
      if (same && typeof k.top === "number") list.scrollTop = k.top;
      else if (c && c.offsetHeight) list.scrollTop += c.getBoundingClientRect().top - list.getBoundingClientRect().top - list.clientHeight / 3;
    }, () => {});
  }
})();
