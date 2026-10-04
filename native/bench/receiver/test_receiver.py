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
    def test_stateful_literal_receipts_on_both_backends(self):
        for backend in ["portable", "batched"]:
            for protocol in [
                "osc-one",
                "osc-three",
                "osc-channels",
                "osc-all",
                "udp-drgb",
                "udp-warls",
                "udp-drgbw",
                "udp-dnrgb",
                "udp-raw",
                "udp-adaptive",
            ]:
                with (
                    self.subTest(protocol=protocol, backend=backend),
                    tempfile.TemporaryDirectory() as directory,
                ):
                    is_osc = protocol.startswith("osc-")
                    count = 1470 if protocol == "udp-dnrgb" else 6
                    fixture = Path(directory) / "fixture"
                    fixture.write_bytes(
                        struct.pack(">f", 0.5) * 6 if is_osc else bytes([7]) * count
                    )
                    receiver = subprocess.Popen(
                        [str(BINARY), protocol, str(fixture), backend, "32", "1"],
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        text=True,
                    )
                    try:
                        ready = json.loads(receiver.stdout.readline())
                        if is_osc:
                            width = (
                                1
                                if protocol == "osc-channels"
                                else 6
                                if protocol == "osc-all"
                                else 3
                            )
                            tag = {
                                "osc-one": ",[fff]",
                                "osc-three": ",fff",
                                "osc-channels": ",f",
                                "osc-all": ",[fff][fff]",
                            }[protocol]
                            packets = []
                            for index in range(6 // width):
                                p = bytearray()
                                for text in [f"/bench/{index}", tag]:
                                    p.extend(text.encode())
                                    p.extend(bytes((-len(p) - 1) % 4 + 1))
                                p.extend(struct.pack(">f", 1))
                                p.extend(struct.pack(">f", 0.5) * (width - 1))
                                packets.append(p)
                        else:
                            packets = {
                                "udp-drgb": [bytes([2, 1, 0, 0, 1, 7, 7, 7])],
                                "udp-warls": [bytes([1, 1, 0, 0, 0, 1])],
                                "udp-drgbw": [bytes([3, 1, 0, 0, 1, 0, 7, 7, 7, 0])],
                                "udp-dnrgb": [
                                    bytes([4, 1, 0, 0, 0, 0, 1]) + bytes([7]) * 1464,
                                    bytes([4, 1, 1, 233, 0, 0, 1]),
                                ],
                                "udp-raw": [bytes([0, 0, 1, 7, 7, 7])],
                                "udp-adaptive": [bytes([1, 1, 0, 0, 0, 1])],
                            }[protocol]
                        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                            for p in reversed(packets):
                                sender.sendto(p, ("127.0.0.1", ready["port"]))
                        out, error = receiver.communicate("stop 1\n", timeout=5)
                        self.assertEqual(receiver.returncode, 0, error)
                        result = json.loads(out)
                        self.assertEqual(result["packets"], len(packets))
                        self.assertEqual(result["complete_frames"], 1)
                        self.assertEqual(result["invalid_packets"], 0)
                        self.assertEqual(result["incomplete_frames"], 0)
                    finally:
                        if receiver.poll() is None:
                            receiver.kill()
                            receiver.communicate()

    def test_e131_mixed_cid_never_completes(self):
        for backend in ["portable", "batched"]:
            with (
                self.subTest(backend=backend),
                tempfile.TemporaryDirectory() as directory,
            ):
                fixture = Path(directory) / "fixture"
                fixture.write_bytes(bytes([7] * 513))
                receiver = subprocess.Popen(
                    [str(BINARY), "e131", str(fixture), backend, "32", "0"],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                try:
                    ready = json.loads(receiver.stdout.readline())
                    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                        destination = ("127.0.0.1", ready["port"])
                        first = packet("e131", 1, 0)
                        first[22:38] = bytes([1] * 16)
                        sender.sendto(first, destination)
                        second = packet("e131", 1, 1)
                        second[22:38] = bytes([2] * 16)
                        sender.sendto(second, destination)
                        # All three control variants use the wrong CID too.
                        sync = first[:49]
                        struct.pack_into("!HI", sync, 16, 0x7021, 8)
                        struct.pack_into("!HI", sync, 38, 0x700B, 1)
                        struct.pack_into("!H", sync, 45, 63999)
                        discovery = bytearray(122)
                        discovery[:44] = sync[:44]
                        struct.pack_into("!HI", discovery, 16, 0x706A, 8)
                        struct.pack_into("!HI", discovery, 38, 0x7054, 2)
                        struct.pack_into("!HI", discovery, 112, 0x700A, 1)
                        struct.pack_into("!H", discovery, 120, 1)
                        termination = first.copy()
                        termination[112] = 0x40
                        for control in [sync, discovery, termination]:
                            control[22:38] = bytes([2] * 16)
                            sender.sendto(control, destination)
                    out, err = receiver.communicate("stop 1\n", timeout=5)
                    self.assertEqual(receiver.returncode, 0, err)
                    result = json.loads(out)
                    self.assertEqual(result["complete_frames"], 0)
                    self.assertEqual(result["incomplete_frames"], 1)
                    self.assertEqual(result["invalid_packets"], 4)
                    self.assertEqual(result["sync_packets"], 0)
                finally:
                    if receiver.poll() is None:
                        receiver.kill()
                        receiver.communicate()

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
