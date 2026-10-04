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
    if not opc and dtype.kind == "f":
        # Explicit DDP float policy, independent of undefined NumPy float->u8
        # casts outside the byte range. Widen without losing long-double bits.
        wide = np.asarray(frame, dtype=np.longdouble)
        in_range = (wide >= np.longdouble(-2147483648)) & (
            wide < np.longdouble(2147483648)
        )
        truncated = np.where(in_range, np.trunc(wide), np.longdouble(0))
        frame = np.remainder(truncated, np.longdouble(256))
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


class _StatefulSender:
    _protocol: ClassVar[str]

    def send(self, frame: Frame, now: float) -> None:
        from ledfx_senders.original import original

        normalized, kind = original(frame, self.channel_count)
        self._engine.send(normalized, kind, now)

    def close(self) -> None:
        self._engine.close()

    @property
    def closed(self) -> bool:
        return self._engine.closed

    def _initialize_stateful(
        self,
        *,
        destination: str,
        port: int,
        pixel_count: int,
        mode_name: str,
        paths: list[bytes],
        timeout: int,
        minimise: bool,
        interval: float,
        mode: str,
        backend: str,
        batch_size: int,
        override_destination: str | None,
    ) -> None:
        _integer(
            pixel_count,
            "pixel_count",
            1,
            1_000_000 if self._protocol == "osc" else 65536,
        )
        _integer(port, "port", 1, 65535)
        _integer(timeout, "timeout", 1, 255)
        self.channel_count = pixel_count * 3
        address = socket.gethostbyname(destination)
        self._engine = _native.StatefulEngine(
            self._protocol,
            pixel_count,
            f"{address}:{port}",
            mode_name,
            paths,
            timeout,
            minimise,
            interval,
            mode,
            backend,
            batch_size,
            override_destination,
        )


class OSCSender(_StatefulSender):
    """OSC arrays/floats, signed-i64 truncation and exact original-value suppression.

    All floating inputs must be finite and in [-2**63, 2**63). Equal values across
    dtypes (including signed zero) suppress; lossy NumPy promotion does not.
    Callers must not mutate an input while send is executing.
    """

    _protocol = "osc"

    def __init__(
        self,
        *,
        destination: str,
        pixel_count: int,
        path: str,
        send_type: str,
        port: int = 9000,
        starting_addr: int = 0,
    ) -> None:
        self._setup(
            destination=destination,
            pixel_count=pixel_count,
            path=path,
            send_type=send_type,
            port=port,
            starting_addr=starting_addr,
        )

    def _setup(
        self,
        *,
        destination: str,
        pixel_count: int,
        path: str,
        send_type: str,
        port: int = 9000,
        starting_addr: int = 0,
        mode: str = "socket",
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> None:
        _integer(pixel_count, "pixel_count", 1, 1_000_000)
        if type(starting_addr) is not int or starting_addr < 0:
            raise ValueError("starting_addr must be a nonnegative integer")
        count = (
            pixel_count * 3
            if send_type == "Three_Addresses"
            else 1
            if send_type == "All_To_One"
            else pixel_count
        )
        paths = [
            path.format(address=starting_addr + i).encode("utf-8") for i in range(count)
        ]
        self._initialize_stateful(
            destination=destination,
            port=port,
            pixel_count=pixel_count,
            mode_name=send_type,
            paths=paths,
            timeout=1,
            minimise=True,
            interval=0,
            mode=mode,
            backend=backend,
            batch_size=batch_size,
            override_destination=override_destination,
        )

    @classmethod
    def _test_sender(
        cls,
        *,
        destination: str,
        pixel_count: int,
        path: str,
        send_type: str,
        port: int = 9000,
        starting_addr: int = 0,
        mode: str,
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> Self:
        sender = cls.__new__(cls)
        sender._setup(
            destination=destination,
            pixel_count=pixel_count,
            path=path,
            send_type=send_type,
            port=port,
            starting_addr=starting_addr,
            mode=mode,
            backend=backend,
            batch_size=batch_size,
            override_destination=override_destination,
        )
        return sender


class UDPRealtimeSender(_StatefulSender):
    """WLED realtime with exact original deltas and whole-frame refresh.

    now uses a monotonic clock. Floating encoding follows the deterministic DDP
    wrap policy, independently of original-value comparisons. No input aliases
    survive send; callers must not mutate input during send.
    """

    _protocol = "realtime"

    def __init__(
        self,
        *,
        destination: str,
        pixel_count: int,
        packet_type: str,
        timeout: int,
        minimise_traffic: bool,
        port: int = 21324,
        keepalive_interval: float | None = None,
    ) -> None:
        self._setup(
            destination=destination,
            pixel_count=pixel_count,
            packet_type=packet_type,
            timeout=timeout,
            minimise_traffic=minimise_traffic,
            port=port,
            keepalive_interval=keepalive_interval,
        )

    def _setup(
        self,
        *,
        destination: str,
        pixel_count: int,
        packet_type: str,
        timeout: int,
        minimise_traffic: bool,
        port: int = 21324,
        keepalive_interval: float | None = None,
        mode: str = "socket",
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> None:
        self._initialize_stateful(
            destination=destination,
            port=port,
            pixel_count=pixel_count,
            mode_name=packet_type,
            paths=[],
            timeout=timeout,
            minimise=minimise_traffic,
            interval=timeout / 2 if keepalive_interval is None else keepalive_interval,
            mode=mode,
            backend=backend,
            batch_size=batch_size,
            override_destination=override_destination,
        )

    @classmethod
    def _test_sender(
        cls,
        *,
        destination: str,
        pixel_count: int,
        packet_type: str,
        timeout: int,
        minimise_traffic: bool,
        port: int = 21324,
        keepalive_interval: float | None = None,
        mode: str,
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> Self:
        sender = cls.__new__(cls)
        sender._setup(
            destination=destination,
            pixel_count=pixel_count,
            packet_type=packet_type,
            timeout=timeout,
            minimise_traffic=minimise_traffic,
            port=port,
            keepalive_interval=keepalive_interval,
            mode=mode,
            backend=backend,
            batch_size=batch_size,
            override_destination=override_destination,
        )
        return sender
