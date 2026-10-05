"""Public owned Hue sender; strict independent oracle uses dummy credentials."""

import os
from collections.abc import Iterator
from pathlib import Path
from threading import Thread
from typing import TYPE_CHECKING

import numpy as np
import pytest
from hue_support import StrictOracle

from ledfx_senders import _native
from ledfx_senders.frames import Frame

UUID = "12345678-1234-1234-1234-123456789abc"
IDENTITY = b"hue-fixture"
KEY = bytes(range(16))


def make_sender(port: int = 2100) -> "HueSender":
    from ledfx_senders.hue import HueSender

    return HueSender(
        destination="127.0.0.1",
        port=port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=(7,),
    )


# Missing root export or eager socket/session construction breaks this lifecycle.
def test_constructor_is_inert_and_root_export_is_canonical() -> None:
    from ledfx_senders import HueSender
    from ledfx_senders.hue import HueSender as Canonical

    assert HueSender is Canonical
    sender = make_sender()
    assert not sender.connected and not sender.closed
    sender.close()
    sender.close()
    assert sender.closed and not sender.connected
    with pytest.raises(ConnectionError):
        sender.connect()


@pytest.mark.parametrize("timeout", [0, -1, float("nan"), float("inf"), True, 1e308])
@pytest.mark.parametrize("field", ["connect_timeout", "send_timeout", "close_timeout"])
def test_invalid_timeout_is_rejected(timeout: float, field: str) -> None:
    from ledfx_senders.hue import HueSender

    arguments: dict[str, object] = {
        "destination": "127.0.0.1",
        "psk_identity": IDENTITY,
        "client_key": KEY,
        "entertainment_id": UUID,
        "channel_ids": (7,),
    }
    arguments[field] = timeout
    with pytest.raises((TypeError, ValueError)):
        HueSender(**arguments)  # type: ignore[arg-type]


@pytest.mark.parametrize(
    "field,value",
    [
        ("destination", "localhost"),
        ("destination", "[::1]"),
        ("port", 0),
        ("port", 65536),
        ("port", True),
        ("psk_identity", b""),
        ("psk_identity", bytes(65536)),
        ("client_key", b""),
        ("client_key", bytes(65536)),
        ("psk_identity", bytearray(b"id")),
        ("client_key", "secret"),
        ("entertainment_id", "invalid"),
        ("entertainment_id", "é" * 36),
        ("channel_ids", ()),
        ("channel_ids", (7, 7)),
        ("channel_ids", [7]),
        ("channel_ids", (-1,)),
        ("channel_ids", (256,)),
        ("channel_ids", (True,)),
        ("sequence", -1),
        ("sequence", 256),
        ("sequence", True),
    ],
    ids=[f"invalid-{index}" for index in range(22)],
)
def test_constructor_rejects_invalid_metadata(field: str, value: object) -> None:
    from ledfx_senders.hue import HueSender

    arguments: dict[str, object] = {
        "destination": "127.0.0.1",
        "psk_identity": IDENTITY,
        "client_key": KEY,
        "entertainment_id": UUID,
        "channel_ids": (7,),
    }
    arguments[field] = value
    with pytest.raises((TypeError, ValueError)):
        HueSender(**arguments)  # type: ignore[arg-type]


@pytest.mark.parametrize("destination", ["127.0.0.1", "::1"])
def test_numeric_addresses_do_not_connect_in_constructor(destination: str) -> None:
    from ledfx_senders.hue import HueSender

    sender = HueSender(
        destination=destination,
        psk_identity=b"id\0opaque",
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=tuple(range(256)),
    )
    assert not sender.connected
    sender.close()


def test_send_requires_connection_and_matching_count() -> None:
    sender = make_sender()
    with pytest.raises(ValueError):
        sender.send(bytes(6))
    with pytest.raises(ConnectionError):
        sender.send(bytes(3))
    sender.close()


@pytest.fixture
def oracle() -> Iterator[StrictOracle]:
    with StrictOracle(Path(os.environ["HUE_ORACLE"]), IDENTITY, KEY) as peer:
        yield peer


@pytest.mark.parametrize(
    "rgb,ids,suffix",
    [
        (b"\xff\x80\0", (0,), "00ffff80800000"),
        (b"\0\x22\xff", (7,), "0700002222ffff"),
        (b"\1\2\3\0\x22\xff", (12, 7), "0c0101020203030700002222ffff"),
    ],
)
@pytest.mark.parametrize(
    "sequence,prefix",
    [
        (0, b"HueStream\x02\0\0\0\0\0\0"),
        (254, b"HueStream\x02\0\xfe\0\0\0\0"),
        (255, b"HueStream\x02\0\xff\0\0\0\0"),
    ],
)
def test_literal_vectors_and_sequence_is_constant(
    oracle: StrictOracle,
    rgb: bytes,
    ids: tuple[int, ...],
    suffix: str,
    sequence: int,
    prefix: bytes,
) -> None:
    from ledfx_senders.encoders import encode_hue
    from ledfx_senders.hue import HueSender

    identifier = "12345678-1234-5678-1234-567812345678"
    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=identifier,
        channel_ids=ids,
        sequence=sequence,
    )
    expected = prefix + identifier.encode() + bytes.fromhex(suffix)
    try:
        sender.connect()
        assert sender.connected and not sender.closed
        sender.connect()
        sender.send(rgb)
        sender.send(rgb)
        assert oracle.receive(len(expected) * 2, 2) == expected * 2
        assert encode_hue(rgb, identifier, ids, sequence) == expected
    finally:
        sender.close()
    assert sender.closed and not sender.connected


@pytest.mark.parametrize(
    "frame,want",
    [
        (b"\1\2\xff", b"\1\2\xff"),
        (bytearray(b"\1\2\xff"), b"\1\2\xff"),
        (memoryview(b"\1\2\xff"), b"\1\2\xff"),
        (np.array([[True, False, True]]), b"\1\0\1"),
        *[
            (np.array([[1, 2, 255]], dtype=dtype), b"\1\2\xff")
            for dtype in ["u1", "i2", "u8", "f4", "f8", ">f8", np.longdouble]
        ],
        (np.array([[-0.9, 255.9, 2.9]]), b"\0\xff\2"),
        (np.array([[1, 99, 2, 99, 255, 99]])[:, ::2], b"\1\2\xff"),
    ],
)
def test_numeric_frames_match_wire(
    oracle: StrictOracle, frame: Frame, want: bytes
) -> None:
    sender = make_sender(oracle.port)
    expected = (
        b"HueStream\x02\0\0\0\0\0\0"
        + UUID.encode()
        + bytes([7, want[0], want[0], want[1], want[1], want[2], want[2]])
    )
    try:
        sender.connect()
        sender.send(frame)
        assert oracle.receive(len(expected), 2) == expected
    finally:
        sender.close()


@pytest.mark.parametrize("dtype", ["f4", "f8", ">f8", np.longdouble])
@pytest.mark.parametrize(
    "levels,error",
    [
        ([256, 0, 0], ValueError),
        ([-1, 0, 0], ValueError),
        ([256, float("nan"), float("inf")], ValueError),
        ([256, float("inf"), float("nan")], OverflowError),
    ],
)
def test_invalid_numeric_input_leaves_session_usable(
    oracle: StrictOracle, dtype: object, levels: list[float], error: type[Exception]
) -> None:
    sender = make_sender(oracle.port)
    try:
        sender.connect()
        with pytest.raises(error):
            sender.send(np.array([levels], dtype=dtype))  # type: ignore[arg-type]
        oracle.expect_no_plaintext(0.03)
        assert sender.connected
        sender.send(bytes(3))
        assert len(oracle.receive(59, 2)) == 59
    finally:
        sender.close()


def test_uppercase_uuid_is_preserved(oracle: StrictOracle) -> None:
    from ledfx_senders.hue import HueSender

    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID.upper(),
        channel_ids=(7,),
    )
    try:
        sender.connect()
        sender.send(bytes(3))
        assert oracle.receive(59, 2)[16:52] == UUID.upper().encode()
    finally:
        sender.close()


def test_snapshot_owns_mutable_frame_before_session_lock(oracle: StrictOracle) -> None:
    sender = make_sender(oracle.port)
    sender.connect()
    frame = bytearray(b"\1\2\3")
    gate = _native._TestLockGate()
    failures: list[BaseException] = []

    def send() -> None:
        try:
            sender._engine._test_send_after_snapshot(frame, 0, gate)
        except BaseException as exc:  # noqa: BLE001 - propagate worker failures
            failures.append(exc)

    thread = Thread(target=send)
    thread.start()
    try:
        gate.wait_entered()
        frame[:] = b"\xff\xff\xff"
        sender.service()
    finally:
        gate.release()
        thread.join(3)
        sender.close()
    assert not thread.is_alive() and not gate.timed_out
    assert not failures, failures
    assert oracle.receive(59, 2)[52:] == b"\7\1\1\2\2\3\3"


def test_buffer_callbacks_can_reenter_service(oracle: StrictOracle) -> None:
    if __import__("sys").version_info < (3, 12):
        pytest.skip("Python buffer protocol hook requires 3.12+")
    sender = make_sender(oracle.port)
    sender.connect()
    calls: list[str] = []

    class Exporter:
        def __buffer__(self, flags: int) -> memoryview:
            sender.service()
            calls.append("acquire")
            return memoryview(b"\1\2\3")

        def __release_buffer__(self, view: memoryview) -> None:
            sender.service()
            calls.append("release")

    try:
        sender._engine.send(Exporter(), 0)
        assert oracle.receive(59, 2)[52:] == b"\7\1\1\2\2\3\3"
        assert calls == ["acquire", "release"]
    finally:
        sender.close()


if TYPE_CHECKING:
    from ledfx_senders.hue import HueSender


def test_service_skips_busy_engine_and_send_lock_wait_has_deadline() -> None:
    import time

    sender = make_sender()
    gate = _native._TestLockGate()
    failures: list[BaseException] = []

    def hold() -> None:
        try:
            sender._engine._test_hold_lock(gate)
        except BaseException as exc:  # noqa: BLE001 - propagate worker failures
            failures.append(exc)

    thread = Thread(target=hold)
    thread.start()
    try:
        gate.wait_entered()
        start = time.monotonic()
        sender.service()
        assert time.monotonic() - start < 0.1
        start = time.monotonic()
        with pytest.raises(TimeoutError):
            sender.send(bytes(3))
        assert 0.15 <= time.monotonic() - start < 1
        with pytest.raises(TimeoutError):
            sender.close()
        assert sender.closed
    finally:
        gate.release()
        thread.join(3)
        sender.close()
    assert not thread.is_alive() and not gate.timed_out
    assert not failures, failures


def test_close_interrupts_silent_connect_and_prevents_reconnect() -> None:
    import socket
    import time

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
        peer.bind(("127.0.0.1", 0))
        peer.settimeout(2)
        sender = make_sender(peer.getsockname()[1])
        failures: list[BaseException] = []

        def connect() -> None:
            try:
                sender.connect()
            except BaseException as exc:  # noqa: BLE001 - propagate worker failures
                failures.append(exc)

        thread = Thread(target=connect)
        thread.start()
        try:
            peer.recv(65535)
            start = time.monotonic()
            sender.close()
            assert time.monotonic() - start < 1
        finally:
            try:
                sender.close()
            finally:
                thread.join(3)
        assert not thread.is_alive()
        assert len(failures) == 1 and isinstance(failures[0], ConnectionError)
        assert sender.closed and not sender.connected
        with pytest.raises(ConnectionError):
            sender.connect()


def test_failed_connect_cannot_reconnect(oracle: StrictOracle) -> None:
    from ledfx_senders.hue import HueSender

    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=b"wrong",
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=(7,),
        connect_timeout=0.2,
    )
    try:
        with pytest.raises((ConnectionError, TimeoutError, OSError)):
            sender.connect()
        assert not sender.connected
        with pytest.raises(ConnectionError):
            sender.connect()
        oracle.expect_no_plaintext(0.03)
    finally:
        sender.close()


def test_all_256_unique_channels_are_representable(oracle: StrictOracle) -> None:
    import socket

    from ledfx_senders.encoders import encode_hue
    from ledfx_senders.hue import HueSender

    # The complete Hue packet encrypts to an 1881-byte UDP datagram. Some
    # test sandboxes drop fragmented loopback IP packets above 1500 bytes.
    # Preserve this network boundary on capable hosts; native Rust-only DTLS
    # exchange covers all 1844 plaintext bytes independently of host filtering.
    with (
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer,
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client,
    ):
        peer.bind(("127.0.0.1", 0))
        client.bind(("0.0.0.0", 0))
        client.connect(peer.getsockname())
        peer.connect(client.getsockname())
        peer.settimeout(0.2)
        assert client.send(bytes(1881)) == 1881
        try:
            assert len(peer.recv(16721)) == 1881
        except TimeoutError:
            pytest.skip(
                "host drops 1881-byte loopback UDP; native encrypted boundary remains covered"
            )
    ids = tuple(range(256))
    frame = bytes([1, 2, 3]) * 256
    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=ids,
    )
    try:
        sender.connect()
        sender.send(frame)
        packet = oracle.receive(1844, 2)
        assert packet == encode_hue(frame, UUID, ids, 0)
        assert packet[-7:] == b"\xff\1\1\2\2\3\3"
    finally:
        sender.close()


def test_connected_connect_is_a_noop_even_when_engine_is_busy(
    oracle: StrictOracle,
) -> None:
    from ledfx_senders.hue import HueSender

    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=(7,),
        connect_timeout=1.0,
    )
    sender.connect()
    gate = _native._TestLockGate()
    failures: list[BaseException] = []

    def hold() -> None:
        try:
            sender._engine._test_hold_lock(gate)
        except BaseException as exc:  # noqa: BLE001 - propagate worker failures
            failures.append(exc)

    thread = Thread(target=hold)
    thread.start()
    try:
        gate.wait_entered()
        sender.connect()
        assert sender.connected
    finally:
        gate.release()
        thread.join(3)
        sender.close()
    assert not thread.is_alive() and not gate.timed_out
    assert not failures, failures
