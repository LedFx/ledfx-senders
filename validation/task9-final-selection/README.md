# Final OSC numeric selection and ARM evidence

This checkpoint carries measured Linux ARM OSC NEON, the faster Apple compiler
OSC route, compiler ARM masks, guarded x86 AVX/AVX512 OSC conversion and AVX2 RGB
masks. No global target-cpu flags or new sender thread are used.

Actual Apple assembly from hosted39abe run37208087044 maps osc_numeric.rs:5–6
to fcvtzs.2d, scvtf.2d, fdiv.2d and fcvtn/fcvtn2. The compiler mask loop instead
uses scalar fcmp and early exits. Explicit NEON masks were rejected: Linux lost
across all patterns; Apple static/sparse improved but dense regressed. Full native
assembly, mapped excerpts, all five hosted artifacts and successful35-runtime
plus sdist CI receipts are preserved. The earlier failed Mac assembly collection
attempt is retained; rlib-only emission corrected extension-linking requirements.

The pinned Rust1.94 rust-src audit verifies all13 four-lane conversion intrinsics
require AVX or SSE. Runtime AVX checking therefore replaces the overly narrow
AVX2 guard for this OSC kernel only. Forced lane/tail/boundary tests pass; generated
256-bit instructions are AVX floating operations, with128-bit integer AVX forms.
The AVX512 source body and normalized emitted instructions remain unchanged.
Three two-second150k-channel repeats: prior AVX2 median30.98us, AVX30.66us,
scalar125.31us. These are kernel results, not application throughput.

Owned, initialized OSC snapshots of the same common dtype can suppress before
normalization when exact original values equal the last successful snapshot.
Shape/export and closed-state checks precede this branch. Prior successful
validation proves those identical typed values are valid; NaN/infinity or invalid
integers cannot equal that snapshot. Rare/cross-dtype and realtime validation
order remains unchanged. Tests cover signed zero, invalid input after valid and
failed sends, malformed rare zero tokens, failed realtime scratch/keepalive,
owned snapshots and closed identical frames.

Bounded explanatory alternating3x2s whole-send controls at50k pixels show static
OSC65.12→41.33us and dense262.92→252.44us. Historical static~24us remains faster;
the remaining~17us/call includes owned copying/equality and is~0.10% of a60FPS
frame budget. No blanket improvement claim. Final primary comparisons remain
consumer-side3x8s paired measurements and are not replaced by these short trials.

Validation:393 regular and393 free-threaded independently installed tests,
29 optimized Rust tests (four manual benchmarks ignored), Clippy all targets with
warnings denied, and all8 actual tracked hooks. The first hook rebuild hit
sandbox DNS; the identical locked hook command succeeded using cached dependencies
with UV_OFFLINE=1. No fallback implementation or weakened type gate was used.

The manifest hashes every archive member, measured binaries and loaded-source
receipts. Measured candidate extension bytes are preserved from the isolated
installed environment; final wheels were rebuilt after formatting. Kernel source
is reconstructible from39abe source plus the saved baseline-test and production
candidate patches; later extra assertions/formatting do not change kernel code.
