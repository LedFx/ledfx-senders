"""Literal OSC/realtime wire and original numeric state contracts."""

import struct
from typing import cast

import numpy as np
import pytest
from numpy.typing import NDArray

from ledfx_senders.packet_senders import OSCSender, UDPRealtimeSender

MODES = ["One_Argument", "Three_Arguments", "Three_Addresses", "All_To_One"]


def osc(mode: str = "One_Argument", pixels: int = 2) -> OSCSender:
    return OSCSender._test_sender(
        destination="127.0.0.1",
        pixel_count=pixels,
        path="/test/{address:04d}",
        starting_addr=7,
        send_type=mode,
        mode="capture",
    )


def realtime(
    mode: str = "DRGB", pixels: int = 2, minimise: bool = True
) -> UDPRealtimeSender:
    return UDPRealtimeSender._test_sender(
        destination="127.0.0.1",
        pixel_count=pixels,
        packet_type=mode,
        timeout=2,
        keepalive_interval=0.975,
        minimise_traffic=minimise,
        mode="capture",
    )


def string(data: bytes, offset: int) -> tuple[str, int]:
    end = data.index(0, offset)
    boundary = (end + 4) & ~3
    assert data[end:boundary] == bytes(boundary - end)
    return data[offset:end].decode(), boundary


def decode(data: bytes) -> tuple[str, str, tuple[float, ...]]:
    path, offset = string(data, 0)
    tags, offset = string(data, offset)
    assert tags.startswith(",") and set(tags[1:]) <= set("[]f")
    assert len(data) - offset == tags.count("f") * 4
    return path, tags, struct.unpack(f">{tags.count('f')}f", data[offset:])


@pytest.mark.parametrize("mode", MODES)
def test_osc_literal_wire_and_duplicate(mode: str) -> None:
    sender = osc(mode)
    values = np.array([[-255.9, -0.9, 1.9], [256.9, 510.9, 7.9]])
    sender.send(values, now=0)
    packets = [decode(p) for p, _ in sender._engine.captures()]
    width = 1 if mode == "Three_Addresses" else 6 if mode == "All_To_One" else 3
    tags = (
        "," + "[fff]" * 2
        if mode == "All_To_One"
        else {
            "One_Argument": ",[fff]",
            "Three_Arguments": ",fff",
            "Three_Addresses": ",f",
        }[mode]
    )
    expected = tuple(
        struct.unpack(">6f", (values.astype(np.int64) / 255).astype(">f4").tobytes())
    )
    assert packets == [
        (f"/test/{7 + i:04d}", tags, expected[i * width : (i + 1) * width])
        for i in range(6 // width)
    ]
    sender.send(values.copy(), now=1)
    assert len(sender._engine.captures()) == len(packets)
    values[0, 0] += 0.1
    sender.send(values, now=2)
    assert len(sender._engine.captures()) == len(packets) * 2


@pytest.mark.parametrize("kind", ["float32", "float64", "longdouble", ">f8"])
def test_invalid_osc_is_atomic(kind: str) -> None:
    sender = osc()
    sender.send(np.ones((2, 3)), now=0)
    before = sender._engine.counters(), sender._engine.committed_copy()
    for value in [np.nan, np.inf, -np.inf, 2**63, -(2**64)]:
        data = np.ones((2, 3), dtype=kind)
        data[-1, -1] = value
        with pytest.raises(ValueError):
            sender.send(data, now=1)
        assert (sender._engine.counters(), sender._engine.committed_copy()) == before


@pytest.mark.parametrize("protocol", ["osc", "realtime"])
def test_original_precision_and_no_alias(protocol: str) -> None:
    sender = osc(pixels=1) if protocol == "osc" else realtime(pixels=1)
    data = np.array([[1.1, 2.2, 3.3]])
    sender.send(data, now=0)
    data[0, 0] = 1.2
    sender.send(data, now=0.1)
    assert len(sender._engine.captures()) == 2
    sender.send(data.astype(np.longdouble), now=0.2)
    assert len(sender._engine.captures()) == 2
    if np.finfo(np.longdouble).nmant > 52:
        rare = data.astype(np.longdouble)
        rare[0, 0] = np.nextafter(rare[0, 0], np.longdouble(2))
        sender.send(rare, now=0.3)
        assert len(sender._engine.captures()) == 3


def test_cross_dtype_equal_values_and_signed_zero() -> None:
    sender = osc(pixels=1)
    for dtype in [np.uint8, np.float32, np.float64, np.int64, np.longdouble]:
        sender.send(np.array([[0, 1, 2]], dtype=dtype), now=0)
    sender.send(np.array([[-0.0, 1, 2]]), now=0)
    assert len(sender._engine.captures()) == 1


def test_cross_dtype_distinct_large_integer_is_not_rounded() -> None:
    sender = realtime(pixels=1)
    sender.send(np.full((1, 3), 2**64 - 1, dtype=np.uint64), now=0)
    sender.send(np.full((1, 3), 2**64, dtype=np.float64), now=0.1)
    assert len(sender._engine.captures()) == 2


@pytest.mark.parametrize(
    "mode,pixels,kind,chunks",
    [
        ("DRGB", 490, 2, 1),
        ("DRGB", 491, 4, 2),
        ("WARLS", 255, 1, 1),
        ("WARLS", 256, 2, 1),
        ("DRGBW", 367, 3, 1),
        ("DRGBW", 368, 2, 1),
        ("DNRGB", 978, 4, 2),
        ("RGB (HyperHDR)", 500, -1, 1),
        ("RGB (HyperHDR)", 501, 4, 2),
    ],
)
def test_realtime_caps_and_whole_frame_refresh(
    mode: str, pixels: int, kind: int, chunks: int
) -> None:
    sender = realtime(mode, pixels)
    frame = np.full((pixels, 3), 42.9)
    sender.send(frame, now=0)
    first = [p for p, _ in sender._engine.captures()]
    assert len(first) == chunks
    if kind != -1:
        assert all(p[:2] == bytes([kind, 2]) for p in first)
    if kind == 4:
        assert [int.from_bytes(p[2:4], "big") for p in first] == list(
            range(0, pixels, 489)
        )
        assert b"".join(p[4:] for p in first) == bytes([42]) * pixels * 3
    elif kind == 3:
        assert first[0][2:] == bytes([42, 42, 42, 0]) * pixels
    sender.send(frame, now=0.975)
    assert len(sender._engine.captures()) == chunks
    sender.send(frame, now=0.976)
    assert len(sender._engine.captures()) == chunks * 2


def test_adaptive_tie_and_original_delta() -> None:
    sender = realtime("adaptive_smallest", 4)
    data = np.zeros((4, 3))
    sender.send(data, now=0)
    data[:3] = 0.1
    sender.send(data, now=0.1)
    assert sender._engine.captures()[-1][0][0] == 2
    data[3] = 0.1
    sender.send(data, now=0.2)
    assert sender._engine.captures()[-1][0] == bytes([1, 2, 3, 0, 0, 0])


def test_osc_i64_boundaries() -> None:
    sender = osc(pixels=1)
    sender.send(np.array([[-(2**63), 2**63 - 1, 0]], dtype=np.int64), now=0)
    expected = struct.pack(">3f", float(-(2**63)) / 255, float(2**63 - 1) / 255, 0)
    assert sender._engine.captures()[0][0].endswith(expected)
    kind = np.longdouble
    upper = kind(2) ** 63
    data = cast(
        NDArray[np.generic],
        np.array([[np.nextafter(upper, kind(0)), -upper, 0]], dtype=kind),
    )
    sender.send(data, now=1)
    assert sender._engine.captures()[-1][0].endswith(
        struct.pack(">3f", float(int(data[0, 0])) / 255, float(-(2**63)) / 255, 0)
    )


@pytest.mark.parametrize("protocol", ["osc", "realtime"])
@pytest.mark.parametrize("operation", ["send", "close"])
def test_contended_calls_release_gil(protocol: str, operation: str) -> None:
    import faulthandler
    import sys
    import threading
    from concurrent.futures import ThreadPoolExecutor

    from ledfx_senders import _native

    sender = osc(pixels=1) if protocol == "osc" else realtime(pixels=1)
    gate = _native._TestLockGate()
    attempted, completed = threading.Event(), threading.Event()

    def contend() -> None:
        attempted.set()
        if operation == "send":
            sender.send(bytes(3), now=0)
        else:
            sender.close()
        completed.set()

    interval = sys.getswitchinterval()
    faulthandler.dump_traceback_later(15, exit=True)
    try:
        sys.setswitchinterval(30)
        with ThreadPoolExecutor(max_workers=2) as pool:
            holder = pool.submit(sender._engine._test_hold_lock, gate)
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
        assert sender.closed == (operation == "close")
    finally:
        sys.setswitchinterval(interval)
        gate.release()
        sender.close()
        faulthandler.cancel_dump_traceback_later()


@pytest.mark.parametrize("protocol", ["osc", "realtime"])
@pytest.mark.parametrize(
    "dtype",
    ["float16", "float32", "float64", ">f8", "longdouble", "int16", "uint64", "bool"],
)
def test_strides_tails_and_numeric_reference(protocol: str, dtype: str) -> None:
    rng = np.random.default_rng(109)
    for pixels in range(1, 34):
        frame = rng.uniform(0, 1024, (pixels * 2, 3)).astype(dtype)[::2, ::-1]
        sender = osc(pixels=pixels) if protocol == "osc" else realtime(pixels=pixels)
        sender.send(frame, now=0)
        if protocol == "osc":
            wire = [v for p, _ in sender._engine.captures() for v in decode(p)[2]]
            expected = [
                struct.unpack(">f", struct.pack(">f", float(int(v)) / 255))[0]
                for v in frame.flat
            ]
            assert wire == expected
        else:
            assert sender._engine.captures()[0][0][2:] == bytes(
                int(v) % 256 for v in frame.flat
            )


@pytest.mark.parametrize("protocol", ["osc", "realtime"])
def test_rare_limb_boundaries_and_padding_independence(protocol: str) -> None:
    kind = np.longdouble
    values = [
        kind(0),
        kind(-0.0),
        kind(1),
        np.nextafter(kind(1), kind(0)),
        np.nextafter(kind(1), kind(2)),
        kind(2) ** -100,
        np.nextafter(kind(0), kind(1)),
        kind(-2) ** 31,
        kind(2) ** 31,
    ]
    frame = np.array(values, dtype=kind).reshape(-1, 3)
    sender = osc(pixels=3) if protocol == "osc" else realtime(pixels=3)
    sender.send(frame, now=0)
    sender.send(frame.copy(), now=0.1)
    assert sender._engine.frame_counters() == (2, 1, 1)
    if protocol == "osc":
        assert [v for p, _ in sender._engine.captures() for v in decode(p)[2]] == [
            struct.unpack(">f", struct.pack(">f", float(int(v)) / 255))[0]
            for v in frame.flat
        ]
    else:
        assert sender._engine.captures()[0][0][2:] == bytes(
            int(v) % 256 if -(2**31) <= v < 2**31 else 0 for v in frame.flat
        )


def test_udp_ceilings_and_invalid_configuration() -> None:
    # Actual padded path length is part of the UDP ceiling.
    good = OSCSender._test_sender(
        destination="127.0.0.1",
        pixel_count=3852,
        path="/x",
        send_type="All_To_One",
        mode="capture",
    )
    good.send(bytes(3852 * 3), now=0)
    assert len(good._engine.captures()[0][0]) <= 65507
    with pytest.raises(ValueError):
        OSCSender._test_sender(
            destination="127.0.0.1",
            pixel_count=3853,
            path="/x",
            send_type="All_To_One",
            mode="capture",
        )
    for path in ["bad", "/bad\0"]:
        with pytest.raises(ValueError):
            OSCSender._test_sender(
                destination="127.0.0.1",
                pixel_count=1,
                path=path,
                send_type="One_Argument",
                mode="capture",
            )
    with pytest.raises(ValueError):
        realtime("DNRGB", 65537)


@pytest.mark.parametrize("protocol", ["osc", "realtime"])
@pytest.mark.parametrize("failure", ["size", "numeric", "closed"])
def test_reentrant_exporter_cleanup(protocol: str, failure: str) -> None:
    import subprocess
    import sys

    if sys.version_info < (3, 12):
        pytest.skip("PEP688 requires Python3.12")
    script = """
import sys
from array import array
from ledfx_senders.packet_senders import OSCSender, UDPRealtimeSender
protocol,failure=sys.argv[1:]
sender=(OSCSender._test_sender(destination='127.0.0.1',pixel_count=1,path='/x',send_type='One_Argument',mode='capture') if protocol=='osc' else UDPRealtimeSender._test_sender(destination='127.0.0.1',pixel_count=1,packet_type='DRGB',timeout=1,minimise_traffic=True,mode='capture'))
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
try:sender._engine.send(Exporter(), 2 if failure=='numeric' else 0, 0)
except (ValueError,RuntimeError,BufferError):pass
else:raise AssertionError('invalid accepted')
assert events==['export','release'],events
"""
    subprocess.run(
        [sys.executable, "-I", "-c", script, protocol, failure], check=True, timeout=10
    )


@pytest.mark.parametrize("protocol", ["osc", "realtime"])
@pytest.mark.parametrize("backend", ["portable", "batched"])
def test_real_owned_loopback_and_closed_sender(protocol: str, backend: str) -> None:
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(1)
        target = f"127.0.0.1:{receiver.getsockname()[1]}"
        if protocol == "osc":
            sender = OSCSender._test_sender(
                destination="192.0.2.1",
                pixel_count=2,
                path="/bench/{address}",
                send_type="One_Argument",
                mode="socket",
                backend=backend,
                override_destination=target,
            )
        else:
            sender = UDPRealtimeSender._test_sender(
                destination="192.0.2.1",
                pixel_count=490,
                packet_type="DNRGB",
                timeout=1,
                minimise_traffic=True,
                mode="socket",
                backend=backend,
                override_destination=target,
            )
        sender.send(bytes([42]) * sender.channel_count, now=0)
        first = receiver.recvfrom(65535)
        second = receiver.recvfrom(65535)
        assert first[1] == second[1] and first[1][1] != receiver.getsockname()[1]
        if protocol == "osc":
            assert (
                decode(first[0])[0] == "/bench/0" and decode(second[0])[0] == "/bench/1"
            )
        else:
            assert first[0][:4] == bytes([4, 1, 0, 0]) and second[0] == bytes(
                [4, 1, 1, 233, 42, 42, 42]
            )
        sender.close()
        sender.close()
        with pytest.raises(RuntimeError):
            sender.send(bytes(sender.channel_count), now=1)


def test_maximum_dnrgb_index_and_timestamp_atomicity() -> None:
    sender = realtime("DNRGB", 65536)
    sender.send(bytes(65536 * 3), now=0)
    packets = sender._engine.captures()
    assert len(packets) == 135 and packets[-1][0][2:4] == (65526).to_bytes(2, "big")
    before = (
        sender._engine.counters(),
        sender._engine.frame_counters(),
        sender._engine.committed_copy(),
    )
    for now in [float("nan"), float("inf"), -float("inf")]:
        with pytest.raises(ValueError):
            sender.send(bytes(65536 * 3), now=now)
        assert (
            sender._engine.counters(),
            sender._engine.frame_counters(),
            sender._engine.committed_copy(),
        ) == before
