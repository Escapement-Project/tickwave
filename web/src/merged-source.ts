// Several recordings on one time axis: the merge's preview, drawn from each input as converted.
// The axis ticks at the least common multiple of the inputs' tick rates, so every input tick is a
// whole number of merged ticks, its factor. Every input starts at tick 0; a view shaping the
// preview moves each one by its alignment. Nothing is resampled: every edge and sample keeps its
// tick, times the factor.

import { channelStart, type Channel, type Edges, type Overview, type Ratio, type RecordingInfo, type Samples, type Source } from "./source.ts";

export interface MergeInput {
  readonly source: Source;
  readonly info: RecordingInfo;
  /** The input's file name without its extension, which prefixes a channel name another input took. */
  readonly stem: string;
}

/** Where a merged channel comes from. */
interface Origin {
  readonly input: number;
  readonly channel: number;
}

function gcd(a: bigint, b: bigint): bigint {
  while (b !== 0n) [a, b] = [b, a % b];
  return a;
}

/** The least common multiple of tick rates, in lowest terms: every rate divides it a whole number
 * of times. Throws where a term passes 2^53. */
export function mergedTimebase(rates: readonly Ratio[]): Ratio {
  let num = 1n;
  let den = 0n;
  for (const r of rates) {
    const g = gcd(BigInt(r.num), BigInt(r.den));
    const [n, d] = [BigInt(r.num) / g, BigInt(r.den) / g];
    num = (num / gcd(num, n)) * n;
    den = gcd(den, d);
  }
  if (num > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError(`the inputs' tick rates have no common rate below 2^53 ticks a second`);
  return { num: Number(num), den: Number(den) };
}

/** Merged ticks a tick of `rate` spans, on `merged`. */
function factorOf(merged: Ratio, rate: Ratio): number {
  // merged / rate = (merged.num rate.den) / (merged.den rate.num), a whole number by construction.
  const n = BigInt(merged.num) * BigInt(rate.den);
  const d = BigInt(merged.den) * BigInt(rate.num);
  if (n % d !== 0n) throw new RangeError(`${merged.num}/${merged.den} is not a multiple of ${rate.num}/${rate.den}`);
  return Number(n / d);
}

/** Each channel's name, a later input's clashing one prefixed with its stem, numbered on if that
 * clashes too. */
function uniqueNames(inputs: readonly MergeInput[]): string[] {
  const taken = new Set<string>();
  const names: string[] = [];
  for (const input of inputs) {
    for (const c of input.info.channels) {
      let name = c.name;
      for (let n = 1; taken.has(name); n++) name = `${n === 1 ? input.stem : `${input.stem}-${n}`}/${c.name}`;
      taken.add(name);
      names.push(name);
    }
  }
  return names;
}

export class MergedSource implements Source {
  readonly #inputs: readonly MergeInput[];
  readonly #origins: readonly Origin[];
  readonly #names: readonly string[];
  readonly #factors: readonly number[];
  readonly #timebase: Ratio;
  readonly #info: RecordingInfo;

  constructor(inputs: readonly MergeInput[]) {
    if (inputs.length === 0) throw new RangeError("a merge needs an input");
    this.#inputs = inputs;
    this.#origins = inputs.flatMap((input, i) => input.info.channels.map((_, c) => ({ input: i, channel: c })));
    this.#names = uniqueNames(inputs);
    this.#timebase = mergedTimebase(inputs.map((i) => i.info.timebase));
    this.#factors = inputs.map((i) => factorOf(this.#timebase, i.info.timebase));
    this.#info = this.#merged();
  }

  /** Merged ticks one tick of each input spans. */
  get factors(): readonly number[] {
    return this.#factors;
  }

  /** The input and channel index merged channel `channel` comes from. */
  origin(channel: number): Origin {
    return this.#origins[channel];
  }

  get mergedInfo(): RecordingInfo {
    return this.#info;
  }

  #merged(): RecordingInfo {
    // Each input's sources, restated in merged ticks; an input that is itself merged has several.
    const all = this.#inputs.every((i) => i.info.sources?.length);
    const first: number[] = [];
    const sources = this.#inputs.flatMap((input, i) => {
      first.push(first.length === 0 ? 0 : first[i - 1] + (this.#inputs[i - 1].info.sources?.length ?? 1));
      const f = this.#factors[i];
      return (input.info.sources ?? []).map((s) => ({ ...s, origin: s.origin * f, ...(s.align && { align: s.align * f }) }));
    });
    // Each input's names, as the merge gives them, for its derived channels' operands.
    const renamed = this.#inputs.map(() => new Map<string, string>());
    this.#origins.forEach(({ input, channel }, i) => renamed[input].set(this.#inputs[input].info.channels[channel].name, this.#names[i]));
    const channels = this.#origins.map(({ input, channel }, i): Channel => {
      const c = this.#inputs[input].info.channels[channel];
      const f = this.#factors[input];
      const name = this.#names[i];
      const source = first[input] + (c.source ?? 0);
      if (c.kind === "digital") return { ...c, name, source, start: channelStart(c) * f, end: c.end * f };
      const product = c.product?.map((n) => renamed[input].get(n) ?? n) as [string, string] | undefined;
      return { ...c, name, source, first: c.first * f, period: { num: c.period.num * f, den: c.period.den }, end: c.end * f, ...(product && { product }) };
    });
    return {
      title: this.#inputs.map((i) => i.info.title).join(" + "),
      timebase: this.#timebase,
      length: Math.max(1, ...channels.map((c) => c.end)),
      channels,
      ...(all && { sources }),
    };
  }

  info(): Promise<RecordingInfo> {
    return Promise.resolve(this.#info);
  }

  async prefetch(): Promise<void> {
    await Promise.all(this.#inputs.map((i) => i.source.prefetch()));
  }

  /** `[start, end)` on the input's own ticks, covering every input tick the merged span touches. */
  #span(channel: number, start: number, end: number): { input: MergeInput; c: number; s: number; e: number; f: number } {
    const { input, channel: c } = this.#origins[channel];
    const f = this.#factors[input];
    const s = Math.max(0, Math.floor(start / f));
    return { input: this.#inputs[input], c, s, e: Math.max(s, Math.ceil(end / f)), f };
  }

  async edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    const { input, c, s, e, f } = this.#span(channel, start, end);
    const got = await input.source.edges(c, s, e, signal);
    const ticks = got.ticks.map((t) => t * f);
    // An input tick before `start` can scale to one inside the merged span's first tick, and the
    // level at `start` then counts the edges before it.
    let lo = 0;
    while (lo < ticks.length && ticks[lo] < start) lo++;
    let hi = ticks.length;
    while (hi > lo && ticks[hi - 1] >= end) hi--;
    return { level: (got.level ^ (lo & 1)) as 0 | 1, ticks: ticks.subarray(lo, hi) };
  }

  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<Overview | null> {
    const { input, c, s, e, f } = this.#span(channel, start, end);
    const got = await input.source.overview(c, s, e, Math.floor(maxBucket / f), signal);
    return got && { ...got, bucket: got.bucket * f, start: got.start * f };
  }

  samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    // Samples go by index: the merged channel's `first` and `period` place them.
    const { input, c, s, e } = this.#span(channel, start, end);
    return input.source.samples(c, s, e, signal);
  }
}
