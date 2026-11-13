// The merge's preview as a `src` the elements read: recordings already open, on one time axis.

import { MergedSource } from "./merged-source.ts";
import { absolute, registerLoader, sourceFor, type Loader } from "./registry.ts";

const previews = new Map<string, MergedSource>();
let made = 0;
const loader: Loader = {
  accepts: (url) => previews.has(url.href),
  source: (url) => previews.get(url.href)!,
};

/**
 * The recordings at `inputs`, each with its file's name without the extension, as one recording a
 * view draws: every channel of each, in order, a later input's clashing name taking its stem.
 */
export async function previewMerge(inputs: readonly { readonly src: string; readonly stem: string }[]): Promise<{ src: string; merged: MergedSource }> {
  const opened = await Promise.all(
    inputs.map(async ({ src, stem }) => {
      const source = await sourceFor(absolute(src));
      return { source, info: await source.info(), stem };
    }),
  );
  const merged = new MergedSource(opened);
  if (made === 0) registerLoader(loader);
  const src = `tickwave-merge:${++made}`;
  previews.set(src, merged);
  return { src, merged };
}
