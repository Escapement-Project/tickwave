// Prints a channel's edges in [start, end) of a .tickwave file as JSON, read through the WASM
// decoder in Node. The browser tests compute expected readings from it in a process of their own.
//
//   node scripts/edges.ts FILE.tickwave CHANNEL START END

import { openTickwave } from "./node-wasm.ts";

const [file, channel, start, end] = process.argv.slice(2);
const r = await openTickwave(file);
const e = await r.edges(Number(channel), Number(start), Number(end));
console.log(JSON.stringify({ level: e.level, ticks: [...e.ticks] }));
