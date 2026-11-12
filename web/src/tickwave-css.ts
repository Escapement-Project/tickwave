// tickwave.css, which build.ts writes beside the modules: the room a <tickwave-view> or a
// <tickwave-chart> in a page's markup takes before its module defines it.

import { chartSheet } from "./chart/reserve.ts";
import { viewSheet } from "./elements/summary-style.ts";

export function tickwaveCss(): string {
  return `/* tickwave.css: <tickwave-view> and <tickwave-chart> before their modules define them, laid out as
   the elements lay out, so neither moves the page when it is defined. A view takes its summary from
   the spans a page writes into it, <span slot="title"> and <span slot="facts">, and leaves the
   button's cell empty. A chart takes its plot's height from --tickwave-chart-height, and a readout
   below the plot the lines --tickwave-chart-readout-lines states. Every rule is in :where(), so any
   rule of the page's wins. */
${viewSheet()}${chartSheet()}`;
}
