"""Compatibility exports for sender imports predating protocol modules."""

from ledfx_senders.artnet import ArtNetSender
from ledfx_senders.ddp import DDPSender
from ledfx_senders.opc import OPCSender
from ledfx_senders.osc import OSCSender
from ledfx_senders.udp_realtime import UDPRealtimeSender

__all__ = ["ArtNetSender", "DDPSender", "OPCSender", "OSCSender", "UDPRealtimeSender"]
