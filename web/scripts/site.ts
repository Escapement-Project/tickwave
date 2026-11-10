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
for (const f of ["tokens.css", "base.css", "doc.css", "face.js", "header.js", "copy.js", "doc.js", "favicon-tickwave.ico", "site.css"]) copyFileSync(join(site, f), join(out, f));
cpSync(join(site, "fonts"), join(out, "fonts"), { recursive: true });

// The demo's and the viewer's examples: committed fixtures, and the SWD annotations made from one.
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

const md = new MarkdownIt({ html: false, linkify: false, typographer: false }).use(anchor, {
  slugify,
  permalink: anchor.permalink.linkInsideHeader({ symbol: "#", placement: "after", ariaHidden: true, class: "anchor" }),
});
// A wide table scrolls inside its own box, so the page never scrolls sideways.
md.renderer.rules.table_open = () => '<div class="table"><table>\n';
md.renderer.rules.table_close = () => "</table></div>\n";

/** tickwave's group in the header: its main page under its name, then its other pages. */
const GROUP: { href: string; name: string; page: string }[] = [
  { href: "index.html", name: "tickwave", page: "tickwave" },
  { href: "convert.html", name: "Convert", page: "tickwave/convert" },
  { href: "demo.html", name: "Demo", page: "tickwave/demo" },
  { href: "docs/index.html", name: "Docs", page: "tickwave/docs" },
];

// The elements: web/README.md's own section, its headings one level up. One source, no copy.
const readme = readFileSync(join(web, "README.md"), "utf8");
const from = readme.indexOf("\n## The elements\n");
if (from < 0) throw new Error("web/README.md has no \"The elements\" section");
const next = readme.indexOf("\n## ", from + 1);
const elements = readme.slice(from + 1, next < 0 ? undefined : next).replace(/^##(#*) /gm, "#$1 ");

/** tickwave's docs, in the rail on each and in the docs' index, each with what it covers. */
const DOCS: { href: string; name: string; markdown: string; description: string }[] = [
  { href: "docs/elements.html", name: "Elements", markdown: elements, description: "tickwave's elements: attributes, methods, events, parts and custom properties." },
  { href: "docs/charts.html", name: "Charts", markdown: readFileSync(join(repo, "CHARTS.md"), "utf8"), description: "tickwave's charts: measurements drawn against a setting, and their data." },
  { href: "docs/format.html", name: "Format", markdown: readFileSync(join(repo, "FORMAT.md"), "utf8"), description: "The tickwave recording format." },
  { href: "docs/annotations.html", name: "Sidecar", markdown: readFileSync(join(repo, "ANNOTATIONS.md"), "utf8"), description: "tickwave's sidecar annotations." },
  {
    href: "docs/converter.html",
    name: "Converter",
    markdown: readFileSync(join(site, "docs/converter.md"), "utf8"),
    description: "Building tickwave's converter, and converting, describing, verifying and bundling recordings.",
  },
];

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
      case "content":
        return opts.content;
      default:
        throw new Error(`layout.html: no value for {{${key}}}`);
    }
  });
  writeFileSync(join(out, path), html);
}

// HTML pages say their title, description and kind in comments at the top.
for (const name of ["index.html", "convert.html", "demo.html"]) {
  const text = readFileSync(join(site, "pages", name), "utf8");
  const meta = (key: string) => {
    const m = new RegExp(`<!-- ${key}: (.*?) -->`).exec(text);
    if (!m) throw new Error(`site/pages/${name}: no ${key}`);
    return m[1];
  };
  page(name, { title: meta("title"), description: meta("description"), kind: meta("kind"), content: text.replace(/^<!-- \w+: .*? -->\n/gm, "") });
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

/**
 * A Markdown document as a docs page; its first heading is the page's title. The page is the
 * project's doc frame (doc.css and doc.js, the markup in the site's design/README.md): every doc of
 * tickwave's in a rail, this one's sections under it, and on a narrow screen a bar that names the
 * section the reader is in.
 */
function doc(path: string, markdown: string, description: string): void {
  const title = /^# (.*)$/m.exec(markdown)?.[1].replace(/`/g, "") ?? path;
  const body = md.render(markdown);
  const docs = DOCS.map(({ href, name }) => {
      const file = href.slice("docs/".length);
      return href === path ? `<li><a href="${file}" aria-current="page">${name}</a>${contents(body)}</li>` : `<li><a href="${file}">${name}</a></li>`;
  }).join("\n");
  const content = `<div class="doc-wrap">
<button class="doc-bar" type="button" aria-expanded="false" aria-controls="doc-rail"><span class="label">Contents</span><span class="where">${esc(title)}</span></button>
<div class="doc-rail" id="doc-rail"><div class="doc-list"><nav class="doc-nav" aria-label="tickwave's docs"><ul>
${docs}
</ul></nav></div></div>
<article class="doc">${body}</article>
</div>`;
  page(path, { title: `tickwave: ${title}`, description, kind: "doc", content });
}

for (const d of DOCS) doc(d.href, d.markdown, d.description);

// The docs' index, where the header's Docs leads: each doc with what it covers, as Verge's lists its own.
page("docs/index.html", {
  title: "tickwave: Docs",
  description: "tickwave's docs: its elements, charts, recording format, sidecar and converter.",
  kind: "docs-index",
  content: `<h1>tickwave's docs</h1>
<ul class="docs-list">
${DOCS.map(({ href, name, description }) => `<li><a href="${href.slice("docs/".length)}">${name}</a><p>${esc(description)}</p></li>`).join("\n")}
</ul>`,
});

console.log(`site: ${out}`);
