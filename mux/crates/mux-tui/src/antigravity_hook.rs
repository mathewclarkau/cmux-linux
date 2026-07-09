use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

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
        _ => {
            eprintln!("cmux-mux: usage: cmux-mux antigravity install-hooks [--uninstall] [--global]");
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
        if !path.exists() {
            println!("No Antigravity hooks file found at {}", path.display());
            return 0;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error reading {}: {e}", path.display());
                return 1;
            }
        };
        let mut config: AntigravityHooksConfig = serde_json::from_str(&content).unwrap_or_default();
        config.hooks.retain(|h| !h.command.contains("cmux-mux report-agent"));
        
        let new_content = serde_json::to_string_pretty(&config).unwrap();
        if let Err(e) = fs::write(&path, new_content) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully removed cmux hooks from {}", path.display());
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut config = if path.exists() {
            let content = fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str::<AntigravityHooksConfig>(&content).unwrap_or_default()
        } else {
            AntigravityHooksConfig::default()
        };

        // Remove any existing cmux hooks to avoid duplicates
        config.hooks.retain(|h| !h.command.contains("cmux-mux report-agent"));

        // Add fresh ones
        config.hooks.push(AntigravityHook {
            event: "PreToolUse".to_string(),
            command: "cmux-mux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source antigravity".to_string(),
        });
        config.hooks.push(AntigravityHook {
            event: "PostToolUse".to_string(),
            command: "cmux-mux report-agent --surface \"$CMUX_MUX_SURFACE\" --state idle --source antigravity".to_string(),
        });
        config.hooks.push(AntigravityHook {
            event: "Stop".to_string(),
            command: "cmux-mux report-agent --surface \"$CMUX_MUX_SURFACE\" --state done --source antigravity".to_string(),
        });

        let new_content = serde_json::to_string_pretty(&config).unwrap();
        if let Err(e) = fs::write(&path, new_content) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux hooks into {}", path.display());
        0
    }
}
