# ledfx-senders

Native packet senders for Python applications, maintained by the LedFx organization.
The distribution contains its Rust engine, Python buffer normalization and E1.31
setup codec. It has no LedFx application imports or dependency. NumPy is its only
runtime dependency.

```python
from ledfx_senders.e131 import E131Sender
from ledfx_senders.e131_buffer import ChannelLayout

sender = E131Sender(ChannelLayout(300), destination="127.0.0.1", source_name="example")
try:
    sender.send(bytes(300))
finally:
    sender.close()
```

`ChannelLayout` counts channels, not RGB pixels. `send` owns a snapshot before
native conversion and submission. Callers must not mutate an input while its
snapshot is being taken. Call `service` regularly for idle refresh and discovery.
`close` is idempotent and sends protocol termination for used universes.

The wheel matrix targets CPython 3.11–3.15 and free-threaded 3.14t–3.15t.
Free-threaded 3.13t is unsupported by PyO3 0.29; its build guard is preserved.
See the [PyO3 changelog](https://pyo3.rs/v0.29.2/changelog.html) (#5865).
The build matrix verifies the library independently; it does not assert that the
LedFx application's other dependencies support every interpreter. The private
`_native` binding and `_test_*` methods are implementation and diagnostic APIs.

## Installation and development

Applications consume the separately published Python package. To require a
compatible wheel and prevent a fallback Rust source build:

```sh
python -m pip install --only-binary=ledfx-senders ledfx-senders
```

This command requires a published version with a wheel for your interpreter and
platform. Release configuration does not itself mean a release exists; check
[PyPI](https://pypi.org/project/ledfx-senders/) for availability. Application
repositories do not need Rust, a source checkout, or a build bootstrap script.

The supported wheel targets are Linux x86-64 and AArch64 (glibc), Windows x86-64,
and macOS Intel and ARM64. There are no Windows ARM64, 32-bit ARM/x86, or musl
wheel targets. A wheel must match both the Python ABI and platform; free-threaded
Python uses its separate `t` ABI. Wheels include `py.typed` and `_native.pyi` for
type checkers, and installed-wheel tests check that both files are distributed.

Contributors building this repository need Rust 1.94.0 (pinned in
`rust-toolchain.toml`) and a host C linker/toolchain. Builds use PDM's setuptools
hook with setuptools-rust and locked Cargo dependencies, not a workspace in the
LedFx application. From the repository:

```sh
rustup toolchain install 1.94.0 --profile minimal
uv sync --locked --group dev
uv run --locked --group dev python -m pytest tests ci
uv run --frozen --only-group dev --python 3.12 python ci/check_types.py
uv run --frozen --only-group dev ruff check .
uv run --frozen --only-group dev ruff format --check .
uv run --frozen --only-group dev prek run --all-files
uv build
```

The sdist contains the Rust sources, locked Cargo dependencies, build hook,
independent protocol fixtures, tests and CI scripts. It builds outside a LedFx
checkout. `ci/native_wheel_smoke.py` runs with isolated Python against an installed
wheel and restricts network tests to loopback. Benchmark scripts are development
tools, not installed public APIs; their measurements are not release gates.

## Repository automation

The independent CI builds 35 portable wheel combinations: seven CPython variants
(3.11–3.15, 3.14t and 3.15t) across Linux x86_64/ARM64, Windows AMD64, and macOS
Intel/ARM64. Installed tests require the native engine, block LedFx imports and
restrict network traffic to loopback. Free-threaded variants assert that NumPy
and the binding leave the GIL disabled, including concurrent sender operations.
The source distribution is rebuilt and tested outside a source checkout.
The [CI run for the release-workflow checkpoint](https://github.com/LedFx/ledfx-senders/actions/runs/37240196477)
passed the complete portable matrix. Each new commit must pass its own applicable
checks; the workflow definition alone is not evidence that a build passed.

Strict Pyrefly checks every maintained Python file and stub, including package
source, tests, CI scripts, the PDM build hook and receiver checks, without a
baseline or suppressions: `uv run --frozen --only-group dev --python 3.12 python ci/check_types.py`.
Recursive project globs include future directories; generated, hidden and virtual
environment directories use the checker defaults. Ruff enables the complete ANN
rule family to require parameter and return annotations and reject explicit Any.
A regression inserts untyped files across those directories and verifies that
both gates reject them. These are static-checking guarantees, not a measured
percentage of expression-level type coverage. NumPy and setuptools-rust are in
the dev group so the gate resolves array and build-hook types without compiling
the native library.
Installed runtime jobs create their environments and run under the runner temp
directory, while referring to scripts and fixtures by absolute checkout paths.

Renovate inherits `LedFx/renovate-config`; versions are locked for development.
Release Please maintains Python, Rust and lockfile versions together on canonical
`main`, using the existing automation App to create the release PR, version tag
and draft release. Merging its release PR is the release action: the tag push
runs `.github/workflows/ci.yml`. An early guard requires the tag, project,
Python package, Rust package, lockfiles and release manifest versions to agree.
All five platform builds, 35 installed-wheel variants and source/native checks
must pass before publishing the same run's `sender-dist` artifact to PyPI.
One queued publication job calls the SHA-pinned
[shared release transaction](https://github.com/LedFx/release-ci), using the exact
35-wheel/sdist policy in `.github/release-policy.json`. It validates archive
metadata, tag/source identity and matching remote hashes before upload, verifies
GitHub provenance against this repository's `ci.yml`, then finalizes the existing
draft by immutable ID last. Publication never rebuilds packages. PR, branch and
manual validation cannot publish, even when manually validating a tag.

PyPI trusted publisher identity remains owner `LedFx`, repository `ledfx-senders`,
workflow `ci.yml`, environment `pypi`. OIDC `id-token: write` is scoped to that
caller job; the shared composite does not move publication into another workflow.
The scoped automation App token permits GitHub contents/attestation writes.
No package API token or hidden enable variable is required.

Matching partial uploads can resume from the original run after SHA-256 checks;
conflicts, missing drafts and unexpected assets fail without clobber or fallback
release creation. Higher stable drafts/public releases block latest promotion;
an abandoned newer draft can delay latest while older version publication remains
possible. Retained snapshots and attestation bundles support inspection/retry.
Use the [shared recovery guide](https://github.com/LedFx/release-ci#failure-and-recovery)
and rerun failed jobs from the original run rather than rebuilding a version.
This migration does not retroactively attest earlier releases.
[Source provenance](PROVENANCE.md) records the original repository and benchmark
evidence.

macOS wheel deployment targets account for both Rust and Python: Intel starts at
10.12 for CPython 3.11, 10.13 for 3.12–3.13, and 10.15 for 3.14+; ARM64 starts at
11.0. These are build targets, not claims that every optional application or
NumPy version runs on every older OS. Cibuildwheel repair and installed tests
must pass before a platform is considered validated.

The private `_TestLockGate` and `Engine._test_hold_lock` diagnostics let tests
hold the real engine mutex with a five-second native deadline. Python controls
a separate release signal; send/service/close still use their normal lock
acquisition paths. This verifies GIL cooperation under mutex contention, not a
throughput claim or a scheduler-dependent number of heartbeat ticks. Production
frame processing never consults the gate. The helper drops its guard before
reattaching to Python, including on timeout.

## DDP and OPC

```python
from ledfx_senders import DDPSender, OPCSender

sender = DDPSender(300, destination="192.0.2.10", destination_id=1)
sender.send(bytes(300))
sender.close()

opc = OPCSender(100, destination="192.0.2.10", channel=0)
opc.send(bytes(300))
opc.close()
```

Each sender owns its UDP socket and synchronously sends a complete frame under a
single 200 ms socket deadline. DDP uses 1440-byte payload chunks, a final PUSH,
and sequences starting at 2 and wrapping through 1–15. OPC sends one datagram;
its maximum is 21,834 RGB pixels (65,506 bytes including the header).

Both accept unsigned-byte buffers and NumPy arrays, including strided arrays.
Byte buffers are channel bytes, not pixels. OPC arrays must have shape
`(pixel_count, 3)`; finite values clamp to 0–255 and truncate. DDP truncates finite floating values in
`[-2**31, 2**31)` toward zero then takes modulo 256; finite floating values outside
that interval produce zero. Integer inputs use exact modulo 256. This policy is
identical across input formats and supported processors, preserving original
precision at the cutoff (including longdouble). **Compatibility correction:**
historical NumPy float-to-uint8 casts outside the byte range had platform-dependent
undefined results; these are now deterministic, not promised to match those
historical casts. Normal 0–255 channel conversion remains unchanged. Nonfinite values are rejected
before packet or sequence mutation. Inputs are copied into owned native storage
before detached work; callers must not mutate a frame during its input copy.
Close is serialized with send and is idempotent. A closed sender rejects sends. DDP advances
its sequence once per validated send attempt, including a socket failure before
any packet is accepted. Invalid input does not advance it, and a failed payload
does not replace the committed frame. Do not assume sequence rollback semantics
are identical across protocols.

Float conversion for DDP, E1.31 and OPC validates every value while using AVX512F/DQ/BW/VL or AVX2 on supported x86-64
CPUs, SSE2 on other x86-64 CPUs, or NEON on AArch64 (including Apple Silicon).
AArch64 uses measured policy/dtype choices: explicit NEON for DDP and E1.31
float64, compiler-vectorized loops for E1.31/OPC float32, and a compiler-optimized
OPC float64 loop with SIMD validation and scalar conversion.
Runtime feature checks guard each specialized kernel. Other targets retain a
portable scalar implementation; ARM32 is not a supported wheel target and has
no promised SIMD float64 path. The compile-time policy specializations preserve DDP wrapping, E1.31 strict
(-1,256) rejection, OPC clamp/truncate, nonfinite rejection and whole-frame atomicity. There is no public unchecked input option.

`Manual SIMD validation and measurements` runs native kernel equivalence tests
and matched scalar/SIMD conversion measurements on Linux x86-64/ARM64, macOS
Intel/ARM64 and Windows x86-64. It neither publishes nor releases anything.
It reports separate conversion-only kernels and public-facade whole-call pairs.
Kernel timings exclude the owning snapshot, packet packing and Python boundary;
whole-call timings include them. Neither is network or physical-device throughput.
See `native/bench/receiver/README.md` for independent delivered-frame measurement.

## Stateful OSC and UDP realtime

`OSCSender` supports `One_Argument`, `Three_Arguments`, `Three_Addresses` and
`All_To_One`, with Python `path.format(address=...)` expansion at construction
and `starting_addr`. Call `send(frame, now=time.monotonic())` and `close()`.
`UDPRealtimeSender` supports DRGB, WARLS, DRGBW, DNRGB, adaptive and raw RGB modes;
its `keepalive_interval` accepts the application's configured refresh threshold.
The default interval is half the timeout. Eligibility uses strict elapsed-time
comparison and all chunks of a DNRGB refresh are sent together.

Both senders own their snapshots and UDP sockets. Callers must not mutate input
during `send`. Input is an RGB ndarray of the configured shape or contiguous
unsigned byte buffer. There are no retained caller aliases or channel objects.
Unsuccessful submissions retain the previous successful suppression state;
accepted datagram counters still include the prefix accepted by the socket.
Every frame shares the existing 200 ms socket-work budget. Close sends no
protocol termination traffic and releases the socket synchronously.

Original values determine equality and WARLS deltas before quantization.
Equal numeric values across dtypes, including positive and negative zero,
compare equal. Extended floating precision is retained. Comparison is exact,
so `uint64(2**64-1)` differs from `float64(2**64)` even though NumPy's mixed
comparison may round the integer. This deliberately corrects lossy historical
suppression. First valid frames are always sent.

OSC uses a portable signed 64-bit domain: finite floating originals in
`[-2**63, 2**63)`, and integers within signed 64-bit bounds. It truncates toward
zero, converts the integer to float64, divides by 255, then writes big-endian
float32. It does not clip to RGB byte bounds. Nonfinite/out-of-domain values
raise `ValueError` before persistent wire buffers, counters or datagrams change.
This explicitly replaces platform-dependent historical NumPy integer casts.
Realtime encoding follows the documented deterministic DDP conversion contract;
that encoding does not change original-value comparisons.

OSC validates every formatted path and UDP's 65507-byte payload ceiling during
construction. Realtime limits remain DRGB490, WARLS255, DRGBW367 and raw500;
unsupported configured sizes fall back to DRGB or DNRGB. DNRGB admits at most
65536 pixels, with 489 pixels per chunk, and adaptive ties choose DRGB.

On x86_64, measured f64 OSC conversion uses runtime-guarded AVX512F/DQ/BW/VL,
then AVX, with portable compiler code on older CPUs. Original f64 delta masks
use measured AVX2 or compiler code; the mask preserves three-channel pixel
boundaries and signed-zero equality. Realtime reuses the existing DDP SIMD
policies. No global host CPU build flags are used. Native Linux AArch64 paired measurements select NEON OSC conversion;
Apple Silicon retains its faster compiler route. A NEON original-mask candidate
is measured independently before production selection. Other dtypes and mixed/extended precision have exact
portable implementations. Hosted SIMD jobs test and measure candidates without
silently enabling an unmeasured architecture path.

For OSC, an owned common typed snapshot equal to the last successfully validated
snapshot of the same dtype can suppress before repeating normalization. Shape
and closed-state checks still happen first. Rare and cross-dtype inputs retain
full validation before comparison; realtime refresh also retains conversion so
failed attempts cannot contaminate a later keepalive. Static OSC can still cost
more than the former borrowed-array equality check because snapshots are owned.

### Art-Net

`ledfx_senders.packet_senders.ArtNetSender` owns a synchronous IPv4 UDP socket
and caches the complete ArtDmx layout. Its keyword configuration is
`destination`, `port`, `universe`, `packet_size`, `even_packet_size`,
`dmx_start_address` (one based), `pixel_count`, `pixels_per_device`, `pre_amble`
and `post_amble` (bytes), `rgb_order`, `white_mode`, and `broadcast`.
`send(frame)` accepts the configured RGB shape or an unsigned-byte buffer.
All six RGB permutations and None/Zero/Brighter/Accurate white modes are supported.

The logical universe stride remains `packet_size` (1–512). Wire payloads have a
minimum of two bytes and round up to even when requested; added bytes are zero,
never borrowed from the next logical universe. This repairs the former size-1
and odd-plus-even cases that silently submitted mismatched buffers to the legacy
library. Odd payload lengths with `even_packet_size=False` and the original
0–255 packet sequence cycle remain compatibility modes, not a claim of strict
Art-Net conformance. Addresses cover the full 15-bit range; the entire span must
fit. Start offset precedes the whole layout, ambles repeat per complete device
group, and an incomplete final group is omitted. All input channels are validated.

White extraction/subtraction precedes byte conversion at original precision.
Integers preserve exact minimum and original-width subtraction's low byte,
including signed overflow. Floats truncate modulo 256 in `[-2**31, 2**31)`;
other finite values become zero. Nonfinite input or arithmetic output rejects
atomically. This defines previously platform-dependent out-of-range NumPy casts.
Bool Accurate subtraction raises TypeError. Common formats use native arithmetic;
float16 and longdouble use bulk NumPy arithmetic at original precision followed
by deterministic byte normalization. Byte order conversion preserves float width.

A send has one 200ms transport deadline. Accepted packets advance sequence;
unsent packets do not, and only a complete frame replaces committed state.
`close(blackout=True)` waits for any current frame, then uses a separate 200ms
cleanup budget to clear **every** configured universe, including ambles and
padding. It always closes after partial/failed cleanup; diagnostics are retained.
Repeated close is harmless and subsequent sends fail. Waiting for an in-flight
send plus cleanup can therefore exceed 200ms. `close(False)` omits blackout.

## Vendor encoders and Nanoleaf

`ledfx_senders.encoders` provides `encode_adalight(frame, color_order)`,
`encode_openrgb(frame, device_id)`, `encode_hue(frame, entertainment_id,
channel_ids, sequence)`, and `encode_govee(frame, stretch)`. Each returns owned
`bytes`; the caller keeps its serial, OpenRGB v3 TCP, Hue DTLS or Govee shared
socket session. `RGBGather(permutation).encode(frame)` caches an immutable RGB
permutation for SDK-owned streaming; it does not implement SDK transport.

For example, encoding bytes does not open or negotiate a device session:

```python
from ledfx_senders.encoders import RGBGather, encode_adalight, encode_openrgb

frame = bytes([255, 0, 0, 0, 128, 255])  # two RGB pixels
serial_packet = encode_adalight(frame, "RGB")
openrgb_packet = encode_openrgb(frame, device_id=0)
reordered_rgb = RGBGather((1, 0)).encode(frame)
```

Callers negotiate OpenRGB protocol v3 and write the result to their own session.
All encoders take an owned snapshot; callers must not mutate input while the
snapshot is taken. Hue accepts 1..256 channels and RGBGather accepts 1..1,000,000
pixels with a complete immutable permutation.

Inputs are RGB ndarrays of shape `(N, 3)` or contiguous unsigned byte buffers.
Strided arrays are normalized; complex/object/structured arrays are rejected.
Adalight, OpenRGB, Govee and RGBGather preserve integer low bytes. Finite floats
in `[-2**31, 2**31)` truncate toward zero then wrap modulo 256; finite values
outside this domain become zero. This explicit exceptional-float policy replaces
platform-dependent NumPy casts. Nonfinite values fail before output. Hue instead
requires integers 0..255 or floats strictly between -1 and 256. Its first
nonfinite channel retains Python `int` exception precedence: NaN -> ValueError,
infinity -> OverflowError, even after an earlier finite out-of-range channel.
Hue IDs are a 36-character ASCII UUID and unique u8 channel IDs. The application
keeps its existing sequence policy; this encoder accepts any u8 sequence.

Adalight supports 1..65536 pixels and writes N-1 in its count field, as decoded by
[Adafruit's original receiver](https://github.com/adafruit/Adalight/blob/b9d88f8a05e5a3099e9b855cea88b3b29351652c/Arduino/LEDstream/LEDstream.pde#L183-L189).
This corrects LedFx's former N header. OpenRGB supports 1..65535 pixels and u32
device IDs. Explicit little-endian UPDATELEDS fields match the current v3 session
and supported little-endian fleet; upstream native memcpy does not establish a
cross-endian protocol guarantee. Govee supports 1..255 segments with LedFx's
existing reverse-engineered BB00FAB0/XOR/base64/JSON representation. Encode speed
is not serial baud rate, encrypted-session throughput or physical-device FPS.

`ledfx_senders.nanoleaf.NanoleafSender(destination=..., port=..., version=...,
panel_ids=(...))` owns UDP output and provides `send(frame)` and `close()`.
Panel IDs must be unique and frame size must match the immutable layout.
V1 allows 1..255 panels with u8 IDs; v2 allows 1..8188 with u16 IDs, limited by
the 65507-byte IPv4 UDP payload ceiling. Values clamp to 0..255 and truncate;
nonfinite input rejects atomically. Large finite values clamp rather than relying
on a signed-integer cast overflowing. V1 emits the documented nFrames=1 byte,
correcting LedFx's former zero, with white=0 and transition=1. V2 retains
white=0 and transition=0. REST activation and HTTP animation remain with callers.
The [manufacturer's API documentation](https://nanoleaf.atlassian.net/wiki/spaces/nlapid/pages/2789310530/Nanoleaf+Light+Panels+Open+API+Documentation)
(version 8, 2026-07-08) recommends streaming no faster than 10 Hz; benchmark CPU
capacity is not a firmware or hardware-rate claim. Large datagrams are tested
for encoding bounds separately from small loopback delivery.

The encoders reuse the validated float-conversion dispatch. Accepted portable
packing routes use preallocated output at 128+ pixels for non-identity Adalight
orders on tested Linux/Windows/macOS x86-64 and Linux/macOS ARM64, and for OpenRGB
except macOS x86-64. Adalight's RGB order uses a generic identity copy, avoiding
an unnecessary permutation. Art-Net caches a short-copy route for groups of at
most 32 channel bytes on those x86-64 platforms. Ordinary/long spans, ARM64
Art-Net, small vendor frames and unmeasured architectures retain reference
packing. These are compiler loops, not new explicit ISA intrinsics.

The [calibrated five-platform whole-call run](https://github.com/LedFx/ledfx-senders/actions/runs/37236053291)
retained paired distributions and unchanged controls. It supports those
conservative categories, not an optimal 128-pixel crossover or a win in every
cell. Small differences within shared-runner variation remain inconclusive;
losing and near-equal observations are retained. Hosted Adalight permutation
measurements used BRG; subsequent RGB cases are labeled separately. No 32-bit
ARM execution or performance validation is claimed.

Owning snapshots, shape/numeric validation and conversion have a cost. A brief
matched LedFx default-RGB check at 50,000 pixels found uint8 near parity after
the identity-copy repair, but float64 throughput remained about 0.38 times the
historical NumPy encoder. This is a retained regression, not a sustained
whole-application result. The compared contracts differ in validation and
ownership; individual cost shares were not isolated. No blanket speedup is
claimed for every dtype, frame size or caller.

The benchmark-only `ledfx-receiver ddp-structural <pixels> <portable|batched>
<loopback-address>` receives arbitrary RGB effect data without modifying it.
Its stdin accepts `snapshot` and `stop`; each stdout JSON snapshot contains
cumulative packet/byte/push/structurally-complete/invalid counters, monotonic
elapsed seconds, CPU time where available, and kernel socket drops where
available. Rates use the difference between the receiver's two snapshot times.
Assembly state persists across snapshots, so a completion can include a frame
started just before the interval. The 4-bit DDP sequence cannot establish unique
frame identity across wraps or reordered traffic. Incomplete-assembly and gap
counts are events, not uniquely identified lost frames. Final stop counts any
remaining pending assembly once as an incomplete event. The original golden
fixture receiver remains available for exact byte/identity validation.

Hosted route measurements freeze both wheels, measured Rust executables, loaded
facades/extension, source archives and SHA-256 manifests before measurement and
verify them afterward. Public-facade whole-call pairs compare an immutable
12d9030 control with this candidate, including tiny/cutover/ordinary layout
controls; these are distinct from sustained application transport measurements.
