# escapement-tickwave-bench

Instruction-count benches for the writer, repair and the view, run under Callgrind by
[gungraun](https://github.com/gungraun/gungraun). Each bench covers one pattern that has cost a
round of measurement on full-size captures: the per-item path, a block whose edge step shrinks
late, a flush after a long idle span, a snapshot with marker lines pending, one compression slice
of each kind of data, repair of a cut file, warm and cold queries, statistics that sum each sample,
and markers by range. All of them run in about a minute, and a count moves by at most a few
hundredths of a percent from run to run, so a change of a percent shows. Confirm a change on full-size
captures before you trust it: counts don't model the page cache, the other threads or the 2 ms
bound on a writer's call.

The browser's benchmark is a separate thing, in `bench/` at the repository's root.

## Running

Needs Valgrind and `gungraun-runner` at the version in `Cargo.lock`
(`cargo install gungraun-runner --version <it>`).

    cargo bench -p escapement-tickwave-bench --target x86_64-unknown-linux-musl

They are built static against musl (`rustup target add x86_64-unknown-linux-musl`, and `musl-gcc`
for zstd's C code), so no system library runs under Valgrind: a dynamic loader built with
AVX-512, as some distributions build glibc, stops Valgrind at its first instruction. Don't build
them with `-C target-cpu=native` either. musl's allocator counts differently from glibc's, so
compare counts with counts from this target only.

Each run compares with the last. To keep a reference across several changes:

    cargo bench ... -- --save-baseline=before
    cargo bench ... -- --baseline=before

A filter runs some benches only: `-- '*statistics*'`.

## What it writes

The recordings are written once into `target/tmp/` by `src/lib.rs`, the first time a bench needs
one (under Valgrind, so that first run takes a few seconds more); delete them to write them again.
Each bench's Callgrind output and a flamegraph of it are under `target/gungraun/`; open the output
with `callgrind_annotate` or KCachegrind to see where the instructions went.
