# Source provenance

The initial commit is the byte-for-byte `packages/ledfx-senders` subtree of
[LedFx/LedFx c5c88522](https://github.com/LedFx/LedFx/commit/c5c88522).
It contains the independently reviewed Task 6 E1.31 library, tests and fixtures.
DDP/OPC work in progress was excluded from that historical extraction baseline.
The current repository subsequently added DDP/OPC, Art-Net, stateful senders,
Nanoleaf and vendor encoders as independently reviewed changes.

The original performance measurements and raw evidence remain in
[LedFx tools/benchmarks/native-e131](https://github.com/LedFx/LedFx/tree/c5c88522/tools/benchmarks/native-e131).
That link is an immutable historical commit, not a current application packaging
or evidence-storage requirement. Those manifests identify the actual pre-extraction binaries and source hashes;
repository extraction is not a new performance measurement or kernel change.
