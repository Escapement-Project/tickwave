// The collapsed view's summary box, written once for two readers: the view's own shadow sheet, and
// tickwave.css, which lays out an undefined <tickwave-view> from the text a page slots into it. The
// two must give the same height and the same line breaks, or the view moves when tickwave.js
// defines it. A page changes the summary's metrics through these properties, which reach both;
// ::part() reaches only the defined element.

/** Each property a page may set, with the value the view uses without it. */
export const SUMMARY_PROPERTIES = {
  "--tickwave-summary-padding": "8px 10px 8px 14px",
  "--tickwave-summary-gap": "2px 12px",
  "--tickwave-summary-border-width": "1px",
  "--tickwave-summary-border-color": "var(--hair, GrayText)",
  "--tickwave-summary-background": "var(--surface, Canvas)",
  "--tickwave-summary-radius": "10px",
  "--tickwave-title-size": "1em",
  "--tickwave-facts-size": "13px",
  "--tickwave-open-button-size": "13px",
  "--tickwave-open-button-padding": "3px 9px",
  "--tickwave-open-button-height": "26px",
} as const;

type Property = keyof typeof SUMMARY_PROPERTIES;

const v = (p: Property) => `var(${p}, ${SUMMARY_PROPERTIES[p]})`;

/** Every element's own text, on :host, and an undefined view's. */
export const HOST_TEXT = "color: var(--ink, CanvasText); font-family: var(--font-text, system-ui, sans-serif); font-size: 14px; line-height: 1.4;";

const BOX = `display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: ${v("--tickwave-summary-gap")};
  padding: ${v("--tickwave-summary-padding")}; background: ${v("--tickwave-summary-background")};
  border-style: solid; border-width: ${v("--tickwave-summary-border-width")}; border-color: ${v("--tickwave-summary-border-color")};
  border-radius: ${v("--tickwave-summary-radius")};`;
const TITLE = `grid-row: 1; grid-column: 1; font-weight: 600; overflow-wrap: anywhere; font-size: ${v("--tickwave-title-size")};`;
const FACTS = `grid-row: 2; grid-column: 1; color: var(--ink-2, GrayText); font-size: ${v("--tickwave-facts-size")}; min-height: 1lh;`;
/** The Open button's box. Its words are hidden boxes in the same metrics, so it is as wide as the wider. */
const BUTTON = `grid-row: 1 / 3; grid-column: 2; box-sizing: border-box; font-size: ${v("--tickwave-open-button-size")}; line-height: 1.2;
  padding: ${v("--tickwave-open-button-padding")}; min-height: ${v("--tickwave-open-button-height")};`;
const WORDS = ["Open", "Close"] as const;

/** The summary's rules in the view's shadow sheet. */
export const SUMMARY_CSS = `
.summary { flex: none; ${BOX} }
:host([open]) .summary { border-bottom-left-radius: 0; border-bottom-right-radius: 0; }
.summary .title { ${TITLE} }
.summary .facts { ${FACTS} }
.summary .toggle { ${BUTTON} }
.summary .toggle::before { content: "${WORDS[0]}"; }
.summary .toggle::after { content: "${WORDS[1]}"; }
.summary .toggle::before, .summary .toggle::after { display: block; height: 0; overflow: hidden; visibility: hidden; }
`;

const UNDEFINED = ":where(tickwave-view:not(:defined):not([hidden]))";

/** tickwave.css's rules for an undefined view: its summary, from the spans a page slots into it. */
export function viewSheet(): string {
  return `${UNDEFINED} { ${HOST_TEXT}
  ${BOX} }
${UNDEFINED} > :where([slot="title"]) { ${TITLE} }
${UNDEFINED} > :where([slot="facts"]) { ${FACTS} }
${UNDEFINED}::before, ${UNDEFINED}::after { ${BUTTON}
  border: 1px solid transparent; visibility: hidden; }
${UNDEFINED}::before { content: "${WORDS[0]}"; }
${UNDEFINED}::after { content: "${WORDS[1]}"; }
`;
}
