import type { Annotation } from "./annotations.ts";
import { absolute, annotationsFromText, infoFor } from "./registry.ts";
import type { RecordingInfo } from "./source.ts";

/** What a recording is, from its head: what the viewer lists beside a view. Shared with every element. */
export function recordingInfo(src: string): Promise<RecordingInfo> {
  return infoFor(absolute(src));
}

/**
 * The annotations of a sidecar whose hand-written file is `text`, checked against the recording at
 * `src` as `tickwave annotations check` checks them. Its includes are read beside `annotations`,
 * the sidecar's address, or from the files registered with the recording when that is null.
 * Rejects with every problem, one a line.
 */
export function resolveSidecar(src: string, text: string, annotations: string | null): Promise<Annotation[]> {
  return annotationsFromText(text, annotations ? absolute(annotations) : null, absolute(src));
}
