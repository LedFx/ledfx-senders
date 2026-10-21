#!/usr/bin/env bash
# Runs inside a 32-bit Debian armhf container against the built armv7l wheels.
# Usage: armv7l_wheel_runtime.sh <system | uv-managed Python version> <checkout>
# "system" installs with Debian's Python 3.13 and NumPy and runs the smokes and
# the test suite. PyPI has no armv7l NumPy wheels, so other interpreters install
# the wheel without dependencies and load its extension module.
set -euo pipefail
python_version=$1
src=$2
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  ca-certificates python3-venv python3-numpy python3-pytest python3-hypothesis >/dev/null

deps=()
if [[ "$python_version" == system ]]; then
  python3 -m venv --system-site-packages /opt/sender-venv
else
  deps=(--no-deps)
  python3 -m venv /opt/uv
  /opt/uv/bin/python -m pip install -q uv==0.13.0
  /opt/uv/bin/uv venv --seed --python "$python_version" /opt/sender-venv
fi
python=/opt/sender-venv/bin/python
"$python" -c 'import platform, struct, sys; assert struct.calcsize("P") == 4, "not 32-bit"; print(sys.version, platform.machine())'
# pip, as Raspberry Pi OS users run it, selects the manylinux armv7l wheel. The
# runner's kernel is AArch64, like 32-bit Raspberry Pi OS on its default 64-bit
# kernel, so the machine name printed above can be aarch64 or armv8l.
"$python" -m pip install -q --no-index --only-binary :all: --find-links "$src/wheelhouse" \
  "${deps[@]}" ledfx-senders
"$python" -m pip list

cd /tmp
if [[ "$python_version" != system ]]; then
  # The package imports NumPy, which has no wheel here: load the extension alone.
  "$python" -I - <<'PY'
import importlib.metadata
import importlib.util
import sys
import sysconfig

(native,) = [
    f
    for f in importlib.metadata.files("ledfx-senders") or ()
    if f.name.startswith("_native.") and f.name.endswith(".so")
]
spec = importlib.util.spec_from_file_location("ledfx_senders._native", native.locate())
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
names = [n for n in dir(module) if not n.startswith("__")]
assert names, "native module exports nothing"
if sysconfig.get_config_var("Py_GIL_DISABLED"):
    assert not sys._is_gil_enabled(), "loading the extension re-enabled the GIL"
print(native.locate(), len(names), "exports")
PY
  exit 0
fi
"$python" -I "$src/ci/native_wheel_smoke.py"
"$python" -I "$src/ci/hue_wheel_smoke.py"
"$python" -I -m pytest --noconftest -p no:cacheprovider "$src/tests" \
  -m "not native_probe and not hue_oracle" -q -rs
