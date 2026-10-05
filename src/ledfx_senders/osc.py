"""OSC sender with original-value traffic suppression."""

from typing import Self

from ledfx_senders._sender import _StatefulSender
from ledfx_senders._validation import _integer


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
