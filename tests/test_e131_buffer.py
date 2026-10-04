from collections.abc import Callable

import numpy as np
import pytest
from numpy.typing import DTypeLike

from ledfx_senders.e131_buffer import ChannelLayout, Frame, PacketBanks


def test_owned_atomic_conversion() -> None:
    banks = PacketBanks(ChannelLayout(3))
    banks.update(np.array([-0.9, 255.9, 1.9]))
    assert banks.snapshot()[0][126:129] == bytes([0, 255, 1])
    before = banks.snapshot()
    staging = banks.native.snapshot(True)
    for bad in (np.nan, np.inf, -1, 256):
        with pytest.raises(ValueError):
            banks.update(np.array([0, 0, bad]))
        assert banks.snapshot() == before
        assert banks.native.snapshot(True) == staging


@pytest.mark.parametrize("pixels", [1, 170, 171, 1000, 50000])
@pytest.mark.parametrize("size", [510, 512, 1])
@pytest.mark.parametrize("offset", [0, 1, 7, 510, 511, 513, 1027])
def test_every_wire_slot(pixels: int, size: int, offset: int) -> None:
    count = pixels * 3
    if (offset + count - 1) // size + 1 > 63999:
        with pytest.raises(ValueError):
            ChannelLayout(count, universe_size=size, channel_offset=offset)
        return
    layout = ChannelLayout(count, universe_size=size, channel_offset=offset)
    banks = PacketBanks(layout, fill=93)
    frame = np.arange(count, dtype=np.uint32).astype(np.uint8)
    expected = [bytearray([93]) * 512 for _ in layout.universes]
    for index, value in enumerate(frame):
        packet, slot = divmod(offset + index, size)
        expected[packet][slot] = value
    banks.update(frame)
    assert [packet[126:] for packet in banks.snapshot()] == expected


@pytest.mark.parametrize(
    "dtype",
    [
        np.bool_,
        np.int8,
        np.int16,
        np.int32,
        np.int64,
        np.uint8,
        np.uint16,
        np.uint32,
        np.uint64,
        np.float16,
        np.float32,
        np.float64,
        np.longdouble,
        ">f4",
        ">f8",
        ">i8",
    ],
)
@pytest.mark.parametrize("order", ["C", "F", "strided"])
def test_formats(dtype: DTypeLike, order: str) -> None:
    frame = np.array([[0, 1], [1, 0]], dtype=dtype, order="F" if order == "F" else "C")
    if order == "strided":
        frame = np.repeat(frame, 2, axis=1)[:, ::2]
    banks = PacketBanks(ChannelLayout(4))
    banks.update(frame)
    assert banks.snapshot()[0][126:130] == bytes([0, 1, 1, 0])


@pytest.mark.parametrize("factory", [bytes, bytearray, memoryview])
def test_byte_buffers(factory: Callable[[bytes], Frame]) -> None:
    banks = PacketBanks(ChannelLayout(3))
    banks.update(factory(bytes([0, 128, 255])))
    assert banks.snapshot()[0][126:129] == bytes([0, 128, 255])


@pytest.mark.parametrize(
    "frame",
    [
        np.array([2**64 - 1], dtype=np.uint64),
        np.array([-1], dtype=np.int64),
        np.array([256], dtype=np.uint16),
        np.array([1j]),
        np.array([object()]),
        np.array(["1"]),
        np.array([(1,)], dtype=[("x", "u1")]),
        memoryview(bytes(4))[::2],
        memoryview(np.array([1], dtype=np.int16)),
        bytes(2),
    ],
)
def test_rejected_inputs_atomic(frame: Frame) -> None:
    banks = PacketBanks(ChannelLayout(1), fill=93)
    before = banks.snapshot()
    with pytest.raises((ValueError, TypeError)):
        banks.update(frame)
    assert banks.snapshot() == before


def test_released_view() -> None:
    frame = memoryview(b"a")
    frame.release()
    with pytest.raises(ValueError):
        PacketBanks(ChannelLayout(1)).update(frame)


def test_uncommon_precision_before_narrowing() -> None:
    banks = PacketBanks(ChannelLayout(1))
    for dtype in (np.float16, np.longdouble, ">f8"):
        for value in (-1, 256, np.inf, np.nan):
            with pytest.raises(ValueError):
                banks.update(np.array([value], dtype=dtype))
    if np.finfo(np.longdouble).eps < np.finfo(np.float64).eps:
        banks.update(
            np.array(
                [np.longdouble(256) - np.finfo(np.longdouble).eps * 128],
                dtype=np.longdouble,
            )
        )
        assert banks.snapshot()[0][126] == 255


def test_storage_reuse_and_no_aliases() -> None:
    banks = PacketBanks(ChannelLayout(513), fill=93)
    frame = np.full(513, 12, dtype=np.float32)
    banks.update(frame)
    capacity = banks.capacities()
    before = banks.snapshot()
    frame[:] = 99
    assert banks.snapshot() == before
    for dtype in (np.uint8, np.float32, np.float64):
        value = np.full(513, 12, dtype=dtype)
        for _ in range(100):
            banks.update(value)
        assert banks.capacities() == capacity
    assert isinstance(before[0], bytes)


def test_common_inputs_pass_through() -> None:
    from ledfx_senders.e131_buffer import normalize_frame

    for frame in (
        b"abc",
        bytearray(b"abc"),
        memoryview(b"abc"),
        np.ones(3, dtype=np.uint8),
        np.ones(3, dtype=np.float32),
        np.ones(3, dtype=np.float64),
    ):
        assert normalize_frame(frame, 3) is frame


@pytest.mark.parametrize("dtype", [np.float32, np.float64])
def test_unaligned_numpy_buffer(dtype: DTypeLike) -> None:
    frame = np.ndarray((3,), dtype=dtype, buffer=bytearray(25), offset=1)
    frame[:] = [1.9, 255.9, -0.9]
    assert not frame.flags.aligned
    banks = PacketBanks(ChannelLayout(3))
    banks.update(frame)
    assert banks.snapshot()[0][126:129] == bytes([1, 255, 0])


@pytest.mark.parametrize("value", [np.nan, np.inf, -1, 256])
def test_longdouble_late_invalid_preserves_both_banks(value: float) -> None:
    banks = PacketBanks(ChannelLayout(513), fill=93)
    frame = np.full(513, 12, dtype=np.longdouble)
    banks.update(frame)
    committed, staging = banks.snapshot(), banks.native.snapshot(True)
    frame[-1] = value
    with pytest.raises(ValueError):
        banks.update(frame)
    assert banks.snapshot() == committed
    assert banks.native.snapshot(True) == staging


def test_longdouble_buffer_identity_even_when_dtype_compares_as_float64(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from ledfx_senders.e131_buffer import normalize_frame

    # Emulate the dtype-comparison alias present on macOS ARM/Windows even
    # when this host's longdouble is wider. Its actual buffer still exports g.
    frame = np.array([-0.9, 1.9, 255.9], dtype=np.longdouble)
    original_dtype = np.dtype
    extended = frame.dtype
    assert extended.char == "g" and memoryview(frame).format == "g"

    def comparable_dtype(name: str) -> np.dtype[np.generic]:
        return extended if name == "float64" else original_dtype(name)

    with monkeypatch.context() as context:
        context.setattr(np, "dtype", comparable_dtype)
        normalized = normalize_frame(frame, 3)
    assert memoryview(normalized).format == "B"
    assert bytes(normalized) == bytes([0, 1, 255])
    banks = PacketBanks(ChannelLayout(3))
    banks.update(normalized)
    assert banks.snapshot()[0][126:129] == bytes([0, 1, 255])
