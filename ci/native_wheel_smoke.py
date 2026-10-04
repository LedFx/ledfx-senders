"""Run with python -I: exercise an installed extension using only the stdlib.

Multicast is explicitly routed over 127.0.0.1. No default-interface traffic.
No Rust, NumPy, application dependency install, or source-tree import is needed.
"""

import array
import faulthandler
import ipaddress
import json
import os
import socket
import sys
import sysconfig
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import cast

from ledfx_senders import _native as _e131
from ledfx_senders import e131_packet


def arguments() -> tuple[
    list[bytes], list[tuple[int, int, int, int]], int, list[str], bytes, list[bytes]
]:
    cid = bytes(range(16))
    return (
        [
            bytes(
                e131_packet.data_template(
                    1,
                    cid=cid,
                    source_name="wheel-smoke",
                    priority=100,
                    sync_universe=63999,
                )
            )
        ],
        [(0, 0, 0, 3)],
        3,
        ["239.255.0.1:5568"],
        bytes(e131_packet.sync_template(cid=cid, sync_universe=63999)),
        list(e131_packet.discovery_packets((1,), cid=cid, source_name="wheel-smoke")),
    )


def receive(
    receiver: socket.socket,
    size: int,
    offset: int | None = None,
    value: bytes | None = None,
) -> bytes:
    packet, address = receiver.recvfrom(2048)
    assert ipaddress.ip_address(address[0]).is_loopback, address
    assert 0 < address[1] != 5568, address  # native socket bound an ephemeral port
    assert len(packet) == size, (len(packet), size)
    assert packet[:16] == bytes.fromhex("001000004153432d45312e3137000000")
    assert int.from_bytes(packet[16:18], "big") == 0x7000 | (size - 16)
    if offset is not None:
        assert value is not None
        assert packet[offset : offset + len(value)] == value
    return packet


def exercise(
    multicast: bool, backend: str
) -> dict[str, bool | tuple[str, int, int, int]]:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
        receiver.settimeout(3)
        if multicast:
            receiver.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            receiver.bind(("", 5568))
            for group in ("239.255.0.1", "239.255.249.255", "239.255.250.214"):
                receiver.setsockopt(
                    socket.IPPROTO_IP,
                    socket.IP_ADD_MEMBERSHIP,
                    socket.inet_aton(group) + socket.inet_aton("127.0.0.1"),
                )
            override = None
        else:
            receiver.bind(("127.0.0.1", 0))
            override = f"127.0.0.1:{receiver.getsockname()[1]}"
        engine = _e131.Engine._test_engine(
            *arguments(), "socket", backend, 64, override
        )
        # Always set before send/service/close, including sync and discovery.
        engine._test_loopback_multicast()
        try:
            now = time.monotonic()
            for frame in (bytes([12, 13, 14]), array.array("d", [21.9, 22.1, 23.0])):
                engine.send(frame, now)
                expected = bytes(int(value) for value in frame)
                packet = receive(receiver, 638, 126, expected)
                assert packet[129:] == bytes(509)
                receive(receiver, 49)
                now += 0.1
            engine.service(now)
            receive(receiver, 122, 118, bytes([0, 0, 0, 1]))
        finally:
            engine.close(False, time.monotonic())
        for _ in range(3):
            receive(receiver, 638, 112, b"\x40")
        assert engine.closed
        assert engine.cleanup_error() is None
        engine.close(False, time.monotonic())
        try:
            engine.send(b"\0\0\0", time.monotonic())
        except RuntimeError:
            pass
        else:
            raise AssertionError("closed engine accepted a frame")
        return {"multicast": multicast, "transport": engine.transport_info()}


def main() -> None:
    extension = Path(_e131.__file__).resolve()
    checkout = Path(__file__).resolve().parents[1]
    assert not extension.is_relative_to(checkout), extension
    free_threaded = bool(sysconfig.get_config_var("Py_GIL_DISABLED"))
    if os.environ.get("LEDFX_REQUIRE_FREE_THREADING") == "1":
        assert free_threaded, "Expected a free-threaded interpreter"
    gil_enabled = cast(
        Callable[[], bool], getattr(sys, "_is_gil_enabled", lambda: True)
    )
    if free_threaded:
        assert not gil_enabled(), "Extension import re-enabled the GIL"
    results = [
        exercise(multicast, backend)
        for backend in ("portable", "batched")
        for multicast in (False, True)
    ]
    concurrency()
    if free_threaded:
        assert not gil_enabled(), "GIL enabled during concurrent execution"
    print(
        json.dumps(
            {
                "extension": str(extension),
                "engine": _e131.engine_info(),
                "free_threaded": free_threaded,
                "tests": results,
                "concurrent_send_service_close": "passed",
            },
            indent=2,
        )
    )


def concurrency() -> None:
    """Shared-engine send/service/close races, without datagram callbacks."""
    faulthandler.dump_traceback_later(30, exit=True)
    try:
        engine = _e131.Engine._test_engine(*arguments(), "capture")
        failures = []
        barrier = threading.Barrier(5)

        def worker(index: int, closing: bool = False) -> None:
            try:
                barrier.wait()
                for _ in range(64):
                    try:
                        if index == 4:
                            if closing:
                                engine.close(False, time.monotonic())
                            else:
                                engine.service(time.monotonic())
                        else:
                            engine.send(bytes([index + 1] * 3), time.monotonic())
                    except RuntimeError:
                        if not closing or not engine.closed:
                            raise
            except BaseException as error:  # noqa: BLE001 - propagate every worker failure
                failures.append(error)

        def run(closing: bool) -> None:
            threads = [
                threading.Thread(target=worker, args=(index, closing), daemon=True)
                for index in range(5)
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(10)
            assert not any(thread.is_alive() for thread in threads), (
                "native lock deadlock"
            )
            assert not failures, failures

        run(False)
        packets = [packet for packet, _ in engine.captures()]
        data = [packet for packet in packets if len(packet) == 638]
        assert len(data) == 256
        assert [packet[111] for packet in data] == list(range(256))
        assert all(
            packet[126:129] in [bytes([value] * 3) for value in range(1, 5)]
            for packet in data
        )
        assert len([packet for packet in packets if len(packet) == 49]) == 256
        run(True)
        assert engine.closed and engine.cleanup_error() is None
        terminations = [
            packet
            for packet, _ in engine.captures()
            if len(packet) == 638 and packet[112] == 64
        ]
        assert len(terminations) == 3
    finally:
        faulthandler.cancel_dump_traceback_later()


if __name__ == "__main__":
    main()
