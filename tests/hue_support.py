"""Strict external test oracle and private native probe driver. Dummy secrets only."""

import selectors
import socket
import subprocess
import tempfile
import time
from pathlib import Path
from threading import Event, Lock, Thread
from types import TracebackType
from typing import Self

ROOT = Path(__file__).resolve().parents[1]


class FinalFlightDropRelay:
    """Forward the oracle's records, dropping encrypted server handshake records.

    Reads only DTLS record framing; it never creates records or performs crypto.
    """

    def __init__(self, oracle_port: int) -> None:
        self._downstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._downstream.bind(("127.0.0.1", 0))
        self.port = self._downstream.getsockname()[1]
        self._upstream.bind(("127.0.0.1", 0))
        self._upstream.connect(("127.0.0.1", oracle_port))
        self._stop = Event()
        self._lock = Lock()
        self._dropped = 0
        self._thread = Thread(target=self._forward, name="hue-test-relay")
        self._thread.start()

    @property
    def dropped(self) -> int:
        with self._lock:
            return self._dropped

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
                    self._dropped += 1
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
            ],
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
        if self._process.stdout is not None:
            self._process.stdout.close()
        self._temporary.cleanup()
