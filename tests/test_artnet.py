"""ArtDmx layout and numeric receipts, decoded without production helpers."""

import struct

import numpy as np
import pytest
from numpy.typing import NDArray

from ledfx_senders.packet_senders import ArtNetSender


def sender(
    pixels: int = 3,
    *,
    port: int = 6454,
    universe: int = 254,
    packet_size: int = 510,
    even_packet_size: bool = True,
    dmx_start_address: int = 1,
    pixels_per_device: int = 0,
    pre_amble: bytes = b"",
    post_amble: bytes = b"",
    rgb_order: str = "RGB",
    white_mode: str = "None",
    broadcast: bool = False,
    mode: str = "capture",
) -> ArtNetSender:
    return ArtNetSender._test_sender(
        destination="127.0.0.1",
        port=port,
        universe=universe,
        packet_size=packet_size,
        even_packet_size=even_packet_size,
        dmx_start_address=dmx_start_address,
        pixel_count=pixels,
        pixels_per_device=pixels_per_device,
        pre_amble=pre_amble,
        post_amble=post_amble,
        rgb_order=rgb_order,
        white_mode=white_mode,
        broadcast=broadcast,
        mode=mode,
    )


def decode(packet: bytes) -> tuple[int, int, bytes]:
    assert packet[:12] == b"Art-Net\0\x00\x50\x00\x0e"
    assert packet[13] == 0
    address = struct.unpack("<H", packet[14:16])[0]
    length = struct.unpack(">H", packet[16:18])[0]
    assert length == len(packet) - 18
    return address, packet[12], packet[18:]


def reference(frame: NDArray[np.number], order: str, white: str) -> bytes:
    # Scalar original-dtype arithmetic; explicitly defined final byte policy.
    result = []
    for row in frame:
        rgb = [row["RGB".index(c)] for c in order]
        w = min(rgb)
        if white == "Accurate":
            with np.errstate(over="ignore"):
                rgb = [np.subtract(c, w, dtype=frame.dtype.type) for c in rgb]
        if white != "None":
            rgb.append(frame.dtype.type(0) if white == "Zero" else w)
        for c in rgb:
            n = int(c)
            if frame.dtype.kind == "f" and not np.longdouble(
                -(2**31)
            ) <= c < np.longdouble(2**31):
                n = 0
            result.append(n % 256)
    return bytes(result)


@pytest.mark.parametrize("order", ["RGB", "RBG", "GRB", "GBR", "BRG", "BGR"])
@pytest.mark.parametrize("white", ["None", "Zero", "Brighter", "Accurate"])
@pytest.mark.parametrize(
    "dtype",
    [
        np.uint8,
        np.int8,
        np.uint16,
        np.int64,
        np.uint64,
        np.float16,
        np.float32,
        np.float64,
        np.longdouble,
    ],
)
def test_original_numeric_rgbw(order: str, white: str, dtype: type[np.number]) -> None:
    frame = np.array([[1, 9, 4], [127, 2, 17], [5, 3, 99]], dtype=dtype)
    if frame.dtype.kind == "f":
        frame += np.array(0.75, dtype=dtype)
        frame[0, 1] += np.array(0.5, dtype=dtype)
    s = sender(rgb_order=order, white_mode=white)
    s.send(frame)
    payload = decode(s._engine.captures()[0][0])[2]
    expected = reference(frame, order, white)
    assert payload == expected + bytes(510 - len(expected))


@pytest.mark.parametrize("size", [1, 2, 3, 5, 510, 511, 512])
@pytest.mark.parametrize("even", [False, True])
def test_logical_stride_separate_from_wire_padding(size: int, even: bool) -> None:
    frame = np.arange(9, dtype=np.uint8).reshape(3, 3)
    s = sender(packet_size=size, even_packet_size=even)
    s.send(frame)
    packets = [decode(p) for p, _ in s._engine.captures()]
    wire = max(2, size + (size % 2 if even else 0))
    for i, (address, sequence, payload) in enumerate(packets):
        assert (address, sequence) == (254 + i, i)
        chunk = frame.tobytes()[i * size : (i + 1) * size]
        assert payload == chunk + bytes(wire - len(chunk))


def test_groups_offset_ambles_drop_incomplete_tail() -> None:
    frame = np.arange(15, dtype=np.uint8).reshape(5, 3)
    s = sender(
        5,
        packet_size=5,
        dmx_start_address=4,
        pixels_per_device=2,
        pre_amble=b"\xff\xfe",
        post_amble=b"\xfd",
        white_mode="Zero",
    )
    s.send(frame)
    logical = b"\0" * 3
    for group in [frame[:2], frame[2:4]]:
        logical += b"\xff\xfe" + reference(group, "RGB", "Zero") + b"\xfd"
    packets = [decode(p)[2] for p, _ in s._engine.captures()]
    assert b"".join(p[:5] for p in packets) == logical + bytes((-len(logical)) % 5)
    assert all(p[5:] == b"\0" for p in packets)


def test_sequence_wrap_and_full_blackout() -> None:
    s = sender(86, packet_size=1)
    s.send(np.ones((86, 3), dtype=np.uint8))
    receipts = [decode(p) for p, _ in s._engine.captures()]
    assert [x[1] for x in receipts] == [i % 256 for i in range(258)]
    s.close()
    receipts = [decode(p) for p, _ in s._engine.captures()]
    assert len(receipts) == 516
    assert all(x[2] == b"\0\0" for x in receipts[258:])
    s.close()
    with pytest.raises(RuntimeError, match="closed"):
        s.send(np.ones((86, 3), dtype=np.uint8))
    assert len(s._engine.captures()) == 516


@pytest.mark.parametrize("operation", ["send", "close"])
def test_contended_calls_release_gil(operation: str) -> None:
    import faulthandler
    import sys
    import threading
    from concurrent.futures import ThreadPoolExecutor

    from ledfx_senders import _native

    s = sender(1)
    gate = _native._TestLockGate()
    attempted, completed = threading.Event(), threading.Event()

    def contend() -> None:
        attempted.set()
        if operation == "send":
            s.send(bytes(3))
        else:
            s.close()
        completed.set()

    interval = sys.getswitchinterval()
    faulthandler.dump_traceback_later(15, exit=True)
    try:
        sys.setswitchinterval(30)
        with ThreadPoolExecutor(max_workers=2) as pool:
            holder = pool.submit(s._engine._test_hold_lock, gate)
            gate.wait_entered()
            waiter = pool.submit(contend)
            try:
                assert attempted.wait(2)
                assert not gate.timed_out and not completed.is_set()
            finally:
                gate.release()
            holder.result(timeout=7)
            waiter.result(timeout=7)
        assert completed.is_set() and not gate.timed_out
        assert s.closed == (operation == "close")
    finally:
        sys.setswitchinterval(interval)
        gate.release()
        s.close()
        faulthandler.cancel_dump_traceback_later()


@pytest.mark.parametrize("failure", ["size", "numeric", "closed"])
def test_reentrant_exporter_cleanup(failure: str) -> None:
    import subprocess
    import sys

    if sys.version_info < (3, 12):
        pytest.skip("PEP688 requires Python3.12")
    script = """
import sys
from array import array
from ledfx_senders.packet_senders import ArtNetSender
failure=sys.argv[1]
sender=ArtNetSender._test_sender(destination="127.0.0.1",port=6454,universe=0,packet_size=510,even_packet_size=True,dmx_start_address=1,pixel_count=1,pixels_per_device=0,pre_amble=b"",post_amble=b"",rgb_order="RGB",white_mode="None",broadcast=False,mode="capture")
events=[]
if failure=='closed':sender.close()
class Exporter:
    def __buffer__(self, flags):
        assert sender._engine.closed == (failure=='closed')
        events.append('export')
        return memoryview(array('d',[1,2,float('nan')]) if failure=='numeric' else bytes(2))
    def __release_buffer__(self, view):
        sender._engine.counters()
        events.append('release')
try:sender._engine.send(Exporter(), 2 if failure=='numeric' else 0)
except (ValueError,RuntimeError,BufferError):pass
else:raise AssertionError('invalid accepted')
assert events==['export','release'],events
"""
    subprocess.run(
        [sys.executable, "-I", "-c", script, failure], check=True, timeout=10
    )


@pytest.mark.parametrize("dtype", ["float16", "float32", "float64", "longdouble"])
@pytest.mark.parametrize("white", ["None", "Zero", "Brighter", "Accurate"])
def test_float_boundaries_and_atomic_invalid(dtype: str, white: str) -> None:
    s = sender(2, white_mode=white, pixels_per_device=1)
    frame = np.array([[-2.75, 257.25, 3.5], [0, 1, 255]], dtype=dtype)
    s.send(frame)
    expected = reference(frame, "RGB", white)
    assert decode(s._engine.captures()[0][0])[2].startswith(expected)
    before = s._engine.committed_copy()
    for bad in [float("nan"), float("inf"), -float("inf")]:
        frame[1, 2] = bad
        with pytest.raises(ValueError, match="nonfinite"):
            s.send(frame)
        assert s._engine.committed_copy() == before
        assert len(s._engine.captures()) == 1


def test_reject_nonfinite_in_dropped_group_and_overflow_result() -> None:
    s = sender(3, white_mode="Accurate", pixels_per_device=2)
    frame = np.zeros((3, 3), dtype=np.float32)
    frame[2, 2] = np.nan
    with pytest.raises(ValueError, match="nonfinite"):
        s.send(frame)
    frame[2, 2] = 0
    frame[0] = [-np.finfo(np.float32).max, np.finfo(np.float32).max, 0]
    with pytest.raises(ValueError, match="nonfinite"):
        s.send(frame)
    assert not s._engine.captures()


def test_bool_modes_and_integer_overflow() -> None:
    for white in ["None", "Zero", "Brighter"]:
        s = sender(1, white_mode=white)
        s.send(np.array([[True, False, True]]))
        assert decode(s._engine.captures()[0][0])[2][:3] == b"\x01\0\x01"
    with pytest.raises(TypeError, match="boolean"):
        sender(1, white_mode="Accurate").send(np.ones((1, 3), dtype=bool))
    for dtype in [np.int8, np.int16, np.int32, np.int64, np.uint64]:
        info = np.iinfo(dtype)
        frame = np.array([[info.min, info.max, 0]], dtype=dtype)
        s = sender(1, white_mode="Accurate")
        s.send(frame)
        assert decode(s._engine.captures()[0][0])[2][:4] == reference(
            frame, "RGB", "Accurate"
        )


@pytest.mark.parametrize("broadcast", [False, True])
def test_owned_loopback_broadcast_option_and_no_traffic_after_close(
    broadcast: bool,
) -> None:
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(1)
        s = sender(
            1, port=receiver.getsockname()[1], broadcast=broadcast, mode="socket"
        )
        assert s._engine._test_broadcast() == broadcast
        s.send(b"abc")
        assert decode(receiver.recvfrom(65535)[0])[2][:3] == b"abc"
        s.close(False)
        with pytest.raises(RuntimeError, match="closed"):
            s.send(b"abc")
        receiver.settimeout(0.02)
        with pytest.raises(socket.timeout):
            receiver.recvfrom(65535)


@pytest.mark.parametrize("dtype", ["<f4", ">f4", "<f8", ">f8", "float16", "longdouble"])
def test_fractional_rounding_endian_stride_and_snapshot(dtype: str) -> None:
    frame = np.array([[1.875, 2.125, 3.75], [127.9375, 255.9375, -0.0625]], dtype=dtype)
    expected = reference(frame, "BGR", "Accurate")
    s = sender(2, rgb_order="BGR", white_mode="Accurate")
    noncontiguous = np.zeros((2, 6), dtype=dtype)
    noncontiguous[:, ::2] = frame
    s.send(noncontiguous[:, ::2])
    assert decode(s._engine.captures()[0][0])[2].startswith(expected)
    noncontiguous.fill(0)
    assert decode(s._engine.committed_copy()[0])[2].startswith(expected)


@pytest.mark.parametrize("group", [0, 100, 10**100])
def test_default_or_oversized_group_uses_all_pixels(group: int) -> None:
    s = sender(3, pixels_per_device=group, pre_amble=b"x", post_amble=b"y")
    s.send(bytes(range(9)))
    assert decode(s._engine.captures()[0][0])[2].startswith(
        b"x" + bytes(range(9)) + b"y"
    )


def test_direct_engine_checks_buffer_kind_and_size() -> None:
    s = sender(1)
    for frame, kind in [(bytes(2), 0), (bytes(3), 2), (bytes(3), 5), (bytes(2), 6)]:
        with pytest.raises((ValueError, BufferError)):
            s._engine.send(frame, kind)
    assert not s._engine.captures()


def test_concurrent_send_close_orders_blackout_after_any_accepted_frame() -> None:
    import threading
    from concurrent.futures import ThreadPoolExecutor

    from ledfx_senders import _native

    s = sender(3, packet_size=2, pre_amble=b"x", post_amble=b"y")
    gate = _native._TestLockGate()
    attempted_send, attempted_close = threading.Event(), threading.Event()

    def send() -> None:
        attempted_send.set()
        try:
            s.send(bytes([7]) * 9)
        except RuntimeError:
            assert s.closed

    def close() -> None:
        attempted_close.set()
        s.close()

    with ThreadPoolExecutor(max_workers=3) as pool:
        holder = pool.submit(s._engine._test_hold_lock, gate)
        gate.wait_entered()
        sending, closing = pool.submit(send), pool.submit(close)
        try:
            assert attempted_send.wait(2) and attempted_close.wait(2)
        finally:
            gate.release()
        holder.result(timeout=7)
        sending.result(timeout=7)
        closing.result(timeout=7)
    assert s.closed and not gate.timed_out
    payloads = [decode(p)[2] for p, _ in s._engine.captures()]
    assert len(payloads) in (6, 12)
    assert payloads[-6:] == [b"\0\0"] * 6
    if len(payloads) == 12:
        assert b"".join(payloads[:6]) == b"x" + bytes([7]) * 9 + b"y\0"


@pytest.mark.parametrize(
    "size,even",
    [(1, False), (1, True), (2, True), (3, False), (3, True), (510, True), (512, True)],
)
def test_small_layouts_have_drained_exact_loopback_receipts(
    size: int, even: bool
) -> None:
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(1)
        s = sender(
            3,
            port=receiver.getsockname()[1],
            packet_size=size,
            even_packet_size=even,
            mode="socket",
        )
        frame = bytes(range(1, 10))
        s.send(frame)
        count = (len(frame) + size - 1) // size
        wire = max(2, size + (size % 2 if even else 0))
        for index in range(count):
            address, sequence, payload = decode(receiver.recvfrom(65535)[0])
            assert (address, sequence) == (254 + index, index)
            chunk = frame[index * size : (index + 1) * size]
            assert payload == chunk + bytes(wire - len(chunk))
        s.close(False)
        receiver.settimeout(0.02)
        with pytest.raises(socket.timeout):
            receiver.recvfrom(65535)


@pytest.mark.parametrize("dtype", ["float32", "float64"])
def test_every_invalid_lane_including_vector_tail_rejects_atomically(
    dtype: str,
) -> None:
    s = sender(19, white_mode="Accurate", rgb_order="GBR")
    data = np.ones((19, 3), dtype=dtype)
    s.send(data)
    committed = s._engine.committed_copy()
    for channel in range(data.size):
        data.ravel()[channel] = np.nan
        with pytest.raises(ValueError, match="nonfinite"):
            s.send(data)
        data.ravel()[channel] = 1
        assert s._engine.committed_copy() == committed
        assert len(s._engine.captures()) == 1
