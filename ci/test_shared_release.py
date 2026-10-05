"""Consumer authority remains explicit around the pinned shared transaction."""

import os
import re
import subprocess
import textwrap
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


def test_release_workflow_preserves_identity_gates_and_same_run_artifacts() -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text()
    job = workflow.split("\n  publish-release:\n", 1)[1]
    for required in (
        "needs: [plan, ci-passed]",
        "github.repository == 'LedFx/ledfx-senders'",
        "github.event_name == 'push'",
        "startsWith(github.ref, 'refs/tags/v')",
        "needs.plan.outputs.release == 'true'",
        "environment: pypi",
        "queue: max",
        "cancel-in-progress: false",
        "name: sender-dist",
        "permission-contents: write",
        "permission-attestations: write",
    ):
        assert required in job
    assert "run-id:" not in job
    assert workflow.count("id-token: write") == 1
    pins = re.findall(
        r"uses: LedFx/release-ci/actions/release@([0-9a-f]{40})(?:[ \t]+#.*)?[ \t]*$",
        job,
        re.MULTILINE,
    )
    assert len(pins) == 3 and len(set(pins)) == 1
    assert job.count("uses: LedFx/release-ci/actions/release@") == 3
    assert job.count("project: release-tools") == 3
    assert job.count("wheel-plan: ${{ needs.plan.outputs.wheel-plan }}") == 3
    planning = re.findall(
        r"uses: LedFx/release-ci/actions/plan@([0-9a-f]{40})(?:[ \t]+#.*)?[ \t]*$",
        workflow,
        re.MULTILINE,
    )
    assert planning == [pins[0]]
    assert "sparse-checkout-cone-mode: false" in job
    assert "pyproject.toml" in job
    assert "policy:" not in workflow
    assert (
        "uv run --frozen --only-group wheel-build python -m cibuildwheel ." in workflow
    )
    assert (
        '--config-file pyproject.toml --platform "$PLATFORM" --archs "$ARCH"'
        in workflow
    )
    assert "uses: pypa/cibuildwheel@" not in workflow
    assert (
        job.index("phase: prepare")
        < job.index("uses: actions/attest@")
        < job.index("phase: check-upload")
        < job.index("uses: pypa/gh-action-pypi-publish@")
        < job.index("phase: finalize")
    )
    assert "bundle-path" in job and "sender-release-snapshot.json" in job
    assert "softprops" not in workflow and "--clobber" not in workflow


def test_pyproject_keeps_native_portable_matrix() -> None:
    config = tomllib.loads((ROOT / "pyproject.toml").read_text())
    rows = config["tool"]["release-ci"]["targets"]
    assert rows and len({(row["platform"], row["arch"]) for row in rows}) == len(rows)
    assert all({"runner", "platform", "arch"} <= set(row) for row in rows)
    dependencies = config["dependency-groups"]["wheel-build"]
    assert len(dependencies) == 1
    assert re.fullmatch(
        r"cibuildwheel(?:\[uv\])?==[0-9]+\.[0-9]+\.[0-9]+", dependencies[0]
    )
    assert config["project"]["name"] == "ledfx-senders"
    assert set(config["tool"]["release-ci"]) == {"targets"}
    assert not (ROOT / ".github/release-policy.json").exists()


def test_upload_sidecars_leave_frozen_inputs_unchanged(tmp_path: Path) -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text()
    match = re.search(
        r"(?m)^      - name: Stage verified distributions for PyPI\n"
        r"(?:(?:^        .*\n)|(?:^\n))*?^        run: \|\n"
        r"((?:^          .*\n|^\n)+)",
        workflow,
    )
    assert match is not None, "The uploader needs a separate verified input copy"
    stage = workflow.split("      - name: Stage verified distributions for PyPI\n", 1)[
        1
    ].split("\n      - ", 1)[0]
    assert "if: steps.upload.outputs.pypi_upload == 'true'" in stage
    assert "working-directory: ${{ github.workspace }}" in stage
    assert workflow.index("phase: check-upload") < workflow.index(
        "Stage verified distributions for PyPI"
    )
    uploader = workflow.split("uses: pypa/gh-action-pypi-publish@", 1)[1].split(
        "\n      - ", 1
    )[0]
    assert "packages-dir: pypi-dist/" in uploader
    script = textwrap.dedent(match.group(1))
    original = tmp_path / "dist"
    original.mkdir()
    frozen = {
        "example-1.0-py3-none-any.whl": b"tested wheel",
        "example-1.0.tar.gz": b"tested sdist",
    }
    for name, data in frozen.items():
        (original / name).write_bytes(data)
    result = subprocess.run(
        ["bash", "-euo", "pipefail", "-c", script],
        cwd=tmp_path,
        env={**os.environ, "GITHUB_WORKSPACE": str(tmp_path)},
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    staging = tmp_path / "pypi-dist"
    assert {p.name: p.read_bytes() for p in staging.iterdir()} == frozen
    for name in frozen:
        (staging / (name + ".publish.attestation")).write_bytes(
            b"generated PyPI sidecar"
        )
    assert {p.name: p.read_bytes() for p in original.iterdir()} == frozen
    before_retry = {p.name: p.read_bytes() for p in staging.iterdir()}
    retry = subprocess.run(
        ["bash", "-euo", "pipefail", "-c", script],
        cwd=tmp_path,
        env={**os.environ, "GITHUB_WORKSPACE": str(tmp_path)},
        capture_output=True,
        check=False,
    )
    assert retry.returncode != 0
    assert {p.name: p.read_bytes() for p in staging.iterdir()} == before_retry
    assert {p.name: p.read_bytes() for p in original.iterdir()} == frozen


def _resolved_source(expression: str, context: dict[str, str]) -> str:
    expression = expression.strip()
    assert expression.startswith("${{") and expression.endswith("}}"), expression
    terms = [term.strip() for term in expression[3:-2].split("||")]
    assert terms in (
        ["github.sha"],
        ["github.event.pull_request.head.sha", "github.sha"],
    ), "unsupported source binding"
    return next(context[term] for term in terms if context[term])


def _step_binding(step: str, key: str, default: str | None = None) -> str:
    match = re.search(rf"(?m)^\s+{re.escape(key)}: (.+)$", step)
    if match is not None:
        return match.group(1)
    assert default is not None, f"missing {key} binding"
    return default


@pytest.mark.parametrize("event", ["pull_request", "main", "tag"])
def test_plan_source_matches_actual_checkout_and_audit(
    tmp_path: Path, event: str
) -> None:
    def git(*arguments: str) -> str:
        return subprocess.check_output(
            ["git", "-C", str(tmp_path), *arguments],
            text=True,
            stderr=subprocess.PIPE,
            timeout=10,
        ).strip()

    git("init", "--quiet", "--initial-branch=main")
    commit = (
        "-c",
        "user.name=Source Binding Test",
        "-c",
        "user.email=source@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
    )
    git(*commit, "PR source")
    pull_request_head = git("rev-parse", "HEAD")
    git(*commit, "Event source differs from PR head")
    event_sha = git("rev-parse", "HEAD")
    context = {
        "github.event.pull_request.head.sha": pull_request_head
        if event == "pull_request"
        else "",
        "github.sha": event_sha,
    }
    workflow = (ROOT / ".github/workflows/ci.yml").read_text()
    planning_job = workflow.split("\n  plan:\n", 1)[1].split("\n  lint:\n", 1)[0]
    steps = re.split(r"(?m)^      - ", planning_job)
    checkout = next(step for step in steps if "uses: actions/checkout@" in step)
    planner = next(
        step for step in steps if "uses: LedFx/release-ci/actions/plan@" in step
    )
    checkout_sha = _resolved_source(_step_binding(checkout, "ref"), context)
    # The pinned plan action's published default is github.sha. Exercise that
    # default when the consumer omits its input, reproducing the hosted failure.
    supplied_sha = _resolved_source(
        _step_binding(planner, "source-sha", "${{ github.sha }}"), context
    )
    audit_sha = _resolved_source(_step_binding(workflow, "SOURCE_SHA"), context)
    git("checkout", "--quiet", "--detach", checkout_sha)
    actual_head = git("rev-parse", "HEAD")
    expected = pull_request_head if event == "pull_request" else event_sha
    assert actual_head == expected
    assert supplied_sha == actual_head, "plan input labels another source as tested"
    assert audit_sha == actual_head, "wheel audit labels another source as tested"
    if event == "pull_request":
        assert supplied_sha != event_sha, (
            "synthetic PR merge must not label head wheels"
        )
