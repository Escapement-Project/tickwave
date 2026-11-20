# escapement-tickwave-decode

The contract a protocol decoder is written against, and the host that runs decoders over a
recording. The reference decoders in `escapement-tickwave-decoders` use the contract alone, as a
decoder loaded at run time would.

## A decoder

A decoder declares a `Definition`. It gives the decoder's name, its major version and the
contract version it targets. It lists the inputs the decoder takes: wire ports bound to a
recording's channels, or the output of another analysis it stacks on. It declares its settings,
each with a default. And it holds the `Schema` of what the decoder emits: kinds of item, their
fields, sequences, links and labels, and the rows and lanes a track draws them in. A port, a
setting and an input may each carry a sentence saying what it is, for a form that binds the decoder.

The `Decoder` a definition's factory makes runs a turn at a time. A turn takes the input that is
ready and emits operations: it opens items, gives them fields and sequences, links them to other
items and closes them. A decoder asks for its input and the host answers between turns, so a
decoder never blocks and never reads a file itself.

## The host

A `Host` adds analyses in order. A wire analysis reads channels of a recording; a stacked one
reads the items another analysis completes. The host checks a definition when it is added, then
every operation against it and against the item's lifecycle. It admits a turn's output whole or
refuses it whole, and a run whose turn it refuses runs no more. A decoder that breaks a rule comes
back as a `Refusal` or a `BindError`, never a panic or a run left waiting: a decoder loaded at run
time is untrusted input, as a file is to a reader. The host's
bounds (items open at once, a batch's size, a schema's names) refuse rather than grow, and a read
gives a decoder a bounded span of input however wide a window it asks for.

The forward pass runs each analysis from the start of the recording. A view sets what each
analysis keeps: whole items only where they meet a span it asked for, or none, with each item
tallied as it closes so a zoomed-out view can count without keeping. As the pass goes, the host
keeps checkpoints within a byte budget. `derive` decodes a window again from the nearest
checkpoint before it, in a window chain of its own beside the forward pass, and stops once
nothing more will start in the window. A window the pass has not reached yet is decoded from its
own start, and its items are provisional: they say what they cannot know without what came before.

`rebind` changes an analysis's settings and decodes its chain again from the start: the wire
analysis under it and everything stacked on that.

## Features

- `wire` (default): a wire analysis's input read from a recording through the view core's
  cursors, merged in exact time order. Blocks come through the recording's cache, so the graph and
  a decoder share what either decoded.
- `live` (default): wire input from a recording a writer in the same process is still writing.
- `recipe`: the analyses to run over a recording, as a recipe's `[[decode]]` tables name them,
  bound through a `Registry` of decoders the host application passes.

With no default features, the crate is the contract and the host alone, and builds for wasm32.

## Recipes

A recipe is a TOML file of `[[decode]]` tables, one per analysis:

```toml
[[decode]]
id = "swd"
decoder = "swd"
version = 1
label = "SWD"
inputs = { SWCLK = "SWCLK", SWDIO = "SWDIO" }

[[decode]]
id = "adi"
decoder = "adiv5"
version = 1
inputs = { transfers = { analysis = "swd" } }
```

`id` names the analysis, as citations do. `decoder` and `version` name its definition, by name
and major version. `label` names its track; without one, the registry's name for the decoder
does. `inputs` binds each port by the definition's name for it: a wire port to a channel by the
channel's name, or a stacked input to another analysis by its id. An optional port may be left
out. `settings` gives the definition's settings by name, a choice by its name, and a setting left
out takes its default.

## Versions

The contract is versioned on its own, apart from how the host schedules, stores and keeps
checkpoints. `CONTRACT` is the version this host runs. A host whose contract is `M.h` runs a
decoder that targets `M.m` for any `m` up to `h`, since a minor version only adds. A host refuses
a decoder that targets any other when the decoder is added, never mid-run. A decoder's own major
version is its output schema's: its kinds and fields keep their meaning within it. A recipe names
the major version it binds.
