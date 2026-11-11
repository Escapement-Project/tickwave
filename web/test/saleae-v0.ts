// Version 0 copies of the version 1 test exports, as Logic 8, Logic Pro 8 and Logic Pro 16 write
// them: no chunk count, no rate in a digital file, no trigger in an analog one.

import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** Writes a version 0 copy of the export in `from` to the new directory `to`, and gives `to`. */
export function v0Copy(from: string, to: string): string {
  mkdirSync(to, { recursive: true });
  for (const f of readdirSync(from)) {
    const v1 = readFileSync(join(from, f));
    const kind = v1.readInt32LE(12);
    const head = Buffer.alloc(16);
    v1.copy(head, 0, 0, 8);
    head.writeInt32LE(kind, 12);
    let rest: Buffer;
    if (kind === 0) {
      // initial, begin, end, count, times
      rest = Buffer.concat([v1.subarray(24, 28), v1.subarray(36)]);
    } else {
      // begin, the rate as a u64, downsample, count, samples
      const rate = Buffer.alloc(8);
      rate.writeBigUInt64LE(BigInt(v1.readDoubleLE(40)));
      rest = Buffer.concat([v1.subarray(24, 32), rate, v1.subarray(48)]);
    }
    writeFileSync(join(to, f), Buffer.concat([head, rest]));
  }
  return to;
}
