use std::fs;
use std::path::PathBuf;

use crate::hook_merge;

/// The TypeScript plugin content that reports agent state to mtyx.
/// Installed at `.opencode/plugin/mtyx.ts` (project) or
/// `~/.config/opencode/plugin/mtyx.ts` (global).
const MTYX_PLUGIN: &str = r#"// MTYX-START
// mtyx agent-state reporting plugin for opencode
// Installed by: mtyx opencode install-hooks
// Removed by: mtyx opencode install-hooks --uninstall
import { exec } from "node:child_process"

function reportAgent(state: string) {
  const surface = process.env.MTYX_MUX_SURFACE
  if (!surface) return
  exec(`mtyx report-agent --surface ${surface} --state ${state} --source hook`)
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
// MTYX-END
"#;

fn plugin_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".config").join("opencode").join("plugin").join("mtyx.ts"))
    } else {
        Some(PathBuf::from(".opencode").join("plugin").join("mtyx.ts"))
    }
}

fn skill_path(global: bool) -> Option<PathBuf> {
    let base = if global {
        mux_core::platform::home_dir()?.join(".config").join("opencode").join("skills")
    } else {
        PathBuf::from(".opencode").join("skills")
    };
    Some(base.join("mtyx-orchestration").join("SKILL.md"))
}

fn hotfix_skill_path(global: bool) -> Option<PathBuf> {
    let base = if global {
        mux_core::platform::home_dir()?.join(".config").join("opencode").join("skills")
    } else {
        PathBuf::from(".opencode").join("skills")
    };
    Some(base.join("mtyx-hotfix-race").join("SKILL.md"))
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
            eprintln!("mtyx: usage: mtyx opencode <install-hooks|install-skill> [--uninstall] [--global]");
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
        // If the file contains only our MTYX block, remove it entirely.
        // Otherwise, strip the MTYX-START..MTYX-END block.
        if !path.exists() {
            println!("No opencode plugin found at {}", path.display());
            return 0;
        }
        match fs::read_to_string(&path) {
            Ok(content) => {
                if content.trim() == MTYX_PLUGIN.trim() {
                    if let Err(e) = fs::remove_file(&path) {
                        eprintln!("error removing {}: {e}", path.display());
                        return 1;
                    }
                } else {
                    let stripped = hook_merge::strip_marked_block(&content, &hook_merge::Markers {
                        start: "MTYX-START",
                        end: "MTYX-END",
                    });
                    if let Err(e) = fs::write(&path, stripped) {
                        eprintln!("error writing {}: {e}", path.display());
                        return 1;
                    }
                }
                println!("Successfully removed mtyx plugin from {}", path.display());
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
        if let Err(e) = fs::write(&path, MTYX_PLUGIN) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed mtyx plugin into {}", path.display());
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
        println!("Removed {removed} mtyx skill(s) from opencode");
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
        println!("Successfully installed {installed} mtyx skill(s) into opencode");
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
        assert!(MTYX_PLUGIN.contains("MTYX-START"));
        assert!(MTYX_PLUGIN.contains("MTYX-END"));
    }

    #[test]
    fn plugin_content_reports_agent_state() {
        assert!(MTYX_PLUGIN.contains("report-agent"));
        assert!(MTYX_PLUGIN.contains("working"));
        assert!(MTYX_PLUGIN.contains("idle"));
        assert!(MTYX_PLUGIN.contains("--source hook"));
    }
}