# Changelog

## Unreleased

Protocol decoding, in the native renderer. Decoders for SWD, JTAG and cJTAG, with JTAG-DP, ADIv5
and MEM-AP stacked on them, draw their items as tracks under the graph. Zoomed out, a track shows
how many items start in each column; zoomed in, each item, its parts and their bits.

A side panel shows the selected item in full, and steps to the items before and after it and to
the items that read it. It lists an analysis's items, and joins an analysis's bytes into lines of
text. Its Decoders tab changes an analysis's settings, adds a decoder or a whole stack, and
removes one. A changed setting decodes the window on screen again at once, marked provisional
until the full pass catches up.

A recipe file's `[[decode]]` tables say which decoders to run over a recording, and on which
channels. Recipes for SWD, JTAG and cJTAG come with the decoders, and the renderer's demo opens a
recording decoded with `--recipe`.

`escapement-tickwave-decode` is the contract a protocol decoder is written against, and the host
that runs decoders. A decoder that breaks the contract is refused, never a panic.

On a phone or tablet, a tap on a reference or a chart's point shows its figure without leaving it,
and the next tap closes it. A figure that opens a recording has an Open link under its trace, so a
finger can open it with no note lit, and a chart's figure opened by a tap has one beside the
series' name. A finger held on a reference or a point that turns into a scroll no longer leaves
the figure open.

A view's readout has Clear, which removes the cursors and the selection.

On a narrow screen, a view's key of roles that doesn't fit on the toolbar is cut at its end, so its
first roles stay in view beside the zoom buttons.

## v0.1.1

A recording on another site opens when that site sends `Access-Control-Allow-Origin`, as GitHub
Pages does.

A page opened from disk can load the elements and its recordings from a site.

The elements leave keys held with Alt, Ctrl or Cmd to the browser, so Alt with an arrow goes back or
forward while a view has the focus.

An inset that opens a recording marks where a click or Enter opens it: "Opens here" on the note
lit, or at the start of the span shown when the keys' focus is on it.

## v0.1.0

Initial release.
