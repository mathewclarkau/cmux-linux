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
        _ => {
            eprintln!("cmux-mux: usage: cmux-mux pi install-hooks [--uninstall] [--global]");
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
