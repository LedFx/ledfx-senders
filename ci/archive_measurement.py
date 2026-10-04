"""Freeze the actual measured artifacts before timing and verify afterwards."""

import argparse
import hashlib
import importlib
import json
import shutil
import subprocess
import sys
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main(verify):
    root = Path(__file__).resolve().parents[1]
    archive = root / "measurement-artifacts"
    manifest = archive / "manifest.json"
    if verify:
        records = json.loads(manifest.read_text())
        for path, expected in records["originals"].items():
            assert digest(Path(path)) == expected, path
        for path, expected in records["retained"].items():
            assert digest(archive / path) == expected, path
        for name, expected in json.loads(
            (archive / "source-hashes.json").read_text()
        ).items():
            assert digest(root / name) == expected, name
        return
    archive.mkdir(exist_ok=True)
    originals = {}

    def retain(path, name):
        path = path.resolve()
        target = archive / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        originals[str(path)] = digest(path)

    retain(Path(sys.executable), "interpreter/" + Path(sys.executable).name)
    for module in (
        "ledfx_senders._native",
        "ledfx_senders.encoders",
        "ledfx_senders.packet_senders",
        "ledfx_senders.original",
    ):
        loaded = importlib.import_module(module)
        retain(Path(loaded.__file__), "loaded/" + Path(loaded.__file__).name)
    for label in ("candidate", "control"):
        wheels = list((root / (label + "-wheels")).glob("*.whl"))
        assert len(wheels) == 1, wheels
        retain(wheels[0], label + "/" + wheels[0].name)
    binaries = [
        p
        for p in (root / "native/target/release/deps").glob("_native-*")
        if p.suffix in ("", ".exe")
    ]
    assert binaries, "missing measured Rust test executable"
    for binary in binaries:
        retain(binary, "rust-tests/" + binary.name)
    suffix = ".exe" if sys.platform == "win32" else ""
    retain(
        root / "native/target/release" / ("ledfx-receiver" + suffix),
        "receiver/ledfx-receiver" + suffix,
    )
    source_hashes = {}
    for name in subprocess.check_output(
        ["git", "ls-files"], cwd=root, text=True
    ).splitlines():
        path = root / name
        if path.is_file():
            source_hashes[name] = digest(path)
    (archive / "source-hashes.json").write_text(
        json.dumps(source_hashes, indent=2) + "\n"
    )
    subprocess.run(
        [
            "git",
            "archive",
            "--format=tar.gz",
            "-o",
            str(archive / "candidate-source.tar.gz"),
            "HEAD",
        ],
        cwd=root,
        check=True,
    )
    subprocess.run(
        [
            "git",
            "archive",
            "--format=tar.gz",
            "-o",
            str(archive / "control-source.tar.gz"),
            "HEAD",
        ],
        cwd=root / ".benchmark-control-source",
        check=True,
    )
    manifest.write_text(
        json.dumps(
            {
                "candidate_commit": subprocess.check_output(
                    ["git", "rev-parse", "HEAD"], cwd=root, text=True
                ).strip(),
                "control_commit": subprocess.check_output(
                    ["git", "rev-parse", "HEAD"],
                    cwd=root / ".benchmark-control-source",
                    text=True,
                ).strip(),
                "originals": originals,
                "retained": {
                    str(p.relative_to(archive)): digest(p)
                    for p in archive.rglob("*")
                    if p.is_file()
                },
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", action="store_true")
    main(parser.parse_args().verify)
