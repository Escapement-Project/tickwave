// Prints the names of the sidecar files a .tickwave file bundles as JSON, read through the WASM
// decoder in Node: an empty list for one that bundles none. The browser tests read a saved
// recording's bundle in a process of their own.
//
//   node scripts/bundled.ts FILE.tickwave

import { openTickwave } from "./node-wasm.ts";

const files = await (await openTickwave(process.argv[2])).bundle();
console.log(JSON.stringify(Object.keys(files ?? {}).sort()));
