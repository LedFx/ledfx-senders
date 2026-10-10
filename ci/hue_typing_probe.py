"""Strict typed public usage; run with -I against an installed wheel."""

from pathlib import Path

import numpy as np
from numpy.typing import NDArray

from ledfx_senders import Frame, HueSender, _native


def main() -> None:
    assert (
        not Path(_native.__file__)
        .resolve()
        .is_relative_to(Path(__file__).resolve().parents[1])
    )
    sender = HueSender(
        destination="127.0.0.1",
        psk_identity=b"typing-dummy",
        client_key=bytes(range(16)),
        entertainment_id="12345678-1234-1234-1234-123456789abc",
        channel_ids=(7,),
    )
    byte_frame: Frame = bytes((1, 2, 3))
    array_frame: NDArray[np.float64] = np.array([[1.9, 2.1, 3.0]], dtype=np.float64)
    try:
        for frame in (byte_frame, array_frame):
            try:
                sender.send(frame)
            except ConnectionError:
                pass
            else:
                raise AssertionError("unconnected sender accepted typed frame")
    finally:
        sender.close()


if __name__ == "__main__":
    main()
