"""Short native-receiver capacity smoke; loss is measured, never hidden."""

import json
import os
import platform
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from ledfx_senders import DDPSender


def trial(fps: int) -> dict[str, object]:
    root = Path(__file__).resolve().parents[1]
    suffix = ".exe" if sys.platform == "win32" else ""
    binary = root / "native/target/release" / ("ledfx-receiver" + suffix)
    frame = bytearray([127]) * 150000
    with tempfile.TemporaryDirectory() as directory:
        fixture = Path(directory) / "fixture.rgb"
        fixture.write_bytes(frame)
        receiver = subprocess.Popen(
            [str(binary), "ddp", str(fixture), "batched", "512", "1"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            assert receiver.stdout is not None
            ready = json.loads(receiver.stdout.readline())
            sender = DDPSender(150000, destination="127.0.0.1", port=ready["port"])
            sent = 0
            start = time.perf_counter()
            cpu_start = time.process_time()
            process_start = os.times()
            while time.perf_counter() - start < 2:
                identity = (sent + 1).to_bytes(8, "big")
                for offset in range(0, len(frame), 1440):
                    frame[offset : offset + 8] = identity
                sender.send(frame)
                sent += 1
                if fps:
                    delay = start + sent / fps - time.perf_counter()
                    if delay > 0:
                        time.sleep(delay)
            elapsed = time.perf_counter() - start
            cpu = time.process_time() - cpu_start
            process_end = os.times()
            counters = sender._engine.counters()
            output, error = receiver.communicate(f"stop {sent}\n", timeout=5)
            sender.close()
            if receiver.returncode:
                raise RuntimeError(error)
            received = json.loads(output)
            assert received["invalid_packets"] == 0
            assert received["complete_frames"] <= sent
            return {
                "scope": "short exploratory receiver smoke, including Python chunk identity marking; no performance gate",
                "input_dtype": "uint8",
                "input_format": "B",
                "input_shape": [150000],
                "input_layout": "contiguous bytearray of 50000 RGB pixels",
                "numeric_conversion": "none; byte fast path",
                "platform": platform.platform(),
                "machine": platform.machine(),
                "python": sys.version,
                "requested_fps": fps,
                "seconds": elapsed,
                "sender_cpu_seconds": cpu,
                "sender_cpu_user_seconds": process_end.user - process_start.user,
                "sender_cpu_system_seconds": process_end.system - process_start.system,
                "submitted_frames": sent,
                "sender_counters": counters,
                "receiver_ready": ready,
                "receiver": received,
            }
        finally:
            if receiver.poll() is None:
                receiver.kill()
                receiver.communicate()


if __name__ == "__main__":
    for rate in [60, 0]:
        print(json.dumps(trial(rate)), flush=True)
