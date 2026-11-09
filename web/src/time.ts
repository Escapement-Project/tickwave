// Times as written in attributes and annotations: an integer tick count, or a decimal with a unit
// (s, ms, us, ns, ps; also µs). Converted to ticks exactly, rounding to the nearest tick.

import type { Ratio } from "./source.ts";

const UNIT_EXP: Record<string, number> = { s: 0, ms: -3, us: -6, "µs": -6, "μs": -6, ns: -9, ps: -12 };

/** A decimal number of seconds, mantissa × 10^exp. */
export interface Seconds {
  readonly mantissa: bigint;
  readonly exp: number;
}

export type TimeSpec = { readonly ticks: number } | { readonly seconds: Seconds };

/** Parses "1234" (ticks) or "317.85882 ms". Null when it is neither. */
export function parseTime(text: string): TimeSpec | null {
  const t = text.trim();
  if (/^\d+$/.test(t)) {
    const ticks = Number(t);
    return Number.isSafeInteger(ticks) ? { ticks } : null;
  }
  const m = /^(-?)(\d+)(?:\.(\d+))?\s*([a-zµμ]+)$/i.exec(t);
  if (!m) return null;
  const [, sign, int, frac = "", unit] = m;
  const e = UNIT_EXP[unit];
  if (e === undefined) return null;
  const mantissa = BigInt(int + frac);
  return { seconds: { mantissa: sign ? -mantissa : mantissa, exp: e - frac.length } };
}

/** Seconds to ticks, rounded to the nearest tick, halves away from zero. */
export function secondsToTicks(s: Seconds, timebase: Ratio): number {
  // s.mantissa × 10^exp × num / den
  let n = s.mantissa * BigInt(timebase.num);
  let d = BigInt(timebase.den);
  if (s.exp >= 0) n *= 10n ** BigInt(s.exp);
  else d *= 10n ** BigInt(-s.exp);
  const neg = n < 0n;
  const abs = neg ? -n : n;
  const q = (2n * abs + d) / (2n * d);
  return Number(neg ? -q : q);
}

export function toTicks(spec: TimeSpec, timebase: Ratio): number {
  return "ticks" in spec ? spec.ticks : secondsToTicks(spec.seconds, timebase);
}

/** Seconds as a float, for scales and paddings where a tick's rounding does not matter. */
export function secondsValue(s: Seconds): number {
  return Number(s.mantissa) * 10 ** s.exp;
}
