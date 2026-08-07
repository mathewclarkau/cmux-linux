//! cmux: a tmux-like terminal multiplexer TUI.
//!
//! Runs the mux core (workspaces → split panes → tabs on real PTYs,
//! terminal state from libghostty-vt) with a Ratatui frontend, and always
//! exposes the JSON control socket so external frontends can attach.
//! `cmux attach` connects the same TUI to an existing (usually
//! headless) session over that socket, which is how detach/reattach works.

mod agents;
mod aider_hook;
mod antigravity_hook;
mod app;
mod browser_input;
mod claude_hook;
mod cli;
mod clipboard;
mod codex_hook;
mod config;
mod desktop_notify;
mod finder;
mod git_info;
mod grok_hook;
mod help;
mod hook_merge;
mod host_colors;
mod keys;
mod opencode_hook;
mod pi_hook;
mod plugin;
mod plugin_host;
mod session;
mod session_picker;
mod skill_content;
mod socket_watchdog;
mod ssh_bootstrap;
mod theme;
mod ui;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use mux_core::{Mux, SurfaceOptions};
use session::{RemoteSession, Session};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

pub(crate) fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Acquire)
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handle_signal as *const () as libc::sighandler_t);
    }
}

const USAGE: &str = "\
cmux - terminal multiplexer backed by libghostty-vt

USAGE:
  cmux [OPTIONS]           Start a session (TUI + control socket)
  cmux attach [OPTIONS]    Attach to an existing session's socket
  cmux <verb> [OPTIONS]    Run one control-socket command
  cmux workspace-color <name> <color>  Set a named workspace colour
  cmux claude <subcommand> Claude Code hook integration (see below)
  cmux antigravity install-hooks  Antigravity CLI hook integration (see below)
  cmux codex install-hooks        Codex CLI hook integration (see below)
  cmux pi install-hooks           Pi agent extension integration (see below)
  cmux aider install-hooks        Aider wrapper integration (see below)
  cmux grok install-hooks         Grok CLI hook integration (see below)
  cmux opencode install-hooks     opencode plugin integration (see below)
  cmux agents <list|install>     Manage all agent hook integrations (see below)
  cmux plugin <subcommand> Manage cmux-plugin.toml manifests (see below)
  cmux ssh <host> [OPTS]   Open a remote workspace over SSH (see below)

OPTIONS:
  --session <name>   Session name (default: main). Determines the socket path.
  --socket <path>    Explicit control socket path.
  --headless         Run only the control socket, no TUI.
  --term <value>     TERM for child shells (default: xterm-256color).
  --apply-local-config
                    Attach only: overlay the local mux.local.toml/mux.json
                    (theme, tabs, sidebar, keys) on top of the server config.
  --config <path>    Attach only: explicit local overlay file (overrides
                    $CMUX_LOCAL_CONFIG and the XDG defaults).
  --show-local-config-resolution
                    Attach only: print which local config would apply and
                    how many keys it overrides, then exit without attaching.
  --print-resolved-config
                    Attach only: fetch the server's resolved presentation
                    chrome, layer the local overlay on top (requires
                    --apply-local-config), print the merged chrome as JSON,
                    and exit without starting the TUI. For inspecting
                    overlay layering without a live terminal.
  --session-list     Attach only: discover sessions and either print them
                    (--json) or open the interactive picker instead of
                    attaching directly.
  --json             With --session-list: print the discovered sessions as
                    JSON (one object per session, including socket_path)
                    and exit without attaching.
  -V, --version      Print the cmux version and exit.
  -h, --help         Show this help.

SESSION PICKER  (cmux attach --session-list, without --json)
  Lists every discovered cmux session (newest first) and lets you pick one
  to attach in-process. Stale (unconnectable) sessions are shown grey and
  labelled [unreachable]. Exit codes: 0 clean quit, 1 after a destructive
  kill + quit, 2 Ctrl-C, 0 on attach (then the normal attach/detach flow).
    ↑/↓ or j/k  move focus        Enter  attach to focused (live only)
    x  kill focused session (y/N)    s  kill every stale session (y/N)
    n  new session (inline name)     r  rename (stub; L2 will wire it in)
    q / Esc  quit                    Ctrl-C  abort (exit 2)

KEYS (prefix: Ctrl-b)
  c  new tab in pane   B    new browser tab    n/p  next/prev tab
  1-9  select tab
  %  split right       \"  split down          x    close tab
  ,  rename pane       $    rename workspace
  Tab  next screen     S    new screen
  h/j/k/l or arrows    move focus              d    quit (attach: detach)
  w  next workspace    W    new workspace       s    toggle sidebar
  <  browser back      >    browser forward     r/u  browser reload/edit URL
  ?  show key binding help
  Ctrl-b  send a literal Ctrl-b

MOUSE
  Right-click a pane for rename/new tab/split/close; right-click a
  sidebar workspace or a status-bar screen for rename/close. Click
  tab-bar entries to switch tabs (+ for a new tab), and status-bar
  screen entries to switch screens (+ for a new screen).

CLI VERBS
  identify, list-workspaces, send, read-screen, vt-state, new-tab,
  new-browser-tab, new-workspace, new-screen, split, set-ratio,
  set-default-colors, close-surface, close-pane, close-screen,
  close-workspace, rename-pane, rename-surface, rename-screen,
  rename-workspace, set-workspace-color, set-status, workspace-color,
  trigger-flash, resize-surface,
  focus-pane, select-tab, select-screen, select-workspace, move-tab,
  move-workspace, scroll-surface, subscribe, attach-surface, report-agent,
  list-agents, browser-reload, list-sessions, kill-session, kill-stale,
  theme list

SEND
  cmux send --surface <id> --text <text> [--shell auto|fish|bash|zsh|sh|nu|raw]
      Writes input to a PTY surface (stdin is used when neither --text nor
      --bytes is given). --shell enables shell-aware sanitisation (issue
      #35): with fish/bash/zsh/nu, a leading newline is prefixed when the
      text starts with a shell metacharacter ($, !, quote, bracket, ~, #)
      or contains an unclosed quote, so '$ pwd\n' is typed literally
      instead of being interpreted by the shell's line editor. auto
      resolves the pane's shell from /proc on Linux. Default: raw
      (verbatim passthrough, unchanged from before).

CLAUDE CODE HOOK INTEGRATION
  cmux claude install-hooks [--uninstall]
      Wires ~/.claude/settings.json's hooks to call `cmux claude hook`
      on every lifecycle event, merged alongside any hooks already there.
  cmux claude install-skill [--uninstall] [--global]
      Installs the orchestration skill to .claude/skills/cmux-orchestration/SKILL.md
      (or ~/.claude/skills/cmux-orchestration/SKILL.md if --global).
  cmux claude sessions
      Lists recorded Claude Code sessions (session id, cwd, last event).
  cmux claude resume <session-id>
      Opens a new pane in the recorded cwd and runs `claude --resume`.
  cmux claude hook
      Not for interactive use — this is what install-hooks points Claude
      Code's own hook config at.

ANTIGRAVITY CLI INTEGRATION
  cmux antigravity install-hooks [--uninstall] [--global]
      Installs hooks into .agents/hooks.json (or ~/.gemini/config/hooks.json if --global)
      to automatically report state changes to cmux.
  cmux antigravity install-skill [--uninstall] [--global]
      Installs the orchestration skill to .agents/skills/cmux-orchestration/SKILL.md
      (or ~/.gemini/antigravity-cli/skills/cmux-orchestration/SKILL.md if --global).

CODEX CLI INTEGRATION
  cmux codex install-hooks [--uninstall] [--global]
      Installs hooks into .codex/hooks.json (or ~/.codex/hooks.json if --global) and
      enables hooks feature in config.toml to report state to cmux.
  cmux codex install-skill [--uninstall] [--global]
      Installs the orchestration skill to .agents/skills/cmux-orchestration/SKILL.md
      (or ~/.codex/skills/cmux-orchestration/SKILL.md if --global).

PI AGENT INTEGRATION
  cmux pi install-hooks [--uninstall] [--global]
      Installs TypeScript extensions into .pi/extensions/ (or ~/.pi/agent/extensions/
      if --global) to report state changes.
  cmux pi install-skill [--uninstall] [--global]
      Appends the orchestration skill to .pi/APPEND_SYSTEM.md
      (or ~/.pi/agent/APPEND_SYSTEM.md if --global).

AIDER INTEGRATION
  cmux aider install-hooks [--uninstall] [--global]
      Creates a wrapper script at .bin/aider (or ~/.local/bin/aider if --global)
      that wraps the real aider binary to report working/done state.

GROK CLI INTEGRATION
  cmux grok install-hooks [--uninstall] [--global]
      Installs hooks into .grok/hooks.json (or ~/.grok/hooks.json if --global)
      to automatically report state changes to cmux.
  cmux grok install-skill [--uninstall] [--global]
      Installs the orchestration skill to .agents/skills/cmux-orchestration/SKILL.md
      (or ~/.grok/skills/cmux-orchestration/SKILL.md if --global).

AGENT HOOK INTEGRATION
  cmux agents list [--global]
      Lists installed status, version, timestamp, and path for all six agents.
  cmux agents install --all [--uninstall] [--global]
      Installs or removes every registered agent hook, continuing after failures.
  cmux agents install --only <agent> [--uninstall] [--global]
      Installs or removes one registered agent hook.

PLUGIN LOADER (manifest + registry only; no execution yet)
  cmux plugin list                       List installed plugins (read-only)
  cmux plugin install <manifest-path>    Install a plugin from a cmux-plugin.toml
  cmux plugin uninstall <name>           Remove an installed plugin
  cmux plugin enable <name>              Mark a plugin enabled
  cmux plugin disable <name>             Mark a plugin disabled

      These verbs only manage on-disk manifest state and a small JSON
      registry under ~/.local/share/cmux/plugins.json. Plugin *execution*
      (proxying `cmux <plugin-name> <verb>` to a running plugin process,
      WASM/WASI sandboxing) is NOT implemented by this verb group and is
      deferred to a follow-up PR.

REMOTE (SSH) WORKSPACES
  cmux ssh <host> [--name <workspace-name>] [--session <mux-session>]
      Opens a workspace whose tab is a shell on <host> instead of local.
      Builds and caches a cmuxd-remote binary for the remote's OS/arch
      the first time (needs Go on PATH), uploads it, and starts it in
      persistent mode: closing the tab detaches without killing the
      remote shell, and this session's own daemon restarting reattaches
      to it automatically (see mux/docs/getting-started.md).
";

struct Args {
    attach: bool,
    session: String,
    socket: Option<PathBuf>,
    headless: bool,
    term: Option<String>,
    apply_local_config: bool,
    show_local_config_resolution: bool,
    print_resolved_config: bool,
    config: Option<PathBuf>,
    // Issue #63 L1: `cmux attach --session-list [--json]` — discover
    // sessions and either dump them as JSON or open the interactive picker
    // before attaching. Parsed on the `attach` subcommand in parse_args.
    session_list: bool,
    json: bool,
}

/// cmux version, taken from `crates/mux-tui/Cargo.toml` at compile time.
/// Surfaced by `cmux --version` / `cmux -V` (issue #59).
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn parse_args(args: impl IntoIterator<Item = String>) -> Args {
    let mut out = Args {
        attach: false,
        session: "main".to_string(),
        socket: None,
        headless: false,
        term: None,
        apply_local_config: false,
        show_local_config_resolution: false,
        print_resolved_config: false,
        config: None,
        session_list: false,
        json: false,
    };
    let mut args = args.into_iter().peekable();
    if args.peek().map(|s| s.as_str()) == Some("attach") {
        out.attach = true;
        args.next();
    }
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session" => {
                out.session = args.next().unwrap_or_else(|| usage_exit("--session needs a value"))
            }
            "--socket" => {
                out.socket =
                    Some(args.next().unwrap_or_else(|| usage_exit("--socket needs a value")).into())
            }
            "--headless" => out.headless = true,
            // Issue #63 L1: attach-only session discovery flags.
            "--session-list" => out.session_list = true,
            "--json" => out.json = true,
            "--term" => {
                out.term = Some(args.next().unwrap_or_else(|| usage_exit("--term needs a value")))
            }
            "--apply-local-config" => out.apply_local_config = true,
            "--show-local-config-resolution" => out.show_local_config_resolution = true,
            "--print-resolved-config" => out.print_resolved_config = true,
            "--config" => {
                out.config =
                    Some(args.next().unwrap_or_else(|| usage_exit("--config needs a value")).into())
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            // Issue #59: print version and exit. Sits next to `-h`/`--help`
            // so it works in any position (e.g. `cmux --headless -V`).
            "-V" | "--version" => {
                println!("cmux {VERSION}");
                std::process::exit(0);
            }
            other => usage_exit(&format!("unknown argument {other:?}")),
        }
    }
    out
}

fn main() {
    install_signal_handlers();
    let raw_args = std::env::args().skip(1).collect::<Vec<_>>();
    if raw_args.first().map(|arg| arg.as_str()) == Some("help") {
        print!("{USAGE}");
        std::process::exit(0);
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("claude") {
        std::process::exit(claude_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("antigravity") {
        std::process::exit(antigravity_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("codex") {
        std::process::exit(codex_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("pi") {
        std::process::exit(pi_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("aider") {
        std::process::exit(aider_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("grok") {
        std::process::exit(grok_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("opencode") {
        std::process::exit(opencode_hook::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("theme") {
        match raw_args.get(1).map(String::as_str) {
            Some("list") => std::process::exit(theme::run_list()),
            _ => {
                eprintln!("cmux: usage: cmux theme list");
                std::process::exit(2);
            }
        }
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("agents") {
        std::process::exit(agents::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("plugin") {
        std::process::exit(plugin::run(&raw_args[1..]));
    }
    // `cmux <plugin-name> <verb> [args]` — if the first positional arg
    // names an installed, enabled plugin, route the rest of the argv
    // through plugin_host::invoke. Falls through to the standard
    // verb dispatch if the name doesn't match a plugin.
    if let Some(first) = raw_args.first().map(String::as_str) {
        if !first.starts_with('-')
            && first != "workspace-color"
            && first != "agents"
            && first != "ssh"
            && first != "socket-watchdog"
        {
            if plugin::lookup_plugin(first).is_ok() {
                // Resolve the session's control-socket path the same
                // way cli::list does. The plugin's cmux_call host
                // imports write back to this socket.
                let raw_socket: Option<PathBuf> = std::env::var_os("CMUX_MUX_SOCKET")
                    .map(PathBuf::from)
                    .or_else(|| {
                        let mut idx = 0;
                        while idx + 1 < raw_args.len() {
                            if raw_args[idx] == "--socket" {
                                return Some(PathBuf::from(&raw_args[idx + 1]));
                            }
                            idx += 1;
                        }
                        None
                    });
                let socket_path = raw_socket.unwrap_or_else(|| {
                    // Default: ~/.local/share/cmux/cmux-<pid>.sock
                    // (matches mux_core::platform::default_socket_path
                    // when --session is "main"). Plugins running in
                    // an attached cmux usually want to talk back to
                    // the parent cmux's control socket, so honour
                    // CMUX_MUX_SOCKET first.
                    if let Some(home) = std::env::var_os("HOME") {
                        if !home.is_empty() {
                            return PathBuf::from(home)
                                .join(".local")
                                .join("share")
                                .join("cmux")
                                .join("cmux-main.sock");
                        }
                    }
                    PathBuf::from("/tmp/cmux-main.sock")
                });
                std::process::exit(plugin::cmd_call(first, &raw_args[1..], &socket_path));
            }
        }
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("ssh") {
        std::process::exit(ssh_bootstrap::run(&raw_args[1..]));
    }
    if raw_args.first().map(|arg| arg.as_str()) == Some("socket-watchdog") {
        std::process::exit(socket_watchdog::run(&raw_args[1..]));
    }
    let mut command_index = 0;
    while command_index < raw_args.len() {
        match raw_args[command_index].as_str() {
            "--session" | "--socket" => command_index += 2,
            "--json" => command_index += 1,
            _ => break,
        }
    }
    if raw_args.get(command_index).map(String::as_str) == Some("workspace-color") {
        if raw_args.len() != command_index + 3 {
            eprintln!("cmux: usage: cmux workspace-color <name> <color>");
            std::process::exit(2);
        }
        let mut args = raw_args[..command_index].to_vec();
        args.extend([
            "workspace-color".to_string(),
            "--name".to_string(),
            raw_args[command_index + 1].clone(),
            "--color".to_string(),
            raw_args[command_index + 2].clone(),
        ]);
        std::process::exit(cli::run(&args, USAGE));
    }
    if cli::is_cli_invocation(&raw_args) {
        std::process::exit(cli::run(&raw_args, USAGE));
    }
    let mut args = parse_args(raw_args);
    if args.show_local_config_resolution {
        return show_local_config_resolution(args);
    }
    if args.session_list {
        let global = cli::GlobalArgs {
            session: Some(args.session.clone()),
            socket: args.socket.clone(),
            json: args.json,
        };
        if args.json {
            std::process::exit(cli::run_attach_session_list_json(&global));
        }
        // Interactive picker (Claims 2-7). It restores the terminal on every
        // exit path before returning, so the subsequent app::run (Attach)
        // starts from a clean screen.
        match session_picker::run(&global) {
            Ok(session_picker::PickerOutcome::Attach { socket_path, name }) => {
                // Use the EXACT discovered socket_path (not a recomputed
                // default_socket_path(name)) so --socket-scoped discovery
                // reconnects even if runtime_dir() would resolve elsewhere.
                args.socket = Some(socket_path);
                args.session = name;
            }
            Ok(session_picker::PickerOutcome::Quit { destructive }) => {
                std::process::exit(if destructive { 1 } else { 0 });
            }
            Ok(session_picker::PickerOutcome::CtrlC) => std::process::exit(2),
            Err(e) => {
                eprintln!("cmux: {e}");
                std::process::exit(1);
            }
        }
    }
    let result = if args.attach { run_attach(args) } else { run_server(args) };
    if let Err(e) = result {
        eprintln!("cmux: {e}");
        std::process::exit(1);
    }
}

fn run_attach(args: Args) -> anyhow::Result<()> {
    let overlay =
        if args.apply_local_config { resolve_local_overlay(args.config.as_deref()) } else { None };
    let socket_path =
        args.socket.unwrap_or_else(|| mux_core::server::default_socket_path(&args.session));
    let remote = RemoteSession::connect(&socket_path)
        .with_context(|| format!("attaching to cmux session socket at {}", socket_path.display()))?;
    // `--print-resolved-config` is an inspection escape for thin-client
    // attaches (issue #40 blocker 1): fetch the server's resolved chrome,
    // layer the local overlay on top, print the merged chrome as JSON,
    // and exit without starting the TUI. Used by the integration test
    // that proves the overlay layers over the *server* config.
    if args.print_resolved_config {
        return print_resolved_config(remote, overlay);
    }
    run_tui(Session::Remote(remote), args.session, overlay)
}

/// Print the merged resolved chrome (server base + local overlay) as a
/// JSON object to stdout and exit 0 without attaching the TUI. The shape
/// matches `Config::resolved_chrome_value` so a caller can assert the
/// server's theme survived alongside the local overlay's key bindings.
fn print_resolved_config(
    remote: Arc<RemoteSession>,
    overlay: Option<config::Overlay>,
) -> anyhow::Result<()> {
    let data = remote.request(serde_json::json!({ "cmd": "get-resolved-config" }))?;
    let mut config = config::Config::from_server_chrome(&data);
    if let Some(o) = &overlay {
        o.apply(&mut config);
    }
    let json = serde_json::to_string_pretty(&config.resolved_chrome_value())?;
    println!("{json}");
    Ok(())
}

/// Resolve the local overlay for an attach: log which file applies (or that
/// none was found) and return the parsed `Overlay`. Returns `None` when no
/// path resolves or the file fails to parse, so the attach degrades to the
/// server-side config instead of failing.
fn resolve_local_overlay(explicit: Option<&std::path::Path>) -> Option<config::Overlay> {
    match config::local_config_path(explicit) {
        Some(path) => match config::load_overlay_file(&path) {
            Some(overlay) => {
                eprintln!(
                    "cmux: applying local config from {} (overrides {} keys)",
                    path.display(),
                    overlay.override_count()
                );
                Some(overlay)
            }
            None => {
                eprintln!("cmux: no local config found at {}", path.display());
                None
            }
        },
        None => {
            eprintln!("cmux: no local config found");
            None
        }
    }
}

/// Dry-run for `--show-local-config-resolution`: print which local file
/// would apply and how many keys it overrides, then exit without
/// attaching. Exits 0 whether or not a file resolved.
fn show_local_config_resolution(args: Args) {
    if let Some(path) = config::local_config_path(args.config.as_deref()) {
        if let Some(overlay) = config::load_overlay_file(&path) {
            println!(
                "cmux: local config resolves to {} (overrides {} keys)",
                path.display(),
                overlay.override_count()
            );
        } else {
            eprintln!("cmux: no local config found at {}", path.display());
        }
    } else {
        eprintln!("cmux: no local config found");
    }
    std::process::exit(0);
}

fn run_server(args: Args) -> anyhow::Result<()> {
    // Issue #28: inherit orphaned pane grandchildren so mux.shutdown()
    // can reap them instead of leaving them under PID 1.
    let _ = mux_core::process::set_child_subreaper();

    let mut surface_options = SurfaceOptions::default();
    let config = config::load();
    surface_options.chrome_binary = config.browser.chrome_binary.clone();
    surface_options.cdp_url = config.browser.cdp_url.clone();
    surface_options.browser_discover = config.browser.discover;
    surface_options.browser_discover_ports = config.browser.discover_ports.clone();
    surface_options.browser_user_data_dir = config.browser.user_data_dir.clone();
    surface_options.browser_ephemeral = config.browser.ephemeral;
    surface_options.browser_max_capture_megapixels = config.browser.max_capture_megapixels;
    surface_options.browser_capture_scale = config.browser.capture_scale;
    if let Some(term) = args.term {
        surface_options.term = term;
    }
    // Compute the socket path up front so surface children inherit it.
    let socket_path =
        args.socket.unwrap_or_else(|| mux_core::server::default_socket_path(&args.session));
    surface_options.extra_env.push(("CMUX_MUX_SOCKET".into(), socket_path.display().to_string()));

    let mux = Mux::new(args.session.clone(), surface_options);
    // Issue #40 blocker 1: publish this server's resolved presentation
    // chrome (theme/tabs/sidebar/keys) so a thin-client `cmux attach
    // --apply-local-config` can fetch it via the `get-resolved-config`
    // verb and layer its local overlay on top instead of replacing the
    // server config with the laptop's own. Browser and scrollbar stay
    // server-side truth and are not published here.
    mux.set_resolved_chrome(config.resolved_chrome_value());
    mux.restore_session();
    for workspace in &config.workspaces {
        let id = mux.with_state(|state| {
            state.workspaces.iter().find(|ws| ws.name == workspace.name).map(|ws| ws.id)
        });
        let id = match id {
            Some(id) => id,
            None => {
                mux.new_workspace(Some(workspace.name.clone()), None)
                    .with_context(|| format!("creating workspace {}", workspace.name))?;
                mux.with_state(|state| state.workspaces.last().unwrap().id)
            }
        };
        if let Some(color) = &workspace.color {
            mux.set_workspace_color(id, Some(mux_core::server::parse_workspace_color(color)?));
        }
        if let Some(icon) = &workspace.icon {
            mux.set_workspace_icon(id, Some(mux_core::server::parse_workspace_icon(icon)?));
        }
    }
    mux.enable_persistence();
    mux_core::server::serve(mux.clone(), Some(socket_path.clone()))
        .with_context(|| format!("binding control socket at {}", socket_path.display()))?;
    // Issue #27: detached companion that unlinks .sock/.pid if we die via
    // SIGKILL (handlers/atexit never run). Harmless no-op on graceful exit.
    socket_watchdog::spawn(std::process::id(), &socket_path);

    let result = if args.headless {
        run_headless(&mux, &socket_path)
    } else {
        run_tui(Session::Local(mux.clone()), args.session, None)
    };
    mux.shutdown();
    // Issue #28: after known surfaces are killed, sweep anything that
    // reparented to us via PR_SET_CHILD_SUBREAPER (grandchildren whose
    // intermediate parent already exited before surface.kill ran).
    #[cfg(target_os = "linux")]
    {
        mux_core::process::kill_remaining_children();
    }
    mux_core::server::cleanup(&socket_path);
    result
}

fn run_tui(
    session: Session,
    session_label: String,
    overlay: Option<config::Overlay>,
) -> anyhow::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let colors = host_colors::probe_default_colors();
    let color_result = session.set_default_colors(colors);
    let raw_result = crossterm::terminal::disable_raw_mode();
    if let Err(err) = color_result {
        eprintln!("cmux: failed to set default colors: {err}");
    }
    raw_result?;
    app::run(session, session_label, overlay)
}

fn run_headless(mux: &Arc<Mux>, socket_path: &std::path::Path) -> anyhow::Result<()> {
    eprintln!("cmux: headless, control socket at {}", socket_path.display());
    // Keep the process alive; the control socket drives everything and
    // the mux reaps exited surfaces itself.
    let events = mux.subscribe();
    loop {
        if shutdown_requested() {
            break;
        }
        match events.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::park_timeout(std::time::Duration::from_millis(250))
            }
        }
    }
    Ok(())
}

fn usage_exit(msg: &str) -> ! {
    eprintln!("cmux: {msg}\n\n{USAGE}");
    std::process::exit(2);
}
