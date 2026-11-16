# /// script
# requires-python = ">=3.11"
# ///
"""Writes shade.json: `uv run make-shade.py > shade.json`.

The edge counts a density stands for are worked from FORMAT.md's definition with Python's big
integers, none of the Rust or TypeScript code: a count e has density at least d exactly when
e^4 >= 2^(d - 1), as 1 + floor(4 log2 e) >= d. The other rows' values are worked by hand, each
with its reason in its note; for `looks`, which columns stroke and which run, and each rail's
window, are set out by hand, and only the band's and a stroke's formulas are evaluated.
"""

import json
import math


def least(d):
    """The least count e >= 1 with e^4 >= 2^(d - 1)."""
    t = 2 ** (d - 1)
    e = math.isqrt(math.isqrt(t))
    while e**4 < t:
        e += 1
    while e > 1 and (e - 1) ** 4 >= t:
        e -= 1
    return max(e, 1)


def counts(d):
    """The least and most counts of density d; a step no count reaches stands for the next count up."""
    if d == 0:
        return None
    lo, hi = least(d), least(d + 1) - 1
    return [lo, max(lo, hi)]


def count(d):
    c = counts(d)
    return 0 if c is None else math.sqrt(c[0] * c[1])


def row(fn, args, expect, note=None, close=None):
    r = {"fn": fn, "args": args, "expect": expect}
    if close is not None:
        r["close"] = close
    if note:
        r["note"] = note
    return r


def count_row(d, note=None):
    c = counts(d)
    # A count of several values is a square root: held to a part in 10^12 of it.
    close = max(1e-9, count(d) * 1e-12) if c and c[0] != c[1] else None
    return row("count", {"density": d}, count(d), note, close)


NOTES = {
    2: "No count has density 2, 3 or 4: one edge is 1, two are 5. Each stands for the next count up, 2, alone.",
    6: "Density 6 stands for 3, the next count up; 3 itself is 7.",
    13: "8 and 9 are both 13: 8^4 = 2^12 and 9^4 < 2^13.",
    212: "The largest density a count below 2^53 gives: 2^53 - 1 is the most.",
}
COUNT_NOTES = {0: "No edge.", 1: "Small counts are exact.", 13: "sqrt(8 x 9)."}

TESTS = [
    {
        "name": "counts: the least and most edges a density stands for",
        "rows": [
            row("counts", {"density": d}, counts(d), NOTES.get(d))
            for d in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 13, 40, 100, 212]
        ],
    },
    {
        "name": "count: the geometric mean of the least and most",
        "rows": [count_row(d, COUNT_NOTES.get(d)) for d in [0, 1, 5, 7, 9, 13, 40, 100, 212]],
    },
    {
        "name": "rate: edges a second from edges over ticks of the axis",
        "rows": [
            row("rate", {"edges": 2, "ticks": 96000000, "timebase": [96000000, 1]}, 2, "Two edges in a second of a 96 MHz axis."),
            row("rate", {"edges": 1.5, "ticks": 1000, "timebase": [1000000, 1]}, 1500, "1.5 edges, an estimate from buckets, in a millisecond."),
            row("rate", {"edges": 3, "ticks": 2, "timebase": [3, 2]}, 2.25, "Ticks of 2/3 s: 2 ticks are 4/3 s, 3 edges over them 9/4 a second."),
            row("rate", {"edges": 0, "ticks": 1000, "timebase": [1000, 1]}, 0),
        ],
    },
    {
        "name": "band: the floor up to one edge a second, full from 1e8, linear in the logarithm between",
        "rows": [
            row("band", {"rate": 0}, 0, "No edge: no band."),
            row("band", {"rate": -1}, 0),
            row("band", {"rate": "NaN"}, 0),
            row("band", {"rate": 0.001}, 0.2, "Below one edge a second: the floor, so a rare edge still shows."),
            row("band", {"rate": 1}, 0.2),
            row("band", {"rate": 10000}, 0.6, "Half way along 1 to 1e8 in the logarithm: 0.2 + 0.8 / 2.", 1e-12),
            row("band", {"rate": 100}, 0.4, "A quarter of the way: 0.2 + 0.8 / 4.", 1e-12),
            row("band", {"rate": 1e8}, 1),
            row("band", {"rate": 4e9}, 1, "Past full: held there."),
        ],
    },
    {
        "name": "stroke: a lone edge's stroke covers its stretch of band as the band at its spacing's rate would, up to full ink",
        "rows": [
            row("stroke", {"band": 0.4, "spacing": 3, "width": 2}, 0.6, "Edges 3 device pixels apart, a line 2 wide: the strokes cover 2/3 of the band, so 0.4 of it takes 0.6.", 1e-12),
            row("stroke", {"band": 0.3, "spacing": 4, "width": 2}, 0.6, None, 1e-12),
            row("stroke", {"band": 0.2, "spacing": 20, "width": 2}, 1, "Past full ink: held there."),
            row("stroke", {"band": 0, "spacing": "Infinity", "width": 2}, 1, "No edge beside it: full ink, though no rate is left to shade."),
        ],
    },
    {
        "name": "rails: each level's share of the known time, at least the floor where the column has an edge",
        "rows": [
            row("rails", {"high": 0.5, "edges": False}, [0.5, 0.5]),
            row("rails", {"high": 0, "edges": False}, [0, 1], "Wholly low, no edge: the low rail alone, as a flat column draws."),
            row("rails", {"high": 1, "edges": False}, [1, 0]),
            row("rails", {"high": 0, "edges": True}, [0.2, 1], "A share that rounds to none of the time high, with an edge: the high rail at the floor."),
            row("rails", {"high": 1, "edges": True}, [1, 0.2]),
            row("rails", {"high": 0.1, "edges": True}, [0.2, 0.9]),
            row("rails", {"high": 0.75, "edges": True}, [0.75, 0.25]),
            row("rails", {"high": "NaN", "edges": True}, [0, 0], "Nothing of the column known: neither rail."),
        ],
    },
    {
        "name": "duty_share: what a duty stands for, and the shares that round to it",
        "rows": [
            row("duty_share", {"duty": 0}, [0, 0, 1 / 28], "Under 1/28 of the time high rounds to 0."),
            row("duty_share", {"duty": 1}, [1 / 14, 1 / 28, 3 / 28], None, 1e-15),
            row("duty_share", {"duty": 7}, [0.5, 13 / 28, 15 / 28], None, 1e-15),
            row("duty_share", {"duty": 14}, [1, 27 / 28, 1], None, 1e-15),
            row("duty_share", {"duty": 15}, None, "Wholly outside the kept span: no share."),
        ],
    },
]

FLOOR, RATE_FULL = 0.2, 1e8


def band(rate):
    """FORMAT's scale: the floor to one edge a second, full from 1e8, linear in log2 between."""
    if not rate > 0:
        return 0
    return FLOOR + (1 - FLOOR) * min(1, max(0, math.log2(rate) / math.log2(RATE_FULL)))


def stroke(b, spacing, width):
    return min(1, b * spacing / width)


PER, WIDTH = 1000, 2


def columns(count, edges):
    """Columns of 1,000 ticks, low but where `edges` puts an edge at (column, at), or a busy column
    of n edges at (column, -n), half high: the level flips at each edge."""
    c = {"state": [], "at": [], "edges": [], "high": [], "known": [PER] * count}
    level, todo = False, dict(edges)
    for k in range(count):
        at = todo.get(k)
        if at is not None and at < 0:
            c["state"].append("BUSY"); c["at"].append(0); c["edges"].append(-at); c["high"].append(PER / 2)
            level ^= int(-at) % 2 == 1
        elif at is not None:
            c["state"].append("FALL" if level else "RISE"); c["at"].append(at); c["edges"].append(1)
            c["high"].append(at * PER if level else (1 - at) * PER)
            level = not level
        else:
            c["state"].append("HIGH" if level else "LOW"); c["at"].append(0); c["edges"].append(0)
            c["high"].append(PER if level else 0)
    return c


def looks_row(c, expect, note, close=1e-12):
    args = {"columns": c, "shown": [0, len(c["state"])], "width": WIDTH, "per": PER, "timebase": [1000000, 1]}
    return row("looks", args, expect, note, close)


def shaded(b, share):
    return ["shade", b, share, 1 - share]


RUN = columns(13, [(2, 0.5), (4, 0.5), (6, 0.5), (8, 0.5), (10, 0.5)])
RUN_BAND = band(5 / 0.009)
# Columns 2 to 10 are high 500, 1000, 500, 0, 500, 1000, 500, 0 and 500 of their 1,000 ticks; each
# window is two columns either side, held to the run.
RUN_SHARES = {2: 2000 / 3000, 3: 2000 / 4000, 4: 2500 / 5000, 5: 3000 / 5000, 6: 2500 / 5000, 7: 2000 / 5000, 8: 2500 / 5000, 9: 2000 / 4000, 10: 1000 / 3000}
NO_DATA = columns(11, [(4, 0.9), (6, 0.1)])
NO_DATA["state"][5] = "NONE"
FLOOR_RUN = columns(10, [(2, -2), (3, -2), (4, -2)])
for k in (2, 3, 4):
    FLOOR_RUN["high"][k] = 1
# Busy columns 2 and 4, 2 columns apart centre to centre, so joined, and column 3 low between them.
FLAT_IN_RUN = columns(10, [(2, -2), (4, -2)])
FLAT_IN_RUN["high"][2] = FLAT_IN_RUN["high"][4] = 1
# A rise in column 2 and a fall in column 4, both in their middles; the channel ends 0.6 into
# column 4, so its known ticks are 600, high its first 500.
SHORT = columns(10, [(2, 0.5), (4, 0.5)])
SHORT["known"][4] = 600
for k in range(5, 10):
    SHORT["state"][k] = "NONE"
    SHORT["known"][k] = SHORT["high"][k] = 0
# A busy column with no edge, as an overview gives one where a gap's end lies on a bucket's
# boundary, and an edge 3 columns from it.
EDGELESS = columns(11, [(4, -2), (7, 0.5)])
EDGELESS["edges"][4] = 0

TESTS.append({
    "name": "looks: lone edges and busy columns alone stroked, the rest of a run shaded, its rails over the columns about each",
    "rows": [
        looks_row(columns(11, [(5, 0.5)]), {"4": "flat", "5": ["stroke", 1], "6": "flat"}, "A lone edge with none beside it: full ink."),
        looks_row(columns(13, [(2, 0.5), (6, 0.5), (10, 0.5)]), {str(k): ["stroke", stroke(band(250), 4, WIDTH)] for k in (2, 6, 10)}, "Edges 4 columns apart, a line 2 wide: a gap of 2 between strokes, none joined. An edge every 4 ms, 250 a second, its band 0.44: each stroke 0.88."),
        looks_row(columns(24, [(2, 0.5), (6, 0.5), (18, 0.5)]), {"6": ["stroke", stroke(band(250), 4, WIDTH)], "18": ["stroke", 1]}, "Column 6's edge is 4 columns from one neighbour and 12 from the other: it takes the nearer. Column 18's, 12 from its only neighbour: an edge every 12 ms, 83 a second, band 0.39, its stroke past full ink."),
        looks_row(RUN, {"1": "flat", **{str(k): shaded(RUN_BAND, s) for k, s in RUN_SHARES.items()}, "11": "flat"}, "Edges 2 columns apart join, with the columns between, in a run from column 2 to 10: 5 edges in 9 ms. Column 2's rails take columns 2 to 4, high 2,000 of 3,000; column 7's 5 to 9, 2,000 of 5,000."),
        looks_row(FLOOR_RUN, {str(k): ["shade", band(6 / 0.003), 0.2, 1 - 3 / 3000] for k in (2, 3, 4)}, "Busy columns 2 to 4, two edges each and high a tick each: each column's window is the whole run, high 3 of 3,000 ticks, so its high rail takes the floor."),
        looks_row(FLAT_IN_RUN, {str(k): ["shade", band(4 / 0.003), 0.2, 1 - 2 / 3000] for k in (2, 3, 4)}, "Column 3 holds no edge, but its window, columns 2 to 4, does: its high rail takes the floor too."),
        looks_row(SHORT, {"2": shaded(band(2 / 0.003), 2000 / 2600), "4": shaded(band(2 / 0.003), 2000 / 2600)}, "Column 4 is known for 600 ticks, high 500: the window's share is its ticks high over its ticks known, 2,000 of 2,600, 0.769, not its columns' shares' mean, 0.778."),
        looks_row(EDGELESS, {"4": ["stroke", stroke(band(1 / 0.003), 3, WIDTH)]}, "A busy column with no edge changes level at least once: it strokes as one edge over the 3 ms to its neighbour."),
        looks_row(NO_DATA, {"4": ["stroke", stroke(band(1 / 0.0012), 1.2, WIDTH)], "6": ["stroke", stroke(band(1 / 0.0012), 1.2, WIDTH)]}, "Edges 1.2 columns apart, either side of a column with no data: close enough to join, but a column with no data ends a run."),
        looks_row(columns(9, [(4, -2)]), {"4": ["stroke", 1]}, "A busy column, a pair as verge's under-range flag gives one, with nothing beside it: a lone stroke at full ink."),
        looks_row(columns(11, [(4, -2), (7, 0.5)]), {"4": ["stroke", stroke(band(2 / 0.003), 3, WIDTH)], "7": ["stroke", stroke(band(1 / 0.003), 3, WIDTH)]}, "A busy column and an edge 3 columns apart, a gap of 1 between their paint: each a stroke, the busy column's at the rate of its 2 edges over the 3 ms, 0.72."),
    ],
})

ABOUT = (
    "How a digital column with edges reads: the edge counts a density stands for, the edge rate, the "
    "band's opacity on its fixed scale, a lone edge's stroke, the rails' from the share high, what a "
    "duty stands for, and how each column of a row draws. make-shade.py writes this file: the counts from FORMAT.md's definition in exact "
    "integers, the rest worked by hand."
)


def main():
    # The other tables' layout: one compact line a row.
    out = ["{", f'  "about": {json.dumps(ABOUT)},', '  "source": "web/src/render/shade.ts",', '  "tests": [']
    for i, t in enumerate(TESTS):
        out += ["    {", f'      "name": {json.dumps(t["name"])},', '      "rows": [']
        for j, r in enumerate(t["rows"]):
            out.append("        " + json.dumps(r, separators=(",", ":")) + ("," if j + 1 < len(t["rows"]) else ""))
        out += ["      ]", "    }" + ("," if i + 1 < len(TESTS) else "")]
    out += ["  ]", "}"]
    print("\n".join(out))


if __name__ == "__main__":
    main()
