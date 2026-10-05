"""Matched public facade calls in separate processes, with frozen wheel inputs."""

import argparse
import hashlib
import json
import random
import subprocess
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import NotRequired, TypedDict, cast


class Case(TypedDict):
    protocol: str
    pixels: int
    dtype: str
    order: NotRequired[str]
    group: NotRequired[int]
    white: NotRequired[str]
    check_only: NotRequired[bool]


def decode_case(line: str) -> Case:
    value: object = json.loads(line)
    if not isinstance(value, dict):
        raise TypeError("Expected a benchmark case object")
    for key, expected in (("protocol", str), ("pixels", int), ("dtype", str)):
        if not isinstance(value.get(key), expected):
            raise TypeError(f"Invalid benchmark case field: {key}")
    for key, expected in (
        ("order", str),
        ("group", int),
        ("white", str),
        ("check_only", bool),
    ):
        if key in value and not isinstance(value[key], expected):
            raise TypeError(f"Invalid benchmark case field: {key}")
    return cast(Case, value)


def digest(path: str | Path) -> str:
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def worker(package: str) -> None:
    sys.path.insert(0, str(Path(package).resolve()))
    import numpy as np

    import ledfx_senders
    from ledfx_senders import _native, encoders

    # The pinned historical control predates protocol modules and root exports.
    from ledfx_senders.packet_senders import ArtNetSender

    assert ledfx_senders.__file__ is not None
    package_root = Path(ledfx_senders.__file__).resolve().parent
    print(
        json.dumps(
            {
                "python": sys.executable,
                "python_sha256": digest(sys.executable),
                "numpy": np.__version__,
                "script_sha256": digest(__file__),
                "loaded": {
                    p: digest(p)
                    for p in (
                        _native.__file__,
                        *(str(source) for source in sorted(package_root.rglob("*.py"))),
                    )
                },
            }
        ),
        flush=True,
    )
    for line in sys.stdin:
        case = decode_case(line)
        pixels = case["pixels"]
        frame = (np.arange(pixels * 3).reshape(pixels, 3) % 256).astype(case["dtype"])
        sender: ArtNetSender | None = None
        call: Callable[[], bytes | None]
        captured_hash = None
        if case["protocol"] == "adalight":
            order = case["order"]
            call = lambda frame=frame, order=order: encoders.encode_adalight(
                frame, order
            )
        elif case["protocol"] == "openrgb":
            call = lambda frame=frame: encoders.encode_openrgb(frame, 0)
        else:

            def make_sender(
                mode: str, *, pixels: int = pixels, case: Case = case
            ) -> ArtNetSender:
                return ArtNetSender._test_sender(
                    destination="127.0.0.1",
                    port=6454,
                    universe=0,
                    packet_size=512,
                    even_packet_size=True,
                    dmx_start_address=9,
                    pixel_count=pixels,
                    pixels_per_device=case["group"],
                    pre_amble=b"\xff\x80",
                    post_amble=b"\x40",
                    rgb_order="BRG",
                    white_mode=case["white"],
                    broadcast=False,
                    mode=mode,
                )

            capture = make_sender("capture")
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
            sender = make_sender("discard")
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


def run(control: str, candidate: str, check_only: bool = False) -> None:
    processes: dict[str, subprocess.Popen[str]] = {}
    try:
        for label, package in (("control", control), ("candidate", candidate)):
            process = subprocess.Popen(
                [sys.executable, __file__, "--worker", package],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                text=True,
            )
            assert process.stdout is not None
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
        cases: list[Case] = []
        for protocol in ("adalight", "openrgb"):
            for pixels in (1, 30, 127, 128, 129, 170, 1024, 50000):
                for dtype in ("uint8", "float64"):
                    cases.append(
                        {
                            "protocol": protocol,
                            "pixels": pixels,
                            "dtype": dtype,
                            **({"order": "BRG"} if protocol == "adalight" else {}),
                        }
                    )
        # Actual default order: retain BRG controls and add only four RGB cases.
        for pixels in (170, 50000):
            for dtype in ("uint8", "float64"):
                cases.append(
                    {
                        "protocol": "adalight",
                        "pixels": pixels,
                        "dtype": dtype,
                        "order": "RGB",
                    }
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
                    assert process.stdin is not None and process.stdout is not None
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
