"""Use the locked tool interpreter for strict source, stub and test checking."""

import subprocess
import sys


def main() -> int:
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "pyrefly",
            "check",
            "--python-interpreter-path",
            sys.executable,
        ],
        check=False,
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
