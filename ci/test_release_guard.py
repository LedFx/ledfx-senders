"""Release permissions depend on the event, exact tag and synchronized versions."""

import json
from pathlib import Path

import pytest

from ci.release_guard import release_plan


@pytest.fixture
def repository(tmp_path: Path) -> Path:
    files = {
        "pyproject.toml": '[project]\nname="ledfx-senders"\nversion="0.1.0"\n',
        "uv.lock": '[[package]]\nname="ledfx-senders"\nversion="0.1.0"\n',
        "native/Cargo.toml": '[package]\nname="ledfx-senders"\nversion="0.1.0"\n',
        "native/Cargo.lock": '[[package]]\nname="ledfx-senders"\nversion="0.1.0"\n',
        "src/ledfx_senders/__init__.py": '__version__ = "0.1.0"\n',
        ".release-please-manifest.json": json.dumps({".": "0.1.0"}),
    }
    for name, value in files.items():
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value)
    return tmp_path


@pytest.mark.parametrize(
    ("event", "repo", "ref", "publish"),
    [
        ("push", "LedFx/ledfx-senders", "refs/tags/v0.1.0", True),
        ("workflow_dispatch", "LedFx/ledfx-senders", "refs/tags/v0.1.0", False),
        ("pull_request", "LedFx/ledfx-senders", "refs/pull/1/merge", False),
        ("push", "someone/ledfx-senders", "refs/tags/v0.1.0", False),
        ("push", "LedFx/ledfx-senders", "refs/heads/main", False),
        ("workflow_dispatch", "LedFx/ledfx-senders", "refs/heads/feature", False),
    ],
)
def test_only_canonical_matching_tag_push_can_publish(
    repository: Path, event: str, repo: str, ref: str, publish: bool
) -> None:
    assert release_plan(repository, event, repo, ref) == ("0.1.0", publish)


@pytest.mark.parametrize("event", ["push", "workflow_dispatch"])
def test_mismatched_tag_fails_even_for_manual_validation(
    repository: Path, event: str
) -> None:
    with pytest.raises(ValueError, match="tag"):
        release_plan(repository, event, "LedFx/ledfx-senders", "refs/tags/v0.2.0")


@pytest.mark.parametrize(
    "name",
    [
        "uv.lock",
        "native/Cargo.toml",
        "native/Cargo.lock",
        "src/ledfx_senders/__init__.py",
        ".release-please-manifest.json",
    ],
)
def test_version_drift_prevents_builds(repository: Path, name: str) -> None:
    path = repository / name
    path.write_text(path.read_text().replace("0.1.0", "0.2.0"))
    with pytest.raises(ValueError, match="version"):
        release_plan(repository, "push", "LedFx/ledfx-senders", "refs/heads/main")
