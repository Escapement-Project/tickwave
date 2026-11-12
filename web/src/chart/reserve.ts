// What a chart holds room for before its data, written once for two readers: the chart's own sheet,
// and tickwave.css, which reserves the same room for a chart tickwave-chart.js has not defined yet.

/** The plot's height, a length. It wins over the `height` attribute. */
export const HEIGHT_PROPERTY = "--tickwave-chart-height";
/** The lines a readout below the plot holds before the data: one for the setting, one a series. */
export const LINES_PROPERTY = "--tickwave-chart-readout-lines";
/** The plot's height in CSS pixels, with neither the property nor the attribute. */
export const DEFAULT_HEIGHT = 330;
const READOUT_SIZE = 13;
const READOUT_LEADING = 1.45;
/** A readout's text: a line of it is what the readout lines count. */
export const READOUT_TEXT = `font-size: ${READOUT_SIZE}px; line-height: ${READOUT_LEADING};`;
/** A readout line in CSS pixels, as the chart reserves it: a multiple of `1lh` would differ by a
 * rounding, since Chromium rounds the line height before multiplying. */
const READOUT_LINE = Math.round(READOUT_SIZE * READOUT_LEADING * 1e4) / 1e4;

const UNDEFINED = ":where(tickwave-chart:not(:defined):not([hidden]))";

/** tickwave.css's rules for an undefined chart: its plot, and a readout below's lines. */
export function chartSheet(): string {
  return `${UNDEFINED} { display: block; }
${UNDEFINED}::before { content: ""; display: block; height: var(${HEIGHT_PROPERTY}, ${DEFAULT_HEIGHT}px); }
:where(tickwave-chart[readout="below"]:not(:defined):not([hidden]))::after { content: ""; display: block;
  height: calc(var(${LINES_PROPERTY}, 0) * ${READOUT_LINE}px); }
`;
}
