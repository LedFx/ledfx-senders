"""Owned Nanoleaf UDP session, with immutable panel layout."""

import socket
from typing import Self

from ledfx_senders import _native
from ledfx_senders.encoders import _rgb
from ledfx_senders.frames import Frame
from ledfx_senders.packet_senders import _integer


class NanoleafSender:
    """Finite RGB levels clamp then truncate; one complete UDP datagram.

    V1 supports 255 panels, v2 8188 (IPv4 UDP payload ceiling). The manufacturer's
    <=10 Hz recommendation is distinct from sender CPU throughput. No automatic
    pacing, discovery, REST activation or HTTP animation is performed here.
    """

    def __init__(
        self, *, destination: str, port: int, version: int, panel_ids: tuple[int, ...]
    ) -> None:
        self._setup(destination, port, version, panel_ids, "socket")

    def _setup(
        self,
        destination: str,
        port: int,
        version: int,
        panel_ids: tuple[int, ...],
        mode: str,
    ) -> None:
        _integer(version, "version", 1, 2)
        _integer(port, "port", 1, 65535)
        if not isinstance(panel_ids, tuple):
            raise TypeError("panel_ids must be an immutable tuple")
        maximum = 255 if version == 1 else 8188
        if not 1 <= len(panel_ids) <= maximum or len(set(panel_ids)) != len(panel_ids):
            raise ValueError("panel IDs must be unique and fit one datagram")
        for panel_id in panel_ids:
            _integer(panel_id, "panel ID", 0, 255 if version == 1 else 65535)
        self._count = len(panel_ids)
        address = socket.gethostbyname(destination)
        self._engine = _native.NanoleafEngine(
            f"{address}:{port}", version, panel_ids, mode
        )

    @classmethod
    def _test_sender(
        cls,
        *,
        destination: str,
        port: int,
        version: int,
        panel_ids: tuple[int, ...],
        mode: str,
    ) -> Self:
        sender = cls.__new__(cls)
        sender._setup(destination, port, version, panel_ids, mode)
        return sender

    def send(self, frame: Frame) -> None:
        normalized, _, count = _rgb(frame, self._count, "clip")
        if count != self._count:
            raise ValueError("RGB shape must match every configured panel")
        self._engine.send(normalized)

    def close(self) -> None:
        self._engine.close()

    @property
    def closed(self) -> bool:
        return self._engine.closed
