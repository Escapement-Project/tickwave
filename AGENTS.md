# AGENTS.md

Agent rules for chrono, the Escapement Project's logic and MSO capture viewer for the web. chrono
is built to sit inside a page: a custom element that takes the host page's styles, sizes with its
layout, and answers its text. It is static (no server) and also opens local files. Digital and
analog channels share one time axis.

## Rules

**chrono owns its recording format, and other tools depend on it.** Readers for Saleae Logic 2's
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

**One core, two consumers.** A Rust crate owns encoding, decoding and the block index. The browser
uses it compiled to WASM, the converter CLI natively. A second implementation of the decoder comes
only if a benchmark on real recordings shows the WASM cost matters; record the numbers.

**Style from the host page's tokens.** The project's design system (colour roles by day and night,
data colours, line patterns, type roles) lives in the site repository,
`escapement-project.github.io`; its `design/README.md` explains it. Read the host page's CSS
variables (`--ink`, `--series-verge`, ...) rather than shipping chrono's own colours. chrono's own
site keeps copies of the site's `static/tokens.css` and `static/fonts/`, checked with
`uv run design/build.py --check <copies>` run from the site repository. Interaction is hue-free:
an ink focus ring, an ink wash under the pointer.

**Annotations live beside the recording**, in a text sidecar named after it. Ids stay stable when
a decoder reruns, and generated annotations are told apart from hand-written ones.

**Evidence on request, not centre stage.** The host page's prose carries the argument. A reference
in the text opens a small inset of the annotated span on hover; the full capture stays collapsed
until the reader opens it.

**Licence: MIT OR Apache-2.0.**

## Peers

The Escapement Project session oversees the project and owns the pages that embed chrono and the
converted recordings. Send it any plan that crosses into its side (the element's API, the
converter's interface, the sidecar format its scripts will write) before building it, and ask it
to confirm findings about the recordings before stating them. It does the same with plans that
touch chrono.
