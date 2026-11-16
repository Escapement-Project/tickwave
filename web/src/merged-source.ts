// Several recordings on one time axis: the merge's preview, drawn from each input as converted.
// The axis ticks at the least common multiple of the inputs' tick rates, or at the fastest of them
// where the multiple has a term past 2^53, as `tickwave merge` chooses it. Each input's tick `t`
// lies at merged tick `floor(t × q)`, `q` the merged rate over the input's, a whole number at the
// multiple. Every input starts at tick 0; a view shaping the preview moves each one by its
// alignment. Nothing is resampled: every edge and sample keeps its tick, mapped.

import { channelStart, listedOnAxis, mapOf, mulDivFloor, type Channel, type Edges, type Overview, type Ratio, type RecordingInfo, type Samples, type Source } from "./source.ts";

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

const SAFE = BigInt(Number.MAX_SAFE_INTEGER);

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
  if (num > SAFE) throw new RangeError(`the inputs' tick rates have no common rate below 2^53 ticks a second`);
  return { num: Number(num), den: Number(den) };
}

/** The merge's axis for `rates`: their least common multiple, or the fastest of them where the
 * multiple has a term past 2^53. */
export function mergeAxis(rates: readonly Ratio[]): Ratio {
  try {
    return mergedTimebase(rates);
  } catch {
    const faster = (a: Ratio, b: Ratio) => BigInt(a.num) * BigInt(b.den) > BigInt(b.num) * BigInt(a.den);
    const fastest = rates.reduce((a, b) => (faster(b, a) ? b : a));
    const g = gcd(BigInt(fastest.num), BigInt(fastest.den));
    return { num: Number(BigInt(fastest.num) / g), den: Number(BigInt(fastest.den) / g) };
  }
}

/** `a / b` in lowest terms, exactly. */
function quotient(a: Ratio, b: Ratio): Ratio {
  const [n, d] = [BigInt(a.num) * BigInt(b.den), BigInt(a.den) * BigInt(b.num)];
  const g = gcd(n, d);
  if (n / g > SAFE || d / g > SAFE) throw new RangeError(`${a.num}/${a.den} over ${b.num}/${b.den} has a term past 2^53`);
  return { num: Number(n / g), den: Number(d / g) };
}

/** `a × b` in lowest terms, exactly. */
function product(a: Ratio, b: Ratio): Ratio {
  const [n, d] = [BigInt(a.num) * BigInt(b.num), BigInt(a.den) * BigInt(b.den)];
  const g = gcd(n, d);
  if (n / g > SAFE || d / g > SAFE) throw new RangeError(`${a.num}/${a.den} times ${b.num}/${b.den} has a term past 2^53`);
  return { num: Number(n / g), den: Number(d / g) };
}

/** `floor(t × q)`, exactly. */
function scaled(t: number, q: Ratio): number {
  return mulDivFloor(t, q.num, q.den);
}

/** The least input tick `t` with `floor(t × q)` at or past merged tick `x`: `ceil(x / q)`. */
function inputAtOrAfter(x: number, q: Ratio): number {
  return -scaled(-x, { num: q.den, den: q.num });
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
  readonly #ratios: readonly Ratio[];
  readonly #timebase: Ratio;
  readonly #info: RecordingInfo;

  constructor(inputs: readonly MergeInput[]) {
    if (inputs.length === 0) throw new RangeError("a merge needs an input");
    this.#inputs = inputs;
    this.#origins = inputs.flatMap((input, i) => input.info.channels.map((_, c) => ({ input: i, channel: c })));
    this.#names = uniqueNames(inputs);
    this.#timebase = mergeAxis(inputs.map((i) => i.info.timebase));
    this.#ratios = inputs.map((i) => quotient(this.#timebase, i.info.timebase));
    this.#info = this.#merged();
  }

  /** Merged ticks one tick of each input spans: a whole number at the least common multiple. */
  get ratios(): readonly Ratio[] {
    return this.#ratios;
  }

  /** The input and channel index merged channel `channel` comes from. */
  origin(channel: number): Origin {
    return this.#origins[channel];
  }

  get mergedInfo(): RecordingInfo {
    return this.#info;
  }

  #merged(): RecordingInfo {
    // Each input's sources, restated on the merged axis as the merge restates them: an origin or
    // align `x` at `floor(x × q)`, and each tick `q` times as long. An input that is itself merged
    // has several.
    const all = this.#inputs.every((i) => i.info.sources?.length);
    const first: number[] = [];
    const sources = this.#inputs.flatMap((input, i) => {
      first.push(first.length === 0 ? 0 : first[i - 1] + (this.#inputs[i - 1].info.sources?.length ?? 1));
      const q = this.#ratios[i];
      return (input.info.sources ?? []).map((s) => ({ ...s, origin: scaled(s.origin, q), ...(s.align && { align: scaled(s.align, q) }) }));
    });
    // Each input's names, as the merge gives them, for its derived channels' operands.
    const renamed = this.#inputs.map(() => new Map<string, string>());
    this.#origins.forEach(({ input, channel }, i) => renamed[input].set(this.#inputs[input].info.channels[channel].name, this.#names[i]));
    const channels = this.#origins.map(({ input, channel }, i): Channel => {
      const c = this.#inputs[input].info.channels[channel];
      const q = this.#ratios[input];
      const name = this.#names[i];
      const source = first[input] + (c.source ?? 0);
      if (c.kind === "unavailable") return { ...c, name, source, end: scaled(c.end, q) };
      // The source's ticks stay its own, each `q` times as long, its tick 0 restated.
      const map = mapOf(c);
      const merged = { origin: scaled(map.origin, q), offset: map.offset, m: product(map.m, q) };
      if (c.kind === "digital") {
        return {
          ...c,
          name,
          source,
          start: scaled(channelStart(c), q),
          end: scaled(c.end, q),
          ...(c.gaps && { gaps: c.gaps.map((g) => ({ ...g, from: scaled(g.from, q), to: scaled(g.to, q) })) }),
          map: merged,
        };
      }
      return {
        ...c,
        name,
        source,
        map: merged,
        end: scaled(c.end, q),
        ...(c.derived && { derived: { ...c.derived, operands: c.derived.operands.map((n) => renamed[input].get(n) ?? n) } }),
      };
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
  #span(channel: number, start: number, end: number): { input: MergeInput; c: number; s: number; e: number; q: Ratio } {
    const { input, channel: c } = this.#origins[channel];
    const q = this.#ratios[input];
    const s = Math.max(0, inputAtOrAfter(start, q));
    return { input: this.#inputs[input], c, s, e: Math.max(s, inputAtOrAfter(end, q)), q };
  }

  async edges(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Edges> {
    const { input, c, s, e, q } = this.#span(channel, start, end);
    const got = await input.source.edges(c, s, e, signal);
    const ticks = got.ticks.map((t) => scaled(t, q));
    // An input tick before `start` can map into the merged span's first tick, and the level at
    // `start` then counts the edges before it.
    let lo = 0;
    while (lo < ticks.length && ticks[lo] < start) lo++;
    let hi = ticks.length;
    while (hi > lo && ticks[hi - 1] >= end) hi--;
    const listed = listedOnAxis(got.listed, (t) => scaled(t, q));
    return { level: (got.level ^ (lo & 1)) as 0 | 1, ticks: ticks.subarray(lo, hi), ...(listed && { listed }) };
  }

  async overview(channel: number, start: number, end: number, maxBucket: number, signal?: AbortSignal): Promise<Overview | null> {
    const { input, c, s, e, q } = this.#span(channel, start, end);
    // A bucket of the input's is a whole number of merged ticks only where q is whole; otherwise
    // the caller reads edges or samples.
    if (q.den !== 1) return null;
    const got = await input.source.overview(c, s, e, Math.floor(maxBucket / q.num), signal);
    if (!got) return got;
    const listed = listedOnAxis(got.listed, (t) => scaled(t, q));
    return { ...got, bucket: got.bucket * q.num, start: got.start * q.num, ...(listed && { listed }) };
  }

  async samples(channel: number, start: number, end: number, signal?: AbortSignal): Promise<Samples> {
    // Samples go by index: the merged channel's `first`, `period` and `map` place them.
    const { input, c, s, e, q } = this.#span(channel, start, end);
    const got = await input.source.samples(c, s, e, signal);
    return got.listed ? { ...got, listed: listedOnAxis(got.listed, (t) => scaled(t, q)) } : got;
  }
}
