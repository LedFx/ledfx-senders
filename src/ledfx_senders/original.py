"""Private bulk normalization preserving original values for native comparison."""

from typing import cast

import numpy as np
from numpy.typing import NDArray

from ledfx_senders.e131_buffer import Frame


def original(frame: Frame, count: int) -> tuple[Frame, int]:
    if isinstance(frame, (bytes, bytearray, memoryview)):
        view = memoryview(frame)
        if not view.c_contiguous or view.format != "B" or view.nbytes != count:
            raise ValueError("frame must contain configured unsigned byte channels")
        return frame, 0
    if not isinstance(frame, np.ndarray):
        raise TypeError("frame must be an ndarray or byte buffer")
    if frame.shape != (count // 3, 3):
        raise ValueError("frame must have configured RGB shape")
    dtype = frame.dtype
    if dtype.kind not in "buif":
        raise ValueError("unsupported channel dtype")
    if dtype.char in "Bfd" and dtype.isnative:
        return (
            frame
            if frame.flags.c_contiguous and frame.flags.aligned
            else np.require(frame, requirements=["C", "A"])
        ), {"B": 0, "f": 1, "d": 2}[dtype.char]
    if dtype.kind in "bui":
        unsigned = dtype.kind in "bu"
        return np.ascontiguousarray(
            frame, dtype=np.uint64 if unsigned else np.int64
        ), 4 if unsigned else 3
    if dtype.itemsize <= 8 and dtype.char != "g":
        return np.ascontiguousarray(frame, dtype=np.float64), 2
    # Two limbs cover IEEE binary128 and x87 extended precision. NumPy operates
    # in the original format: no f64 narrowing or platform padding comparison.
    numeric = cast(NDArray[np.floating], frame)
    if np.finfo(numeric.dtype).nmant > 127 or not np.isfinite(numeric).all():
        raise ValueError("unsupported or nonfinite extended floating input")
    magnitude, exponent = np.frexp(np.abs(numeric).ravel())
    high = np.floor(np.ldexp(magnitude, 64))
    low = np.ldexp(np.ldexp(magnitude, 64) - high, 64)
    tokens = np.empty((count, 3), dtype=np.uint64)
    tokens[:, 0] = high.astype(np.uint64)
    tokens[:, 1] = low.astype(np.uint64)
    tokens[:, 2] = (exponent.astype(np.int64) - 128).astype(np.int32).view(np.uint32)
    tokens[:, 2] |= np.signbit(numeric).ravel().astype(np.uint64) << np.uint64(63)
    return tokens, 5
