# ledfx-senders

Native packet senders for Python applications, maintained in the LedFx monorepo.
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

For development, run `uv sync --group dev` at the monorepo root. The uv workspace
installs this package locally. For an application-wheel test before the first
library release, install from the built sender wheelhouse with `--find-links`.
CI currently builds and retains library artifacts; publication is a separate
release decision. No existing PyPI release is assumed.

Source builds require Rust 1.94.0 (pinned in `rust-toolchain.toml`) and the host C
linker/toolchain. Installing a compatible wheel needs no Rust. From the repository:

```sh
rustup toolchain install 1.94.0 --profile minimal
uv build --package ledfx-senders
uv sync --group dev
uv run pytest packages/ledfx-senders/tests
```

The package's sdist includes its Rust sources, locked Cargo dependencies, build
hook, independent protocol fixtures, tests, and CI smoke script. It builds outside
the LedFx checkout. `ci/native_wheel_smoke.py` runs with isolated Python against
an installed wheel and exercises only loopback network interfaces.
