# Native ARM OSC routing

Actual native hosted run37206750275 on039e0ad passed all five ISA jobs and
independent scalar/candidate correctness tests. Raw artifact members, run.json,
local delta checks and lossless hashes are in evidence.tar.gz/manifest.json.

At150000f64 channels, three paired two-second kernel repeats give:
Linux ARM compiler264.89us, explicit NEON222.20us (1.19x); Apple Silicon
compiler75.65us, explicit NEON111.46us (0.68x). Production now selects NEON only
on Linux AArch64; Apple keeps the measured faster compiler route. No x86 hot
code changed. These are kernel results, not end-to-end application claims.

A bounded NEON delta-mask candidate uses vld3q to compare two original RGB
triples, including signed-zero equality; exact scalar equivalence covers all
lane/offset/tail combinations. It remains test-only pending actual paired
native hardware measurements. The workflow now emits ARM assembly to establish
whether the retained compiler route uses SIMD rather than assuming it does.

Local delta checks: optimized original-value and OSC equivalence suites pass;
Clippy all targets with warnings denied and actual tracked8hooks pass. Hosted
native execution of the new mask candidate and assembly inspection are pending
controller publication of this coherent delta.
