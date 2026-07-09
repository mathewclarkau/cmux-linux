# cmux-linux — agent + contributor notes

This file is for AI coding agents (Claude Code, Codex, Pi, Aider, Antigravity) **and** for human contributors. It pins the toolchain and calls out the gotchas you'll hit on a fresh build.

## Pinned toolchain (non-negotiable)

| Tool | Version | Why |
|------|---------|-----|
| **zig** | **0.15.2** | Enforced at compile time by `mux/crates/ghostty-vt-sys/build.zig` via `@compileError`. System zig 0.16.0 (Ubuntu 24.04, current Arch, current Nix `zigPackages.latest`) will fail with "version mismatch" errors. Do **not** `apt install zig` and assume you're done — use `scripts/bootstrap.sh`, which fetches the right zig into `.tools/zig-0.15.2/`. |
| **rust** | **1.75.0** | The CI-tested version in `release.yml`. Anything newer (Rust 1.82+ `is_none_or`, 1.87+ `is_multiple_of`, 1.88+ `let-chains`, 1.85+ format-capture syntax) will fail to compile if introduced by accident. The CI matrix is intentionally pinned to a stable old release; do not bump without a corresponding toolchain bump PR. |
| **bindgen** | **0.70.1** | Pinned in `mux/crates/ghostty-vt-sys/Cargo.toml` and `mux/Cargo.lock`. 0.71+ requires bindgen APIs not available in Rust 1.75. Do not bump for a security advisory without re-verifying 1.75 builds clean. |

## First build

```bash
./scripts/bootstrap.sh           # fetches zig 0.15.2, ensures rust 1.75.0
cargo build                      # builds everything via the zig-cc shim
```

`bootstrap.sh` is idempotent; re-run any time you blow away `.tools/`.

## Build / build.rs gotchas

- `mux/crates/ghostty-vt-sys/build.rs` discovers system include paths by shelling out to `cc -E -Wp,-v -` (with `clang` and `gcc` as fallbacks). If you have `clang` but not `cc`/`gcc` installed, the probe still works via `clang`; if the `clang` probe also fails, bindgen falls back to libclang's bundled resource-dir headers. This works on Ubuntu with `apt install clang libclang-dev` but can fail on stripped-down clang packages (thin Arch `clang`, distroless images, some Nix shells).
  - **If you see `limits.h` not found during bindgen:** install `cc` (or `gcc`), OR set `BINDGEN_EXTRA_CLANG_ARGS="-resource-dir $(clang -print-resource-dir)"` in the environment. The build.rs will pick this up automatically.

## Cargo / repo conventions

- Workspace is under `mux/` (`mux/Cargo.toml`). Run `cargo` from there, not from the repo root.
- No CI exists for PRs. There is no GitHub Actions workflow that catches Rust 1.75 breakage on push; the only check is `./scripts/bootstrap.sh && cargo build` (and the release.yml workflow on tag pushes, which only builds — it does not lint).
- The repo is a Linux port vendoring from `manaflow-ai/cmux`. Preserve the upstream of `mux/crates/ghostty-vt-sys/include/ghostty` unchanged across rebases.
- `Cargo.lock` version is **3** (Rust 1.75 era), not 4. Don't bump it.

## LLM harness hooks (under `mux/crates/mux-tui/src/`)

The four binary subcommands `antigravity`, `codex`, `pi`, `aider` (and the existing `claude` on main) all install hooks by reading a config file (or writing a wrapper/extension), merging in our entries, and writing it back. They share parsing/writing helpers and a `--uninstall`/`--global` flag pair.

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
- The release workflow at `.github/workflows/release.yml` only triggers on `v*` tag pushes. There is no PR CI.
