"""Compile independent test OpenSSL tooling; never include it in sender wheels."""

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def windows_environment() -> dict[str, str]:
    environment = dict(os.environ)
    if shutil.which("cl"):
        return environment
    vswhere = (
        Path(os.environ["ProgramFiles(x86)"])
        / "Microsoft Visual Studio/Installer/vswhere.exe"
    )
    installation = subprocess.check_output(
        [
            str(vswhere),
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
        text=True,
        timeout=20,
    ).strip()
    vcvars = Path(installation) / "VC/Auxiliary/Build/vcvars64.bat"
    result = subprocess.check_output(
        # cmd parses its command tail itself; CRT list quoting would insert
        # literal backslashes before the batch-path quotes.
        f'"{os.environ["COMSPEC"]}" /d /s /c ""{vcvars}" >nul && set"',
        text=True,
        timeout=30,
    )
    for line in result.splitlines():
        key, separator, value = line.partition("=")
        if separator and key:
            environment[key.upper()] = value
    return environment


def default_prefix() -> Path:
    if sys.platform == "win32":
        return Path(os.environ["ProgramFiles"]) / "OpenSSL"
    if sys.platform == "darwin":
        return Path(
            subprocess.check_output(
                ["brew", "--prefix", "openssl@3"], text=True
            ).strip()
        )
    return Path("/usr")


def build(output: Path, prefix: Path) -> dict[str, str]:
    output = output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    include = prefix / "include"
    if not (include / "openssl/ssl.h").is_file():
        raise RuntimeError(f"required OpenSSL headers missing: {include}")
    source = ROOT / "tests/fixtures/hue/openssl_psk_server.c"
    environment = windows_environment() if sys.platform == "win32" else dict(os.environ)
    if sys.platform == "win32":
        libraries = prefix / "lib/VC/x64/MD"
        if not (libraries / "libssl.lib").is_file():
            libraries = prefix / "lib"
        for name in ("libssl.lib", "libcrypto.lib"):
            if not (libraries / name).is_file():
                raise RuntimeError(
                    f"required OpenSSL import library missing: {libraries / name}"
                )
        compiler = shutil.which("cl", path=environment["PATH"])
        if compiler is None:
            raise RuntimeError(
                "required Windows C compiler missing from developer PATH"
            )
        command = [
            str(Path(compiler).resolve()),
            "/nologo",
            "/std:c11",
            "/W4",
            "/WX",
            "/O2",
            f"/I{include}",
            str(source),
            f"/Fe:{output}",
            f"/Fo:{output.with_suffix('.obj')}",
            "/link",
            f"/LIBPATH:{libraries}",
            "libssl.lib",
            "libcrypto.lib",
            "Ws2_32.lib",
        ]
        environment["PATH"] = str(prefix / "bin") + os.pathsep + environment["PATH"]
    else:
        command = [
            os.environ.get("CC", "cc"),
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            f"-I{include}",
        ]
        # Debian splits architecture-specific configuration headers and libraries.
        for directory in sorted(include.glob("*-linux-gnu")):
            command.append(f"-I{directory}")
        command.append(str(source))
        for directory in (
            prefix / "lib",
            prefix / "lib64",
            *sorted((prefix / "lib").glob("*-linux-gnu")),
        ):
            if directory.is_dir():
                command.extend([f"-L{directory}", f"-Wl,-rpath,{directory}"])
        command.extend(["-lssl", "-lcrypto", "-o", str(output)])
    result = subprocess.run(
        command,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
        timeout=60,
    )
    if result.returncode:
        raise RuntimeError(result.stdout + result.stderr)
    versions = subprocess.check_output(
        [str(output), "--version"], env=environment, text=True, timeout=10
    ).splitlines()
    openssl = prefix / "bin" / ("openssl.exe" if sys.platform == "win32" else "openssl")
    executable = (
        str(openssl)
        if openssl.is_file()
        else shutil.which("openssl", path=environment.get("PATH"))
    )
    if executable is None:
        raise RuntimeError("required independent OpenSSL executable missing")
    cli_version = subprocess.check_output(
        [executable, "version", "-a"], env=environment, text=True, timeout=10
    )
    return {
        "executable": str(output),
        "openssl": executable,
        "openssl_prefix": str(prefix),
        "build_version": versions[0],
        "runtime_version": versions[1],
        "cli_version": cli_version,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--openssl-prefix", type=Path)
    parser.add_argument("--github-env", type=Path)
    arguments = parser.parse_args()
    metadata = build(arguments.output, arguments.openssl_prefix or default_prefix())
    print(json.dumps(metadata))
    if arguments.github_env is not None:
        with arguments.github_env.open("a", encoding="utf-8") as file:
            file.write(
                f"HUE_ORACLE={metadata['executable']}\nHUE_OPENSSL={metadata['openssl']}\nLEDFX_REQUIRE_HUE_INTEROP=1\n"
            )
            if sys.platform == "win32":
                file.write(
                    f"PATH={Path(metadata['openssl']).parent}{os.pathsep}{os.environ['PATH']}\n"
                )


if __name__ == "__main__":
    main()
