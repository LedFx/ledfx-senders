"""Validate version_only_change against the repository's real history."""

import subprocess
import sys

sys.path.insert(0, "/tmp/ci-gating")

from ci.release_guard import version_only_change  # noqa: E402

REPO = "/tmp/ci-gating"


def at(ref: str, event: str = "push") -> bool:
    """Run the check in a worktree checked out at ref."""
    worktree = f"/tmp/vo-check-{ref.replace('/', '-')}"
    subprocess.run(
        ["git", "worktree", "add", "--detach", worktree, ref],
        cwd=REPO,
        check=True,
        capture_output=True,
    )
    try:
        return version_only_change(Path(worktree), event, "refs/heads/main")
    finally:
        subprocess.run(
            ["git", "worktree", "remove", "--force", worktree],
            cwd=REPO,
            check=True,
            capture_output=True,
        )


from pathlib import Path  # noqa: E402

results = {
    "release merge 219fe2a (should be True)": at("219fe2a"),
    "hue PR merge aab94ed (should be False)": at("aab94ed"),
    "main HEAD (should be False, CI commit)": at("HEAD"),
}
for label, value in results.items():
    print(f"{label}: {value}")
assert results["release merge 219fe2a (should be True)"] is True
assert results["hue PR merge aab94ed (should be False)"] is False
assert results["main HEAD (should be False, CI commit)"] is False
print("REAL-HISTORY VALIDATION OK")
