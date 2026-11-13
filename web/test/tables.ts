// The shared tables in testdata/view/: the Rust view core's tests read the same rows, so both
// implementations are held to one set of cases. testdata/view/README.md gives a row's layout and
// how a result is matched.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

export interface Row {
  readonly fn: string;
  // Whatever each function takes: each table's calls read them.
  readonly args: Record<string, any>;
  readonly expect: unknown;
  readonly close?: number;
  readonly view?: string;
  readonly note?: string;
  readonly then?: readonly Row[];
}

interface Group {
  readonly name: string;
  readonly rows: readonly Row[];
}

/** The numbers JSON has no literal for, as the tables write them. */
const SPECIAL: Record<string, number> = { Infinity: Infinity, "-Infinity": -Infinity, NaN: NaN };

function groups(name: string): Group[] {
  const text = readFileSync(new URL(`../../testdata/view/${name}.json`, import.meta.url), "utf8");
  return JSON.parse(text, (_k, v) => (typeof v === "string" && Object.hasOwn(SPECIAL, v) ? SPECIAL[v] : v)).tests;
}

/** A row's call: its arguments, the result before it for a `then` row, and the group's shared
 * objects, so two rows that name the same input read one object, as two draws of one tile do. */
export type Call = (args: Row["args"], context: { before?: unknown; shared: <T>(input: unknown, make: () => T) => T }) => unknown;

/** `actual` holds what `expect` states, numbers equal or within `close`. An object expected of an
 * object names the keys checked; one expected of an array names its indices. */
function check(actual: unknown, expect: unknown, close: number | undefined, at: string): void {
  if (typeof expect === "number") {
    assert.equal(typeof actual, "number", `${at}: ${String(actual)} is not a number`);
    if (close === undefined) assert.ok(Object.is(actual, expect), `${at}: ${String(actual)}, expected ${expect}`);
    else assert.ok(Math.abs((actual as number) - expect) < close, `${at}: ${String(actual)} is not ${expect}`);
  } else if (Array.isArray(expect)) {
    assert.ok(Array.isArray(actual), `${at}: ${JSON.stringify(actual)} is not an array`);
    assert.equal(actual.length, expect.length, `${at}: ${actual.length} items, expected ${expect.length}`);
    expect.forEach((e, i) => check(actual[i], e, close, `${at}[${i}]`));
  } else if (expect !== null && typeof expect === "object") {
    assert.ok(actual !== null && typeof actual === "object", `${at}: ${JSON.stringify(actual)} is not an object`);
    for (const [k, e] of Object.entries(expect)) {
      assert.ok(k in actual, `${at}: no ${k} in ${JSON.stringify(actual)}`);
      check((actual as Record<string, unknown>)[k], e, close, Array.isArray(actual) ? `${at}[${k}]` : `${at}.${k}`);
    }
  } else assert.equal(actual, expect, at);
}

/** Projections a row's `view` names, of what its call gave. */
const VIEWS: Record<string, (result: unknown, args: Row["args"]) => unknown> = {
  span_and_place: (r, args) => {
    const { lo, hi } = r as { lo: number; hi: number };
    return { span: hi - lo, place: (args.at - lo) / (hi - lo) };
  },
};

/** One test a group of `name`'s table, each running its rows through `calls`. */
export function runTable(name: string, calls: Record<string, Call>): void {
  for (const group of groups(name)) {
    test(group.name, () => {
      const objects = new Map<string, unknown>();
      const shared = <T>(input: unknown, make: () => T): T => {
        const key = JSON.stringify(input);
        if (!objects.has(key)) objects.set(key, make());
        return objects.get(key) as T;
      };
      group.rows.forEach((row, i) => {
        const at = `${group.name} [${i}] ${row.fn}`;
        const call = calls[row.fn];
        assert.ok(call, `${name} has no ${row.fn}`);
        const result = call(row.args, { shared });
        const view = row.view === undefined ? result : VIEWS[row.view](result, row.args);
        check(view, row.expect, row.close, at);
        for (const then of row.then ?? []) {
          const next = calls[then.fn];
          assert.ok(next, `${name} has no ${then.fn}`);
          check(next(then.args, { before: result, shared }), then.expect, then.close, `${at} then ${then.fn}`);
        }
      });
    });
  }
}

/** A `[num, den]` as a Ratio. */
export const ratio = ([num, den]: [number, number]) => ({ num, den });
