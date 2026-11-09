// The messages between the page and the decoder's worker.

export type Request =
  | { id: number; op: "open"; key: string; url?: string; file?: File; firstRead?: number; cacheBytes?: number }
  | { id: number; op: "edges" | "samples"; key: string; channel: number; start: number; end: number }
  | { id: number; op: "overview"; key: string; channel: number; start: number; end: number; maxBucket: number }
  | { id: number; op: "sidecar"; key: string; url: string; recordingFile: string | null }
  | { id: number; op: "bundled"; key: string }
  | { id: number; op: "resolve"; key: string; hand: string; names: string[]; texts: string[]; recordingFile: string | null };

export type Reply =
  | { id: number; ok: true; value: unknown }
  | { id: number; ok: false; name: string; message: string };
