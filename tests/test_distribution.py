"""The installed sender library is independent of the LedFx application."""

import subprocess
import sys
import sysconfig
from importlib.resources import files

import numpy  # noqa: F401 - verify dependencies do not re-enable a free-threaded GIL

from ledfx_senders import _native


def test_import_does_not_enable_gil() -> None:
    if sysconfig.get_config_var("Py_GIL_DISABLED"):
        assert not getattr(sys, "_is_gil_enabled", lambda: True)()
    assert _native.engine_info()["profile"] == "release"


def test_application_imports_are_not_needed() -> None:
    script = """
import importlib.abc, sys
class BlockApplication(importlib.abc.MetaPathFinder):
    def find_spec(self, fullname, path=None, target=None):
        if fullname == 'ledfx' or fullname.startswith('ledfx.'):
            raise ModuleNotFoundError('LedFx application intentionally unavailable')
sys.meta_path.insert(0, BlockApplication())
from ledfx_senders import Frame
from ledfx_senders import E131Sender
from ledfx_senders.e131 import ChannelLayout
sender = E131Sender._test_sender(ChannelLayout(3), destination='multicast', source_name='independent', mode='capture')
frame: Frame = bytes([1, 2, 3])
sender.send(frame)
assert sender._engine.captures()[0][0][126:129] == bytes([1, 2, 3])
sender.close(False)
from ledfx_senders import DDPSender, OPCSender
for cls, count in ((DDPSender, 3), (OPCSender, 1)):
    sender = cls._test_sender(count, destination='127.0.0.1', mode='capture')
    sender.send(bytes([1, 2, 3]))
    assert sender._engine.captures()[0][0].endswith(bytes([1, 2, 3]))
    sender.close()
"""
    script += """
from ledfx_senders import OSCSender, UDPRealtimeSender
osc = OSCSender._test_sender(destination='127.0.0.1',pixel_count=1,path='/x',send_type='Three_Arguments',mode='capture')
rt = UDPRealtimeSender._test_sender(destination='127.0.0.1',pixel_count=1,packet_type='DRGB',timeout=1,minimise_traffic=True,mode='capture')
for sender in (osc,rt):
    sender.send(bytes([1,2,3]),now=0)
    sender.send(bytes([1,2,3]),now=0.1)
    assert len(sender._engine.captures())==1
    sender.close()
"""
    subprocess.run([sys.executable, "-I", "-c", script], check=True, timeout=15)


def test_installed_distribution_includes_public_typing() -> None:
    package = files("ledfx_senders")
    assert package.joinpath("py.typed").is_file()
    assert package.joinpath("_native.pyi").is_file()
