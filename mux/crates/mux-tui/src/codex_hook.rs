use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CodexHook {
    pub command: String,
    #[serde(rename = "statusMessage", default)]
    pub status_message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct CodexHooksConfig {
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<CodexHook>>,
}

fn paths(global: bool) -> Option<(PathBuf, PathBuf)> {
    let home = mux_core::platform::home_dir()?;
    if global {
        let codex_dir = home.join(".codex");
        Some((codex_dir.join("hooks.json"), codex_dir.join("config.toml")))
    } else {
        let codex_dir = PathBuf::from(".codex");
        Some((codex_dir.join("hooks.json"), codex_dir.join("config.toml")))
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
            eprintln!("cmux-mux: usage: cmux-mux codex <install-hooks|install-skill> [--uninstall] [--global]");
            2
        }
    }
}

fn run_install(uninstall: bool, global: bool) -> i32 {
    let Some((hooks_path, config_path)) = paths(global) else {
        eprintln!("error: could not resolve home directory for global hooks");
        return 1;
    };

    if uninstall {
        if hooks_path.exists() {
            let content = match fs::read_to_string(&hooks_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error reading {}: {e}", hooks_path.display());
                    return 1;
                }
            };
            let mut config: CodexHooksConfig = serde_json::from_str(&content).unwrap_or_default();
            for hooks_list in config.hooks.values_mut() {
                hooks_list.retain(|h| !h.command.contains("cmux-mux report-agent"));
            }
            // Retain only events that still have hooks
            config.hooks.retain(|_, v| !v.is_empty());

            let new_content = match serde_json::to_string_pretty(&config) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: failed to serialize config: {e}");
                    return 1;
                }
            };
            if let Err(e) = fs::write(&hooks_path, new_content) {
                eprintln!("error writing {}: {e}", hooks_path.display());
                return 1;
            }
            println!("Successfully removed cmux hooks from {}", hooks_path.display());
        }
        0
    } else {
        if let Some(parent) = hooks_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        // 1. Setup config.toml features. Only add `codex_hooks = true` to
        // an actual `[features]` section header — never to a substring
        // match that might be inside a comment or a longer key like
        // `docs.codex_hooks`. Done by walking lines.
        let mut config_content = if config_path.exists() {
            match fs::read_to_string(&config_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warning: could not read {}: {e}", config_path.display());
                    String::new()
                }
            }
        } else {
            String::new()
        };

        let already_featured = config_content
            .lines()
            .any(|l| l.trim() == "codex_hooks = true");
        if !already_featured {
            if let Some(features_idx) = config_content
                .lines()
                .position(|l| l.trim() == "[features]")
            {
                // Find the next blank line or section header after [features],
                // insert after that. Default to appending at the section.
                let mut new_lines: Vec<String> = config_content.lines().map(String::from).collect();
                let insert_at = features_idx + 1;
                new_lines.insert(insert_at, "codex_hooks = true".to_string());
                config_content = new_lines.join("\n");
                if !config_content.ends_with('\n') {
                    config_content.push('\n');
                }
            } else {
                if !config_content.ends_with('\n') && !config_content.is_empty() {
                    config_content.push('\n');
                }
                config_content.push_str("\n[features]\ncodex_hooks = true\n");
            }
            if let Err(e) = fs::write(&config_path, &config_content) {
                eprintln!("error: could not update {}: {e}", config_path.display());
                return 1;
            }
        }

        // 2. Setup hooks.json — fail-loud on malformed config.
        let mut config = if hooks_path.exists() {
            let content = match fs::read_to_string(&hooks_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error reading {}: {e}", hooks_path.display());
                    return 1;
                }
            };
            match serde_json::from_str::<CodexHooksConfig>(&content) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "error: malformed Codex hooks config at {}: {e}",
                        hooks_path.display()
                    );
                    return 1;
                }
            }
        } else {
            CodexHooksConfig::default()
        };

        // Clear existing cmux hooks
        for hooks_list in config.hooks.values_mut() {
            hooks_list.retain(|h| !h.command.contains("cmux-mux report-agent"));
        }

        let new_hooks = vec![
            ("PreToolUse", "cmux-mux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source codex"),
            ("PostToolUse", "cmux-mux report-agent --surface \"$CMUX_MUX_SURFACE\" --state idle --source codex"),
            ("Stop", "cmux-mux report-agent --surface \"$CMUX_MUX_SURFACE\" --state done --source codex"),
        ];

        for (event, command) in new_hooks {
            config.hooks.entry(event.to_string()).or_insert_with(Vec::new).push(CodexHook {
                command: command.to_string(),
                status_message: Some("Reporting state to cmux".to_string()),
            });
        }

        let new_content = match serde_json::to_string_pretty(&config) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: failed to serialize config: {e}");
                return 1;
            }
        };
        if let Err(e) = fs::write(&hooks_path, new_content) {
            eprintln!("error writing {}: {e}", hooks_path.display());
            return 1;
        }
        println!("Successfully installed cmux hooks into {}", hooks_path.display());
        0
    }
}

fn skill_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".codex").join("skills").join("cmux-orchestration").join("SKILL.md"))
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
