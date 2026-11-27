"""Identify encoded images and read their pixel size from the header bytes.

The connector reports and maps coordinates using the size of the image it
actually sends, so it reads that size from the PNG or JPEG header instead of
trusting the scale factor it asked the capture tool for.
"""

from __future__ import annotations

import struct
from typing import Optional, Tuple

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

# JPEG start-of-frame markers that carry the image size: SOF0..SOF15 except
# DHT (0xC4), JPG (0xC8) and DAC (0xCC), which share the 0xCn range.
_SOF_MARKERS = {0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF}


def sniff_mime(data: bytes) -> Optional[str]:
    """MIME type from magic bytes, for the formats the daemon forwards."""
    if data.startswith(PNG_SIGNATURE):
        return "image/png"
    if data.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if data.startswith(b"GIF87a") or data.startswith(b"GIF89a"):
        return "image/gif"
    if len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "image/webp"
    return None


def png_size(data: bytes) -> Tuple[int, int]:
    # Signature (8), IHDR length (4), "IHDR" (4), width (4), height (4).
    if len(data) < 24 or not data.startswith(PNG_SIGNATURE) or data[12:16] != b"IHDR":
        raise ValueError("not a PNG with an IHDR header")
    width, height = struct.unpack(">II", data[16:24])
    return width, height


def jpeg_size(data: bytes) -> Tuple[int, int]:
    if not data.startswith(b"\xff\xd8"):
        raise ValueError("not a JPEG")
    index = 2
    length = len(data)
    while index < length:
        if data[index] != 0xFF:
            raise ValueError("JPEG marker expected")
        while index < length and data[index] == 0xFF:  # fill bytes
            index += 1
        if index >= length:
            break
        marker = data[index]
        index += 1
        if marker == 0x01 or 0xD0 <= marker <= 0xD8:  # TEM, RSTn, SOI: no length
            continue
        if marker in (0xD9, 0xDA):  # EOI or start of scan before any SOF
            break
        if index + 2 > length:
            break
        (segment,) = struct.unpack(">H", data[index:index + 2])
        if segment < 2:
            raise ValueError("bad JPEG segment length")
        if marker in _SOF_MARKERS:
            if index + 7 > length:
                break
            height, width = struct.unpack(">HH", data[index + 3:index + 7])
            return width, height
        index += segment
    raise ValueError("JPEG has no frame header")


def image_size(data: bytes) -> Tuple[str, int, int]:
    """Return (mime, width, height) for a PNG or JPEG image."""
    mime = sniff_mime(data)
    if mime == "image/png":
        width, height = png_size(data)
    elif mime == "image/jpeg":
        width, height = jpeg_size(data)
    else:
        raise ValueError("capture is neither PNG nor JPEG")
    if width <= 0 or height <= 0:
        raise ValueError("image has an empty size")
    return mime, width, height
