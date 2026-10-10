"""Release permissions depend on the event, exact tag and synchronized versions."""

import json
import subprocess
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


@pytest.fixture
def git_repository(repository: Path) -> Path:
    subprocess.run(["git", "init", "-q"], cwd=repository, check=True)
    subprocess.run(["git", "add", "."], cwd=repository, check=True)
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base"],
        cwd=repository,
        check=True,
    )
    return repository


def commit(repository: Path, *names: str) -> None:
    for name in names:
        path = repository / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists():
            path.write_text(path.read_text().replace("0.1.0", "0.2.0"))
        else:
            path.write_text("new in bump commit\n")
    subprocess.run(["git", "add", "."], cwd=repository, check=True)
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "bump"],
        cwd=repository,
        check=True,
    )


@pytest.mark.parametrize(
    ("files", "expected"),
    [
        (("CHANGELOG.md", ".release-please-manifest.json"), True),
        (("src/ledfx_senders/__init__.py", "CHANGELOG.md"), True),
        (("pyproject.toml", "uv.lock", "native/Cargo.lock"), True),
        (("pyproject.toml", "tests/test_new.py"), False),
        (("native/src/hue/mod.rs",), False),
    ],
)
def test_metadata_only_change_detection(
    git_repository: Path, files: tuple[str, ...], expected: bool
) -> None:
    commit(git_repository, *files)
    from ci.release_guard import metadata_only, version_only_change

    assert version_only_change(git_repository, "push", "refs/heads/main") is expected
    assert metadata_only(files) is (
        set(files)
        <= {
            "CHANGELOG.md",
            ".release-please-manifest.json",
            "src/ledfx_senders/__init__.py",
        }
    )


def test_lock_maintenance_is_not_version_only(git_repository: Path) -> None:
    # A renovate lock bump rewrites other packages' versions and hashes.
    lock = git_repository / "uv.lock"
    text = lock.read_text()
    lock.write_text(
        text.replace(
            '[[package]]\nname = "ledfx-senders"', '[[package]]\nname = "ledfx-senders"'
        )
        + '\n[[package]]\nname = "example-dep"\nversion = "9.9.9"\n'
    )
    subprocess.run(["git", "add", "."], cwd=git_repository, check=True)
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "lock"],
        cwd=git_repository,
        check=True,
    )
    from ci.release_guard import version_only_change

    assert version_only_change(git_repository, "push", "refs/heads/main") is False


def test_version_only_never_applies_to_tags_or_dispatch(git_repository: Path) -> None:
    commit(git_repository, "CHANGELOG.md")
    from ci.release_guard import version_only_change

    assert version_only_change(git_repository, "push", "refs/tags/v0.2.0") is False
    assert (
        version_only_change(git_repository, "workflow_dispatch", "refs/heads/main")
        is False
    )


def test_version_only_fails_open_without_git_history(repository: Path) -> None:
    from ci.release_guard import version_only_change

    assert version_only_change(repository, "push", "refs/heads/main") is False
