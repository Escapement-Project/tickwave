# /// script
# requires-python = ">=3.11"
# ///
"""Writes what libsigrok reads from each .sr fixture here, for the chrono-import tests.

    uv run make-oracles.py [FILE.sr ...]       (from this directory; default: every .sr here)

For each <name>.sr, through sigrok-cli (0.7.2 here, libsigrok 0.5.2-git-9a10edf9):
- <name>.oracle.txt: the samplerate and logic sample count from `--show`; each logic channel's
  initial level and edges in samples, from `-O bits`; each analog channel's values as f32 bit
  patterns, from `-O wav`, which writes them unchanged as IEEE floats.
- <name>.vcd: the `-O vcd` output, a VCD fixture in its own right, written only when its edges
  agree with `-O bits` for every channel. sigrok-cli writes analog samples into it as text lines
  ("V: 3.26 V DC"), which are not VCD; those lines are removed.

`-O bits` takes probe N from bit N-1, as libsigrok's session loader numbers channels. `-O vcd`
takes the enabled channels' bits as packed together, as libsigrok's demo driver packs them. The
two disagree on a file with disabled channels, demo-disabled.sr, whose writer packed them.

Where the fixtures came from:
- marker.sr, marker.csv: verge's sigrok export of a marker shot, and its CSV
- markers-1000.sr, markers-1000.csv: verge's sigrok export of 1,000 markers, and its CSV
- demo-mixed.sr: sigrok-cli -d demo:logic_channels=12:analog_channels=2
      --config samplerate=31500 --samples 3000
- demo-disabled.sr: the same, with -C D1,D5,D10,A1
- demo-analog.sr: sigrok-cli -d demo:logic_channels=0:analog_channels=1
      --config samplerate=1000000 --samples 1500
- demo-logic.sr: sigrok-cli -d demo:logic_channels=8:analog_channels=0
      --config samplerate=2500000 --samples 5000
"""

import re
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


def sigrok(*args: str, check: bool = True) -> str:
    return subprocess.run(
        ["sigrok-cli", *args], check=check, capture_output=True, text=True
    ).stdout


def show(path: Path) -> dict:
    out = sigrok("-i", str(path), "--show")
    info = {"logic": [], "analog": [], "logic_samples": 0}
    for line in out.splitlines():
        if m := re.fullmatch(r"Samplerate: (\d+)", line):
            info["samplerate"] = int(m[1])
        elif m := re.fullmatch(r"- (.+): (logic|analog)", line):
            info[m[2]].append(m[1])
        elif m := re.fullmatch(r"Logic sample count: (\d+)", line):
            info["logic_samples"] = int(m[1])
    return info


def vcd(path: Path, out: Path) -> tuple[str, int]:
    with tempfile.TemporaryDirectory() as tmp:
        raw = Path(tmp) / "out.vcd"
        sigrok("-i", str(path), "-O", "vcd", "-o", str(raw))
        lines = raw.read_text().splitlines(keepends=True)
    kept = [l for l in lines if not re.fullmatch(r"\S+: .* V DC\n?", l)]
    out.write_text("".join(kept))
    text = "".join(kept)
    m = re.search(r"\$timescale (\d+) (s|ms|us|ns|ps|fs) \$end", text)
    per_second = {"s": 1, "ms": 10**3, "us": 10**6, "ns": 10**9, "ps": 10**12, "fs": 10**15}
    return text, per_second[m[2]] // int(m[1])


def vcd_edges(text: str, rate: int, ticks_per_second: int) -> dict:
    """Each channel's (initial, edges), times converted back to sample numbers."""
    names = dict(re.findall(r"\$var wire 1 (\S+) (\S+) \$end", text))
    body = text.split("$enddefinitions $end", 1)[1]
    levels, initial, edges = {}, {}, {n: [] for n in names.values()}
    time = None
    for token in body.split():
        if token.startswith("#"):
            ts = int(token[1:])
            sample = round(ts * rate / ticks_per_second)
            # sigrok-cli writes round(sample / rate * timescale).
            assert round(sample * ticks_per_second / rate) == ts, (ts, sample)
            time = sample
        else:
            value, code = int(token[0]), token[1:]
            name = names[code]
            if time == 0:
                initial[name] = value
            elif levels[name] != value:
                edges[name].append(time)
            levels[name] = value
    return {name: (initial[name], edges[name]) for name in names.values()}


def bits_edges(path: Path, names: list) -> dict:
    """Each logic channel's (initial, edges), from `-O bits` with one line a channel."""
    out = sigrok("-i", str(path), "-O", "bits:width=0")
    result = {}
    for line in out.splitlines():
        m = re.fullmatch(r"(\S+):([01 ]+)", line)
        if not m or m[1] not in names:
            continue
        level = [int(b) for b in m[2] if b != " "]
        changes = [i for i in range(1, len(level)) if level[i] != level[i - 1]]
        assert m[1] not in result, f"{m[1]}: more than one line"
        result[m[1]] = (level[0], changes, len(level))
    return result


def wav_values(path: Path, channels: int) -> list:
    with tempfile.TemporaryDirectory() as tmp:
        out = Path(tmp) / "out.wav"
        sigrok("-i", str(path), "-O", "wav", "-o", str(out))
        data = out.read_bytes()
    assert data[:4] == b"RIFF" and data[8:12] == b"WAVE"
    pos, fmt, samples = 12, None, None
    while pos < len(data):
        kind, size = data[pos : pos + 4], struct.unpack_from("<I", data, pos + 4)[0]
        body = data[pos + 8 :]
        if kind == b"fmt ":
            fmt = struct.unpack_from("<HHIIHH", body)
        elif kind == b"data":
            samples = body  # sigrok-cli leaves the data size as 0xffffffff
            break
        pos += 8 + size + (size & 1)
    format_tag, nchannels, _, _, _, bits = fmt
    assert format_tag == 3 and bits == 32 and nchannels == channels, fmt
    words = struct.unpack(f"<{len(samples) // 4}I", samples[: len(samples) // 4 * 4])
    return [words[c::channels] for c in range(channels)]


def member_values(path: Path, names: list) -> list:
    """Each analog channel's members in chunk order, as f32 bit patterns."""
    with zipfile.ZipFile(path) as z:
        meta = z.read("metadata").decode()
        index = {name: n for n, name in re.findall(r"^analog(\d+)=(.*)$", meta, re.M)}
        result = []
        for name in names:
            words, chunk = [], 1
            while f"analog-1-{index[name]}-{chunk}" in z.namelist():
                data = z.read(f"analog-1-{index[name]}-{chunk}")
                words += struct.unpack(f"<{len(data) // 4}I", data)
                chunk += 1
            result.append(tuple(words))
    return result


def printed_values(path: Path, names: list) -> dict:
    """`-O analog`'s values, printed to two decimals."""
    # sigrok-cli can fail an assertion on exit after printing everything; the caller compares
    # every value, so a short output still fails.
    out = sigrok("-i", str(path), "-O", "analog", check=False)
    printed = {name: [] for name in names}
    for line in out.splitlines():
        if m := re.fullmatch(r"(\S+): (-?\d+\.\d\d) V DC", line):
            printed[m[1]].append(m[2])
    return printed


def oracle(path: Path) -> None:
    info = show(path)
    rate, count = info["samplerate"], info["logic_samples"]
    lines = [
        f"# from {path.name} by make-oracles.py through sigrok-cli",
        f"samplerate {rate}",
        f"logic_samples {count}",
    ]
    # Channels libsigrok names by number have no probeN key: they were disabled when saved.
    logic = [name for name in info["logic"] if not name.isdigit()]
    if logic:
        bits = bits_edges(path, logic)
        vcd_path = path.with_suffix(".vcd")
        text, ticks = vcd(path, vcd_path)
        from_vcd = vcd_edges(text, rate, ticks)
        disagree = [n for n in logic if from_vcd[n] != bits[n][:2]]
        if disagree:
            vcd_path.unlink()
            lines.append(f"# -O vcd disagrees with -O bits on {' '.join(disagree)}; no .vcd written")
        for name in logic:
            initial, edges, samples = bits[name]
            assert samples == count, (name, samples, count)
            lines.append(" ".join(["digital", name, str(initial), *map(str, edges)]))
    if info["analog"]:
        names = info["analog"]
        try:
            values = wav_values(path, len(names))
        except subprocess.CalledProcessError:
            # It crashes on demo-mixed.sr, with a varying signal.
            lines.append(
                "# -O wav failed; analog values read from the zip members,"
                " checked against -O analog to two decimals"
            )
            values = member_values(path, names)
        printed = printed_values(path, names)
        for name, words in zip(names, values):
            floats = struct.unpack(f"<{len(words)}f", struct.pack(f"<{len(words)}I", *words))
            assert [f"{x:.2f}" for x in floats] == printed[name], f"{name}: -O analog disagrees"
        for name, words in zip(names, values):
            lines.append(" ".join(["analog", name, *(f"{w:08x}" for w in words)]))
    path.with_suffix(".oracle.txt").write_text("\n".join(lines) + "\n")
    print(f"{path.name}: {len(logic)} logic, {len(info['analog'])} analog, {count} samples")


if __name__ == "__main__":
    here = Path(__file__).parent
    files = [Path(f) for f in sys.argv[1:]] or sorted(here.glob("*.sr"))
    for f in files:
        oracle(f)
