"""Actual Windows command startup regressions; no C compiler or OpenSSL needed."""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import override
from unittest.mock import patch

from ci import build_hue_oracle as builder


@unittest.skipUnless(sys.platform == "win32", "requires Windows process creation")
class WindowsOracleBootstrapTests(unittest.TestCase):
    @override
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="hue windows tools ", dir=builder.ROOT.parent
        )
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.parent_environment = dict(os.environ)
        self.parent_environment["PATH"] = str(
            Path(os.environ["SystemRoot"]) / "System32"
        )
        self.parent_environment["ProgramFiles(x86)"] = str(self.directory)
        self.compiler_directory = self.directory / "Compiler Tools"
        self.compiler_directory.mkdir()
        # Match PATHEXT casing even on the case-sensitive WSL UNC share.
        self.compiler = self.compiler_directory / "cl.EXE"
        # cmd is a real Windows executable with system DLLs; only startup is tested.
        shutil.copyfile(os.environ["COMSPEC"], self.compiler)

    def test_vcvars_batch_with_spaces_captures_environment(self) -> None:
        installation = self.directory / "Visual Studio"
        batch = installation / "VC/Auxiliary/Build/vcvars64.bat"
        batch.parent.mkdir(parents=True)
        batch.write_text(
            "@echo off\nset HUE_VCVARS_PROBE=spaced-batch-ran\n"
            f"set Path={self.compiler_directory}\n",
            encoding="utf-8",
        )
        vswhere = self.directory / "Microsoft Visual Studio/Installer/vswhere.exe"
        check_output = subprocess.check_output

        def discover(args: str | list[str], *, text: bool, timeout: int) -> str:
            # This host has no Visual Studio; replace discovery only, then execute
            # the production cmd invocation against the actual temporary batch.
            if isinstance(args, list) and args[0] == str(vswhere):
                return str(installation)
            assert text
            return check_output(args, text=True, timeout=timeout)

        with (
            patch.dict(os.environ, self.parent_environment, clear=True),
            patch.object(builder.subprocess, "check_output", side_effect=discover),
        ):
            self.assertIsNone(shutil.which("cl"))
            environment = builder.windows_environment()
        self.assertEqual(environment["HUE_VCVARS_PROBE"], "spaced-batch-ran")
        self.assertEqual(environment["PATH"], str(self.compiler_directory))
        self.assertEqual(
            [key for key in environment if key.upper() == "PATH"], ["PATH"]
        )

    def test_compiler_launch_uses_captured_path_without_parent_compiler(self) -> None:
        prefix = self.directory / "OpenSSL"
        for relative in (
            "include/openssl/ssl.h",
            "lib/libssl.lib",
            "lib/libcrypto.lib",
        ):
            file = prefix / relative
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_bytes(b"startup-only fixture")
        environment = dict(self.parent_environment)
        environment["PATH"] = str(self.compiler_directory)
        run = subprocess.run

        def launch(
            command: list[str],
            *,
            env: dict[str, str],
            capture_output: bool,
            text: bool,
            check: bool,
            timeout: int,
        ) -> subprocess.CompletedProcess[str]:
            # Retain the builder's actual executable choice and child environment.
            # C arguments are replaced because this intentionally is not a compiler.
            assert capture_output and text and not check
            result = run(
                [command[0], "/d", "/c", "echo hue-compiler-started"],
                env=env,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), "hue-compiler-started")
            return subprocess.CompletedProcess(command, 1, result.stdout, "")

        with (
            patch.dict(os.environ, self.parent_environment, clear=True),
            patch.object(builder, "windows_environment", return_value=environment),
            patch.object(builder.subprocess, "run", side_effect=launch),
        ):
            self.assertIsNone(shutil.which("cl"))
            with self.assertRaisesRegex(RuntimeError, "hue-compiler-started"):
                builder.build(self.directory / "oracle.exe", prefix)


if __name__ == "__main__":
    unittest.main()
