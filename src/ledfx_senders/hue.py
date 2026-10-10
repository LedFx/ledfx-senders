"""Owned synchronous HueStream DTLS sender with explicit caller-driven service."""

import ipaddress
import math
from uuid import UUID

from ledfx_senders import _native
from ledfx_senders._validation import _integer
from ledfx_senders.encoders import _rgb
from ledfx_senders.frames import Frame


class HueSender:
    """A numeric-address DTLS session; construction performs no network I/O."""

    def __init__(
        self,
        *,
        destination: str,
        psk_identity: bytes,
        client_key: bytes,
        entertainment_id: str,
        channel_ids: tuple[int, ...],
        port: int = 2100,
        sequence: int = 0,
        connect_timeout: float = 5.0,
        send_timeout: float = 0.2,
        close_timeout: float = 0.2,
    ) -> None:
        if not isinstance(destination, str):
            raise TypeError("destination must be a numeric IP address")
        address = ipaddress.ip_address(destination)
        _integer(port, "port", 1, 65535)
        _integer(sequence, "sequence", 0, 255)
        for secret in (psk_identity, client_key):
            if not isinstance(secret, bytes):
                raise TypeError("credentials must be bytes")
            if not 1 <= len(secret) <= 65535:
                raise ValueError("invalid credential length")
        if not isinstance(entertainment_id, str) or len(entertainment_id) != 36:
            raise ValueError("entertainment_id must be an ASCII UUID")
        try:
            if str(UUID(entertainment_id)) != entertainment_id.lower():
                raise ValueError("invalid UUID")
            identifier = entertainment_id.encode("ascii")
        except (ValueError, UnicodeError) as exc:
            raise ValueError("entertainment_id must be an ASCII UUID") from exc
        if not isinstance(channel_ids, tuple):
            raise TypeError("channel_ids must be an immutable tuple")
        if not 1 <= len(channel_ids) <= 256:
            raise ValueError("invalid Hue channel count")
        for channel in channel_ids:
            _integer(channel, "channel ID", 0, 255)
        if len(set(channel_ids)) != len(channel_ids):
            raise ValueError("channel IDs must be unique")
        for timeout in (connect_timeout, send_timeout, close_timeout):
            if isinstance(timeout, bool):
                raise TypeError("timeout must be numeric")
            if not math.isfinite(timeout) or timeout <= 0:
                raise ValueError("timeout must be finite and positive")
        self._count = len(channel_ids)
        self._engine = _native.HueEngine(
            str(address),
            port,
            psk_identity,
            client_key,
            identifier,
            bytes(channel_ids),
            sequence,
            connect_timeout,
            send_timeout,
            close_timeout,
        )

    def connect(self) -> None:
        self._engine.connect()

    def send(self, frame: Frame) -> None:
        normalized, kind, count = _rgb(frame, 256, "strict")
        if count != self._count:
            raise ValueError("frame does not match configured Hue channels")
        self._engine.send(normalized, kind)

    def service(self) -> None:
        self._engine.service()

    def close(self) -> None:
        self._engine.close()

    @property
    def connected(self) -> bool:
        return self._engine.connected

    @property
    def closed(self) -> bool:
        return self._engine.closed
