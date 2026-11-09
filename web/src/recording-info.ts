import { absolute, infoFor } from "./registry.ts";
import type { RecordingInfo } from "./source.ts";

/** What a recording is, from its head: what the viewer lists beside a view. Shared with every element. */
export function recordingInfo(src: string): Promise<RecordingInfo> {
  return infoFor(absolute(src));
}
