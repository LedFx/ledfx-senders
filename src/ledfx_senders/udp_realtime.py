"""WLED realtime sender with original-value traffic suppression."""

from typing import Self

from ledfx_senders._sender import _StatefulSender


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
