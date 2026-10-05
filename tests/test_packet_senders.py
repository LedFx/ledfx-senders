"""Independent DDP and OPC wire oracle; no LedFx imports."""

import socket
import struct
import warnings
from typing import TypeAlias

import numpy as np
import pytest

from ledfx_senders import DDPSender, OPCSender

Sender: TypeAlias = DDPSender | OPCSender


def numeric_oracle(frame: np.ndarray, opc: bool = False) -> bytes:
    """Integer math from original scalar precision, never float->uint8 casts."""
    output = bytearray()
    for value in frame.flat:
        integer = int(value)
        if opc:
            output.append(min(255, max(0, integer)))
        elif frame.dtype.kind == "f" and not (-2147483648 <= value < 2147483648):
            output.append(0)
        else:
            output.append(integer % 256)
    return bytes(output)


def capture(
    cls: type[Sender], count: int, *, destination_id: int = 1, channel: int = 0
) -> Sender:
    return cls._test_sender(
        count,
        destination="127.0.0.1",
        mode="capture",
        destination_id=destination_id,
        channel=channel,
    )


@pytest.mark.parametrize("pixels", [1, 479, 480, 481, 960, 961, 50000])
def test_ddp_wire(pixels: int) -> None:
    sender = capture(DDPSender, pixels * 3, destination_id=249)
    frame = (np.arange(pixels * 3) % 256).astype(np.uint8)
    sender.send(frame)
    packets = [p for p, _ in sender._engine.captures()]
    assert len(packets) == (pixels * 3 + 1439) // 1440
    for i, packet in enumerate(packets):
        payload = frame.tobytes()[i * 1440 : (i + 1) * 1440]
        assert (
            packet
            == struct.pack(
                "!BBBBIH",
                0x41 if i == len(packets) - 1 else 0x40,
                2,
                11,
                249,
                i * 1440,
                len(payload),
            )
            + payload
        )
    sender.close()


def test_ddp_sequence_wrap() -> None:
    sender = capture(DDPSender, 3)
    for _ in range(32):
        sender.send(bytes(3))
    assert [p[1] for p, _ in sender._engine.captures()] == [
        i % 15 + 1 for i in range(1, 33)
    ]


@pytest.mark.parametrize("pixels", [1, 480, 1000, 21834])
def test_opc_wire(pixels: int) -> None:
    sender = capture(OPCSender, pixels, channel=247)
    frame = (np.arange(pixels * 3) % 256).astype(np.uint8).reshape(pixels, 3)
    sender.send(frame)
    assert (
        sender._engine.captures()[0][0]
        == bytes([247, 0]) + (pixels * 3).to_bytes(2, "big") + frame.tobytes()
    )


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize(
    "dtype", ["float32", "float64", ">f8", "int16", "uint64", "uint8"]
)
def test_numeric_policy_strides_and_alias(cls: type[Sender], dtype: str) -> None:
    if dtype == "uint64":
        frame = np.array([[2**64 - 257, 2**64 - 1, 256], [255, 511, 100]], dtype=dtype)
    elif dtype == "uint8":
        frame = np.array([[255, 255, 0], [255, 255, 100]], dtype=dtype)
    else:
        frame = np.array(
            [[-257.9, -1.9, 256.9], [255.9, 511.2, 100.8]], dtype="float64"
        ).astype(dtype)
    frame = np.repeat(frame, 2, axis=0)[::2, ::-1]
    original = frame.copy()
    sender = capture(cls, 6 if cls is DDPSender else 2)
    expected = numeric_oracle(frame, opc=cls is OPCSender)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        sender.send(frame)
    packet = sender._engine.captures()[0][0]
    assert packet[10 if cls is DDPSender else 4 :] == expected
    np.testing.assert_array_equal(frame, original)
    frame[:] = 0
    assert sender._engine.captures()[0][0] == packet


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize("value", [float("nan"), float("inf"), -float("inf")])
@pytest.mark.parametrize("dtype", ["float64", "float16", ">f8", "longdouble"])
def test_late_invalid_atomicity(cls: type[Sender], value: float, dtype: str) -> None:
    sender = capture(cls, 3000 if cls is DDPSender else 1000)
    frame = np.zeros((1000, 3), dtype=dtype)
    sender.send(frame)
    before = (
        sender._engine.captures(),
        sender._engine.counters(),
        sender._engine.committed_copy(),
    )
    frame[-1, -1] = value
    with pytest.raises((ValueError, OverflowError)):
        sender.send(frame)
    assert (
        sender._engine.captures(),
        sender._engine.counters(),
        sender._engine.committed_copy(),
    ) == before
    frame[-1, -1] = 1
    sender.send(frame)
    if cls is DDPSender:
        assert sender._engine.captures()[-1][0][1] == 3


@pytest.mark.parametrize("cls,count", [(DDPSender, 6), (OPCSender, 2)])
def test_bytes_close_and_loopback(cls: type[Sender], count: int) -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(1)
        port = int(receiver.getsockname()[1])
        sender = cls(count, destination="127.0.0.1", port=port)
        sender.send(b"abcdef")
        packet, address = receiver.recvfrom(65535)
        assert packet.endswith(b"abcdef") and address[1] != receiver.getsockname()[1]
        sender.close()
        sender.close()
        assert sender.closed
        with pytest.raises(RuntimeError):
            sender.send(b"abcdef")


def test_opc_limits_and_shape() -> None:
    with pytest.raises(ValueError):
        capture(OPCSender, 21835)
    sender = capture(OPCSender, 1)
    with pytest.raises(ValueError):
        sender.send(np.zeros(3))
    with pytest.raises(ValueError):
        sender.send(b"x")


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize("operation", ["send", "close"])
def test_contended_calls_release_gil(cls: type[Sender], operation: str) -> None:
    import faulthandler
    import sys
    import threading
    from concurrent.futures import ThreadPoolExecutor

    from ledfx_senders import _native

    sender = capture(cls, 3 if cls is DDPSender else 1)
    gate = _native._TestLockGate()
    attempted, completed = threading.Event(), threading.Event()

    def contend() -> None:
        attempted.set()
        if operation == "send":
            sender.send(bytes(3))
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


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize("dtype", ["float32", "float64", "longdouble"])
def test_finite_boundaries(cls: type[Sender], dtype: str) -> None:
    frame = np.array(
        [
            [-1025.99, -256.99, -1.01],
            [-0.99, 255.99, 256.01],
            [1024.99, 1048575, -1048576],
        ],
        dtype=dtype,
    )
    sender = capture(cls, 9 if cls is DDPSender else 3)
    expected = numeric_oracle(frame, opc=cls is OPCSender)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        sender.send(frame)
    assert sender._engine.captures()[0][0][10 if cls is DDPSender else 4 :] == expected


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
def test_dtype_comparison_alias_uses_original_precision(cls: type[Sender]) -> None:
    frame = np.array([[1, 2, 3]], dtype=np.longdouble)
    sender = capture(cls, 3 if cls is DDPSender else 1)
    sender.send(frame)
    assert sender._engine.captures()[0][0].endswith(b"\x01\x02\x03")
    frame[0, -1] = np.longdouble("nan")
    with pytest.raises(ValueError):
        sender.send(frame)
    assert len(sender._engine.captures()) == 1


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize("callback", ["export", "release"])
def test_native_exporter_reentrancy(cls: type[Sender], callback: str) -> None:
    import subprocess
    import sys

    if sys.version_info < (3, 12):
        pytest.skip("Python buffer exporters require Python 3.12")
    script = """
import sys
from ledfx_senders import DDPSender, OPCSender
cls = DDPSender if sys.argv[1] == 'ddp' else OPCSender
sender = cls._test_sender(3 if cls is DDPSender else 1, destination='127.0.0.1', mode='capture')
events = []
class Exporter:
    def __buffer__(self, flags):
        events.append('export')
        if sys.argv[2] == 'export': sender.send(bytes([4,5,6]))
        return memoryview(bytes([1,2,3]))
    def __release_buffer__(self, view):
        events.append('release')
        if sys.argv[2] == 'release': sender.send(bytes([7,8,9]))
sender._engine.send(Exporter())
assert events == ['export', 'release']
assert [p[-3] for p,_ in sender._engine.captures()] == ([4,1] if sys.argv[2] == 'export' else [1,7])
"""
    subprocess.run(
        [
            sys.executable,
            "-I",
            "-c",
            script,
            "ddp" if cls is DDPSender else "opc",
            callback,
        ],
        timeout=15,
        check=True,
    )


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize("backend", ["portable", "batched"])
def test_routed_loopback_preserves_packet_boundaries(
    cls: type[Sender], backend: str
) -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(1)
        sender = cls._test_sender(
            1443 if cls is DDPSender else 480,
            destination="192.0.2.1",
            mode="socket",
            backend=backend,
            override_destination=f"127.0.0.1:{receiver.getsockname()[1]}",
        )
        sender.send(bytes([42]) * sender.channel_count)
        packets = [
            receiver.recvfrom(65535)[0] for _ in range(2 if cls is DDPSender else 1)
        ]
        if cls is DDPSender:
            assert list(map(len, packets)) == [1450, 13]
            assert packets[0][0] == 0x40 and packets[1][0] == 0x41
        else:
            assert packets[0] == bytes([0, 0, 5, 160]) + bytes([42]) * 1440
        assert sender._engine.counters() == (len(packets), sum(map(len, packets)), 0)
        sender.close()


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
def test_byte_views_and_rejection(cls: type[Sender]) -> None:
    sender = capture(cls, 3 if cls is DDPSender else 1)
    for frame in (bytearray([1, 2, 3]), memoryview(bytes([1, 2, 3]))):
        sender.send(frame)
    before = sender._engine.counters()
    with pytest.raises(ValueError):
        sender.send(memoryview(bytes(6))[::2])
    with pytest.raises(ValueError):
        sender.send(np.array([[1 + 2j, 3, 4]]))
    assert sender._engine.counters() == before


@pytest.mark.parametrize("cls", [DDPSender, OPCSender])
@pytest.mark.parametrize("count", [0, -1, True])
def test_invalid_configuration(cls: type[Sender], count: int) -> None:
    with pytest.raises((TypeError, ValueError)):
        capture(cls, count)
    with pytest.raises(ValueError):
        cls._test_sender(
            3 if cls is DDPSender else 1,
            destination="127.0.0.1",
            mode="capture",
            port=0,
        )


def test_protocol_numeric_policies_are_distinct() -> None:
    from ledfx_senders import E131Sender
    from ledfx_senders.e131 import ChannelLayout

    frame = np.array([[-1.0, 256.0, 300.0]])
    ddp = capture(DDPSender, 3)
    opc = capture(OPCSender, 1)
    strict = E131Sender._test_sender(
        ChannelLayout(3), destination="127.0.0.1", source_name="policy", mode="capture"
    )
    ddp.send(frame)
    opc.send(frame)
    assert ddp._engine.captures()[0][0][-3:] == bytes([255, 0, 44])
    assert opc._engine.captures()[0][0][-3:] == bytes([0, 255, 255])
    with pytest.raises(ValueError):
        strict.send(frame)
    assert strict._engine.counters() == (0, 0, 0)
    strict.close(False)


@pytest.mark.parametrize("dtype", ["float32", "float64", "longdouble", ">f8"])
@pytest.mark.parametrize(
    "first,second,error",
    [
        (float("nan"), float("inf"), ValueError),
        (float("inf"), float("nan"), OverflowError),
    ],
)
def test_opc_preserves_first_invalid_exception(
    dtype: str, first: float, second: float, error: type[Exception]
) -> None:
    sender = capture(OPCSender, 1)
    sender.send(bytes(3))
    before = (
        sender._engine.captures(),
        sender._engine.counters(),
        sender._engine.committed_copy(),
    )
    frame = np.array([[first, second, 0.0]], dtype=dtype)
    with pytest.raises(error):
        sender.send(frame)
    assert (
        sender._engine.captures(),
        sender._engine.counters(),
        sender._engine.committed_copy(),
    ) == before


@pytest.mark.parametrize("dtype", ["float32", "float64", ">f4", ">f8", "longdouble"])
def test_ddp_float_cutoffs_preserve_original_precision(dtype: str) -> None:
    kind = np.dtype(dtype).type
    lower, upper = kind(-2147483648), kind(2147483648)
    values = [
        kind(-1025.99),
        kind(-1.99),
        kind(-0.99),
        kind(255.99),
        kind(256.01),
        np.nextafter(lower, kind("-inf")),
        lower,
        np.nextafter(lower, kind("inf")),
        np.nextafter(upper, kind("-inf")),
        upper,
        np.nextafter(upper, kind("inf")),
        kind(np.finfo(kind).max),
    ]
    frame = np.repeat(np.array(values, dtype=dtype), 2)[::2]
    sender = capture(DDPSender, frame.size)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        sender.send(frame)
    assert sender._engine.captures()[0][0][10:] == numeric_oracle(frame)
    # The last representable value below 2**31 must not round up through f64.
    if np.finfo(kind).nmant > np.finfo(np.float64).nmant:
        assert sender._engine.captures()[0][0][10 + 8] == 255


@pytest.mark.parametrize(
    "dtype", ["float16", "float32", "float64", ">f8", "longdouble"]
)
def test_ddp_normal_channel_numpy_parity(dtype: str) -> None:
    frame = np.array([0, 0.99, 1.01, 100.75, 254.99, 255], dtype=dtype)
    sender = capture(DDPSender, frame.size)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        sender.send(frame)
        expected = frame.astype(np.uint8).tobytes()
    assert sender._engine.captures()[0][0][10:] == expected
