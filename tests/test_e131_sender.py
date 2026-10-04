from ledfx_senders.e131 import E131Sender
from ledfx_senders.e131_buffer import ChannelLayout


def test_capture_sequence_and_close():
    sender = E131Sender._test_sender(
        ChannelLayout(513, universe_size=512),
        destination="127.0.0.1",
        source_name="test",
        mode="capture",
        clock=lambda: 0.0,
    )
    for _ in range(257):
        sender.send(bytes(513))
    packets = sender._engine.captures()
    data = [p for p, _ in packets if len(p) == 638]
    assert [p[111] for p in data[::2]] == list(range(256)) + [0]
    sync = [(p, d) for p, d in packets if len(p) == 49]
    assert len(sync) == 257
    assert [p[44] for p, _ in sync] == list(range(256)) + [0]
    assert [len(p) for p, _ in packets] == [638, 638, 49] * 257
    assert all(d == "239.255.249.255:5568" for _, d in sync)
    sender.close()
    assert sender.closed


import socket
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest


def make_sender(count: int = 3) -> E131Sender:
    return E131Sender._test_sender(
        ChannelLayout(count, universe_size=1),
        destination="multicast",
        source_name="test",
        mode="capture",
        clock=lambda: 0.0,
    )


def test_refresh_discovery_coalescing_and_owned_snapshot():
    sender = make_sender()
    frame = bytearray([1, 2, 3])
    sender.send(frame)
    frame[:] = bytes([9, 9, 9])
    sender.service(0)
    before = sender._engine.counters()
    sender.service(0.79)
    assert sender._engine.counters() == before
    sender.service(0.8)
    packets = sender._engine.captures()
    assert [p[126] for p, _ in packets if len(p) == 638] == [1, 2, 3] * 2
    sender.service(100)
    added = sender._engine.captures()[len(packets) :]
    assert len(added) == 5
    assert added[0][1] == "239.255.250.214:5568"
    assert [len(p) for p, _ in added[1:]] == [638, 638, 638, 49]


def test_discovery_pagination():
    sender = make_sender(513)
    sender.send(bytes(513))
    start = len(sender._engine.captures())
    sender.service(0)
    captures = sender._engine.captures()[start:]
    assert [len(p) for p, _ in captures] == [1144, 122]
    assert [p[118:120] for p, _ in captures] == [bytes([0, 1]), bytes([1, 1])]
    assert all(d == "239.255.250.214:5568" for _, d in captures)


def test_close_blackout_and_termination_and_unused():
    sender = make_sender()
    sender.close()
    assert sender._engine.captures() == []
    sender.service(100)
    with pytest.raises(RuntimeError):
        sender.send(bytes(3))
    sender = make_sender()
    sender.send(bytes([4, 5, 6]))
    start = len(sender._engine.captures())
    sender.close()
    packets = sender._engine.captures()[start:]
    assert len(packets) == 13
    assert all(p[126:] == bytes(512) for p, _ in packets if len(p) == 638)
    assert [p[112] for p, _ in packets if len(p) == 638] == [0] * 3 + [64] * 9
    sender.close()
    assert len(sender._engine.captures()) == start + 13


@pytest.mark.parametrize("backend", ["portable", "batched"])
def test_real_loopback_all_packets_use_same_state_machine(backend: str):
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.bind(("127.0.0.1", 0))
        receiver.settimeout(2)
        sender = E131Sender._test_sender(
            ChannelLayout(3, universe_size=1),
            destination="multicast",
            source_name="test",
            mode="socket",
            backend=backend,
            override_destination=f"127.0.0.1:{receiver.getsockname()[1]}",
            clock=lambda: 0.0,
        )
        sender.send(bytes([1, 2, 3]))
        packets = [receiver.recv(2048) for _ in range(4)]
        assert [len(p) for p in packets] == [638, 638, 638, 49]
        assert [p[126] for p in packets[:3]] == [1, 2, 3]
        assert sender._engine.counters()[0] == 4
        sender.service(0)
        packets.append(receiver.recv(2048))
        assert len(packets[-1]) == 126
        assert sender._engine.counters()[:2] == (5, sum(map(len, packets)))
        actual_backend = (
            "batched"
            if backend == "batched" and sys.platform == "linux"
            else "portable"
        )
        assert sender._engine.transport_info() == (
            actual_backend,
            64,
            5 if actual_backend == "portable" else 3,
            0,
        )
        sender.close(False)
        assert all(receiver.recv(2048)[112] == 64 for _ in range(9))


def test_concurrent_direct_native_calls_and_python_heartbeat():
    sender = E131Sender._test_sender(
        ChannelLayout(50000),
        destination="127.0.0.1",
        source_name="test",
        mode="discard",
    )
    import faulthandler

    faulthandler.dump_traceback_later(10, exit=True)
    stop = threading.Event()
    started = threading.Event()
    barrier = threading.Barrier(2)
    beats = []

    def heartbeat():
        while not stop.is_set():
            beats.append(1)
            stop.wait(0.001)

    thread = threading.Thread(target=heartbeat)
    thread.start()

    def send():
        barrier.wait()
        for i in range(100):
            try:
                sender._engine.send(bytes(50000), i * 0.01)
                if i == 10:
                    started.set()
            except RuntimeError:
                pass

    def service():
        barrier.wait()
        for i in range(100):
            sender._engine.service(i * 0.01)

    def close():
        assert started.wait(5)
        sender.close()

    with ThreadPoolExecutor(max_workers=3) as pool:
        futures = [pool.submit(send), pool.submit(service), pool.submit(close)]
        for future in futures:
            future.result(timeout=10)
    faulthandler.cancel_dump_traceback_later()
    stop.set()
    thread.join(timeout=2)
    assert len(beats) > 1
    assert sender.closed


def test_invalid_frame_leaves_counters_and_banks_unchanged():
    import numpy as np

    sender = make_sender()
    sender.send(bytes([1, 2, 3]))
    counters = sender._engine.counters()
    committed = sender._engine.committed_copy()
    with pytest.raises(ValueError):
        sender.send(np.array([1, 2, float("nan")]))
    assert sender._engine.counters() == counters
    assert sender._engine.committed_copy() == committed


def test_real_multicast_production_sender():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        receiver.bind(("", 5568))
        receiver.settimeout(2)
        for group in ("239.255.0.1", "239.255.249.255", "239.255.250.214"):
            receiver.setsockopt(
                socket.IPPROTO_IP,
                socket.IP_ADD_MEMBERSHIP,
                socket.inet_aton(group) + socket.inet_aton("127.0.0.1"),
            )
        sender = E131Sender(
            ChannelLayout(3), destination="multicast", source_name="test"
        )
        sender._engine._test_loopback_multicast()
        sender.send(bytes([12, 13, 14]))
        datagrams = [receiver.recvfrom(2048) for _ in range(2)]
        assert all(address[0] == "127.0.0.1" for _, address in datagrams)
        assert all(0 < address[1] != 5568 for _, address in datagrams)
        packets = [packet for packet, _ in datagrams]
        assert [len(p) for p in packets] == [638, 49]
        assert packets[0][126:129] == bytes([12, 13, 14])
        sender.service()
        assert len(receiver.recv(2048)) == 122
        sender.close(False)
        assert all(receiver.recv(2048)[112] == 64 for _ in range(3))


@pytest.mark.parametrize(
    "mode,backend,batch_size,override_destination",
    [
        ("bogus", "batched", 64, None),
        ("capture", "bogus", 64, None),
        ("capture", "batched", 0, None),
        ("capture", "batched", 64, "192.0.2.1:5568"),
    ],
)
def test_internal_factory_rejects_invalid_backend_configuration(
    mode: str,
    backend: str,
    batch_size: int,
    override_destination: str | None,
):
    with pytest.raises(ValueError):
        E131Sender._test_sender(
            ChannelLayout(3),
            destination="multicast",
            source_name="test",
            mode=mode,
            backend=backend,
            batch_size=batch_size,
            override_destination=override_destination,
        )


def test_discovery_exact_ten_second_boundary():
    sender = make_sender()
    sender.send(bytes(3))
    sender.service(0)
    sender.service(9.999)
    assert sum(d == "239.255.250.214:5568" for _, d in sender._engine.captures()) == 1
    sender.service(10)
    assert sum(d == "239.255.250.214:5568" for _, d in sender._engine.captures()) == 2


@pytest.mark.parametrize("target", ["engine", "banks"])
@pytest.mark.parametrize("callback", ["export", "release"])
@pytest.mark.parametrize("wrapped", [False, True])
@pytest.mark.skipif(
    sys.version_info < (3, 12),
    reason="PEP 688 Python buffer exporters require Python 3.12",
)
def test_buffer_protocol_reentrancy_under_subprocess_watchdog(
    target: str,
    callback: str,
    wrapped: bool,
):
    import subprocess
    import textwrap

    script = textwrap.dedent("""
        import sys
        from ledfx_senders.e131 import E131Sender
        from ledfx_senders.e131_buffer import ChannelLayout, PacketBanks
        target, callback, wrapped = sys.argv[1:]
        events = []
        if target == 'engine':
            sender = E131Sender._test_sender(ChannelLayout(3), destination='multicast',
                                           source_name='test', mode='capture')
            native = sender._engine
            def update(frame): native.send(frame, 0.0)
            def inspect(): return native.closed
        else:
            native = PacketBanks(ChannelLayout(3)).native
            update = native.update
            inspect = native.snapshot
        class Exporter:
            def __buffer__(self, flags):
                events.append('export')
                if callback == 'export':
                    inspect()
                    update(bytes([4, 5, 6]))
                return memoryview(bytes([1, 2, 3]))
            def __release_buffer__(self, buffer):
                events.append('release')
                if callback == 'release':
                    inspect()
                    update(bytes([7, 8, 9]))
        exporter = Exporter()
        if wrapped == 'True':
            view = memoryview(exporter)
            update(view)
            view.release()
        else:
            update(exporter)
        assert events == ['export', 'release'], events
        expected = [4, 1] if callback == 'export' else [1, 7]
        if target == 'engine':
            assert [p[126] for p, _ in native.captures() if len(p) == 638] == expected
        else:
            assert native.snapshot()[0][126] == expected[-1]
    """)
    result = subprocess.run(
        [sys.executable, "-c", script, target, callback, str(wrapped)],
        capture_output=True,
        check=False,
        text=True,
        timeout=5,
    )
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize(
    "target,failure",
    [
        ("engine", "count"),
        ("banks", "count"),
        ("engine", "float"),
        ("banks", "float"),
        ("engine", "closed"),
    ],
)
@pytest.mark.skipif(
    sys.version_info < (3, 12),
    reason="PEP 688 Python buffer exporters require Python 3.12",
)
def test_buffer_release_reentrancy_on_native_errors(
    target: str,
    failure: str,
):
    import subprocess
    import textwrap

    script = textwrap.dedent("""
        import sys
        from array import array
        from ledfx_senders.e131 import E131Sender
        from ledfx_senders.e131_buffer import ChannelLayout, PacketBanks
        target, failure = sys.argv[1:]
        events = []
        if target == 'engine':
            sender = E131Sender._test_sender(ChannelLayout(3), destination='multicast',
                                           source_name='test', mode='capture')
            native = sender._engine
            def update(frame): native.send(frame, 0.0)
            def inspect(): return native.closed
            if failure == 'closed': native.close(False, 0.0)
        else:
            native = PacketBanks(ChannelLayout(3)).native
            update = native.update
            inspect = native.snapshot
        class Exporter:
            def __buffer__(self, flags):
                events.append('export')
                inspect()
                data = array('d', [1, 2, float('nan')]) if failure == 'float' else bytes(2)
                return memoryview(data)
            def __release_buffer__(self, buffer):
                inspect()
                events.append('release')
        try:
            update(Exporter())
        except (OSError, RuntimeError, ValueError, BufferError):
            pass
        else:
            raise AssertionError('invalid input unexpectedly accepted')
        assert events == ['export','release'], events
    """)
    result = subprocess.run(
        [sys.executable, "-c", script, target, failure],
        capture_output=True,
        check=False,
        text=True,
        timeout=5,
    )
    assert result.returncode == 0, result.stderr


def test_service_before_first_complete_frame_is_idle():
    sender = make_sender()
    sender.service(0)
    sender.service(100)
    assert sender._engine.captures() == []
    assert sender._engine.counters() == (0, 0, 0)
    sender.send(bytes([1, 2, 3]))
    assert [len(p) for p, _ in sender._engine.captures()] == [638, 638, 638, 49]
    sender.service(100)
    captures = sender._engine.captures()
    assert captures[4][1] == "239.255.250.214:5568"
