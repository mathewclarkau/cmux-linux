//! Shared helpers for the agent hook installers (`antigravity_hook`,
//! `codex_hook`, `pi_hook`).
//!
//! ## What is and isn't migrated here
//!
//! `claude_hook.rs` is deliberately **NOT** migrated into this module:
//! post-PR #7 it is already minimal, and it is the precedent for "if the
//! file is already small, don't refactor it for the sake of it."
//!
//! `aider_hook.rs` is also untouched. It installs a bash wrapper and has
//! no JSON config and no marker-block logic, so it consumes **none** of
//! these helpers. Adding a `use crate::hook_merge;` there would be an
//! abstraction with nothing behind it.
//!
//! ## Behavior contracts preserved from the original inline code
//!
//! These are the two contracts the scout report flagged as load-bearing;
//! getting either wrong is a silent regression, not a refactor:
//!
//!   * **`load_or_default` fails loud on parse errors** and only
//!     substitutes `T::default()` on a *genuinely missing* file. A
//!     silent `unwrap_or_default()`-style swallow would overwrite a
//!     user's real, merely-schema-drifted config (the old inline code
//!     carried explicit comments warning about this at
//!     `antigravity_hook.rs:65-67` and `codex_hook.rs:67`).
//!
//!   * **`replace_marked_block` strips-then-appends**, not
//!     replace-in-place: any existing block is removed from wherever it
//!     sits and a fresh block is appended at the very end. This matches
//!     `pi_hook.rs`'s original install behavior. The trailing whitespace
//!     is trimmed before appending (the old install path did not trim,
//!     which left blank-line drift; the uninstall path did trim — this
//!     module makes the install path match the uninstall path).

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::Path;

/// Why a config load failed.
///
/// `NotFound` is *recoverable*: the caller may legitimately substitute
/// `T::default()` (install path) or skip (uninstall path). `Io` and
/// `Parse` are **not** recoverable — surfacing them silently would risk
/// overwriting a user's real, merely-schema-drifted config, so they are
/// propagated and the caller is expected to exit non-zero.
#[derive(Debug)]
pub(crate) enum LoadError {
    /// The file does not exist. Recoverable.
    NotFound,
    /// Reading the file failed for a reason other than "missing".
    Io(std::io::Error),
    /// The file's contents are not valid JSON for `T`.
    Parse(serde_json::Error),
}

/// Why a config save failed.
#[derive(Debug)]
pub(crate) enum SaveError {
    /// `value` could not be serialized to JSON.
    Serialize(serde_json::Error),
    /// Writing the serialized JSON to `path` failed.
    Io(std::io::Error),
}

/// A delimited cmux-managed block. `start` / `end` are the literal
/// substrings searched for on each line (e.g. the HTML-comment markers
/// `<!-- CMUX-START -->` / `<!-- CMUX-END -->`). Carried as a pair so a
/// caller can never pass half a pair.
pub(crate) struct Markers {
    pub start: &'static str,
    pub end: &'static str,
}

/// The real cmux marker tokens used by `pi_hook.rs`'s `APPEND_SYSTEM.md`
/// rewriter. **HTML-comment style** — NOT the `<<<CMUX-START>>>` form
/// mentioned in the issue brief. Using the wrong tokens here would break
/// existing installs (the markers already written into users'
/// `APPEND_SYSTEM.md` files are these HTML-comment ones).
pub(crate) const CMUX_MARKERS: Markers =
    Markers { start: "<!-- CMUX-START -->", end: "<!-- CMUX-END -->" };

/// Read and parse a JSON file at `path`.
///
/// Returns `Err(LoadError::NotFound)` only when the file is genuinely
/// missing; other read errors become `Err(LoadError::Io(..))` and a
/// failed parse becomes `Err(LoadError::Parse(..))`. None of those are
/// silently swallowed — see the module docs for why.
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, LoadError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::NotFound),
        Err(e) => return Err(LoadError::Io(e)),
    };
    serde_json::from_str::<T>(&text).map_err(LoadError::Parse)
}

/// Load a JSON config, falling back to `T::default()` only when the file
/// is *missing*.
///
/// This is the install-path convenience: "if the user has no config yet,
/// start from a default; otherwise parse what they have." Parse and IO
/// errors are still propagated as `Err(..)` — they are **not** defaulted
/// — because silently defaulting on a parse error would overwrite a
/// user's real config on schema drift. The uninstall path should use
/// [`load_json`] instead, since it must *skip* on missing (not default
/// and then write a freshly-created file).
pub(crate) fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T, LoadError> {
    match load_json::<T>(path) {
        Ok(v) => Ok(v),
        Err(LoadError::NotFound) => Ok(T::default()),
        Err(e) => Err(e),
    }
}

/// Serialize `value` as pretty JSON (2-space indent, matching
/// `serde_json::to_string_pretty`) and write it to `path`, creating or
/// truncating the file.
///
/// Does **not** create parent directories. The contract is that callers
/// `fs::create_dir_all(parent)` first — every hook installer already
/// does this separately, and keeping the mkdir out of this helper means
/// a missing-parent error stays a real `Err(Io)` instead of being
/// papered over.
pub(crate) fn save_pretty<T: Serialize>(path: &Path, value: &T) -> Result<(), SaveError> {
    let s = serde_json::to_string_pretty(value).map_err(SaveError::Serialize)?;
    std::fs::write(path, s).map_err(SaveError::Io)
}

/// Strip every `start..end`-delimited block from `content` and return
/// the surviving lines (marker lines and inter-block content are
/// dropped; everything outside the block(s) is kept), with no trailing
/// newline.
///
/// An unclosed `start` swallows everything from there to EOF — matching
/// the original `pi_hook.rs` strip loops (the `skipping` flag, once set
/// by a `start` line with no matching `end`, never clears).
///
/// This is the shared core of [`parse_flags`] (which returns the
/// *inside* of the block) and [`replace_marked_block`] (which returns
/// the *outside* plus a fresh appended block). It is `pub(crate)` rather
/// than private because `pi_hook`'s uninstall path needs a pure strip
/// (remove the block; if the file ends up empty, delete it) that
/// neither `parse_flags` (extract) nor `replace_marked_block`
/// (strip-then-append) provides on its own.
pub(crate) fn strip_marked_block(content: &str, markers: &Markers) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line.contains(markers.start) {
            skipping = true;
            continue;
        }
        if line.contains(markers.end) {
            skipping = false;
            continue;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    // The loop appends a trailing '\n' after every kept line; trim it so
    // callers (replace_marked_block, the uninstall path) control the
    // exact tail and we avoid blank-line drift.
    out.trim_end().to_string()
}

/// Return the lines strictly *between* the first `start..end` block in
/// `content` (marker lines excluded), joined with `\n`.
///
/// Returns `None` when no `start` marker is present at all. If a
/// `start` is never closed by an `end`, the block runs to EOF —
/// preserving the original `pi_hook.rs` "unclosed start swallows to
/// EOF" behavior — and the returned content is everything after the
/// `start` to EOF. Only the *first* block is returned; a second
/// `start..end` block later in the file is ignored (the canonical
/// choice per the scout report, since a file should only ever hold one
/// cmux-managed block).
///
/// This is API surface for "detect what cmux already added" (the issue
/// brief's 5-helper list). None of the current installers actually read
/// the existing block content — they unconditionally strip and
/// re-append — so this helper has no call site in the 3 edited files
/// today; it is provided and tested so future installers (and
/// `claude_hook.rs` when it migrates) can adopt it without reshape.
#[allow(dead_code)] // API surface per issue #5; no current call site.
pub(crate) fn parse_flags(content: &str, markers: &Markers) -> Option<String> {
    let mut inner = String::new();
    let mut in_block = false;
    let mut found = false;
    for line in content.lines() {
        if !in_block {
            if line.contains(markers.start) {
                in_block = true;
                found = true;
            }
            // Lines before the block are not part of the inner content.
        } else if line.contains(markers.end) {
            // Stop after the FIRST block.
            break;
        } else {
            inner.push_str(line);
            inner.push('\n');
        }
    }
    if !found {
        return None;
    }
    Some(inner.trim_end().to_string())
}

/// Replace the cmux-managed block in `content` with a fresh block
/// wrapping `replacement_inner`, preserving the original
/// "strip-any-existing-block-from-anywhere, then append a fresh block
/// at the end" behavior of `pi_hook.rs`'s install path (NOT
/// replace-in-place).
///
/// `replacement_inner` is the text that goes *between* the markers; it
/// must not include the marker lines themselves. Trailing whitespace on
/// the stripped content is trimmed before the fresh block is appended
/// (the old install path did not trim, leaving blank-line drift; this
/// makes it match the uninstall path's `trim_end()`).
///
/// The appended block is exactly `\n{start}\n{inner}\n{end}\n`, so an
/// empty `content` yields just that block with a leading newline.
pub(crate) fn replace_marked_block(
    content: &str,
    markers: &Markers,
    replacement_inner: &str,
) -> String {
    let stripped = strip_marked_block(content, markers);
    let mut out = stripped;
    out.push('\n');
    out.push_str(markers.start);
    out.push('\n');
    out.push_str(replacement_inner);
    out.push('\n');
    out.push_str(markers.end);
    out.push('\n');
    out
}

/// How a config path relates to the user. See [`path_kind`].
#[allow(dead_code)] // provisioned for future safety guards; no current call site (see parse_flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathKind {
    /// Project-local: a relative path starting with `.` (e.g. `.codex/`,
    /// `.agents/`, `.pi/`).
    Project,
    /// Under the user's home directory (e.g. `~/.codex/`,
    /// `~/.gemini/`), i.e. an absolute path that starts with `home`.
    Global,
    /// Absolute and outside `$HOME` (e.g. `/etc/...`). Not produced by
    /// any current hook; reserved for future safety guards.
    System,
}

/// Classify `path`.
///
/// `home` is the user's home directory (callers pass
/// `mux_core::platform::home_dir()`) so the function is testable
/// without depending on `$HOME`.
///
/// **`~` is NOT expanded.** `std::path::Path` treats a literal `"~/..."`
/// path as relative (it does not start with `/`) and not starting with
/// `.`, so it falls through to [`PathKind::System`] unless the caller
/// pre-expands the tilde to the absolute home path. The current hook
/// code never produces literal `~` paths — `home_dir()` always returns
/// an absolute path — so this is documented, not handled internally.
#[allow(dead_code)] // new abstraction per issue #5; no current call site (hook files use a `global: bool` convention).
pub(crate) fn path_kind(path: &Path, home: Option<&Path>) -> PathKind {
    let s = path.to_string_lossy();
    if !path.is_absolute() && s.starts_with('.') {
        PathKind::Project
    } else if let Some(h) = home {
        if path.starts_with(h) {
            PathKind::Global
        } else {
            PathKind::System
        }
    } else {
        PathKind::System
    }
}

/// `true` iff `path` is a user-owned path ([`PathKind::Project`] or
/// [`PathKind::Global`]) — i.e. not a system path like `/etc/...`.
#[allow(dead_code)] // see path_kind: no current call site.
pub(crate) fn is_user_path(path: &Path, home: Option<&Path>) -> bool {
    matches!(path_kind(path, home), PathKind::Project | PathKind::Global)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    // A small JSON config type standing in for the real
    // AntigravityHooksConfig / CodexHooksConfig — the helpers are
    // generic, so a toy type exercises them without pulling in the
    // hook modules.
    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Cfg {
        #[serde(default)]
        hooks: Vec<String>,
    }

    // Each file-I/O test gets its own unique scratch directory under the
    // system temp dir. We don't have `tempfile` as a dev-dep, so this
    // rolls our own: a per-process, per-call counter ensures parallel
    // tests and repeated runs never collide. Leaking the dirs is fine
    // (the OS reaps /tmp).
    static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(label: &str) -> PathBuf {
        let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "cmux_hook_merge_test_{}_{}_{}",
            std::process::id(),
            n,
            label
        ));
        // Start from a clean slate in case a prior run left it behind.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    // ---- load_or_default ----

    #[test]
    fn load_or_default_missing_file_returns_default() {
        // Happy path: a genuinely missing file yields Ok(T::default()),
        // and must NOT create the file or print an error.
        let dir = scratch_dir("load_missing");
        let path = dir.join("hooks.json");
        assert!(!path.exists());
        let got: Cfg = load_or_default(&path).expect("missing file should Ok(default)");
        assert_eq!(got, Cfg::default());
        // Contract: no file is created by a load.
        assert!(!path.exists());
    }

    #[test]
    fn load_or_default_malformed_json_returns_parse_err() {
        // Error/edge path: a file that exists but is not valid JSON
        // returns Err(LoadError::Parse). This is the case the old
        // inline comments explicitly protected — defaulting here would
        // silently clobber a user's real, schema-drifted config.
        let dir = scratch_dir("load_malformed");
        let path = dir.join("hooks.json");
        std::fs::write(&path, "{not json").unwrap();
        match load_or_default::<Cfg>(&path) {
            Err(LoadError::Parse(_)) => {}
            other => panic!("expected Err(Parse), got {other:?}"),
        }
    }

    // ---- save_pretty ----

    #[test]
    fn save_pretty_round_trips_through_load_json() {
        // Happy path: save then load yields an equal value, and the
        // on-disk JSON uses 2-space pretty indentation.
        let dir = scratch_dir("save_roundtrip");
        let path = dir.join("hooks.json");
        let cfg = Cfg { hooks: vec!["cmux-mux report-agent".to_string(), "other".to_string()] };
        save_pretty(&path, &cfg).expect("save should succeed");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("\n  "),
            "expected 2-space pretty indent on disk, got:\n{on_disk}"
        );
        let back: Cfg = load_json(&path).expect("reload should succeed");
        assert_eq!(back, cfg);
    }

    #[test]
    fn save_pretty_missing_parent_returns_io_err() {
        // Error/edge path: the helper does NOT auto-mkdir, so writing
        // into a path whose parent does not exist is Err(SaveError::Io)
        // — preserving the contract that callers create the parent.
        let dir = scratch_dir("save_no_parent");
        let path = dir.join("does_not_exist").join("hooks.json");
        assert!(dir.join("does_not_exist").exists() == false);
        match save_pretty(&path, &Cfg::default()) {
            Err(SaveError::Io(_)) => {}
            other => panic!("expected Err(Io) for missing parent, got {other:?}"),
        }
        // No file was created.
        assert!(!path.exists());
    }

    // ---- parse_flags ----

    #[test]
    fn parse_flags_returns_inner_lines_of_first_block() {
        // Happy path: the lines strictly between the first START..END
        // block are returned (marker lines excluded), and text before
        // and after the block is not part of the result.
        let content = "preamble\n\
            <!-- CMUX-START -->\n\
            line one\n\
            line two\n\
            <!-- CMUX-END -->\n\
            epilogue\n";
        let got = parse_flags(content, &CMUX_MARKERS).expect("block present");
        assert_eq!(got, "line one\nline two");
    }

    #[test]
    fn parse_flags_no_markers_returns_none() {
        // Error/edge path: no START marker at all -> None, so an
        // installer can decide to append rather than crash.
        let content = "just some text\nno markers here\n";
        assert_eq!(parse_flags(content, &CMUX_MARKERS), None);
    }

    // ---- replace_marked_block ----

    #[test]
    fn replace_marked_block_strips_existing_and_appends_fresh_at_end() {
        // Happy path: an existing block in the middle is removed from
        // its old position, and a fresh block is appended at the very
        // end. Text before and after the old block is preserved and
        // concatenated; trailing whitespace is trimmed so there is a
        // single newline before the new block (no blank-line drift).
        let content = "before\n\
            <!-- CMUX-START -->\n\
            stale old skill\n\
            <!-- CMUX-END -->\n\
            after\n";
        let got = replace_marked_block(content, &CMUX_MARKERS, "fresh skill");
        assert_eq!(
            got,
            "before\nafter\n\
             <!-- CMUX-START -->\n\
             fresh skill\n\
             <!-- CMUX-END -->\n"
        );
    }

    #[test]
    fn replace_marked_block_empty_content_yields_just_the_block() {
        // Error/edge path: empty content -> exactly the fresh block with
        // a leading newline (the append-to-empty case). Pins the leading
        // newline so installers don't accidentally drop it.
        let got = replace_marked_block("", &CMUX_MARKERS, "skill");
        assert_eq!(got, "\n<!-- CMUX-START -->\nskill\n<!-- CMUX-END -->\n");
    }

    // ---- path_kind / is_user_path ----

    #[test]
    fn path_kind_classifies_project_global_and_system() {
        // Happy path: relative `.`-prefixed -> Project, absolute under
        // home -> Global, absolute outside home -> System; and
        // is_user_path is true for the user kinds, false for System.
        let home = Path::new("/home/user");

        let project = Path::new(".codex/hooks.json");
        assert_eq!(path_kind(project, Some(home)), PathKind::Project);
        assert!(is_user_path(project, Some(home)));

        let global = Path::new("/home/user/.codex/hooks.json");
        assert_eq!(path_kind(global, Some(home)), PathKind::Global);
        assert!(is_user_path(global, Some(home)));

        let system = Path::new("/etc/cmux/config.toml");
        assert_eq!(path_kind(system, Some(home)), PathKind::System);
        assert!(!is_user_path(system, Some(home)));
    }

    #[test]
    fn path_kind_literal_tilde_is_not_expanded() {
        // Error/edge path: a literal "~/..." path is NOT recognized as
        // Global, because std::path::Path does not expand `~`. It is
        // relative and not `.`-prefixed, so with a home set it falls to
        // System. This pins the "callers must pre-expand tilde" contract
        // documented on path_kind.
        let home = Path::new("/home/user");
        let tilde = Path::new("~/.codex/hooks.json");
        assert_eq!(path_kind(tilde, Some(home)), PathKind::System);
        assert!(!is_user_path(tilde, Some(home)));
    }
}
