use std::fs;
use std::path::PathBuf;

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
            eprintln!("cmux-mux: usage: cmux-mux pi <install-hooks|install-skill> [--uninstall] [--global]");
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
import { exec } from "child_process";

export default function cmuxExtension(pi: ExtensionAPI) {
  const report = (state: string) => {
    const surface = process.env.CMUX_MUX_SURFACE;
    if (surface) {
      exec(`cmux-mux report-agent --surface ${surface} --state ${state} --source pi`, (err) => {
        // Silent error
      });
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

        let mut new_content = String::new();
        let mut skipping = false;
        for line in content.lines() {
            if line.contains("<!-- CMUX-START -->") {
                skipping = true;
                continue;
            }
            if line.contains("<!-- CMUX-END -->") {
                skipping = false;
                continue;
            }
            if !skipping {
                new_content.push_str(line);
                new_content.push('\n');
            }
        }

        let new_content = new_content.trim_end().to_string() + "\n";
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

        let mut cleaned = String::new();
        let mut skipping = false;
        for line in content.lines() {
            if line.contains("<!-- CMUX-START -->") {
                skipping = true;
                continue;
            }
            if line.contains("<!-- CMUX-END -->") {
                skipping = false;
                continue;
            }
            if !skipping {
                cleaned.push_str(line);
                cleaned.push('\n');
            }
        }

        let skill_block = format!(
            "\n<!-- CMUX-START -->\n{}\n<!-- CMUX-END -->\n",
            crate::skill_content::ORCHESTRATION_SKILL
        );
        cleaned.push_str(&skill_block);

        if let Err(e) = fs::write(&path, cleaned) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }
        println!("Successfully installed cmux skill into {}", path.display());
        0
    }
}
