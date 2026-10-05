"""Owned frame buffers and explicit E1.31 channel conversion policy."""

from dataclasses import dataclass, field
from typing import cast

import numpy as np
from numpy.typing import NDArray

from ledfx_senders import _native as _e131
from ledfx_senders.e131_packet import data_template
from ledfx_senders.frames import Frame

__all__ = ["ChannelLayout", "Frame", "PacketBanks", "normalize_frame"]


@dataclass(frozen=True)
class ChannelLayout:
    channel_count: int
    universe: int = 1
    universe_size: int = 510
    channel_offset: int = 0
    universes: tuple[int, ...] = field(init=False)
    spans: tuple[tuple[int, int, int, int], ...] = field(init=False)

    def __post_init__(self) -> None:
        for name in ("channel_count", "universe", "universe_size", "channel_offset"):
            if type(getattr(self, name)) is not int:
                raise TypeError(f"{name} must be an integer")
        if self.channel_count < 1 or self.channel_offset < 0:
            raise ValueError("channel_count must be positive and offset nonnegative")
        if not 1 <= self.universe_size <= 512 or not 1 <= self.universe <= 63999:
            raise ValueError("invalid universe or universe_size")
        count = (self.channel_offset + self.channel_count - 1) // self.universe_size + 1
        if self.universe + count - 1 > 63999:
            raise ValueError("layout exceeds E1.31 universe range")
        spans = []
        for packet in range(count):
            start = max(self.channel_offset, packet * self.universe_size)
            end = min(
                self.channel_offset + self.channel_count,
                (packet + 1) * self.universe_size,
            )
            if end > start:
                spans.append(
                    (
                        packet,
                        start - self.channel_offset,
                        start % self.universe_size,
                        end - start,
                    )
                )
        object.__setattr__(
            self, "universes", tuple(range(self.universe, self.universe + count))
        )
        object.__setattr__(self, "spans", tuple(spans))


def normalize_frame(frame: Frame, channel_count: int) -> Frame:
    """Validate uncommon formats before narrowing; common buffers pass through.

    Float levels must be finite and strictly between -1 and 256, and truncate
    toward zero. Callers must not mutate input until the native copy returns.
    """
    if isinstance(frame, (bytes, bytearray, memoryview)):
        try:
            view = memoryview(frame)
            if not view.c_contiguous or view.format != "B" or view.itemsize != 1:
                raise ValueError("frame must be a contiguous unsigned-byte buffer")
            if view.nbytes != channel_count:
                raise ValueError("wrong channel count")
        except (ValueError, TypeError) as exc:
            raise ValueError("invalid byte buffer") from exc
        return frame
    if not isinstance(frame, np.ndarray):
        raise TypeError("frame must be an ndarray or byte buffer")
    if frame.size != channel_count:
        raise ValueError("wrong channel count")
    dtype = frame.dtype
    if dtype.kind not in "buif":
        raise ValueError("unsupported channel dtype")
    # Dtype equality is not buffer-format identity: on some platforms
    # longdouble equals float64 but exports PEP 3118 'g', not native f64 'd'.
    if dtype.isnative and dtype.char in ("B", "f", "d"):
        return (
            frame
            if frame.flags.c_contiguous and frame.flags.aligned
            else np.require(frame, requirements=["C", "A"])
        )
    numeric = cast(NDArray[np.number | np.bool_], frame)
    if dtype.kind == "f":
        valid = np.isfinite(numeric) & (numeric > -1) & (numeric < 256)
    else:
        valid = (numeric >= 0) & (numeric <= 255)
    if not np.all(valid):
        raise ValueError("channels must be finite and strictly between -1 and 256")
    return np.ascontiguousarray(frame, dtype=np.uint8)


class PacketBanks:
    """Setup facade; native storage is exposed for the subsequent sender engine."""

    def __init__(self, layout: ChannelLayout, *, fill: int = 0) -> None:
        self.layout = layout
        templates = []
        for universe in layout.universes:
            packet = data_template(
                universe,
                cid=bytes(16),
                source_name="LedFx",
                priority=100,
                sync_universe=0,
            )
            packet[126:] = bytes([fill]) * 512
            templates.append(bytes(packet))
        self.native = _e131.PacketBanks(templates, layout.spans, layout.channel_count)

    def update(self, frame: Frame) -> None:
        self.native.update(normalize_frame(frame, self.layout.channel_count))

    def snapshot(self) -> tuple[bytes, ...]:
        return self.native.snapshot()

    def capacities(self) -> tuple[int, ...]:
        return tuple(self.native.capacities())
