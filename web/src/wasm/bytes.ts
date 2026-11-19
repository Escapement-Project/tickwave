// Byte ranges of a recording: over HTTP with Range requests, or from a local File.

import { aborted, named } from "../source.ts";

export interface Bytes {
  /** Bytes [from, to) of the file; fewer where the file ends first. */
  read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array>;
  /** The file's last `n` bytes, or all of it when it is shorter, and the file's size. */
  tail(n: number, signal?: AbortSignal): Promise<Tail>;
  /** For messages. */
  readonly name: string;
}

export interface Tail {
  readonly bytes: Uint8Array;
  readonly size: number;
}

function join(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a);
  out.set(b, a.length);
  return out;
}

export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

/** Reads from the file's end are kept up to this size: the footer, and what a first read held. */
const END_CAP = 1 << 20;

/**
 * Reads by HTTP Range. The first read asks for the file's end (`bytes=-n`). A server that answers
 * 200 instead of 206 has sent the whole file: it is kept, and every later read is served from it.
 * The bytes read from the file's end are kept too, so a block inside the first read is not
 * fetched again.
 *
 * A file on another origin is read from its start first (`bytes=0-`), for its size. There, the
 * browser sends a read from the end only after a preflight, which GitHub Pages refuses. And the
 * browser hides `Content-Range` unless the server exposes it, while `Content-Length` is always
 * readable.
 */
export class HttpBytes implements Bytes {
  readonly name: string;
  readonly #url: string;
  readonly #fetch: Fetch;
  readonly #otherOrigin: boolean;
  #whole: Uint8Array | null = null;
  #end: Uint8Array = new Uint8Array(0);
  /** Where `#end` starts in the file, once a tail is read. */
  #endFrom = Infinity;
  /** The file's size, once a tail is read: an answer without a readable `Content-Range` is
   * checked by its length against it. */
  #size: number | undefined;

  /** `origin` is the one reading, `globalThis.origin` by default; without one, the file is
   * taken as the reader's own. */
  constructor(url: string, fetchFn: Fetch = (u, i) => fetch(u, i), origin: string | undefined = globalThis.origin) {
    this.#url = url;
    this.name = decodeURIComponent(new URL(url).pathname.split("/").pop() ?? url);
    this.#fetch = fetchFn;
    this.#otherOrigin = origin !== undefined && new URL(url).origin !== origin;
  }

  async #get(range: string, signal?: AbortSignal, allow416 = false): Promise<Response> {
    if (signal?.aborted) throw aborted();
    let res: Response;
    try {
      res = await this.#fetch(this.#url, { headers: { Range: range }, signal });
    } catch (e) {
      if (signal?.aborted) throw aborted();
      throw named("TickwaveNetworkError", `${this.name}: no answer from ${new URL(this.#url).host || "the server"} (${e instanceof Error ? e.message : e})`);
    }
    if (res.status !== 200 && res.status !== 206 && !(allow416 && res.status === 416)) {
      throw named("TickwaveHttpError", `${this.name}: HTTP ${res.status}`);
    }
    // A compressed response's ranges count bytes of the compressed stream, not of the file.
    const encoding = res.headers.get("content-encoding");
    if (res.status === 206 && encoding && encoding !== "identity") {
      throw new Error(`${this.name}: the server compresses it (${encoding}), so its byte ranges are unreliable`);
    }
    return res;
  }

  async tail(n: number, signal?: AbortSignal): Promise<Tail> {
    if (this.#whole) return { bytes: this.#whole.subarray(Math.max(0, this.#whole.length - n)), size: this.#whole.length };
    if (this.#otherOrigin) return this.#tailFromStart(n, signal);
    const res = await this.#get(`bytes=-${n}`, signal, true);
    if (res.status === 200) {
      this.#whole = new Uint8Array(await res.arrayBuffer());
      return this.tail(n, signal);
    }
    // A file shorter than the read: RFC 9110 sends it whole, but GitHub Pages answers 416 with
    // its size, and the file is read whole from its start.
    if (res.status === 416) {
      const size = /^bytes \*\/(\d+)$/.exec(res.headers.get("content-range") ?? "");
      if (!size) throw named("TickwaveHttpError", `${this.name}: HTTP 416`);
      this.#size = Number(size[1]);
      if (this.#size === 0) return { bytes: new Uint8Array(0), size: 0 };
      const bytes = await this.read(0, this.#size, signal);
      if (!this.#whole) [this.#end, this.#endFrom] = [bytes, 0];
      return { bytes, size: this.#size };
    }
    const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(res.headers.get("content-range") ?? "");
    if (!range) throw new Error(`${this.name}: the server's answer to a read from the end says no range`);
    const bytes = new Uint8Array(await res.arrayBuffer());
    const [from, size] = [Number(range[1]), Number(range[3])];
    if (from + bytes.length !== size) throw new Error(`${this.name}: asked for the last ${n} bytes, got ${range[0]}`);
    this.#size = size;
    this.#end = bytes;
    this.#endFrom = from;
    return { bytes, size };
  }

  /** The tail of a file on another origin: its size from a read from its start, whose body is
   * kept when it is the whole tail, then its end by an explicit range. */
  async #tailFromStart(n: number, signal?: AbortSignal): Promise<Tail> {
    // Stopped by an abort: Firefox goes on downloading a body that is only cancelled.
    const stop = new AbortController();
    const res = await this.#get("bytes=0-", signal ? AbortSignal.any([signal, stop.signal]) : stop.signal, true);
    if (res.status === 200) {
      this.#whole = new Uint8Array(await res.arrayBuffer());
      return this.tail(n, signal);
    }
    // A read from the start is unsatisfiable only when the file is empty.
    if (res.status === 416) {
      this.#size = 0;
      return { bytes: new Uint8Array(0), size: 0 };
    }
    const range = res.headers.get("content-range");
    const said = range !== null ? /^bytes 0-\d+\/(\d+)$/.exec(range)?.[1] : res.headers.get("content-length");
    if (!said || !/^\d+$/.test(said)) {
      stop.abort();
      throw new Error(`${this.name}: the server's answer to a read from the start says no size`);
    }
    const size = (this.#size = Number(said));
    if (size <= n) {
      const bytes = new Uint8Array(await res.arrayBuffer());
      if (bytes.length !== size) throw new Error(`${this.name}: asked for its ${size} bytes, got ${bytes.length}`);
      [this.#end, this.#endFrom] = [bytes, 0];
      return { bytes, size };
    }
    stop.abort();
    const bytes = await this.read(size - n, size, signal);
    [this.#end, this.#endFrom] = [bytes, size - n];
    return { bytes, size };
  }

  async read(from: number, to: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (signal?.aborted) throw aborted();
    if (this.#whole) return this.#whole.subarray(from, Math.min(to, this.#whole.length));
    if (from >= this.#endFrom) return this.#end.subarray(from - this.#endFrom, to - this.#endFrom);
    const res = await this.#get(`bytes=${from}-${to - 1}`, signal);
    if (res.status === 200) {
      this.#whole = new Uint8Array(await res.arrayBuffer());
      return this.#whole.subarray(from, Math.min(to, this.#whole.length));
    }
    const range = /^bytes (\d+)-\d+\//.exec(res.headers.get("content-range") ?? "");
    if (range && Number(range[1]) !== from) throw new Error(`${this.name}: asked for bytes from ${from}, got ${range[1]}`);
    const bytes = new Uint8Array(await res.arrayBuffer());
    // Where the answer starts can't be seen: its length is checked instead.
    if (!range && (this.#size === undefined ? bytes.length > to - from : bytes.length !== Math.max(0, Math.min(to, this.#size) - from))) {
      throw new Error(`${this.name}: asked for bytes ${from} to ${to}, got ${bytes.length}`);
    }
    // The rest of a footer the first read did not hold: it joins what is kept from the end.
    if (from + bytes.length === this.#endFrom && bytes.length + this.#end.length <= END_CAP) {
      this.#end = join(bytes, this.#end);
      this.#endFrom = from;
    }
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

  async tail(n: number, signal?: AbortSignal): Promise<Tail> {
    const size = this.#file.size;
    return { bytes: await this.read(Math.max(0, size - n), size, signal), size };
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

  async tail(n: number, signal?: AbortSignal): Promise<Tail> {
    const size = this.#bytes.length;
    return { bytes: await this.read(Math.max(0, size - n), size, signal), size };
  }
}
