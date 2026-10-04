"""The installed sender library is independent of the LedFx application."""

import subprocess
import sys
import sysconfig

import numpy  # noqa: F401 - verify dependencies do not re-enable a free-threaded GIL

from ledfx_senders import _native


def test_import_does_not_enable_gil():
    if sysconfig.get_config_var("Py_GIL_DISABLED"):
        assert not getattr(sys, "_is_gil_enabled", lambda: True)()
    assert _native.engine_info()["profile"] == "release"


def test_application_imports_are_not_needed():
    script = """
import importlib.abc, sys
class BlockApplication(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == 'ledfx' or fullname.startswith('ledfx.'):
            raise ModuleNotFoundError('LedFx application intentionally unavailable')
sys.meta_path.insert(0, BlockApplication())
from ledfx_senders.e131 import E131Sender
from ledfx_senders.e131_buffer import ChannelLayout
sender = E131Sender._test_sender(ChannelLayout(3), destination='multicast', source_name='independent', mode='capture')
sender.send(bytes([1, 2, 3]))
assert sender._engine.captures()[0][0][126:129] == bytes([1, 2, 3])
sender.close(False)
"""
    subprocess.run([sys.executable, "-I", "-c", script], check=True, timeout=15)
