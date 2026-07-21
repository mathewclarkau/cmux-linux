# cmux-linux — agent + contributor notes

This file is for AI coding agents (Claude Code, Codex, Pi, Aider, Antigravity) **and** for human contributors. It pins the toolchain and calls out the gotchas you'll hit on a fresh build.

## Pinned toolchain (non-negotiable)

| Tool | Version | Why |
|------|---------|-----|
| **zig** | **0.15.2** | Enforced at compile time by `mux/crates/ghostty-vt-sys/build.zig` via `@compileError`. **Zig is NOT on the system PATH** — the repo vendors it under `.tools/zig-<arch>-linux-0.15.2/zig` (where `<arch>` is `x86_64` or `aarch64`). System zig 0.16.0 (Ubuntu 24.04, current Arch, current Nix `zigPackages.latest`) will fail with "version mismatch" errors. Do **not** `apt install zig` and assume you're done. Instead: (1) run `scripts/bootstrap.sh` first (it fetches the right zig into `.tools/`), then (2) export `ZIG=$REPO/.tools/zig-$(uname -m | sed 's/x86_64/x86_64/;s/aarch64/aarch64/')-linux-0.15.2/zig` before running cargo, OR (3) just run cargo from within `scripts/bootstrap.sh` (which sets `ZIG` for you). The GitHub Actions pr-build.yml workflow does (1)+(2) and is the canonical example. |
| **rust** | **1.75.0** | The CI-tested version in `release.yml`. Anything newer (Rust 1.82+ `is_none_or`, 1.87+ `is_multiple_of`, 1.88+ `let-chains`, 1.85+ format-capture syntax) will fail to compile if introduced by accident. The CI matrix is intentionally pinned to a stable old release; do not bump without a corresponding toolchain bump PR. Use `dtolnay/rust-toolchain@1.75.0` (or `rustup toolchain install 1.75.0 && cargo +1.75.0 …`). |
| **bindgen** | **0.70.1** | Pinned in `mux/crates/ghostty-vt-sys/Cargo.toml` and `mux/Cargo.lock`. 0.71+ requires bindgen APIs not available in Rust 1.75. Do not bump for a security advisory without re-verifying 1.75 builds clean. |

## First build

```bash
# Option A: all-in-one (recommended) — bootstrap.sh does rust + zig + cargo build
./scripts/bootstrap.sh
# → sets ZIG=$REPO/.tools/zig-<arch>-linux-0.15.2/zig and runs `cargo build --release -p mux-tui`

# Option B: two-step — bootstrap.sh downloads zig, then you run cargo with ZIG exported
./scripts/bootstrap.sh
export ZIG="$PWD/.tools/zig-$(uname -m | sed 's/x86_64/x86_64/;s/aarch64/aarch64/')-linux-0.15.2/zig"
cd mux && cargo build                       # build.rs reads $ZIG

# Option C: just cargo (assumes someone else exported ZIG)
cd mux && cargo build                       # build.rs falls back to `zig` (PATH) — DO NOT rely on this on a fresh system
```

`bootstrap.sh` is idempotent; re-run any time you blow away `.tools/`.

**If `cargo build` fails with "version mismatch" on the zig @compileError**: you forgot to export `ZIG`. The build.rs falls back to `zig` on PATH, which on most systems is 0.16.x and rejected.

## Build / build.rs gotchas

- `mux/crates/ghostty-vt-sys/build.rs` discovers system include paths by shelling out to `cc -E -Wp,-v -` (with `clang` and `gcc` as fallbacks). If you have `clang` but not `cc`/`gcc` installed, the probe still works via `clang`; if the `clang` probe also fails, bindgen falls back to libclang's bundled resource-dir headers. This works on Ubuntu with `apt install clang libclang-dev` but can fail on stripped-down clang packages (thin Arch `clang`, distroless images, some Nix shells).
  - **If you see `limits.h` not found during bindgen:** install `cc` (or `gcc`), OR set `BINDGEN_EXTRA_CLANG_ARGS="-resource-dir $(clang -print-resource-dir)"` in the environment. The build.rs will pick this up automatically.

## Cargo / repo conventions

- Workspace is under `mux/` (`mux/Cargo.toml`). Run `cargo` from there, not from the repo root.
- No CI exists for PRs. There is no GitHub Actions workflow that catches Rust 1.75 breakage on push; the only check is `./scripts/bootstrap.sh && cargo build` (and the release.yml workflow on tag pushes, which only builds — it does not lint).
- The repo is a Linux port vendoring from `manaflow-ai/cmux`. Preserve the upstream of `mux/crates/ghostty-vt-sys/include/ghostty` unchanged across rebases.
- `Cargo.lock` version is **3** (Rust 1.75 era), not 4. Don't bump it.

## LLM harness hooks (under `mux/crates/mux-tui/src/`)

The binary subcommands `antigravity`, `codex`, `grok`, `pi`, `aider` (and the existing `claude` on main) all install hooks by reading a config file (or writing a wrapper/extension), merging in our entries, and writing it back. They share parsing/writing helpers and a `--uninstall`/`--global` flag pair.

**Editing one of these files?** Consider a refactor PR first — there's substantial duplication across them (load-or-default JSON, write-pretty, `--uninstall`/`--global` parse, the `CMUX-START`/`CMUX-END` block rewriter). A `hook_merge` module with `load_or_default<T>`, `save_pretty<T>`, `parse_flags(&[String]) -> (bool, bool)`, and `replace_marked_block(path, start, end, content)` would collapse ~200 lines of duplication.

**Security gotchas these files all share (review checklist):**

- **JSON parse errors must propagate, not silently `unwrap_or_default()`.** A schema-drift user config should not be silently overwritten with `Default::default() + our hooks`.
- **Symlink check before write.** `fs::write` on a symlink path overwrites the *target*, not the symlink. Use `fs::symlink_metadata` to detect, or `OpenOptions::new().custom_flags(libc::O_NOFOLLOW)` to fail open.
- **Toml edits use line-by-line checks, not `String::contains()`.** A key like `docs.codex_hooks = true` would false-positive-match `contains("codex_hooks = true")`.
- **Shell-exec escapes arg arrays.** When emitting shell scripts or Node `exec()` calls, use `execFile(cmd, [args], cb)` with an arg array — never string-interpolate an env-var value into a shell command.

## Output style for this repo

- Keep `git diff` clean and minimal — no whitespace-only churn.
- Use `cargo check`, not `cargo build`, for fast iteration.
- Never commit `.tools/`, `mux/target/`, `.venv/`, or anything in `.gitignore`.
- Commit messages: `type(scope): short description` — types: `feat`, `fix`, `refactor`, `docs`, `chore`, `test`.

## Known issues / follow-ups

- The hook-file duplication noted above is the highest-priority refactor.
- The `claude_hook.rs` `map_or(true, ...)` rewrite (PR #1) will trip `clippy::unnecessary_map_or` and `clippy::manual_is_multiple_of` under Rust 1.96+ — the `// allow(clippy::unnecessary_map_or)` workaround is fine for now, but a cleaner alternative is `.is_some_and(|list| !list.is_empty())` which works on Rust 1.75.
- PRs are now auto-gated by `.github/workflows/pr-build.yml` (pinned to Rust 1.75.0 + zig 0.15.2; runs `cargo check -p mux-tui` and `cargo test -p mux-tui` on `ubuntu-latest`/x86_64 and `ubuntu-24.04-arm`/aarch64). Tag-push releases remain owned by `.github/workflows/release.yml`.
