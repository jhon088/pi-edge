"""Reading, writing and cutting out photographs, in pure Python.

Shared by the two scripts that prepare the retail lane's artwork. No Pillow,
no ImageMagick: `sips` converts whatever the source format is into a PNG, and
everything after that is arithmetic.
"""

from __future__ import annotations

import struct
import subprocess
import zlib
from pathlib import Path


# ----------------------------------------------------------------- png io


def read_png(path: Path):
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    pos, idat, width, height, depth, kind = 8, b"", 0, 0, 0, 0
    while pos < len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        name = data[pos + 4 : pos + 8]
        payload = data[pos + 8 : pos + 8 + length]
        if name == b"IHDR":
            width, height, depth, kind = struct.unpack(">IIBB", payload[:10])
        elif name == b"IDAT":
            idat += payload
        pos += 12 + length
    if depth != 8:
        raise ValueError(f"{path}: {depth}-bit is not supported")
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[kind]
    raw = zlib.decompress(idat)
    stride = width * channels
    out = bytearray(height * stride)
    previous = bytearray(stride)
    at = 0
    for y in range(height):
        filter_kind = raw[at]
        at += 1
        line = bytearray(raw[at : at + stride])
        at += stride
        if filter_kind == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 255
        elif filter_kind == 2:
            for i in range(stride):
                line[i] = (line[i] + previous[i]) & 255
        elif filter_kind == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + previous[i]) >> 1)) & 255
        elif filter_kind == 4:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                up = previous[i]
                upleft = previous[i - channels] if i >= channels else 0
                pa, pb, pc = (
                    abs(up - upleft),
                    abs(left - upleft),
                    abs(left + up - 2 * upleft),
                )
                best = left if (pa <= pb and pa <= pc) else (up if pb <= pc else upleft)
                line[i] = (line[i] + best) & 255
        out[y * stride : (y + 1) * stride] = line
        previous = line
    return width, height, channels, bytes(out)


def write_png(path: Path, width: int, height: int, pixels) -> None:
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for x in range(width):
            r, g, b = pixels[y * width + x]
            raw.append(max(0, min(255, int(r))))
            raw.append(max(0, min(255, int(g))))
            raw.append(max(0, min(255, int(b))))

    def chunk(name: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + name
            + payload
            + struct.pack(">I", zlib.crc32(name + payload) & 0xFFFFFFFF)
        )

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def as_png(source: Path, into: Path) -> Path:
    """Whatever the photograph is, as a PNG this script can read."""
    target = into / (source.stem + ".png")
    subprocess.run(
        ["sips", "-s", "format", "png", str(source), "--out", str(target)],
        check=True,
        capture_output=True,
    )
    return target


# --------------------------------------------------------------- cut-outs


def cut_out(width, height, channels, pixels):
    """Alpha for a packshot on white, by flooding in from the border.

    Only white *connected to the edge* is background, so the white inside a
    label or a bottle cap stays where it is. The ramp keeps the edge soft
    rather than cut with scissors.
    """
    alpha = [1.0] * (width * height)

    def whiteness(index):
        base = index * channels
        r, g, b = pixels[base], pixels[base + 1], pixels[base + 2]
        return min(r, g, b)

    # Flood from every border pixel that is near-white.
    stack = []
    seen = bytearray(width * height)
    for x in range(width):
        stack.append(x)
        stack.append((height - 1) * width + x)
    for y in range(height):
        stack.append(y * width)
        stack.append(y * width + width - 1)

    while stack:
        index = stack.pop()
        if seen[index]:
            continue
        seen[index] = 1
        value = whiteness(index)
        if value < 228:
            continue
        # 228..248 ramps the edge; above that it is plainly background.
        alpha[index] = max(0.0, min(1.0, (248 - value) / 20.0))
        y, x = divmod(index, width)
        if x > 0:
            stack.append(index - 1)
        if x + 1 < width:
            stack.append(index + 1)
        if y > 0:
            stack.append(index - width)
        if y + 1 < height:
            stack.append(index + width)
    return alpha


def erase(alpha, width, height, boxes):
    """Blank a rectangle of the source: what is there is not wanted."""
    for x0, y0, x1, y1 in boxes:
        for y in range(int(y0 * height), int(y1 * height)):
            row = y * width
            for x in range(int(x0 * width), int(x1 * width)):
                alpha[row + x] = 0.0
