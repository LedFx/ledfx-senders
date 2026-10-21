#!/usr/bin/env bash
# Runs inside a 32-bit Debian armhf container against the built armv7l wheels.
# Usage: armv7l_wheel_runtime.sh <system | uv-managed Python version> <checkout>
# "system" installs with Debian's Python 3.13 and NumPy and runs the test suite.
# PyPI has no armv7l NumPy wheels, so other interpreters run the stdlib smokes.
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
"$python" -c 'import struct, sys; assert struct.calcsize("P") == 4, "not 32-bit"; print(sys.version)'
# pip, as Raspberry Pi OS users run it, selects the manylinux armv7l wheel.
"$python" -m pip install -q --no-index --only-binary :all: --find-links "$src/wheelhouse" \
  "${deps[@]}" ledfx-senders
"$python" -m pip list

cd /tmp
if [[ "$python_version" == *t ]]; then
  export LEDFX_REQUIRE_FREE_THREADING=1
fi
"$python" -I "$src/ci/native_wheel_smoke.py"
"$python" -I "$src/ci/hue_wheel_smoke.py"
if [[ "$python_version" == *t ]]; then
  PYTHON_GIL=0 "$python" -I -X gil=0 "$src/ci/native_wheel_smoke.py"
  PYTHON_GIL=0 "$python" -I -X gil=0 "$src/ci/hue_wheel_smoke.py"
fi
if [[ "$python_version" == system ]]; then
  "$python" -I -m pytest --noconftest -p no:cacheprovider "$src/tests" \
    -m "not native_probe and not hue_oracle" -q -rs
fi
