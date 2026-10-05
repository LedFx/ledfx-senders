"""Art-Net sender with cached channel layout and owned UDP transport."""

import socket
from typing import Self, cast

import numpy as np
from numpy.typing import NDArray

from ledfx_senders import _native
from ledfx_senders._validation import _integer
from ledfx_senders.frames import Frame


class ArtNetSender:
    """Cached ArtDmx layout, original-dtype RGBW, and synchronous bounded UDP.

    Logical packet_size is independent of wire padding. Odd lengths with
    even_packet_size=False and sequences 0..255 retain legacy compatibility.
    Float conversion follows deterministic wrap AFTER original-dtype arithmetic:
    finite [-2**31, 2**31) truncates modulo256; other finite values become zero.
    Nonfinite inputs/results reject atomically. Callers must not mutate input
    during send. Float16/longdouble arithmetic uses NumPy at original precision.
    """

    def __init__(
        self,
        *,
        destination: str,
        port: int,
        universe: int,
        packet_size: int,
        even_packet_size: bool,
        dmx_start_address: int,
        pixel_count: int,
        pixels_per_device: int,
        pre_amble: bytes,
        post_amble: bytes,
        rgb_order: str,
        white_mode: str,
        broadcast: bool,
    ) -> None:
        self._setup(
            destination=destination,
            port=port,
            universe=universe,
            packet_size=packet_size,
            even_packet_size=even_packet_size,
            dmx_start_address=dmx_start_address,
            pixel_count=pixel_count,
            pixels_per_device=pixels_per_device,
            pre_amble=pre_amble,
            post_amble=post_amble,
            rgb_order=rgb_order,
            white_mode=white_mode,
            broadcast=broadcast,
        )

    @staticmethod
    def validate_layout(
        *,
        pixel_count: int,
        universe: int,
        packet_size: int,
        dmx_start_address: int,
        pixels_per_device: int,
        pre_amble: bytes,
        post_amble: bytes,
        rgb_order: str,
        white_mode: str,
    ) -> None:
        _integer(pixel_count, "pixel_count", 1, 32768 * 512)
        _integer(universe, "universe", 0, 32767)
        _integer(packet_size, "packet_size", 1, 512)
        _integer(dmx_start_address, "dmx_start_address", 1, 512)
        if type(pixels_per_device) is not int or pixels_per_device < 0:
            raise ValueError("pixels_per_device must be a nonnegative integer")
        if not isinstance(pre_amble, bytes) or not isinstance(post_amble, bytes):
            raise TypeError("ambles must be bytes")
        if rgb_order not in ("RGB", "RBG", "GRB", "GBR", "BRG", "BGR"):
            raise ValueError("invalid RGB order")
        if white_mode not in ("None", "Zero", "Brighter", "Accurate"):
            raise ValueError("invalid white mode")
        group = (
            pixels_per_device if 0 < pixels_per_device <= pixel_count else pixel_count
        )
        total = (
            dmx_start_address
            - 1
            + (pixel_count // group)
            * (
                len(pre_amble)
                + len(post_amble)
                + group * (3 if white_mode == "None" else 4)
            )
        )
        if universe + (total + packet_size - 1) // packet_size > 32768:
            raise ValueError("Art-Net channel layout exceeds universe 32767")

    def _setup(
        self,
        *,
        destination: str,
        port: int,
        universe: int,
        packet_size: int,
        even_packet_size: bool,
        dmx_start_address: int,
        pixel_count: int,
        pixels_per_device: int,
        pre_amble: bytes,
        post_amble: bytes,
        rgb_order: str,
        white_mode: str,
        broadcast: bool,
        mode: str = "socket",
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> None:
        self.validate_layout(
            pixel_count=pixel_count,
            universe=universe,
            packet_size=packet_size,
            dmx_start_address=dmx_start_address,
            pixels_per_device=pixels_per_device,
            pre_amble=pre_amble,
            post_amble=post_amble,
            rgb_order=rgb_order,
            white_mode=white_mode,
        )
        _integer(port, "port", 1, 65535)
        if type(even_packet_size) is not bool or type(broadcast) is not bool:
            raise TypeError("even_packet_size and broadcast must be bool")
        self.channel_count = pixel_count * 3
        self._indices = ["RGB".index(c) for c in rgb_order]
        self._white = white_mode
        address = socket.gethostbyname(destination)
        self._engine = _native.ArtNetEngine(
            pixel_count,
            f"{address}:{port}",
            universe,
            packet_size,
            even_packet_size,
            dmx_start_address,
            min(pixels_per_device, pixel_count),
            pre_amble,
            post_amble,
            rgb_order,
            white_mode,
            broadcast,
            mode,
            backend,
            batch_size,
            override_destination,
        )

    @classmethod
    def _test_sender(
        cls,
        *,
        destination: str,
        port: int,
        universe: int,
        packet_size: int,
        even_packet_size: bool,
        dmx_start_address: int,
        pixel_count: int,
        pixels_per_device: int,
        pre_amble: bytes,
        post_amble: bytes,
        rgb_order: str,
        white_mode: str,
        broadcast: bool,
        mode: str,
        backend: str = "batched",
        batch_size: int = 64,
        override_destination: str | None = None,
    ) -> Self:
        s = cls.__new__(cls)
        s._setup(
            destination=destination,
            port=port,
            universe=universe,
            packet_size=packet_size,
            even_packet_size=even_packet_size,
            dmx_start_address=dmx_start_address,
            pixel_count=pixel_count,
            pixels_per_device=pixels_per_device,
            pre_amble=pre_amble,
            post_amble=post_amble,
            rgb_order=rgb_order,
            white_mode=white_mode,
            broadcast=broadcast,
            mode=mode,
            backend=backend,
            batch_size=batch_size,
            override_destination=override_destination,
        )
        return s

    def send(self, frame: Frame) -> None:
        from ledfx_senders.original import original

        if isinstance(frame, np.ndarray):
            if frame.dtype.kind == "b" and self._white == "Accurate":
                raise TypeError("numpy boolean subtract is not supported")
            if frame.shape != (self.channel_count // 3, 3):
                raise ValueError("frame must have configured RGB shape")
            if frame.dtype.kind == "f" and (
                frame.dtype.itemsize == 2 or frame.dtype.char == "g"
            ):
                # Rare formats retain original arithmetic rounding. Never demote
                # longdouble or promote half until white extraction/subtraction ends.
                if not np.isfinite(frame).all():
                    raise ValueError("nonfinite channel level")
                rgb = cast(NDArray[np.floating], frame[:, self._indices])
                if self._white != "None":
                    w = (
                        np.zeros((len(rgb), 1), dtype=rgb.dtype)
                        if self._white == "Zero"
                        else np.min(rgb, axis=1, keepdims=True)
                    )
                    with np.errstate(over="ignore", invalid="ignore"):
                        rgb = np.concatenate(
                            (rgb - w if self._white == "Accurate" else rgb, w), axis=1
                        )
                if not np.isfinite(rgb).all():
                    raise ValueError("nonfinite channel level after RGBW arithmetic")
                wide = rgb.astype(np.longdouble)
                valid = (wide >= np.longdouble(-2147483648)) & (
                    wide < np.longdouble(2147483648)
                )
                encoded = np.remainder(
                    np.where(valid, np.trunc(wide), np.longdouble(0)),
                    np.longdouble(256),
                ).astype(np.uint8)
                self._engine.send(np.ascontiguousarray(encoded), 6)
                return
        if (
            isinstance(frame, np.ndarray)
            and frame.dtype.char in "fd"
            and not frame.dtype.isnative
        ):
            # Endian normalization must not promote f32 before Accurate subtraction.
            frame = frame.astype(frame.dtype.newbyteorder("="))
        normalized, kind = original(frame, self.channel_count)
        self._engine.send(normalized, kind)

    def close(self, blackout: bool = True) -> None:
        """Wait for any in-flight frame, then use a separate 200ms blackout budget.

        All configured payload slots are cleared, including ambles and padding.
        Cleanup errors are recorded; transport closes even after partial blackout.
        """
        self._engine.close(blackout)

    @property
    def closed(self) -> bool:
        return self._engine.closed
