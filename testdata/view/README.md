# The view's shared cases

Each file here holds the fixed cases of one of the web viewer's modules. The web's unit tests in
`web/test/` and the Rust view core's in `crates/view/tests/tables.rs` both read them, so the two
implementations are held to one set of cases.

| File | Module | Web test |
| --- | --- | --- |
| `readouts.json` | `web/src/readouts.ts`, `readouts` | `readouts.test.ts` |
| `time.json` | `web/src/time.ts`, `time` | `time.test.ts` |
| `scale.json` | `web/src/render/scale.ts`, `scale` | `scale.test.ts` |
| `axis.json` | `web/src/render/axis.ts`, `axis` | `layout.test.ts` |
| `columns.json` | `web/src/render/columns.ts`, `columns` | `columns.test.ts` |
| `probe.json` | `web/src/probe.ts`, `probe` | `probe.test.ts` |
| `shade.json` | `web/src/render/shade.ts`, `shade` | `shade.test.ts` |
| `roles.json` | `web/src/roles.ts`, `roles` | `roles.test.ts` |

A file holds `tests`, each a named group of rows that runs as one test. A row gives:

- `fn`: the function, by its Rust name. Each test file maps it to its own.
- `args`: its arguments by name. One left out takes the function's default: `step` and `runs` 1,
  an axis's `gap` 16. A timebase or a period is `[num, den]`.
- `expect`: what it gives. An object expected of an object names only the keys checked, and one
  expected of an array names the indices checked. A missing value is `null`.
- `close`, optionally: numbers match within it rather than exactly. Without it they match as
  `assert.deepStrictEqual` matches them, so NaN equals NaN and −0 is not +0.
- `view`, optionally: what of the result is checked, in place of all of it. `span_and_place` is a
  range's `hi − lo` and where `args.at` sits in it, from 0 to 1.
- `then`, optionally: rows called on the result, as a `stats` argument.
- `note`: why the expected value is what it is.

Expected values are worked by hand from the inputs, never copied from what the code gives.
`make-shade.py` writes `shade.json`: it works the edge counts a density stands for from FORMAT.md's
definition in exact integers, and holds the other rows as worked by hand. JSON has no literal for
some numbers, so `"Infinity"`, `"-Infinity"` and `"NaN"` stand for them, and a count of
`"Infinity"` is a tile whose channel's count doesn't matter. A no-break space and the minus sign
are written `\u00a0` and `\u2212`, since they look like a space and a hyphen. A pulse's edge is a
tick, `null` where there is none, or `"unread"` where the edges in hand don't reach it.

Rows in one group that name the same `samples` read one tile in the web's tests, as two draws of
one tile do: the web keeps a tile's runs between draws.
