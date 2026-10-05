"""Owned vendor frames; serial, SDK and encrypted sessions stay with callers.

RGB ndarrays have shape (N, 3). Unsigned byte buffers contain packed RGB.
Wrapping protocols truncate finite floats in [-2**31, 2**31) modulo 256;
finite floats outside that domain become zero. Nonfinite levels are rejected.
Hue instead preserves int-then-byte range checks and exception precedence.
Callers must not mutate input until encoding returns.
"""

from typing import Literal, cast
from uuid import UUID

import numpy as np
from numpy.typing import NDArray

from ledfx_senders import _native
from ledfx_senders.frames import Frame
from ledfx_senders.packet_senders import _integer

Policy = Literal["wrap", "strict", "clip"]


def _rgb(frame: Frame, limit: int, policy: Policy) -> tuple[Frame, int, int]:
    if isinstance(frame, (bytes, bytearray, memoryview)):
        view = memoryview(frame)
        if not view.c_contiguous or view.format != "B" or view.itemsize != 1:
            raise ValueError("expected contiguous unsigned bytes")
        count, tail = divmod(view.nbytes, 3)
        if tail or not 1 <= count <= limit:
            raise ValueError("invalid RGB pixel count")
        return frame, 0, count
    if not isinstance(frame, np.ndarray):
        raise TypeError("expected RGB ndarray or unsigned byte buffer")
    if frame.ndim != 2 or frame.shape[1] != 3 or not 1 <= len(frame) <= limit:
        raise ValueError("expected representable RGB shape (N, 3)")
    dtype = frame.dtype
    if dtype.kind not in "buif":
        raise ValueError("unsupported channel dtype")
    if dtype.isnative and dtype.char in "Bfd":
        normalized = (
            frame
            if frame.flags.c_contiguous and frame.flags.aligned
            else np.require(frame, requirements=["C", "A"])
        )
        return normalized, {"B": 0, "f": 1, "d": 2}[dtype.char], len(frame)
    numeric = cast(NDArray[np.number | np.bool_], frame)
    finite = np.isfinite(numeric)
    if not finite.all():
        if policy == "strict":
            # Historical Hue first converts every channel with int(), before
            # appending any channel bytes. A later nonfinite value therefore
            # takes precedence over an earlier finite out-of-range value.
            int(numeric.ravel()[np.flatnonzero(~finite)[0]])
        raise ValueError("nonfinite channel level")
    if policy == "strict":
        valid = (
            ((numeric > -1) & (numeric < 256))
            if dtype.kind == "f"
            else ((numeric >= 0) & (numeric <= 255))
        )
        if not valid.all():
            raise ValueError("channel outside byte range")
    elif policy == "clip":
        frame = np.clip(frame, 0, 255)
    elif dtype.kind == "f":
        wide = np.asarray(frame, dtype=np.longdouble)
        valid = (wide >= np.longdouble(-2147483648)) & (
            wide < np.longdouble(2147483648)
        )
        frame = np.remainder(
            np.where(valid, np.trunc(wide), np.longdouble(0)), np.longdouble(256)
        )
    return np.ascontiguousarray(frame, dtype=np.uint8), 0, len(frame)


def encode_adalight(frame: Frame, color_order: str) -> bytes:
    """Adafruit inclusive u16 count (N-1), checksum and ordered RGB."""
    if color_order not in ("RGB", "RBG", "GRB", "GBR", "BRG", "BGR"):
        raise ValueError("invalid RGB order")
    data, kind, count = _rgb(frame, 65536, "wrap")
    return _native.encode_adalight(data, kind, count, color_order)


def encode_openrgb(frame: Frame, device_id: int) -> bytes:
    """OpenRGB protocol-v3 UPDATELEDS packet for a little-endian session."""
    _integer(device_id, "device_id", 0, 2**32 - 1)
    data, kind, count = _rgb(frame, 65535, "wrap")
    return _native.encode_openrgb(data, kind, count, device_id)


def encode_hue(
    frame: Frame, entertainment_id: str, channel_ids: tuple[int, ...], sequence: int
) -> bytes:
    """HueStream v2 bytes for the caller's DTLS session."""
    if not isinstance(entertainment_id, str) or len(entertainment_id) != 36:
        raise ValueError("entertainment_id must be an ASCII UUID")
    try:
        if str(UUID(entertainment_id)) != entertainment_id.lower():
            raise ValueError("invalid UUID")
        identifier = entertainment_id.encode("ascii")
    except (ValueError, UnicodeError) as exc:
        raise ValueError("entertainment_id must be an ASCII UUID") from exc
    _integer(sequence, "sequence", 0, 255)
    if not isinstance(channel_ids, tuple):
        raise TypeError("channel_ids must be an immutable tuple")
    for channel in channel_ids:
        _integer(channel, "channel ID", 0, 255)
    data, kind, count = _rgb(frame, 256, "strict")
    if len(channel_ids) != count or len(set(channel_ids)) != count:
        raise ValueError("channel IDs must uniquely match RGB pixels")
    return _native.encode_hue(data, kind, count, identifier, channel_ids, sequence)


def encode_govee(frame: Frame, stretch: bool) -> bytes:
    """Complete current LedFx Razer JSON envelope for the existing socket."""
    if type(stretch) is not bool:
        raise TypeError("stretch must be bool")
    data, kind, count = _rgb(frame, 255, "wrap")
    return _native.encode_govee(data, kind, count, stretch)


class RGBGather:
    """Cached RGB permutation; SDK authentication and chunking remain external."""

    def __init__(self, permutation: tuple[int, ...]) -> None:
        if not isinstance(permutation, tuple):
            raise TypeError("permutation must be an immutable tuple")
        count = len(permutation)
        if not 1 <= count <= 1_000_000:
            raise ValueError("invalid RGB count")
        for index in permutation:
            _integer(index, "pixel index", 0, count - 1)
        if len(set(permutation)) != count:
            raise ValueError("indices must form a complete permutation")
        self._count = count
        self._engine = _native.RGBGather(permutation)

    def encode(self, frame: Frame) -> bytes:
        normalized, kind, count = _rgb(frame, self._count, "wrap")
        if count != self._count:
            raise ValueError("RGB shape must match permutation")
        return self._engine.encode(normalized, kind)
