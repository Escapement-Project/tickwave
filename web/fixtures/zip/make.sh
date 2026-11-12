#!/bin/sh
# Writes the zip fixtures from the committed recordings and test capture, each zip as a tool
# people use writes it. Needs Info-ZIP's zip, 7z and uv. Run from anywhere:
#
#   sh web/fixtures/zip/make.sh
#
# Times are fixed, so a rerun on this machine writes the same bytes, except encrypted.zip, whose
# encryption header is random. Info-ZIP also records the files' owner, which differs between
# machines.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM
export TZ=UTC

mkdir -p "$work/identify-swd" "$work/set" "$work/capture" "$work/a" "$work/b"
cp "$repo/web/fixtures/identify-swd.tickwave" "$work/identify-swd/"
cat > "$work/identify-swd/identify-swd.annotations.toml" <<'EOF'
format = "tickwave-annotations/1"
recording = "identify-swd.tickwave"
recording_id = "blake3:ccc8b7d8c3e4cb30da7fc6088e54ebeba2858ad402eb3fb5a9aeb50be989accf"

[[annotation]]
id = "from-a-zip"
kind = "note"
start = "100 ms"
label = "Opened from a zip"
EOF
cp "$work/identify-swd/"* "$work/set/"
cp "$repo/web/fixtures/identify-jtag.tickwave" "$work/set/"
cp "$repo/testdata/import/demo-logic.vcd" "$work/capture/"
cp "$repo/web/fixtures/identify-swd.tickwave" "$work/a/"
cp "$repo/web/fixtures/identify-swd.tickwave" "$work/b/"
printf "A capture and its notes.\n" > "$work/README.txt"
find "$work" -exec touch -d '2026-11-12 12:00:00' {} +

cd "$work/identify-swd"
rm -f "$here"/*.zip
# Info-ZIP keeps extended timestamps and the owner in each local header, longer than the central
# directory's copies: a reader that takes a file's start from the central directory is off.
zip -q -0 "$here/stored.zip" identify-swd.tickwave identify-swd.annotations.toml
zip -q -X -9 "$here/deflated.zip" identify-swd.tickwave identify-swd.annotations.toml
zip -q -P secret "$here/encrypted.zip" identify-swd.tickwave
7z a -bso0 -tzip -mm=BZip2 "$here/bzip2.zip" ../capture/demo-logic.vcd
cd "$work"
zip -q -r "$here/set.zip" set
zip -q -r "$here/capture.zip" capture
zip -q -r "$here/duplicate.zip" a b
zip -q -X "$here/nothing.zip" README.txt

# A streaming writer: every file with a data descriptor (flag bit 3), its sizes after its data
# and in the central directory only. macos.zip adds what macOS's Compress adds: the files in
# their folder, and each one's resource fork under __MACOSX/ as ._<name>.
uv run --quiet --no-project python - "$work" "$here" <<'EOF'
import sys, zipfile
work, here = sys.argv[1], sys.argv[2]

class Stream:
    """A file the writer can't seek in, so it writes data descriptors."""
    def __init__(self, f): self.f, self.n = f, 0
    def write(self, b): self.n += len(b); return self.f.write(b)
    def tell(self): return self.n
    def flush(self): self.f.flush()

def add(z, name, data, method):
    info = zipfile.ZipInfo(name, date_time=(2026, 11, 12, 12, 0, 0))
    info.compress_type = method
    with z.open(info, "w") as w:
        w.write(data)

files = [(n, open(f"{work}/identify-swd/{n}", "rb").read()) for n in ("identify-swd.tickwave", "identify-swd.annotations.toml")]
with open(f"{here}/streamed.zip", "wb") as f, zipfile.ZipFile(Stream(f), "w") as z:
    add(z, files[0][0], files[0][1], zipfile.ZIP_STORED)
    add(z, files[1][0], files[1][1], zipfile.ZIP_DEFLATED)
# A resource fork's AppleDouble header: its magic, version 2, sixteen bytes of filler, no entries.
fork = bytes.fromhex("00051607" "00020000") + b"Mac OS X        " + bytes(2)
with open(f"{here}/macos.zip", "wb") as f, zipfile.ZipFile(Stream(f), "w") as z:
    for name, data in files:
        add(z, f"identify-swd/{name}", data, zipfile.ZIP_DEFLATED)
        add(z, f"__MACOSX/identify-swd/._{name}", fork, zipfile.ZIP_DEFLATED)
EOF
ls -l "$here"/*.zip
