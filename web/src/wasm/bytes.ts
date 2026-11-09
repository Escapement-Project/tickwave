// Byte ranges of a recording: over HTTP with Range requests, or from a local File.

import { aborted, named } from "../source.ts";

export interface Bytes {
  /** Bytes [from, to) of the file; fewer where the file ends first. */
  read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array>;
  /** For messages. */
  readonly name: string;
}

function join(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a);
  out.set(b, a.length);
  return out;
}

export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

/** Reads from the file's start are kept up to this size: the head, and what a first read held. */
const PREFIX_CAP = 1 << 20;

/**
 * Reads by HTTP Range. A server that answers 200 instead of 206 has sent the whole file: it is
 * kept, and every later read is served from it. The bytes read from the start of the file are
 * kept too, so a block inside the first read is not fetched again.
 */
export class HttpBytes implements Bytes {
  readonly name: string;
  readonly #url: string;
  readonly #fetch: Fetch;
  #whole: Uint8Array | null = null;
  #prefix: Uint8Array = new Uint8Array(0);

  constructor(url: string, fetchFn: Fetch = (u, i) => fetch(u, i)) {
    this.#url = url;
    this.name = decodeURIComponent(new URL(url).pathname.split("/").pop() ?? url);
    this.#fetch = fetchFn;
  }

  async read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (signal?.aborted) throw aborted();
    if (this.#whole) return this.#whole.subarray(from, Math.min(to, this.#whole.length));
    if (to <= this.#prefix.length) return this.#prefix.subarray(from, to);
    let res: Response;
    try {
      res = await this.#fetch(this.#url, { headers: { Range: `bytes=${from}-${to - 1}` }, signal });
    } catch (e) {
      if (signal?.aborted) throw aborted();
      throw named("TickwaveNetworkError", `${this.name}: no answer from ${new URL(this.#url).host || "the server"} (${e instanceof Error ? e.message : e})`);
    }
    if (res.status === 200) {
      this.#whole = new Uint8Array(await res.arrayBuffer());
      return this.#whole.subarray(from, Math.min(to, this.#whole.length));
    }
    if (res.status !== 206) throw named("TickwaveHttpError", `${this.name}: HTTP ${res.status}`);
    // A compressed response's ranges count bytes of the compressed stream (PLAN.md §0).
    const encoding = res.headers.get("content-encoding");
    if (encoding && encoding !== "identity") {
      throw new Error(`${this.name}: the server compresses it (${encoding}), so its byte ranges are unreliable`);
    }
    const range = /^bytes (\d+)-\d+\//.exec(res.headers.get("content-range") ?? "");
    if (range && Number(range[1]) !== from) throw new Error(`${this.name}: asked for bytes from ${from}, got ${range[1]}`);
    const bytes = new Uint8Array(await res.arrayBuffer());
    if (from === this.#prefix.length && from + bytes.length <= PREFIX_CAP) this.#prefix = join(this.#prefix, bytes);
    return bytes;
  }
}

export class FileBytes implements Bytes {
  readonly name: string;
  readonly #file: Blob;

  constructor(file: Blob, name: string) {
    this.#file = file;
    this.name = name;
  }

  async read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (signal?.aborted) throw aborted();
    return new Uint8Array(await this.#file.slice(from, to).arrayBuffer());
  }
}

export { join };

/** A file already in memory; counts its reads, for tests. */
export class MemoryBytes implements Bytes {
  readonly name: string;
  readonly #bytes: Uint8Array;
  reads: [number, number][] = [];

  constructor(bytes: Uint8Array, name: string) {
    this.#bytes = bytes;
    this.name = name;
  }

  async read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (signal?.aborted) throw aborted();
    this.reads.push([from, to]);
    return this.#bytes.slice(from, Math.min(to, this.#bytes.length));
  }
}
