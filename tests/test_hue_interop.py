"""Independent authenticated DTLS gate; all credentials are dummy fixture data."""

import os
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest
from hue_support import FinalFlightDropRelay, StrictOracle, run_probe

IDENTITY = b"hue-fixture"
KEY = bytes(range(16))
PAYLOAD = (
    b"HueStream\x02\x00\x00\x00\x00\x00\x00"
    + b"12345678-1234-1234-1234-123456789abc"
    + b"\x07\x01\x01\x02\x02\xff\xff"
)


@pytest.fixture
def oracle() -> Iterator[StrictOracle]:
    executable = Path(os.environ["HUE_ORACLE"])
    with StrictOracle(executable, IDENTITY, KEY) as server:
        yield server


def test_probe_authenticates_and_delivers_plaintext(oracle: StrictOracle) -> None:
    result = run_probe("127.0.0.1", oracle.port, IDENTITY, KEY, PAYLOAD, 1000)
    assert result.returncode == 0, result.stderr
    assert oracle.receive(len(PAYLOAD), 2) == PAYLOAD
    assert oracle.identity == IDENTITY
    assert oracle.negotiated == ("DTLSv1.2", "PSK-AES128-GCM-SHA256")


def test_probe_wrong_identity_is_rejected(oracle: StrictOracle) -> None:
    result = run_probe("127.0.0.1", oracle.port, b"wrong", KEY, PAYLOAD, 300)
    assert result.returncode == 2
    assert result.stderr.strip() in {"configuration", "dtls", "io", "timeout", "closed"}
    oracle.expect_no_plaintext(timeout=0.2)


def test_probe_wrong_key_is_rejected(oracle: StrictOracle) -> None:
    result = run_probe("127.0.0.1", oracle.port, IDENTITY, b"\xff" * 16, PAYLOAD, 300)
    assert result.returncode == 2
    oracle.expect_no_plaintext(timeout=0.2)


def test_probe_requires_authenticated_server_finished(oracle: StrictOracle) -> None:
    with FinalFlightDropRelay(oracle.port) as relay:
        result = run_probe("127.0.0.1", relay.port, IDENTITY, KEY, PAYLOAD, 300)
        assert relay.dropped > 0, "route never delivered a server final flight"
        assert result.returncode == 2
        oracle.expect_no_plaintext(timeout=0.2)


def test_probe_ccm_only_is_rejected() -> None:
    with StrictOracle(
        Path(os.environ["HUE_ORACLE"]), IDENTITY, KEY, "PSK-AES128-CCM"
    ) as server:
        result = run_probe("127.0.0.1", server.port, IDENTITY, KEY, PAYLOAD, 300)
        assert result.returncode == 2
        server.expect_no_plaintext(timeout=0.2)


@pytest.mark.parametrize("identity, accepted", [(IDENTITY, True), (b"wrong", False)])
def test_oracle_enforces_identity(
    oracle: StrictOracle, identity: bytes, accepted: bool
) -> None:
    command = [
        "openssl",
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
        client.stdin.write(PAYLOAD)
        client.stdin.flush()
        if accepted:
            assert oracle.receive(len(PAYLOAD), 2) == PAYLOAD
            assert oracle.identity == IDENTITY
            assert oracle.negotiated == ("DTLSv1.2", "PSK-AES128-GCM-SHA256")
        else:
            oracle.expect_no_plaintext(timeout=0.3)
    finally:
        client.terminate()
        client.wait(timeout=2)
        if client.stdin is not None:
            client.stdin.close()
