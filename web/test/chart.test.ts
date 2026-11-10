// The chart's data, geometry and markup, on Figure 1 of the landing page (fixtures/figure1.chart.json,
// from the landing page's data). Expected positions are worked by hand from its margins: at 620 px
// wide and 330 high with end labels, the plot runs from x 52 to 556 and from y 28 to 286.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseChart, type ChartData } from "../src/chart/data.ts";
import { grouped } from "../src/chart/decimal.ts";
import { layout } from "../src/chart/layout.ts";
import { figure } from "../src/chart/readout.ts";
import { render, type Colours, type State } from "../src/chart/render.ts";

const RAW = JSON.parse(readFileSync(new URL("../fixtures/figure1.chart.json", import.meta.url), "utf8"));
const FIG1 = parseChart(RAW);
const SHOWN = new Set(["clang", "stock"]);
/** The 2026-11-04 request-clock chart, with NXP's failures marked. */
const WIRE = parseChart(JSON.parse(readFileSync(new URL("../fixtures/wire.chart.json", import.meta.url), "utf8")));
const WIRE_SHOWN = new Set(["gcc", "capture", "stock"]);

const COLOURS: Colours = {
  series: (name) => ({ value: ({ verge: "#0f8a6c", nxp: "#2a78d6" })[name] ?? "#000000", css: `var(--series-${name})` }),
  dash: (p) => ({ solid: "", "second-build": "6 3", reference: "8 3 2 3", "capture-second": "12 4", misread: "2 4" })[p],
  fault: { value: "#b81c20", css: "var(--fault-ink)" },
  captureWidth: 1.5,
  captureOpacity: 0.6,
  fontText: "sans-serif",
};

const AT_REST: State = { x: null, series: null, limit: false, rings: [], sweep: null, labels: null, endLabels: true, xName: "SWD clock asked for, MHz", yUnit: "KiB/s" };

const near = (actual: number, expected: number, why: string) => assert.ok(Math.abs(actual - expected) < 0.05, `${why}: ${actual}, not ${expected}`);
const log50 = (v: number) => 52 + (504 * Math.log(v)) / Math.log(50);

test("grouping keeps every digit", () => {
  assert.equal(grouped("1238.6"), "1,238.6");
  assert.equal(grouped("81.0"), "81.0");
  assert.equal(grouped("1234567"), "1,234,567");
  assert.equal(grouped("-1234.50"), "-1,234.50");
  assert.equal(grouped("0.0001"), "0.0001");
  assert.equal(grouped("999"), "999");
});

test("the data: Figure 1 parses, and every problem in a bad chart is reported", () => {
  assert.deepEqual(FIG1.x.values, ["1", "2", "5", "10", "20", "30", "50"]);
  const stock = FIG1.series.find((s) => s.id === "stock")!;
  assert.equal(stock.points[4]!.value, "666.7");
  assert.equal(stock.points[5]!.failed, true);
  assert.equal(stock.points[6]!.wrong, true);
  assert.deepEqual([stock.points[6]!.value, stock.points[6]!.partial], [null, { failed: 1, of: 2, value: "744.6" }]);
  assert.equal(stock.markFailures, false);
  assert.equal(FIG1.series.find((s) => s.id === "gcc")!.hidden, true);

  const bad = structuredClone(RAW);
  bad.format = "tickwave-chart/2";
  bad.x.values[3] = "4";
  bad.series[0].points.pop();
  bad.series[1].pattern = "zigzag";
  bad.series[1].points[0] = { value: "76.7", partial: { failed: 1, of: 2, value: "70.0" } };
  bad.series[1].points[1] = { value: 148.1 };
  bad.x.min = "1.5";
  bad.series[2].id = 'gcc"><img src=x>';
  bad.x.max = 62.5;
  assert.throws(
    () => parseChart(bad),
    (e: Error) => {
      assert.equal(e.name, "TickwaveChartError");
      for (const needle of [
        'format: "tickwave-chart/2", not "tickwave-chart/1"',
        "x.values[3]: not above the one before",
        "series[0].points: not one a setting (7)",
        'series[1].pattern: "zigzag" is not one of',
        "series[1].points[0]: a partial point has no value",
        "series[1].points[1].value: 148.1 is not a decimal string",
        "x.min: above the first setting",
        "x.max: 62.5 is not a decimal string",
        `series[2].id: "gcc\\"><img src=x>" is not letters, digits, ".", "_" and "-"`,
      ]) {
        assert.ok(e.message.includes(needle), `no ${needle} in:\n${e.message}`);
      }
      return true;
    },
  );
});

test("the axes: settings placed on the log scale, labels thinned by distance, the fixed y ticks", () => {
  const l = layout(FIG1, SHOWN, 620, 330, 40);
  assert.deepEqual([l.left, l.right, l.top, l.bottom], [52, 556, 28, 286]);
  [1, 2, 5, 10, 20, 30, 50].forEach((v, i) => near(l.xs[i], log50(v), `${v} MHz`));
  assert.deepEqual(l.xLabels.map((t) => t.text), ["1", "2", "5", "10", "20", "30", "50"]);
  assert.deepEqual(l.yTicks.map((t) => t.text), ["0", "500", "1,000", "1,500", "2,000", "2,500"]);
  near(l.yTicks[5].y, 28, "2,500 at the top");
  near(l.yTicks[0].y, 286, "0 at the bottom");

  // At 320 px the plot is 204 wide: 30 MHz would sit 21.2 px after 20 MHz, so its label goes.
  const narrow = layout(FIG1, SHOWN, 320, 330, 40);
  assert.deepEqual(narrow.xLabels.map((t) => t.text), ["1", "2", "5", "10", "20", "50"]);

  // Each setting owns the band halfway to its neighbours.
  near(l.bands[4][0], (log50(10) + log50(20)) / 2, "20 MHz's band starts");
  near(l.bands[4][1], (log50(20) + log50(30)) / 2, "20 MHz's band ends");
  assert.equal(l.bands[0][0], 52);
  assert.equal(l.bands[6][1], 556);
});

test("the x axis reaches past the settings to its given ends, as the measurements page pads it", () => {
  const padded = parseChart({ ...RAW, x: { ...RAW.x, min: "0.8", max: "62.5" } });
  assert.deepEqual([padded.x.min, padded.x.max], ["0.8", "62.5"]);
  const l = layout(padded, SHOWN, 620, 330, 40);
  // log(v / 0.8) / log(62.5 / 0.8) of the way across the plot, from 52 to 556.
  const at = (v: number) => 52 + (504 * Math.log(v / 0.8)) / Math.log(62.5 / 0.8);
  [1, 2, 5, 10, 20, 30, 50].forEach((v, i) => near(l.xs[i], at(v), `${v} MHz`));
  assert.ok(l.xs[0] > 52 && l.xs[6] < 556, "the first and last points sit inside the plot");
  // The edges still pick the first and last settings, and the limit sits at its setting.
  assert.deepEqual([l.bands[0][0], l.bands[6][1]], [52, 556]);
  near(l.limits[0].x, at(20), "the limit at 20 MHz");
  // One setting with ends given sits on the scale, not in the middle.
  const one = parseChart({ ...RAW, x: { ...RAW.x, values: ["20"], min: "10", max: "40" }, series: RAW.series.map((s: { points: unknown[] }) => ({ ...s, points: [s.points[4]] })) });
  near(layout(one, SHOWN, 620, 330, 40).xs[0], 52 + 504 * (Math.log(2) / Math.log(4)), "20 between 10 and 40");
  assert.throws(() => parseChart({ ...RAW, x: { ...RAW.x, min: "0" } }), /x\.min: not above 0 on a log scale/);
  assert.throws(() => parseChart({ ...RAW, x: { ...RAW.x, max: "40" } }), /x\.max: below the last setting/);
});

test("without a fixed top, y takes d3's steps of 1, 2 or 5", () => {
  const data: ChartData = { ...FIG1, y: { ...FIG1.y, max: null, step: null } };
  // Clang's 2,257.8 is the top figure: 2,348.1 with 4 % room, which d3 rounds out to 2,500.
  assert.deepEqual(layout(data, SHOWN, 620, 330, 40).yTicks.map((t) => t.text), ["0", "500", "1,000", "1,500", "2,000", "2,500"]);
  // NXP alone tops out at 666.7: 693.4 with room, rounded to 800, in steps of 200. Its partial
  // 744.6 at 50 MHz draws no point, so it takes no room.
  assert.deepEqual(layout(data, new Set(["stock"]), 620, 330, 40).yTicks.map((t) => t.text), ["0", "200", "400", "600", "800"]);
});

/** Figure 1, or another chart, with one series' point at a setting replaced. */
const withPoint = (id: string, i: number, point: object, base: ChartData = FIG1): ChartData => ({
  ...base,
  series: base.series.map((s) => (s.id === id ? { ...s, points: s.points.map((p, j) => (j === i ? { ...p!, ...point } : p)) } : s)),
});

test("a failed setting breaks the line, the segment into a point read wrong is dotted, and only marked series get fail marks", () => {
  const l = layout(FIG1, SHOWN, 620, 330, 40);
  const stock = l.series.find((s) => s.series.id === "stock")!;
  assert.deepEqual(stock.placed.map((p) => p.index), [0, 1, 2, 3, 4]);
  // NXP failed at 30 MHz and in one of two runs at 50: its line ends at 20, and 50 has no point.
  assert.deepEqual(stock.segments.map((s) => [s.from.index, s.to.index, s.wrong]), [
    [0, 1, false],
    [1, 2, false],
    [2, 3, false],
    [3, 4, false],
  ]);
  const at = render(l, AT_REST, COLOURS).match(/<g class="tw-series" data-series="stock">(.*?)<\/g>/)![1];
  assert.equal((at.match(/<path /g) ?? []).length, 1, "one solid path, 1 to 20 MHz");
  // Had both runs at 50 MHz given a figure, read wrong, it would stand alone and hollow.
  const wrong50 = withPoint("stock", 6, { value: "744.6", partial: null });
  const alone = render(layout(wrong50, SHOWN, 620, 330, 40), AT_REST, COLOURS).match(/<g class="tw-series" data-series="stock">(.*?)<\/g>/)![1];
  assert.equal((alone.match(/class="tw-hollow"/g) ?? []).length, 1, "50 MHz, alone and hollow");
  assert.equal((alone.match(/<path /g) ?? []).length, 1, "still one path, 1 to 20 MHz");
  // Had 30 MHz read right too, the segment into 50 would be dotted.
  const read = layout(withPoint("stock", 5, { failed: false, value: "700.0" }, wrong50), SHOWN, 620, 330, 40);
  assert.deepEqual(read.series.find((s) => s.series.id === "stock")!.segments.at(-1)!.wrong, true);
  assert.match(render(read, AT_REST, COLOURS), /<path d="M\S+ \S+L\S+ \S+" fill="none" stroke="#2a78d6" style="stroke:var\(--series-nxp\)" stroke-width="2" stroke-dasharray="2 4"\/>/);
  // A setting without a value inside a solid run splits its path in two, rather than joining
  // across it: failed, partly failed, or never measured.
  for (const gap of [{ failed: true, value: null }, { partial: { failed: 1, of: 2, value: "740.0" }, value: null }]) {
    const split = layout(withPoint("clang", 3, gap), SHOWN, 620, 330, 40);
    const clang = render(split, AT_REST, COLOURS).match(/<g class="tw-series" data-series="clang">(.*?)<\/g>/)![1];
    assert.deepEqual([...clang.matchAll(/<path d="([^"]*)"/g)].map((m) => (m[1].match(/M/g) ?? []).length), [1, 1]);
  }
  const unmeasured: ChartData = { ...FIG1, series: FIG1.series.map((s) => (s.id === "clang" ? { ...s, points: s.points.map((p, j) => (j === 3 ? null : p)) } : s)) };
  const joins = layout(unmeasured, SHOWN, 620, 330, 40).series.find((s) => s.series.id === "clang")!.segments;
  assert.deepEqual(joins.map((s) => [s.from.index, s.to.index]), [[0, 1], [1, 2], [4, 5], [5, 6]]);
  assert.deepEqual(l.fails, []);
  const marked: ChartData = { ...FIG1, series: FIG1.series.map((s) => (s.id === "stock" ? { ...s, markFailures: true } : s)) };
  assert.deepEqual(layout(marked, SHOWN, 620, 330, 40).fails.map((f) => [f.text, Math.round(f.x)]), [
    ["NXP failed", Math.round(log50(30))],
    ["NXP failed", 556],
  ]);

  const svg = render(l, AT_REST, COLOURS);
  assert.ok(!svg.includes("url(#"), "nothing refers to an id");
  assert.equal((svg.match(/class="tw-hollow"/g) ?? []).length, 0, "no point at rest reads wrong");
  assert.equal((svg.match(/class="tw-pt"/g) ?? []).length, 7 + 5, "Clang's seven points and NXP's five read right");
  assert.ok(svg.includes('stroke="#0f8a6c"'), "Verge's teal as an attribute");
  // Where the page's CSS applies, each data colour is its token, which follows the face.
  assert.ok(svg.includes('stroke="#0f8a6c" style="stroke:var(--series-verge)"'), "a line's token beside its colour");
  assert.ok(svg.includes('class="tw-pt" fill="#0f8a6c" style="fill:var(--series-verge)"'), "a point's token beside its colour");
  assert.ok(render(layout(marked, SHOWN, 620, 330, 40), AT_REST, COLOURS).includes('stroke="#b81c20" style="stroke:var(--fault-ink)"'), "a failure's mark too");
});

test("end labels that would overlap stand apart, inside the plot, and the margin fits the widest", () => {
  const l = layout(WIRE, WIRE_SHOWN, 620, 330, 100);
  assert.equal(l.right, 620 - (14 + 100 + 4));
  assert.equal(layout(WIRE, WIRE_SHOWN, 620, 330, 30).right, 620 - 64, "never narrower than before");
  const label = (id: string) => l.series.find((s) => s.series.id === id)!.label!;
  // Verge's two runs both end at 50.0 MHz: the first in the data keeps its place, the other moves a line down.
  const end = l.series.find((s) => s.series.id === "gcc")!.placed.at(-1)!;
  assert.deepEqual(label("gcc"), { x: end.x + 14, y: end.y });
  assert.deepEqual(label("capture"), { x: end.x + 14, y: end.y + 14 });
  near(label("stock").y, l.y(18.4), "NXP's, alone, at its point");
  assert.ok(render(l, AT_REST, COLOURS).includes(`y="${Math.round((end.y + 18) * 10) / 10}" fill="currentColor" font-weight="600">Verge, capturing</text>`));
  // At the plot's bottom, labels move up rather than out of it.
  const flat = parseChart({
    format: "tickwave-chart/1",
    x: { scale: "linear", values: ["1", "2"] },
    y: { scale: "linear", max: "10", step: "5" },
    series: ["a", "b"].map((id) => ({ id, name: id, colour: "verge", points: [{ value: "5" }, { value: "0" }] })),
  });
  const low = layout(flat, new Set(["a", "b"]), 620, 330, 40);
  assert.deepEqual(low.series.map((s) => s.label!.y), [low.bottom - 14, low.bottom]);
});

test("a failure's mark names its point, so a failure can name its recording", () => {
  const l = layout(WIRE, WIRE_SHOWN, 620, 330, 100);
  assert.deepEqual(
    l.fails.map((f) => [f.text, f.index, f.point.evidence?.annotation]),
    [["NXP failed", 1, "dpidr-request"]],
  );
  assert.equal(l.fails[0].x, l.xs[1]);
});

test("the limit grows from its series' point as the sweep crosses it", () => {
  const l = layout(FIG1, SHOWN, 620, 330, 40);
  const origin = 286 - (666.7 / 2500) * 258;
  near(l.limits[0].origin!, origin, "NXP's 20 MHz point");
  near(l.limits[0].reach!, 556, "the last setting, though NXP's line ends at 20 MHz");
  const limit = (sweep: number | null) => render(l, { ...AT_REST, sweep }, COLOURS).match(/class="tw-limit"[^>]*y1="([\d.]+)" y2="([\d.]+)"/);
  assert.equal(limit(log50(20) - 1), null, "not before the sweep reaches it");
  const half = (log50(20) + 556) / 2;
  const m = limit(half)!;
  near(Number(m[1]), origin - (origin - 28) / 2, "halfway up");
  near(Number(m[2]), origin + (286 - origin) / 2, "halfway down");
  const whole = limit(null)!;
  assert.deepEqual([whole[1], whole[2]], ["28", "286"]);
});

test("the sweep cuts the lines where it is, and shows whole markers only", () => {
  const l = layout(FIG1, SHOWN, 620, 330, 40);
  const svg = render(l, { ...AT_REST, sweep: 300, labels: 0 }, COLOURS);
  const clang = svg.match(/<g class="tw-series" data-series="clang">(.*?)<\/g>/)![1];
  assert.match(clang, /L300 /, "Clang's line ends at the sweep");
  assert.equal((clang.match(/<circle/g) ?? []).length, 3, "1, 2 and 5 MHz, before 300");
  assert.ok(!clang.includes("tw-end"), "no end label until the lines land");
  const labels = render(l, { ...AT_REST, labels: 0.5 }, COLOURS);
  assert.ok(labels.includes(">Ver</text>"), "half of Verge's five characters, rounded up");
  assert.ok(render(l, AT_REST, COLOURS).includes(">Verge</text>"));
});

test("a readout figure keeps its digits and says what is known", () => {
  const p = (o: object) => ({ value: null, wrong: false, failed: false, partial: null, range: null, ring: false, note: null, evidence: null, ...o });
  assert.equal(figure(p({ value: "1238.6" }), "KiB/s"), "1,238.6 KiB/s");
  assert.equal(figure(p({ value: "744.6", wrong: true }), "KiB/s"), "744.6 KiB/s, reads wrong");
  assert.equal(figure(p({ value: "81.0", range: ["80.4", "81.6"] }), "KiB/s"), "81.0 KiB/s (80.4 to 81.6)");
  assert.equal(figure(p({ value: "37.5", note: "from SWDIO" }), "MHz"), "37.5 MHz, from SWDIO");
  assert.equal(figure(p({ partial: { failed: 1, of: 2, value: "1180.5" } }), "KiB/s"), "failed in 1 of 2 runs (1,180.5 KiB/s in the other)");
  assert.equal(figure(p({ failed: true }), "KiB/s"), "failed");
  assert.equal(figure(p({ value: "18.4", note: "the read failed partway" }), "MHz"), "18.4 MHz, the read failed partway");
  assert.equal(figure(p({ failed: true, note: "under the leads" }), "MHz"), "failed, under the leads");
  assert.equal(figure(p({ partial: { failed: 1, of: 3, value: "740.0" }, note: "x" }), "KiB/s"), "failed in 1 of 3 runs (740.0 KiB/s in the others), x");
  assert.equal(figure(p({ note: "not recorded" }), "MHz"), "not recorded");
  assert.equal(figure(null, "KiB/s"), "–");
});

test("the left margin makes room for the widest y label and the unit above them", () => {
  const cliff = parseChart({
    format: "tickwave-chart/1",
    x: { scale: "linear", values: ["1", "2"] },
    y: { unit: "commands/s", scale: "linear", max: "60000", step: "20000" },
    series: [{ id: "a", name: "A", colour: "verge", points: [{ value: "10000" }, { value: "50000" }] }],
  });
  // Seven pixels a character, the estimate without a measure: "commands/s" is 70, "60,000" 42.
  const l = layout(cliff, new Set(["a"]), 620, 330, null);
  assert.equal(l.left, 70 + 8 + 4);
  const svg = render(l, { ...AT_REST, yUnit: "commands/s" }, COLOURS);
  const x = Number(/<text x="([\d.]+)" y="[\d.]+" text-anchor="end"[^>]*>commands\/s</.exec(svg)![1]);
  assert.ok(x - 70 >= 0, `the unit starts at ${x - 70}, left of the SVG`);
  // Figure 1's labels fit the margin it always had.
  assert.equal(layout(FIG1, SHOWN, 620, 330, 40).left, 52);
});
