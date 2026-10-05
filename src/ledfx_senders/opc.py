"""Open Pixel Control sender with an owned native UDP transport."""

from ledfx_senders._sender import _PacketSender


class OPCSender(_PacketSender):
    """One UDP OPC packet; finite RGB values clamp then truncate."""

    _protocol = "opc"

    def __init__(
        self, pixel_count: int, *, destination: str, port: int = 7890, channel: int = 0
    ) -> None:
        self._initialize(pixel_count, destination, port, channel)
