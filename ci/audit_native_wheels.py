"""Audit repaired distributions without importing or executing their extension."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FORBIDDEN = re.compile(
    r"(?:lib)?(?:ssl|crypto)[\w.-]*(?:\.so|\.dylib|\.dll)|mbedtls|mbedcrypto|mbedx509|aws[-_]lc|oracle|openssl_psk_server",
    re.IGNORECASE,
)


def platform_tool() -> str:
    name = (
        "dumpbin"
        if sys.platform == "win32"
        else "delocate-listdeps"
        if sys.platform == "darwin"
        else "auditwheel"
    )
    executable = shutil.which(name)
    if executable is None and sys.platform == "win32":
        vswhere = (
            Path(os.environ.get("ProgramFiles(x86)", ""))
            / "Microsoft Visual Studio/Installer/vswhere.exe"
        )
        if vswhere.is_file():
            installation = subprocess.check_output(
                [
                    str(vswhere),
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                    "-property",
                    "installationPath",
                ],
                text=True,
                timeout=20,
            ).strip()
            matches = sorted(
                Path(installation).glob("VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe")
            )
            if matches:
                executable = str(matches[-1])
    if executable is None:
        raise RuntimeError(f"required platform audit tool missing: {name}")
    return executable


def audit(wheel: Path) -> dict[str, str]:
    with zipfile.ZipFile(wheel) as archive:
        members = archive.namelist()
        for member in members:
            if FORBIDDEN.search(Path(member).name):
                raise RuntimeError(f"forbidden wheel member: {member}")
        native = [
            member
            for member in members
            if Path(member).name.startswith("_native.")
            and member.endswith((".so", ".pyd"))
        ]
        if len(native) != 1:
            raise RuntimeError("wheel must contain exactly one native extension")
        tool = platform_tool()
        with tempfile.TemporaryDirectory(prefix="sender-wheel-audit-") as directory:
            if sys.platform == "win32":
                extension = Path(directory) / Path(native[0]).name
                extension.write_bytes(archive.read(native[0]))
                command = [tool, "/DEPENDENTS", str(extension)]
            elif sys.platform == "darwin":
                command = [tool, str(wheel)]
            else:
                command = [tool, "show", str(wheel)]
            result = subprocess.run(
                command, capture_output=True, text=True, check=False, timeout=30
            )
            if result.returncode:
                raise RuntimeError(result.stdout + result.stderr)
            if FORBIDDEN.search(result.stdout + result.stderr):
                raise RuntimeError(
                    "forbidden native runtime linkage: " + result.stdout + result.stderr
                )
    print(result.stdout)
    return {
        "wheel": wheel.name,
        "sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
        "audit": tool,
    }


def main() -> None:
    directory = Path(sys.argv[1])
    wheels = sorted(directory.glob("*.whl"))
    if not wheels:
        raise RuntimeError("no repaired wheels to audit")
    records = [audit(wheel) for wheel in wheels]
    source_sha = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, timeout=10
    ).strip()
    expected = os.environ.get("SOURCE_SHA")
    if expected is not None and expected != source_sha:
        raise RuntimeError("wheel checkout SHA differs from requested source SHA")
    metadata = {"source_sha": source_sha, "wheels": records}
    (directory / "native-wheel-audit.json").write_text(
        json.dumps(metadata, indent=2) + "\n"
    )
    print(json.dumps(metadata))


if __name__ == "__main__":
    main()
