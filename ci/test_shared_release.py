"""Consumer authority remains explicit around the pinned shared transaction."""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SHARED_SHA = "f57f5eced6c74aadb1d432fc349554212d7cf05f"


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
    assert job.count("LedFx/release-ci/actions/release@" + SHARED_SHA) == 3
    assert job.count("policy: release-tools/.github/release-policy.json") == 3
    assert (
        job.index("phase: prepare")
        < job.index("uses: actions/attest@")
        < job.index("phase: check-upload")
        < job.index("uses: pypa/gh-action-pypi-publish@")
        < job.index("phase: finalize")
    )
    assert "bundle-path" in job and "sender-release-snapshot.json" in job
    assert "softprops" not in workflow and "--clobber" not in workflow


def test_explicit_policy_keeps_native_portable_matrix() -> None:
    policy = json.loads((ROOT / ".github/release-policy.json").read_text())
    assert policy["repository"] == "LedFx/ledfx-senders"
    assert policy["workflow"] == ".github/workflows/ci.yml"
    assert policy["python"]["project"] == "ledfx-senders"
    tags = policy["python"]["wheel_tags"]
    assert len(tags) == len(set(tags)) == 35
    assert sum("cp314t-" in tag or "cp315t-" in tag for tag in tags) == 10
    assert policy["python"]["sdist"] == "ledfx_senders-{version}.tar.gz"
    assert policy["github_assets"] == {"distributions": True, "files": []}
    assert policy["oci"] == []
