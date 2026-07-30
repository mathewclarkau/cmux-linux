#!/usr/bin/env bash
# Fetches a pinned zig (build.zig hard-requires 0.15.2; see README) into
# .tools/ without touching any system zig install, then builds mux-tui.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZIG_VERSION="0.15.2"
TOOLS_DIR="$ROOT/.tools"
case "$(uname -m)" in
  x86_64) ZIG_ARCH="x86_64" ;;
  aarch64) ZIG_ARCH="aarch64" ;;
  *) echo "error: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
ZIG_DIR="$TOOLS_DIR/zig-${ZIG_ARCH}-linux-${ZIG_VERSION}"
ZIG_TARBALL="zig-${ZIG_ARCH}-linux-${ZIG_VERSION}.tar.xz"

if [ ! -x "$ZIG_DIR/zig" ]; then
  echo "==> fetching zig $ZIG_VERSION ($ZIG_ARCH) into $ZIG_DIR"
  mkdir -p "$TOOLS_DIR"
  curl -fsSL -o "$TOOLS_DIR/$ZIG_TARBALL" \
    "https://ziglang.org/download/${ZIG_VERSION}/${ZIG_TARBALL}"
  tar -C "$TOOLS_DIR" -xf "$TOOLS_DIR/$ZIG_TARBALL"
  rm "$TOOLS_DIR/$ZIG_TARBALL"
fi

if [ ! -e "$ROOT/ghostty/build.zig" ]; then
  echo "==> initializing ghostty submodule"
  git -C "$ROOT" submodule update --init ghostty
fi

for patch in "$ROOT"/patches/*.patch; do
  [ -e "$patch" ] || continue
  if git -C "$ROOT/ghostty" apply --check --reverse "$patch" 2>/dev/null; then
    continue # already applied
  fi
  echo "==> applying $(basename "$patch") to ghostty/"
  git -C "$ROOT/ghostty" apply "$patch"
done

echo "==> building mux-tui (release)"
cd "$ROOT/mux"
# Pin cargo to the Rust 1.97 toolchain via rustup's "+toolchain" syntax.
# The dtolnay/rust-toolchain action installs Rust 1.97 under
# ~/.cargo but the CI image's PATH may still surface an older
# system cargo first (1.74 on ubuntu-22.04, 1.82 on ubuntu-24.04).
# Older cargo < 1.78 cannot read Cargo.lock v4, which we generate
# by default. The "+1.97" prefix routes through rustup regardless of
# PATH ordering. See AGENTS.md "Pinned toolchain" for the rust pin.
if command -v cargo >/dev/null 2>&1; then
    CARGO_BIN="$(command -v cargo)"
else
    echo "error: cargo not found on PATH (dtolnay/rust-toolchain should install it)" >&2
    exit 1
fi
echo "    using: $("$CARGO_BIN" --version)"
ZIG="$ZIG_DIR/zig" "$CARGO_BIN" "+1.97" build --release -p mux-tui

echo "==> built: $ROOT/mux/target/release/cmux"
