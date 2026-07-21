use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use crate::hook_merge;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GrokHook {
    pub event: String,
    pub command: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct GrokHooksConfig {
    #[serde(default)]
    pub hooks: Vec<GrokHook>,
}

fn config_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".grok").join("hooks.json"))
    } else {
        Some(PathBuf::from(".grok").join("hooks.json"))
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
        let mut config: GrokHooksConfig = match hook_merge::load_json(&path) {
            Ok(c) => c,
            Err(hook_merge::LoadError::NotFound) => {
                println!("No Grok hooks file found at {}", path.display());
                return 0;
            }
            Err(hook_merge::LoadError::Parse(e)) => {
                eprintln!(
                    "error: malformed Grok config at {}: {e}",
                    path.display()
                );
                return 1;
            }
            Err(hook_merge::LoadError::Io(e)) => {
                eprintln!("error reading {}: {e}", path.display());
                return 1;
            }
        };
        config.hooks.retain(|h| !h.command.contains("cmux report-agent"));

        if let Err(e) = hook_merge::save_pretty(&path, &config) {
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
        println!("Successfully removed cmux hooks from {}", path.display());
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut config = match hook_merge::load_or_default::<GrokHooksConfig>(&path) {
            Ok(c) => c,
            Err(hook_merge::LoadError::Parse(e)) => {
                eprintln!(
                    "error: malformed Grok config at {}: {e}",
                    path.display()
                );
                return 1;
            }
            Err(hook_merge::LoadError::Io(e)) => {
                eprintln!("error reading {}: {e}", path.display());
                return 1;
            }
            Err(hook_merge::LoadError::NotFound) => GrokHooksConfig::default(),
        };

        // Remove any existing cmux hooks to avoid duplicates
        config.hooks.retain(|h| !h.command.contains("cmux report-agent"));

        // Add fresh ones
        config.hooks.push(GrokHook {
            event: "PreToolUse".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source grok".to_string(),
        });
        config.hooks.push(GrokHook {
            event: "PostToolUse".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state idle --source grok".to_string(),
        });
        config.hooks.push(GrokHook {
            event: "Stop".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state done --source grok".to_string(),
        });

        if let Err(e) = hook_merge::save_pretty(&path, &config) {
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
        println!("Successfully installed cmux hooks into {}", path.display());
        0
    }
}

fn skill_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".grok").join("skills").join("cmux-orchestration").join("SKILL.md"))
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
    fn test_grok_hooks_config_serialization() {
        let mut config = GrokHooksConfig::default();
        config.hooks.push(GrokHook {
            event: "PreToolUse".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source grok".to_string(),
        });
        let json = serde_json::to_string(&config).unwrap();
        let parsed: GrokHooksConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_run_unknown_subcommand() {
        let code = run(&["invalid".to_string()]);
        assert_eq!(code, 2);
    }
}
