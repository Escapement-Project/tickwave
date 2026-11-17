// tickwave's web elements. Importing this module defines <tickwave-view>, <tickwave-inset> and
// <tickwave-ref>, and reads .tickwave recordings through the WASM decoder in a worker beside it.

import { TickwaveInset } from "./elements/inset.ts";
import { TickwaveRef } from "./elements/ref.ts";
import { TickwaveView } from "./elements/view.ts";
import { registerLoader } from "./registry.ts";
import { tickwaveLoader } from "./wasm/client.ts";

export { registerLoader, type BundledRead, type Loader } from "./registry.ts";
export { readSidecar, recordingInfo, resolveSidecar } from "./recording-info.ts";
export type { Colour, Role } from "./roles.ts";
export { configure, registerFile } from "./wasm/client.ts";
export { previewMerge } from "./merge-preview.ts";
export type { MergedSource } from "./merged-source.ts";
export type { Annotation, Declared, RawAnnotation, Measure, Sidecar, StoredView } from "./annotations.ts";
export type * from "./source.ts";
export type { PeriodStats } from "./readouts.ts";
export { TickwaveInset, TickwaveRef, TickwaveView };
export type { ChannelGroup, ChannelShape, Summary } from "./elements/view.ts";

registerLoader(tickwaveLoader);

for (const [name, element] of [
  ["tickwave-inset", TickwaveInset],
  ["tickwave-view", TickwaveView],
  ["tickwave-ref", TickwaveRef],
] as const) {
  if (!customElements.get(name)) customElements.define(name, element);
}
