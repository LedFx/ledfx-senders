"""Independent executable loopback controls; stdlib only, no sender import.

Run after cargo build --release --bin ledfx-receiver.
"""

import json
import socket
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

BINARY = Path(__file__).resolve().parents[2] / "target/release/ledfx-receiver"


def packet(protocol, identity, index=0):
    chunk = 1440 if protocol == "ddp" else 510
    size = chunk if index == 0 else 3
    data = bytearray([7] * size)
    width = min(size, 8)
    data[:width] = identity.to_bytes(8, "big")[-width:]
    if protocol == "ddp":
        return (
            struct.pack(
                "!BBBBIH",
                0x40 if index == 0 else 0x41,
                identity % 15 + 1,
                11,
                1,
                index * chunk,
                size,
            )
            + data
        )
    p = bytearray(638)
    p[:16] = bytes([0, 16, 0, 0, 65, 83, 67, 45, 69, 49, 46, 49, 55, 0, 0, 0])
    struct.pack_into("!HI", p, 16, 0x726E, 4)
    struct.pack_into("!HI", p, 38, 0x7258, 2)
    p[108] = 100
    struct.pack_into("!HBBH", p, 109, 63999, (identity - 1) % 256, 0, index + 1)
    p[115:126] = bytes([0x72, 11, 2, 0xA1, 0, 0, 0, 1, 2, 1, 0])
    p[126 : 126 + size] = data
    return p


class ReceiverTests(unittest.TestCase):
    def test_wire_controls_on_both_receive_backends(self):
        for backend in ["portable", "batched"]:
            for protocol in ["ddp", "e131", "opc"]:
                with (
                    self.subTest(backend=backend, protocol=protocol),
                    tempfile.TemporaryDirectory() as directory,
                ):
                    count = {"ddp": 1443, "e131": 513, "opc": 12}[protocol]
                    fixture = Path(directory) / "fixture"
                    fixture.write_bytes(bytes([7] * count))
                    receiver = subprocess.Popen(
                        [
                            str(BINARY),
                            protocol,
                            str(fixture),
                            backend,
                            "32",
                            "1" if protocol == "ddp" else "0",
                        ],
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        text=True,
                    )
                    try:
                        ready = json.loads(receiver.stdout.readline())
                        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                            destination = ("127.0.0.1", ready["port"])
                            sender.sendto(b"bad", destination)
                            if protocol == "opc":
                                p = (
                                    b"\0\0\0\x0c"
                                    + (1).to_bytes(8, "big")
                                    + bytes([7] * 4)
                                )
                                sender.sendto(p, destination)
                                sender.sendto(p, destination)
                            else:
                                for identity, index in [(1, 1), (1, 0), (1, 0), (2, 0)]:
                                    sender.sendto(
                                        packet(protocol, identity, index), destination
                                    )
                        out, err = receiver.communicate("stop 3\n", timeout=5)
                        self.assertEqual(receiver.returncode, 0, err)
                        result = json.loads(out)
                        self.assertEqual(result["complete_frames"], 1)
                        self.assertEqual(result["duplicate_packets"], 1)
                        self.assertEqual(result["invalid_packets"], 1)
                        self.assertEqual(
                            result["incomplete_frames"], int(protocol != "opc")
                        )
                        self.assertEqual(
                            result["unseen_frames"], 2 if protocol == "opc" else 1
                        )
                        self.assertEqual(result["wrong_sequence"], 0)
                        if result["cpu_seconds"] is not None:
                            self.assertAlmostEqual(
                                result["cpu_seconds"],
                                result["cpu_user_seconds"]
                                + result["cpu_system_seconds"],
                            )
                    finally:
                        if receiver.poll() is None:
                            receiver.kill()
                            receiver.communicate()


if __name__ == "__main__":
    unittest.main()
