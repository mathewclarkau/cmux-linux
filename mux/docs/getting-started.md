# Getting started

## Prerequisites

Builds need zig 0.15.2, a Rust toolchain, and the `ghostty` submodule. `ghostty-vt-sys` compiles `libghostty-vt.a` from that submodule, so an uninitialized submodule fails before the TUI starts.

```bash
cd mux
cargo build -p mux-tui
```

## Local session

A normal run starts an in-process mux, opens the TUI, and serves the control socket.

```bash
cd mux
cargo run -p mux-tui
cargo run -p mux-tui -- --session agents
```

The default session is `main`. Quitting a local TUI shuts down that in-process session and removes its socket.

Use `--term <value>` to set `TERM` for child PTYs. Without it, children get `xterm-256color`; the surface layer also honors `CMUX_MUX_TERM` when no CLI value is supplied.

## Headless server and attach

Headless mode starts only the mux backend and control socket.

```bash
cd mux
cargo run -p mux-tui -- --headless --session agents
```

Attach a TUI to that session from another terminal.

```bash
cd mux
cargo run -p mux-tui -- attach --session agents
```

Detach from an attached TUI with prefix `d`. With default keys, that is `Ctrl-b d`. The server keeps running, and another `attach` reconnects to the same tree. PTY tabs attach with a Ghostty VT-state replay followed by a live output stream.

## Sessions and sockets

The default socket path is:

```text
$TMPDIR/cmux-mux-<uid>/<session>.sock
```

The usual default is `$XDG_RUNTIME_DIR/cmux-mux-<uid>/main.sock` when `XDG_RUNTIME_DIR` is set, then `$TMPDIR/cmux-mux-<uid>/main.sock`, then `/tmp/cmux-mux-<uid>/main.sock`. `--session <name>` changes the final file name. `--socket <path>` bypasses the session-derived path. Server-started child processes receive `CMUX_MUX_SOCKET` with the socket path.

## Session persistence

Every session (headless or local TUI) writes a snapshot of its workspace/screen/pane layout — split shape and ratios, names, and each tab's cwd — to `$XDG_STATE_HOME/cmux-mux/sessions/<session>.json` (falling back to `~/.local/state/...`), debounced a few hundred ms after each structural change and again on clean shutdown. Starting a session with the same `--session` name again (a real daemon restart, or just restarting the local TUI) replays that snapshot: same panes, same directories. Closing every workspace deletes the file rather than leaving a stale one to resurrect later.

Not restored: a tab's *command*. Every restored tab is the default shell, `cd`'d into its recorded directory (visibly, briefly, before a `clear`) — if something specific was running there (a dev server, `claude --resume ...`), you'll need to relaunch it. See `mux-core/src/persist.rs` for why, and `Mux::restore_session`/`Mux::enable_persistence` for the implementation.

## Remote (SSH) workspaces

```bash
cargo run -p mux-tui -- ssh <host>
cargo run -p mux-tui -- ssh <host> --name my-remote-work
```

Opens a workspace whose tab is a shell on `<host>` instead of local, backed by
[`cmuxd-remote`](../../daemon/remote) (vendored from upstream cmux, unmodified) speaking
NDJSON RPC over an SSH-exec'd pipe (not a real allocated local pty — see
`mux-core/src/remote_pty.rs`'s module doc for how `portable_pty`'s traits get
implemented against that RPC channel instead). The first connection to a host
builds and caches a `cmuxd-remote` binary for its OS/arch (needs Go on `PATH`;
cross-compiles via `GOOS`/`GOARCH`, no toolchain needed on the remote), uploads
it, and starts it in **persistent** mode: it forks a detached background daemon
on the remote that outlives both the SSH connection and this local process.

Two consequences:

- **Closing the tab detaches, not kills.** The remote shell keeps running; the
  session survives disconnecting.
- **Restarting this session's daemon reattaches automatically**, the same way
  local tabs' layout restores (see Session Persistence above) — a workspace's
  first tab being remote is recorded in the snapshot (host, slot, session id,
  and the cached binary path) and `Mux::restore_session` calls
  `Mux::new_remote_workspace` with the same session id instead of spawning a
  local shell. This only works for a workspace's very first tab today — a
  second tab in a pane, or any pane a split created, has no such path
  (`new_tab`/`split` only ever spawn local shells) and downgrades to an
  ordinary local tab on restore, with a status message noting it.

There's no verb to actually end a remote session (only detach it) — a stale one
needs manual cleanup on the remote host (`rm -rf ~/.cmux/daemon ~/.cache/cmux-mux`
there, or `kill` its `cmuxd-remote serve --persistent-server` process).

## Platforms and XDG

cmux supports macOS and Linux; Windows support via ConPTY is planned for phase 2. The TUI config path resolves `CMUX_MUX_CONFIG`, then `$XDG_CONFIG_HOME/cmux/mux.json`, then `~/.config/cmux/mux.json`.

Launched Chrome profile paths are platform-specific. On macOS the default is `~/Library/Application Support/cmux-mux/chrome-profile`. On Linux and other non-macOS targets, `XDG_DATA_HOME` is used when set, then `~/.local/share/cmux-mux/chrome-profile`.

## Development flow

Run tests from `mux/`.

```bash
cargo test
```

Run the smoke scripts against a built binary. Set `CMUX_MUX_BIN` to test a non-default binary.

```bash
cargo build -p mux-tui
python3 scripts/smoke-tui.py
python3 scripts/smoke-attach.py
```

This checkout does not contain `scripts/mux-dev.sh`; use the cargo and smoke commands above for the TUI flow.
