use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use mux_core::platform::transport;
use serde_json::{json, Value};

const REQUEST_ID: u64 = 1;

type BuildFn = fn(&FlagMap) -> Result<Value, UsageError>;
type PrintFn = fn(&Value, &mut dyn Write) -> io::Result<()>;

#[derive(Debug)]
pub struct UsageError(String);

struct CliArgs {
    global: GlobalArgs,
    verb: &'static VerbSpec,
    flags: FlagMap,
}

#[derive(Default)]
pub(crate) struct GlobalArgs {
    pub(crate) session: Option<String>,
    pub(crate) socket: Option<PathBuf>,
    pub(crate) json: bool,
}

#[derive(Default)]
struct FlagMap {
    values: BTreeMap<String, String>,
    /// The verbatim argv captured by `--exec -- <argv...>` (issue #76).
    /// Kept out of `values`: it is a list, not a `--flag value` pair, and
    /// it consumes the rest of the command line.
    exec: Option<Vec<String>>,
}

struct VerbSpec {
    name: &'static str,
    allowed: &'static [&'static str],
    build: BuildFn,
    print: PrintFn,
    stream: bool,
}

const VERBS: &[VerbSpec] = &[
    VerbSpec {
        name: "identify",
        allowed: &[],
        build: build_no_args,
        print: print_identify,
        stream: false,
    },
    VerbSpec {
        name: "list-workspaces",
        allowed: &[],
        build: build_no_args,
        print: print_tree,
        stream: false,
    },
    VerbSpec {
        // Issue #40: returns the server's resolved presentation chrome
        // (theme/tabs/sidebar/keys) for a thin-client attach to layer its
        // local `Overlay` on top of. Read-only; `cmux attach
        // --apply-local-config` invokes the same verb internally, and
        // `cmux attach --print-resolved-config` shows the merged
        // (server + local overlay) chrome for inspection.
        name: "get-resolved-config",
        allowed: &[],
        build: build_no_args,
        print: print_get_resolved_config,
        stream: false,
    },
    VerbSpec {
        name: "send",
        allowed: &["surface", "text", "bytes", "send-cr", "shell"],
        build: build_send,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "read-screen",
        allowed: &["surface"],
        build: build_surface,
        print: print_read_screen,
        stream: false,
    },
    VerbSpec {
        name: "vt-state",
        allowed: &["surface"],
        build: build_surface,
        print: print_vt_state,
        stream: false,
    },
    VerbSpec {
        // "exec"/"env" (issue #76) carry an explicit child argv/env —
        // `cmux new-tab --exec -- <argv...>` is the agent-start primitive
        // that `layout export` records and `layout apply` replays.
        name: "new-tab",
        allowed: &["pane", "cwd", "cols", "rows", "branch", "label", "prompt-file", "exec", "env"],
        build: build_new_tab,
        print: print_surface,
        stream: false,
    },
    VerbSpec {
        name: "new-browser-tab",
        allowed: &["url", "pane", "cols", "rows"],
        build: build_new_browser_tab,
        print: print_surface,
        stream: false,
    },
    VerbSpec {
        name: "new-workspace",
        allowed: &["name", "cols", "rows"],
        build: build_new_workspace,
        print: print_surface,
        stream: false,
    },
    VerbSpec {
        name: "new-screen",
        allowed: &["workspace", "cols", "rows"],
        build: build_new_screen,
        print: print_surface,
        stream: false,
    },
    VerbSpec {
        name: "split",
        allowed: &["pane", "dir", "cols", "rows", "branch", "label", "exec", "env"],
        build: build_split,
        print: print_surface,
        stream: false,
    },
    VerbSpec {
        name: "set-ratio",
        allowed: &["pane", "dir", "ratio"],
        build: build_set_ratio,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "set-default-colors",
        allowed: &["fg", "bg"],
        build: build_set_default_colors,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "close-surface",
        allowed: &["surface"],
        build: build_surface,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "close-pane",
        allowed: &["pane"],
        build: build_pane,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "close-screen",
        allowed: &["screen"],
        build: build_screen,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "close-workspace",
        allowed: &["workspace"],
        build: build_workspace,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "rename-pane",
        allowed: &["pane", "name"],
        build: build_rename_pane,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "rename-surface",
        allowed: &["surface", "name"],
        build: build_rename_surface,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "rename-screen",
        allowed: &["screen", "name"],
        build: build_rename_screen,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "rename-workspace",
        allowed: &["workspace", "name"],
        build: build_rename_workspace,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "set-workspace-color",
        allowed: &["workspace", "color", "colour"],
        build: build_set_workspace_color,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "set-status",
        allowed: &["icon", "workspace"],
        build: build_set_status,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "workspace-color",
        allowed: &["name", "color"],
        build: build_workspace_color,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "trigger-flash",
        allowed: &["workspace", "surface"],
        build: build_trigger_flash,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "resize-surface",
        allowed: &["surface", "cols", "rows"],
        build: build_resize_surface,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "focus-pane",
        allowed: &["pane"],
        build: build_pane,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "select-tab",
        allowed: &["pane", "index", "delta"],
        build: build_select_tab,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "select-screen",
        allowed: &["index", "delta"],
        build: build_select_screen,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "select-workspace",
        allowed: &["index", "delta"],
        build: build_select_workspace,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "move-tab",
        allowed: &["surface", "pane", "index"],
        build: build_move_tab,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "move-workspace",
        allowed: &["workspace", "index"],
        build: build_move_workspace,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "scroll-surface",
        allowed: &["surface", "delta"],
        build: build_scroll_surface,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "subscribe",
        allowed: &[],
        build: build_no_args,
        print: print_empty,
        stream: true,
    },
    VerbSpec {
        name: "attach-surface",
        allowed: &["surface"],
        build: build_surface,
        print: print_empty,
        stream: true,
    },
    VerbSpec {
        name: "report-agent",
        // "session" collides with the global --session (mux session name)
        // flag, so the agent's own session id is --agent-session on the
        // CLI even though the wire protocol field is plain "session".
        // Issue #75: --agent names the pane for the name-addressed verbs,
        // --message carries free-text context, --surface may be omitted
        // inside a pane ($CMUX_MUX_SURFACE) and --source defaults to
        // socket (hooks stay the authority).
        allowed: &["surface", "state", "source", "agent-session", "agent", "message"],
        build: build_report_agent,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "list-agents",
        allowed: &["surface", "state"],
        build: build_list_agents,
        print: print_agents,
        stream: false,
    },
    // Per-pane git worktrees (issue #77). The issue documents these as
    // the three-word form `pane worktree create`; the CLI accepts that
    // verbatim via `rewrite_pane_worktree_alias` (main.rs rewrites the
    // argv triple to the flat form before verb dispatch).
    VerbSpec {
        name: "pane-worktree-create",
        allowed: &["pane", "branch", "label"],
        build: build_pane_worktree_create,
        print: print_worktree_created,
        stream: false,
    },
    VerbSpec {
        name: "pane-worktree-list",
        allowed: &["pane"],
        build: build_pane_worktree_list,
        print: print_worktrees,
        stream: false,
    },
    VerbSpec {
        name: "pane-worktree-remove",
        allowed: &["pane", "branch"],
        build: build_pane_worktree_remove,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        // Issue #78 AC1: ambient detection on one surface (the repo's
        // flat-verb spelling of the issue's `pane detect-agent --pane`;
        // a pane's content lives on its active tab surface, which is
        // what every sibling verb — read-screen, report-agent — targets).
        name: "detect-agent",
        allowed: &["surface"],
        build: build_surface,
        print: print_detect_agent,
        stream: false,
    },
    VerbSpec {
        // Issue #78 AC2: the issue's `agent detect-batch`, spelled to
        // mirror the plural `list-agents` convention.
        name: "detect-agents",
        allowed: &[],
        build: build_no_args,
        print: print_detect_agents,
        stream: false,
    },
    VerbSpec {
        name: "agent-pattern-add",
        allowed: &["name", "pattern", "kind", "confidence", "case-insensitive"],
        build: build_agent_pattern_add,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "agent-pattern-list",
        allowed: &[],
        build: build_no_args,
        print: print_agent_patterns,
        stream: false,
    },
    VerbSpec {
        name: "agent-pattern-remove",
        allowed: &["name"],
        build: build_agent_pattern_remove,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        // Issue #75 AC3: read an agent's pane by name (or surface id).
        name: "agent-read",
        allowed: &["target", "source", "lines"],
        build: build_agent_read,
        print: print_read_screen,
        stream: false,
    },
    VerbSpec {
        name: "browser-reload",
        allowed: &["surface"],
        build: build_surface,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "list-sessions",
        allowed: &[],
        build: build_no_args,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "kill-session",
        allowed: &["session"],
        build: build_kill_session,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "kill-stale",
        allowed: &[],
        build: build_no_args,
        print: print_empty,
        stream: false,
    },
    // `rename-session` is special-cased in `run_command` (it does its own
    // socket discovery + connect + exit-code map, like list/kill-session).
    // The VerbSpec exists so `verb_by_name` recognises it during arg
    // parsing; `build_rename_session` only carries the flags.
    VerbSpec {
        name: "rename-session",
        allowed: &["old", "new"],
        build: build_rename_session,
        print: print_empty,
        stream: false,
    },
    // Issue #76: the layout export/apply verbs are special-cased in
    // `run_command` (they do local file I/O around the socket round-trip,
    // like list/kill/rename-session). The VerbSpecs exist so `parse`
    // recognises the verbs and their flags.
    VerbSpec {
        name: "layout-export",
        allowed: &["workspace", "output"],
        build: build_layout_export,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "layout-apply",
        allowed: &["input", "workspace"],
        build: build_layout_apply,
        print: print_empty,
        stream: false,
    },
    VerbSpec {
        name: "layout-export-all",
        allowed: &["output-dir"],
        build: build_layout_export_all,
        print: print_empty,
        stream: false,
    },
];

pub fn is_cli_invocation(args: &[String]) -> bool {
    matches!(first_command_arg(args), FirstCommand::Help | FirstCommand::Verb)
}

pub fn run(args: &[String], usage: &str) -> i32 {
    match parse(args) {
        Ok(Parsed::Help) => {
            print!("{usage}");
            0
        }
        Ok(Parsed::Command(args)) => run_command(args),
        Err(err) => {
            eprintln!("cmux: {}", err.0);
            2
        }
    }
}

enum FirstCommand {
    None,
    Help,
    Verb,
}

enum Parsed {
    Help,
    Command(CliArgs),
}

fn first_command_arg(args: &[String]) -> FirstCommand {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" | "--session" => i += 2,
            "--json" => i += 1,
            "-h" | "--help" => return FirstCommand::Help,
            arg if arg.starts_with("--") => return FirstCommand::None,
            "help" => return FirstCommand::Help,
            arg if verb_by_name(arg).is_some() => return FirstCommand::Verb,
            _ => return FirstCommand::None,
        }
    }
    FirstCommand::None
}

fn parse(args: &[String]) -> Result<Parsed, UsageError> {
    if matches!(first_command_arg(args), FirstCommand::Help) {
        return Ok(Parsed::Help);
    }

    let mut global = GlobalArgs::default();
    let mut flags = FlagMap::default();
    let mut verb: Option<&'static VerbSpec> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" | "help" => return Ok(Parsed::Help),
            "--json" => {
                global.json = true;
                i += 1;
            }
            "--socket" => {
                global.socket = Some(PathBuf::from(value_after(args, i, "--socket")?));
                i += 2;
            }
            "--session" => {
                global.session = Some(value_after(args, i, "--session")?);
                i += 2;
            }
            _ if verb.is_none() && verb_by_name(arg).is_some() => {
                verb = verb_by_name(arg);
                i += 1;
            }
            // Issue #76: `--exec -- <argv...>` — everything after the
            // literal `--` is the child's verbatim argv (no quoting
            // loss). Must be the verb's LAST flag: it eats the rest of
            // the command line.
            _ if arg == "--exec" && verb.is_some() => {
                let spec = verb.unwrap();
                if !spec.allowed.contains(&"exec") {
                    return Err(UsageError(format!("unknown flag --exec for {}", spec.name)));
                }
                if flags.exec.is_some() {
                    return Err(UsageError("duplicate --exec".to_string()));
                }
                if args.get(i + 1).map(|s| s.as_str()) != Some("--") {
                    return Err(UsageError(
                        "--exec must be followed by \"--\" and the command argv".to_string(),
                    ));
                }
                let argv: Vec<String> = args[i + 2..].to_vec();
                if argv.is_empty() {
                    return Err(UsageError("--exec needs a command after \"--\"".to_string()));
                }
                flags.exec = Some(argv);
                i = args.len();
            }
            _ if arg.starts_with("--") => {
                let Some(spec) = verb else {
                    return Err(UsageError(format!("unknown global flag {arg:?}")));
                };
                let name = arg.trim_start_matches("--");
                if !spec.allowed.contains(&name) {
                    return Err(UsageError(format!("unknown flag {arg:?} for {}", spec.name)));
                }
                let value = value_after(args, i, arg)?;
                if flags.values.insert(name.to_string(), value).is_some() {
                    return Err(UsageError(format!("duplicate flag {arg:?}")));
                }
                i += 2;
            }
            _ if verb.is_some() => {
                return Err(UsageError(format!("unexpected argument {arg:?}")));
            }
            _ => return Err(UsageError(format!("unknown argument {arg:?}"))),
        }
    }

    let Some(verb) = verb else { return Err(UsageError("missing verb".to_string())) };
    Ok(Parsed::Command(CliArgs { global, verb, flags }))
}

fn value_after(args: &[String], index: usize, flag: &str) -> Result<String, UsageError> {
    args.get(index + 1).cloned().ok_or_else(|| UsageError(format!("{flag} needs a value")))
}

fn verb_by_name(name: &str) -> Option<&'static VerbSpec> {
    VERBS.iter().find(|verb| verb.name == name)
}

fn run_command(args: CliArgs) -> i32 {
    match args.verb.name {
        "list-sessions" => return run_list_sessions(&args.global, &args.flags),
        "kill-session" => return run_kill_session(&args.global, &args.flags),
        "kill-stale" => return run_kill_stale(&args.global, &args.flags),
        "rename-session" => return run_rename_session(&args.global, &args.flags),
        "layout-export" => return run_layout_export(&args.global, &args.flags),
        "layout-apply" => return run_layout_apply(&args.global, &args.flags),
        "layout-export-all" => return run_layout_export_all(&args.global, &args.flags),
        _ => {}
    }
    let request = match (args.verb.build)(&args.flags) {
        Ok(mut value) => {
            value["cmd"] = json!(args.verb.name);
            value["id"] = json!(REQUEST_ID);
            value
        }
        Err(err) => {
            eprintln!("cmux: {}", err.0);
            return 2;
        }
    };
    let socket_path = resolve_socket(&args.global);
    let mut stream = match transport::connect(&socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("cannot connect to session socket {}: {err}", socket_path.display());
            return 3;
        }
    };
    if args.verb.stream {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    } else {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    }
    let mut line = match serde_json::to_vec(&request) {
        Ok(line) => line,
        Err(err) => {
            eprintln!("failed to encode request: {err}");
            return 2;
        }
    };
    line.push(b'\n');
    if let Err(err) = stream.write_all(&line) {
        eprintln!("transport error: {err}");
        return 3;
    }

    let mut reader = BufReader::new(stream);
    if args.verb.stream {
        run_stream(reader)
    } else {
        run_one_response(&mut reader, args.global.json, args.verb.print)
    }
}

fn resolve_socket(global: &GlobalArgs) -> PathBuf {
    if let Some(path) = &global.socket {
        return path.clone();
    }
    if let Some(path) = std::env::var_os("CMUX_MUX_SOCKET") {
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let session = global.session.as_deref().unwrap_or("main");
    mux_core::server::default_socket_path(session)
}

fn run_one_response(
    reader: &mut BufReader<Box<dyn transport::Stream>>,
    json_output: bool,
    print_human: PrintFn,
) -> i32 {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("transport closed before response");
                return 3;
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("transport error: {err}");
                return 3;
            }
        }
        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("bad response: {err}");
                return 3;
            }
        };
        if value.get("event").is_some() {
            continue;
        }
        return print_response(&value, json_output, print_human);
    }
}

fn run_stream(mut reader: BufReader<Box<dyn transport::Stream>>) -> i32 {
    let mut line = String::new();
    loop {
        if crate::shutdown_requested() {
            return 0;
        }
        match reader.read_line(&mut line) {
            Ok(0) => {
                if line.is_empty() {
                    return 0;
                }
                eprintln!("transport closed with partial stream line");
                return 3;
            }
            Ok(_) if !line.ends_with('\n') => {
                eprintln!("transport closed with partial stream line");
                return 3;
            }
            Ok(_) => {}
            Err(err)
                if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
            {
                continue;
            }
            Err(err) => {
                eprintln!("transport error: {err}");
                return 3;
            }
        }
        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("bad stream line: {err}");
                return 3;
            }
        };
        if value.get("event").is_some() {
            print!("{}", line.trim_end_matches(['\r', '\n']));
            println!();
            line.clear();
            if io::stdout().flush().is_err() {
                return 3;
            }
            continue;
        }
        if value.get("id").and_then(Value::as_u64) != Some(REQUEST_ID) {
            line.clear();
            continue;
        }
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            line.clear();
            continue;
        }
        let error = value.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        eprintln!("{error}");
        return 1;
    }
}

fn print_response(value: &Value, json_output: bool, print_human: PrintFn) -> i32 {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = value.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        eprintln!("{error}");
        return 1;
    }
    let data = value.get("data").unwrap_or(&Value::Null);
    let mut stdout = io::stdout();
    let result = if json_output {
        serde_json::to_writer(&mut stdout, data)
            .and_then(|_| stdout.write_all(b"\n").map_err(serde_json::Error::io))
            .map_err(io::Error::other)
    } else {
        print_human(data, &mut stdout)
    };
    match result.and_then(|_| stdout.flush()) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("stdout error: {err}");
            3
        }
    }
}

fn build_no_args(flags: &FlagMap) -> Result<Value, UsageError> {
    flags.reject_remaining()?;
    Ok(json!({}))
}

fn build_surface(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "surface": flags.required_u64("surface")? }))
}

fn build_pane(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "pane": flags.required_u64("pane")? }))
}

fn build_screen(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "screen": flags.required_u64("screen")? }))
}

fn build_workspace(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "workspace": flags.required_u64("workspace")? }))
}

fn build_send(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({ "surface": flags.required_u64("surface")? });
    if let Some(text) = flags.optional("text") {
        value["text"] = json!(text);
    }
    if let Some(bytes) = flags.optional("bytes") {
        value["bytes"] = json!(bytes);
    }
    // `--send-cr` (boolean flag) appends a literal CR (0x0D) to the written bytes
    // so that fish (and other line-edited REPLs) submit their input buffer.
    // Default false. See `Command::Send::send_cr` in mux-core/src/server.rs.
    if let Some(send_cr) = flags.optional_bool("send-cr") {
        value["send_cr"] = json!(send_cr);
    }
    // `--shell` (issue #35): shell-aware input sanitisation. `raw` (the
    // default, matching pre-#35 passthrough) writes bytes verbatim; a
    // known shell prefixes a `\n` when the text could be mis-parsed;
    // `auto` resolves the pane's shell from /proc on Linux. See
    // `Command::Send::shell` in mux-core/src/server.rs.
    if let Some(shell) = flags.optional("shell") {
        if !matches!(shell.as_str(), "auto" | "fish" | "bash" | "zsh" | "sh" | "nu" | "raw") {
            return Err(UsageError(format!(
                "--shell must be one of auto, fish, bash, zsh, sh, nu, raw (got {shell:?})"
            )));
        }
        value["shell"] = json!(shell);
    }
    if value.get("text").is_none() && value.get("bytes").is_none() {
        let mut text = String::new();
        io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| UsageError(format!("failed to read stdin: {err}")))?;
        value["text"] = json!(text);
    }
    Ok(value)
}

fn build_new_tab(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_u64(&mut value, "pane")?;
    flags.insert_optional_string(&mut value, "cwd");
    flags.insert_optional_size(&mut value)?;
    // Issue #77 AC4: `--branch` creates a worktree and spawns the tab
    // inside it; `--prompt-file` reads the same keys from a leading
    // frontmatter block. The two are mutually exclusive so there is
    // never a precedence question.
    if let Some(path) = flags.optional("prompt-file") {
        if flags.optional("branch").is_some() || flags.optional("label").is_some() {
            return Err(UsageError(
                "--prompt-file frontmatter cannot be combined with --branch/--label".into(),
            ));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|err| UsageError(format!("failed to read --prompt-file {path:?}: {err}")))?;
        let (branch, label) = parse_prompt_frontmatter(&text)?;
        if let Some(branch) = branch {
            value["branch"] = json!(branch);
        }
        if let Some(label) = label {
            value["label"] = json!(label);
        }
    } else {
        flags.insert_optional_string(&mut value, "branch");
        flags.insert_optional_string(&mut value, "label");
    }
    // Issue #76: `--exec -- <argv...>` / `--env K=V,K2=V2` layer on top
    // of any worktree/frontmatter choices.
    insert_exec_env(flags, &mut value)?;
    Ok(value)
}

/// Issue #76: `--exec -- <argv...>` (verbatim argv passthrough) and
/// `--env K=V,K2=V2` (comma-separated pairs) → the socket command's
/// `command` / `env` fields.
fn insert_exec_env(flags: &FlagMap, value: &mut Value) -> Result<(), UsageError> {
    if let Some(argv) = &flags.exec {
        value["command"] = json!(argv);
    }
    if let Some(env) = flags.optional("env") {
        let mut map = serde_json::Map::new();
        for pair in env.split(',') {
            let Some((key, val)) = pair.split_once('=') else {
                return Err(UsageError(format!("--env entries must be K=V (got {pair:?})")));
            };
            if key.is_empty() {
                return Err(UsageError("--env keys cannot be empty".to_string()));
            }
            map.insert(key.to_string(), json!(val));
        }
        value["env"] = Value::Object(map);
    }
    Ok(())
}

/// Parse a leading `---` frontmatter block from an agent prompt file
/// (issue #77 AC4). A file that does not start with `---` has no
/// frontmatter and yields `(None, None)`. Strict, per the repo rule
/// that parse errors propagate instead of silently defaulting: the
/// block must close with a `---` line, only `branch`/`label` keys are
/// allowed, keys may not repeat, and values must be non-empty.
fn parse_prompt_frontmatter(
    text: &str,
) -> Result<(Option<String>, Option<String>), UsageError> {
    let mut lines = text.lines();
    if lines.next().map(|first| first.trim_end_matches('\r')) != Some("---") {
        return Ok((None, None));
    }
    let mut branch: Option<String> = None;
    let mut label: Option<String> = None;
    for line in lines {
        let line = line.trim_end_matches('\r');
        if line == "---" {
            return Ok((branch, label));
        }
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(UsageError(format!(
                "malformed frontmatter line {line:?}: expected `key: value`"
            )));
        };
        let value = value.trim();
        if value.is_empty() {
            return Err(UsageError(format!("frontmatter key {key:?} needs a non-empty value")));
        }
        let slot = match key.trim() {
            "branch" => &mut branch,
            "label" => &mut label,
            other => {
                return Err(UsageError(format!(
                    "unknown frontmatter key {other:?} (want branch or label)"
                )));
            }
        };
        if slot.is_some() {
            return Err(UsageError(format!("duplicate frontmatter key {key:?}")));
        }
        *slot = Some(value.to_string());
    }
    Err(UsageError("unterminated frontmatter block: missing closing `---`".into()))
}

fn build_new_browser_tab(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({ "url": flags.required("url")? });
    flags.insert_optional_u64(&mut value, "pane")?;
    flags.insert_optional_size(&mut value)?;
    Ok(value)
}

fn build_new_workspace(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "name");
    flags.insert_optional_size(&mut value)?;
    Ok(value)
}

fn build_new_screen(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_u64(&mut value, "workspace")?;
    flags.insert_optional_size(&mut value)?;
    Ok(value)
}

fn build_split(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({ "pane": flags.required_u64("pane")?, "dir": flags.required_dir()? });
    flags.insert_optional_size(&mut value)?;
    // Issue #77 AC4: branch/label record a worktree on the new pane.
    flags.insert_optional_string(&mut value, "branch");
    flags.insert_optional_string(&mut value, "label");
    // Issue #76: --exec / --env layer on top.
    insert_exec_env(flags, &mut value)?;
    Ok(value)
}

fn build_set_ratio(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({
        "pane": flags.required_u64("pane")?,
        "dir": flags.required_dir()?,
        "ratio": flags.required_f32("ratio")?,
    }))
}

fn build_set_default_colors(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "fg");
    flags.insert_optional_string(&mut value, "bg");
    Ok(value)
}

fn build_rename_pane(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "pane": flags.required_u64("pane")?, "name": flags.required("name")? }))
}

fn build_rename_surface(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "surface": flags.required_u64("surface")?, "name": flags.required("name")? }))
}

fn build_rename_screen(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "screen": flags.required_u64("screen")?, "name": flags.required("name")? }))
}

fn build_rename_workspace(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "workspace": flags.required_u64("workspace")?, "name": flags.required("name")? }))
}

/// `pane-worktree-create` (issue #77): `--branch` names the branch to
/// create via `git worktree add -b`; `--label` is a display badge.
fn build_pane_worktree_create(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value =
        json!({ "pane": flags.required_u64("pane")?, "branch": flags.required("branch")? });
    flags.insert_optional_string(&mut value, "label");
    Ok(value)
}

fn build_pane_worktree_list(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "pane": flags.required_u64("pane")? }))
}

fn build_pane_worktree_remove(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({
        "pane": flags.required_u64("pane")?,
        "branch": flags.required("branch")?,
    }))
}

/// Rewrite the issue-#77 three-word verb form (`cmux pane worktree
/// create ...`) into the canonical flat verb (`pane-worktree-create`)
/// at the first-command position, so the issue's documented invocation
/// works verbatim while the wire protocol keeps the flat kebab-case
/// shape every other verb uses (scout plan §2.8). Called from `main`
/// BEFORE `is_cli_invocation`, which otherwise would not recognise the
/// triple as a CLI invocation at all.
pub(crate) fn rewrite_pane_worktree_alias(args: &mut Vec<String>) {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" | "--session" => i += 2,
            "--json" => i += 1,
            "pane"
                if args.get(i + 1).map(String::as_str) == Some("worktree")
                    && matches!(
                        args.get(i + 2).map(String::as_str),
                        Some("create" | "list" | "remove")
                    ) =>
            {
                let flat = format!("pane-worktree-{}", args[i + 2]);
                args.splice(i..i + 3, [flat]);
                return;
            }
            _ => return,
        }
    }
}

/// A colour value is required so an omitted flag never silently clears
/// the workspace colour. `--color` is primary; `--colour` remains an alias.
fn build_set_workspace_color(flags: &FlagMap) -> Result<Value, UsageError> {
    let workspace = flags.required_u64("workspace")?;
    let color = match (flags.optional("color"), flags.optional("colour")) {
        (Some(_), Some(_)) => return Err(UsageError("use only one of --color or --colour".into())),
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => return Err(UsageError("missing --color".into())),
    };
    let colour = if color.is_empty() { Value::Null } else { json!(color) };
    Ok(json!({ "workspace": workspace, "colour": colour }))
}

fn build_set_status(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({ "icon": flags.required("icon")? });
    flags.insert_optional_u64(&mut value, "workspace")?;
    Ok(value)
}

fn build_workspace_color(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "name": flags.required("name")?, "color": flags.required("color")? }))
}

fn build_trigger_flash(flags: &FlagMap) -> Result<Value, UsageError> {
    let workspace = flags.required_u64("workspace")?;
    let mut value = json!({ "workspace": workspace });
    flags.insert_optional_u64(&mut value, "surface")?;
    Ok(value)
}

fn build_resize_surface(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({
        "surface": flags.required_u64("surface")?,
        "cols": flags.required_u16("cols")?,
        "rows": flags.required_u16("rows")?,
    }))
}

fn build_select_tab(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = selector_request(flags)?;
    flags.insert_optional_u64(&mut value, "pane")?;
    Ok(value)
}

fn build_select_screen(flags: &FlagMap) -> Result<Value, UsageError> {
    selector_request(flags)
}

fn build_select_workspace(flags: &FlagMap) -> Result<Value, UsageError> {
    selector_request(flags)
}

fn build_move_tab(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({
        "surface": flags.required_u64("surface")?,
        "pane": flags.required_u64("pane")?,
        "index": flags.required_usize("index")?,
    }))
}

fn build_move_workspace(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({
        "workspace": flags.required_u64("workspace")?,
        "index": flags.required_usize("index")?,
    }))
}

fn build_scroll_surface(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({
        "surface": flags.required_u64("surface")?,
        "delta": flags.required_isize("delta")?,
    }))
}

fn build_report_agent(flags: &FlagMap) -> Result<Value, UsageError> {
    // Issue #75 AC1: --surface defaults to $CMUX_MUX_SURFACE so a pane's
    // own child (hook or agent) can self-report without knowing its id.
    let surface = match flags.optional("surface") {
        Some(raw) => parse_u64("surface", &raw)?,
        None => match std::env::var("CMUX_MUX_SURFACE") {
            Ok(value) => parse_u64("CMUX_MUX_SURFACE", &value)?,
            Err(_) => {
                return Err(UsageError(
                    "--surface is required (or run inside a cmux pane via $CMUX_MUX_SURFACE)"
                        .into(),
                ))
            }
        },
    };
    // --source defaults to "socket": an in-pane self-report keeps the
    // existing authority model where hook reports still override it.
    let mut value = json!({
        "surface": surface,
        "state": flags.required("state")?,
        "source": flags.optional("source").unwrap_or_else(|| "socket".into()),
    });
    if let Some(session) = flags.optional("agent-session") {
        value["session"] = json!(session);
    }
    if let Some(agent) = flags.optional("agent") {
        value["agent"] = json!(agent);
    }
    if let Some(message) = flags.optional("message") {
        value["message"] = json!(message);
    }
    Ok(value)
}

fn build_list_agents(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_u64(&mut value, "surface")?;
    flags.insert_optional_string(&mut value, "state");
    Ok(value)
}

/// Issue #78 AC4: `agent-pattern-add`. Patterns are substring/glob (`*`
/// wildcard), not regex — validated server-side against the same values.
fn build_agent_pattern_add(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({
        "name": flags.required("name")?,
        "pattern": flags.required("pattern")?,
    });
    flags.insert_optional_string(&mut value, "kind");
    flags.insert_optional_string(&mut value, "confidence");
    if let Some(ci) = flags.optional_bool("case-insensitive") {
        value["case_insensitive"] = json!(ci);
    }
    Ok(value)
}

fn build_agent_read(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({ "target": flags.required("target")? });
    if let Some(source) = flags.optional("source") {
        if !matches!(source.as_str(), "visible" | "recent" | "recent-unwrapped") {
            return Err(UsageError(format!(
                "--source must be one of visible, recent, recent-unwrapped (got {source:?})"
            )));
        }
        value["source"] = json!(source);
    }
    if let Some(lines) = flags.optional("lines") {
        value["lines"] = json!(parse_usize("lines", &lines)?);
    }
    Ok(value)
}

fn build_agent_pattern_remove(flags: &FlagMap) -> Result<Value, UsageError> {
    Ok(json!({ "name": flags.required("name")? }))
}

fn build_kill_session(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "session");
    Ok(value)
}

/// Layout-verb parsers carry their flags; the runners below do the
/// required-flag checks, the file I/O, and their own exit-code maps
/// (issue #76).
fn build_layout_export(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "workspace");
    flags.insert_optional_string(&mut value, "output");
    Ok(value)
}

fn build_layout_apply(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "input");
    flags.insert_optional_string(&mut value, "workspace");
    Ok(value)
}

fn build_layout_export_all(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "output-dir");
    Ok(value)
}

/// `rename-session` parser: carry the `--old`/`--new` flags. The real
/// connect/send (and CLI-side name validation + exit-code map) live in
/// `run_rename_session`, which is special-cased in `run_command` like
/// the other name-keyed verbs (list/kill-session/kill-stale).
fn build_rename_session(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "old");
    flags.insert_optional_string(&mut value, "new");
    Ok(value)
}

pub(crate) fn get_runtime_dir(global: &GlobalArgs) -> PathBuf {
    global
        .socket
        .as_ref()
        .and_then(|s| s.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(mux_core::platform::runtime_dir)
}

pub(crate) fn read_pid_file(path: &std::path::Path) -> Option<u32> {
    if let Ok(content) = std::fs::read_to_string(path) {
        content.trim().parse::<u32>().ok()
    } else {
        None
    }
}

/// One discovered cmux session (issue #63 L1).
///
/// `socket_path` is the exact path to reconnect to; `mtime` is for the
/// picker's newest-first sort (pid-file mtime preferred — the socket mtime
/// can shift on each connect — falling back to socket mtime, then `None`).
#[derive(Clone, Debug)]
pub(crate) struct DiscoveredSession {
    pub(crate) session: String,
    pub(crate) socket_path: PathBuf,
    pub(crate) pid: Option<u32>,
    pub(crate) live: bool,
    pub(crate) mtime: Option<std::time::SystemTime>,
}

/// Socket-centric discovery of cmux sessions in the runtime dir honoured
/// by `global` (parent of `--socket`, else `platform::runtime_dir()`).
/// One row per `*.sock`: derive the pid via `server::pid_path`, liveness via
/// `server::is_session_socket_live`, and an mtime for uptime sort. Shared by
/// `run_list_sessions`, `run_kill_stale`, `run_attach_session_list_json`,
/// and the interactive picker. Returned unsorted (read_dir order is
/// filesystem-dependent); callers sort as needed.
pub(crate) fn discover_sessions(global: &GlobalArgs) -> Vec<DiscoveredSession> {
    let dir = get_runtime_dir(global);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let pid_p = mux_core::server::pid_path(&path);
        let pid = read_pid_file(&pid_p);
        let live = mux_core::server::is_session_socket_live(&path);
        let mtime = std::fs::metadata(&pid_p)
            .and_then(|m| m.modified())
            .or_else(|_| std::fs::metadata(&path).and_then(|m| m.modified()))
            .ok();
        out.push(DiscoveredSession {
            session: stem.to_string(),
            socket_path: path,
            pid,
            live,
            mtime,
        });
    }
    out
}

fn run_list_sessions(global: &GlobalArgs, _flags: &FlagMap) -> i32 {
    let mut sessions = discover_sessions(global);
    // Preserve the historical alphabetical order: the old impl built the
    // name set from a BTreeSet, and read_dir order is filesystem-dependent.
    sessions.sort_by(|a, b| a.session.cmp(&b.session));

    if global.json {
        let json_list: Vec<Value> = sessions
            .iter()
            .map(|s| {
                json!({
                    "session": s.session,
                    "name": s.session,
                    "pid": s.pid,
                    "status": if s.live { "live" } else { "stale" },
                })
            })
            .collect();
        let payload = json!({ "sessions": json_list });
        if serde_json::to_writer(io::stdout(), &payload).is_ok() {
            println!();
            0
        } else {
            3
        }
    } else {
        for s in &sessions {
            let pid_str = s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string());
            let status = if s.live { "live" } else { "stale" };
            println!("{} {} {}", s.session, pid_str, status);
        }
        0
    }
}

/// `cmux attach --session-list --json` (issue #63 L1): non-interactive
/// discovery dump. Same shape as `run_list_sessions`'s JSON branch PLUS a
/// `socket_path` per entry, so a caller can reconnect to the exact socket
/// — important when discovery is scoped by `--socket <parent>/x.sock` and
/// `runtime_dir()` would resolve elsewhere. Exit 0; 3 on write error.
pub(crate) fn run_attach_session_list_json(global: &GlobalArgs) -> i32 {
    let mut sessions = discover_sessions(global);
    sessions.sort_by(|a, b| a.session.cmp(&b.session));
    let json_list: Vec<Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "session": s.session,
                "name": s.session,
                "pid": s.pid,
                "status": if s.live { "live" } else { "stale" },
                "socket_path": s.socket_path.display().to_string(),
            })
        })
        .collect();
    let payload = json!({ "sessions": json_list });
    if serde_json::to_writer(io::stdout(), &payload).is_ok() {
        println!();
        0
    } else {
        3
    }
}

/// Kill the cmux process owning `socket_path` (SIGTERM, escalate to SIGKILL
/// after 2s, reap up to 1s more) and remove its `.sock`/`.pid`. Shared by
/// `run_kill_session` and the picker's kill-focused (Claim 3). Returns true
/// if the pidfile named a live cmux process that was signalled (regardless
/// of whether it died in time); false if there was no pid / no cmux process.
/// The `.sock`/`.pid` are removed unconditionally, matching the historical
/// `run_kill_session` behaviour.
pub(crate) fn kill_session_at(socket_path: &std::path::Path, pid: Option<u32>) -> bool {
    if let Some(pid) = pid {
        if mux_core::server::is_cmux_process(pid) {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                if !mux_core::server::is_process_alive(pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if mux_core::server::is_process_alive(pid) {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
                let deadline2 = std::time::Instant::now() + Duration::from_secs(1);
                while std::time::Instant::now() < deadline2 {
                    if !mux_core::server::is_process_alive(pid) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    let pid_p = mux_core::server::pid_path(socket_path);
    let _ = std::fs::remove_file(socket_path);
    let _ = std::fs::remove_file(&pid_p);
    pid.is_some()
}

/// Outcome of a `rename-session` RPC. Distinguishes a transport failure
/// (CLI exit 3) from a server-reported error (CLI exit 1) so the verb's
/// exit-code table (scout-plan Q5) can map them separately. Shared by the
/// CLI verb (`run_rename_session`) and the picker helper.
enum RenameOutcome {
    /// Server reported ok:true. Carries the new socket path and the
    /// (unchanged) daemon pid from the response.
    Ok { socket_path: PathBuf, pid: u64 },
    /// Server reported ok:false (rename refused/failed) or a malformed reply.
    ServerErr(String),
    /// Could not (re)establish the socket connection or the transport died.
    ConnectErr(String),
}

/// Connect to the daemon at `socket`, send `{"cmd":"rename-session",
/// "new_name":new_name}`, and read one response (skipping any pushed
/// events). Returns the parsed outcome. Used by both `run_rename_session`
/// (CLI verb) and `rename_session_at` (picker helper) so they share one
/// code path.
/// Outcome of a one-shot control-socket RPC (connect → write one request
/// line → read the first matching response, skipping pushed events).
/// Distinguishes a transport failure (`ConnectErr`) from a server-reported
/// error (`ServerErr`, also used for malformed replies) so callers like the
/// `rename-session` exit-code table can map them separately. Shared by
/// `rename_rpc`, the session-manager overlay's `list-workspaces` fetch, and
/// its `select-workspace` remote-focus one-shot (issue #63 L3).
pub(crate) enum OneShotOutcome {
    /// Server reported `ok:true`. Carries the full parsed response.
    Ok(Value),
    /// Server reported `ok:false`, sent a malformed reply, or the transport
    /// closed before a reply.
    ServerErr(String),
    /// Could not establish the connection or a write/read failed.
    ConnectErr(String),
}

/// Connect to `socket`, serialise `request` as one JSON line tagged with
/// `REQUEST_ID`, write it, and read the first non-event response. Bounded by
/// a 10s read timeout set on the fresh stream. No behaviour change versus
/// the inlined body `rename_rpc` previously had; the rename flow keeps its
/// own `RenameOutcome` so its exit-code table (server vs connect error) is
/// preserved, and now just maps from this generic outcome.
pub(crate) fn one_shot_rpc(socket: &std::path::Path, request: Value) -> OneShotOutcome {
    let mut stream = match transport::connect(socket) {
        Ok(stream) => stream,
        Err(err) => {
            return OneShotOutcome::ConnectErr(format!(
                "cannot connect to session socket {}: {err}",
                socket.display()
            ))
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = match serde_json::to_vec(&request) {
        Ok(line) => line,
        Err(err) => return OneShotOutcome::ServerErr(format!("failed to encode request: {err}")),
    };
    line.push(b'\n');
    if let Err(err) = stream.write_all(&line) {
        return OneShotOutcome::ConnectErr(format!("transport error: {err}"));
    }
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => return OneShotOutcome::ServerErr("transport closed before response".into()),
            Ok(_) => {}
            Err(err) => return OneShotOutcome::ConnectErr(format!("transport error: {err}")),
        }
        let value: Value = match serde_json::from_str(&buf) {
            Ok(value) => value,
            Err(err) => return OneShotOutcome::ServerErr(format!("bad response: {err}")),
        };
        if value.get("event").is_some() {
            continue;
        }
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return OneShotOutcome::Ok(value);
        }
        let err = value.get("error").and_then(Value::as_str).unwrap_or("request failed");
        return OneShotOutcome::ServerErr(err.to_string());
    }
}

/// Send `rename-session` to the daemon at `socket` and parse the reply into
/// the rename-specific outcome. Delegates the connect/write/read-loop to
/// `one_shot_rpc` so the rename CLI verb, the picker helper, and the
/// session-manager overlay share one transport path.
fn rename_rpc(socket: &std::path::Path, new_name: &str) -> RenameOutcome {
    let request = json!({ "cmd": "rename-session", "new_name": new_name, "id": REQUEST_ID });
    match one_shot_rpc(socket, request) {
        OneShotOutcome::Ok(value) => {
            let data = value.get("data").unwrap_or(&Value::Null);
            let socket_path = data.get("socket_path").and_then(Value::as_str).map(PathBuf::from);
            let pid = data.get("pid").and_then(Value::as_u64);
            match (socket_path, pid) {
                (Some(p), Some(pid)) => RenameOutcome::Ok { socket_path: p, pid },
                _ => {
                    RenameOutcome::ServerErr("rename response missing socket_path/pid".into())
                }
            }
        }
        OneShotOutcome::ServerErr(e) => RenameOutcome::ServerErr(e),
        OneShotOutcome::ConnectErr(e) => RenameOutcome::ConnectErr(e),
    }
}

/// Send `rename-session` to the daemon bound at `socket_path` and return
/// the new socket path on success. Shared by the picker's `r` flow so the
/// TUI keybinding and the CLI verb exercise one code path (`rename_rpc`).
pub(crate) fn rename_session_at(
    socket_path: &std::path::Path,
    new_name: &str,
) -> Result<PathBuf, String> {
    match rename_rpc(socket_path, new_name) {
        RenameOutcome::Ok { socket_path, .. } => Ok(socket_path),
        RenameOutcome::ServerErr(e) | RenameOutcome::ConnectErr(e) => Err(e),
    }
}

/// Send `select-workspace` (by index) as a one-shot RPC to the daemon at
/// `socket` so a *different* session lands on workspace `index`. Used by the
/// in-TUI session manager overlay (issue #63 L3) to focus a workspace in
/// another session before the running TUI switches to it. Reuses
/// `one_shot_rpc`, the same path the rename flow rides. Best-effort: an
/// unreachable socket yields `Err` (the caller renders an `[unreachable]`
/// column rather than crashing).
pub(crate) fn select_workspace_remote(
    socket: &std::path::Path,
    index: usize,
) -> Result<(), String> {
    let request = json!({ "cmd": "select-workspace", "index": index, "id": REQUEST_ID });
    match one_shot_rpc(socket, request) {
        OneShotOutcome::Ok(_) => Ok(()),
        OneShotOutcome::ServerErr(e) | OneShotOutcome::ConnectErr(e) => Err(e),
    }
}

fn run_kill_session(global: &GlobalArgs, flags: &FlagMap) -> i32 {
    let target_session = flags.optional("session").or_else(|| global.session.clone());
    let Some(session_name) = target_session else {
        eprintln!("cmux: --session is required");
        return 2;
    };

    let dir = get_runtime_dir(global);
    let sock_path = dir.join(format!("{session_name}.sock"));
    let pid_p = dir.join(format!("{session_name}.pid"));

    if !sock_path.exists() && !pid_p.exists() {
        eprintln!("cmux: session {session_name:?} not found");
        return 1;
    }

    let pid = read_pid_file(&pid_p);
    kill_session_at(&sock_path, pid);

    if global.json {
        println!("{}", json!({ "ok": true }));
    }
    0
}

fn run_kill_stale(global: &GlobalArgs, _flags: &FlagMap) -> i32 {
    let cleaned = kill_stale(global);
    if global.json {
        println!("{}", json!({ "ok": true, "cleaned": cleaned }));
    }
    0
}

/// Kill every stale (socket-not-connectable) session in the runtime dir
/// honoured by `global` and return how many were cleaned. Mirrors the
/// historical `run_kill_stale` semantics (remove `.sock` + `.pid` for each
/// `!live` row). Shared by the `kill-stale` CLI verb, the interactive
/// pre-attach picker (L1), and the in-TUI session manager (L3) so all three
/// exercise one code path.
pub(crate) fn kill_stale(global: &GlobalArgs) -> usize {
    let mut sessions = discover_sessions(global);
    sessions.sort_by(|a, b| a.session.cmp(&b.session));
    let mut cleaned = 0;
    for s in &sessions {
        if !s.live {
            let _ = std::fs::remove_file(&s.socket_path);
            let _ = std::fs::remove_file(mux_core::server::pid_path(&s.socket_path));
            cleaned += 1;
        }
    }
    cleaned
}

/// `cmux rename-session --old <name> --new <name>` (issue #63). Resolves
/// the old session's socket the same way `kill-session` does (parent of
/// `--socket`, else `runtime_dir()`), pre-checks the target, then connects
/// and issues `rename-session`. Exit-code table (scout-plan Q5):
///   0 success · 1 old not found / server ok:false · 2 bad/missing flags,
///     invalid name, or target already live · 3 connect/transport failure.
fn run_rename_session(global: &GlobalArgs, flags: &FlagMap) -> i32 {
    // Parse --old/--new (UsageError -> exit 2).
    let old = match flags.required("old") {
        Ok(v) => v,
        Err(err) => {
            eprintln!("cmux: {}", err.0);
            return 2;
        }
    };
    let new = match flags.required("new") {
        Ok(v) => v,
        Err(err) => {
            eprintln!("cmux: {}", err.0);
            return 2;
        }
    };
    // CLI-side name validation (defence in depth; exit 2 before connecting).
    // The server re-validates as the security authority. Validate both the
    // source (`--old`) and destination (`--new`) so a bad `--old` yields a
    // clean "session name …" error instead of a cryptic "session not found".
    for name in [&old, &new] {
        if let Err(err) = mux_core::server::validate_session_name(name) {
            eprintln!("cmux: {err}");
            return 2;
        }
    }

    let dir = get_runtime_dir(global);
    let old_sock = dir.join(format!("{old}.sock"));
    let old_pid = dir.join(format!("{old}.pid"));
    let new_sock = dir.join(format!("{new}.sock"));

    // Old session must be present (mirrors kill-session's not-found exit 1).
    if !old_sock.exists() && !old_pid.exists() {
        eprintln!("cmux: session {old:?} not found");
        return 1;
    }
    // Criterion 5: refuse a LIVE target BEFORE connecting (exit 2). The
    // server re-checks inside the handler to cover direct API use and the
    // connect-vs-precheck race.
    if mux_core::server::is_session_socket_live(&new_sock) {
        eprintln!("cmux: session {new:?} already exists");
        return 2;
    }

    match rename_rpc(&old_sock, &new) {
        RenameOutcome::Ok { socket_path, pid } => {
            if global.json {
                println!(
                    "{}",
                    json!({
                        "session": new,
                        "socket_path": socket_path.display().to_string(),
                        "pid": pid,
                    })
                );
            }
            // Plain mode is quiet, consistent with kill-session/rename-workspace.
            0
        }
        RenameOutcome::ServerErr(err) => {
            eprintln!("cmux: {err}");
            1
        }
        RenameOutcome::ConnectErr(err) => {
            eprintln!("{err}");
            3
        }
    }
}

// -- issue #76: layout export/apply runners ------------------------------

/// `cmux layout-export --workspace <name-or-id> --output <path>.json`.
/// The server produces the document; the CLIENT writes the file (tmp +
/// rename, refusing symlinked targets) so no daemon ever touches the
/// invoker's filesystem. Exit codes: 0 ok · 1 server/file error · 2 bad
/// flags · 3 transport.
fn run_layout_export(global: &GlobalArgs, flags: &FlagMap) -> i32 {
    let (workspace, output) = match (flags.required("workspace"), flags.required("output")) {
        (Ok(w), Ok(o)) => (w, o),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("cmux: {}", e.0);
            return 2;
        }
    };
    let output = PathBuf::from(output);
    if let Err(e) = refuse_symlink(&output) {
        eprintln!("cmux: {e}");
        return 1;
    }
    let request = json!({ "cmd": "layout-export", "workspace": workspace, "id": REQUEST_ID });
    match one_shot_rpc(&resolve_socket(global), request) {
        OneShotOutcome::Ok(value) => {
            let doc = value.get("data").cloned().unwrap_or(Value::Null);
            let pretty = match serde_json::to_string_pretty(&doc) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("cmux: encoding layout document: {e}");
                    return 1;
                }
            };
            if let Err(e) = write_json_atomic(&output, &pretty) {
                eprintln!("cmux: writing {}: {e}", output.display());
                return 1;
            }
            if global.json {
                println!("{}", json!({ "output": output.display().to_string() }));
            } else {
                println!("{}", output.display());
            }
            0
        }
        OneShotOutcome::ServerErr(e) => {
            eprintln!("cmux: {e}");
            1
        }
        OneShotOutcome::ConnectErr(e) => {
            eprintln!("{e}");
            3
        }
    }
}

/// `cmux layout-apply --input <path>.json --workspace <name>` (issue #76
/// AC2): replay a saved layout, creating the workspace if missing. The
/// file is parsed structurally here (parse errors propagate, exit 2);
/// the schema gate lives server-side so a version mismatch surfaces as
/// the daemon's loud error (exit 1).
fn run_layout_apply(global: &GlobalArgs, flags: &FlagMap) -> i32 {
    let (input, workspace) = match (flags.required("input"), flags.required("workspace")) {
        (Ok(i), Ok(w)) => (i, w),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("cmux: {}", e.0);
            return 2;
        }
    };
    let contents = match std::fs::read_to_string(&input) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cmux: reading layout {input:?}: {e}");
            return 2;
        }
    };
    let document: mux_core::LayoutDocument = match serde_json::from_str(&contents) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cmux: parsing layout {input:?}: {e}");
            return 2;
        }
    };
    let request =
        json!({ "cmd": "layout-apply", "workspace": workspace, "document": document, "id": REQUEST_ID });
    match one_shot_rpc(&resolve_socket(global), request) {
        OneShotOutcome::Ok(value) => {
            if global.json {
                if let Some(data) = value.get("data") {
                    println!("{data}");
                }
            }
            0
        }
        OneShotOutcome::ServerErr(e) => {
            eprintln!("cmux: {e}");
            1
        }
        OneShotOutcome::ConnectErr(e) => {
            eprintln!("{e}");
            3
        }
    }
}

/// `cmux layout-export-all --output-dir <dir>` (issue #76 AC3): fetch one
/// document per workspace and fan them out as `<dir>/<sanitized>.json`.
fn run_layout_export_all(global: &GlobalArgs, flags: &FlagMap) -> i32 {
    let dir = match flags.required("output-dir") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cmux: {}", e.0);
            return 2;
        }
    };
    let dir = PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cmux: creating {}: {e}", dir.display());
        return 2;
    }
    let request = json!({ "cmd": "layout-export-all", "id": REQUEST_ID });
    match one_shot_rpc(&resolve_socket(global), request) {
        OneShotOutcome::Ok(value) => {
            let files = value
                .get("data")
                .and_then(|d| d.get("files"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if files.is_empty() {
                eprintln!("cmux: no workspaces to export");
                return 1;
            }
            let mut written = Vec::new();
            for file in &files {
                let Some(name) = file.get("filename").and_then(Value::as_str) else {
                    eprintln!("cmux: export-all response entry missing filename");
                    return 1;
                };
                // The server sanitizes, but never trust a path component
                // off the wire: refuse anything that could escape --output-dir.
                if name.is_empty()
                    || name == "."
                    || name == ".."
                    || name.contains('/')
                    || name.contains('\\')
                {
                    eprintln!("cmux: refusing unsafe export filename {name:?}");
                    return 1;
                }
                let path = dir.join(name);
                if let Err(e) = refuse_symlink(&path) {
                    eprintln!("cmux: {e}");
                    return 1;
                }
                let pretty = match serde_json::to_string_pretty(file.get("document").unwrap_or(&Value::Null)) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("cmux: encoding layout document: {e}");
                        return 1;
                    }
                };
                if let Err(e) = write_json_atomic(&path, &pretty) {
                    eprintln!("cmux: writing {}: {e}", path.display());
                    return 1;
                }
                written.push(path.display().to_string());
            }
            if global.json {
                println!("{}", json!({ "files": written }));
            } else {
                for path in &written {
                    println!("{path}");
                }
            }
            0
        }
        OneShotOutcome::ServerErr(e) => {
            eprintln!("cmux: {e}");
            1
        }
        OneShotOutcome::ConnectErr(e) => {
            eprintln!("{e}");
            3
        }
    }
}

/// Atomic pretty-JSON write (write-to-temp then rename — the
/// `persist::SessionSnapshot::save` pattern) so a crash or a concurrent
/// reader never observes a truncated file.
fn write_json_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    // A leftover tmp from a crashed run could itself be a symlink; the
    // rename must never write through one.
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// `fs::write` on a symlink path overwrites the TARGET, not the link —
/// refuse symlinked output paths outright (AGENTS.md review checklist).
fn refuse_symlink(path: &std::path::Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(format!("refusing to write through symlink {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(_) => Ok(()), // nothing there yet — fine
    }
}

fn selector_request(flags: &FlagMap) -> Result<Value, UsageError> {
    match (flags.optional("index"), flags.optional("delta")) {
        (Some(_), Some(_)) => Err(UsageError("use only one of --index or --delta".to_string())),
        (Some(index), None) => Ok(json!({ "index": parse_usize("index", &index)? })),
        (None, Some(delta)) => Ok(json!({ "delta": parse_isize("delta", &delta)? })),
        (None, None) => Err(UsageError("one of --index or --delta is required".to_string())),
    }
}

impl FlagMap {
    fn reject_remaining(&self) -> Result<(), UsageError> {
        if let Some(name) = self.values.keys().next() {
            return Err(UsageError(format!("unexpected --{name}")));
        }
        Ok(())
    }

    fn optional(&self, name: &str) -> Option<String> {
        self.values.get(name).cloned()
    }

    /// Boolean-flag reader. The flag may be passed as `--flag` (true),
    /// `--flag=1` (true), `--flag=0` (false), `--flag=true`, `--flag=false`.
    /// Returns None if the flag wasn't passed at all.
    fn optional_bool(&self, name: &str) -> Option<bool> {
        self.values.get(name).map(|v| {
            // Treat any non-"0"/"false"/"no" value as true; explicit
            // "0" / "false" / "no" as false. Matches the common CLI convention.
            !matches!(v.as_str(), "0" | "false" | "no" | "False" | "No" | "FALSE" | "NO")
        })
    }

    fn required(&self, name: &str) -> Result<String, UsageError> {
        self.optional(name).ok_or_else(|| UsageError(format!("--{name} is required")))
    }

    fn required_u64(&self, name: &str) -> Result<u64, UsageError> {
        parse_u64(name, &self.required(name)?)
    }

    fn required_u16(&self, name: &str) -> Result<u16, UsageError> {
        parse_u16(name, &self.required(name)?)
    }

    fn required_usize(&self, name: &str) -> Result<usize, UsageError> {
        parse_usize(name, &self.required(name)?)
    }

    fn required_isize(&self, name: &str) -> Result<isize, UsageError> {
        parse_isize(name, &self.required(name)?)
    }

    fn required_f32(&self, name: &str) -> Result<f32, UsageError> {
        self.required(name)?
            .parse::<f32>()
            .map_err(|_| UsageError(format!("--{name} must be a number")))
    }

    fn required_dir(&self) -> Result<String, UsageError> {
        let dir = self.required("dir")?;
        if dir == "right" || dir == "down" {
            Ok(dir)
        } else {
            Err(UsageError("--dir must be right or down".to_string()))
        }
    }

    fn insert_optional_string(&self, value: &mut Value, name: &str) {
        if let Some(text) = self.optional(name) {
            value[name] = json!(text);
        }
    }

    fn insert_optional_u64(&self, value: &mut Value, name: &str) -> Result<(), UsageError> {
        if let Some(raw) = self.optional(name) {
            value[name] = json!(parse_u64(name, &raw)?);
        }
        Ok(())
    }

    fn insert_optional_size(&self, value: &mut Value) -> Result<(), UsageError> {
        match (self.optional("cols"), self.optional("rows")) {
            (Some(cols), Some(rows)) => {
                value["cols"] = json!(parse_u16("cols", &cols)?);
                value["rows"] = json!(parse_u16("rows", &rows)?);
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err(UsageError("--cols and --rows must be supplied together".to_string())),
        }
    }
}

fn parse_u64(name: &str, value: &str) -> Result<u64, UsageError> {
    value.parse::<u64>().map_err(|_| UsageError(format!("--{name} must be a uint64")))
}

fn parse_u16(name: &str, value: &str) -> Result<u16, UsageError> {
    value.parse::<u16>().map_err(|_| UsageError(format!("--{name} must be a uint16")))
}

fn parse_usize(name: &str, value: &str) -> Result<usize, UsageError> {
    value.parse::<usize>().map_err(|_| UsageError(format!("--{name} must be a usize")))
}

fn parse_isize(name: &str, value: &str) -> Result<isize, UsageError> {
    value.parse::<isize>().map_err(|_| UsageError(format!("--{name} must be an isize")))
}

fn print_empty(_: &Value, _: &mut dyn Write) -> io::Result<()> {
    Ok(())
}

fn print_agents(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    let Some(agents) = data.get("agents").and_then(Value::as_array) else {
        return Ok(());
    };
    // Issue #75 AC2: the line ends with the agent name and last message
    // (`-` when absent), so the message (which may contain spaces) is
    // always the final, unambiguous column.
    for agent in agents {
        writeln!(
            out,
            "{} {} {} {} {} {}",
            agent.get("surface").and_then(Value::as_u64).unwrap_or(0),
            agent.get("state").and_then(Value::as_str).unwrap_or("unknown"),
            agent.get("source").and_then(Value::as_str).unwrap_or("?"),
            agent.get("session").and_then(Value::as_str).unwrap_or("-"),
            agent.get("agent").and_then(Value::as_str).unwrap_or("-"),
            agent.get("message").and_then(Value::as_str).unwrap_or("-"),
        )?;
    }
    Ok(())
}

/// Issue #78 AC1 human output: `<surface> <agent> <confidence> <evidence>`.
fn print_detect_agent(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "{} {} {} {}",
        data.get("surface").and_then(Value::as_u64).unwrap_or(0),
        data.get("agent").and_then(Value::as_str).unwrap_or("unknown"),
        data.get("confidence").and_then(Value::as_str).unwrap_or("none"),
        data.get("evidence").and_then(Value::as_str).unwrap_or(""),
    )
}

/// Issue #78 AC2 human output: `<surface> <agent>` rows, id-ordered.
fn print_detect_agents(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    let Some(agents) = data.get("agents").and_then(Value::as_object) else {
        return Ok(());
    };
    let mut rows: Vec<(u64, &str)> = agents
        .iter()
        .filter_map(|(id, agent)| Some((id.parse::<u64>().ok()?, agent.as_str()?)))
        .collect();
    rows.sort_unstable();
    for (id, agent) in rows {
        writeln!(out, "{id} {agent}")?;
    }
    Ok(())
}

/// Issue #78 AC4 human output: `<name> <kind> <confidence> <pattern>`.
fn print_agent_patterns(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    let Some(patterns) = data.get("patterns").and_then(Value::as_array) else {
        return Ok(());
    };
    for pattern in patterns {
        writeln!(
            out,
            "{} {} {} {}",
            pattern.get("name").and_then(Value::as_str).unwrap_or("?"),
            pattern.get("kind").and_then(Value::as_str).unwrap_or("?"),
            pattern.get("confidence").and_then(Value::as_str).unwrap_or("?"),
            pattern.get("pattern").and_then(Value::as_str).unwrap_or(""),
        )?;
    }

    Ok(())
}

/// Human stdout for `pane-worktree-create` (issue #77): just the
/// worktree path — the thing a caller pipes into something else.
/// `--json` prints the full `{pane,branch,path}` object.
fn print_worktree_created(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}", data.get("path").and_then(Value::as_str).unwrap_or(""))
}

/// Human stdout for `pane-worktree-list` (issue #77): one line per
/// worktree, `branch path label`, in creation order.
fn print_worktrees(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    let Some(worktrees) = data.get("worktrees").and_then(Value::as_array) else {
        return Ok(());
    };
    for worktree in worktrees {
        writeln!(
            out,
            "{} {} {}",
            worktree.get("branch").and_then(Value::as_str).unwrap_or("unknown"),
            worktree.get("path").and_then(Value::as_str).unwrap_or("-"),
            worktree.get("label").and_then(Value::as_str).unwrap_or("-"),
        )?;
    }
    Ok(())
}

/// Human stdout for `cmux get-resolved-config`: pretty-print the
/// server's resolved chrome as JSON (matches the shape that
/// `Config::resolved_chrome_value` produces and `cmux attach
/// --print-resolved-config` prints for the merged view). `--json`
/// mode prints the same object compact via `print_response`.
fn print_get_resolved_config(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    let pretty = serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string());
    writeln!(out, "{pretty}")
}

fn print_identify(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "cmux session={} protocol={} pid={}",
        data.get("session").and_then(Value::as_str).unwrap_or(""),
        data.get("protocol").and_then(Value::as_u64).unwrap_or(0),
        data.get("pid").and_then(Value::as_u64).unwrap_or(0)
    )
}

fn print_read_screen(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    write!(out, "{}", data.get("text").and_then(Value::as_str).unwrap_or(""))
}

fn print_vt_state(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "cols={} rows={} data={}",
        data.get("cols").and_then(Value::as_u64).unwrap_or(0),
        data.get("rows").and_then(Value::as_u64).unwrap_or(0),
        data.get("data").and_then(Value::as_str).unwrap_or("")
    )
}

fn print_surface(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{}", data.get("surface").and_then(Value::as_u64).unwrap_or(0))
}

fn print_tree(data: &Value, out: &mut dyn Write) -> io::Result<()> {
    let Some(workspaces) = data.get("workspaces").and_then(Value::as_array) else {
        return Ok(());
    };
    for workspace in workspaces {
        let workspace_id = id_field(workspace, "id");
        writeln!(
            out,
            "workspace id={} name={} color={} active={}",
            workspace_id,
            atom(workspace.get("name")),
            atom(workspace.get("color")),
            bool_field(workspace, "active")
        )?;
        let Some(screens) = workspace.get("screens").and_then(Value::as_array) else {
            continue;
        };
        for screen in screens {
            let screen_id = id_field(screen, "id");
            writeln!(
                out,
                "screen id={} workspace={} name={} active={} active_pane={}",
                screen_id,
                workspace_id,
                atom(screen.get("name")),
                bool_field(screen, "active"),
                id_field(screen, "active_pane")
            )?;
            let Some(panes) = screen.get("panes").and_then(Value::as_array) else {
                continue;
            };
            for pane in panes {
                let pane_id = id_field(pane, "id");
                if bool_field(pane, "dead") {
                    writeln!(out, "pane id={} screen={} dead=true", pane_id, screen_id)?;
                    continue;
                }
                writeln!(
                    out,
                    "pane id={} screen={} name={} active_tab={}",
                    pane_id,
                    screen_id,
                    atom(pane.get("name")),
                    id_field(pane, "active_tab")
                )?;
                let Some(tabs) = pane.get("tabs").and_then(Value::as_array) else {
                    continue;
                };
                for tab in tabs {
                    let size = tab.get("size");
                    let (cols, rows) = match size {
                        Some(size) if size.is_object() => {
                            (id_field(size, "cols"), id_field(size, "rows"))
                        }
                        _ => (0, 0),
                    };
                    writeln!(
                        out,
                        "tab surface={} pane={} kind={} browser_source={} name={} title={} dead={} cols={} rows={}",
                        id_field(tab, "surface"),
                        pane_id,
                        tab.get("kind").and_then(Value::as_str).unwrap_or(""),
                        atom(tab.get("browser_source")),
                        atom(tab.get("name")),
                        atom(tab.get("title")),
                        bool_field(tab, "dead"),
                        cols,
                        rows
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn id_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn atom(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => serde_json::to_string(text).unwrap_or_default(),
        Some(Value::Null) | None => "null".to_string(),
        Some(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    //! Tests for `cli` internals that a bin-only crate cannot expose to its
    //! integration-test file (`tests/cli.rs` links only against `mux-core`
    //! + the `cmux` binary, not `mux-tui`'s private modules). These unit
    //! tests can call `pub(crate)` helpers directly and drive an in-process
    //! `mux-core` server — no subprocess spawn needed.
    use super::*;
    use mux_core::{server, Mux, SurfaceOptions};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// AC7/picker (scout-plan T11): the non-TUI helper the picker's `r` flow
    /// uses (`rename_session_at`) renames a live session over a direct socket
    /// connection. Driven against an in-process `mux-core` server so no
    // `CARGO_BIN_EXE_cmux` (unavailable to in-source unit tests of a bin
    // crate) is needed. The accept thread outlives the assertion but dies
    // with the test process; the temp socket is unique per run.
    // --- prompt-file frontmatter (issue #77 AC4) ---

    #[test]
    fn prompt_frontmatter_parses_branch_and_label() {
        let text = "---\nbranch: feat-auth\nlabel: auth pane\n---\nFix the login flow.\n";
        let (branch, label) = parse_prompt_frontmatter(text).unwrap();
        assert_eq!(branch.as_deref(), Some("feat-auth"));
        assert_eq!(label.as_deref(), Some("auth pane"));

        // Only one key, blank lines tolerated, CRLF line endings.
        let text = "---\r\nbranch: x\r\n\r\n---\r\nbody";
        let (branch, label) = parse_prompt_frontmatter(text).unwrap();
        assert_eq!(branch.as_deref(), Some("x"));
        assert_eq!(label, None);
    }

    #[test]
    fn prompt_frontmatter_absent_when_file_has_no_block() {
        let (branch, label) = parse_prompt_frontmatter("just a prompt body\n").unwrap();
        assert_eq!(branch, None);
        assert_eq!(label, None);
    }

    #[test]
    fn prompt_frontmatter_rejects_malformed_blocks() {
        // Unterminated block.
        assert!(parse_prompt_frontmatter("---\nbranch: x\nbody").is_err());
        // Unknown key.
        assert!(parse_prompt_frontmatter("---\ncommit: abc\n---\nb").is_err());
        // Duplicate key.
        assert!(parse_prompt_frontmatter("---\nbranch: a\nbranch: b\n---\nb").is_err());
        // Empty value.
        assert!(parse_prompt_frontmatter("---\nbranch:\n---\nb").is_err());
        // Not a key: value line.
        assert!(parse_prompt_frontmatter("---\nfeat-auth\n---\nb").is_err());
    }

    #[test]
    fn rename_session_at_renames_via_socket() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = PathBuf::from("/tmp").join(format!("cmux-t11-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old_sock = dir.join("old.sock");

        // In-process daemon on old.sock (session "old").
        let mux = Mux::new("old", SurfaceOptions::default());
        server::serve(mux, Some(old_sock.clone())).expect("serve should bind old.sock");

        let new_sock =
            rename_session_at(&old_sock, "bar").expect("rename_session_at should succeed");
        assert!(new_sock.exists(), "returned new socket path should exist");
        assert_eq!(
            new_sock.file_name().and_then(|n| n.to_str()),
            Some("bar.sock"),
            "helper should return the new socket path"
        );
        assert!(server::is_session_socket_live(&new_sock));
        assert!(!old_sock.exists(), "old socket should be gone after helper rename");

        // Best-effort cleanup of the (now-renamed) files; the leaked accept
        // thread is reaped when the test process exits.
        let _ = std::fs::remove_file(&new_sock);
        let _ = std::fs::remove_file(server::pid_path(&new_sock));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
