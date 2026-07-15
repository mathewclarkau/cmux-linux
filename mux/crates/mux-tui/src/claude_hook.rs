//! `cmux claude ...` — Claude Code hook integration.
//!
//! `install-hooks` points Claude Code's own hook config at `cmux claude
//! hook`. Claude invokes that on every lifecycle event with a JSON payload
//! on stdin; `hook` reports agent state over the pane's own control socket
//! (found via `$CMUX_MUX_SOCKET`/`$CMUX_MUX_SURFACE`, set on every pty
//! child — see `Surface::spawn` in mux-core) and records the session in a
//! local store that `sessions`/`resume` read back. A hook must never block
//! or fail Claude Code's own turn, so every path here exits 0.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use mux_core::platform::transport;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_SESSIONS: usize = 100;

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("hook") => run_hook(),
        Some("install-hooks") => {
            run_install_hooks(args.get(1).map(String::as_str) == Some("--uninstall"))
        }
        Some("install-skill") => {
            let uninstall = args.iter().any(|arg| arg == "--uninstall");
            let global = args.iter().any(|arg| arg == "--global");
            run_install_skill(uninstall, global)
        }
        Some("sessions") => run_sessions(),
        Some("resume") => run_resume(args.get(1).map(String::as_str)),
        _ => {
            eprintln!(
                "cmux: usage: cmux claude <hook|install-hooks [--uninstall]|install-skill [--uninstall] [--global]|sessions|resume [session-id]>"
            );
            2
        }
    }
}

// ---------- hook ----------

#[derive(Deserialize, Default)]
struct HookPayload {
    session_id: Option<String>,
    cwd: Option<String>,
    hook_event_name: Option<String>,
}

fn run_hook() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let payload: HookPayload = match serde_json::from_str(&input) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cmux: malformed hook payload from stdin: {e}");
            return 1;
        }
    };
    let event = payload.hook_event_name.as_deref();

    if let Some(session_id) = &payload.session_id {
        record_session(session_id, payload.cwd.as_deref(), event);
    }

    if let (Some(state), Some(surface)) = (event.and_then(agent_state_for_event), surface_id()) {
        let mut params = json!({ "surface": surface, "state": state, "source": "hook" });
        if let Some(session_id) = &payload.session_id {
            params["session"] = json!(session_id);
        }
        // Best-effort: a dead socket or unknown surface must not fail the hook.
        let _ = send_request("report-agent", params);
    }

    0
}

fn agent_state_for_event(event: &str) -> Option<&'static str> {
    Some(match event {
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => "working",
        "Notification" => "blocked",
        "Stop" | "SubagentStop" => "idle",
        "SessionEnd" => "done",
        _ => return None,
    })
}

fn surface_id() -> Option<u64> {
    std::env::var("CMUX_MUX_SURFACE").ok()?.parse().ok()
}

// ---------- socket client ----------
// A minimal request/response round trip, independent of the `cli` module:
// `hook` needs to never fail loudly, and `resume` needs the response data
// (the new surface id), which the `cli` module only ever prints.

fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CMUX_MUX_SOCKET") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    mux_core::server::default_socket_path("main")
}

fn send_request(cmd: &str, mut params: Value) -> Option<Value> {
    let stream = transport::connect(&socket_path()).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    params["cmd"] = json!(cmd);
    params["id"] = json!(1);
    let mut line = serde_json::to_vec(&params).ok()?;
    line.push(b'\n');
    let mut writer = stream.try_clone_box().ok()?;
    writer.write_all(&line).ok()?;
    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).ok()?;
    let response: Value = serde_json::from_str(&response_line).ok()?;
    (response.get("ok").and_then(Value::as_bool) == Some(true))
        .then(|| response.get("data").cloned())
        .flatten()
}

// ---------- session store ----------
// `$XDG_STATE_HOME/cmux-mux/claude-sessions.json`, most-recent-first,
// deduplicated by session_id, capped at MAX_SESSIONS. Locked with flock
// for the read-modify-write since multiple panes' hooks can fire
// concurrently.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    last_event: Option<String>,
    updated_at_ms: u64,
}

fn store_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| mux_core::platform::home_dir().map(|home| home.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cmux-mux").join("claude-sessions.json")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Locks the store file for the duration of `f`, which reads the current
/// records, mutates them, and returns whatever the caller wants back.
///
/// Returns `None` if the store file is corrupted JSON — the existing
/// records are left untouched on disk (no silent wipe, no schema-drift
/// data loss). Callers handle the None as "treat as no records".
fn with_locked_store<T>(f: impl FnOnce(&mut Vec<SessionRecord>) -> T) -> Option<T> {
    let path = store_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    let mut file =
        std::fs::OpenOptions::new().read(true).write(true).create(true).open(&path).ok()?;
    let fd = file.as_raw_fd();
    // SAFETY: fd is a valid, open file descriptor for the lifetime of this
    // call; flock is released explicitly below and also on process exit.
    unsafe {
        libc::flock(fd, libc::LOCK_EX);
    }
    let mut contents = String::new();
    let _ = file.read_to_string(&mut contents);
    // Fail loud on a corrupted sessions file: silently wiping it with
    // `unwrap_or_default()` would let a partial write (or a manual
    // user edit gone wrong, or a schema drift from a future version)
    // delete the user's recorded session history on the next write.
    // The file is left untouched on disk; the caller treats None as
    // "no records this run".
    if contents.trim().is_empty() {
        // Treat empty file as the no-records-yet case rather than an error.
    } else {
        let _: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "cmux: {} is not valid JSON ({e}); leaving it untouched",
                    path.display()
                );
                unsafe {
                    libc::flock(fd, libc::LOCK_UN);
                }
                return None;
            }
        };
    }
    let mut records: Vec<SessionRecord> = serde_json::from_str(&contents).unwrap_or_default();

    let result = f(&mut records);
    records.truncate(MAX_SESSIONS);

    if let Ok(json) = serde_json::to_string_pretty(&records) {
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = file.write_all(json.as_bytes());
    }
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
    }
    Some(result)
}

fn record_session(session_id: &str, cwd: Option<&str>, event: Option<&str>) {
    with_locked_store(|records| {
        records.retain(|r| r.session_id != session_id);
        records.insert(
            0,
            SessionRecord {
                session_id: session_id.to_string(),
                cwd: cwd.map(str::to_string),
                last_event: event.map(str::to_string),
                updated_at_ms: now_ms(),
            },
        );
    });
}

fn load_sessions() -> Vec<SessionRecord> {
    with_locked_store(|records| records.clone()).unwrap_or_default()
}

// ---------- sessions / resume ----------

fn run_sessions() -> i32 {
    let records = load_sessions();
    if records.is_empty() {
        println!("no recorded claude sessions");
        return 0;
    }
    for record in &records {
        println!(
            "{}  {}  {}",
            record.session_id,
            record.cwd.as_deref().unwrap_or("-"),
            record.last_event.as_deref().unwrap_or("-"),
        );
    }
    0
}

fn run_resume(session_id: Option<&str>) -> i32 {
    let records = load_sessions();
    let Some(session_id) = session_id else {
        eprintln!("cmux: usage: cmux claude resume <session-id>");
        if !records.is_empty() {
            eprintln!("recorded sessions:");
            for record in &records {
                eprintln!("  {}  {}", record.session_id, record.cwd.as_deref().unwrap_or("-"));
            }
        }
        return 2;
    };
    let Some(record) = records.iter().find(|r| r.session_id.starts_with(session_id)) else {
        eprintln!("cmux: no recorded session matching {session_id:?}");
        return 1;
    };

    let mut new_tab_params = json!({});
    if let Some(cwd) = &record.cwd {
        new_tab_params["cwd"] = json!(cwd);
    }
    let Some(data) = send_request("new-tab", new_tab_params) else {
        eprintln!("cmux: failed to create a pane (is a session running?)");
        return 1;
    };
    let Some(surface) = data.get("surface").and_then(Value::as_u64) else {
        eprintln!("cmux: new-tab did not return a surface id");
        return 1;
    };

    let resume_command = format!("claude --resume {}\n", record.session_id);
    let send_params = json!({ "surface": surface, "text": resume_command });
    if send_request("send", send_params).is_none() {
        eprintln!("cmux: created pane {surface} but failed to launch claude --resume");
        return 1;
    }
    println!("{surface}");
    0
}

// ---------- install-hooks ----------
// Claude Code hook settings live in `~/.claude/settings.json` under a
// "hooks" key, e.g.:
//   "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "..." } ] } ] }
// Installing merges our command into every event we care about without
// touching any other hooks the user already configured for those events.

const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

fn claude_settings_path() -> Option<PathBuf> {
    Some(mux_core::platform::home_dir()?.join(".claude").join("settings.json"))
}

fn hook_command() -> String {
    let bin = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "cmux".to_string());
    format!("{bin} claude hook")
}

fn run_install_hooks(uninstall: bool) -> i32 {
    let Some(path) = claude_settings_path() else {
        eprintln!("cmux: could not resolve $HOME to find ~/.claude/settings.json");
        return 1;
    };
    let mut settings: Value = if path.exists() {
        match std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()) {
            Some(value) => value,
            None => {
                eprintln!("cmux: {} exists but is not valid JSON; not touching it", path.display());
                return 1;
            }
        }
    } else {
        json!({})
    };
    if !settings.is_object() {
        eprintln!("cmux: {} does not contain a JSON object at the top level", path.display());
        return 1;
    }

    let command = hook_command();
    let hooks = settings.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        eprintln!("cmux: {}'s \"hooks\" key is not an object; not touching it", path.display());
        return 1;
    }
    let hooks = hooks.as_object_mut().unwrap();

    // Match by this suffix, not the full `command` string, so a rebuilt or
    // renamed binary (different absolute path, same `claude hook` verb)
    // still recognizes and replaces its own previously-installed entry
    // instead of accumulating a duplicate that points at a since-deleted
    // path. `antigravity_hook.rs`/`codex_hook.rs`/`pi_hook.rs` already use
    // an equivalent substring match (`contains("cmux report-agent")`) for
    // the same reason.
    const HOOK_MARKER: &str = "claude hook";
    let is_our_hook = |cmd: &str| cmd.ends_with(HOOK_MARKER);

    for event in HOOK_EVENTS {
        if uninstall {
            // Only touch events the user already has an entry for; don't
            // manufacture new empty ones just to immediately remove them.
            let Some(entries) = hooks.get_mut(*event).and_then(Value::as_array_mut) else {
                continue;
            };
            for group in entries.iter_mut() {
                if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    list.retain(|h| {
                        !h.get("command").and_then(Value::as_str).is_some_and(is_our_hook)
                    });
                }
            }
            entries.retain(|group| {
                group.get("hooks").and_then(Value::as_array).map_or(true, |list| !list.is_empty())
            });
            if entries.is_empty() {
                hooks.remove(*event);
            }
        } else {
            let entries = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
            let Some(entries) = entries.as_array_mut() else {
                eprintln!(
                    "cmux: {}'s hooks.{event} is not an array; leaving it alone",
                    path.display()
                );
                continue;
            };
            // Drop any prior installation of this hook (however it got its
            // absolute path) before adding the current one, so re-running
            // install-hooks after a rebuild/rename replaces in place instead
            // of accumulating a stale, now-broken duplicate.
            for group in entries.iter_mut() {
                if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    list.retain(|h| {
                        !h.get("command").and_then(Value::as_str).is_some_and(is_our_hook)
                    });
                }
            }
            entries.retain(|group| {
                group.get("hooks").and_then(Value::as_array).map_or(true, |list| !list.is_empty())
            });
            entries.push(json!({ "hooks": [{ "type": "command", "command": command }] }));
        }
    }

    if let Some(dir) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            eprintln!("cmux: failed to create {}: {err}", dir.display());
            return 1;
        }
    }
    let Ok(pretty) = serde_json::to_string_pretty(&settings) else {
        eprintln!("cmux: failed to serialize {}", path.display());
        return 1;
    };
    if let Err(err) = std::fs::write(&path, pretty + "\n") {
        eprintln!("cmux: failed to write {}: {err}", path.display());
        return 1;
    }

    println!(
        "{} hooks in {}",
        if uninstall { "removed" } else { "installed" },
        path.display()
    );
    0
}

fn skill_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| {
            h.join(".claude")
                .join("skills")
                .join("cmux-orchestration")
                .join("SKILL.md")
        })
    } else {
        Some(
            PathBuf::from(".claude")
                .join("skills")
                .join("cmux-orchestration")
                .join("SKILL.md"),
        )
    }
}

fn run_install_skill(uninstall: bool, global: bool) -> i32 {
    let Some(path) = skill_path(global) else {
        eprintln!("error: could not resolve home directory");
        return 1;
    };

    if uninstall {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("error removing {}: {e}", path.display());
                return 1;
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent);
                if let Some(grandparent) = parent.parent() {
                    let _ = std::fs::remove_dir(grandparent);
                }
            }
            println!("Successfully removed cmux skill from {}", path.display());
        } else {
            println!("No cmux skill found at {}", path.display());
        }
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Refuse to overwrite a symlink: same rationale as aider_hook.rs
        // (PR #3 hardening) — fs::write on a symlink path overwrites the
        // symlink target, not the symlink itself. An attacker-placed symlink
        // in a user-writable target path could redirect the write to an
        // arbitrary file.
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                eprintln!(
                    "error: refusing to overwrite symlink at {}.                      Remove it manually if you want to install the skill.",
                    path.display()
                );
                return 1;
            }
        }
        if let Err(e) = std::fs::write(&path, crate::skill_content::ORCHESTRATION_SKILL) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux skill into {}", path.display());
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// HOME/XDG_STATE_HOME are process-global; tests that set them must
    /// not run concurrently with each other (mirrors config.rs's pattern).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn agent_state_maps_known_events_and_ignores_unknown() {
        assert_eq!(agent_state_for_event("SessionStart"), Some("working"));
        assert_eq!(agent_state_for_event("UserPromptSubmit"), Some("working"));
        assert_eq!(agent_state_for_event("PreToolUse"), Some("working"));
        assert_eq!(agent_state_for_event("PostToolUse"), Some("working"));
        assert_eq!(agent_state_for_event("Notification"), Some("blocked"));
        assert_eq!(agent_state_for_event("Stop"), Some("idle"));
        assert_eq!(agent_state_for_event("SubagentStop"), Some("idle"));
        assert_eq!(agent_state_for_event("SessionEnd"), Some("done"));
        assert_eq!(agent_state_for_event("SomethingFuture"), None);
    }

    fn temp_state_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("claude-hook-test-{name}-{}", std::process::id()))
    }

    #[test]
    fn record_session_dedups_and_orders_most_recent_first() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = temp_state_dir("sessions");
        std::env::set_var("XDG_STATE_HOME", &dir);

        record_session("sess-a", Some("/proj/a"), Some("SessionStart"));
        record_session("sess-b", Some("/proj/b"), Some("SessionStart"));
        // Re-reporting an existing session updates it in place and moves
        // it to the front, rather than duplicating it.
        record_session("sess-a", Some("/proj/a"), Some("Stop"));

        let sessions = load_sessions();
        std::env::remove_var("XDG_STATE_HOME");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(sessions.len(), 2, "re-reporting sess-a must not duplicate it");
        assert_eq!(sessions[0].session_id, "sess-a");
        assert_eq!(sessions[0].last_event.as_deref(), Some("Stop"));
        assert_eq!(sessions[1].session_id, "sess-b");
    }

    #[test]
    fn install_hooks_preserves_existing_hooks_and_is_idempotent() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = temp_state_dir("install");
        let claude_dir = dir.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"model":"sonnet","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo existing"}]}]}}"#,
        )
        .unwrap();
        std::env::set_var("HOME", &dir);

        assert_eq!(run_install_hooks(false), 0);
        assert_eq!(run_install_hooks(false), 0, "installing twice must not duplicate entries");

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
                .unwrap();
        let stop_entries = settings["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 2, "existing hook preserved, ours appended, not duplicated");
        assert_eq!(settings["model"], "sonnet", "unrelated settings must survive untouched");
        for event in HOOK_EVENTS {
            if *event == "Stop" {
                continue;
            }
            assert_eq!(settings["hooks"][event].as_array().unwrap().len(), 1);
        }

        assert_eq!(run_install_hooks(true), 0);
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
                .unwrap();
        let stop_entries = settings["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop_entries.len(), 1, "uninstall removes ours, keeps the pre-existing hook");
        assert!(
            settings["hooks"].get("Notification").is_none(),
            "events left with no hooks after uninstall should be pruned, not left as []"
        );

        std::env::remove_var("HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_hooks_replaces_a_stale_entry_from_a_different_binary_path() {
        // Regression test: reinstalling after the binary moved/was renamed
        // (a different absolute path, same `claude hook` command) must
        // replace the stale entry, not add a second one alongside it - this
        // is exactly what happened live when cmux-mux was renamed to cmux
        // and the old absolute path stopped existing.
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = temp_state_dir("install-stale-path");
        let claude_dir = dir.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/old/deleted/path/cmux-mux claude hook"}]}]}}"#,
        )
        .unwrap();
        std::env::set_var("HOME", &dir);

        assert_eq!(run_install_hooks(false), 0);

        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
                .unwrap();
        let stop_entries = settings["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(
            stop_entries.len(),
            1,
            "the stale entry must be replaced in place, not left alongside a new one"
        );
        let command = stop_entries[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            !command.contains("/old/deleted/path/"),
            "the surviving command must be the current binary's path, not the stale one: {command}"
        );

        std::env::remove_var("HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
