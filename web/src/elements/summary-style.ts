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
.summary .actions { position: relative; grid-row: 1 / 3; grid-column: 2; display: flex; gap: 6px; align-items: center; }
.summary .toggle { ${BUTTON} }
/* A download is a link that looks like the Open button: its box and its metrics. */
.summary .download { ${BUTTON} display: inline-flex; align-items: center; color: var(--ink, CanvasText); text-decoration: none;
  border: 1px solid var(--rule, GrayText); border-radius: 6px; }
.summary .download:hover { background: var(--wash, color-mix(in srgb, CanvasText 12%, transparent)); }
.summary .toggle::before { content: "${WORDS[0]}"; }
.summary .toggle::after { content: "${WORDS[1]}"; }
.summary .toggle::before, .summary .toggle::after { display: block; height: 0; overflow: hidden; visibility: hidden; }
`;

/** An undefined view, with `attributes` as well, under no specificity, so any rule of the page's
 * wins. */
const undefinedView = (attributes = "") => `:where(tickwave-view${attributes}:not(:defined):not([hidden]))`;
const UNDEFINED = undefinedView();

/** tickwave.css's rules for an undefined view: its summary, from the spans a page slots into it. */
export function viewSheet(): string {
  // With download links, the button column holds them too. ::after is as wide as the links and
  // the button side by side: their words on a line, once with Open's and once with Close's, each
  // word apart from the next by a right-to-left mark, which takes no room and keeps the two from
  // kerning into each other as they don't in boxes of their own. Each side of each box's padding is
  // taken once by ::after's padding, once by its border and, with three boxes, once by its margin;
  // an empty column after the words, the 1 px borders and the 6 px between boxes. No taller than
  // nothing: ::before keeps the button's height.
  const pad = v("--tickwave-open-button-padding");
  const links = (attributes: string, words: string[], extra: string) => {
    const line = words.map((w) => `${w}\\200F `).join("");
    const fixed = 2 * (words.length + 1) + 6 * words.length;
    return `${undefinedView(attributes)}::after { content: "${line}${WORDS[0]}\\A ${line}${WORDS[1]}"; white-space: pre;
  display: grid; grid-template-columns: max-content ${fixed}px; border-style: solid; border-color: transparent;
  border-width: ${pad}; ${extra}border-top-width: 0; border-bottom-width: 0; padding-top: 0; padding-bottom: 0;
  min-height: 0; height: 0; overflow: hidden; }`;
  };
  return `${UNDEFINED} { ${HOST_TEXT}
  ${BOX} }
${UNDEFINED} > :where([slot="title"]) { ${TITLE} }
${UNDEFINED} > :where([slot="facts"]) { ${FACTS} }
${UNDEFINED}::before, ${UNDEFINED}::after { ${BUTTON}
  border: 1px solid transparent; visibility: hidden; }
${UNDEFINED}::before { content: "${WORDS[0]}"; }
${UNDEFINED}::after { content: "${WORDS[1]}"; }
${links("[download]", ["Download"], "")}
${links("[download][annotations]", ["Download", "Annotations"], `margin: ${pad}; margin-top: 0; margin-bottom: 0; `)}
`;
}
