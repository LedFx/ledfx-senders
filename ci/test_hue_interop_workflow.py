"""Execute the native oracle workflow with a cold target and controlled tools."""

import json
import os
import re
import shlex
import subprocess
import sys
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_native_oracle_prebuilds_debug_probe_before_protocols(tmp_path: Path) -> None:
    workflow = (ROOT / ".github/workflows/ci.yml").read_text()
    job = workflow.split("\n  native-hue-interop:\n", 1)[1].split(
        "\n  native-wheels:\n", 1
    )[0]
    scripts = re.findall(
        r"(?m)^        run: (?:\|\n((?:^          .*\n|^\n)+)|([^\n]+))", job
    )
    tools = tmp_path / "tools"
    tools.mkdir()
    driver = tools / "driver.py"
    driver.write_text(
        textwrap.dedent("""\
        import json
        import os
        import sys
        from pathlib import Path

        command, *args = sys.argv[1:]
        state_file = Path(os.environ["RUNNER_TEMP"]) / "state.json"
        state = json.loads(state_file.read_text()) if state_file.exists() else {}
        runtime = str(Path(os.environ["RUNNER_TEMP"]) / "project-python")
        if command in {"sudo", "rustup"}:
            sys.exit(0)
        if command == "uv":
            if args[0] == "sync":
                sys.exit(0)  # Package installation supplies only release artifacts.
            assert args[:2] == ["run", "--frozen"], args
            assert args[2] in {"--group", "--only-group"} and args[3] == "dev", args
            args = args[4:]
            if args[:2] == ["python", "-c"]:
                print(runtime)
                sys.exit(0)
            if "ci/build_hue_oracle.py" in args:
                assert state.get("built"), "cold debug probe not built before oracle"
                state["oracle"] = True
            else:
                assert args[:3] == ["python", "-m", "pytest"], args
                assert state.get("oracle"), "protocol tests precede oracle setup"
                assert state.get("built"), "cold probe would compile during watchdog"
                assert os.environ.get("PYO3_PYTHON") == runtime
                current = {key: os.environ.get(key) for key in state["build_env"]}
                assert current == state["build_env"], "probe build/run env differs"
                state["tested"] = True
        elif command == "cargo":
            assert args == ["build", "--manifest-path", "native/Cargo.toml",
                            "--locked", "--example", "hue-probe"], args
            assert os.environ.get("PYO3_PYTHON") == runtime
            state["built"] = True
            state["build_env"] = {key: os.environ.get(key) for key in
                                  ("PYO3_PYTHON", "CARGO_HOME", "RUSTUP_HOME",
                                   "CARGO_TARGET_DIR", "RUSTUP_TOOLCHAIN")}
        else:
            raise AssertionError(command)
        state_file.write_text(json.dumps(state))
        """)
    )
    for command in ("sudo", "rustup", "uv", "cargo"):
        executable = tools / command
        executable.write_text(
            "#!/bin/sh\nexec "
            + shlex.join([sys.executable, str(driver), command])
            + ' "$@"\n'
        )
        executable.chmod(0o755)
    github_env = tmp_path / "github-env"
    github_env.touch()
    # Toolchain steps read the real pinned channel; stage it beside the replay.
    (tmp_path / "rust-toolchain.toml").write_text(
        (ROOT / "rust-toolchain.toml").read_text()
    )
    environment = {
        **os.environ,
        "PATH": str(tools) + os.pathsep + os.environ["PATH"],
        "RUNNER_TEMP": str(tmp_path),
        "GITHUB_ENV": str(github_env),
        "CARGO_HOME": str(tmp_path / "cargo"),
        "RUSTUP_HOME": str(tmp_path / "rustup"),
        "CARGO_TARGET_DIR": str(tmp_path / "cold-target"),
        "RUSTUP_TOOLCHAIN": "1.94.0",
    }
    environment.pop("PYO3_PYTHON", None)
    for block, line in scripts:
        result = subprocess.run(
            ["bash", "-euo", "pipefail", "-c", textwrap.dedent(block) or line],
            cwd=tmp_path,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        for setting in github_env.read_text().splitlines():
            key, value = setting.split("=", 1)
            environment[key] = value
    state = json.loads((tmp_path / "state.json").read_text())
    assert state.get("tested"), "workflow must reach the protocol test gate"
