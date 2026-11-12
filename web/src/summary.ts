// What a closed view says about its recording, and how far an open one spans: both follow the
// channels the view shows. No DOM, so the wording can be tested without a page.

import { formatBrief, formatBytes } from "./readouts.ts";
import type { RecordingInfo } from "./source.ts";

/**
 * The ticks a view of `shown` channels spans: the recording's length with all of them, else as far
 * as the furthest of them ends.
 */
export function shownExtent(info: RecordingInfo, shown: readonly number[]): number {
  if (new Set(shown).size === info.channels.length) return info.length;
  const end = Math.max(0, ...shown.map((i) => info.channels[i].end));
  return end > 0 ? end : info.length;
}

/**
 * The summary's facts by part name: "524 ms", "2 channels: SWCLK, SWDIO", "4.7 KiB". A view of some
 * of the channels names those, out of the recording's, "2 of 3 channels: TCKC, TMSC", and gives the
 * length they span; the size is still the file's.
 */
export function summaryFacts(info: RecordingInfo, shown?: readonly number[]): [part: string, text: string][] {
  const all = info.channels.map((_, i) => i);
  const indices = shown && shown.length > 0 ? [...new Set(shown)] : all;
  const names = indices.map((i) => info.channels[i].name);
  const n = info.channels.length;
  const count = indices.length === n ? `${n} channel${n === 1 ? "" : "s"}` : `${indices.length} of ${n} channels`;
  const facts: [string, string][] = [
    ["fact-length", formatBrief(shownExtent(info, indices), info.timebase)],
    ["fact-channels", `${count}: ${names.join(", ")}`],
  ];
  if (info.bytes !== undefined) facts.push(["fact-size", formatBytes(info.bytes)]);
  return facts;
}
