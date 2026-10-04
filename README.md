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

For development, run `uv sync --group dev` in this repository. For an
application-wheel test before the first library release, install a sender wheel
from the reviewed commit before installing the application artifact.
CI currently builds and retains library artifacts; publication is a separate
release decision. No existing PyPI release is assumed.

Source builds require Rust 1.94.0 (pinned in `rust-toolchain.toml`) and the host C
linker/toolchain. Installing a compatible wheel needs no Rust. From the repository:

```sh
rustup toolchain install 1.94.0 --profile minimal
uv build
uv sync --group dev
uv run --group dev pytest tests
```

The package's sdist includes its Rust sources, locked Cargo dependencies, build
hook, independent protocol fixtures, tests, and CI smoke script. It builds outside
the LedFx checkout. `ci/native_wheel_smoke.py` runs with isolated Python against
an installed wheel and exercises only loopback network interfaces.

## Repository automation

The independent CI builds 35 portable wheel combinations: seven CPython variants
(3.11–3.15, 3.14t and 3.15t) across Linux x86_64/ARM64, Windows AMD64, and macOS
Intel/ARM64. Installed tests require the native engine, block LedFx imports and
restrict network traffic to loopback. Free-threaded variants assert that NumPy
and the binding leave the GIL disabled, including concurrent sender operations.
The source distribution is rebuilt and tested outside a source checkout.
Hosted results remain pending until CI runs; matrix configuration is not proof
that a platform passed.

Strict Pyrefly checks all Python source, native stubs and tests without a baseline
or suppressions: `uv run --frozen --only-group dev --python 3.12 python ci/check_types.py`.
Its source search paths also include the independent test oracle. NumPy is in the
dev group so this gate resolves its types without building the native library.
Installed runtime jobs create their environments and run under the runner temp
directory, while referring to scripts and fixtures by absolute checkout paths.

Renovate inherits `LedFx/renovate-config`; versions are locked for development.
Release Please maintains Python, Rust and lockfile versions together. Release
workflows are disabled unless `ENABLE_RELEASE_AUTOMATION=true`. Before enabling
it, maintainers must configure the org automation App, the protected `production`
environment and PyPI trusted publishing for `publish.yml`. Publishing also needs
an explicit tag dispatch with a successful CI run from that exact commit. No
initial PyPI release exists or is assumed. [Source provenance](PROVENANCE.md)
records the original repository and benchmark evidence.

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
`[-2**31, 2**31)` toward zero then takes modulo256; finite floating values outside
that interval produce zero. Integer inputs use exact modulo256. This policy is
identical across input formats and supported processors, preserving original
precision at the cutoff (including longdouble). **Compatibility correction:**
historical NumPy float-to-uint8 casts outside the byte range had platform-dependent
undefined results; these are now deterministic, not promised to match those
historical casts. Normal0–255 channel conversion remains unchanged. Nonfinite values are rejected
before packet or sequence mutation. Inputs are copied into owned native storage
before detached work; callers must not mutate a frame during its input copy.
Close is serialized with send and is idempotent. A closed sender rejects sends.

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
Its shared-runner measurements are conversion-only and exclude the owning
snapshot, packet packing and Python boundary; they are not network throughput.
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
then AVX2, with portable compiler code on older CPUs. Original f64 delta masks
use measured AVX2 or compiler code; the mask preserves three-channel pixel
boundaries and signed-zero equality. Realtime reuses the existing DDP SIMD
policies. No global host CPU build flags are used. Native Linux AArch64 paired measurements select NEON OSC conversion;
Apple Silicon retains its faster compiler route. A NEON original-mask candidate
is measured independently before production selection. Other dtypes and mixed/extended precision have exact
portable implementations. Hosted SIMD jobs test and measure candidates without
silently enabling an unmeasured architecture path.
