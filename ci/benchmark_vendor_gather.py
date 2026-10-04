import hashlib
import json
import random
import sys
import time
from pathlib import Path

import numpy as np

from ledfx_senders import _native
from ledfx_senders.encoders import RGBGather

print(
    json.dumps(
        {
            "python": sys.executable,
            "python_sha256": hashlib.sha256(
                Path(sys.executable).read_bytes()
            ).hexdigest(),
            "native": _native.__file__,
            "native_sha256": hashlib.sha256(
                Path(_native.__file__).read_bytes()
            ).hexdigest(),
            "numpy": np.__version__,
            "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        }
    ),
    flush=True,
)
rng = random.Random(1010)
for count in (64, 1024, 50000):
    permutation = np.random.default_rng(1010).permutation(count)
    encoder = RGBGather(tuple(map(int, permutation)))
    for dtype in (np.uint8, np.float32, np.float64):
        frame = (np.arange(count * 3).reshape(count, 3) % 256).astype(dtype)
        reference = frame.astype(np.uint8)[permutation].tobytes()
        assert encoder.encode(frame) == reference
        old = lambda frame=frame, permutation=permutation: frame.astype(np.uint8)[
            permutation
        ].tobytes()
        new = lambda encoder=encoder, frame=frame: encoder.encode(frame)
        for trial in range(7):
            cases = [("numpy", old), ("native", new)]
            rng.shuffle(cases)
            loops = 10000 if count == 64 else 1000
            for name, call in cases:
                begin = time.perf_counter_ns()
                for _ in range(loops):
                    call()
                print(
                    json.dumps(
                        {
                            "pixels": count,
                            "dtype": np.dtype(dtype).name,
                            "trial": trial,
                            "route": name,
                            "ns": (time.perf_counter_ns() - begin) / loops,
                            "loops": loops,
                        }
                    ),
                    flush=True,
                )
