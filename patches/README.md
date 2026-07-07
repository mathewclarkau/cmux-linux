# Patches to `ghostty/`

Applied automatically by `scripts/bootstrap.sh` after `git submodule update`.
The submodule itself stays pinned to the exact upstream commit cmux uses (see
`../PROVENANCE.md`) — these patches are layered on top, not committed into the
submodule, so a fresh clone can always fetch that commit from the real
`manaflow-ai/ghostty` remote.

To add a new patch: make the change in a checked-out `ghostty/` working tree,
verify it (see the commit that introduced it for how), then:

```bash
git -C ghostty diff > patches/000N-short-description.patch
git -C ghostty checkout -- <the files you changed>
```

## 0001-osc-extract-desktop-notification-title-body.patch

Adds two `GhosttyOscCommandData` values to the public C API
(`GHOSTTY_OSC_DATA_DESKTOP_NOTIFICATION_TITLE_STR`/`..._BODY_STR`) so
`ghostty_osc_command_data` can extract a parsed OSC 9 / OSC 777 / kitty
desktop notification's title and body — mirroring the existing
`GHOSTTY_OSC_DATA_CHANGE_WINDOW_TITLE_STR` pattern exactly. The underlying
Zig OSC parser (`src/terminal/osc.zig`) already parses all three formats into
one `show_desktop_notification { title, body }` command; only the C API
extraction for it was missing. Used by `mux/crates/ghostty-vt/src/osc.rs`
(the safe Rust wrapper) and `mux/crates/mux-core/src/notify.rs` (the
per-surface pty output watcher) — see `mux/docs/protocol.md`'s "Desktop
Notifications" section for the resulting feature.
