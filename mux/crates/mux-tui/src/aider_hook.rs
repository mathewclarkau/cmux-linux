use std::fs;
use std::path::PathBuf;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn wrapper_path(global: bool) -> Option<PathBuf> {
    if global {
        mux_core::platform::home_dir().map(|h| h.join(".local").join("bin").join("aider"))
    } else {
        Some(PathBuf::from(".bin").join("aider"))
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
            eprintln!("cmux-mux: usage: cmux-mux aider install-hooks [--uninstall] [--global]");
            2
        }
    }
}

fn run_install(uninstall: bool, global: bool) -> i32 {
    let Some(path) = wrapper_path(global) else {
        eprintln!("error: could not resolve home directory for global wrapper");
        return 1;
    };

    if uninstall {
        if path.exists() {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("error removing {}: {e}", path.display());
                return 1;
            }
            println!("Successfully removed aider wrapper from {}", path.display());
        } else {
            println!("No aider wrapper found at {}", path.display());
        }
        0
    } else {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let script = r#"#!/usr/bin/env bash
# cmux aider wrapper

# Find the real aider binary on PATH excluding this script
REAL_AIDER=$(which -a aider 2>/dev/null | grep -v "$0" | head -n 1)

if [ -z "$REAL_AIDER" ]; then
  # Fallback to standard command search if not found specifically
  REAL_AIDER="aider"
fi

if [ -n "${CMUX_MUX_SURFACE:-}" ]; then
  cmux-mux report-agent --surface "$CMUX_MUX_SURFACE" --state working --source aider
fi

# Run the real aider with all arguments
"$REAL_AIDER" "$@"
RESULT=$?

if [ -n "${CMUX_MUX_SURFACE:-}" ]; then
  cmux-mux report-agent --surface "$CMUX_MUX_SURFACE" --state done --source aider
fi

exit $RESULT
"#;

        if let Err(e) = fs::write(&path, script) {
            eprintln!("error writing {}: {e}", path.display());
            return 1;
        }

        #[cfg(unix)]
        {
            if let Ok(metadata) = fs::metadata(&path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(&path, perms);
            }
        }

        println!("Successfully installed aider wrapper at {}", path.display());
        if !global {
            println!("Note: Remember to run your agent using .bin/aider or prepend .bin to your PATH.");
        }
        0
    }
}
