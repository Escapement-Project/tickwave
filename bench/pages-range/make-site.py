# /// script
# dependencies = []
# ///
"""Builds the throwaway Pages site for the byte-range check: random files under each candidate
extension, a page that fetches ranges from them, and files.json with the SHA-256 of every range
the page asks for, computed from the local copies.

    uv run make-site.py OUT_DIR
"""
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

FILES = [("small.tickwave", 1 << 20), ("small.bin", 1 << 20), ("small.zst", 1 << 20), ("large.tickwave", 60 << 20)]


def ranges(size: int) -> list[tuple[int, int]]:
    """Inclusive byte ranges: the head a reader fetches first, a block mid-file, the file's end."""
    mid = size // 2
    return [(0, 65535), (mid, mid + min(size // 4, 1 << 20) - 1), (size - 1000, size - 1)]


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    manifest = []
    for name, size in FILES:
        path = out / name
        if not path.exists() or path.stat().st_size != size:
            path.write_bytes(os.urandom(size))
        data = path.read_bytes()
        manifest.append({
            "name": name,
            "size": size,
            "ranges": [{"start": a, "end": b, "sha256": hashlib.sha256(data[a:b + 1]).hexdigest()} for a, b in ranges(size)],
        })
    (out / "files.json").write_text(json.dumps(manifest, indent=1))
    shutil.copy(Path(__file__).with_name("check.html"), out / "index.html")
    (out / ".nojekyll").write_text("")


if __name__ == "__main__":
    main()
