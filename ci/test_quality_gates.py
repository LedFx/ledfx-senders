"""Repository gates must reject untyped additions outside the public package too."""

import json
import subprocess
import sys
from pathlib import Path

import pytest

from ci.archive_measurement import hash_records
from ci.benchmark_whole_call import decode_case
from ci.structural_smoke import decode_message

ROOT = Path(__file__).resolve().parents[1]


def test_new_python_and_stubs_cannot_escape_project_gates(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_bytes((ROOT / "pyproject.toml").read_bytes())
    names = (
        "pdm_build.py",
        "ci/new_guard.py",
        "native/bench/receiver/new_check.py",
        "src/example/new_binding.pyi",
        "tests/test_new_behavior.py",
        "future_tools/new_tool.py",
    )
    for name in names:
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("def missing(value):\n    return value\n")
    (tmp_path / "explicit_any.py").write_text(
        "from typing import Any\ndef untyped(value: Any) -> Any:\n    return value\n"
    )
    commands = (
        [
            "pyrefly",
            "check",
            "--config",
            "pyproject.toml",
            "--output-format",
            "min-text",
        ],
        ["ruff", "check", ".", "--output-format", "concise"],
    )
    for command in commands:
        result = subprocess.run(
            [sys.executable, "-m", *command],
            cwd=tmp_path,
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        output = (result.stdout + result.stderr).replace("\\", "/")
        assert result.returncode == 1, output
        for name in (*names, "explicit_any.py"):
            assert name in output, output
        if command[0] == "ruff":
            assert "ANN001" in output and "ANN201" in output and "ANN401" in output
        else:
            assert "implicit-any-parameter" in output and "explicit-any" in output


@pytest.mark.parametrize("value", [{"file": 123}, ["not", "a", "mapping"]])
def test_artifact_hashes_reject_non_string_data(value: object) -> None:
    with pytest.raises(TypeError):
        hash_records(value)


def test_case_and_receiver_json_validate_consumed_fields() -> None:
    with pytest.raises(TypeError, match="pixels"):
        decode_case(
            json.dumps({"protocol": "adalight", "pixels": "170", "dtype": "uint8"})
        )
    with pytest.raises(TypeError, match="counts"):
        decode_message(json.dumps({"counts": [1, "two"]}))
    with pytest.raises(TypeError, match="incomplete_assembly_events"):
        decode_message(json.dumps({"incomplete_assembly_events": "one"}))
    assert (
        decode_case(
            '{"protocol":"adalight","pixels":170,"dtype":"uint8","order":"RGB"}'
        )["order"]
        == "RGB"
    )
    assert decode_message('{"counts":[1,2,3,4,5],"port":4048}')["counts"] == [
        1,
        2,
        3,
        4,
        5,
    ]
