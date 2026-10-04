"""Check synchronized package versions before any build or release decision."""

import ast
import json
import os
import re
import tomllib
from pathlib import Path


def release_plan(root: Path, event: str, repository: str, ref: str) -> tuple[str, bool]:
    def toml(name: str) -> dict:
        return tomllib.loads((root / name).read_text())

    version = toml("pyproject.toml")["project"]["version"]
    if not isinstance(version, str) or not re.fullmatch(
        r"[0-9][0-9A-Za-z.+-]*", version
    ):
        raise ValueError("Invalid project version")
    versions = {"native/Cargo.toml": toml("native/Cargo.toml")["package"]["version"]}
    for name in ("uv.lock", "native/Cargo.lock"):
        matches = [
            p["version"] for p in toml(name)["package"] if p["name"] == "ledfx-senders"
        ]
        if len(matches) != 1:
            raise ValueError(f"{name}: expected one ledfx-senders version")
        versions[name] = matches[0]
    module = ast.parse((root / "src/ledfx_senders/__init__.py").read_text())
    declared = [
        ast.literal_eval(node.value)
        for node in module.body
        if isinstance(node, ast.Assign)
        and any(isinstance(t, ast.Name) and t.id == "__version__" for t in node.targets)
    ]
    if len(declared) != 1:
        raise ValueError("Expected one Python package version")
    versions["src/ledfx_senders/__init__.py"] = declared[0]
    versions[".release-please-manifest.json"] = json.loads(
        (root / ".release-please-manifest.json").read_text()
    )["."]
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


def main() -> None:
    version, publish = release_plan(
        Path(__file__).resolve().parents[1],
        os.environ.get("GITHUB_EVENT_NAME", ""),
        os.environ.get("GITHUB_REPOSITORY", ""),
        os.environ.get("GITHUB_REF", ""),
    )
    values = f"version={version}\nrelease={str(publish).lower()}\n"
    print(values, end="")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with Path(output).open("a") as handle:
            handle.write(values)


if __name__ == "__main__":
    main()
