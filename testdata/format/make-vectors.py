# /// script
# requires-python = ">=3.11"
# ///
"""Writes the format's reference vectors beside this script: places.json, overview.json and
parts.json. Every expected value is worked from FORMAT.md's rules with exact fractions and big
integers, apart from tickwave's code. Run it with `uv run testdata/format/make-vectors.py`."""

import json
import math
from fractions import Fraction as F
from pathlib import Path

HERE = Path(__file__).parent


def ratio(r):
    return F(r[0], r[1])


def pair(f):
    return [f.numerator, f.denominator]


def floor(f):
    return math.floor(f)


# Places (FORMAT.md, "Times", "Header" and "Gaps").


def grid(period, phase):
    """A channel's grid D: the least common multiple of its phase's and its period's denominators."""
    return math.lcm(period.denominator, (phase or F(0)).denominator)


def runs_of(first, phase, d, breaks):
    """Every run as (index, tick, lost, phase units), the first at index 0."""
    return [(0, first, 0, int((phase or F(0)) * d))] + [tuple(b) for b in breaks]


def run_holding(runs, i):
    return [r for r in runs if r[0] <= i][-1]


def place(run, period, d, i):
    """Sample `i`'s place on the grid of `run`: tick + phase / D + (i - index) x period."""
    index, tick, _, ph = run
    return tick + F(ph, d) + (i - index) * period


def start(run, d):
    return run[1] + F(run[3], d)


def sample_time(runs, period, d, count, i):
    """From the sample's place to the next place on its run's grid, or the next run's start."""
    run = run_holding(runs, i)
    lo, hi = place(run, period, d, i), place(run, period, d, i + 1)
    after = [r for r in runs if r[0] == i + 1]
    if after and i + 1 < count:
        hi = min(hi, start(after[0], d))
    return lo, max(hi, lo)


def lost_time(runs, period, d, k):
    """The samples lost before run `k` (k >= 1): places index to index + lost on the run
    before, cut at the run's start."""
    b = runs[k]
    if b[2] == 0:
        return None
    before = runs[k - 1]
    end = start(b, d)
    return min(place(before, period, d, b[0]), end), min(place(before, period, d, b[0] + b[2]), end)


def axis(origin, offset, m, k):
    """T = origin + floor((k + offset) x m), k an exact place."""
    return origin + floor((k + offset) * m)


def axis_time(origin, offset, m, k):
    return origin + (k + offset) * m


def place_rows(case, indices):
    period = ratio(case["grid"]["period"])
    phase = ratio(case["grid"]["phase"]) if case["grid"]["phase"] else None
    d = grid(period, phase)
    runs = runs_of(case["first"], phase, d, case["breaks"])
    mp = case["map"]
    origin, offset, m = mp["origin"], mp["offset"], ratio(mp["m"])
    rows = []
    for i in indices:
        k = place(run_holding(runs, i), period, d, i)
        lo, hi = sample_time(runs, period, d, case["count"], i)
        rows.append(
            {
                "index": i,
                "expect": {
                    "d": d,
                    "place": int(k * d),
                    "axis": axis(origin, offset, m, k),
                    "axis_time": pair(axis_time(origin, offset, m, k)),
                    "time": [int(lo * d), int(hi * d)],
                },
            }
        )
    lost = []
    for k in range(1, len(runs)):
        span = lost_time(runs, period, d, k)
        if span:
            lost.append({"break": k - 1, "expect": [int(span[0] * d), int(span[1] * d)]})
    return rows, lost


IDENTITY = {"origin": 0, "offset": 0, "m": [1, 1]}

PLACE_CASES = [
    (
        "a period of 7/3 and a phase of 1/3: a grid of thirds",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10, "breaks": [], "count": 6,
         "map": IDENTITY},
        [0, 1, 2, 5],
    ),
    (
        "breaks with phases of their own, a pause and a loss",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10,
         "breaks": [[3, 100, 0, 2], [5, 200, 4, 0]], "count": 8, "map": IDENTITY},
        [2, 3, 4, 5, 7],
    ),
    (
        "a loss the next run's start cuts short, and the sample before it",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10,
         "breaks": [[3, 18, 2, 0]], "count": 5, "map": IDENTITY},
        [2, 3],
    ),
    (
        "a pause whose run starts before the next place: the sample before it ends there",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10,
         "breaks": [[3, 16, 0, 0]], "count": 5, "map": IDENTITY},
        [2, 3],
    ),
    (
        "a grid finer than the period's: a period of 3/2 and a phase of 1/4",
        {"grid": {"period": [3, 2], "phase": [1, 4]}, "first": 0, "breaks": [[4, 7, 0, 3]],
         "count": 6, "map": IDENTITY},
        [0, 1, 3, 4, 5],
    ),
    (
        "a negative origin: the source starts before the axis does",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 60, "breaks": [], "count": 4,
         "map": {"origin": -50, "offset": 0, "m": [1, 1]}},
        [0, 3],
    ),
    (
        "a negative product: floor rounds toward minus infinity",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10, "breaks": [], "count": 2,
         "map": {"origin": 100, "offset": -12, "m": [1, 1]}},
        [0, 1],
    ),
    (
        "a stretch of 1001/1000",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10, "breaks": [], "count": 2000,
         "map": {"origin": 7, "offset": 0, "m": [1001, 1000]}},
        [0, 1, 1000, 1999],
    ),
    (
        "a source slower than the axis: three axis ticks a source tick",
        {"grid": {"period": [7, 3], "phase": [1, 3]}, "first": 10, "breaks": [], "count": 3,
         "map": {"origin": 0, "offset": 5, "m": [3, 1]}},
        [0, 2],
    ),
]


def trim(case):
    """A cut from source tick `from` keeps every sample whose place lies at or after it; the first
    kept starts the run at its whole tick, the rest its phase (FORMAT.md, "Moved and trimmed
    channels")."""
    period = ratio(case["period"])
    phase = ratio(case["phase"]) if case["phase"] else None
    d = grid(period, phase)
    runs = runs_of(case["first"], phase, d, case["breaks"])
    places = [place(run_holding(runs, i), period, d, i) for i in range(case["count"])]
    skip = next(i for i, p in enumerate(places) if p >= case["from"])
    p0 = places[skip]
    first, new_phase = floor(p0), p0 - floor(p0)
    new_d = grid(period, new_phase or None)
    breaks = [[r[0] - skip, r[1], r[2], int(F(r[3], d) * new_d)] for r in runs[1:] if r[0] > skip]
    return {
        "first": first,
        "phase": pair(new_phase) if new_phase else None,
        "breaks": breaks,
        "count": case["count"] - skip,
        "places": [int(p * new_d) for p in places[skip:]],
        "d": new_d,
    }


TRIM_CASES = [
    (
        "a cut at 4 of a period of 3/2: sample 3, at 4.5, starts the run at tick 4, phase 1/2",
        {"period": [3, 2], "phase": None, "first": 0, "breaks": [], "count": 60, "from": 4},
    ),
    (
        "a cut at 1 of a phase of 1/2: sample 1, at 2.5, starts the channel, both runs kept",
        {"period": [2, 1], "phase": [1, 2], "first": 0, "breaks": [[2, 10, 0, 0], [4, 20, 0, 1]],
         "count": 6, "from": 1},
    ),
]


def places():
    tests = []
    for name, case, indices in PLACE_CASES:
        rows, lost = place_rows(case, indices)
        tests.append({"name": name, "channel": case, "samples": rows, "lost": lost})
    trims = [{"name": name, "channel": case, "expect": trim(case)} for name, case in TRIM_CASES]
    return {
        "note": "Generated by make-vectors.py from FORMAT.md's rules; see README.md.",
        "places": tests,
        "trims": trims,
    }


# Overview (FORMAT.md, "Overview").


def density(e):
    """1 + floor(4 log2 e), 0 for no edge: the largest r with 2^r <= e^4, plus one, which is
    e^4's bit length."""
    return 0 if e == 0 else (e**4).bit_length()


def duty(h, k):
    return 15 if k == 0 else (28 * h + k) // (2 * k)


def float_traps(limit):
    """Counts below 2^53 where a floating-point log2 lands on the wrong side of a step."""
    out = []
    for p in range(0, 53):
        for r in (1, 2, 3):
            edge = math.ceil(2 ** (p + r / 4))
            for e in range(max(1, edge - 2), edge + 3):
                if e < 2**53 and 1 + math.floor(4 * math.log2(e)) != density(e):
                    out.append(e)
    return sorted(set(out))[:limit]


def levels(channel):
    """Each bucket's (edges, K, H) at the finest level and the next, and the bytes."""
    length, shift = channel["length"], channel["shift"]
    edges, gaps = channel["edges"], channel["gaps"]
    kept = channel.get("kept")
    lo, hi = (kept[0], kept[1]) if kept else (0, length)
    # The level at each tick: initial through every edge and gap end at or before it.
    changes = sorted([(t, 0, None) for t in edges] + [(g[1], 1, g[2]) for g in gaps])
    level, at, j = channel["initial"], [], 0
    for t in range(length):
        while j < len(changes) and changes[j][0] <= t:
            _, kind, to = changes[j]
            level = (not level) if kind == 0 else to
            j += 1
        at.append(level)
    size = 1 << shift
    n = -(-length // size)
    sums = []
    for b in range(n):
        ticks = range(b * size, min((b + 1) * size, length))
        e = sum(1 for t in edges if b * size <= t < (b + 1) * size)
        k = sum(1 for t in ticks if lo <= t < hi)
        h = sum(1 for t in ticks if lo <= t < hi and at[t])
        sums.append((e, k, h))

    def coarse(sums):
        return [tuple(map(sum, zip(*sums[i:i + 16]))) for i in range(0, len(sums), 16)]

    # From the finest in steps of 4, to the first with at most 1,024 buckets: the strip.
    out = []
    while True:
        out.append({
            "density": [density(e) for e, _, _ in sums],
            "duty": [duty(h, k) for _, k, h in sums],
        })
        if len(sums) <= 1024 or shift >= 62:
            return out
        sums, shift = coarse(sums), shift + 4


def overview():
    counts = [0, 1, 2, 3, 4, 5, 6, 7, 8, 15, 16, 17, 1 << 20, (1 << 52) + 1, (1 << 53) - 1]
    traps = float_traps(4)
    dens = [{"edges": e, "expect": density(e)} for e in counts + traps]
    duties = [
        {"k": k, "h": h, "expect": duty(h, k)}
        for k, h in [(0, 0), (10, 0), (10, 10), (10, 5), (28, 1), (4, 3), (3, 1), (3, 2), (1 << 40, 1)]
    ]
    busy = {
        "note": "Two edges in bucket 0, one in bucket 1, four in bucket 10, two at one tick in "
                "bucket 15 (the level doesn't change), a gap from 100 to 110 ending high, and "
                "edges every 3 ticks from 4,000 to 4,300.",
        "length": 8192, "shift": 2, "initial": False,
        "edges": [1, 2, 5, 40, 41, 42, 43, 60, 60, 90] + list(range(4000, 4300, 3)),
        "gaps": [[100, 110, True]],
    }
    kept = {
        "note": "A channel kept from 10 to 50 beside one kept from 0: its buckets outside read 15.",
        "length": 64, "shift": 2, "initial": True, "edges": [12, 30], "gaps": [], "kept": [10, 50],
    }
    channels = []
    for c in (busy, kept):
        levels_ = levels(c)
        channels.append({**c, "expect": levels_})
    return {
        "note": "Generated by make-vectors.py from FORMAT.md's rules; see README.md.",
        "density": dens,
        "duty": duties,
        "channels": channels,
    }


# Parts (FORMAT.md, "Annotations" and "Index").

PART_TARGET = 64 << 10


def parts():
    gens = [
        {"source": "alpha", "count": 700, "first": 0, "step": 10, "text": 90},
        {"source": "beta", "count": 300, "first": 5, "step": 10, "text": 140},
    ]
    records = []
    for g in gens:
        for i in range(g["count"]):
            s = g["first"] + g["step"] * i
            line = json.dumps({"id": f"{g['source'][0]}/{i}", "start": s, "text": "x" * g["text"]},
                              separators=(",", ":"))
            records.append((s, g["source"], i, line))
    # In order of their starts; at one start, in the order of `gens`.
    order = {g["source"]: k for k, g in enumerate(gens)}
    records.sort(key=lambda r: (r[0], order[r[1]]))
    rows = []
    pending = {g["source"]: [] for g in gens}
    held = {g["source"]: 0 for g in gens}
    first_seen = []
    for s, source, i, line in records:
        if source not in first_seen:
            first_seen.append(source)
        pending[source].append((i, s))
        held[source] += len(line.encode()) + 1
        if held[source] >= PART_TARGET:
            rows.append((source, pending[source]))
            pending[source], held[source] = [], 0
    for source in first_seen:
        if pending[source]:
            rows.append((source, pending[source]))
    expect = [
        {"source": src, "first_line": lines[0][0], "lines": len(lines),
         "start": min(s for _, s in lines), "end": max(s for _, s in lines)}
        for src, lines in rows
    ]
    return {
        "note": "Generated by make-vectors.py from FORMAT.md's rules; see README.md.",
        "generators": gens,
        "expect": {
            "rows": expect,
            "top": {"first": min(r["start"] for r in expect), "number": max(r["end"] for r in expect)},
        },
    }


def write(name, value):
    (HERE / name).write_text(json.dumps(value, indent=1) + "\n")


write("places.json", places())
write("overview.json", overview())
write("parts.json", parts())
