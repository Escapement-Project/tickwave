// Chart values are decimal strings, as whatever measured them wrote them: "81.0" keeps its last
// digit. They become numbers only to be placed.

const DECIMAL = /^-?\d+(\.\d+)?$/;

export function isDecimal(s: unknown): s is string {
  return typeof s === "string" && DECIMAL.test(s);
}

/** The value, for placing it on an axis. */
export function toNumber(s: string): number {
  return Number(s);
}

/** "1238.6" reads "1,238.6": the integer part's thousands grouped, no digit added or dropped. */
export function grouped(s: string): string {
  const negative = s.startsWith("-");
  const digits = negative ? s.slice(1) : s;
  const [whole, fraction] = digits.split(".");
  const groups = whole.replace(/\B(?=(\d{3})+$)/g, ",");
  return `${negative ? "-" : ""}${groups}${fraction === undefined ? "" : `.${fraction}`}`;
}
