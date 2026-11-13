# AGENTS.md

Agent rules for tickwave, the Escapement Project's logic and MSO capture viewer for the web.
tickwave is built to sit inside a page: a custom element that takes the host page's styles, sizes
with its layout, and answers its text. It is static (no server) and also opens local files. Digital
and analog channels share one time axis.

## Rules

**tickwave owns its recording format, and other tools depend on it.** Readers for Saleae Logic 2's
binary export, sigrok `.sr` and VCD convert into it, and the Escapement Project's pages publish
recordings in it. Give the format a version and a written specification in this repository, and
change either deliberately: a change that breaks published recordings needs a reader for the old
version.

**Store what the instrument recorded, exactly.** Never resample, smooth or round a time in a way
that implies better resolution than the source had. The header records the source's resolution,
and a readout never shows more digits than it carries.

**Load lazily by construction.** A recording is independent compressed blocks behind an index that
a reader fetches first, so a view fetches only the blocks it covers. Nothing loads until the page
shows a reference to a recording or the reader opens it.

**Plotting costs as little as it can.** Captures get very large, and the viewer runs inside other
pages on less powerful machines. A view's work stays off the page's main thread where it can, and
what is left fits well inside a frame at 6x CPU throttle. Latency from input or from new data to
the screen is measured, stated and kept down: it feels worse than it measures. A change to drawing
or querying comes with its frame-time and latency figures.

**One core, two consumers.** A Rust crate owns encoding, decoding and the block index. The browser
uses it compiled to WASM, the converter CLI natively. A second implementation of the decoder comes
only if a benchmark on real recordings shows the WASM cost matters; record the numbers.

**Style from the host page's tokens.** The project's design system (colour roles by day and night,
data colours, line patterns, type roles) lives in the site repository,
`escapement-project.github.io`; its `design/README.md` explains it. Read the host page's CSS
variables (`--ink`, `--series-verge`, ...) rather than shipping tickwave's own colours. tickwave's
own site keeps copies of the site's `static/tokens.css` and `static/fonts/`, and of the project's
shared header, footer and base sheet, checked with
`uv run design/build.py --check <copies>` run from the site repository. Interaction is hue-free:
an ink focus ring, an ink wash under the pointer.

**Nothing moves under the reader.** A readout's words and figures hold their places as values
change: figures are tabular, and a word that changes is set as wide as its widest form ("low" as
wide as "high"). A label keeps off what it reads. An inset or chart keeps its height once laid out.
A control stays where the reader pressed it. One that opens or switches something undoes it when
pressed again: Open becomes Close. The user corrected each of these once; apply the rule unasked.

**Annotations live beside the recording**, in a text sidecar named after it. Ids stay stable when
a decoder reruns, and generated annotations are told apart from hand-written ones.

**Evidence on request, not centre stage.** The host page's prose carries the argument. A reference
in the text opens a small inset of the annotated span on hover; the full capture stays collapsed
until the reader opens it.

**Licence: MIT OR Apache-2.0.**

## Peers

The Escapement Project session oversees the project and owns the pages that embed tickwave and the
converted recordings. Send it any plan that crosses into its side (the element's API, the
converter's interface, the sidecar format its scripts will write) before building it, and ask it
to confirm findings about the recordings before stating them. It does the same with plans that
touch tickwave.
