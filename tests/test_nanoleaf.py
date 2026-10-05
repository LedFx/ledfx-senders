"""Nanoleaf official version-1/2 fixtures, atomic validation and UDP ownership."""

import socket

import numpy as np
import pytest

from ledfx_senders import NanoleafSender


def sender(
    version: int, ids: tuple[int, ...], mode: str = "capture", port: int = 60222
) -> NanoleafSender:
    return NanoleafSender._test_sender(
        destination="127.0.0.1", port=port, version=version, panel_ids=ids, mode=mode
    )


@pytest.mark.parametrize(
    ("version", "expected"),
    [
        (1, "020701112233000109014455660001"),
        (2, "000200071122330000000009445566000000"),
    ],
)
def test_independent_official_layout(version: int, expected: str) -> None:
    s = sender(version, (7, 9))
    s.send(np.array([[17, 34, 51], [68, 85, 102]], dtype=np.uint8))
    assert s._engine.captures()[0][0] == bytes.fromhex(expected)
    s.close()


def test_clipping_avoids_platform_dependent_int_overflow() -> None:
    s = sender(2, (23,))
    s.send(np.array([[-1e300, 255.9, 1e300]]))
    assert s._engine.captures()[0][0] == bytes.fromhex("0001001700ffff000000")
    before = s._engine.committed_copy()
    for frame in (
        np.array([[1, 2, np.nan]]),
        np.array([[1, 2, np.inf]]),
        np.zeros((2, 3)),
    ):
        with pytest.raises(ValueError):
            s.send(frame)
        assert s._engine.committed_copy() == before
        assert len(s._engine.captures()) == 1
    s.close()


@pytest.mark.parametrize(
    ("version", "ids"),
    [
        (1, (256,)),
        (1, (-1,)),
        (1, tuple(range(256))),
        (2, (65536,)),
        (2, tuple(range(8189))),
        (2, (1, 1)),
        (2, ()),
    ],
)
def test_invalid_metadata_rejected_before_transport(
    version: int, ids: tuple[int, ...]
) -> None:
    with pytest.raises((ValueError, OverflowError)):
        sender(version, ids)


@pytest.mark.parametrize(
    ("version", "count", "size"), [(1, 255, 1786), (2, 8188, 65506)]
)
def test_full_datagram_boundary_without_ip_fragment_delivery(
    version: int, count: int, size: int
) -> None:
    s = sender(version, tuple(range(count)))
    s.send(bytes(count * 3))
    assert len(s._engine.captures()[0][0]) == size
    s.close()


def test_actual_loopback_delivery_and_close() -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(1)
        s = sender(1, (42,), mode="socket", port=receiver.getsockname()[1])
        s.send(np.array([[1, 2, 3]], dtype=np.uint8))
        assert receiver.recv(128) == bytes.fromhex("012a010102030001")
        s.close()
        s.close()
        assert s.closed
        with pytest.raises(RuntimeError):
            s.send(bytes(3))


@pytest.mark.parametrize("dtype", [np.float16, np.longdouble])
def test_rare_clamp_truncates_in_original_precision(dtype: type[np.floating]) -> None:
    s = sender(2, (7,))
    frame = np.array(
        [[np.nextafter(dtype(2), dtype(0)), dtype(-1024), dtype(1024)]], dtype=dtype
    )
    s.send(frame)
    assert s._engine.captures()[0][0] == bytes.fromhex("000100070100ff000000")
    s.close()


@pytest.mark.parametrize("operation", ["send", "close"])
def test_contended_calls_release_gil(operation: str) -> None:
    import faulthandler
    import sys
    import threading
    from concurrent.futures import ThreadPoolExecutor

    from ledfx_senders import _native

    s = sender(1, (7,))
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
from ledfx_senders import NanoleafSender
failure=sys.argv[1]
sender=NanoleafSender._test_sender(destination="127.0.0.1",port=60222,version=1,panel_ids=(7,),mode="capture")
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
try:sender._engine.send(Exporter())
except (ValueError,RuntimeError,BufferError):pass
else:raise AssertionError('invalid accepted')
assert events==['export','release'],events
"""
    subprocess.run(
        [sys.executable, "-I", "-c", script, failure], check=True, timeout=10
    )
