# cmux-linux

A Linux-native repurposing of [manaflow-ai/cmux](https://github.com/manaflow-ai/cmux),
a terminal environment built for running AI coding agents (Claude Code, Codex, etc.)
in parallel with git-worktree-style workspace isolation.

The upstream `cmux` app is a macOS-only Swift/AppKit application and isn't portable.
This repo instead builds on `mux/`, upstream's own cross-platform Rust + Zig backend
(`cmux-mux` / `mux-tui`), which already reimplements the same session → workspace →
screen → pane → surface model over a JSON-Lines Unix-socket control protocol. See
[`PROVENANCE.md`](./PROVENANCE.md) for exactly what was vendored from where, and
under what license.

## Status

The vendored `mux-tui` builds and runs as-is on Linux (verified on this machine).
What's missing before this feels like `cmux` rather than a bare multiplexer:

1. Git branch / PR / cwd info in the sidebar
2. Claude Code hook layer (session tracking, restore) ported to speak `mux-mux`'s
   control-socket protocol instead of the macOS app's
3. Agent-state notifications (OSC 9/99/777 → desktop notification / "needs attention")
4. Session persistence across daemon restarts
5. Remote/SSH workspaces, wiring upstream's existing Go `cmuxd-remote` daemon
   (already cross-compiles for `linux/amd64` and `linux/arm64`) into `mux-core`
   as a transport

## Build

Requires **zig 0.15.2** exactly and a Rust toolchain. `build.zig` hard-gates on that
version, and it's not just a paranoid check: zig 0.16 breaks the build with at least
two unrelated stdlib signature changes (`Dir.readFileAlloc`'s new `Io`-threaded
signature, `std.process.EnvMap` moving) in the first few seconds of the build graph —
confirmed by patching around both and hitting more. Zig is pre-1.0 and makes exactly
this kind of sweeping breaking change between minor releases, so pinning the exact
version upstream tested against is the correct move, not a workaround.

`scripts/bootstrap.sh` fetches zig 0.15.2 into `.tools/` (not system-wide, doesn't
touch any other zig install), initializes the `ghostty` submodule if needed, and
builds `mux-tui` in release mode:

```bash
./scripts/bootstrap.sh
```

Re-running it is idempotent (skips the zig download and does an incremental
`cargo build` if nothing changed).

## Run

```bash
ln -sf "$(pwd)/mux/target/release/cmux-mux" ~/.local/bin/cmux-mux
cmux-mux
```

(assumes `~/.local/bin` is on your `PATH`, as it already is on this machine)

See [`mux/README.md`](./mux/README.md) and [`mux/docs/`](./mux/docs/) for the full
multiplexer docs (keybindings, config, control-socket protocol) — all still accurate
here since `mux/` was vendored verbatim.
