/** The first index in [lo, hi) whose value is not less than x; hi when there is none. */
export function lowerBound(a: ArrayLike<number>, x: number, lo = 0, hi = a.length): number {
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (a[mid] < x) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** `lowerBound` over [lo, a.length) for an x likely near lo: steps 1, 2, 4, ... out from lo, then
 * searches the last step, so an answer k past lo costs about 2 log2(k − lo) reads, not log2 of the
 * whole range. */
export function gallop(a: ArrayLike<number>, x: number, lo = 0): number {
  let hi = lo;
  for (let step = 1; hi < a.length && a[hi] < x; step *= 2) {
    lo = hi + 1;
    hi += step;
  }
  return lowerBound(a, x, lo, Math.min(hi, a.length));
}
