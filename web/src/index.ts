// tickwave's web elements. Importing this module defines <tickwave-view>, <tickwave-inset> and
// <tickwave-ref>, and reads .tickwave recordings through the WASM decoder in a worker beside it.

import { TickwaveInset } from "./elements/inset.ts";
import { TickwaveRef } from "./elements/ref.ts";
import { TickwaveView } from "./elements/view.ts";
import { registerLoader } from "./registry.ts";
import { tickwaveLoader } from "./wasm/client.ts";

export { registerLoader, type Loader } from "./registry.ts";
export { recordingInfo } from "./recording-info.ts";
export { configure, registerFile } from "./wasm/client.ts";
export type { Annotation, RawAnnotation, Measure } from "./annotations.ts";
export type * from "./source.ts";
export { TickwaveInset, TickwaveRef, TickwaveView };
export type { Summary } from "./elements/view.ts";

registerLoader(tickwaveLoader);

for (const [name, element] of [
  ["tickwave-inset", TickwaveInset],
  ["tickwave-view", TickwaveView],
  ["tickwave-ref", TickwaveRef],
] as const) {
  if (!customElements.get(name)) customElements.define(name, element);
}
