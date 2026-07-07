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

Requires **zig 0.15.2** exactly (0.16 breaks the build on a stdlib signature change)
and a Rust toolchain. Don't need to install zig 0.15.2 system-wide — point `ZIG` at
a local copy:

```bash
git submodule update --init ghostty
cd mux
ZIG=/path/to/zig-0.15.2/zig cargo build -p mux-tui
```

## Run

```bash
cd mux
cargo run -p mux-tui
```

See [`mux/README.md`](./mux/README.md) and [`mux/docs/`](./mux/docs/) for the full
multiplexer docs (keybindings, config, control-socket protocol) — all still accurate
here since `mux/` was vendored verbatim.
