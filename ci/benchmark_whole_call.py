"""Matched public facade calls in separate processes, with frozen wheel inputs."""

import argparse
import hashlib
import json
import random
import subprocess
import sys
import time
from pathlib import Path


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def worker(package):
    sys.path.insert(0, str(Path(package).resolve()))
    import numpy as np

    from ledfx_senders import _native, encoders, packet_senders
    from ledfx_senders.packet_senders import ArtNetSender

    print(
        json.dumps(
            {
                "python": sys.executable,
                "python_sha256": digest(sys.executable),
                "numpy": np.__version__,
                "script_sha256": digest(__file__),
                "loaded": {
                    str(p): digest(p)
                    for p in (
                        _native.__file__,
                        encoders.__file__,
                        packet_senders.__file__,
                    )
                },
            }
        ),
        flush=True,
    )
    for line in sys.stdin:
        case = json.loads(line)
        pixels = case["pixels"]
        frame = (np.arange(pixels * 3).reshape(pixels, 3) % 256).astype(case["dtype"])
        sender = None
        captured_hash = None
        if case["protocol"] == "adalight":
            call = lambda frame=frame: encoders.encode_adalight(frame, "BRG")
        elif case["protocol"] == "openrgb":
            call = lambda frame=frame: encoders.encode_openrgb(frame, 0)
        else:
            settings = {
                "destination": "127.0.0.1",
                "port": 6454,
                "universe": 0,
                "packet_size": 512,
                "even_packet_size": True,
                "dmx_start_address": 9,
                "pixel_count": pixels,
                "pixels_per_device": case["group"],
                "pre_amble": b"\xff\x80",
                "post_amble": b"\x40",
                "rgb_order": "BRG",
                "white_mode": case["white"],
                "broadcast": False,
            }
            capture = ArtNetSender._test_sender(**settings, mode="capture")
            try:
                capture.send(frame)
                packets = capture._engine.captures()
                assert packets
                packet_digest = hashlib.sha256()
                for packet, _address in packets:
                    packet_digest.update(len(packet).to_bytes(4, "big"))
                    packet_digest.update(packet)
                captured_hash = packet_digest.hexdigest()
            finally:
                capture.close()
            sender = ArtNetSender._test_sender(**settings, mode="discard")
            call = lambda sender=sender, frame=frame: sender.send(frame)
        try:
            result = call()
            loops = elapsed = warmup = 0
            calibration = []
            if not case.get("check_only"):
                warmup = 100
                for _ in range(warmup):
                    call()
                calibration_loops = 64
                for _attempt in range(8):
                    begin = time.perf_counter_ns()
                    for _ in range(calibration_loops):
                        call()
                    calibration_ns = time.perf_counter_ns() - begin
                    calibration.append(
                        {"loops": calibration_loops, "elapsed_ns": calibration_ns}
                    )
                    if calibration_ns >= 10_000_000 or calibration_loops == 1_000_000:
                        break
                    calibration_loops = min(calibration_loops * 4, 1_000_000)
                loops = max(
                    1,
                    min(
                        1_000_000,
                        int(150_000_000 * calibration_loops / max(1, calibration_ns)),
                    ),
                )
                begin = time.perf_counter_ns()
                for _ in range(loops):
                    call()
                elapsed = time.perf_counter_ns() - begin
            print(
                json.dumps(
                    {
                        "ns": elapsed / loops if loops else None,
                        "loops": loops,
                        "elapsed_ns": elapsed,
                        "warmup_calls": warmup,
                        "calibration": calibration,
                        "target_ns": 150_000_000,
                        "loop_cap": 1_000_000,
                        "encoded_sha256": hashlib.sha256(result).hexdigest()
                        if result is not None
                        else captured_hash,
                    }
                ),
                flush=True,
            )
        finally:
            if sender:
                sender.close()


def run(control, candidate, check_only=False):
    processes = {}
    try:
        for label, package in (("control", control), ("candidate", candidate)):
            process = subprocess.Popen(
                [sys.executable, __file__, "--worker", package],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                text=True,
            )
            processes[label] = process
            print(
                json.dumps(
                    {
                        "route": label,
                        "provenance": json.loads(process.stdout.readline()),
                    }
                ),
                flush=True,
            )
        cases = []
        for protocol in ("adalight", "openrgb"):
            for pixels in (1, 30, 127, 128, 129, 170, 1024, 50000):
                for dtype in ("uint8", "float64"):
                    cases.append(
                        {"protocol": protocol, "pixels": pixels, "dtype": dtype}
                    )
        for pixels in (170, 50000):
            for white in ("None", "Accurate"):
                for group in (0, 1, 3, 7, 8, 9, 10, 11, 16, 64, 170):
                    cases.append(
                        {
                            "protocol": "artnet",
                            "pixels": pixels,
                            "dtype": "uint8",
                            "group": group,
                            "white": white,
                        }
                    )
        rng = random.Random(1010)
        for trial in range(1 if check_only else 7):
            rng.shuffle(cases)
            for case in cases:
                case["check_only"] = check_only
                labels = list(processes)
                rng.shuffle(labels)
                hashes = []
                for label in labels:
                    process = processes[label]
                    process.stdin.write(json.dumps(case) + "\n")
                    process.stdin.flush()
                    result = json.loads(process.stdout.readline())
                    assert result["encoded_sha256"] is not None, case
                    hashes.append(result["encoded_sha256"])
                    print(
                        json.dumps({**case, "trial": trial, "route": label, **result}),
                        flush=True,
                    )
                assert hashes[0] == hashes[1], case
    finally:
        for process in processes.values():
            if process.stdin:
                process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        if any(p.returncode for p in processes.values()):
            raise RuntimeError("whole-call worker failed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker")
    parser.add_argument("--control")
    parser.add_argument("--candidate")
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()
    if args.worker:
        worker(args.worker)
    elif args.control and args.candidate:
        run(args.control, args.candidate, args.check_only)
    else:
        parser.error("provide --control and --candidate extracted wheel paths")
