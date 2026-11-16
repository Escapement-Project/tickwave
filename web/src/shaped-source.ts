// A recording with each channel moved along the time axis by its own offset, in whole ticks: the
// convert flow's preview of a move, drawn from the recording already converted. Times are the
// capture's own, so a channel moved before the capture's start reaches below tick 0, and nothing
// else moves with it. Nothing is resampled: every edge and sample keeps its tick, plus the offset.

import { listedOnAxis, mapOf, type Channel, type Edges, type Overview, type RecordingInfo, type Samples, type Source } from "./source.ts";

export class ShapedSource implements Source {
  readonly #inner: Source;
  readonly #base: RecordingInfo;
  #offsets: number[];
  #info: RecordingInfo;

  constructor(inner: Source, info: RecordingInfo) {
    this.#inner = inner;
    this.#base = info;
    this.#offsets = info.channels.map(() => 0);
    this.#info = info;
  }

  /** The recording as converted, which this one moves. */
  get inner(): Source {
    return this.#inner;
  }

  /** Each channel's offset in ticks, in channel order. */
  get offsets(): readonly number[] {
    return this.#offsets;
  }

  /** Moves channel `channel` to `ticks` from where the capture has it. */
  setOffset(channel: number, ticks: number): void {
    if (!Number.isSafeInteger(ticks)) throw new RangeError(`an offset is a whole number of ticks, not ${ticks}`);
    this.#offsets = this.#offsets.map((d, i) => (i === channel ? ticks : d));
    this.#info = shaped(this.#base, this.#offsets);
  }

  /** The recording as moved: each channel's data where its offset puts it, and the length to the
   * end of the last. */
  get shapedInfo(): RecordingInfo {
    return this.#info;
  }

  info(): Promise<RecordingInfo> {
    return Promise.resolve(this.#info);
  }

  prefetch(): Promise<void> {
    return this.#inner.prefetch();
  }

  /** `[start, end)` in the capture's ticks, kept to where the channel's data can be. */
  #span(channel: number, start: number, end: number): [number, number, number] {
    const d = this.#offsets[channel];
    const s = Math.max(0, start - d);
    return [s, Math.max(s, end - d), d];
  }

  async edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    const [s, e, d] = this.#span(channel, start, end);
    const got = await this.#inner.edges(channel, s, e, signal);
    if (d === 0) return got;
    const listed = listedOnAxis(got.listed, (t) => t + d);
    return { level: got.level, ticks: got.ticks.map((t) => t + d), ...(listed && { listed }) };
  }

  async edgeCount(channel: number, start: number, end: number, signal?: AbortSignal): Promise<number> {
    const [s, e] = this.#span(channel, start, end);
    if (this.#inner.edgeCount) return this.#inner.edgeCount(channel, s, e, signal);
    return (await this.#inner.edges(channel, s, e, signal)).ticks.length;
  }

  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<Overview | null> {
    const [s, e, d] = this.#span(channel, start, end);
    const got = await this.#inner.overview(channel, s, e, maxBucket, signal);
    if (!got || d === 0) return got;
    const listed = listedOnAxis(got.listed, (t) => t + d);
    return { ...got, start: got.start + d, ...(listed && { listed }) };
  }

  async samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    // Samples go by index: the moved channel's `first` places them.
    const [s, e, d] = this.#span(channel, start, end);
    const got = await this.#inner.samples(channel, s, e, signal);
    if (d === 0 || !got.listed) return got;
    return { ...got, listed: listedOnAxis(got.listed, (t) => t + d) };
  }
}

/** `info` with each channel moved by its offset. */
export function shaped(info: RecordingInfo, offsets: readonly number[]): RecordingInfo {
  const channels = info.channels.map((c, i): Channel => {
    const d = offsets[i] ?? 0;
    // A channel this version can't read has nothing to move.
    if (d === 0 || c.kind === "unavailable") return c;
    // Its source's ticks stay its own: the source's tick 0 moves. A digital channel's other ticks
    // are on the axis already; its map only places a stated kept span, where it has one.
    const map = mapOf(c);
    const moved = { ...map, origin: map.origin + d };
    if (c.kind === "digital") return { ...c, start: (c.start ?? 0) + d, end: c.end + d, ...(c.map && { map: moved }) };
    return { ...c, map: moved, end: c.end + d };
  });
  return { ...info, channels, length: Math.max(info.length, ...channels.map((c) => c.end)) };
}
