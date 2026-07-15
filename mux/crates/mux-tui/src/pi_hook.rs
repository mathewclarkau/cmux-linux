use std::fs;
use std::path::PathBuf;

use crate::hook_merge;

fn extension_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".pi").join("agent").join("extensions").join("cmux.ts"))
    } else {
        Some(PathBuf::from(".pi").join("extensions").join("cmux.ts"))
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
            eprintln!("cmux: usage: cmux pi <install-hooks|install-skill> [--uninstall] [--global]");
            2
        }
    }
}

fn run_install(uninstall: bool, global: bool) -> i32 {
    let Some(path) = extension_path(global) else {
        eprintln!("error: could not resolve home directory for global extensions");
        return 1;
    };

    if uninstall {
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("error removing {}: {e}", path.display());
                return 1;
            }
            println!("Successfully removed cmux extension from {}", path.display());
        } else {
            println!("No cmux extension found at {}", path.display());
        }
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let code = r#"import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { execFile } from "child_process";

export default function cmuxExtension(pi: ExtensionAPI) {
  const report = (state: string) => {
    const surface = process.env.CMUX_MUX_SURFACE;
    if (surface) {
      // Use execFile (arg array) instead of exec (shell string) so the
      // surface id is never passed through a shell parser. Defends
      // against future callers that may set CMUX_MUX_SURFACE from
      // untrusted input.
      execFile(
        "cmux",
        ["report-agent", "--surface", surface, "--state", state, "--source", "pi"],
        (err) => {
          // Silent error
        }
      );
    }
  };

  pi.on("tool_call", () => {
    report("working");
  });

  pi.on("session_shutdown", () => {
    report("done");
  });
}
"#;

        if let Err(e) = fs::write(&path, code) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux extension into {}", path.display());
        0
    }
}

fn skill_path(global: bool) -> Option<std::path::PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".pi").join("agent").join("APPEND_SYSTEM.md"))
    } else {
        Some(std::path::PathBuf::from(".pi").join("APPEND_SYSTEM.md"))
    }
}

fn run_install_skill(uninstall: bool, global: bool) -> i32 {
    let Some(path) = skill_path(global) else {
        eprintln!("error: could not resolve home directory");
        return 1;
    };

    if uninstall {
        if !path.exists() {
            println!("No APPEND_SYSTEM.md found at {}", path.display());
            return 0;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error reading {}: {e}", path.display());
                return 1;
            }
        };

        // Strip the cmux-managed block (marker lines and inter-block
        // content dropped, everything outside kept). strip_marked_block
        // already trim_end()s, matching the old
        // `new_content.trim_end().to_string() + "\n"` exactly.
        let new_content =
            hook_merge::strip_marked_block(&content, &hook_merge::CMUX_MARKERS) + "\n";
        if new_content == "\n" {
            let _ = fs::remove_file(&path);
            println!("Removed empty APPEND_SYSTEM.md at {}", path.display());
        } else {
            if let Err(e) = fs::write(&path, new_content) {
                eprintln!("error writing {}: {e}", path.display());
                return 1;
            }
            println!("Successfully removed cmux skill from {}", path.display());
        }
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let content = if path.exists() {
            fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };

        // Strip any existing cmux block from anywhere in the file, then
        // append a fresh block at the end (the original strip-then-append
        // behavior, NOT replace-in-place). Trailing whitespace is trimmed
        // before appending so there is a single newline before the block —
        // the old install path left a blank line here (it did not trim,
        // while the uninstall path did); this matches the uninstall path
        // and avoids blank-line drift on repeated installs.
        let cleaned = hook_merge::replace_marked_block(
            &content,
            &hook_merge::CMUX_MARKERS,
            crate::skill_content::ORCHESTRATION_SKILL,
        );

        if let Err(e) = fs::write(&path, cleaned) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux skill into {}", path.display());
        0
    }
}
