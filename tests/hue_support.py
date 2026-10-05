"""Strict external test oracle and private native probe driver. Dummy secrets only."""

import faulthandler
import selectors
import socket
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Callable
from pathlib import Path
from threading import Event, Lock, Thread
from types import TracebackType
from typing import Self

ROOT = Path(__file__).resolve().parents[1]


class FinalFlightDropRelay:
    """Forward the oracle's records, dropping or corrupting encrypted server handshake records.

    Reads only DTLS record framing; it never creates records or performs crypto.
    """

    def __init__(self, oracle_port: int, *, corrupt: bool = False) -> None:
        self._corrupt = corrupt
        self._downstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._downstream.bind(("127.0.0.1", 0))
        self.port = self._downstream.getsockname()[1]
        self._upstream.bind(("127.0.0.1", 0))
        self._upstream.connect(("127.0.0.1", oracle_port))
        self._stop = Event()
        self._lock = Lock()
        self._affected = 0
        self._thread = Thread(target=self._forward, name="hue-test-relay")
        self._thread.start()

    @property
    def dropped(self) -> int:
        with self._lock:
            return 0 if self._corrupt else self._affected

    @property
    def corrupted(self) -> int:
        with self._lock:
            return self._affected if self._corrupt else 0

    def _filter(self, datagram: bytes) -> bytes:
        offset = 0
        forwarded = bytearray()
        while offset < len(datagram):
            if len(datagram) - offset < 13:
                return datagram
            end = (
                offset + 13 + int.from_bytes(datagram[offset + 11 : offset + 13], "big")
            )
            if end > len(datagram):
                return datagram
            epoch = int.from_bytes(datagram[offset + 3 : offset + 5], "big")
            if datagram[offset] == 22 and epoch > 0:
                with self._lock:
                    self._affected += 1
                if self._corrupt:
                    record = bytearray(datagram[offset:end])
                    record[-1] ^= 1  # Damage authentication tag; retain entire record.
                    forwarded.extend(record)
            else:
                forwarded.extend(datagram[offset:end])
            offset = end
        return bytes(forwarded)

    def _forward(self) -> None:
        peer: tuple[str, int] | None = None
        with selectors.DefaultSelector() as selector:
            selector.register(self._downstream, selectors.EVENT_READ)
            selector.register(self._upstream, selectors.EVENT_READ)
            while not self._stop.is_set():
                for key, _ in selector.select(timeout=0.02):
                    if key.fileobj is self._downstream:
                        datagram, peer = self._downstream.recvfrom(65536)
                        self._upstream.send(datagram)
                    else:
                        datagram = self._filter(self._upstream.recv(65536))
                        if datagram and peer is not None:
                            self._downstream.sendto(datagram, peer)

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()

    def close(self) -> None:
        self._stop.set()
        self._thread.join(timeout=2)
        self._downstream.close()
        self._upstream.close()
        assert not self._thread.is_alive(), "test relay failed to stop"


class DatagramFaultRelay:
    """Test-only UDP proxy; always forwards each retained datagram unchanged.

    Hold the first retained client datagram until the next datagram arrives,
    then forward the pair in reverse. For a silent handshake this exercises
    retransmission and replay handling without splitting coalesced records.
    Unrelated noise uses a separate source port, exercising connected UDP filtering.
    """

    def __init__(self, oracle_port: int, fault: str) -> None:
        assert fault in {"drop", "reorder", "noise", "all"}
        self._fault = fault
        self._downstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._stranger = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._downstream.bind(("127.0.0.1", 0))
        self.port = self._downstream.getsockname()[1]
        self._upstream.bind(("127.0.0.1", 0))
        self._upstream.connect(("127.0.0.1", oracle_port))
        self._stop = Event()
        self._lock = Lock()
        self._dropped = 0
        self._reordered = 0
        self._noise = 0
        self._thread = Thread(target=self._forward, name="hue-datagram-fault-relay")
        self._thread.start()

    @property
    def dropped(self) -> int:
        with self._lock:
            return self._dropped

    @property
    def reordered(self) -> int:
        with self._lock:
            return self._reordered

    @property
    def noise(self) -> int:
        with self._lock:
            return self._noise

    def _forward(self) -> None:
        peer: tuple[str, int] | None = None
        held: bytes | None = None
        with selectors.DefaultSelector() as selector:
            selector.register(self._downstream, selectors.EVENT_READ)
            selector.register(self._upstream, selectors.EVENT_READ)
            while not self._stop.is_set():
                for key, _ in selector.select(timeout=0.005):
                    if key.fileobj is self._downstream:
                        datagram, peer = self._downstream.recvfrom(65535)
                        if self._fault in {"drop", "all"} and self.dropped == 0:
                            with self._lock:
                                self._dropped += 1
                            continue
                        if held is not None:
                            self._upstream.send(datagram)
                            self._upstream.send(held)
                            held = None
                            with self._lock:
                                self._reordered += 2
                        elif self._fault in {"reorder", "all"} and self.reordered == 0:
                            held = datagram
                        else:
                            self._upstream.send(datagram)
                    else:
                        datagram = self._upstream.recv(65535)
                        if peer is not None:
                            self._downstream.sendto(datagram, peer)
                if peer is not None and self._fault in {"noise", "all"}:
                    self._stranger.sendto(b"unrelated-peer noise", peer)
                    with self._lock:
                        self._noise += 1

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()

    def close(self) -> None:
        self._stop.set()
        self._thread.join(timeout=2)
        self._downstream.close()
        self._upstream.close()
        self._stranger.close()
        assert not self._thread.is_alive(), "test fault relay failed to stop"


def run_probe(
    destination: str,
    port: int,
    identity: bytes,
    key: bytes,
    payload: bytes,
    timeout_ms: int,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--manifest-path",
            str(ROOT / "native/Cargo.toml"),
            "--locked",
            "--example",
            "hue-probe",
            "--",
            destination,
            str(port),
            identity.hex(),
            key.hex(),
            payload.hex(),
            str(timeout_ms),
        ],
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )


class StrictOracle:
    """One peer/association; logs, accepted metadata and plaintext stay separate."""

    def __init__(
        self,
        executable: Path,
        identity: bytes,
        key: bytes,
        cipher: str = "PSK-AES128-GCM-SHA256",
        *,
        ems: bool = True,
        ipv6: bool = False,
        certificate_only: bool = False,
    ) -> None:
        self._temporary = tempfile.TemporaryDirectory(prefix="hue-oracle-")
        directory = Path(self._temporary.name)
        self._output = directory / "plaintext"
        self._metadata = directory / "metadata"
        self._process = subprocess.Popen(
            [
                str(executable),
                identity.hex(),
                key.hex(),
                cipher,
                str(self._output),
                str(self._metadata),
                "ems" if ems else "non-ems",
                "6" if ipv6 else "4",
                "certificate" if certificate_only else "psk",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        try:
            assert self._process.stdout is not None
            with selectors.DefaultSelector() as selector:
                selector.register(self._process.stdout, selectors.EVENT_READ)
                if not selector.select(timeout=2):
                    raise AssertionError("oracle did not become ready")
                line = self._process.stdout.readline().decode("ascii")
            assert line.startswith("READY "), "oracle startup failed"
            self.port = int(line.split()[1])
        except BaseException:
            self.close()
            raise

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self.close()

    def receive(self, size: int, timeout: float) -> bytes:
        assert size > 0
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = self._output.read_bytes()
            if len(result) >= size:
                return result
            time.sleep(0.01)
        raise AssertionError("oracle did not receive expected plaintext")

    def command(self, command: str) -> None:
        assert command in {"close", "silence"}
        assert self._process.stdin is not None
        self._process.stdin.write((command + "\n").encode("ascii"))
        self._process.stdin.flush()
        # Fixture acknowledges only after executing the requested operation.
        assert self._process.stdout is not None
        with selectors.DefaultSelector() as selector:
            selector.register(self._process.stdout, selectors.EVENT_READ)
            assert selector.select(timeout=2), "oracle control was not acknowledged"
            assert (
                self._process.stdout.readline() == ("DONE " + command + "\n").encode()
            )

    @property
    def authenticated(self) -> bool:
        lines = self._metadata.read_bytes().splitlines()
        return (
            len(lines) >= 4 and lines[1] == b"DTLSv1.2" and lines[3].startswith(b"EMS ")
        )

    @property
    def extended_master_secret(self) -> bool:
        return self._metadata.read_text().splitlines()[3] == "EMS 1"

    @property
    def fatal_alert(self) -> int | None:
        for line in self._metadata.read_text().splitlines():
            if line.startswith("FATAL "):
                return int(line.split()[1])
        return None

    @property
    def identity(self) -> bytes:
        return self._metadata.read_bytes().splitlines()[0]

    @property
    def negotiated(self) -> tuple[str, str]:
        lines = self._metadata.read_text().splitlines()
        return lines[1], lines[2]

    def expect_no_plaintext(self, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            assert self._output.stat().st_size == 0, (
                "unexpected decrypted application data"
            )
            time.sleep(min(0.01, max(0, deadline - time.monotonic())))
        assert self._output.stat().st_size == 0, "unexpected decrypted application data"

    def close(self) -> None:
        if self._process.poll() is None:
            self._process.terminate()
            try:
                self._process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait(timeout=2)
        if self._process.stdin is not None:
            self._process.stdin.close()
        if self._process.stdout is not None:
            self._process.stdout.close()
        self._temporary.cleanup()


def socket_handles() -> set[str] | None:
    directory = Path("/proc/self/fd")
    if not directory.is_dir():
        return None
    handles: set[str] = set()
    for entry in directory.iterdir():
        try:
            target = str(entry.readlink())
        except FileNotFoundError:
            continue
        if target.startswith("socket:["):
            handles.add(target)
    return handles


def lifecycle_watchdog(case: str, port: int = 2100) -> None:
    """Isolate real installed Hue operations; detect native hangs and process crashes."""
    assert case in {"silent", "cancel", "busy", "simultaneous", "failed", "deadline"}
    result = subprocess.run(
        [sys.executable, "-I", str(Path(__file__).resolve()), case, str(port)],
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    assert result.returncode == 0, result.stdout + result.stderr


def _lifecycle_scenario(case: str, port: int) -> None:
    from ledfx_senders import HueSender, _native

    baseline_threads = set(threading.enumerate())
    baseline_sockets = socket_handles()
    peer = None
    if case in {"silent", "cancel"}:
        peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        peer.bind(("127.0.0.1", 0))
        peer.settimeout(2)
        port = peer.getsockname()[1]
    sender = HueSender(
        destination="127.0.0.1",
        port=port,
        psk_identity=b"wrong" if case == "failed" else b"hue-fixture",
        client_key=bytes(range(16)),
        entertainment_id="12345678-1234-1234-1234-123456789abc",
        channel_ids=(7,),
        connect_timeout=0.2 if case in {"silent", "failed"} else 5,
    )
    failures: list[BaseException] = []

    def invoke(operation: Callable[[], None]) -> None:
        try:
            operation()
        except BaseException as exc:  # noqa: BLE001 - propagate native worker failures
            failures.append(exc)

    if case in {"silent", "cancel"}:
        worker = threading.Thread(target=invoke, args=(sender.connect,))
        worker.start()
        assert peer is not None
        peer.recv(65535)  # Prove native handshake/socket ownership before cancellation.
        if case == "cancel":
            start = time.monotonic()
            sender.close()
            assert time.monotonic() - start < 0.5
        worker.join(0.5)
        assert not worker.is_alive(), "native connect did not finish"
        assert len(failures) == 1, failures
        assert isinstance(
            failures[0], ConnectionError if case == "cancel" else TimeoutError
        ), failures
        assert not sender.connected
        assert not sender._engine._test_has_client()
    elif case == "failed":
        invoke(sender.connect)
        assert len(failures) == 1 and isinstance(
            failures[0], (ConnectionError, TimeoutError)
        ), failures
        assert not sender.connected and not sender._engine._test_has_client()
    elif case == "deadline":
        gate = _native._TestLockGate()
        holder = threading.Thread(
            target=invoke, args=(lambda: sender._engine._test_hold_lock(gate),)
        )
        holder.start()
        worker = threading.Thread(target=invoke, args=(lambda: sender.send(bytes(3)),))
        try:
            gate.wait_entered()
            worker.start()
            worker.join(0.5)
            assert not worker.is_alive(), "busy lock renewed the 200 ms send budget"
            assert len(failures) == 1 and isinstance(failures[0], TimeoutError), (
                failures
            )
            assert not sender.connected
        finally:
            gate.release()
            holder.join(0.5)
            if worker.ident is not None:
                worker.join(0.5)
        assert not holder.is_alive() and not gate.timed_out
        assert not sender._engine._test_has_client()
    else:
        if case == "simultaneous":
            sender.connect()
            assert sender.connected
        gate = _native._TestLockGate()
        holder = threading.Thread(
            target=invoke, args=(lambda: sender._engine._test_hold_lock(gate),)
        )
        holder.start()
        gate.wait_entered()
        # Check the busy skip before cancellation can mask the try-lock contract.
        service_probe = threading.Thread(target=invoke, args=(sender.service,))
        service_probe.start()
        service_probe.join(0.5)
        assert not service_probe.is_alive(), "service waited for busy engine"
        assert not failures, failures
        send_gate = _native._TestLockGate()
        worker = threading.Thread(
            target=invoke,
            args=(
                lambda: sender._engine._test_send_after_snapshot(
                    bytes(3), 0, send_gate
                ),
            ),
        )
        worker.start()
        try:
            send_gate.wait_entered()
            send_gate.release()
            # With the engine held, overlap service and close with waiting send.
            barrier = threading.Barrier(3)

            def racing(operation: Callable[[], None]) -> None:
                barrier.wait(timeout=2)
                invoke(operation)

            service = threading.Thread(target=racing, args=(sender.service,))
            closer = threading.Thread(target=racing, args=(sender.close,))
            service.start()
            closer.start()
            start = time.monotonic()
            barrier.wait(timeout=2)
            service.join(0.5)
            assert not service.is_alive(), "service waited for busy engine"
            closer.join(0.5)
            assert not closer.is_alive(), "close exceeded native budget"
            assert time.monotonic() - start < 0.5
            assert sender.closed and not sender.connected
            worker.join(0.5)
            assert not worker.is_alive(), "close failed to cancel waiting send"
        finally:
            send_gate.release()
            gate.release()
            holder.join(0.5)
            worker.join(0.5)
        assert not holder.is_alive() and not gate.timed_out and not send_gate.timed_out
        assert failures and all(
            isinstance(exc, (ConnectionError, TimeoutError)) for exc in failures
        ), failures
        assert not sender._engine._test_has_client()

    if case in {"failed", "silent"}:
        # Failure itself must forbid reuse, before explicit close can mask it.
        try:
            sender.connect()
        except ConnectionError:
            pass
        else:
            raise AssertionError("failed connect allowed session reuse before close")
        assert not sender.connected and not sender._engine._test_has_client()
    if peer is not None:
        peer.close()
    sender.close()
    sender.close()
    assert sender.closed and not sender.connected
    for operation in (sender.connect, lambda: sender.send(bytes(3)), sender.service):
        try:
            operation()
        except ConnectionError:
            pass
        else:
            raise AssertionError("terminal sender accepted reconnect/send")
    assert set(threading.enumerate()) == baseline_threads, "worker thread leaked"
    remaining = socket_handles()
    if baseline_sockets is not None and remaining is not None:
        assert remaining == baseline_sockets, "UDP socket leaked"
    print(
        case,
        "closed, disconnected, workers joined; socket handles checked:",
        baseline_sockets is not None,
    )


if __name__ == "__main__":
    faulthandler.dump_traceback_later(10, exit=True)
    _lifecycle_scenario(sys.argv[1], int(sys.argv[2]))
    faulthandler.cancel_dump_traceback_later()
