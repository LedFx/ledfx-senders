#!/usr/bin/env bash
# Runs only inside the immutable manylinux wheel container.
set -euo pipefail
case "$(uname -m)" in
  x86_64) digest=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c ;;
  aarch64) digest=e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c ;;
  *) exit 1 ;;
esac
mkdir -p /tmp/ledfx-native-rust
curl --fail --silent --show-error --location \
  "https://static.rust-lang.org/rustup/archive/1.28.2/$(uname -m)-unknown-linux-gnu/rustup-init" \
  --output /tmp/ledfx-native-rust/rustup-init
echo "$digest  /tmp/ledfx-native-rust/rustup-init" | sha256sum --check
chmod +x /tmp/ledfx-native-rust/rustup-init
/tmp/ledfx-native-rust/rustup-init -y --no-modify-path --profile minimal --default-toolchain 1.94.0
