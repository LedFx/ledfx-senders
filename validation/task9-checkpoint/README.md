# OSC/realtime implementation checkpoint

Base: `cd180015e2492592be8e0451343b139bd55b0371`. This checkpoint adds
stateful senders, exact numeric snapshots, selected x86 f64 kernels, independent
OSC/realtime receive parsing, E1.31 malformed-control validation and hosted
candidate measurements. It is not final Task9 acceptance or an end-to-end
performance claim. Application integration and primary measurements continue in
the separate LedFx repository; immutable consumer pins are controller-managed.

`evidence.tar.gz` retains local successful and failed raw logs, test-first RED,
compiler/CPU data and disassembly. `manifest.json` hashes every archive member,
source file and installed wheel. Early exploratory kernel logs identify input
sizes/iterations, compiler, selected binary path and test code, but exact
pre-experiment source snapshots were not captured before later edits. Do not
attribute those exploratory numbers to the final source hash. Final paired
application trials record actual loaded source/binary/input hashes per trial.

Validation commands (run with Rust1.94 CARGO_HOME=/tmp/ledfx-rust/cargo,
RUSTUP_HOME=/tmp/ledfx-rust/rustup and corresponding bin PATH):

- `uv build --offline --wheel --out-dir /tmp/ledfx-task9-checkpoint-dist`;
  free-threaded build adds `--python /tmp/ledfx-portable-fixed-ft-env/bin/python`.
- From `/tmp`: `/tmp/ledfx-task9-env/bin/python -I -m pytest --noconftest
  /tmp/ledfx-senders/tests -q`: 393 passed.
- From `/tmp`: `PYTHON_GIL=0 /tmp/ledfx-task9-ft-env/bin/python -I -X gil=0
  -m pytest --noconftest /tmp/ledfx-senders/tests -q`: 393 passed.
- `cargo test --manifest-path native/Cargo.toml --locked`: native and independent
  receiver tests pass; manual measurement tests remain intentionally ignored.
- `cargo clippy --manifest-path native/Cargo.toml --locked --all-targets --
  -D warnings`: passed. Rustfmt passed.
- `uv run --offline --frozen --only-group dev prek run --all-files` after staging
  every new source/test: all eight actual tracked hooks passed, including strict
  source/stub/test types and workflow security lint. No new type allowances.
- `/tmp/ledfx-task9-env/bin/python native/bench/receiver/test_receiver.py`:
  three test methods passed, including twenty exact OSC/realtime combinations
  over portable/batched receive backends.

Kernel measurements use three separate two-second repeats at150000f64 channels,
without simultaneous CPU work. Portable OSC domain conversion about120us,
AVX2about31us, AVX512about20us. Original f64 masks: compiler about36us for
static/sparse, AVX2about21us; dense roughly22us for both. Guarded wider conversion
and AVX2 masks are selected on this host. A proposed compiler truncation rewrite
was slower and remains test-only. These are isolated kernel timings, not send
or application throughput. Explicit NEON conversion is test-only pending paired
native hosted evidence; SSE2/ARM compiler paths remain supported and exact.

The standalone receiver executable and paired application preflight retain
accepted-but-unreceived observations, including a raw1500-byte UDP frame on this
host. They do not prove a lossless capacity or diagnose a kernel overflow. The
full independent literal receiver tests use bounded receipts; throughput trials
must separately report acceptance, completeness, loss and suppression.
