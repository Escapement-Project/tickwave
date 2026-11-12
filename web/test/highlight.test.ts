// The docs' code highlighting: which parts of a block take which of the project's code roles, what
// stays plain, and that every block keeps its text exactly.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { codeHighlighter } from "../scripts/highlight.ts";

const highlight = await codeHighlighter();

/** The text a reader sees and copies: the markup's tags gone and its escapes undone. */
const text = (html: string) => html.replace(/<[^>]*>/g, "").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");

/** Each span's text, by the property its colour comes from. */
function painted(html: string): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const m of html.matchAll(/<span style="color:var\((--[a-z0-9-]+)\)">([^<]*)<\/span>/g)) out.set(m[1], [...(out.get(m[1]) ?? []), text(m[2])]);
  return out;
}

test("output and a block with no language stay plain", () => {
  assert.equal(highlight("tickwave 0.1.0\n", "text"), "");
  assert.equal(highlight("anything\n", ""), "");
});

test("a block in a language the docs don't list stops the build, naming it", () => {
  assert.throws(() => highlight("fn main() {}\n", "rust"), /a code block in rust: add it/);
});

test("a TOML block: strings, numbers and operators take their roles, comments the secondary ink, keys the text's", () => {
  const html = highlight('label = "read" # the label\ncount = 12\n', "toml");
  const p = painted(html);
  assert.deepEqual(p.get("--code-string")?.join(""), '"read"');
  assert.deepEqual(p.get("--code-number"), ["12"]);
  assert.deepEqual(p.get("--code-operator"), ["=", "="]);
  assert.equal(p.get("--ink-2")?.join(""), "# the label");
  // A key is in no span: it takes the block's own ink.
  assert.match(html, /^label /);
});

test("HTML in a block is escaped, and its tag names and attributes take their roles", () => {
  const html = highlight('<script type="module" src="tickwave.js"></script>\n', "html");
  assert.doesNotMatch(html, /<script/);
  const p = painted(html);
  assert.deepEqual(p.get("--code-function"), ["script", "script"]);
  assert.deepEqual(p.get("--code-type"), ["type", "src"]);
});

test("every code block in the docs keeps its text exactly", () => {
  const sources = ["../site/docs/converter.md", "../ANNOTATIONS.md", "../CHARTS.md", "../README.md"];
  let blocks = 0;
  for (const f of sources) {
    for (const m of readFileSync(new URL(`../${f}`, import.meta.url), "utf8").matchAll(/^```(sh|toml|json|html)\n([\s\S]*?)^```/gm)) {
      assert.equal(text(highlight(m[2], m[1])), m[2], `a ${m[1]} block in ${f}`);
      blocks++;
    }
  }
  assert.ok(blocks >= 14, `${blocks} blocks`);
});
