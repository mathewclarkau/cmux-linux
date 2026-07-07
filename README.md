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

1. ~~Git branch / cwd info in the sidebar~~ — done. Every pane tracks a cwd (live
   OSC 7 report when the shell sends one, else the directory it was spawned in —
   see `Surface::cwd()` in `mux-core`), and the sidebar shows the git branch for
   it (`crates/mux-tui/src/git_info.rs`). PR status is not included — it needs
   `gh`/GitHub API access and felt like a separate, heavier addition.
2. ~~Claude Code hook layer (session tracking, restore)~~ — done. `cmux-mux claude
   install-hooks` wires `~/.claude/settings.json` to call `cmux-mux claude hook` on
   every lifecycle event (merged alongside any hooks already there, safely
   idempotent). It reports agent state over `report-agent`/`list-agents`
   (`crates/mux-tui/src/claude_hook.rs`) and records sessions to
   `$XDG_STATE_HOME/cmux-mux/claude-sessions.json` for `cmux-mux claude sessions`
   / `cmux-mux claude resume <session-id>`.
3. ~~Agent-state notifications (OSC 9/99/777 → desktop notification)~~ — done.
   Every pane's raw output is watched for an OSC 9, OSC 777, or kitty-protocol
   desktop notification (`crates/mux-core/src/notify.rs`); each one sets
   `agent_state: blocked` (lowest-authority "detected" source — a hook or
   `report-agent` call still wins) and forwards to the desktop via
   `notify-send`. Needed extending libghostty-vt's C API by two data-extraction
   values — see `patches/` — since it could already *detect* this OSC command
   but not extract its title/body text.
4. ~~Session persistence across daemon restarts~~ — done. Every session
   (headless or local TUI) debounce-writes a snapshot of its workspace/screen/
   pane layout — split shape+ratios, names, each tab's cwd, active selections —
   to `$XDG_STATE_HOME/cmux-mux/sessions/<session>.json`
   (`crates/mux-core/src/persist.rs`), and replays it on next start with the
   same `--session` name. Closing every workspace deletes the file instead of
   leaving something to resurrect later. Deliberately not restored: a tab's
   *command* — every restored tab is the default shell, `cd`'d into its
   recorded directory; something you had running there needs relaunching.
   Verified via a real kill-and-restart of the compiled binary, not just
   library tests — see `mux/docs/getting-started.md`'s "Session persistence".
5. ~~Remote/SSH workspaces~~ — done. `cmux-mux ssh <host>` opens a workspace
   backed by upstream's existing Go `cmuxd-remote` daemon (vendored unmodified
   in `daemon/remote/`, already cross-compiles for `linux/{amd64,arm64}`) instead
   of a local shell. `mux-core/src/remote_pty.rs` implements `portable_pty`'s
   `MasterPty`/`SlavePty`/`Child` traits against an SSH-exec'd NDJSON RPC pipe —
   no real local pty involved. The first connection to a host builds/uploads/
   starts `cmuxd-remote` in persistent mode, so it survives both the SSH
   connection and the local `cmux-mux` process dying; closing the tab detaches
   rather than kills, and restarting the session daemon reattaches
   automatically for a workspace's first tab (same mechanism as #4). Verified
   against a real sshd (localhost), including a kill-and-restart of the
   compiled binary that reattached to the still-running remote shell — see
   `mux/docs/getting-started.md`'s "Remote (SSH) workspaces".

### Known environment quirk (not a cmux-mux bug)

Every new pane spawns your login shell (`$SHELL`) fresh. If you use zsh with
Powerlevel10k and haven't completed its setup wizard yet (no `~/.p10k.zsh`),
that wizard launches in every new pane and blocks on an interactive prompt.
Run `p10k configure` once in a normal terminal (outside cmux-mux) to fix it
for good.

## Build

Requires **zig 0.15.2** exactly and a Rust toolchain. `build.zig` hard-gates on that
version, and it's not just a paranoid check: zig 0.16 breaks the build with at least
two unrelated stdlib signature changes (`Dir.readFileAlloc`'s new `Io`-threaded
signature, `std.process.EnvMap` moving) in the first few seconds of the build graph —
confirmed by patching around both and hitting more. Zig is pre-1.0 and makes exactly
this kind of sweeping breaking change between minor releases, so pinning the exact
version upstream tested against is the correct move, not a workaround.

`scripts/bootstrap.sh` fetches zig 0.15.2 into `.tools/` (not system-wide, doesn't
touch any other zig install), initializes the `ghostty` submodule if needed, applies
this repo's patches to it (see [`patches/README.md`](./patches/README.md) — the
submodule pointer stays on the real upstream commit; local changes are layered on
top so a fresh clone can always fetch it), and builds `mux-tui` in release mode:

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

### Desktop notifications

No setup needed: any program in any pane gets a real desktop notification (via
`notify-send`) and a red sidebar dot just by writing an OSC 9 sequence, e.g.
`printf '\033]9;Build failed\007'`. OSC 777 and the kitty notification protocol
(both can include a title) work the same way. This also feeds the Claude Code
hook layer's status dot: a `report-agent` call (from a hook or the socket) always
takes priority over this passive detection.

### Claude Code integration

```bash
cmux-mux claude install-hooks        # wire up ~/.claude/settings.json (see below)
cmux-mux claude install-hooks --uninstall
cmux-mux claude sessions             # recorded sessions: id, cwd, last event
cmux-mux claude resume <session-id>  # new pane in the recorded cwd, runs claude --resume
```

Once installed, panes running Claude Code show a status dot in the sidebar next to
the git branch: amber while working, red when it needs you (a permission prompt or
similar), green when a turn finishes, dim when idle. This has no effect outside a
cmux-mux pane (`CMUX_MUX_SOCKET`/`CMUX_MUX_SURFACE` are unset, so the hook is a no-op
past recording the session locally) — safe to install even if you don't always run
Claude Code inside cmux-mux.

See [`mux/README.md`](./mux/README.md) and [`mux/docs/`](./mux/docs/) for the full
multiplexer docs (keybindings, config, control-socket protocol) — all still accurate
here since `mux/` was vendored verbatim.
