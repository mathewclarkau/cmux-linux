use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::hook_merge;

/// Legacy flat-array schema previously written to `.grok/hooks.json`.
/// Kept so install/uninstall can clean leftovers that Grok Build never loaded.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
struct LegacyGrokHooksConfig {
    #[serde(default)]
    hooks: Vec<LegacyGrokHook>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct LegacyGrokHook {
    event: String,
    command: String,
}

const HOOK_FILENAME: &str = "cmux-agent-state.json";

/// Grok Build loads `$GROK_HOME/hooks/*.json` (and `<repo>/.grok/hooks/*.json`)
/// in the Claude-compatible object schema. The old installer wrote
/// `.grok/hooks.json` (wrong path) in a flat array (wrong shape) with
/// `--source grok` (rejected by `report-agent`, which only accepts
/// `socket` or `hook`).
pub(crate) fn config_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".grok").join("hooks").join(HOOK_FILENAME))
    } else {
        Some(PathBuf::from(".grok").join("hooks").join(HOOK_FILENAME))
    }
}

fn legacy_config_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".grok").join("hooks.json"))
    } else {
        Some(PathBuf::from(".grok").join("hooks.json"))
    }
}

fn report_command(state: &str) -> String {
    format!(
        "test -n \"$CMUX_MUX_SURFACE\" && cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state {state} --source hook || true"
    )
}

fn grok_native_hooks() -> Value {
    let command = |state: &str| {
        json!({
            "type": "command",
            "command": report_command(state),
            "timeout": 5
        })
    };
    let group = |state: &str| json!([{ "hooks": [command(state)] }]);
    json!({
        "hooks": {
            "SessionStart": group("working"),
            "PreToolUse": group("working"),
            "PostToolUse": group("idle"),
            "Notification": [{
                "matcher": "idle_prompt|permission_prompt",
                "hooks": [command("blocked")]
            }],
            "Stop": group("done"),
            "SubagentStart": group("working"),
            "SubagentStop": group("idle")
        }
    })
}

fn refuse_symlink(path: &Path) -> Option<i32> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            eprintln!(
                "error: refusing to overwrite symlink at {}. Remove it manually if you want to install the hooks.",
                path.display()
            );
            return Some(1);
        }
    }
    None
}

/// Strip leftover cmux entries from the pre-fix `.grok/hooks.json`. Deletes
/// the file when nothing else remains. Leaves an unreadable or non-legacy
/// file alone so we never clobber a user config we don't understand.
fn clean_legacy(global: bool) -> bool {
    let Some(path) = legacy_config_path(global) else {
        return false;
    };
    match hook_merge::load_json::<LegacyGrokHooksConfig>(&path) {
        Ok(mut config) => {
            let before = config.hooks.len();
            config.hooks.retain(|h| !h.command.contains("cmux report-agent"));
            if config.hooks.is_empty() {
                let _ = fs::remove_file(&path);
                before > 0
            } else if config.hooks.len() != before {
                let _ = hook_merge::save_pretty(&path, &config);
                true
            } else {
                false
            }
        }
        Err(hook_merge::LoadError::NotFound) => false,
        Err(_) => false,
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut uninstall = false;
    let mut global = false;

    for arg in args.iter().skip(1) {
        if arg == "--uninstall" {
            uninstall = true;
        } else if arg == "--global" {
            global = true;
        }
    }

    match args.first().map(String::as_str) {
        Some("install-hooks") => run_install(uninstall, global),
        Some("install-skill") => run_install_skill(uninstall, global),
        _ => {
            eprintln!("cmux: usage: cmux grok <install-hooks|install-skill> [--uninstall] [--global]");
            2
        }
    }
}

fn run_install(uninstall: bool, global: bool) -> i32 {
    let Some(path) = config_path(global) else {
        eprintln!("error: could not resolve home directory for global hooks");
        return 1;
    };

    if uninstall {
        let mut removed = false;
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("error removing {}: {e}", path.display());
                return 1;
            }
            removed = true;
        }
        let cleaned_legacy = clean_legacy(global);
        if removed || cleaned_legacy {
            println!("Successfully removed cmux hooks from {}", path.display());
        } else {
            println!("No Grok hooks file found at {}", path.display());
        }
        return 0;
    }

    if let Some(code) = refuse_symlink(&path) {
        return code;
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("error creating {}: {e}", parent.display());
            return 1;
        }
    }

    if let Err(e) = hook_merge::save_pretty(&path, &grok_native_hooks()) {
        match e {
            hook_merge::SaveError::Serialize(e) => {
                eprintln!("error: failed to serialize config: {e}");
                return 1;
            }
            hook_merge::SaveError::Io(e) => {
                eprintln!("error writing {}: {e}", path.display());
                return 1;
            }
        }
    }
    clean_legacy(global);
    println!("Successfully installed cmux hooks into {}", path.display());
    0
}

fn skill_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir()
            .map(|h| h.join(".grok").join("skills").join("cmux-orchestration").join("SKILL.md"))
    } else {
        Some(PathBuf::from(".agents").join("skills").join("cmux-orchestration").join("SKILL.md"))
    }
}

fn run_install_skill(uninstall: bool, global: bool) -> i32 {
    let Some(path) = skill_path(global) else {
        eprintln!("error: could not resolve home directory");
        return 1;
    };

    if uninstall {
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("error removing {}: {e}", path.display());
                return 1;
            }
            if let Some(parent) = path.parent() {
                let _ = fs::remove_dir(parent);
                if let Some(grandparent) = parent.parent() {
                    let _ = fs::remove_dir(grandparent);
                }
            }
            println!("Successfully removed cmux skill from {}", path.display());
        } else {
            println!("No cmux skill found at {}", path.display());
        }
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // Refuse to overwrite a symlink: same rationale as claude_hook.rs
        // (PR #18 / issue #10 hardening) and aider_hook.rs (PR #3) — fs::write
        // on a symlink path overwrites the symlink target, not the symlink
        // itself. An attacker-placed symlink in a user-writable target path
        // could redirect the write to an arbitrary file.
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                eprintln!(
                    "error: refusing to overwrite symlink at {}.                      Remove it manually if you want to install the skill.",
                    path.display()
                );
                return 1;
            }
        }
        if let Err(e) = fs::write(&path, crate::skill_content::ORCHESTRATION_SKILL) {
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

    #[test]
    fn native_hooks_use_grok_object_schema_and_hook_source() {
        let hooks = grok_native_hooks();
        let pre = &hooks["hooks"]["PreToolUse"][0]["hooks"][0];
        assert_eq!(pre["type"], "command");
        let command = pre["command"].as_str().expect("command is a string");
        assert!(command.contains("--source hook"), "{command}");
        assert!(!command.contains("--source grok"), "{command}");
        assert!(command.contains("test -n \"$CMUX_MUX_SURFACE\""), "{command}");
        assert_eq!(hooks["hooks"]["Notification"][0]["matcher"], "idle_prompt|permission_prompt");
    }

    #[test]
    fn config_path_is_the_grok_hooks_directory() {
        let project = config_path(false).expect("project path");
        assert_eq!(
            project,
            PathBuf::from(".grok").join("hooks").join("cmux-agent-state.json")
        );
    }

    #[test]
    fn test_run_unknown_subcommand() {
        let code = run(&["invalid".to_string()]);
        assert_eq!(code, 2);
    }
}
