"""Functional arbitrary-effect receiver IPC check, not a capacity benchmark."""

import json
import queue
import socket
import struct
import subprocess
import sys
import threading
from pathlib import Path
from typing import TypedDict, cast


class ReceiverMessage(TypedDict, total=False):
    port: int
    unique_identity: bool
    counts: list[int]
    incomplete_assembly_events: int
    elapsed_seconds: float


def decode_message(line: str) -> ReceiverMessage:
    value: object = json.loads(line)
    if not isinstance(value, dict):
        raise TypeError("Expected a receiver object")
    for key in ("port", "incomplete_assembly_events"):
        if key in value and not isinstance(value[key], int):
            raise TypeError(f"Invalid receiver field: {key}")
    if "unique_identity" in value and not isinstance(value["unique_identity"], bool):
        raise TypeError("Invalid unique_identity")
    if "counts" in value:
        counts = value["counts"]
        if not isinstance(counts, list) or not all(
            isinstance(item, int) for item in counts
        ):
            raise TypeError("Invalid receiver counts")
    if "elapsed_seconds" in value and not isinstance(
        value["elapsed_seconds"], (float, int)
    ):
        raise TypeError("Invalid elapsed_seconds")
    return cast(ReceiverMessage, value)


def main() -> None:
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
    assert process.stdout is not None and process.stdin is not None
    output = process.stdout
    input_pipe = process.stdin
    messages: queue.Queue[str] = queue.Queue()
    threading.Thread(
        target=lambda: [messages.put(line) for line in output], daemon=True
    ).start()

    def read() -> ReceiverMessage:
        return decode_message(messages.get(timeout=5))

    def command(text: str) -> ReceiverMessage:
        input_pipe.write(text + "\n")
        input_pipe.flush()
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
