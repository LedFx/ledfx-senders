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
