"""Check synchronized package versions before any build or release decision."""

import ast
import json
import os
import re
import subprocess
import tomllib
from collections.abc import Iterable
from pathlib import Path
from typing import cast

# Every path release-please or a dependency lock bump may rewrite. A change
# set entirely inside these files cannot alter built artifacts, so the heavy
# native jobs skip it on pull_request and main pushes. Tags always build.
RELEASE_METADATA = frozenset(
    {
        "CHANGELOG.md",
        ".release-please-manifest.json",
        "pyproject.toml",
        "src/ledfx_senders/__init__.py",
        "uv.lock",
        "native/Cargo.toml",
        "native/Cargo.lock",
    }
)


def table(value: object) -> dict[str, object]:
    if not isinstance(value, dict) or not all(isinstance(key, str) for key in value):
        raise TypeError("Expected a table with string keys")
    return cast(dict[str, object], value)


def package_version(value: object) -> str:
    version = table(value)["version"]
    if not isinstance(version, str):
        raise TypeError("Expected a string package version")
    return version


def release_plan(root: Path, event: str, repository: str, ref: str) -> tuple[str, bool]:
    def toml(name: str) -> dict[str, object]:
        return tomllib.loads((root / name).read_text())

    version = package_version(toml("pyproject.toml")["project"])
    if not isinstance(version, str) or not re.fullmatch(
        r"[0-9][0-9A-Za-z.+-]*", version
    ):
        raise ValueError("Invalid project version")
    versions = {
        "native/Cargo.toml": package_version(toml("native/Cargo.toml")["package"])
    }
    for name in ("uv.lock", "native/Cargo.lock"):
        packages = toml(name)["package"]
        if not isinstance(packages, list):
            raise TypeError(f"{name}: expected a package array")
        matches = [
            package_version(p) for p in packages if table(p)["name"] == "ledfx-senders"
        ]
        if len(matches) != 1:
            raise ValueError(f"{name}: expected one ledfx-senders version")
        versions[name] = matches[0]
    module = ast.parse((root / "src/ledfx_senders/__init__.py").read_text())
    declared: list[object] = [
        ast.literal_eval(node.value)
        for node in module.body
        if isinstance(node, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == "__version__" for t in node.targets)
    ]
    if len(declared) != 1:
        raise ValueError("Expected one Python package version")
    if not isinstance(declared[0], str):
        raise TypeError("Expected a string Python package version")
    versions["src/ledfx_senders/__init__.py"] = declared[0]
    manifest_version = table(
        json.loads((root / ".release-please-manifest.json").read_text())
    )["."]
    if not isinstance(manifest_version, str):
        raise TypeError("Expected a string release manifest version")
    versions[".release-please-manifest.json"] = manifest_version
    for name, actual in versions.items():
        if actual != version:
            raise ValueError(
                f"{name}: version {actual!r} differs from project {version!r}"
            )
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{version}":
        raise ValueError(
            f"Release tag {ref!r} does not match package version {version!r}"
        )
    publish = (
        event == "push"
        and repository == "LedFx/ledfx-senders"
        and ref == f"refs/tags/v{version}"
    )
    return version, publish


def metadata_only(changed: Iterable[str]) -> bool:
    """True when every changed path is release metadata rebuilds cannot alter."""
    paths = set(changed)
    return bool(paths) and paths <= RELEASE_METADATA


def version_only_change(root: Path, event: str, ref: str) -> bool:
    """True when the whole diff is release metadata a rebuild cannot alter."""
    if event not in {"pull_request", "push"} or ref.startswith("refs/tags/"):
        return False
    # Plan checks out full history: PR merge refs carry no parents otherwise.
    base = "origin/main...HEAD" if event == "pull_request" else "HEAD^"
    try:
        listed = subprocess.run(
            ["git", "diff", "--name-only", base, "HEAD"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        ).stdout.split()
    except (OSError, subprocess.SubprocessError):
        return False  # Unknown history must not silently skip validation.
    return metadata_only(listed)


def main() -> None:
    root = Path(__file__).resolve().parents[1]
    event = os.environ.get("GITHUB_EVENT_NAME", "")
    version, publish = release_plan(
        root,
        event,
        os.environ.get("GITHUB_REPOSITORY", ""),
        os.environ.get("GITHUB_REF", ""),
    )
    values = (
        f"version={version}\n"
        f"release={str(publish).lower()}\n"
        f"version_only={str(version_only_change(root, event, os.environ.get('GITHUB_REF', ''))).lower()}\n"
    )
    print(values, end="")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with Path(output).open("a") as handle:
            handle.write(values)


if __name__ == "__main__":
    main()
