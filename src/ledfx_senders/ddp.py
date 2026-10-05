"""DDP sender with an owned native UDP transport."""

from ledfx_senders._sender import _PacketSender


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
