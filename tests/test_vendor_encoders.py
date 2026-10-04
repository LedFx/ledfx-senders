"""Independent wire fixtures for session-owned vendor transports."""

import base64
import json
import struct

import numpy as np
import pytest
from numpy.typing import DTypeLike

from ledfx_senders import encoders as vendor
from ledfx_senders.e131_buffer import Frame


@pytest.mark.parametrize(
    ("order", "payload"),
    [
        ("RGB", b"\x11\x22\x33"),
        ("RBG", b"\x11\x33\x22"),
        ("GRB", b"\x22\x11\x33"),
        ("GBR", b"\x22\x33\x11"),
        ("BRG", b"\x33\x11\x22"),
        ("BGR", b"\x33\x22\x11"),
    ],
)
def test_adalight_one_pixel_receiver_count(order: str, payload: bytes) -> None:
    assert (
        vendor.encode_adalight(np.array([[17, 34, 51]], dtype=np.uint8), order)
        == b"Ada\0\0\x55" + payload
    )


def test_adalight_back_to_back_stream_receiver():
    # Adafruit b9d88f8a LEDstream.pde lines 183-189: count is inclusive.
    frames = [
        np.array([[1, 2, 3]], dtype=np.uint8),
        np.array([[4, 5, 6], [7, 8, 9]], dtype=np.uint8),
    ]
    stream = b"".join(vendor.encode_adalight(f, "RGB") for f in frames)
    pending = bytearray()
    delivered = []
    for byte in stream:
        pending.append(byte)
        if len(pending) >= 6:
            assert pending[:3] == b"Ada"
            high, low, checksum = pending[3:6]
            assert checksum == high ^ low ^ 0x55
            wanted = 6 + 3 * (256 * high + low + 1)
            if len(pending) == wanted:
                delivered.append(bytes(pending[6:]))
                pending.clear()
    assert delivered == [b"\1\2\3", b"\4\5\6\7\10\11"]
    assert not pending


def test_openrgb_literal_v3_packet():
    got = vendor.encode_openrgb(
        np.array([[17, 34, 51], [68, 85, 102]], dtype=np.uint8), 0x12345678
    )
    assert got == bytes.fromhex(
        "4f524742785634121a0400000e0000000e00000002001122330044556600"
    )


def test_hue_literal_ascii_uuid_and_duplicate_components():
    got = vendor.encode_hue(
        np.array([[-0.9, 34.9, 255.9]]),
        "12345678-1234-1234-1234-123456789abc",
        (7,),
        254,
    )
    assert (
        got
        == b"HueStream\2\0\xfe\0\0\0\0"
        + b"12345678-1234-1234-1234-123456789abc"
        + b"\7\0\0\x22\x22\xff\xff"
    )


@pytest.mark.parametrize(
    ("values", "error"),
    [
        ([256, 1, 2], ValueError),
        ([256, float("inf"), 0], OverflowError),
        ([float("nan"), float("inf"), 0], ValueError),
        ([float("inf"), float("nan"), 0], OverflowError),
    ],
)
def test_hue_preserves_int_conversion_error_precedence(
    values: list[float], error: type[Exception]
) -> None:
    with pytest.raises(error):
        vendor.encode_hue(
            np.array([values]), "12345678-1234-1234-1234-123456789abc", (0,), 0
        )


@pytest.mark.parametrize("stretch", [False, True])
def test_govee_independent_envelope_checksum(stretch: bool) -> None:
    packet = vendor.encode_govee(np.array([[17, 34, 51]], dtype=np.uint8), stretch)
    body = json.loads(packet)
    raw = base64.b64decode(body["msg"]["data"]["pt"])
    assert raw[:6] == bytes([187, 0, 250, 176, int(stretch), 1])
    assert raw[6:9] == b"\x11\x22\x33"
    assert raw[-1] == (240 if not stretch else 241)
    assert packet == json.dumps(body).encode()


@pytest.mark.parametrize(
    "dtype",
    [
        np.uint8,
        np.float32,
        np.float64,
        np.int64,
        np.uint64,
        np.float16,
        np.longdouble,
        ">f8",
        ">i4",
        np.bool_,
    ],
)
def test_strided_rgb_is_ordered_and_owned(dtype: DTypeLike) -> None:
    original = np.array([[1, 2, 3], [4, 5, 6], [7, 8, 9]], dtype=dtype)
    frame = original[::-2, ::-1]
    expected = frame.astype(np.uint8).tobytes()
    packet = vendor.encode_adalight(frame, "RGB")
    original[:] = 0
    assert packet[6:] == expected
    assert isinstance(packet, bytes)


@pytest.mark.parametrize(
    "name,limit", [("adalight", 65536), ("openrgb", 65535), ("govee", 255)]
)
def test_vendor_count_limits(name: str, limit: int) -> None:
    encode = getattr(vendor, "encode_" + name)
    arg = {"adalight": "RGB", "openrgb": 0, "govee": False}[name]
    packet = encode(np.zeros((limit, 3), dtype=np.uint8), arg)
    assert isinstance(packet, bytes)
    for count in (0, limit + 1):
        with pytest.raises(ValueError):
            encode(np.zeros((count, 3), dtype=np.uint8), arg)


@pytest.mark.parametrize(
    "frame",
    [
        np.zeros(3),
        np.zeros((3, 1)),
        np.zeros((1, 3), dtype=complex),
        np.array([[object(), 1, 2]], dtype=object),
        memoryview(np.zeros(3, dtype=np.int8)),
    ],
)
def test_rejects_non_rgb_and_unsupported_formats(frame: Frame) -> None:
    with pytest.raises((ValueError, TypeError)):
        vendor.encode_adalight(frame, "RGB")


def test_wrap_policy_is_explicit_and_integers_keep_low_bits():
    values = np.array([[-1.9, 256.9, 257.9], [-(2**31), 2**31, 1e300]])
    assert vendor.encode_adalight(values, "RGB")[6:] == b"\xff\0\1\0\0\0"
    values = np.array([[2**64 - 1, 2**63 + 1, 256]], dtype=np.uint64)
    assert vendor.encode_openrgb(values, 0)[22:] == b"\xff\1\0\0"
    for bad in (float("nan"), float("inf"), -float("inf")):
        with pytest.raises(ValueError):
            vendor.encode_govee(np.array([[1, 2, bad]]), False)


def test_openrgb_count_and_lengths_decode_independently():
    packet = vendor.encode_openrgb(bytes(3 * 257), 23)
    assert struct.unpack_from("<III", packet, 4) == (23, 1050, 1034)
    assert struct.unpack_from("<IH", packet, 16) == (1034, 257)


@pytest.mark.parametrize(
    "dtype", ["uint8", "float32", "float64", "int64", "longdouble"]
)
def test_cached_rgb_gather_preserves_order_and_numeric_policy(dtype: str) -> None:
    gather = vendor.RGBGather((2, 0, 1))
    frame = np.array([[1, 2, 3], [4, 5, 6], [7, 8, 9]], dtype=dtype)
    output = gather.encode(frame)
    frame[:] = 0
    assert output == b"\7\10\11\1\2\3\4\5\6"
    with pytest.raises(ValueError):
        gather.encode(np.zeros((2, 3)))
    with pytest.raises(ValueError):
        gather.encode(np.array([[1, 2, 3], [4, 5, 6], [7, 8, np.inf]]))


@pytest.mark.parametrize("permutation", [(), (0, 0), (0, 2), (-1, 0), (True,)])
def test_rgb_gather_rejects_malformed_permutations(
    permutation: tuple[int, ...],
) -> None:
    with pytest.raises((TypeError, ValueError)):
        vendor.RGBGather(permutation)


@pytest.mark.parametrize("dtype", [np.float16, np.longdouble])
def test_rare_float_boundaries_keep_original_precision(
    dtype: type[np.floating],
) -> None:
    below_two = np.nextafter(dtype(2), dtype(0))
    above_minus_one = np.nextafter(dtype(-1), dtype(0))
    below_256 = np.nextafter(dtype(256), dtype(0))
    frame = np.array([[below_two, above_minus_one, below_256]], dtype=dtype)
    expected = bytes([1, 0, 255])
    assert vendor.encode_adalight(frame, "RGB")[6:] == expected
    assert vendor.encode_openrgb(frame, 0)[22:] == expected + b"\0"
    hue = vendor.encode_hue(frame, "12345678-1234-1234-1234-123456789abc", (0,), 0)
    assert hue[52:] == b"\0\1\1\0\0\xff\xff"
    for bad in (dtype(-1), dtype(256)):
        frame[0, 2] = bad
        with pytest.raises(ValueError):
            vendor.encode_hue(frame, "12345678-1234-1234-1234-123456789abc", (0,), 0)


def test_longdouble_wrap_cutoff_preserves_low_bits_before_narrowing() -> None:
    dtype = np.longdouble
    values = [
        np.nextafter(dtype(2**31), dtype(0)),
        np.nextafter(dtype(-(2**31)), dtype(0)),
        dtype(2**31),
    ]
    frame = np.array([values], dtype=dtype)
    # Native-width extended precision differs by platform; int() operates on
    # each original scalar and establishes the independent truncation oracle.
    expected = bytes([int(values[0]) % 256, int(values[1]) % 256, 0])
    assert vendor.encode_adalight(frame, "RGB")[6:] == expected
