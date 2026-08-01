use std::fs;
use std::path::PathBuf;

use crate::hook_merge;

/// The TypeScript plugin content that reports agent state to cmux.
/// Installed at `.opencode/plugin/cmux.ts` (project) or
/// `~/.config/opencode/plugin/cmux.ts` (global).
const CMUX_PLUGIN: &str = r#"// CMUX-START
// cmux agent-state reporting plugin for opencode
// Installed by: cmux opencode install-hooks
// Removed by: cmux opencode install-hooks --uninstall
import { exec } from "node:child_process"

function reportAgent(state: string) {
  const surface = process.env.CMUX_MUX_SURFACE
  if (!surface) return
  exec(`cmux report-agent --surface ${surface} --state ${state} --source opencode`)
}

export default async () => {
  return {
    "tool.execute.before": async () => {
      reportAgent("working")
    },
    "tool.execute.after": async () => {
      reportAgent("idle")
    },
  }
}
// CMUX-END
"#;

fn plugin_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".config").join("opencode").join("plugin").join("cmux.ts"))
    } else {
        Some(PathBuf::from(".opencode").join("plugin").join("cmux.ts"))
    }
}

fn skill_path(global: bool) -> Option<PathBuf> {
    let base = if global {
        mux_core::platform::home_dir()?.join(".config").join("opencode").join("skills")
    } else {
        PathBuf::from(".opencode").join("skills")
    };
    Some(base.join("cmux-orchestration").join("SKILL.md"))
}

fn hotfix_skill_path(global: bool) -> Option<PathBuf> {
    let base = if global {
        mux_core::platform::home_dir()?.join(".config").join("opencode").join("skills")
    } else {
        PathBuf::from(".opencode").join("skills")
    };
    Some(base.join("cmux-hotfix-race").join("SKILL.md"))
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
            eprintln!("cmux: usage: cmux opencode <install-hooks|install-skill> [--uninstall] [--global]");
            2
        }
    }
}

fn run_install(uninstall: bool, global: bool) -> i32 {
    let Some(path) = plugin_path(global) else {
        eprintln!("error: could not resolve home directory for global plugin");
        return 1;
    };

    if uninstall {
        // If the file contains only our CMUX block, remove it entirely.
        // Otherwise, strip the CMUX-START..CMUX-END block.
        if !path.exists() {
            println!("No opencode plugin found at {}", path.display());
            return 0;
        }
        match fs::read_to_string(&path) {
            Ok(content) => {
                if content.trim() == CMUX_PLUGIN.trim() {
                    if let Err(e) = fs::remove_file(&path) {
                        eprintln!("error removing {}: {e}", path.display());
                        return 1;
                    }
                } else {
                    let stripped = hook_merge::strip_marked_block(&content, &hook_merge::Markers {
                        start: "CMUX-START",
                        end: "CMUX-END",
                    });
                    if let Err(e) = fs::write(&path, stripped) {
                        eprintln!("error writing {}: {e}", path.display());
                        return 1;
                    }
                }
                println!("Successfully removed cmux plugin from {}", path.display());
            }
            Err(e) => {
                eprintln!("error reading {}: {e}", path.display());
                return 1;
            }
        }
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                eprintln!(
                    "error: refusing to overwrite symlink at {}.
                     Remove it manually if you want to install the plugin.",
                    path.display()
                );
                return 1;
            }
        }
        if let Err(e) = fs::write(&path, CMUX_PLUGIN) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux plugin into {}", path.display());
        0
    }
}

fn run_install_skill(uninstall: bool, global: bool) -> i32 {
    let Some(path) = skill_path(global) else {
        eprintln!("error: could not resolve home directory");
        return 1;
    };
    let Some(hotfix_path) = hotfix_skill_path(global) else {
        eprintln!("error: could not resolve home directory");
        return 1;
    };

    if uninstall {
        let mut removed = 0;
        for p in [&path, &hotfix_path] {
            if p.exists() {
                if let Err(e) = fs::remove_file(p) {
                    eprintln!("error removing {}: {e}", p.display());
                    return 1;
                }
                if let Some(parent) = p.parent() {
                    let _ = fs::remove_dir(parent);
                    if let Some(grandparent) = parent.parent() {
                        let _ = fs::remove_dir(grandparent);
                    }
                }
                removed += 1;
            }
        }
        println!("Removed {removed} cmux skill(s) from opencode");
        0
    } else {
        let mut installed = 0;
        for (p, content) in [(&path, crate::skill_content::ORCHESTRATION_SKILL), (&hotfix_path, crate::skill_content::HOTFIX_RACE_SKILL)] {
            if let Some(parent) = p.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(meta) = fs::symlink_metadata(p) {
                if meta.file_type().is_symlink() {
                    eprintln!(
                        "error: refusing to overwrite symlink at {}.
                         Remove it manually if you want to install the skill.",
                        p.display()
                    );
                    continue;
                }
            }
            if let Err(e) = fs::write(p, content) {
                eprintln!("error writing {}: {e}", p.display());
                continue;
            }
            installed += 1;
        }
        println!("Successfully installed {installed} cmux skill(s) into opencode");
        if installed > 0 { 0 } else { 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_unknown_subcommand() {
        let code = run(&["invalid".to_string()]);
        assert_eq!(code, 2);
    }

    #[test]
    fn plugin_content_has_markers() {
        assert!(CMUX_PLUGIN.contains("CMUX-START"));
        assert!(CMUX_PLUGIN.contains("CMUX-END"));
    }

    #[test]
    fn plugin_content_reports_agent_state() {
        assert!(CMUX_PLUGIN.contains("report-agent"));
        assert!(CMUX_PLUGIN.contains("working"));
        assert!(CMUX_PLUGIN.contains("idle"));
        assert!(CMUX_PLUGIN.contains("opencode"));
    }
}