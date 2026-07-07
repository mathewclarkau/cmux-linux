# Provenance

This repository repurposes pieces of [manaflow-ai/cmux](https://github.com/manaflow-ai/cmux)
for Linux. The upstream `cmux` app itself is macOS-only (Swift/AppKit) and is not
included here. What's included:

- `mux/` — vendored verbatim (build artifacts excluded) from
  `manaflow-ai/cmux@adc48877acd03a000da1660006713ac9f81ed611`, subdirectory `mux/`.
  This is the project's own cross-platform Rust + Zig terminal-multiplexer backend
  (`cmux-mux` / `mux-tui`), already OS-agnostic upstream. It is not a git submodule
  here because we intend to actively modify it (session persistence, notifications,
  agent hooks, remote transport) rather than track upstream.
- `ghostty/` — git submodule tracking `manaflow-ai/ghostty` (Manaflow's Ghostty fork),
  pinned to the same commit (`a78fe53efaaea56b80d47569d85e0d7b76512aa7`) that upstream
  cmux pins. Only its VT engine (`libghostty-vt`) is built; the macOS GUI app parts of
  that tree are unused. The submodule pointer itself stays pinned to that exact
  upstream commit (so a fresh clone can always fetch it from the real
  `manaflow-ai/ghostty` remote); local modifications live as patch files in
  `patches/`, applied by `scripts/bootstrap.sh` after `submodule update`. See
  `patches/*.patch` for what each one does and why.

## Licensing

- The upstream `cmux` repository (including the `mux/` subtree) is licensed
  GPL-3.0-or-later, with a commercial-license alternative available from Manaflow,
  Inc. (see `LICENSE`, copied verbatim from upstream). `mux/Cargo.toml` declares
  `license = "MIT"` at the package level, but there is no separate `LICENSE` file
  inside `mux/` overriding the repository-wide grant, so the GPL-3.0-or-later terms
  govern this vendored copy unless/until Manaflow states otherwise.
- `ghostty` (the submodule) is MIT-licensed; see `ghostty/LICENSE`.

## What was NOT touched

Nothing in `/home/matc/Projects/cmux` (the original checkout) was modified. All
Linux-port work happens in this repository only.
