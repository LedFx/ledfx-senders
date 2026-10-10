"""The source archive must independently build the Hue extension and its gates."""

import os
import subprocess
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_sdist_contains_hue_native_api_typing_and_test_tooling(tmp_path: Path) -> None:
    supplied = os.environ.get("LEDFX_SDIST")
    if supplied is None:
        subprocess.run(
            ["uv", "build", "--sdist", "--out-dir", str(tmp_path)],
            cwd=ROOT,
            check=True,
            timeout=60,
        )
        archive = next(tmp_path.glob("*.tar.gz"))
    else:
        archive = Path(supplied)
    with tarfile.open(archive) as source:
        members = {name.partition("/")[2] for name in source.getnames()}
        for required in (
            "src/ledfx_senders/hue.py",
            "src/ledfx_senders/_native.pyi",
            "src/ledfx_senders/py.typed",
            "native/Cargo.toml",
            "native/Cargo.lock",
            "native/src/hue/mod.rs",
            "native/src/hue/session.rs",
            "tests/fixtures/hue/openssl_psk_server.c",
            "tests/hue_support.py",
            "ci/build_hue_oracle.py",
            "ci/hue_typing_probe.py",
            "ci/hue_wheel_smoke.py",
        ):
            assert required in members, required
