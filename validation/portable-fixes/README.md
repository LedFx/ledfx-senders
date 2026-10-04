# Portable CI corrections: evidence

Baseline: `b9b4be9f3c9ab20a56118c8cf6af786e5123150b`.
The original hosted run is recorded in `manifest.json`; its three failed platform
logs are preserved alongside local checks in `evidence.tar.gz`. The manifest
records every member's byte count and SHA-256, the archive hash, final source
hashes, and hashes of the three installed wheel binaries used for validation.
The wheels themselves remain local build artifacts, not committed binaries.

Read without extracting:

```sh
tar -tzf validation/portable-fixes/evidence.tar.gz
tar -xOzf validation/portable-fixes/evidence.tar.gz ledfx-portable-contention-negative.log
```

All archive entries are bounded flat filenames. To extract, create a fresh
scratch directory and use `tar -xzf evidence.tar.gz -C /path/to/scratch`.

- Regular and free-threaded installed wheels each passed 238 tests. The FT smoke
  confirms the GIL remained disabled and loopback/concurrent lifecycle checks
  passed. Local wheels target the host's manylinux 2.43, not portable 2.28.
- The old wheel fails the new longdouble buffer-identity regression. The corrected
  wheel passes, including late-invalid input atomicity. Local longdouble is wider
  than float64; the regression emulates equal-dtype comparison without pretending
  this machine supplies a macOS runtime.
- The contention negative control uses an isolated copy replacing production
  `.lock_py_attached(py)` acquisition with `.lock()`. Its separately installed
  wheel fails all three send/service/close gate tests in 15.56 seconds on observable
  timeouts under a 30-second subprocess limit. The real source retains cooperative
  acquisition. This proves progress during contended mutex acquisition, not
  whole-send scheduling or throughput. The private helper adds diagnostic API
  surface but no production frame branch.
- Rust tests: 13 passed; Clippy with `-D warnings`, Rustfmt, and all locked hooks
  passed. Zizmor retains the repository's existing suppressions/offline warning;
  strict Pyrefly uses no baseline or new allowances.
- Build identifier checks still select all seven variants per checked platform.
  Repaired macOS wheel tags, Windows runtime tests, and the complete hosted matrix
  require the next hosted run. This evidence does not declare those gates green.

Deployment floors were checked against the actual Rust 1.94 compiler and
[cibuildwheel 4.2.1 macOS code](https://github.com/pypa/cibuildwheel/blob/v4.2.1/cibuildwheel/platforms/macos.py).
The [Windows runner](https://github.com/pypa/cibuildwheel/blob/v4.2.1/cibuildwheel/platforms/windows.py)
and [command helper](https://github.com/pypa/cibuildwheel/blob/v4.2.1/cibuildwheel/util/cmd.py)
explain why range operators in test requirements reach `cmd.exe`; exact pins
remove those operators while retaining the tests.
