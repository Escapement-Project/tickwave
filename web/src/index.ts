// chrono's web elements. Importing this module defines <chrono-view>, <chrono-inset> and
// <chrono-ref>. A page or the decoder registers a loader that reads its recordings.

import { ChronoInset } from "./elements/inset.ts";
import { ChronoRef } from "./elements/ref.ts";
import { ChronoView } from "./elements/view.ts";

export { registerLoader, type Loader } from "./registry.ts";
export type { RawAnnotation, Measure } from "./annotations.ts";
export type * from "./source.ts";
export { ChronoInset, ChronoRef, ChronoView };

for (const [name, element] of [
  ["chrono-inset", ChronoInset],
  ["chrono-view", ChronoView],
  ["chrono-ref", ChronoRef],
] as const) {
  if (!customElements.get(name)) customElements.define(name, element);
}
