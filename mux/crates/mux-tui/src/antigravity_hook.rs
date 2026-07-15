use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use crate::hook_merge;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AntigravityHook {
    pub event: String,
    pub command: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct AntigravityHooksConfig {
    #[serde(default)]
    pub hooks: Vec<AntigravityHook>,
}

fn config_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".gemini").join("config").join("hooks.json"))
    } else {
        Some(PathBuf::from(".agents").join("hooks.json"))
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
            eprintln!("cmux: usage: cmux antigravity <install-hooks|install-skill> [--uninstall] [--global]");
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
        let mut config: AntigravityHooksConfig = match hook_merge::load_json(&path) {
            Ok(c) => c,
            Err(hook_merge::LoadError::NotFound) => {
                println!("No Antigravity hooks file found at {}", path.display());
                return 0;
            }
            // Fail-loud on malformed config: silent `unwrap_or_default()`
            // would overwrite the user's real config on schema drift.
            Err(hook_merge::LoadError::Parse(e)) => {
                eprintln!(
                    "error: malformed Antigravity config at {}: {e}",
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
        let mut config = match hook_merge::load_or_default::<AntigravityHooksConfig>(&path) {
            Ok(c) => c,
            // Fail-loud on malformed config: silent `unwrap_or_default()`
            // would overwrite the user's real config on schema drift.
            Err(hook_merge::LoadError::Parse(e)) => {
                eprintln!(
                    "error: malformed Antigravity config at {}: {e}",
                    path.display()
                );
                return 1;
            }
            Err(hook_merge::LoadError::Io(e)) => {
                eprintln!("error reading {}: {e}", path.display());
                return 1;
            }
            // load_or_default converts NotFound -> Ok(default) internally,
            // so this arm is unreachable in practice; kept for exhaustiveness.
            Err(hook_merge::LoadError::NotFound) => AntigravityHooksConfig::default(),
        };

        // Remove any existing cmux hooks to avoid duplicates
        config.hooks.retain(|h| !h.command.contains("cmux report-agent"));

        // Add fresh ones
        config.hooks.push(AntigravityHook {
            event: "PreToolUse".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source antigravity".to_string(),
        });
        config.hooks.push(AntigravityHook {
            event: "PostToolUse".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state idle --source antigravity".to_string(),
        });
        config.hooks.push(AntigravityHook {
            event: "Stop".to_string(),
            command: "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state done --source antigravity".to_string(),
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
        mux_core::platform::home_dir().map(|h| h.join(".gemini").join("antigravity-cli").join("skills").join("cmux-orchestration").join("SKILL.md"))
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
        if let Err(e) = fs::write(&path, crate::skill_content::ORCHESTRATION_SKILL) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux skill into {}", path.display());
        0
    }
}
