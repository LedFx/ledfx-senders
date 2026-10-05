"""Public owned Hue sender; strict independent oracle uses dummy credentials."""

import subprocess
from collections.abc import Iterator
from threading import Thread
from typing import TYPE_CHECKING

import numpy as np
import pytest
from hue_support import (
    StrictOracle,
    lifecycle_watchdog,
    openssl_executable,
    oracle_executable,
    socket_handles,
)

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
    with StrictOracle(oracle_executable(), IDENTITY, KEY) as peer:
        yield peer


@pytest.mark.hue_oracle
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


@pytest.mark.hue_oracle
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
    oracle: StrictOracle,
    frame: Frame,
    want: bytes,
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


@pytest.mark.hue_oracle
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
    oracle: StrictOracle,
    dtype: object,
    levels: list[float],
    error: type[Exception],
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


@pytest.mark.hue_oracle
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


@pytest.mark.hue_oracle
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


@pytest.mark.hue_oracle
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


def test_service_skips_busy_engine_and_close_cancels_waiting_send() -> None:
    lifecycle_watchdog("busy")


def test_busy_send_lock_wait_keeps_absolute_deadline() -> None:
    lifecycle_watchdog("deadline")


def test_close_interrupts_silent_connect_and_prevents_reconnect() -> None:
    lifecycle_watchdog("cancel")


def test_silent_connect_has_bounded_timeout_and_releases_resources() -> None:
    lifecycle_watchdog("silent")


@pytest.mark.hue_oracle
def test_failed_connect_cannot_reconnect(oracle: StrictOracle) -> None:
    lifecycle_watchdog("failed", oracle.port)
    oracle.expect_no_plaintext(0.03)


@pytest.mark.hue_oracle
def test_simultaneous_send_service_close_releases_connected_owner(
    oracle: StrictOracle,
) -> None:
    lifecycle_watchdog("simultaneous", oracle.port)
    oracle.expect_no_plaintext(0.03)


@pytest.mark.hue_oracle
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


@pytest.mark.hue_oracle
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


@pytest.mark.hue_oracle
@pytest.mark.parametrize("operation", ["send", "close"])
def test_expired_idle_operation_disposes_client_and_socket(
    oracle: StrictOracle, operation: str
) -> None:
    from ledfx_senders.hue import HueSender

    before = socket_handles()
    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=(7,),
        send_timeout=1e-9,
        close_timeout=1e-9,
    )
    sender.connect()
    connected = socket_handles()
    owned = None if before is None or connected is None else connected - before
    if owned is not None:
        assert len(owned) == 1
    try:
        with pytest.raises(TimeoutError):
            if operation == "send":
                sender.send(bytes(3))
            else:
                sender.close()
        assert not sender.connected
        remaining = socket_handles()
        if owned is not None and remaining is not None:
            assert owned.isdisjoint(remaining), (
                "terminal idle operation retained UDP socket"
            )
        assert not sender._engine._test_has_client()
        oracle.expect_no_plaintext(0.03)
    finally:
        try:
            sender.close()
        except TimeoutError:
            pass  # This tiny cleanup budget may expire; resources must already be gone.


@pytest.mark.hue_oracle
def test_snapshot_expiry_disposes_idle_client_before_return(
    oracle: StrictOracle,
) -> None:
    from threading import Event

    from ledfx_senders.hue import HueSender

    before = socket_handles()
    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=(7,),
        send_timeout=0.01,
    )
    sender.connect()
    connected = socket_handles()
    owned = None if before is None or connected is None else connected - before
    gate = _native._TestLockGate()
    failures: list[BaseException] = []

    def send() -> None:
        try:
            sender._engine._test_send_after_snapshot(bytes(3), 0, gate)
        except BaseException as exc:  # noqa: BLE001 - propagate worker failures
            failures.append(exc)

    thread = Thread(target=send)
    thread.start()
    try:
        gate.wait_entered()
        # The gate is after snapshot and after the absolute budget starts.
        # Waiting longer than that budget deterministically expires it.
        Event().wait(0.02)
        gate.release()
        thread.join(3)
        assert not thread.is_alive() and not gate.timed_out
        assert len(failures) == 1 and isinstance(failures[0], TimeoutError), failures
        remaining = socket_handles()
        if owned is not None and remaining is not None:
            assert len(owned) == 1 and owned.isdisjoint(remaining)
        assert not sender._engine._test_has_client()
        assert not sender.connected
        oracle.expect_no_plaintext(0.03)
    finally:
        gate.release()
        thread.join(3)
        sender.close()


@pytest.mark.hue_oracle
def test_close_cancellation_after_successful_finish_disposes_owner_resources(
    oracle: StrictOracle,
) -> None:
    from ledfx_senders.hue import HueSender

    before = socket_handles()
    sender = HueSender(
        destination="127.0.0.1",
        port=oracle.port,
        psk_identity=IDENTITY,
        client_key=KEY,
        entertainment_id=UUID,
        channel_ids=(7,),
        close_timeout=1e-9,
    )
    sender.connect()
    connected = socket_handles()
    owned = None if before is None or connected is None else connected - before
    gate = _native._TestLockGate()
    failures: list[BaseException] = []

    def hold_finished_owner() -> None:
        try:
            sender._engine._test_hold_after_finish(gate)
        except BaseException as exc:  # noqa: BLE001 - propagate worker failures
            failures.append(exc)

    thread = Thread(target=hold_finished_owner)
    thread.start()
    try:
        gate.wait_entered()  # Native owner passed finish successfully and still holds the mutex.
        with pytest.raises(TimeoutError):
            sender.close()
        assert sender.closed and not sender.connected
    finally:
        gate.release()
        thread.join(3)
    assert not thread.is_alive() and not gate.timed_out
    assert not failures, failures
    remaining = socket_handles()
    if owned is not None and remaining is not None:
        assert len(owned) == 1 and owned.isdisjoint(remaining)
    assert not sender._engine._test_has_client()
    oracle.expect_no_plaintext(0.03)


@pytest.mark.hue_oracle
@pytest.mark.parametrize("sequence", [0, 255])
def test_ipv6_authenticated_literal_frames(sequence: int) -> None:
    import socket

    from ledfx_senders.hue import HueSender

    try:
        with socket.socket(socket.AF_INET6, socket.SOCK_DGRAM) as capability:
            capability.bind(("::1", 0))
    except OSError as exc:
        pytest.skip(f"IPv6 loopback unavailable: {exc}")
    with StrictOracle(oracle_executable(), IDENTITY, KEY, ipv6=True) as peer:
        sender = HueSender(
            destination="::1",
            port=peer.port,
            psk_identity=IDENTITY,
            client_key=KEY,
            entertainment_id=UUID,
            channel_ids=(7,),
            sequence=sequence,
        )
        expected = (
            b"HueStream\2\0"
            + bytes([sequence])
            + bytes(4)
            + UUID.encode()
            + b"\7\1\1\2\2\3\3"
        )
        try:
            sender.connect()
            sender.send(b"\1\2\3")
            sender.send(b"\1\2\3")
            assert peer.receive(len(expected) * 2, 2) == expected * 2
            assert peer.negotiated == ("DTLSv1.2", "PSK-AES128-GCM-SHA256")
        finally:
            sender.close()


@pytest.mark.hue_oracle
def test_idle_peer_close_notify_service_disposes_and_send_fails() -> None:
    import time

    with StrictOracle(oracle_executable(), IDENTITY, KEY) as peer:
        sender = make_sender(peer.port)
        sender.connect()
        peer.command("close")
        deadline = time.monotonic() + 1
        try:
            while sender.connected:
                try:
                    sender.service()
                except ConnectionError:
                    break
                assert time.monotonic() < deadline, "idle peer alert was not detected"
                time.sleep(0.005)
            assert not sender.connected
            assert not sender._engine._test_has_client()
            with pytest.raises(ConnectionError):
                sender.send(bytes(3))
            with pytest.raises(ConnectionError):
                sender.connect()
            peer.expect_no_plaintext(0.03)
        finally:
            sender.close()


@pytest.mark.hue_oracle
def test_authenticated_silence_does_not_imply_udp_peer_death(
    oracle: StrictOracle,
) -> None:
    oracle_sender = make_sender(oracle.port)
    oracle_sender.connect()
    try:
        oracle.command("silence")
        assert oracle.authenticated
        for _ in range(3):
            oracle_sender.service()
            assert oracle_sender.connected
        oracle_sender.send(bytes(3))
        assert oracle_sender.connected
        oracle.expect_no_plaintext(0.03)
    finally:
        oracle_sender.close()


@pytest.mark.hue_oracle
@pytest.mark.parametrize("identity, accepted", [(IDENTITY, True), (b"wrong", False)])
def test_oracle_enforces_identity(
    oracle: StrictOracle,
    identity: bytes,
    accepted: bool,
) -> None:
    command = [
        openssl_executable(),
        "s_client",
        "-dtls1_2",
        "-connect",
        f"127.0.0.1:{oracle.port}",
        "-cipher",
        "PSK-AES128-GCM-SHA256",
        "-psk_identity",
        identity.decode(),
        "-psk",
        KEY.hex(),
        "-quiet",
        "-ign_eof",
    ]
    client = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        assert client.stdin is not None
        client.stdin.write(b"strict-oracle-identity-check")
        client.stdin.flush()
        if accepted:
            assert oracle.receive(28, 2) == b"strict-oracle-identity-check"
            assert oracle.identity == IDENTITY
            assert oracle.negotiated == ("DTLSv1.2", "PSK-AES128-GCM-SHA256")
        else:
            oracle.expect_no_plaintext(timeout=0.3)
    finally:
        client.terminate()
        client.wait(timeout=2)
        if client.stdin is not None:
            client.stdin.close()


@pytest.mark.hue_oracle
@pytest.mark.parametrize("rejection", ["identity", "key", "suite"])
def test_public_sender_rejects_independent_peer(rejection: str) -> None:
    from ledfx_senders import HueSender

    with StrictOracle(
        oracle_executable(),
        IDENTITY,
        KEY,
        "PSK-AES128-CCM" if rejection == "suite" else "PSK-AES128-GCM-SHA256",
    ) as peer:
        sender = HueSender(
            destination="127.0.0.1",
            port=peer.port,
            psk_identity=b"wrong" if rejection == "identity" else IDENTITY,
            client_key=b"\xff" * 16 if rejection == "key" else KEY,
            entertainment_id=UUID,
            channel_ids=(7,),
            connect_timeout=0.3,
        )
        try:
            with pytest.raises((ConnectionError, TimeoutError)):
                sender.connect()
            assert not sender.connected
            peer.expect_no_plaintext(0.03)
            assert not peer.authenticated
        finally:
            sender.close()
