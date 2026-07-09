use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // The ghostty submodule at the cmux repo root is the default source.
    // CMUX_GHOSTTY_SRC overrides it for out-of-tree builds.
    let ghostty_dir = match env::var("CMUX_GHOSTTY_SRC") {
        Ok(p) => PathBuf::from(p),
        Err(_) => manifest_dir.join("../../../ghostty"),
    };
    let ghostty_dir = ghostty_dir.canonicalize().unwrap_or_else(|e| {
        panic!(
            "ghostty source not found at {} ({}). Run `git submodule update --init` \
             or set CMUX_GHOSTTY_SRC.",
            ghostty_dir.display(),
            e
        )
    });

    println!("cargo:rerun-if-env-changed=CMUX_GHOSTTY_SRC");
    println!("cargo:rerun-if-env-changed=ZIG");
    println!("cargo:rerun-if-env-changed=CMUX_GHOSTTY_VT_ZIG_CPU");
    println!("cargo:rerun-if-changed={}", ghostty_dir.join("include").display());
    println!("cargo:rerun-if-changed={}", ghostty_dir.join("build.zig").display());
    println!("cargo:rerun-if-changed={}", ghostty_dir.join("src").display());

    // Build libghostty-vt.a with zig. ReleaseFast regardless of the cargo
    // profile: the VT parser is on the PTY hot path and a debug zig build
    // is an order of magnitude slower.
    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    let prefix = out_dir.join("ghostty-vt");
    let target = env::var("TARGET").unwrap();
    let host = env::var("HOST").unwrap();
    let mut command = Command::new(&zig);
    command
        .current_dir(&ghostty_dir)
        .arg("build")
        .arg("-Demit-lib-vt=true")
        .arg("-Demit-xcframework=false")
        .arg("-Doptimize=ReleaseFast");
    if target != host {
        if let Some(zig_target) = zig_target_for_rust_target(&target) {
            command.arg(format!("-Dtarget={zig_target}"));
        }
    }
    // Valgrind's instruction emulation doesn't cover every CPU-native SIMD
    // extension zig's default target detection can select (e.g. some AVX-512
    // variants), which SIGILLs under valgrind. CI's valgrind job sets this to
    // "baseline" to match the same workaround ghostty's own build.zig uses
    // for its valgrind step (see `Config.baselineTarget()`).
    if let Ok(cpu) = env::var("CMUX_GHOSTTY_VT_ZIG_CPU") {
        command.arg(format!("-Dcpu={cpu}"));
    }
    let status = command.arg("--prefix").arg(&prefix).status().unwrap_or_else(|e| {
        panic!("failed to run `{zig} build` in {}: {e}", ghostty_dir.display())
    });
    if !status.success() {
        panic!("zig build of libghostty-vt failed with {status}");
    }

    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    if target.contains("windows") {
        println!("cargo:rustc-link-lib=static=ghostty-vt-static");
    } else {
        println!("cargo:rustc-link-lib=static=ghostty-vt");
    }

    // Generate bindings from the public C header.
    let include_dir = ghostty_dir.join("include");
    let mut builder = bindgen::Builder::default()
        .header(include_dir.join("ghostty/vt.h").to_str().unwrap().to_string())
        .clang_arg(format!("-I{}", include_dir.display()))
        .allowlist_function("ghostty_.*")
        .allowlist_type("Ghostty.*")
        .allowlist_var("GHOSTTY_.*")
        .prepend_enum_name(false)
        .derive_default(true)
        .layout_tests(false);

    // Feed system include paths to clang to help find headers (like limits.h)
    // when libclang doesn't have its own resource directory headers.
    //
    // Try the project's own toolchain first (cc -> clang), then gcc as a
    // last resort. CI uses `apt install clang libclang-dev` (no gcc) so the
    // gcc-only probe would silently no-op there.
    for cc in ["cc", "clang", "gcc"] {
        if let Some(paths) = probe_system_includes(cc) {
            for path in paths {
                builder = builder.clang_arg(format!("-isystem{}", path.display()));
            }
            break;
        }
    }

    // If clang is on PATH, ask it for its resource dir and feed it to
    // bindgen via -resource-dir. This is the surest way to find clang's
    // bundled limits.h/stddef.h on stripped-down clang packages.
    if let Some(resdir) = clang_resource_dir() {
        builder = builder.clang_arg(format!("-resource-dir={}", resdir.display()));
    }

    let bindings = builder
        .generate()
        .expect("bindgen failed for ghostty/vt.h");
    bindings.write_to_file(out_dir.join("bindings.rs")).expect("failed to write bindings.rs");
}

fn zig_target_for_rust_target(target: &str) -> Option<&'static str> {
    match target {
        "x86_64-pc-windows-gnu" => Some("x86_64-windows-gnu"),
        "x86_64-pc-windows-msvc" => Some("x86_64-windows-msvc"),
        "aarch64-pc-windows-msvc" => Some("aarch64-windows-msvc"),
        _ => None,
    }
}

/// Run `<cc> -E -Wp,-v -` and return the include paths listed between
/// `#include <...>` and `End of search list.` on stderr. Returns None if
/// the probe couldn't be run (binary missing or non-zero exit).
fn probe_system_includes(cc: &str) -> Option<Vec<std::path::PathBuf>> {
    let output = Command::new(cc)
        .args(["-E", "-Wp,-v", "-"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut paths = Vec::new();
    let mut in_search_list = false;
    for line in stderr.lines() {
        if line.contains("#include <...>") {
            in_search_list = true;
            continue;
        }
        if line.contains("End of search list.") {
            break;
        }
        if in_search_list {
            let path = line.trim();
            if !path.is_empty() && std::path::Path::new(path).exists() {
                paths.push(std::path::PathBuf::from(path));
            }
        }
    }
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

/// Ask `clang` for its resource directory (the path containing clang's
/// bundled `include/limits.h` etc.). Returns None if clang isn't on PATH
/// or the probe failed. We feed this to bindgen via `-resource-dir` so
/// the build works on systems where the system `limits.h` is missing or
/// libclang's resource-dir detection is broken (thin Arch `clang`,
/// distroless images, some Nix shells).
fn clang_resource_dir() -> Option<std::path::PathBuf> {
    let output = Command::new("clang")
        .arg("-print-resource-dir")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if dir.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(dir))
    }
}
