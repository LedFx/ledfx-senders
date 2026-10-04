"""Functional arbitrary-effect receiver IPC check, not a capacity benchmark."""

import json
import queue
import socket
import struct
import subprocess
import sys
import threading
from pathlib import Path


def main():
    suffix = ".exe" if sys.platform == "win32" else ""
    binary = (
        Path(__file__).resolve().parents[1]
        / "native/target/release"
        / ("ledfx-receiver" + suffix)
    )
    process = subprocess.Popen(
        [str(binary), "ddp-structural", "4", "batched", "127.0.0.1"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    messages = queue.Queue()
    threading.Thread(
        target=lambda: [messages.put(line) for line in process.stdout], daemon=True
    ).start()

    def read():
        return json.loads(messages.get(timeout=5))

    def command(text):
        process.stdin.write(text + "\n")
        process.stdin.flush()
        return read()

    try:
        ready = read()
        assert ready["unique_identity"] is False
        before = command("snapshot")
        assert before["counts"] == [0, 0, 0, 0, 0]
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            for sequence in (15, 1):
                for offset, data in ((0, b"abcdef"), (6, b"ghijkl")):
                    packet = (
                        struct.pack(
                            "!BBBBLH",
                            0x40 | (offset == 6),
                            sequence,
                            11,
                            1,
                            offset,
                            len(data),
                        )
                        + data
                    )
                    sock.sendto(packet, ("127.0.0.1", ready["port"]))
            for _ in range(50):
                after = command("snapshot")
                if after["counts"][3] == 2:
                    break
            assert after["counts"] == [4, 64, 2, 2, 0], after
            sock.sendto(
                struct.pack("!BBBBLH", 0x40, 2, 11, 1, 0, 6) + b"abcdef",
                ("127.0.0.1", ready["port"]),
            )
        stopped = command("stop")
        process.wait(timeout=5)
        assert process.returncode == 0
        assert stopped["counts"] == [5, 80, 2, 2, 0]
        assert (
            stopped["incomplete_assembly_events"]
            == after["incomplete_assembly_events"] + 1
        )
        assert after["elapsed_seconds"] > before["elapsed_seconds"]
        print(
            json.dumps(
                {"ready": ready, "before": before, "after": after, "stopped": stopped}
            )
        )
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()


if __name__ == "__main__":
    main()
