// Paint roles (roles.ts): each field of a look from the page's tokens, then the recording's
// declaration, then the built-in look of the role's place among the declared ones; which role draws
// each stretch of a row; and where a dash's phase starts. The cases are in testdata/view/roles.json,
// which the Rust view core's tests read too.

import { test } from "node:test";
import assert from "node:assert/strict";
import type { Palette, RoleTokens } from "../src/palette.ts";
import { lengthAt, paintStretches, parseDash, parseWeight, roleLook, type Role } from "../src/roles.ts";
import { runTable } from "./tables.ts";

/** A palette whose page gives `host` for role `name`, with the trace and ink named as such. */
function palette(name: string, host: Partial<RoleTokens>, night: boolean): Palette {
  return {
    ink: "ink",
    ink2: "",
    hair: "",
    rule: "",
    page: "",
    wash: "",
    trace: "trace",
    night,
    fontText: "",
    fontFigures: "",
    lineWeight: 1.5,
    roleTokens: (n) => ({ colour: null, weight: null, dash: null, ...(n === name ? host : {}) }),
  };
}

runTable("roles", {
  look: (args) => roleLook(palette(args.name, args.host, args.night), args.roles as Role[], args.name),
  stretches: (args) =>
    paintStretches(
      (args.spans as [number, number, string][]).map(([from, to, role]) => ({ from, to, role })),
      (args.roles as string[]).map((name) => ({ name, label: name, colour: null, weight: 0, dash: null })),
      args.px0,
      args.px1,
    ).map((s) => [s.px0, s.px1, s.role]),
  length_at: (args) => lengthAt((args.paths as number[][]).map((points) => ({ points })), args.x),
});

test("a page's dash and weight tokens read only as the schema's bounds allow", () => {
  assert.deepEqual(parseDash("2 1"), [2, 1]);
  assert.deepEqual(parseDash(" 4, 1.5 1 1.5 "), [4, 1.5, 1, 1.5]);
  for (const bad of ["", "2", "2 1 1", "0 0", "1 65", "-1 1", "a b", "1 1 1 1 1 1 1 1 1 1"]) assert.equal(parseDash(bad), null, bad);
  assert.equal(parseWeight("1"), 1);
  assert.equal(parseWeight(" 2.5px "), 2.5);
  for (const bad of ["", "5", "-1", "thick", "1em"]) assert.equal(parseWeight(bad), null, bad);
});
