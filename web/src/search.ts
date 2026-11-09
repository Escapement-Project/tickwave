/** The first index in [lo, hi) whose value is not less than x; hi when there is none. */
export function lowerBound(a: ArrayLike<number>, x: number, lo = 0, hi = a.length): number {
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (a[mid] < x) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}
