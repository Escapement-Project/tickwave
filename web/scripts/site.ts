// Builds tickwave's site into dist/site/, from site/ at the repository's root. Run from web/ after
// build.ts: node scripts/site.ts
//
// Every URL in the site is relative, as it is published under a subpath (/tickwave/), except the
// shared header's other places: they lead to the project's other pages, at the published site.

import * as esbuild from "esbuild";
import MarkdownIt from "markdown-it";
import anchor from "markdown-it-anchor";
import { execFileSync } from "node:child_process";
import { copyFileSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repo = resolve(web, "..");
const site = join(repo, "site");
const out = join(web, "dist/site");

rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, "docs"), { recursive: true });
mkdirSync(join(out, "fixtures"), { recursive: true });

// The elements, and the converter page's worker and module, as build.ts made them.
for (const f of ["tickwave.js", "tickwave-worker.js", "tickwave.wasm", "tickwave-convert-worker.js", "tickwave-convert.wasm"]) copyFileSync(join(web, "dist", f), join(out, f));

// The page script, which imports the elements from the tickwave.js beside it.
await esbuild.build({
  entryPoints: [join(site, "site.ts")],
  outfile: join(out, "site.js"),
  bundle: true,
  format: "esm",
  target: "es2022",
  minify: true,
  logLevel: "warning",
  plugins: [
    {
      name: "tickwave-beside",
      setup(b) {
        b.onResolve({ filter: /^tickwave$/ }, () => ({ path: "./tickwave.js", external: true }));
      },
    },
  ],
});

// The design system's files, as copied into site/, and the site's own styles.
for (const f of ["tokens.css", "base.css", "doc.css", "face.js", "header.js", "copy.js", "doc.js", "search.js", "mark.js", "favicon-tickwave.ico", "site.css"]) copyFileSync(join(site, f), join(out, f));
cpSync(join(site, "fonts"), join(out, "fonts"), { recursive: true });

// The overview's and the viewer's examples: committed fixtures, and the SWD annotations made from one.
for (const f of ["identify-swd.tickwave", "identify-swd.annotations.toml", "swd50-cut.tickwave"]) {
  copyFileSync(join(web, "fixtures", f), join(out, "fixtures", f));
}
execFileSync(process.execPath, ["scripts/swd-annotations.ts", "fixtures/identify-swd.tickwave", "--out", join(out, "fixtures/identify-swd.swd.jsonl")], { cwd: web });

/** A heading's id: its text, lower case, letters, digits and hyphens. The contents list uses it too. */
const slugify = (text: string) =>
  text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s-]/gu, "")
    .replace(/\s+/g, "-");

// Every heading has an id; h2 to h4 end with the link to themselves (the site's design/README.md).
const selfLink = anchor.permalink.linkInsideHeader({
  symbol: "#",
  placement: "after",
  space: false,
  class: "anchor",
  renderAttrs: () => ({ "aria-label": "Link to this section" }),
});
const md = new MarkdownIt({ html: false, linkify: false, typographer: false }).use(anchor, {
  slugify,
  permalink: (slug, opts, state, i) => {
    if (["h2", "h3", "h4"].includes(state.tokens[i].tag)) selfLink(slug, opts, state, i);
  },
});
type Token = ReturnType<typeof md.parse>[number];

/** The longest word of inline code kept on one line (the site's design/README.md, "A doc page"). */
const WHOLE = 22;
// Inline code keeps each word, a run between spaces, whole where doc.css's .word holds it; a longer
// word, a path, still breaks anywhere, so it fits a phone.
md.renderer.rules.code_inline = (tokens, i, _options, _env, self) => {
  const word = (w: string) => (w && [...w].length <= WHOLE ? `<span class="word">${md.utils.escapeHtml(w)}</span>` : md.utils.escapeHtml(w));
  return `<code${self.renderAttrs(tokens[i])}>${tokens[i].content.split(" ").map(word).join(" ")}</code>`;
};

/** tickwave's group in the header: its main page under its name, then its other pages. */
const GROUP: { href: string; name: string; page: string }[] = [
  { href: "index.html", name: "tickwave", page: "tickwave" },
  { href: "open.html", name: "Open", page: "tickwave/open" },
  { href: "docs/index.html", name: "Docs", page: "tickwave/docs" },
];

// The elements: web/README.md's own section, its headings one level up. One source, no copy.
const readme = readFileSync(join(web, "README.md"), "utf8");
const from = readme.indexOf("\n## The elements\n");
if (from < 0) throw new Error("web/README.md has no \"The elements\" section");
const next = readme.indexOf("\n## ", from + 1);
const elements = readme.slice(from + 1, next < 0 ? undefined : next).replace(/^##(#*) /gm, "#$1 ");

/** Where tickwave's sources are published; a doc's source line links its file there. */
const REPOSITORY = "https://github.com/Escapement-Project/tickwave";

/** tickwave's docs in groups, as Verge's are: on the docs' index, in the rail and on the overview. */
const GROUPS: { id: string; title: string; about: string }[] = [
  { id: "using-it", title: "Using it", about: "Reading a recording in the viewer, and converting a capture into one, in the browser or on the command line." },
  { id: "in-your-page", title: "In your page", about: "The elements that put a capture in a page's text, and the charts that draw measurements." },
  { id: "the-files", title: "The files", about: "The recording format, and the sidecar of annotations that sits beside a recording." },
];

/** tickwave's docs, group by group, each with what it covers and its source. */
const DOCS: { href: string; group: string; name: string; markdown: string; description: string; source: string }[] = [
  {
    href: "docs/viewer.html",
    group: "using-it",
    name: "Viewer",
    markdown: readFileSync(join(site, "docs/viewer.md"), "utf8"),
    description: "Opening a recording, moving and zooming, reading its channels, cursors and annotations.",
    source: "site/docs/viewer.md",
  },
  {
    href: "docs/converter.html",
    group: "using-it",
    name: "Converter",
    markdown: readFileSync(join(site, "docs/converter.md"), "utf8"),
    description: "Building tickwave's converter, and converting, describing, verifying and bundling recordings.",
    source: "site/docs/converter.md",
  },
  {
    href: "docs/elements.html",
    group: "in-your-page",
    name: "Elements",
    markdown: elements,
    description: "tickwave's elements: attributes, methods, events, parts and custom properties.",
    source: "web/README.md#the-elements",
  },
  {
    href: "docs/charts.html",
    group: "in-your-page",
    name: "Charts",
    markdown: readFileSync(join(repo, "CHARTS.md"), "utf8"),
    description: "tickwave's charts: measurements drawn against a setting, and their data.",
    source: "CHARTS.md",
  },
  { href: "docs/format.html", group: "the-files", name: "Format", markdown: readFileSync(join(repo, "FORMAT.md"), "utf8"), description: "The recording format: times in whole ticks, the header, the block index and the blocks.", source: "FORMAT.md" },
  {
    href: "docs/annotations.html",
    group: "the-files",
    name: "Sidecar",
    markdown: readFileSync(join(repo, "ANNOTATIONS.md"), "utf8"),
    description: "Annotations beside a recording: the hand-written file, a generator's, their fields and measures.",
    source: "ANNOTATIONS.md",
  },
];

/** A page's title: its name, tickwave's unless it is tickwave's main page, then the project's. */
const titled = (name: string | null) => [name, "tickwave", "Escapement Project"].filter(Boolean).join(" \u2013 ");

const layout = readFileSync(join(site, "layout.html"), "utf8");

/** The project's address, where the shared header's places are. */
const PUBLISHED = "https://escapement-project.github.io";

/** A copy's text, after checking it still has the shape this build fills in. */
function shaped(name: string, markers: [string, number][]): string {
  const text = readFileSync(join(site, name), "utf8").trim();
  for (const [marker, count] of markers) {
    const n = text.split(marker).length - 1;
    if (count ? n !== count : n === 0) throw new Error(`site/${name}: expected ${count || "some"} ${marker}; the site's copy changed shape`);
  }
  return text;
}

// The shared header, with the project's mark and its places at the published site; the shared
// footer, with tickwave's licence.
const mark = readFileSync(join(site, "dial.svg"), "utf8").trim().replace("<svg ", '<svg aria-hidden="true" fill="currentColor" ');
// tickwave's mark, on the overview's plate, and its motion, which the shared mark.js plays.
const plateMark = readFileSync(join(site, "tickwave.svg"), "utf8").trim().replace("<svg ", '<svg width="66" height="66" aria-hidden="true" fill="currentColor" ');
const motion = `<script id="tickwave-mark" type="application/json">${readFileSync(join(site, "tickwave-mark.json"), "utf8").trim()}</script>`;
const TICKWAVE = '<a href="{{base}}/tickwave/" data-page="tickwave">tickwave</a>';
const shared = shaped("header.html", [["{{mark}}", 1], ["{{base}}", 0], [TICKWAVE, 1]]).replace("{{mark}}", () => mark);

/**
 * The header on the page at `path`: tickwave's link gives way to its group (the site's
 * design/README.md), each entry marked where the reader is. The page's own entry is "page"; the
 * entry it sits inside, as Docs is for a doc, and tickwave's name off its main page, are "true".
 */
function header(path: string, root: string): string {
  const within = (href: string) => href.endsWith("/index.html") && path.startsWith(href.slice(0, -"index.html".length));
  const group = GROUP.map(({ href, name, page }, i) => {
    const at = href === path ? "page" : i === 0 || within(href) ? "true" : null;
    return `<a href="${root}${href}" data-page="${page}"${at ? ` aria-current="${at}"` : ""}>${name}</a>`;
  }).join("");
  return shared.replace(TICKWAVE, () => `<span class="product">${group}</span>`).replaceAll("{{base}}", PUBLISHED);
}

const footer = shaped("footer.html", [["</div></footer>", 1]]).replace(
  "</div></footer>",
  "<span class=\"licence\">tickwave is free software under the MIT or Apache-2.0 licence.</span></div></footer>",
);
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

function page(path: string, opts: { title: string; description: string; kind: string; content: string }): void {
  const root = path.includes("/") ? "../" : "";
  const html = layout.replace(/\{\{(\w+)\}\}/g, (_, key: string) => {
    switch (key) {
      case "title":
        return esc(opts.title);
      case "description":
        return esc(opts.description);
      case "root":
        return root;
      case "header":
        return header(path, root);
      case "footer":
        return footer;
      case "kind":
        return opts.kind;
      // The overview's head and sections set their own column, as the project's pages do, so its
      // sections' rules run the page's width.
      case "main":
        return opts.kind === "overview" ? "" : ' class="wrap"';
      case "content":
        return opts.content;
      default:
        throw new Error(`layout.html: no value for {{${key}}}`);
    }
  });
  writeFileSync(join(out, path), html);
}

// HTML pages say their description and kind in comments at the top. A page's title is its name in
// the header's group, and tickwave's main page is tickwave's.
for (const name of ["index.html", "open.html"]) {
  const text = readFileSync(join(site, "pages", name), "utf8");
  const meta = (key: string) => {
    const m = new RegExp(`<!-- ${key}: (.*?) -->`).exec(text);
    if (!m) throw new Error(`site/pages/${name}: no ${key}`);
    return m[1];
  };
  const title = titled(name === GROUP[0].href ? null : GROUP.find((g) => g.href === name)!.name);
  const content = text
    .replace(/^<!-- \w+: .*? -->\n/gm, "")
    .replace("<!-- mark -->", () => plateMark)
    .replace("<!-- motion -->", () => motion)
    .replace("<!-- docs -->", () => GROUPS.map((g) => `<div><dt><a href="docs/index.html#${g.id}">${g.title}</a></dt><dd>${esc(g.about)}</dd></div>`).join(""));
  page(name, { title, description: meta("description"), kind: meta("kind"), content });
}

/** A heading's text as the rail shows it: its HTML without the link to itself. */
const headingText = (inner: string) => inner.replace(/<a class="anchor"[^>]*>.*?<\/a>/g, "").trim();

/** The doc's sections and their subsections, from its rendered h2 and h3, as the rail's .toc. */
function contents(body: string): string {
  const sections: { id: string; text: string; subs: { id: string; text: string }[] }[] = [];
  for (const m of body.matchAll(/<h([23]) id="([^"]+)"[^>]*>([\s\S]*?)<\/h\1>/g)) {
    const entry = { id: m[2], text: headingText(m[3]) };
    if (m[1] === "2") sections.push({ ...entry, subs: [] });
    else sections.at(-1)?.subs.push(entry);
  }
  if (sections.length === 0) return "";
  const item = (e: { id: string; text: string }, inner = "") => `<li><a href="#${e.id}">${e.text}</a>${inner}</li>`;
  return `<ul class="toc">${sections.map((s) => item(s, s.subs.length ? `<ul>${s.subs.map((sub) => item(sub)).join("")}</ul>` : "")).join("")}</ul>`;
}

const squash = (parts: string[]) => parts.join("").replace(/\s+/g, " ").trim();

/**
 * A doc's sections for the docs' search, to the site's definition (design/README.md, "The search
 * index"): the text after the h1 and before the first h2 or h3 with an id, then each h2 or h3 with an
 * id and its text up to the next. A heading without an id, and h4 and deeper, stay in their section.
 * Text is plain, a heading's too, without the heading link, its whitespace collapsed and a space
 * between blocks. Read from the tokens the doc's article is rendered from, which hold no svg,
 * script, style or source line.
 */
function searchSections(tokens: Token[]): { id: string; heading: string | null; text: string }[] {
  const sections: { id: string; heading: string | null; parts: string[] }[] = [{ id: "", heading: null, parts: [] }];
  let into: "title" | "heading" | null = null;
  let heading: string[] = [];
  const put = (text: string) => (into ? heading : sections.at(-1)!.parts).push(text);
  for (const t of tokens) {
    const id = t.type === "heading_open" ? (t.attrGet("id")?.toString() ?? null) : null;
    if (t.type === "heading_open" && t.tag === "h1") {
      into = "title";
      heading = [];
    } else if (id && (t.tag === "h2" || t.tag === "h3")) {
      into = "heading";
      heading = [];
      sections.push({ id, heading: null, parts: [] });
    } else if (t.type === "heading_close" && into) {
      if (into === "heading") sections.at(-1)!.heading = squash(heading);
      into = null;
    } else if (t.type === "inline") {
      let link = false;
      for (const c of t.children ?? []) {
        if (c.type === "link_open" && c.attrGet("class") === "anchor") link = true;
        else if (link) link = c.type !== "link_close";
        else if (c.type === "text" || c.type === "code_inline") put(c.content);
        else if (c.type === "softbreak" || c.type === "hardbreak" || c.type === "image") put(" ");
      }
    } else if (t.type === "fence" || t.type === "code_block") put(` ${t.content} `);
    // Every other token opens or closes a block: a space keeps the words either side of it apart.
    else put(" ");
  }
  return sections.map((s) => ({ id: s.id, heading: s.heading, text: squash(s.parts) })).filter((s) => s.id || s.text);
}

// The docs' search field, as the site's templates/partials/find.html has it, with tickwave's index
// and label; and the list its results go in.
const FIND = `<search class="find" data-index="search.json">
<input id="find" type="search" role="combobox" aria-label="Search tickwave's docs" aria-autocomplete="list" aria-expanded="false" aria-controls="found" placeholder="Search the docs" autocomplete="off" spellcheck="false" enterkeyhint="search">
<kbd class="key" aria-hidden="true">/</kbd>
<button class="clear" type="button" aria-label="Clear the search" hidden></button>
<p class="count" role="status"></p>
</search>`;
const FOUND = `<div class="found" id="found" role="listbox" aria-label="Sections found" hidden></div>`;

/**
 * A Markdown document as a docs page, in the project's doc frame (doc.css, doc.js and search.js, the
 * markup in the site's design/README.md): the search and every doc of tickwave's in a rail, this
 * one's sections under it, its source under its text, and on a narrow screen a bar that names the
 * section the reader is in. Returns its sections for the search.
 */
function doc(d: (typeof DOCS)[number]): { id: string; heading: string | null; text: string }[] {
  const title = /^# (.*)$/m.exec(d.markdown)?.[1].replace(/`/g, "") ?? d.name;
  const env = {};
  const tokens = md.parse(d.markdown, env);
  const body = md.renderer.render(tokens, md.options, env);
  const docs = GROUPS.map((g) => {
    const items = DOCS.filter((o) => o.group === g.id).map(({ href, name }) => {
      const file = href.slice("docs/".length);
      return href === d.href ? `<li><a href="${file}" aria-current="page">${name}</a>${contents(body)}</li>` : `<li><a href="${file}">${name}</a></li>`;
    });
    return `<details class="group"${g.id === d.group ? " open" : ""}><summary>${g.title}</summary><ul>\n${items.join("\n")}\n</ul></details>`;
  }).join("\n");
  const content = `<div class="doc-wrap">
<button class="doc-bar" type="button" aria-expanded="false" aria-controls="doc-rail"><span class="label">Contents</span><span class="where">${esc(title)}</span></button>
<div class="doc-rail" id="doc-rail">
${FIND}
<div class="doc-list"><nav class="doc-nav" aria-label="tickwave's docs">
${docs}
</nav>
${FOUND}</div></div>
<article class="doc">${body}<p class="doc-source">From <a href="${REPOSITORY}/blob/HEAD/${d.source}">tickwave</a>, where changes to this page go.</p></article>
</div>`;
  page(d.href, { title: titled(d.name), description: d.description, kind: "doc-page", content });
  return searchSections(tokens);
}

// Each doc, and the search's index of their sections: the format and the definition of a section
// are the site's design/README.md's, "The search index".
const sections = DOCS.flatMap((d, n) => doc(d).map((s) => [n, s.id, s.heading ?? d.name, s.text]));
const docsIndex = DOCS.map((d) => [d.name, d.href.slice("docs/".length)]);
writeFileSync(join(out, "docs/search.json"), JSON.stringify({ docs: docsIndex, sections }));

// The docs' index, where the header's Docs leads: the search, and each doc with what it covers, in
// one list.
page("docs/index.html", {
  title: titled("Docs"),
  description: "tickwave's docs: its viewer and converter, its elements and charts, the recording format and the sidecar.",
  kind: "docs-index-page",
  content: `<div class="docs-index">
<div class="docs-head">
<h1>tickwave's docs</h1>
${FIND}
</div>
<p class="intro">Each doc is written in <a href="${REPOSITORY}">tickwave's repository</a>.</p>
${FOUND}
<div class="doc-groups">
${GROUPS.map(
  (g) => `<div id="${g.id}"><h2 class="group">${g.title}</h2><ul>
${DOCS.filter((d) => d.group === g.id)
  .map(({ href, name, description }) => `<li><a href="${href.slice("docs/".length)}">${name}</a><p>${esc(description)}</p></li>`)
  .join("\n")}
</ul></div>`,
).join("\n")}
</div>
</div>`,
});

console.log(`site: ${out}`);
