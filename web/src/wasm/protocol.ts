// The messages between the page and the decoder's worker.

/** The first read of a recording's head, unless the page configures another (FORMAT.md). */
export const FIRST_READ = 32 * 1024;

/** The most the decoder takes from one block, overview block or head overview, unless the page
 * configures another: the format sets no maximum, and a file can state up to 4 GiB. */
export const MAX_DECODED = 64 << 20;

export type Request =
  | { id: number; op: "open"; key: string; url?: string; file?: File; firstRead?: number; maxDecoded?: number; cacheBytes?: number }
  | { id: number; op: "edges" | "samples" | "edgeCount" | "periods"; key: string; channel: number; start: number; end: number }
  | { id: number; op: "overview"; key: string; channel: number; start: number; end: number; maxBucket: number }
  | { id: number; op: "sidecar"; key: string; url: string; recordingFile: string | null }
  /** The bundled annotations the parts meeting ticks `span` hold, with every part read before; all
   * of them where `span` is null. */
  | { id: number; op: "bundled"; key: string; span: readonly [number, number] | null }
  | { id: number; op: "resolve"; key: string; hand: string; names: string[]; texts: string[]; recordingFile: string | null }
  | { id: number; op: "sidecarText"; key: string; hand: string; base: string | null; recordingFile: string | null }
  /** The names a hand-written file includes, in its order: the file at `url`, else `hand`, which
   * `where` names in a syntax error. */
  | { id: number; op: "includes"; url: string | null; hand: string | null; where: string };

export type Reply =
  | { id: number; ok: true; value: unknown }
  | { id: number; ok: false; name: string; message: string };
