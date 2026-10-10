"""Distribution audit must reject test or alternate-crypto payloads before repair."""

import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


@pytest.mark.parametrize(
    "member",
    [
        "ledfx_senders/hue-oracle.exe",
        "tests/fixtures/hue/openssl_psk_server.c",
        "ledfx_senders/libssl.so.3",
        "ledfx_senders/libcrypto-3-x64.dll",
        "ledfx_senders/libmbedcrypto.dylib",
        "ledfx_senders/aws-lc.dll",
    ],
)
def test_audit_rejects_forbidden_wheel_payload(tmp_path: Path, member: str) -> None:
    wheel = tmp_path / "example.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr(member, b"test")
    result = subprocess.run(
        [sys.executable, str(ROOT / "ci/audit_native_wheels.py"), str(tmp_path)],
        capture_output=True,
        text=True,
        check=False,
        timeout=15,
    )
    assert result.returncode != 0
    assert "forbidden wheel member" in result.stderr


def test_audit_missing_platform_tool_fails(tmp_path: Path) -> None:
    wheel = tmp_path / "example.whl"
    with zipfile.ZipFile(wheel, "w") as archive:
        archive.writestr("ledfx_senders/_native.test.so", b"not loaded")
    result = subprocess.run(
        [sys.executable, str(ROOT / "ci/audit_native_wheels.py"), str(tmp_path)],
        env={"PATH": ""},
        capture_output=True,
        text=True,
        check=False,
        timeout=15,
    )
    assert result.returncode != 0
    assert "required platform audit tool missing" in result.stderr
