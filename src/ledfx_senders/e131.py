"""Synchronous E1.31 sender backed by owned native packet storage."""

import logging
import socket
import time
from collections.abc import Callable
from uuid import UUID, uuid4

from ledfx_senders import _native as _e131
from ledfx_senders import e131_packet
from ledfx_senders.e131_buffer import ChannelLayout, normalize_frame
from ledfx_senders.frames import Frame

_LOGGER = logging.getLogger(__name__)


class E131Sender:
    def __init__(
        self,
        layout: ChannelLayout,
        *,
        destination: str,
        source_name: str,
        priority: int = 100,
        cid: UUID | None = None,
        sync_universe: int = 63999,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._initialize(
            layout, destination, source_name, priority, cid, sync_universe, clock
        )
        self._engine = _e131.Engine(*self._arguments)
        del self._arguments

    def _initialize(
        self,
        layout: ChannelLayout,
        destination: str,
        source_name: str,
        priority: int,
        cid: UUID | None,
        sync_universe: int,
        clock: Callable[[], float],
    ) -> None:
        self.layout = layout
        self._clock = clock
        cid_bytes = (cid or uuid4()).bytes
        templates = [
            bytes(
                e131_packet.data_template(
                    u,
                    cid=cid_bytes,
                    source_name=source_name,
                    priority=priority,
                    sync_universe=sync_universe,
                )
            )
            for u in layout.universes
        ]
        address = (
            None if destination == "multicast" else socket.gethostbyname(destination)
        )
        destinations = [
            f"{address or e131_packet.multicast_address(u)}:5568"
            for u in layout.universes
        ]
        self._arguments = (
            templates,
            layout.spans,
            layout.channel_count,
            destinations,
            bytes(
                e131_packet.sync_template(cid=cid_bytes, sync_universe=sync_universe)
            ),
            list(
                e131_packet.discovery_packets(
                    layout.universes, cid=cid_bytes, source_name=source_name
                )
            ),
        )

    @classmethod
    def _test_sender(
        cls,
        layout: ChannelLayout,
        *,
        destination: str,
        source_name: str,
        mode: str,
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
        priority: int = 100,
        cid: UUID | None = None,
        sync_universe: int = 63999,
        clock: Callable[[], float] = time.monotonic,
    ) -> "E131Sender":
        sender = cls.__new__(cls)
        sender._initialize(
            layout, destination, source_name, priority, cid, sync_universe, clock
        )
        sender._engine = _e131.Engine._test_engine(
            *sender._arguments, mode, backend, batch_size, override_destination
        )
        del sender._arguments
        return sender

    def send(self, frame: Frame) -> None:
        now = self._clock()
        self._engine.send(normalize_frame(frame, self.layout.channel_count), now)

    def service(self, now: float | None = None) -> None:
        self._engine.service(self._clock() if now is None else now)

    def close(self, blackout: bool = True) -> None:
        self._engine.close(blackout, self._clock())
        if error := self._engine.cleanup_error():
            _LOGGER.warning("E1.31 cleanup failed: %s", error)

    @property
    def closed(self) -> bool:
        return self._engine.closed
