"""Portable external-oracle control parsing without socket or Cargo requirements."""

import queue

import pytest
from hue_support import StrictOracle, oracle_executable


def test_oracle_control_normalizes_windows_line_endings() -> None:
    # Exercise the real helper queue boundary used by the subprocess reader.
    oracle = StrictOracle.__new__(StrictOracle)
    oracle._lines = queue.Queue()
    oracle._lines.put(b"DONE close\r\n")
    assert oracle._readline() == b"DONE close\n"


def test_required_interop_missing_oracle_fails(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LEDFX_REQUIRE_HUE_INTEROP", "1")
    monkeypatch.delenv("HUE_ORACLE", raising=False)
    with pytest.raises(AssertionError, match="requires HUE_ORACLE"):
        oracle_executable()
