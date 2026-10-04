"""Synchronous DDP and OPC senders with owned native UDP transports."""

import socket
from typing import ClassVar, Self, cast

import numpy as np
from numpy.typing import NDArray

from ledfx_senders import _native
from ledfx_senders.e131_buffer import Frame


def _integer(value: int, name: str, low: int, high: int) -> None:
    if type(value) is not int:
        raise TypeError(f"{name} must be an integer")
    if not low <= value <= high:
        raise ValueError(f"{name} must be between {low} and {high}")


def _normalize(frame: Frame, count: int, opc: bool) -> Frame:
    if isinstance(frame, (bytes, bytearray, memoryview)):
        view = memoryview(frame)
        if not view.c_contiguous or view.format != "B" or view.nbytes != count:
            raise ValueError(
                "frame must contain the configured number of unsigned bytes"
            )
        return frame
    if not isinstance(frame, np.ndarray):
        raise TypeError("frame must be an ndarray or byte buffer")
    if frame.size != count or (opc and frame.shape != (count // 3, 3)):
        raise ValueError("data must contain the configured number of RGB pixels")
    dtype = frame.dtype
    if dtype.kind not in "buif":
        raise ValueError("unsupported channel dtype")
    # PEP3118 identity, not dtype equality: longdouble may compare equal to f64
    # on some platforms while exporting a different buffer format.
    if dtype.isnative and dtype.char in "Bfd":
        return (
            frame
            if frame.flags.c_contiguous and frame.flags.aligned
            else np.require(frame, requirements=["C", "A"])
        )
    numeric = cast(NDArray[np.number | np.bool_], frame)
    finite = np.isfinite(numeric)
    if not finite.all():
        if opc:
            # Preserve the original first-invalid int() exception (NaN vs inf).
            int(numeric.ravel()[np.flatnonzero(~finite)[0]])
        raise ValueError("nonfinite channel level")
    return np.ascontiguousarray(
        np.clip(frame, 0, 255) if opc else frame, dtype=np.uint8
    )


class _PacketSender:
    _protocol: ClassVar[str]

    def _initialize(
        self,
        count: int,
        destination: str,
        port: int,
        identifier: int,
        mode: str = "socket",
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> None:
        opc = self._protocol == "opc"
        _integer(
            count,
            "pixel_count" if opc else "channel_count",
            1,
            21834 if opc else 2**32 - 1,
        )
        _integer(port, "port", 1, 65535)
        _integer(
            identifier, "channel" if opc else "destination_id", 0 if opc else 1, 255
        )
        address = socket.gethostbyname(destination)
        self.channel_count = count * 3 if opc else count
        self._engine = _native.PacketEngine(
            self._protocol,
            count,
            f"{address}:{port}",
            identifier,
            mode,
            backend,
            batch_size,
            override_destination,
        )

    @classmethod
    def _test_sender(
        cls,
        count: int,
        *,
        destination: str,
        mode: str,
        port: int | None = None,
        destination_id: int = 1,
        channel: int = 0,
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> Self:
        sender = cls.__new__(cls)
        opc = cls._protocol == "opc"
        sender._initialize(
            count,
            destination,
            port if port is not None else (7890 if opc else 4048),
            channel if opc else destination_id,
            mode,
            backend,
            batch_size,
            override_destination,
        )
        return sender

    def send(self, frame: Frame) -> None:
        self._engine.send(
            _normalize(frame, self.channel_count, self._protocol == "opc")
        )

    def close(self) -> None:
        self._engine.close()

    @property
    def closed(self) -> bool:
        return self._engine.closed


class DDPSender(_PacketSender):
    """DDP uint8 wrapping, 1440-byte chunks, final PUSH and sequences 1..15."""

    _protocol = "ddp"

    def __init__(
        self,
        channel_count: int,
        *,
        destination: str,
        port: int = 4048,
        destination_id: int = 1,
    ) -> None:
        self._initialize(channel_count, destination, port, destination_id)


class OPCSender(_PacketSender):
    """One UDP OPC packet; finite RGB values clamp then truncate."""

    _protocol = "opc"

    def __init__(
        self, pixel_count: int, *, destination: str, port: int = 7890, channel: int = 0
    ) -> None:
        self._initialize(pixel_count, destination, port, channel)
