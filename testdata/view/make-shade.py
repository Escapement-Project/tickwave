# /// script
# requires-python = ">=3.11"
# ///
"""Writes shade.json: `uv run make-shade.py > shade.json`.

The edge counts a density stands for are worked from FORMAT.md's definition with Python's big
integers, none of the Rust or TypeScript code: a count e has density at least d exactly when
e^4 >= 2^(d - 1), as 1 + floor(4 log2 e) >= d. The other rows' values are worked by hand, each
with its reason in its note; for `looks`, which columns stroke, which make a pulse and which a
run, each pulse's centre and spacing and each rail's window, are set out by hand, and only the
band's, a stroke's and a rail's formulas are evaluated.
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
        "name": "band: the floor up to one edge a second, the top, 0.6, from 1e8, linear in the logarithm between",
        "rows": [
            row("band", {"rate": 0}, 0, "No edge: no band."),
            row("band", {"rate": -1}, 0),
            row("band", {"rate": "NaN"}, 0),
            row("band", {"rate": 0.001}, 0.2, "Below one edge a second: the floor, so a rare edge still shows."),
            row("band", {"rate": 1}, 0.2),
            row("band", {"rate": 10000}, 0.4, "Half way along 1 to 1e8 in the logarithm: 0.2 + 0.4 / 2.", 1e-12),
            row("band", {"rate": 100}, 0.3, "A quarter of the way: 0.2 + 0.4 / 4.", 1e-12),
            row("band", {"rate": 1e8}, 0.6, "The top: short of full ink, so the busiest band still reads as a fill.", 1e-12),
            row("band", {"rate": 4e9}, 0.6, "Past the top: held there.", 1e-12),
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
        "name": "rails: with an edge, the band's opacity times each level's share over the larger, at least a fifth of it; without, the band's opacity times each level's share",
        "rows": [
            row("rails", {"high": 0.5, "edges": True, "band": 0.6}, [0.6, 0.6], "An even wave: both rails at the band's opacity."),
            row("rails", {"high": 0.75, "edges": True, "band": 0.6}, [0.6, 0.2], "75 % high: the high rail at the band's, the low at a third of it.", 1e-12),
            row("rails", {"high": 0.3, "edges": True, "band": 0.5}, [0.5 * 3 / 7, 0.5], "30 % high: the low rail at the band's, the high at 3/7 of it.", 1e-12),
            row("rails", {"high": 0.1, "edges": True, "band": 0.5}, [0.1, 0.5], "10 % high: 1/9 of the low share, under the floor, so a fifth of the band's.", 1e-12),
            row("rails", {"high": 0, "edges": True, "band": 0.25}, [0.05, 0.25], "A share that rounds to none of the time high, with an edge: the high rail at a fifth of the band's.", 1e-12),
            row("rails", {"high": 1, "edges": True, "band": 0.25}, [0.25, 0.05], None, 1e-12),
            row("rails", {"high": 0.5, "edges": False, "band": 0.6}, [0.3, 0.3], "No edge about it: each rail the band's opacity times its share, with no floor.", 1e-12),
            row("rails", {"high": 0.25, "edges": False, "band": 0.4}, [0.1, 0.3], "A quarter high, no edge: the shares themselves, not over the larger, so the high rail 0.1, not 0.133.", 1e-12),
            row("rails", {"high": 0, "edges": False, "band": 0.6}, [0, 0.6], "Wholly low, no edge: the low rail alone, at the band's opacity, so a level held inside a run reads in the band's tone.", 1e-12),
            row("rails", {"high": 1, "edges": False, "band": 0.6}, [0.6, 0], None, 1e-12),
            row("rails", {"high": "NaN", "edges": True, "band": 0.6}, [0, 0], "Nothing of the column known: neither rail."),
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

FLOOR, TOP, RATE_TOP, RAIL_FLOOR = 0.2, 0.6, 1e8, 0.2


def band(rate):
    """The scale: the floor to one edge a second, the top from 1e8, linear in log2 between."""
    if not rate > 0:
        return 0
    return FLOOR + (TOP - FLOOR) * min(1, max(0, math.log2(rate) / math.log2(RATE_TOP)))


def rail(share, b):
    """A rail where the window holds an edge: the band's opacity by its level's share over the larger share, at least the floor's part."""
    return b * max(RAIL_FLOOR, share / max(share, 1 - share))


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


def looks_row(c, expect, note, close=1e-12, width=WIDTH):
    args = {"columns": c, "shown": [0, len(c["state"])], "width": width, "per": PER, "timebase": [1000000, 1]}
    return row("looks", args, expect, note, close)


def shaded(b, share):
    """A run's column whose window holds an edge, high `share` of its known time."""
    return ["shade", b, rail(share, b), rail(1 - share, b)]


def flat(count, but):
    """Every column of `count` flat, but those `but` names."""
    return {**{str(k): "flat" for k in range(count)}, **but}


# Edges 2 columns apart: with a line 2 wide, nothing between their paint, so they join; the run's
# first and last 8 apart, past the join distance of 3.
RUN = columns(13, [(2, 0.5), (4, 0.5), (6, 0.5), (8, 0.5), (10, 0.5)])
RUN_BAND = band(5 / 0.009)
# Columns 2 to 10 are high 500, 1000, 500, 0, 500, 1000, 500, 0 and 500 of their 1,000 ticks; each
# window is two columns either side, held to the run.
RUN_SHARES = {2: 2000 / 3000, 3: 2000 / 4000, 4: 2500 / 5000, 5: 3000 / 5000, 6: 2500 / 5000, 7: 2000 / 5000, 8: 2500 / 5000, 9: 2000 / 4000, 10: 1000 / 3000}
NO_DATA = columns(11, [(4, 0.9), (6, 0.1)])
NO_DATA["state"][5] = "NONE"
# Busy columns 2 to 6, two edges each and high a tick each: a run whose first and last are 4 apart.
FLOOR_RUN = columns(10, [(2, -2), (3, -2), (4, -2), (5, -2), (6, -2)])
for k in range(2, 7):
    FLOOR_RUN["high"][k] = 1
# Busy columns 2, 4 and 6, 2 columns apart centre to centre, so joined, and low columns between.
FLAT_IN_RUN = columns(10, [(2, -2), (4, -2), (6, -2)])
for k in (2, 4, 6):
    FLAT_IN_RUN["high"][k] = 1
# A rise in column 2, a fall in column 4 and a rise in column 6, each in its middle; the channel
# ends 0.6 into column 6, so its known ticks are 600, high 100 of them.
KNOWN = columns(10, [(2, 0.5), (4, 0.5), (6, 0.5)])
KNOWN["known"][6], KNOWN["high"][6] = 600, 100
for k in range(7, 10):
    KNOWN["state"][k] = "NONE"
    KNOWN["known"][k] = KNOWN["high"][k] = 0
# A busy column with no edge, as an overview gives one where a gap's end lies on a bucket's
# boundary, and an edge 3 columns from it.
EDGELESS = columns(11, [(4, -2), (7, 0.5)])
EDGELESS["edges"][4] = 0
# Three edges 1.5 and 1.6 columns apart: joined, first to last 3.1, so a run; columns 8 to 11 high
# 500, 1000, 0 and 400 ticks.
LONG3 = columns(16, [(8, 0.5), (10, 0.0), (11, 0.6)])
# With a line 5 wide the join distance is 6: three edges 3 and 3.1 apart, first to last 6.1, a run
# from column 4 to 10, high 500, 1000, 1000, 500, 0, 0 and 400 ticks.
WIDE_LONG = columns(14, [(4, 0.5), (7, 0.5), (10, 0.6)])
# A line 5 wide: edges 5.9 apart join, and columns 5 and 11, midway, have none in their windows.
WIDE_HELD = columns(17, [(2, 0.5), (8, 0.4), (14, 0.3)])
# Two runs, each first to last 4, past the join distance of 3, high between them: the first's last
# edge at 6.5, its paint to 7.5; the second's first at 11.4 or 11.6, its paint from 10.4 or 10.6, a
# bare 2.9 or 3.1 between. One run holds 3 edges in its 5 ms, the other 5, so their bands differ.
PAUSE_29 = columns(18, [(2, 0.5), (4, 0.5), (6, 0.5), (11, 0.4), (12, 0.4), (13, 0.4), (14, 0.4), (15, 0.4)])
PAUSE_29_FIRST = columns(18, [(2, 0.5), (3, 0.5), (4, 0.5), (5, 0.5), (6, 0.5), (11, 0.4), (13, 0.4), (15, 0.4)])
PAUSE_31 = columns(18, [(2, 0.5), (4, 0.5), (6, 0.5), (11, 0.6), (12, 0.6), (13, 0.6), (14, 0.6), (15, 0.6)])
PAUSE_CUT = columns(18, [(2, 0.5), (4, 0.5), (6, 0.5), (11, 0.4), (12, 0.4), (13, 0.4), (14, 0.4), (15, 0.4)])
PAUSE_CUT["state"][8] = "NONE"
PAUSE_CUT["known"][8] = PAUSE_CUT["high"][8] = 0
# A line 5 wide, the join distance 6: runs of three edges 5 apart, first to last 10; the first's
# last edge at 12.5, its paint to 15; the second's first at 23.4 or 23.6, a bare 5.9 or 6.1 between.
WIDE_PAUSE_59 = columns(36, [(2, 0.5), (7, 0.5), (12, 0.5), (23, 0.4), (28, 0.4), (33, 0.4)])
WIDE_PAUSE_61 = columns(36, [(2, 0.5), (7, 0.5), (12, 0.5), (23, 0.6), (28, 0.6), (33, 0.6)])

TESTS.append({
    "name": "looks: lone edges stroked, busy columns alone and short runs pulses at their centres, full ink beside each other, the rest of a run shaded, its rails over the columns about each, and a level held briefly between runs at their band",
    "rows": [
        looks_row(columns(11, [(5, 0.5)]), {"4": "flat", "5": ["stroke", 1], "6": "flat"}, "A lone edge with none beside it: full ink."),
        looks_row(columns(13, [(2, 0.5), (6, 0.5), (10, 0.5)]), {str(k): ["stroke", stroke(band(250), 4, WIDTH)] for k in (2, 6, 10)}, "Edges 4 columns apart, a line 2 wide: a gap of 2 between strokes, none joined. An edge every 4 ms, 250 a second, its band 0.36: each stroke 0.72."),
        looks_row(columns(24, [(2, 0.5), (6, 0.5), (18, 0.5)]), {"6": ["stroke", stroke(band(250), 4, WIDTH)], "18": ["stroke", 1]}, "Column 6's edge is 4 columns from one neighbour and 12 from the other: it takes the nearer. Column 18's, 12 from its only neighbour: an edge every 12 ms, 83 a second, band 0.32, its stroke past full ink."),
        looks_row(RUN, {"1": "flat", **{str(k): shaded(RUN_BAND, s) for k, s in RUN_SHARES.items()}, "11": "flat"}, "Edges 2 columns apart join, with the columns between, in a run from column 2 to 10: 5 edges in 9 ms. Column 2's rails take columns 2 to 4, high 2,000 of 3,000: the high rail the band's opacity, the low half of it; column 7's 5 to 9, 2,000 of 5,000: the low rail the band's, the high two thirds of it."),
        looks_row(FLOOR_RUN, {str(k): ["shade", band(10 / 0.005), band(10 / 0.005) * RAIL_FLOOR, band(10 / 0.005)] for k in range(2, 7)}, "Busy columns 2 to 6, two edges each and high a tick each: each column's window is high a few ticks of thousands, so its high rail takes a fifth of the band's opacity, the low the band's."),
        looks_row(FLAT_IN_RUN, {str(k): ["shade", band(6 / 0.005), band(6 / 0.005) * RAIL_FLOOR, band(6 / 0.005)] for k in range(2, 7)}, "Columns 3 and 5 hold no edge, but their windows, 2 to 5 and 3 to 6, do: their high rails take a fifth of the band's opacity too."),
        looks_row(KNOWN, {"2": shaded(band(3 / 0.005), 2000 / 3000), "6": shaded(band(3 / 0.005), 600 / 2600)}, "Column 6 is known for 600 ticks, high 100: its window, columns 4 to 6, is high 600 of the 2,600 ticks known, 0.231, not its columns' shares' mean, 0.222. Its high rail takes 0.3 of the band's opacity, 0.231 over 0.769."),
        looks_row(EDGELESS, {"4": ["pulse", stroke(band(1 / 0.003), 3, WIDTH), 4.5]}, "A busy column with no edge changes level at least once: a pulse at its middle, stroked as one edge over the 3 ms to its neighbour."),
        looks_row(NO_DATA, {"4": ["stroke", stroke(band(1 / 0.0012), 1.2, WIDTH)], "6": ["stroke", stroke(band(1 / 0.0012), 1.2, WIDTH)]}, "Edges 1.2 columns apart, either side of a column with no data: close enough to join, but a column with no data ends a run."),
        looks_row(columns(9, [(4, -2)]), flat(9, {"4": ["pulse", 1, 4.5]}), "A busy column, a pair as verge's under-range flag gives one, with nothing beside it: a pulse at its middle at full ink."),
        looks_row(columns(11, [(4, -2), (7, 0.5)]), {"4": ["pulse", stroke(band(2 / 0.003), 3, WIDTH), 4.5], "7": ["stroke", stroke(band(1 / 0.003), 3, WIDTH)]}, "A busy column and an edge 3 columns apart, a gap of 1 between their paint: the busy column a pulse at the rate of its 2 edges over the 3 ms, the edge a stroke at its 1 over them."),
        looks_row(columns(20, [(10, 0.7), (11, 0.3)]), flat(20, {"10": ["pulse", 1, 11.0], "11": ["pulse", 1, 11.0]}), "The same pair across a column boundary, a rise at 10.7 and a fall at 11.3: they join, 0.6 apart, under the join distance, so a pulse at their middle at full ink, each of its columns holding it, as the busy column above."),
        looks_row(columns(16, [(8, 0.5), (10, 0.0), (11, 0.4)]), flat(16, {str(k): ["pulse", 1, (8.5 + 11.4) / 2] for k in range(8, 12)}), "Three edges 1.5 and 1.4 columns apart: joined, first to last 2.9, under the join distance of 3, so any two of them would join alone. One pulse at its middle, 9.95."),
        looks_row(LONG3, {"7": "flat", "8": shaded(band(3 / 0.004), 1500 / 3000), "9": shaded(band(3 / 0.004), 1900 / 4000), "10": shaded(band(3 / 0.004), 1900 / 4000), "11": shaded(band(3 / 0.004), 1400 / 3000), "12": "flat"}, "Three edges 1.5 and 1.6 apart: first to last 3.1, past the join distance, so a run, 3 edges in 4 ms, its rails by window."),
        looks_row(columns(20, [(10, 0.7), (11, 0.3), (15, 0.5)]), {"10": ["pulse", stroke(band(2 / 0.0045), 4.5, WIDTH), 11.0], "11": ["pulse", stroke(band(2 / 0.0045), 4.5, WIDTH), 11.0], "15": ["stroke", stroke(band(1 / 0.0045), 4.5, WIDTH)]}, "A pulse with its middle at 11.0 and a lone edge at 15.5: each is 4.5 from the other, the edge measuring to the pulse's middle, where it draws. The pulse at its 2 edges over the 4.5 ms, the edge at its 1."),
        looks_row(columns(14, [(4, 0.5), (9, 0.0)]), flat(14, {str(k): ["pulse", 1, 6.75] for k in range(4, 10)}), "A line 5 wide joins edges under 6 apart: two edges 4.5 apart are a pulse at their middle, 6.75.", 1e-12, 5),
        looks_row(columns(14, [(4, 0.5), (9, 0.0)]), {"4": ["stroke", stroke(band(1 / 0.0045), 4.5, WIDTH)], "9": ["stroke", stroke(band(1 / 0.0045), 4.5, WIDTH)]}, "The same edges with a line 2 wide: apart, each a stroke."),
        looks_row(columns(14, [(4, 0.5), (7, 0.5), (10, 0.4)]), flat(14, {str(k): ["pulse", 1, 7.45] for k in range(4, 11)}), "A line 5 wide: three edges 3 and 2.9 apart, first to last 5.9, under the join distance of 6: one pulse.", 1e-12, 5),
        looks_row(WIDE_LONG, {
            "3": "flat",
            **{str(k): shaded(band(3 / 0.007), s) for k, s in {4: 2500 / 3000, 5: 3000 / 4000, 6: 3000 / 5000, 7: 2500 / 5000, 8: 1900 / 5000, 9: 900 / 4000, 10: 400 / 3000}.items()},
            "11": "flat",
        }, "A line 5 wide: three edges 3 and 3.1 apart, first to last 6.1, a run from column 4 to 10, 3 edges in 7 ms. Column 10's window, 8 to 10, is high 400 of 3,000: its high rail 0.154 of the low share, so a fifth of the band's opacity.", 1e-12, 5),
        looks_row(WIDE_HELD, {"2": shaded(band(3 / 0.013), 2500 / 3000), "5": ["shade", band(3 / 0.013), band(3 / 0.013), 0], "11": ["shade", band(3 / 0.013), 0, band(3 / 0.013)]}, "A line 5 wide: edges 5.9 apart join in a run from column 2 to 14, 3 edges in 13 ms. Column 5's window, 3 to 7, holds no edge and is wholly high: its high rail takes the band's opacity, not full ink, and its low none; column 11's, wholly low, the reverse.", 1e-12, 5),
        looks_row(columns(14, [(4, -2), (9, -2)]), flat(14, {"4": ["pulse", 1, 4.5], "9": ["pulse", 1, 9.5]}), "Two busy columns 5 apart, a pair each: two pulses, apart. A pulse's spacing counts no pulse beside it, so each takes full ink, where the 5 ms between them would give 0.83."),
        looks_row(columns(16, [(4, -2), (8, -2), (12, -2)]), flat(16, {"4": ["pulse", 1, 4.5], "8": ["pulse", 1, 8.5], "12": ["pulse", 1, 12.5]}), "Three pulses 4 apart, a chain: each takes full ink, the middle one too."),
        looks_row(columns(20, [(10, 0.7), (11, 0.3), (15, -2)]), flat(20, {"10": ["pulse", 1, 11.0], "11": ["pulse", 1, 11.0], "15": ["pulse", 1, 15.5]}), "A pulse across a column boundary, its middle at 11.0, and a busy column 4.5 from it: both full ink."),
        looks_row(columns(16, [(4, -2), (8, -2), (11, 0.5)]), {"4": ["pulse", 1, 4.5], "8": ["pulse", stroke(band(2 / 0.003), 3, WIDTH), 8.5], "11": ["stroke", stroke(band(1 / 0.003), 3, WIDTH)]}, "Pulses at 4.5 and 8.5 and a lone edge at 11.5: the first's only neighbour is a pulse, so full ink; the second counts the edge, 3 from it, and not the pulse; the edge counts the pulse beside it."),
        looks_row(columns(16, [(3, -2), (7, 0.5), (9, 0.5), (11, 0.5), (13, 0.5)]), {"3": ["pulse", stroke(band(2 / 0.004), 4, WIDTH), 3.5], "4": "flat", "5": "flat", "6": "flat"}, "A pulse 4 from a run's first edge: a run is no pulse, so the pulse counts it. The level between, a bare 2 between their paint, is beside a pulse: full ink."),
        looks_row(PAUSE_29, {
            "6": shaded(band(3 / 0.005), 1000 / 3000),
            **{str(k): ["held", band(5 / 0.005)] for k in range(7, 11)},
            "11": shaded(band(5 / 0.005), 1400 / 3000),
        }, "Two runs a bare 2.9 apart, under the join distance of 3: two runs, 3 and 5 edges in 5 ms. The high level held between keeps its gap, no band, its line at the larger band, the second run's."),
        looks_row(PAUSE_29_FIRST, {
            "6": shaded(band(5 / 0.005), 1500 / 3000),
            **{str(k): ["held", band(5 / 0.005)] for k in range(7, 11)},
            "11": shaded(band(3 / 0.005), 1000 / 3000),
        }, "The same with the busier run first: the held line takes its band, the first run's."),
        looks_row(PAUSE_31, {
            "6": shaded(band(3 / 0.005), 1000 / 3000),
            **{str(k): "flat" for k in range(7, 11)},
            "11": shaded(band(5 / 0.005), 1600 / 3000),
        }, "Two runs a bare 3.1 apart: the level between draws as any other, at full ink."),
        looks_row(PAUSE_CUT, {str(k): "flat" for k in range(7, 11)}, "The runs 2.9 apart with no data in column 8: the level either side of it draws at full ink."),
        looks_row(WIDE_PAUSE_59, {
            "12": shaded(band(3 / 0.011), 500 / 3000),
            **{str(k): ["held", band(3 / 0.011)] for k in range(13, 23)},
            "23": shaded(band(3 / 0.011), 400 / 3000),
        }, "A line 5 wide: runs a bare 5.9 apart, under the join distance of 6: the level between held at their band, 3 edges in 11 ms each.", 1e-12, 5),
        looks_row(WIDE_PAUSE_61, {
            "12": shaded(band(3 / 0.011), 500 / 3000),
            **{str(k): "flat" for k in range(13, 23)},
            "23": shaded(band(3 / 0.011), 600 / 3000),
        }, "A line 5 wide: runs a bare 6.1 apart, the level between at full ink.", 1e-12, 5),
        looks_row(columns(16, [(2, 0.5), (4, 0.5), (6, 0.5), (9, 0.5)]), {"6": shaded(band(3 / 0.005), 1000 / 3000), "7": "flat", "8": "flat", "9": ["stroke", stroke(band(1 / 0.003), 3, WIDTH)]}, "A run and a lone edge a bare 1 after it: the level between beside an edge, not between two runs, draws at full ink."),
        looks_row(columns(16, [(2, 0.5), (4, 0.5), (6, 0.5), (9, -2)]), {"6": shaded(band(3 / 0.005), 1000 / 3000), "7": "flat", "8": "flat", "9": ["pulse", stroke(band(2 / 0.003), 3, WIDTH), 9.5]}, "A run and a busy column a bare 1 after it: a pulse at its 2 edges over the 3 ms to the run's last edge, and the level between, beside a pulse, at full ink."),
    ],
})

ABOUT = (
    "How a digital column with edges reads: the edge counts a density stands for, the edge rate, the "
    "band's opacity on its fixed scale, a lone edge's stroke, the rails' from the share high and the band's, what a "
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
