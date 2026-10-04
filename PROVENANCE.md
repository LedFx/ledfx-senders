# Source provenance

The initial commit is the byte-for-byte `packages/ledfx-senders` subtree of
[LedFx/LedFx c5c88522](https://github.com/LedFx/LedFx/commit/c5c88522).
It contains the independently reviewed Task 6 E1.31 library, tests and fixtures.
Task 7 DDP/OPC work in progress was excluded from this baseline and is preserved
separately by the LedFx controller for migration after extraction review.

The original performance measurements and raw evidence remain in
[LedFx tools/benchmarks/native-e131](https://github.com/LedFx/LedFx/tree/c5c88522/tools/benchmarks/native-e131).
Those manifests identify the actual pre-extraction binaries and source hashes;
repository extraction is not a new performance measurement or kernel change.
