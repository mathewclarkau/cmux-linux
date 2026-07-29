//! `cmux plugin` verb group: manifest-only plugin registry.
//!
//! Implements `list`, `install`, `uninstall`, `enable`, and `disable`
//! for `cmux-plugin.toml` manifests. This PR does NOT spawn, execute,
//! or sandbox any plugin code; it only manages on-disk manifest state
//! and a small JSON registry file. Plugin *execution* (proxying
//! `cmux <plugin-name> <verb>` calls to a running plugin process,
//! WASM/WASI sandboxing) is deferred to a follow-up PR and is not
//! implemented by anything in this module.
//!
//! On-disk layout (under the cmux data directory, which honours
//! `XDG_DATA_HOME` and falls back to `~/.local/share/cmux`):
//!
//! ```text
//! <base>/
//!   plugins.json          <- registry: { "plugins": [PluginEntry, ...] }
//!   plugins/
//!     <name>/
//!       cmux-plugin.toml  <- the manifest copied verbatim at install time
//! ```
//!
//! Manifest shape (`cmux-plugin.toml`):
//!
//! ```toml
//! [plugin]
//! name = "pifactory-fleet"
//! entry = "bin/fleet.wasm"
//! verbs = ["deploy", "rollback"]
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const USAGE: &str = "\
cmux plugin - manage cmux-plugin.toml manifests (no execution yet)

USAGE:
  cmux plugin list                       List installed plugins (read-only)
  cmux plugin install <manifest-path>    Install a plugin from a manifest
  cmux plugin uninstall <name>           Remove an installed plugin
  cmux plugin enable <name>              Mark a plugin enabled
  cmux plugin disable <name>             Mark a plugin disabled

Shared global flags (accepted before the subcommand):
  --json     Emit machine-readable JSON for `list`.

NOT IMPLEMENTED (deferred to a follow-up PR):
  Plugin *execution* (proxying `cmux <plugin-name> <verb>` to a running
  plugin process, WASM/WASI sandboxing, the permission model) is out of
  scope for this verb group. These verbs only manage manifest state.

The manifest file is `cmux-plugin.toml` with a single `[plugin]` table:
  name   (string, required, non-empty)    plugin id and on-disk dir name
  entry  (string, required, non-empty)    path to the plugin entry artefact
                                          (stored verbatim; not resolved,
                                          not validated as executable here)
  verbs  (array of strings, required,    the verbs this plugin claims
          non-empty, each non-empty)      (stored verbatim; not proxied)
";

/// Entry-level dispatch. Returns a process exit code. Called from
/// `main.rs` when `raw_args.first() == Some("plugin")`.
pub fn run(args: &[String]) -> i32 {
    let mut json = false;
    let mut rest: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if !json && arg == "--json" {
            json = true;
            i += 1;
            continue;
        }
        rest.push(arg);
        i += 1;
    }

    let Some(sub) = rest.first().copied() else {
        print!("{USAGE}");
        return 0;
    };

    let positional = &rest[1..];
    match base_dir() {
        Ok(base) => dispatch(&base, sub, positional, json),
        Err(err) => {
            eprintln!("cmux plugin: {err}");
            1
        }
    }
}

fn dispatch(base: &Path, sub: &str, positional: &[&str], json: bool) -> i32 {
    match sub {
        "list" => cmd_list(base, json),
        "install" => cmd_install(base, positional),
        "uninstall" => cmd_uninstall(base, positional),
        "enable" => cmd_set_enabled(base, positional, true),
        "disable" => cmd_set_enabled(base, positional, false),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            0
        }
        other => {
            eprintln!("cmux plugin: unknown subcommand {other:?}\n\n{USAGE}");
            2
        }
    }
}

// ----- paths ---------------------------------------------------------------

/// Resolve the cmux data base directory (`<base>/plugins` and
/// `<base>/plugins.json` live under this). Honours `XDG_DATA_HOME` and
/// falls back to `~/.local/share/cmux`, mirroring the chrome profile
/// resolution in `mux_core::platform`. Pure: no IO.
fn base_dir() -> Result<PathBuf, String> {
    if let Some(raw) = std::env::var_os("XDG_DATA_HOME") {
        if !raw.is_empty() {
            return Ok(PathBuf::from(raw).join("cmux"));
        }
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".local").join("share").join("cmux"))
        .ok_or_else(|| "could not resolve cmux data dir (set XDG_DATA_HOME or HOME)".to_string())
}

fn plugins_dir(base: &Path) -> PathBuf {
    base.join("plugins")
}

fn registry_path(base: &Path) -> PathBuf {
    base.join("plugins.json")
}

// ----- manifest ------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ManifestFile {
    plugin: ManifestPlugin,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestPlugin {
    name: String,
    entry: String,
    verbs: Vec<String>,
}

/// Parse a manifest string into a validated `ManifestPlugin`. Returns a
/// clear, single-line error for malformed TOML, missing required
/// fields, or semantically empty required fields. Pure: no IO.
fn parse_manifest(content: &str) -> Result<ManifestPlugin, String> {
    let file: ManifestFile =
        toml::from_str(content).map_err(|err| format!("malformed cmux-plugin.toml: {err}"))?;
    let ManifestPlugin { name, entry, verbs } = &file.plugin;
    if name.trim().is_empty() {
        return Err("manifest missing required field [plugin] name".to_string());
    }
    if entry.trim().is_empty() {
        return Err("manifest missing required field [plugin] entry".to_string());
    }
    if verbs.is_empty() {
        return Err("manifest missing required field [plugin] verbs".to_string());
    }
    for verb in verbs {
        if verb.trim().is_empty() {
            return Err("manifest [plugin] verbs contains an empty entry".to_string());
        }
    }
    // The name becomes a directory under `plugins/`, so it must be a
    // single path component (no separators, no "." / "..").
    if name.contains(std::path::MAIN_SEPARATOR) || name == "." || name == ".." || name.contains('/')
    {
        return Err(format!("plugin name {name:?} must not be a path"));
    }
    Ok(file.plugin)
}

// ----- registry ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginEntry {
    name: String,
    enabled: bool,
    entry: String,
    verbs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Registry {
    plugins: Vec<PluginEntry>,
}

/// Read the registry file. Missing file is an empty registry, not an
/// error: a fresh install has no `plugins.json` yet.
fn load_registry(base: &Path) -> Result<Registry, String> {
    let path = registry_path(base);
    match fs::read_to_string(&path) {
        Ok(content) => {
            if content.trim().is_empty() {
                return Ok(Registry::default());
            }
            serde_json::from_str::<Registry>(&content)
                .map_err(|err| format!("corrupt registry {}: {err}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(err) => Err(format!("failed to read {}: {err}", path.display())),
    }
}

/// Persist the registry as pretty JSON. Creates the parent directory
/// on demand so a fresh install works without a pre-existing base dir.
fn save_registry(base: &Path, reg: &Registry) -> Result<(), String> {
    let path = registry_path(base);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(reg)
        .map_err(|err| format!("failed to encode registry: {err}"))?;
    fs::write(&path, json).map_err(|err| format!("failed to write {}: {err}", path.display()))
}

fn find_entry_mut<'a>(reg: &'a mut Registry, name: &str) -> Option<&'a mut PluginEntry> {
    reg.plugins.iter_mut().find(|p| p.name == name)
}

// ----- subcommands ---------------------------------------------------------

fn cmd_list(base: &Path, json: bool) -> i32 {
    let reg = match load_registry(base) {
        Ok(reg) => reg,
        Err(err) => {
            eprintln!("cmux plugin list: {err}");
            return 1;
        }
    };
    if reg.plugins.is_empty() {
        if json {
            let _ = println!("{}", serde_json::to_string(&reg).unwrap_or_default());
        } else {
            println!("no plugins installed");
        }
        return 0;
    }
    // Sorted, stable, greppable output (matches the repo output style).
    let mut entries = reg.plugins.clone();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    if json {
        let _ = println!("{}", serde_json::to_string(&reg).unwrap_or_default());
        return 0;
    }
    for e in &entries {
        let state = if e.enabled { "enabled" } else { "disabled" };
        let verbs = e.verbs.join(",");
        println!("{} {} {} {}", e.name, state, e.entry, verbs);
    }
    0
}

fn cmd_install(base: &Path, positional: &[&str]) -> i32 {
    let Some(&manifest_path) = positional.first() else {
        eprintln!("cmux plugin install: missing <manifest-path>\n\n{USAGE}");
        return 2;
    };
    if positional.len() > 1 {
        eprintln!("cmux plugin install: unexpected extra argument {:?}", positional[1]);
        return 2;
    }
    let content = match fs::read_to_string(manifest_path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("cmux plugin install: cannot read {manifest_path:?}: {err}");
            return 1;
        }
    };
    let plugin = match parse_manifest(&content) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("cmux plugin install: {err}");
            return 1;
        }
    };
    let mut reg = match load_registry(base) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("cmux plugin install: {err}");
            return 1;
        }
    };
    if reg.plugins.iter().any(|p| p.name == plugin.name) {
        eprintln!(
            "cmux plugin install: a plugin named {:?} is already installed",
            plugin.name
        );
        return 1;
    }
    let plugin_dir = plugins_dir(base).join(&plugin.name);
    if let Err(err) = fs::create_dir_all(&plugin_dir) {
        eprintln!("cmux plugin install: failed to create {}: {err}", plugin_dir.display());
        return 1;
    }
    let dest = plugin_dir.join("cmux-plugin.toml");
    if let Err(err) = fs::write(&dest, &content) {
        // Roll back the directory we just made so a failed install does
        // not leave an empty half-registered plugin on disk.
        let _ = fs::remove_dir_all(&plugin_dir);
        eprintln!("cmux plugin install: failed to write {}: {err}", dest.display());
        return 1;
    }
    reg.plugins.push(PluginEntry {
        name: plugin.name.clone(),
        enabled: true,
        entry: plugin.entry.clone(),
        verbs: plugin.verbs.clone(),
    });
    if let Err(err) = save_registry(base, &reg) {
        let _ = fs::remove_dir_all(&plugin_dir);
        eprintln!("cmux plugin install: {err}");
        return 1;
    }
    println!("installed plugin {} from {}", plugin.name, Path::new(manifest_path).display());
    0
}

fn cmd_uninstall(base: &Path, positional: &[&str]) -> i32 {
    let Some(&name) = positional.first() else {
        eprintln!("cmux plugin uninstall: missing <name>\n\n{USAGE}");
        return 2;
    };
    if positional.len() > 1 {
        eprintln!("cmux plugin uninstall: unexpected extra argument {:?}", positional[1]);
        return 2;
    }
    let mut reg = match load_registry(base) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("cmux plugin uninstall: {err}");
            return 1;
        }
    };
    let before = reg.plugins.len();
    reg.plugins.retain(|p| p.name != name);
    if reg.plugins.len() == before {
        eprintln!("cmux plugin uninstall: no plugin named {name:?} is installed");
        return 1;
    }
    let plugin_dir = plugins_dir(base).join(name);
    if plugin_dir.exists() {
        if let Err(err) = fs::remove_dir_all(&plugin_dir) {
            eprintln!(
                "cmux plugin uninstall: failed to remove {}: {err}",
                plugin_dir.display()
            );
            return 1;
        }
    }
    if let Err(err) = save_registry(base, &reg) {
        eprintln!("cmux plugin uninstall: {err}");
        return 1;
    }
    println!("uninstalled plugin {name}");
    0
}

fn cmd_set_enabled(base: &Path, positional: &[&str], enabled: bool) -> i32 {
    let label = if enabled { "enable" } else { "disable" };
    let Some(&name) = positional.first() else {
        eprintln!("cmux plugin {label}: missing <name>\n\n{USAGE}");
        return 2;
    };
    if positional.len() > 1 {
        eprintln!("cmux plugin {label}: unexpected extra argument {:?}", positional[1]);
        return 2;
    }
    let mut reg = match load_registry(base) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("cmux plugin {label}: {err}");
            return 1;
        }
    };
    let Some(entry) = find_entry_mut(&mut reg, name) else {
        eprintln!("cmux plugin {label}: no plugin named {name:?} is installed");
        return 1;
    };
    let already = entry.enabled == enabled;
    if !already {
        entry.enabled = enabled;
    }
    if let Err(err) = save_registry(base, &reg) {
        eprintln!("cmux plugin {label}: {err}");
        return 1;
    }
    let state = if enabled { "enabled" } else { "disabled" };
    if already {
        println!("plugin {name} already {state}");
    } else {
        println!("plugin {name} {state}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str, entry: &str, verbs: &[&str]) -> String {
        let verbs = verbs
            .iter()
            .map(|v| format!("\"{v}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[plugin]\nname = \"{name}\"\nentry = \"{entry}\"\nverbs = [{verbs}]\n"
        )
    }

    fn tmp_base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cmux-plugin-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_manifest_success() {
        let m = parse_manifest(&manifest("fleet", "bin/fleet.wasm", &["deploy", "rollback"]))
            .expect("valid manifest parses");
        assert_eq!(m.name, "fleet");
        assert_eq!(m.entry, "bin/fleet.wasm");
        assert_eq!(m.verbs, vec!["deploy".to_string(), "rollback".to_string()]);
    }

    #[test]
    fn parse_manifest_missing_name() {
        let err = parse_manifest("[plugin]\nentry = \"x\"\nverbs = [\"y\"]\n").unwrap_err();
        assert!(err.contains("name"), "error should name the missing field: {err}");
    }

    #[test]
    fn parse_manifest_missing_entry() {
        let err = parse_manifest("[plugin]\nname = \"x\"\nverbs = [\"y\"]\n").unwrap_err();
        assert!(err.contains("entry"), "error should name the missing field: {err}");
    }

    #[test]
    fn parse_manifest_missing_verbs() {
        let err = parse_manifest("[plugin]\nname = \"x\"\nentry = \"y\"\n").unwrap_err();
        assert!(err.contains("verbs"), "error should name the missing field: {err}");
    }

    #[test]
    fn parse_manifest_malformed_toml() {
        let err = parse_manifest("this is not = = valid toml").unwrap_err();
        assert!(err.contains("malformed cmux-plugin.toml"), "error: {err}");
    }

    #[test]
    fn parse_manifest_empty_verbs_list() {
        let err =
            parse_manifest("[plugin]\nname = \"x\"\nentry = \"y\"\nverbs = []\n").unwrap_err();
        assert!(err.contains("verbs"), "error: {err}");
    }

    #[test]
    fn parse_manifest_name_must_not_be_a_path() {
        let err = parse_manifest(&manifest("../evil", "x", &["y"])).unwrap_err();
        assert!(err.contains("path"), "error: {err}");
        let err = parse_manifest(&manifest("a/b", "x", &["y"])).unwrap_err();
        assert!(err.contains("path"), "error: {err}");
    }

    /// Covers AC1..AC5 against an injected temp base dir (no env-var
    /// mutation, so it is safe under `cargo test` parallelism).
    #[test]
    fn install_list_uninstall_enable_disable_round_trip() {
        let base = tmp_base("roundtrip");

        // Empty list prints the empty message, exit 0.
        assert_eq!(cmd_list(&base, false), 0);
        assert_eq!(load_registry(&base).unwrap().plugins.len(), 0);

        // Install a valid manifest from a temp file.
        let manifest_dir = std::env::temp_dir().join(format!(
            "cmux-plugin-manifest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&manifest_dir).unwrap();
        let manifest_file = manifest_dir.join("cmux-plugin.toml");
        fs::write(&manifest_file, manifest("fleet", "bin/fleet.wasm", &["deploy", "rollback"]))
            .unwrap();
        let path_str = manifest_file.to_str().unwrap().to_string();
        assert_eq!(cmd_install(&base, &[path_str.as_str()]), 0);

        // AC1: registered enabled by default, manifest copied under plugins/.
        let reg = load_registry(&base).unwrap();
        assert_eq!(reg.plugins.len(), 1);
        let entry = &reg.plugins[0];
        assert_eq!(entry.name, "fleet");
        assert!(entry.enabled);
        assert_eq!(entry.entry, "bin/fleet.wasm");
        assert_eq!(
            entry.verbs,
            vec!["deploy".to_string(), "rollback".to_string()]
        );
        let dest = plugins_dir(&base).join("fleet").join("cmux-plugin.toml");
        assert!(dest.exists(), "manifest should be copied to {}", dest.display());
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            manifest("fleet", "bin/fleet.wasm", &["deploy", "rollback"])
        );

        // AC5: duplicate name fails, does not partially register.
        assert_eq!(cmd_install(&base, &[path_str.as_str()]), 1);
        assert_eq!(load_registry(&base).unwrap().plugins.len(), 1);

        // AC4: list reflects the installed plugin.
        assert_eq!(cmd_list(&base, false), 0);

        // AC3: disable persists across a fresh registry read.
        assert_eq!(cmd_set_enabled(&base, &["fleet"], false), 0);
        assert!(!load_registry(&base).unwrap().plugins[0].enabled);
        assert_eq!(cmd_set_enabled(&base, &["fleet"], true), 0);
        assert!(load_registry(&base).unwrap().plugins[0].enabled);

        // AC5: enable/disable of an unknown plugin fails clearly.
        assert_eq!(cmd_set_enabled(&base, &["nope"], true), 1);

        // AC4: uninstall of an unknown plugin fails clearly.
        assert_eq!(cmd_uninstall(&base, &["nope"]), 1);

        // AC2: uninstall removes the registry entry and the plugin dir.
        assert_eq!(cmd_uninstall(&base, &["fleet"]), 0);
        assert_eq!(load_registry(&base).unwrap().plugins.len(), 0);
        assert!(!dest.exists(), "plugin dir should be removed after uninstall");

        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&manifest_dir);
    }

    #[test]
    fn install_missing_manifest_arg_is_usage_error() {
        let base = tmp_base("noarg");
        assert_eq!(cmd_install(&base, &[]), 2);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn install_nonexistent_file_is_command_error() {
        let base = tmp_base("nofile");
        assert_eq!(cmd_install(&base, &["/nonexistent/cmux-plugin.toml"]), 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn install_malformed_manifest_is_command_error() {
        let base = tmp_base("badman");
        let dir = std::env::temp_dir().join(format!(
            "cmux-plugin-bad-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cmux-plugin.toml");
        fs::write(&file, "[plugin]\nname = \"x\"\n").unwrap();
        let path = file.to_str().unwrap().to_string();
        assert_eq!(cmd_install(&base, &[path.as_str()]), 1);
        assert_eq!(load_registry(&base).unwrap().plugins.len(), 0);
        let _ = fs::remove_dir_all(&base);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_subcommand_is_usage_error() {
        let base = tmp_base("unknown");
        assert_eq!(dispatch(&base, "frobnicate", &[], false), 2);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dispatch_help_prints_usage() {
        let base = tmp_base("help");
        assert_eq!(dispatch(&base, "--help", &[], false), 0);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn list_empty_prints_no_plugins_installed() {
        let base = tmp_base("empty");
        // Human mode prints the empty message.
        assert_eq!(cmd_list(&base, false), 0);
        // JSON mode emits an empty registry object.
        assert_eq!(cmd_list(&base, true), 0);
        let _ = fs::remove_dir_all(&base);
    }
}