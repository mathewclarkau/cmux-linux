use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use mux_core::platform::transport;
use serde_json::{json, Value};

const REQUEST_ID: u64 = 1;

type BuildFn = fn(&FlagMap) -> Result<Value, UsageError>;
type PrintFn = fn(&Value, &mut dyn Write) -> io::Result<()>;

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
        name: "new-tab",
        allowed: &["pane", "cwd", "cols", "rows"],
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
        allowed: &["pane", "dir", "cols", "rows"],
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
        allowed: &["surface", "state", "source", "agent-session"],
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
    Ok(value)
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
    let mut value = json!({
        "surface": flags.required_u64("surface")?,
        "state": flags.required("state")?,
        "source": flags.required("source")?,
    });
    if let Some(session) = flags.optional("agent-session") {
        value["session"] = json!(session);
    }
    Ok(value)
}

fn build_list_agents(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_u64(&mut value, "surface")?;
    flags.insert_optional_string(&mut value, "state");
    Ok(value)
}

fn build_kill_session(flags: &FlagMap) -> Result<Value, UsageError> {
    let mut value = json!({});
    flags.insert_optional_string(&mut value, "session");
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
#[derive(Clone)]
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

/// Send `rename-session` to the daemon bound at `socket_path` and return
/// the new socket path on success. Shared by the picker's `r` flow so the
/// TUI keybinding and the CLI verb exercise one code path.
///
/// RED-suite compile scaffolding (issue #63 L2, commit 2): this stub
/// always errors so the T11 helper test is RED. The real connect/send
/// lands in the `feat(mux-tui)` commit and turns T11 green.
#[allow(dead_code)] // wired by the picker 'r' flow (commit 6); unused until then.
pub(crate) fn rename_session_at(
    _socket_path: &std::path::Path,
    _new_name: &str,
) -> Result<PathBuf, String> {
    Err("rename-session not yet implemented".to_string())
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

    if global.json {
        println!("{}", json!({ "ok": true, "cleaned": cleaned }));
    }
    0
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
    for agent in agents {
        writeln!(
            out,
            "{} {} {} {}",
            agent.get("surface").and_then(Value::as_u64).unwrap_or(0),
            agent.get("state").and_then(Value::as_str).unwrap_or("unknown"),
            agent.get("source").and_then(Value::as_str).unwrap_or("?"),
            agent.get("session").and_then(Value::as_str).unwrap_or("-"),
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
    #[test]
    fn rename_session_at_renames_via_socket() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = PathBuf::from("/tmp")
            .join(format!("cmux-t11-{}-{stamp}", std::process::id()));
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
