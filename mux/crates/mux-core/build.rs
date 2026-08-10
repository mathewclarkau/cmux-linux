//! Resolves the cmux version that gets baked into the binary (issue #71).
//!
//! Releases are cut by pushing a `v*` tag; nothing in-tree is bumped by
//! hand. That is why `cmux --version` reported `0.1.0` for seventeen
//! releases — it read `CARGO_PKG_VERSION`, and no human ever remembered
//! to edit the manifest. The version now travels with the binary as a
//! `rustc-env` constant resolved here at build time, in this order:
//!
//! 1. `$CMUX_VERSION` — set by `.github/workflows/release.yml` from the
//!    pushed tag. Authoritative for a release build.
//! 2. `git describe --tags` — an exact tag gives `0.17.2`, anything
//!    downstream gives `0.17.2-14-gabc1234`, so a dev build is never
//!    mistakable for a release.
//! 3. `CARGO_PKG_VERSION` + `-g<sha>` — a repo whose history holds no
//!    reachable tag. That is the normal case in CI: `actions/checkout`
//!    grafts a depth-1 clone, so no tag is an ancestor of HEAD even
//!    when tag refs were fetched.
//! 4. `CARGO_PKG_VERSION` — source tarball / vendored build with no git
//!    and no override. A floor, not the source of truth.
//!
//! Every tier keeps the leading semver triple, which is the invariant
//! `version_is_not_the_stale_placeholder` pins: what varies is the
//! suffix that marks a build as *not* a release. Deliberately never
//! `describe --always`, whose bare-SHA fallback (`79e84a4`) drops the
//! triple entirely and reads as a version from nowhere.
//!
//! Every git call fails soft: a missing `git`, a shallow clone with no
//! tag history, or a non-repo checkout falls through to the next tier
//! rather than breaking the build.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Emitting *any* rerun-if-changed opts out of cargo's default
    // "rerun when anything in the package changes", so every input the
    // resolved constant depends on has to be listed explicitly —
    // including this file. Without the git refs below, an incremental
    // rebuild after tagging would happily reuse the stale string, which
    // is the same class of silent drift issue #71 is about.
    println!("cargo:rerun-if-env-changed=CMUX_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(git_dir) = git_dir() {
        // A commit, a checkout, or a new tag lands in one of these three.
        // Only emit paths that exist: cargo treats an unstattable path as
        // permanently dirty, which would rerun this script on every build.
        for name in ["HEAD", "packed-refs", "refs/tags"] {
            let path = git_dir.join(name);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }

    println!("cargo:rustc-env=CMUX_VERSION={}", resolve_version());
}

fn resolve_version() -> String {
    if let Some(explicit) = env("CMUX_VERSION") {
        return normalise(&explicit);
    }
    let fallback = || env("CARGO_PKG_VERSION").unwrap_or_else(|| "unknown".to_string());
    let mut version = match git(&["describe", "--tags"]) {
        Some(described) => normalise(&described),
        // No tag in reach. Keep the manifest triple as the floor and
        // append the commit, so the string still says which build this
        // is without pretending to be a release.
        None => match git(&["rev-parse", "--short", "HEAD"]) {
            Some(sha) => format!("{}-g{sha}", fallback()),
            None => return fallback(),
        },
    };
    if is_dirty() {
        version.push_str("-dirty");
    }
    version
}

/// Tags are `v0.17.2`; the version we report is `0.17.2`.
fn normalise(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed.strip_prefix('v').unwrap_or(trimmed).to_string()
}

/// `--ignore-submodules` is load-bearing: `scripts/bootstrap.sh` applies
/// `patches/*.patch` into the `ghostty` submodule on every checkout, so
/// a plain `git describe --dirty` would mark *every* build — including a
/// clean CI build from a pristine tag — as dirty. Only our own tracked
/// files count as a local modification.
fn is_dirty() -> bool {
    match git(&["status", "--porcelain", "--untracked-files=no", "--ignore-submodules=all"]) {
        Some(out) => !out.is_empty(),
        None => false,
    }
}

/// `--git-common-dir` (not `--git-dir`) resolves to the *main* `.git`
/// for linked worktrees, which is where `refs/tags` and `packed-refs`
/// actually live.
fn git_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(git(&["rev-parse", "--git-common-dir"])?);
    // git returns an absolute path from a subdirectory and a bare
    // ".git" from the top level; the latter is relative to the cwd we
    // ran it in, which is the manifest dir.
    Some(if dir.is_absolute() { dir } else { manifest_dir().join(dir) })
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").current_dir(manifest_dir()).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(text)
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"))
}

/// Treats an empty or whitespace-only value as unset — CI passes an
/// empty `CMUX_VERSION` on non-tag runs to mean "fall through to git".
fn env(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}
