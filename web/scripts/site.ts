// Builds tickwave's site into dist/site/, from site/ at the repository's root. Run from web/ after
// build.ts: node scripts/site.ts
//
// Every URL in the site is relative, as it is published under a subpath (/tickwave/), except the
// shared header's: they lead to the project's other pages, at the published site.

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

// The elements, as build.ts made them.
for (const f of ["tickwave.js", "tickwave-worker.js", "tickwave.wasm"]) copyFileSync(join(web, "dist", f), join(out, f));

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
for (const f of ["tokens.css", "base.css", "face.js", "header.js", "favicon.ico", "site.css"]) copyFileSync(join(site, f), join(out, f));
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

const NAV: [string, string][] = [
  ["index.html", "Viewer"],
  ["demo.html", "Demo"],
  ["docs/elements.html", "Elements"],
  ["docs/charts.html", "Charts"],
  ["docs/format.html", "Format"],
  ["docs/annotations.html", "Sidecar"],
  ["docs/converter.html", "Converter"],
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

// The shared header, with the project's mark, its places at the published site, and tickwave marked
// as the place shown; the shared footer, with tickwave's licence.
const mark = readFileSync(join(site, "dial.svg"), "utf8").trim().replace("<svg ", '<svg aria-hidden="true" fill="currentColor" ');
const header = shaped("header.html", [["{{mark}}", 1], ["{{base}}", 0], ['data-page="tickwave"', 1]])
  .replace("{{mark}}", () => mark)
  .replaceAll("{{base}}", PUBLISHED)
  .replace('data-page="tickwave"', 'data-page="tickwave" aria-current="page"');
const footer = shaped("footer.html", [["</div></footer>", 1]]).replace(
  "</div></footer>",
  "<span class=\"licence\">tickwave is free software under the MIT or Apache-2.0 licence.</span></div></footer>",
);
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);

function page(path: string, opts: { title: string; description: string; kind: string; content: string }): void {
  const root = path.includes("/") ? "../" : "";
  const nav = NAV.map(([href, name]) => `<a href="${root}${href}"${href === path ? ' aria-current="page"' : ""}>${name}</a>`).join("\n");
  const html = layout.replace(/\{\{(\w+)\}\}/g, (_, key: string) => {
    switch (key) {
      case "title":
        return esc(opts.title);
      case "description":
        return esc(opts.description);
      case "root":
        return root;
      case "nav":
        return nav;
      case "header":
        return header;
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
for (const name of ["index.html", "demo.html"]) {
  const text = readFileSync(join(site, "pages", name), "utf8");
  const meta = (key: string) => {
    const m = new RegExp(`<!-- ${key}: (.*?) -->`).exec(text);
    if (!m) throw new Error(`site/pages/${name}: no ${key}`);
    return m[1];
  };
  page(name, { title: meta("title"), description: meta("description"), kind: meta("kind"), content: text.replace(/^<!-- \w+: .*? -->\n/gm, "") });
}

/** A Markdown document as a docs page; its first heading is the page's title. */
function doc(path: string, markdown: string, description: string): void {
  const title = /^# (.*)$/m.exec(markdown)?.[1].replace(/`/g, "") ?? path;
  const toc = [...markdown.matchAll(/^## (.*)$/gm)].map((m) => m[1]);
  const body = md.render(markdown);
  // A contents list after the first heading, for a document with several sections.
  const contents =
    toc.length > 2
      ? `<nav class="toc" aria-label="Contents"><p>On this page: ${toc
          .map((t) => `<a href="#${slugify(t.replace(/`/g, ""))}">${md.renderInline(t)}</a>`)
          .join(" · ")}</p></nav>`
      : "";
  const content = `<article class="doc">${body.replace(/(<\/h1>\n)/, `$1${contents}\n`)}</article>`;
  page(path, { title: `tickwave: ${title}`, description, kind: "doc", content });
}

// The elements: web/README.md's own section, its headings one level up. One source, no copy.
const readme = readFileSync(join(web, "README.md"), "utf8");
const from = readme.indexOf("\n## The elements\n");
if (from < 0) throw new Error("web/README.md has no \"The elements\" section");
const next = readme.indexOf("\n## ", from + 1);
const elements = readme.slice(from + 1, next < 0 ? undefined : next).replace(/^##(#*) /gm, "#$1 ");
doc("docs/elements.html", elements, "tickwave's elements: attributes, methods, events, parts and custom properties.");
doc("docs/charts.html", readFileSync(join(repo, "CHARTS.md"), "utf8"), "tickwave's charts: measurements drawn against a setting, and their data.");
doc("docs/format.html", readFileSync(join(repo, "FORMAT.md"), "utf8"), "The tickwave recording format.");
doc("docs/annotations.html", readFileSync(join(repo, "ANNOTATIONS.md"), "utf8"), "tickwave's sidecar annotations.");
doc("docs/converter.html", readFileSync(join(site, "docs/converter.md"), "utf8"), "Building tickwave's converter, and converting, describing, verifying and bundling recordings.");

console.log(`site: ${out}`);
