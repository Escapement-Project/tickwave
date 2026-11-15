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
  /** `[name, time]`: a channel moved by a time, later when positive. By the channel's name in the
   * recording; times as a sidecar writes them, in the source's time. */
  offsets: [string, string][];
  /** `[name, from, to]`: the part of a channel kept, in its own time before its move; null for an
   * open end. */
  keeps: [string, string | null, string | null][];
}

export const DEFAULTS: Settings = { channels: [], title: null, meta: [], level: 19, blockKib: 1024, units: [], codes: true, rate: null, offsets: [], keeps: [] };

/** A recording a merge takes, and what to take of it. */
export interface MergeInput {
  file: File;
  /** Its file's name without the extension, which prefixes a default name an earlier input took. */
  stem: string;
  /** The time it is moved by on the merged axis, later when positive; null for none. */
  align: string | null;
  /** `[name, new name]`, by the recording's own names, in the order to store them; none takes
   * every channel under its own name. */
  channels: [string, string][];
  /** Its sidecar's files, read where the recording bundles none: the hand-written .toml and the
   * files it includes. */
  sidecar: File[];
}

/** What `tickwave merge` takes beyond its inputs. */
export interface MergeSettings {
  /** The merged recording's file name without `.tickwave`, which names the generator files it
   * bundles. */
  stem: string;
  title: string | null;
  meta: [string, string][];
  level: number;
  blockKib: number;
  /** Analog channels' units, by the merged recording's names. */
  units: [string, string][];
  /** `[name, time]`: a channel moved by a time on the merged axis, later when positive. */
  offsets: [string, string][];
  /** `[name, from, to]`: the part of a channel kept, in its own input's time before any move; null
   * for an open end. */
  keeps: [string, string | null, string | null][];
}

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

export type Pass = "check" | "codes" | "encode";

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
  | { op: "merge"; inputs: MergeInput[]; settings: MergeSettings }
  /** `edit` applied to `text`, or `text` only read when it is null. */
  | { op: "edit"; text: string; edit: Edit | null }
  /** The files in a zip whose names end in one of `ends` (lower case), out of their folders. */
  | { op: "unzip"; file: File; ends: string[] };

export type FromWorker =
  | { op: "described"; described: Described }
  | { op: "edited"; text: string; hand: HandAnnotation[] }
  | { op: "unzipped"; files: File[] }
  | { op: "start"; pass: Pass; channel: string; total: number }
  | { op: "step"; n: number }
  | { op: "converted"; file: ArrayBuffer; codes: [string, number | null][]; seconds: number }
  /** `report`: what the merge left out or renamed, a sentence each. */
  | { op: "merged"; file: ArrayBuffer; report: string[]; seconds: number }
  | { op: "failed"; message: string };

/** The module's request for `kind` over `files`, as JSON. */
export function request(kind: SourceKind, files: FileRef[], settings?: Settings): string {
  return JSON.stringify(settings ? { kind, files, ...settings } : { kind, files });
}
