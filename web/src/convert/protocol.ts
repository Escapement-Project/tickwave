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

/** An annotation's fields in the hand-written file, as the editor writes them (ANNOTATIONS.md). A
 * time is text, such as "317.85882 ms", or ticks. */
export interface Fields {
  id: string;
  start?: string | number;
  end?: string | number;
  channels?: string[];
  label?: string;
  kind?: string;
  text?: string;
  paint?: string;
  measure?: { channel: string; what: string };
  ref?: string;
}

/** One change to the hand-written file. `index` is the annotation's place among its tables. */
export type Edit =
  | { op: "start"; recording: string; recording_id: string }
  | { op: "insert"; index: number; fields: Fields }
  | { op: "replace"; index: number; fields: Fields }
  | { op: "remove"; index: number }
  | { op: "move"; from: number; to: number };

/** An annotation as the hand-written file holds it, any key it has included. */
export type HandAnnotation = Fields & Record<string, unknown>;

export type ToWorker =
  | { op: "describe"; kind: SourceKind; files: File[] }
  | { op: "convert"; kind: SourceKind; files: File[]; settings: Settings }
  /** `edit` applied to `text`, or `text` only read when it is null. */
  | { op: "edit"; text: string; edit: Edit | null };

export type FromWorker =
  | { op: "described"; described: Described }
  | { op: "edited"; text: string; hand: HandAnnotation[] }
  | { op: "start"; pass: Pass; channel: string; total: number }
  | { op: "step"; n: number }
  | { op: "converted"; file: ArrayBuffer; codes: [string, number | null][]; seconds: number }
  | { op: "failed"; message: string };

/** The module's request for `kind` over `files`, as JSON. */
export function request(kind: SourceKind, files: FileRef[], settings?: Settings): string {
  return JSON.stringify(settings ? { kind, files, ...settings } : { kind, files });
}
