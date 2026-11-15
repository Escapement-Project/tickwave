// The empty lane's words: what the plot says, and that it walks the annotations again only when
// they or the shown channels change.

import { test } from "node:test";
import assert from "node:assert/strict";
import type { Annotation, Resolved } from "../src/annotations.ts";
import { Plot, type Layout } from "../src/render/plot.ts";
import type { Track } from "../src/render/track.ts";

/** Annotations whose list counts the walks over it. */
function counted(list: Partial<Annotation>[]): { annotations: Resolved; walks: () => number } {
  let walks = 0;
  const byTime = Object.assign(list as Annotation[], {
    some(this: Annotation[], f: (a: Annotation) => boolean) {
      walks++;
      return Array.prototype.some.call(this, f);
    },
  });
  return { annotations: { byTime } as unknown as Resolved, walks: () => walks };
}

const tracks = (...names: string[]) => names.map((name) => ({ channel: { name } })) as unknown as Track[];

test("the lane says its words only with no annotation on a shown channel, and walks the list only when that may have changed", () => {
  const plot = new Plot({ getContext: () => ({}) } as unknown as HTMLCanvasElement, {} as never);
  plot.layout = { laneHeight: 58 } as Layout;
  const { annotations, walks } = counted([{ id: "on-swdio", channels: ["SWDIO"] }]);
  plot.annotations = annotations;
  plot.tracks = tracks("SWCLK");
  // While the annotations load: no words, and no walk.
  assert.equal(plot.laneSays(), null);
  assert.equal(walks(), 0);
  plot.emptyLane = "No annotations on the channels shown";
  // Redraws with nothing changed: one walk.
  for (let i = 0; i < 5; i++) assert.equal(plot.laneSays(), "No annotations on the channels shown");
  assert.equal(walks(), 1);
  // Another set of channels, the same names in new tracks: no walk; SWDIO shown: one, and no words.
  plot.tracks = tracks("SWCLK");
  plot.laneSays();
  assert.equal(walks(), 1);
  plot.tracks = tracks("SWCLK", "SWDIO");
  assert.equal(plot.laneSays(), null);
  plot.laneSays();
  assert.equal(walks(), 2);
  // Another list, as a reannotate makes: walked again.
  const again = counted([{ id: "on-swdio", channels: ["SWDIO"] }]);
  plot.annotations = again.annotations;
  plot.tracks = tracks("SWCLK");
  assert.equal(plot.laneSays(), "No annotations on the channels shown");
  assert.equal(again.walks(), 1);
  // A recording-wide annotation falls on every channel.
  const wide = counted([{ id: "all", channels: null }]);
  plot.annotations = wide.annotations;
  assert.equal(plot.laneSays(), null);
  // No lane laid out: nothing to say.
  plot.annotations = annotations;
  plot.layout = { laneHeight: 0 } as Layout;
  assert.equal(plot.laneSays(), null);
});
