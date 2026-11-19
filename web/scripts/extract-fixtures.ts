// Writes the dev fixtures from real captures. Run from web/:
//
//   node scripts/extract-fixtures.ts [--exports DIR] [--energy CSV] [NAME...]
//
// identify-swd goes to fixtures/ (committed); the rest to fixtures-local/ (ignored by git).
// The file layout is in src/fixture-file.ts, which reads it.

import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { gzipSync, constants } from "node:zlib";
import { parseArgs } from "node:util";
import { fileURLToPath } from "node:url";
import { parse } from "csv-parse/sync";
import { FIXTURE_FORMAT, FIXTURE_MAGIC, type FixtureChannel, type FixtureHeader } from "../src/fixture-file.ts";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// The captures are not in this repository: --exports names the folder of Saleae exports, one
// folder per capture, and --energy verge's energy CSV.
const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    exports: { type: "string" },
    energy: { type: "string" },
  },
});

const SWD = [
  ["digital_3_0", "SWCLK"],
  ["digital_3_1", "SWDIO"],
];
const JTAG = [
  ["digital_4_0", "TCK"],
  ["digital_4_1", "TDO"],
  ["digital_4_2", "TDI"],
  ["digital_4_3", "TMS"],
];
const SALEAE: { name: string; dir: string; channels: string[][] }[] = [
  { name: "identify-swd", dir: "fixtures", channels: SWD },
  { name: "gcc-swd-reads", dir: "fixtures-local", channels: SWD },
  { name: "stock-swd-reads", dir: "fixtures-local", channels: SWD },
  { name: "stock-reads", dir: "fixtures-local", channels: JTAG },
];
const ENERGY = { name: "markers-100000", dir: "fixtures-local" };

// The converter's rule (PLAN.md §2): refuse an edge further than this from its tick.
const GRID_LIMIT = 0.01;

class Bytes {
  buf = new Uint8Array(1 << 16);
  length = 0;
  push(b: number) {
    if (this.length === this.buf.length) {
      const next = new Uint8Array(this.buf.length * 2);
      next.set(this.buf);
      this.buf = next;
    }
    this.buf[this.length++] = b;
  }
  leb128(v: number) {
    if (!Number.isSafeInteger(v) || v < 0) throw new Error(`cannot code ${v}`);
    while (v >= 128) {
      this.push((v % 128) | 0x80);
      v = Math.floor(v / 128);
    }
    this.push(v);
  }
  bytes() {
    return this.buf.subarray(0, this.length);
  }
}

interface SaleaeDigital {
  initial: 0 | 1;
  rate: number;
  begin: number;
  end: number;
  times: Float64Array;
}

function readSaleae(path: string): SaleaeDigital {
  const b = readFileSync(path);
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  if (b.subarray(0, 8).toString("latin1") !== "<SALEAE>") throw new Error(`${path}: not a Saleae export`);
  const version = v.getInt32(8, true);
  const type = v.getInt32(12, true);
  const chunks = Number(v.getBigInt64(16, true));
  if (version !== 1 || type !== 0 || chunks !== 1) throw new Error(`${path}: version ${version} type ${type} chunks ${chunks}`);
  const initial = v.getUint32(24, true);
  const rate = v.getFloat64(28, true);
  const begin = v.getFloat64(36, true);
  const end = v.getFloat64(44, true);
  const count = Number(v.getBigInt64(52, true));
  if (b.length !== 60 + 8 * count) throw new Error(`${path}: ${b.length} bytes for ${count} edges`);
  const times = new Float64Array(count);
  for (let i = 0; i < count; i++) times[i] = v.getFloat64(60 + 8 * i, true);
  return { initial: initial ? 1 : 0, rate, begin, end, times };
}

function writeFixture(path: string, header: FixtureHeader, payloads: Uint8Array[]) {
  const json = new TextEncoder().encode(JSON.stringify(header));
  const total = 12 + json.length + payloads.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  out.set(new TextEncoder().encode(FIXTURE_MAGIC), 0);
  new DataView(out.buffer).setUint32(8, json.length, true);
  out.set(json, 12);
  let at = 12 + json.length;
  for (const p of payloads) {
    out.set(p, at);
    at += p.length;
  }
  const gz = gzipSync(out, { level: constants.Z_BEST_COMPRESSION });
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, gz);
  console.log(`${path}: ${total} bytes, ${gz.length} gzipped`);
}

function saleae(spec: (typeof SALEAE)[number]) {
  const dir = join(values.exports!, spec.name);
  const channels: FixtureChannel[] = [];
  const payloads: Uint8Array[] = [];
  let begin: number | undefined;
  let rate: number | undefined;
  let worst = 0;
  let length = 0;
  for (const [file, name] of spec.channels) {
    const d = readSaleae(join(dir, `${file}.bin`));
    // Ticks count from the capture's start, so every channel must share one begin and one rate.
    if (begin !== undefined && (d.begin !== begin || d.rate !== rate)) throw new Error(`${spec.name}: ${file} has another begin or rate`);
    begin = d.begin;
    rate = d.rate;
    const out = new Bytes();
    let prev = 0;
    for (let i = 0; i < d.times.length; i++) {
      const exact = (d.times[i] - d.begin) * d.rate;
      const tick = Math.round(exact);
      worst = Math.max(worst, Math.abs(exact - tick));
      if (Math.abs(exact - tick) > GRID_LIMIT) throw new Error(`${spec.name}/${file}: edge ${i} is ${exact - tick} off the grid`);
      if (i > 0 && tick <= prev) throw new Error(`${spec.name}/${file}: edge ${i} does not advance`);
      out.leb128(tick - (i > 0 ? prev : 0));
      prev = tick;
    }
    const end = Math.round((d.end - d.begin) * d.rate);
    if (d.times.length > 0 && prev >= end) throw new Error(`${spec.name}/${file}: an edge at or past the end`);
    length = Math.max(length, end);
    const p = out.bytes();
    payloads.push(p);
    channels.push({ kind: "digital", name, end, initial: d.initial, count: d.times.length, bytes: p.length });
    console.log(`  ${name} (${file}): ${d.times.length} edges, end ${end}`);
  }
  console.log(`  worst distance from the grid: ${worst} of a tick`);
  const header: FixtureHeader = {
    format: FIXTURE_FORMAT,
    title: spec.name,
    timebase: [rate!, 1],
    length,
    channels,
    source: {
      kind: "saleae-binary",
      capture: spec.name,
      files: spec.channels.map(([f]) => `${f}.bin`),
      begin,
      rule: "tick = round((t - begin) * rate)",
      worstOffGrid: worst,
    },
  };
  writeFixture(join(web, spec.dir, `${spec.name}.tickwavefx`), header, payloads);
}

// verge's energy capture CSV. A row with a sequence number is a sample (integer mV and nA, and the
// DIO levels at that sample); a row without one is a DIO edge or a marker, each at its own ns time.
function energy() {
  const rows = parse(readFileSync(values.energy!, "utf8"), { columns: true }) as Record<string, string>[];
  for (const n of ["sequence", "time_ns", "millivolts", "nanoamps", "dio3", "marker_number", "marker_label"]) {
    if (!(n in rows[0])) throw new Error(`no column ${n}`);
  }
  const mv: number[] = [];
  const na: number[] = [];
  const sampleTimes: number[] = [];
  const dio: number[] = [];
  let dioInitial: 0 | 1 | undefined;
  let dioLevel: number | undefined;
  const markers: { tick: number; number: string; label: string }[] = [];
  for (const f of rows) {
    const t = Number(f.time_ns);
    if (f.sequence !== "") {
      sampleTimes.push(t);
      mv.push(Number(f.millivolts));
      na.push(Number(f.nanoamps));
      if (dioInitial === undefined) dioInitial = dioLevel = Number(f.dio3) as 0 | 1;
    } else if (f.marker_number !== "") {
      markers.push({ tick: t, number: f.marker_number, label: f.marker_label });
    } else {
      const level = Number(f.dio3);
      if (level !== dioLevel) {
        dio.push(t);
        dioLevel = level;
      }
    }
  }
  const period = 10000;
  sampleTimes.forEach((t, i) => {
    if (t !== 5000 + period * i) throw new Error(`sample ${i} at ${t} ns is off the 100 kHz grid`);
  });
  // time_ns runs 5000, 15000, ...: the capture is taken to cover [0, count * 10 us).
  const length = sampleTimes.length * period;
  const deltas = (codes: number[]) => {
    const out = new Bytes();
    let prev = 0;
    for (const c of codes) {
      const d = c - prev;
      out.leb128(d >= 0 ? 2 * d : -2 * d - 1);
      prev = c;
    }
    return out.bytes();
  };
  const edges = new Bytes();
  dio.forEach((t, i) => edges.leb128(t - (i > 0 ? dio[i - 1] : 0)));
  const payloads = [deltas(mv), deltas(na), edges.bytes()];
  const analog = (name: string, unit: string, scale: number, codes: number[], bytes: number): FixtureChannel => ({
    kind: "analog", name, end: length, unit, scale, offset: 0, first: 5000, period: [period, 1], count: codes.length, bytes,
  });
  const header: FixtureHeader = {
    format: FIXTURE_FORMAT,
    title: ENERGY.name,
    timebase: [1e9, 1],
    length,
    channels: [
      analog("voltage", "V", 1e-3, mv, payloads[0].length),
      analog("current", "A", 1e-9, na, payloads[1].length),
      { kind: "digital", name: "DIO3", end: length, initial: dioInitial ?? 0, count: dio.length, bytes: payloads[2].length },
    ],
    source: { kind: "verge-energy-csv", file: values.energy, columns: ["millivolts", "nanoamps", "dio3"] },
  };
  console.log(`  ${mv.length} samples, ${dio.length} DIO3 edges, ${markers.length} markers`);
  writeFixture(join(web, ENERGY.dir, `${ENERGY.name}.tickwavefx`), header, payloads);
  // The markers are in the capture itself, so they are generated annotations.
  const jsonl = [
    JSON.stringify({ format: "tickwave-annotations/1", source: "markers" }),
    ...markers.map((m) =>
      JSON.stringify({ id: `marker/${m.tick}`, kind: "marker", start: m.tick, label: m.label, channels: ["voltage", "current"] }),
    ),
  ].join("\n");
  writeFileSync(join(web, ENERGY.dir, `${ENERGY.name}.markers.jsonl`), jsonl + "\n");
}

const wanted = new Set(positionals);
for (const spec of SALEAE) {
  if (wanted.size > 0 && !wanted.has(spec.name)) continue;
  if (!values.exports) {
    console.log(`${spec.name}: no --exports folder, skipped`);
    continue;
  }
  if (!existsSync(join(values.exports, spec.name))) {
    console.log(`${spec.name}: no export at ${values.exports}, skipped`);
    continue;
  }
  console.log(spec.name);
  saleae(spec);
}
if (wanted.size === 0 || wanted.has(ENERGY.name)) {
  if (!values.energy) console.log(`${ENERGY.name}: no --energy CSV, skipped`);
  else if (existsSync(values.energy)) {
    console.log(ENERGY.name);
    energy();
  } else console.log(`${ENERGY.name}: no CSV at ${values.energy}, skipped`);
}
