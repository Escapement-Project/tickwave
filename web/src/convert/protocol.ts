// The converter's messages between the page and its worker, and the WASM module's requests, which
// the worker and Node's host (scripts/node-convert.ts) both send.

export type SourceKind = "saleae" | "sigrok" | "vcd";

/** A file the worker holds, by its index there. */
export interface FileRef {
  id: number;
  name: string;
  size: number;
}

/** What `tickwave convert` takes beyond the source, with its defaults. */
export interface Settings {
  /** `[source name, name]`, in the order to store them. A Saleae export needs at least one; for the
   * others, none takes every channel under its own name. */
  channels: [string, string][];
  title: string | null;
  meta: [string, string][];
  /** The zstd level. */
  level: number;
  /** The decoded size each block aims for, in KiB. */
  blockKib: number;
  /** Analog channels' units, by name, where the source states none. */
  units: [string, string][];
  /** Store analog values as exact integer codes where some reproduce them. */
  codes: boolean;
  /** A Saleae export's sample rate as typed, such as 50M; null to take the rate its files record,
   * or to infer it from the edges where they record none. */
  rate: string | null;
}

export const DEFAULTS: Settings = { channels: [], title: null, meta: [], level: 19, blockKib: 1024, units: [], codes: true, rate: null };

export interface ChannelInfo {
  /** The source's own name: a Saleae file's stem, a sigrok probe, a VCD scope path. */
  name: string;
  analog: boolean;
  /** Edges or samples, where the source states them before it is read. */
  count: number | null;
  unit: string | null;
}

export interface Described {
  channels: ChannelInfo[];
  skipped: { name: string; reason: string }[];
  /** A digital file records no sample rate: a Saleae export of version 0. */
  rateMissing: boolean;
}

export type Pass = "check" | "codes" | "encode" | "verify";

export type ToWorker =
  | { op: "describe"; kind: SourceKind; files: File[] }
  | { op: "convert"; kind: SourceKind; files: File[]; settings: Settings };

export type FromWorker =
  | { op: "described"; described: Described }
  | { op: "start"; pass: Pass; channel: string; total: number }
  | { op: "step"; n: number }
  | { op: "converted"; file: ArrayBuffer; codes: [string, number | null][]; seconds: number }
  | { op: "failed"; message: string };

/** The module's request for `kind` over `files`, as JSON. */
export function request(kind: SourceKind, files: FileRef[], settings?: Settings): string {
  return JSON.stringify(settings ? { kind, files, ...settings } : { kind, files });
}
