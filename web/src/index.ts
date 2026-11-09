// chrono's web elements. Importing this module defines <chrono-view>, <chrono-inset> and
// <chrono-ref>, and reads .chrono recordings through the WASM decoder in a worker beside it.

import { ChronoInset } from "./elements/inset.ts";
import { ChronoRef } from "./elements/ref.ts";
import { ChronoView } from "./elements/view.ts";
import { registerLoader } from "./registry.ts";
import { chronoLoader } from "./wasm/client.ts";

export { registerLoader, type Loader } from "./registry.ts";
export { configure, registerFile } from "./wasm/client.ts";
export type { Annotation, RawAnnotation, Measure } from "./annotations.ts";
export type * from "./source.ts";
export { ChronoInset, ChronoRef, ChronoView };

registerLoader(chronoLoader);

for (const [name, element] of [
  ["chrono-inset", ChronoInset],
  ["chrono-view", ChronoView],
  ["chrono-ref", ChronoRef],
] as const) {
  if (!customElements.get(name)) customElements.define(name, element);
}
