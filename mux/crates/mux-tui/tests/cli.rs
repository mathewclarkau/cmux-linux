use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mux_core::platform::transport;

struct HeadlessServer {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
}

impl HeadlessServer {
    fn start(name: &str) -> Self {
        let dir = unique_temp_dir(name);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("mux.sock");
        let child = Command::new(bin())
            .args(["--headless", "--socket"])
            .arg(&socket)
            .env("XDG_STATE_HOME", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let server = Self { child, socket, dir };
        server.wait_for_socket();
        server
    }

    fn start_with_config(name: &str, contents: &str) -> Self {
        let dir = unique_temp_dir(name);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("mux.sock");
        let config = dir.join("mux.toml");
        fs::write(&config, contents).unwrap();
        let child = Command::new(bin())
            .args(["--headless", "--socket"])
            .arg(&socket)
            .env("CMUX_MUX_CONFIG", &config)
            .env("XDG_STATE_HOME", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let server = Self { child, socket, dir };
        server.wait_for_socket();
        server
    }

    fn wait_for_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.socket.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("headless server did not create socket at {}", self.socket.display());
    }
}

impl Drop for HeadlessServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn cli_verbs_cover_command_output_errors_and_streams() {
    let server = HeadlessServer::start("matrix");

    let identify = cli(&server, &["identify"]);
    assert_success(&identify);
    assert!(String::from_utf8_lossy(&identify.stdout).starts_with("cmux session="));

    let identify_json = cli(&server, &["--json", "identify"]);
    assert_success(&identify_json);
    let value: serde_json::Value = serde_json::from_slice(&identify_json.stdout).unwrap();
    assert_eq!(value.get("app").and_then(|v| v.as_str()), Some("cmux"));
    assert!(value.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) >= 5);

    let workspace = cli(&server, &["new-workspace", "--name", "cli-test"]);
    assert_success(&workspace);
    let surface = String::from_utf8(workspace.stdout).unwrap().trim().parse::<u64>().unwrap();
    assert!(surface > 0, "new-workspace should print the new surface id");

    let marker = format!("cmux_cli_marker_{}", std::process::id());
    let marker_suffix = std::process::id().to_string();
    let send = cli(
        &server,
        &[
            "send",
            "--surface",
            &surface.to_string(),
            "--text",
            &format!("printf 'cmux_cli_marker_%s\\n' '{marker_suffix}'\n"),
        ],
    );
    assert_success(&send);
    assert!(send.stdout.is_empty(), "mutating commands should be quiet on success");
    let screen = wait_for_screen(&server, surface, &marker);
    assert!(screen.contains(&marker), "screen did not contain marker; got {screen:?}");

    let select_bare = cli(&server, &["select-tab"]);
    assert_eq!(select_bare.status.code(), Some(2));

    let close = cli(&server, &["close-surface", "--surface", &surface.to_string()]);
    assert_success(&close);
    let closed_read = cli(&server, &["read-screen", "--surface", &surface.to_string()]);
    assert_eq!(closed_read.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&closed_read.stderr).contains("unknown surface"));

    let bogus = Command::new(bin())
        .args(["--socket"])
        .arg(server.dir.join("missing.sock"))
        .arg("identify")
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_eq!(bogus.status.code(), Some(3));

    assert_subscribe_reports_tree_changed(&server);
}

#[test]
fn report_agent_and_list_agents_round_trip() {
    let server = HeadlessServer::start("agents");
    let workspace = cli(&server, &["new-workspace", "--name", "agent-test"]);
    assert_success(&workspace);
    let surface = String::from_utf8(workspace.stdout).unwrap().trim().parse::<u64>().unwrap();

    // Nothing reported yet.
    let empty = cli(&server, &["--json", "list-agents"]);
    assert_success(&empty);
    let value: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(value["agents"].as_array().unwrap().len(), 0);

    let report = cli(
        &server,
        &[
            "report-agent",
            "--surface",
            &surface.to_string(),
            "--state",
            "working",
            "--source",
            "hook",
            "--agent-session",
            "sess-abc",
        ],
    );
    assert_success(&report);
    assert!(report.stdout.is_empty(), "report-agent should be quiet on success");

    let list = cli(&server, &["--json", "list-agents"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let agents = value["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["surface"].as_u64(), Some(surface));
    assert_eq!(agents[0]["state"].as_str(), Some("working"));
    assert_eq!(agents[0]["source"].as_str(), Some("hook"));
    assert_eq!(agents[0]["session"].as_str(), Some("sess-abc"));

    // A socket report cannot downgrade an existing hook report.
    let downgrade = cli(
        &server,
        &[
            "report-agent",
            "--surface",
            &surface.to_string(),
            "--state",
            "idle",
            "--source",
            "socket",
        ],
    );
    assert_success(&downgrade);
    let list = cli(&server, &["--json", "list-agents", "--state", "working"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(
        value["agents"].as_array().unwrap().len(),
        1,
        "hook report should still be in effect"
    );

    // Plain (non-JSON) output is one line per agent.
    let plain = cli(&server, &["list-agents"]);
    assert_success(&plain);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert_eq!(text.trim(), format!("{surface} working hook sess-abc"));

    // Bad state/source are rejected.
    let bad = cli(
        &server,
        &[
            "report-agent",
            "--surface",
            &surface.to_string(),
            "--state",
            "nonsense",
            "--source",
            "hook",
        ],
    );
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn agent_session_round_trips_through_list_workspaces_json() {
    // AC2 fix: `pane_json` serialises `agent_session` per tab (alongside
    // `agent_state`) so a remote-attach client reading `list-workspaces`
    // sees the session id, not `None`. The client tree-parsing test in
    // `session/tree.rs` covers the read-back; this test pins the server
    // half of the round trip end to end through the real control socket.
    let server = HeadlessServer::start("agent-session-rpc");
    let workspace = cli(&server, &["new-workspace", "--name", "agent-rpc"]);
    assert_success(&workspace);
    let surface = String::from_utf8(workspace.stdout)
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap();

    let report = cli(
        &server,
        &[
            "report-agent",
            "--surface",
            &surface.to_string(),
            "--state",
            "working",
            "--source",
            "hook",
            "--agent-session",
            "sess-rpc-42",
        ],
    );
    assert_success(&report);

    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();

    // Walk every tab's JSON to find the surface we reported on and confirm
    // both the state and the session id survived the round trip.
    let tab = value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .flat_map(|ws| ws["screens"].as_array().into_iter().flatten())
        .flat_map(|screen| screen["panes"].as_array().into_iter().flatten())
        .flat_map(|pane| pane["tabs"].as_array().into_iter().flatten())
        .find(|tab| tab["surface"].as_u64() == Some(surface))
        .expect("reported surface present in list-workspaces");
    assert_eq!(tab["agent_state"].as_str(), Some("working"));
    assert_eq!(tab["agent_session"].as_str(), Some("sess-rpc-42"));
}

#[test]
fn set_workspace_color_sets_and_clears() {
    let server = HeadlessServer::start("workspace-color");

    // Create a workspace first -- HeadlessServer::start doesn't auto-create
    // one, and `list-workspaces` returns an empty array otherwise (panicking
    // the `[0]` index below). Caught by pr-build.yml in CI run 29032172606
    // (2026-07-10, issue #16).
    let created = cli(&server, &["new-workspace", "--name", "color-test"]);
    assert_success(&created);
    let _created_id: u64 = String::from_utf8(created.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("new-workspace should print a surface id");

    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let workspace_id = value["workspaces"][0]["id"].as_u64().unwrap();
    assert_eq!(value["workspaces"][0]["color"], serde_json::Value::Null);

    let set = cli(
        &server,
        &["set-workspace-color", "--workspace", &workspace_id.to_string(), "--color", "#ff8800"],
    );
    assert_success(&set);
    assert!(set.stdout.is_empty(), "set-workspace-color should be quiet on success");

    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["color"].as_str(), Some("#ff8800"));

    let preset = cli(
        &server,
        &["set-workspace-color", "--workspace", &workspace_id.to_string(), "--color", "blue"],
    );
    assert_success(&preset);
    let list = cli(&server, &["--json", "list-workspaces"]);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["color"].as_str(), Some("#0000ff"));

    // Restore the colour used by the plain-output assertion below.
    assert_success(&cli(
        &server,
        &["set-workspace-color", "--workspace", &workspace_id.to_string(), "--color", "#ff8800"],
    ));

    // Plain (non-JSON) output surfaces the color too.
    let plain = cli(&server, &["list-workspaces"]);
    assert_success(&plain);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert!(
        text.lines().next().unwrap().contains("color=\"#ff8800\""),
        "expected color in plain output, got: {text}"
    );

    // An explicit empty value clears it.
    let clear = cli(
        &server,
        &["set-workspace-color", "--workspace", &workspace_id.to_string(), "--colour", ""],
    );
    assert_success(&clear);
    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["color"], serde_json::Value::Null);

    // Omitting --colour entirely is a usage error, not a silent clear.
    let missing = cli(&server, &["set-workspace-color", "--workspace", &workspace_id.to_string()]);
    assert_eq!(missing.status.code(), Some(2));

    // A malformed hex value is rejected by the server.
    let bad = cli(
        &server,
        &["set-workspace-color", "--workspace", &workspace_id.to_string(), "--colour", "nope"],
    );
    assert_eq!(bad.status.code(), Some(1));
}

#[test]
fn status_icon_round_trips_and_rejects_unknown_names() {
    let server = HeadlessServer::start("workspace-icon");
    assert_success(&cli(&server, &["new-workspace", "--name", "first"]));
    assert_success(&cli(&server, &["new-workspace", "--name", "active"]));
    let list = cli(&server, &["--json", "list-workspaces"]);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let first = value["workspaces"][0]["id"].as_u64().unwrap().to_string();

    assert_success(&cli(&server, &["set-status", "--workspace", &first, "--icon", "robot"]));
    assert_success(&cli(&server, &["set-status", "--icon", "eye"]));
    let list = cli(&server, &["--json", "list-workspaces"]);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["icon"].as_str(), Some("🤖"));
    assert_eq!(value["workspaces"][1]["icon"].as_str(), Some("👁"));

    let bad = cli(&server, &["set-status", "--icon", "bogus"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown workspace icon"));
}

#[test]
fn workspace_color_shorthand_creates_named_workspace() {
    let server = HeadlessServer::start("workspace-color-short");
    let output = Command::new(bin())
        .arg("--socket")
        .arg(&server.socket)
        .args(["workspace-color", "Build Team", "green"])
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);

    let list = cli(&server, &["--json", "list-workspaces"]);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["name"].as_str(), Some("Build Team"));
    assert_eq!(value["workspaces"][0]["color"].as_str(), Some("#00ff00"));
}

#[test]
fn configured_workspaces_apply_color_and_icon_at_startup() {
    let server = HeadlessServer::start_with_config(
        "workspace-config",
        "[[workspaces]]\nname = \"Configured\"\ncolor = \"purple\"\nicon = \"gear\"\n",
    );
    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["name"].as_str(), Some("Configured"));
    assert_eq!(value["workspaces"][0]["color"].as_str(), Some("#800080"));
    assert_eq!(value["workspaces"][0]["icon"].as_str(), Some("⚙"));
}

#[test]
fn set_default_colors_regression_keeps_working() {
    let server = HeadlessServer::start("default-colors-regression");
    assert_success(&cli(&server, &["new-workspace", "--name", "colours"]));
    let set = cli(&server, &["set-default-colors", "--fg", "#112233", "--bg", "#445566"]);
    assert_success(&set);
    assert_success(&cli(&server, &["new-tab"]));
}

// Refuses when the install target is a symlink; exit non-zero, message
// mentions "symlink", and the symlink target file is byte-identical after the
// run (regression test for the symlink_metadata pre-check in claude_hook.rs).
#[test]
fn install_skill_refuses_symlinks() {
    // AC1 setup: a temp project dir whose .claude/skills/cmux-orchestration/SKILL.md
    // is a symlink to a temp file with known content. Drop guard removes both.
    let guard = SymlinkSkillFixture::new();

    // Invoke the REAL cmux binary (integration test, not a unit call into
    // run_install_skill), so AC3's "remove the check and the test fails" holds.
    // `claude` MUST be arg[0] (main.rs dispatches it before --socket parsing),
    // so we cannot use the cli() helper; install-skill never used the socket
    // anyway. current_dir pins skill_path(false)'s CWD-relative target.
    let output = Command::new(bin())
        .args(["claude", "install-skill"])
        .current_dir(&guard.project_dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .expect("failed to spawn cmux claude install-skill");

    // Assertion 1 — exit code is non-zero (the refusal path returns 1;
    // claude_hook.rs:474).
    assert!(
        !output.status.success(),
        "install-skill must refuse a symlink target, got status {:?}\n\
         stdout:\n{}\n\
         stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Assertion 3 — combined stdout+stderr mentions "symlink"
    // (case-insensitive), so the user learns *why* it failed
    // (the eprintln at claude_hook.rs:470-473).
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.to_lowercase().contains("symlink"),
        "error output must mention \"symlink\", got: {combined:?}"
    );

    // Assertion 2 — the symlink target file is byte-for-byte unchanged,
    // proving the refusal happened *before* fs::write (which would otherwise
    // follow the symlink and clobber the target — the exact attack vector).
    let read_back =
        fs::read(&guard.target_path).expect("symlink target file must still exist after refusal");
    assert_eq!(
        read_back,
        guard.original_content.as_bytes(),
        "refusal must not have written through the symlink to its target"
    );

    // guard goes out of scope here: Drop removes the symlink, the target
    // file, and the temp project dir — even if any assertion above panicked.
}

// Regression test for the symlink_metadata guard added to
// grok_hook::run_install_skill (PR #24 follow-up). Sibling to the Claude
// test above (PR #18 / issue #10) — the grok non-global skill path is
// `.agents/skills/cmux-orchestration/SKILL.md` (see grok_hook.rs:147-149),
// so the fixture is built with `top = ".agents"`. Exercises the same
// attack vector on the new grok install path: an attacker-placed symlink
// must NOT silently redirect fs::write at the target file.
#[test]
fn install_skill_refuses_symlinks_grok() {
    // AC1 setup: a temp project dir whose .agents/skills/cmux-orchestration/SKILL.md
    // is a symlink to a temp file with known content. Drop guard removes both.
    let guard = SymlinkSkillFixture::new_for(".agents", "install-skill-symlink-grok");

    // Invoke the REAL cmux binary (integration test, not a unit call into
    // run_install_skill), so removing the check makes this test fail.
    // `grok` MUST be arg[0] (main.rs dispatches it before --socket parsing);
    // install-skill never uses the socket anyway. current_dir pins
    // skill_path(false)'s CWD-relative target.
    let output = Command::new(bin())
        .args(["grok", "install-skill"])
        .current_dir(&guard.project_dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .expect("failed to spawn cmux grok install-skill");

    // Assertion 1 — exit code is non-zero (the refusal path returns 1;
    // grok_hook.rs::run_install_skill install branch).
    assert!(
        !output.status.success(),
        "grok install-skill must refuse a symlink target, got status {:?}\n\
         stdout:\n{}\n\
         stderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Assertion 2 — combined stdout+stderr mentions "symlink"
    // (case-insensitive), so the user learns *why* it failed
    // (the eprintln in grok_hook::run_install_skill).
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.to_lowercase().contains("symlink"),
        "error output must mention \"symlink\", got: {combined:?}"
    );

    // Assertion 3 — the symlink target file is byte-for-byte unchanged,
    // proving the refusal happened *before* fs::write (which would
    // otherwise follow the symlink and clobber the target — the exact
    // attack vector).
    let read_back =
        fs::read(&guard.target_path).expect("symlink target file must still exist after refusal");
    assert_eq!(
        read_back,
        guard.original_content.as_bytes(),
        "refusal must not have written through the symlink to its target"
    );

    // guard goes out of scope here: Drop removes the symlink, the target
    // file, and the temp project dir — even if any assertion above panicked.
}

#[test]
fn trigger_flash_returns_success() {
    let server = HeadlessServer::start("trigger-flash");

    // Create a workspace. `new-workspace` prints the *surface* id of the
    // workspace's initial tab to stdout (a bare u64), not the workspace id.
    let created = cli(&server, &["new-workspace", "--name", "flash-test"]);
    assert_success(&created);
    let surface: u64 = String::from_utf8(created.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("new-workspace should print a surface id");

    // Discover the workspace id via `list-workspaces --json` — this is the
    // `WorkspaceId` that `trigger-flash --workspace` expects (the same two-step
    // lookup the template `set_workspace_color_sets_and_clears` uses).
    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let workspace_id = value["workspaces"][0]["id"].as_u64().unwrap();

    // 1. Real workspace, no --surface -> success, quiet on stdout. Human mode
    //    uses `print_empty`, which writes nothing on success (scout: cli.rs
    //    lines 875–877, VerbSpec `print: print_empty` at lines 190–196).
    let out = cli(&server, &["trigger-flash", "--workspace", &workspace_id.to_string()]);
    assert_success(&out);
    assert!(out.stdout.is_empty(), "trigger-flash should be quiet on success");

    // 2. Real workspace, explicit --surface -> success, quiet on stdout.
    //    `--surface` is advisory and NOT validated server-side (scout: server.rs
    //    lines 265–272, mux.rs ~lines 1002–1013), so the real surface id printed
    //    by new-workspace is fine.
    let out = cli(
        &server,
        &[
            "trigger-flash",
            "--workspace",
            &workspace_id.to_string(),
            "--surface",
            &surface.to_string(),
        ],
    );
    assert_success(&out);
    assert!(out.stdout.is_empty(), "trigger-flash should be quiet on success");

    // 3. Unknown workspace id -> server-side `anyhow::bail!("unknown workspace {workspace}")`
    //    (server.rs line 863), surfaced by `print_response` (cli.rs lines 550–555)
    //    as exit code 1 and a bare (no `cmux:` prefix) stderr line
    //    `unknown workspace 99999`.
    let out = cli(&server, &["trigger-flash", "--workspace", "99999"]);
    assert_eq!(out.status.code(), Some(1), "unknown workspace should fail with exit 1");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown workspace"),
        "expected 'unknown workspace' in stderr, got: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.stdout.is_empty(), "no stdout on server error");

    // 4. Missing --workspace flag entirely -> client-side usage error from
    //    `build_trigger_flash` (cli.rs lines 696–701) -> `flags.required_u64("workspace")`
    //    (cli.rs lines 798–804) -> `UsageError("--workspace is required")` (line 799),
    //    which `run_command` prints as `cmux: --workspace is required` (line 408)
    //    and returns exit code 2. This is the same usage-error contract the
    //    template asserts for `set-workspace-color`'s missing `--colour` case
    //    (lines 377–378: `assert_eq!(missing.status.code(), Some(2));`).
    let out = cli(&server, &["trigger-flash"]);
    assert_eq!(out.status.code(), Some(2), "missing --workspace should be a usage error (exit 2)");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--workspace is required"),
        "expected '--workspace is required' in stderr, got: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.stdout.is_empty(), "no stdout on usage error");
}

fn assert_subscribe_reports_tree_changed(server: &HeadlessServer) {
    let mut child = Command::new(bin())
        .args(["--socket"])
        .arg(&server.socket)
        .arg("subscribe")
        .env_remove("CMUX_MUX_SOCKET")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });

    std::thread::sleep(Duration::from_millis(200));
    let tab = cli(server, &["new-tab"]);
    assert_success(&tab);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(250)) {
            lines.push(line.clone());
            if line.contains("\"event\":\"tree-changed\"") {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("subscribe did not print tree-changed event; lines={lines:?}");
}

#[test]
fn stream_preserves_partial_line_across_read_timeout() {
    let dir = unique_temp_dir("partial-line");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("mux.sock");
    let listener = transport::listen(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        let mut request = String::new();
        {
            let read_half = stream.try_clone_box().unwrap();
            let mut reader = BufReader::new(read_half);
            reader.read_line(&mut request).unwrap();
        }
        assert!(request.contains("\"cmd\":\"subscribe\""));

        stream.write_all(br#"{"event":"status","message":""#).unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(350));
        stream.write_all(br#"split-line-ok"}"#).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
    });

    let output = Command::new(bin())
        .args(["--socket"])
        .arg(&socket)
        .arg("subscribe")
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    server.join().unwrap();
    let _ = fs::remove_file(&socket);
    let _ = fs::remove_dir_all(&dir);

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "{\"event\":\"status\",\"message\":\"split-line-ok\"}\n"
    );
}

fn wait_for_screen(server: &HeadlessServer, surface: u64, marker: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        let output = cli(server, &["read-screen", "--surface", &surface.to_string()]);
        assert_success(&output);
        last = String::from_utf8(output.stdout).unwrap();
        if last.contains(marker) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

fn cli(server: &HeadlessServer, args: &[&str]) -> Output {
    Command::new(bin())
        .args(["--socket"])
        .arg(&server.socket)
        .args(args)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "expected success, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    PathBuf::from("/tmp").join(format!("cmux-cli-{name}-{}-{stamp}", std::process::id()))
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cmux")
}

/// Fixture for the install-skill symlink-refusal test.
///
/// Owns a temp "project" dir acting as CWD for `cmux claude install-skill`
/// (whose non-global `skill_path` is `.claude/skills/cmux-orchestration/SKILL.md`,
/// relative to CWD — see claude_hook.rs:427-432). Inside it we place:
///   <project>/.claude/skills/cmux-orchestration/SKILL.md -> <project>/target.txt
///
/// On drop we remove the symlink, the target file, and the whole project dir,
/// even if the test panicked — mirroring HeadlessServer::drop (tests/cli.rs:45-52).
struct SymlinkSkillFixture {
    /// Temp dir used as `current_dir` for the cmux subprocess.
    project_dir: PathBuf,
    /// Absolute path of the symlink itself (the path install-skill targets).
    symlink_path: PathBuf,
    /// Absolute path of the regular file the symlink points at.
    target_path: PathBuf,
    /// Byte content written to `target_path` at construction, kept so the
    /// test can compare against it after running install-skill.
    original_content: String,
}

impl SymlinkSkillFixture {
    /// Default fixture for the Claude install-skill path (`.claude/...`).
    fn new() -> Self {
        Self::new_for(".claude", "install-skill-symlink")
    }

    /// Build a fixture for an install-skill variant whose non-global path is
    /// `<top>/skills/cmux-orchestration/SKILL.md` relative to CWD (e.g.
    /// `.claude` for claude, `.agents` for grok — see grok_hook.rs:147-149).
    /// `tag` keeps the temp dir name unique per variant so parallel test
    /// runs never collide.
    fn new_for(top: &str, tag: &str) -> Self {
        const KNOWN_CONTENT: &str =
            "#!/bin/sh\necho this is a precious file that must survive install-skill\n";

        let project_dir = unique_temp_dir(tag);
        fs::create_dir_all(&project_dir).expect("mkdir project_dir");

        // The exact non-global path install-skill will write to.
        let symlink_path: PathBuf =
            project_dir.join(top).join("skills").join("cmux-orchestration").join("SKILL.md");
        fs::create_dir_all(symlink_path.parent().expect("symlink_path has parent"))
            .expect("mkdir skills/cmux-orchestration");

        // The real file the symlink redirects to. Putting it inside the same
        // temp project dir keeps cleanup to one remove_dir_all on drop.
        let target_path: PathBuf = project_dir.join("target.txt");
        fs::write(&target_path, KNOWN_CONTENT).expect("write target.txt");
        let original_content = KNOWN_CONTENT.to_string();

        // Reuse the existing precondition: install-skill calls create_dir_all
        // on the parent (claude_hook.rs:460-462) — idempotent here.
        symlink(&target_path, &symlink_path).expect("create symlink at skill path");

        // Sanity: the symlink really resolves to a real file, so that without
        // the check fs::write would follow it and clobber target_path. This
        // guards against a misconfigured fixture silently making the test
        // pass for the wrong reason (a missing symlink ⇒ error before check).
        assert!(symlink_path.exists(), "fixture broken: symlink does not resolve to a real file");
        let meta = fs::symlink_metadata(&symlink_path)
            .expect("symlink_metadata on the freshly created symlink");
        assert!(meta.file_type().is_symlink(), "fixture broken: SKILL.md is not a symlink");

        Self { project_dir, symlink_path, target_path, original_content }
    }
}

impl Drop for SymlinkSkillFixture {
    fn drop(&mut self) {
        // Order matters: remove the symlink before its target so we never
        // leave a dangling symlink pointing at a removed file mid-cleanup.
        let _ = fs::remove_file(&self.symlink_path);
        let _ = fs::remove_file(&self.target_path);
        let _ = fs::remove_dir_all(&self.project_dir);
    }
}

#[test]
fn list_sessions_lists_active_headless_session() {
    let dir = unique_temp_dir("list-sess");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("test-list.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid_file = dir.join("test-list.pid");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() && pid_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(socket.exists(), "socket must exist");
    assert!(pid_file.exists(), "pid file must exist");

    let output = Command::new(bin())
        .args(["--socket"])
        .arg(&socket)
        .arg("list-sessions")
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("test-list"), "output should list session test-list, got: {stdout}");
    assert!(stdout.contains("live"), "output should show live status, got: {stdout}");

    let json_output = Command::new(bin())
        .args(["--socket"])
        .arg(&socket)
        .args(["--json", "list-sessions"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&json_output);
    let value: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["session"] == "test-list" && s["status"] == "live"),
        "expected test-list with status live in json, got {value}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn kill_session_terminates_daemon_and_cleans_files() {
    let dir = unique_temp_dir("kill-sess");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("target-sess.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid_file = dir.join("target-sess.pid");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() && pid_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(socket.exists());
    assert!(pid_file.exists());

    let output = Command::new(bin())
        .args(["--socket"])
        .arg(&socket)
        .args(["kill-session", "--session", "target-sess"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);

    // Verify process is killed.
    let status = child.wait().unwrap();
    assert!(
        status.success() || status.code().is_none(),
        "process should exit cleanly on SIGTERM or be killed"
    );
    assert!(!socket.exists(), "socket should be removed");
    assert!(!pid_file.exists(), "pid file should be removed");

    // Verify killing non-existent session returns exit 1.
    let missing = Command::new(bin())
        .args(["--socket"])
        .arg(&socket)
        .args(["kill-session", "--session", "nonexistent"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn kill_stale_removes_stale_pair_and_leaves_live_untouched() {
    let dir = unique_temp_dir("kill-stale");
    fs::create_dir_all(&dir).unwrap();
    let live_socket = dir.join("live-sess.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&live_socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let live_pid = dir.join("live-sess.pid");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if live_socket.exists() && live_pid.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    // Create fake stale files (nonexistent PID 999999)
    let stale_socket = dir.join("stale-sess.sock");
    let stale_pid = dir.join("stale-sess.pid");
    fs::write(&stale_socket, "fake").unwrap();
    fs::write(&stale_pid, "999999\n").unwrap();

    let output = Command::new(bin())
        .args(["--socket"])
        .arg(&live_socket)
        .args(["kill-stale"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);

    assert!(!stale_socket.exists(), "stale socket should be removed");
    assert!(!stale_pid.exists(), "stale pid file should be removed");
    assert!(live_socket.exists(), "live socket should be untouched");
    assert!(live_pid.exists(), "live pid file should be untouched");

    // Idempotent test: run again when no stale sessions remain.
    let output2 = Command::new(bin())
        .args(["--socket"])
        .arg(&live_socket)
        .args(["kill-stale"])
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output2);

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn serve_recovers_from_stale_socket() {
    let dir = unique_temp_dir("recover-stale");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("recover.sock");
    let pid_file = dir.join("recover.pid");

    // Synthetic stale pair (dead PID). We no longer rely on SIGKILL leaving
    // files behind — the socket-watchdog (#27) cleans those up within ≤5s.
    fs::write(&socket, b"").unwrap();
    fs::write(&pid_file, "999999\n").unwrap();
    assert!(socket.exists());
    assert!(pid_file.exists());

    // Start a new daemon on the same socket path — should clear stale socket and bind
    let mut child2 = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut rebound = false;
    while Instant::now() < deadline {
        if socket.exists() && pid_file.exists() {
            if let Ok(pid_str) = fs::read_to_string(&pid_file) {
                if pid_str.trim() == child2.id().to_string() {
                    rebound = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(rebound, "new daemon should overwrite stale pid file with its own pid");

    let _ = child2.kill();
    let _ = child2.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// Issue #27 acceptance: `kill -9` on a headless daemon leaves zero
/// leftover `.sock`/`.pid` files within ≤5s without operator action.
#[test]
fn sigkill_watchdog_removes_socket_and_pid_within_5s() {
    let dir = unique_temp_dir("sigkill-wd");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("wd-sess.sock");
    let pid_file = dir.join("wd-sess.pid");

    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() && pid_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(socket.exists(), "daemon should create socket");
    assert!(pid_file.exists(), "daemon should create pid file");

    // SIGKILL bypasses handlers/atexit — only the external watchdog cleans up.
    let _ = child.kill();
    let _ = child.wait();

    let cleanup_deadline = Instant::now() + Duration::from_secs(5);
    let mut cleaned = false;
    while Instant::now() < cleanup_deadline {
        if !socket.exists() && !pid_file.exists() {
            cleaned = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        cleaned,
        "watchdog should unlink socket+pid within 5s after SIGKILL (socket={}, pid={})",
        socket.exists(),
        pid_file.exists()
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Issue #40: `cmux attach --show-local-config-resolution` is a dry run that
/// resolves the local overlay file (theme/sidebar_rail + keys/prefix here)
/// and prints the path plus the override count, without attaching to a
/// server or starting the TUI. Exits 0 and needs no live session.
#[test]
fn show_local_config_resolution_prints_path_without_attaching() {
    let dir = unique_temp_dir("show-local-config-res");
    fs::create_dir_all(&dir).unwrap();
    let cmux_dir = dir.join("cmux");
    fs::create_dir_all(&cmux_dir).unwrap();
    fs::write(
        cmux_dir.join("mux.local.toml"),
        "[theme]\nsidebar_rail = 42\n[keys]\nprefix = \"ctrl+s\"\n",
    )
    .unwrap();

    let output = Command::new(bin())
        .args(["attach", "--show-local-config-resolution"])
        .env("XDG_CONFIG_HOME", &dir)
        .env_remove("CMUX_LOCAL_CONFIG")
        .env_remove("CMUX_MUX_CONFIG")
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\n{}",
        output.status.code(),
        combined
    );
    assert!(
        combined.contains("mux.local.toml"),
        "expected the resolved overlay path, got: {combined}"
    );
    assert!(
        combined.contains("overrides 2 keys"),
        "expected theme+keys = 2 overrides, got: {combined}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Issue #40 round-2 blocker 1: a thin-client `cmux attach
/// --apply-local-config` must layer the local overlay on top of the
/// *server's* resolved config, not replace it with the laptop's own. We
/// start a headless server whose `mux.json` sets a distinctive theme
/// colour (server-side truth), then run `cmux attach --socket <sock>
/// --apply-local-config --print-resolved-config` with a local overlay
/// that overrides a key binding (NOT theme), and assert the merged
/// chrome JSON carries the server's theme colour AND the local overlay's
/// key binding — proving layering, not replacement. `--print-resolved-config`
/// is the attach-only inspection escape added for this: it fetches the
/// server chrome, applies the overlay, and prints the merged result as
/// JSON without starting the TUI.
#[test]
fn attach_overlay_layers_over_server_config() {
    let dir = unique_temp_dir("attach-overlay-layering");

    // Server-side config: only a theme colour, so the local overlay (keys
    // only) must NOT clobber it.
    let server_cfg_root = dir.join("server-config");
    let server_cmux_dir = server_cfg_root.join("cmux");
    fs::create_dir_all(&server_cmux_dir).unwrap();
    fs::write(
        server_cmux_dir.join("mux.json"),
        r##"{"theme": {"sidebar_rail": "#112233"}}"##,
    )
    .unwrap();

    let socket = dir.join("mux.sock");
    let mut server = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_CONFIG_HOME", &server_cfg_root)
        .env_remove("CMUX_MUX_CONFIG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !socket.exists() {
        let _ = server.kill();
        let _ = server.wait();
        panic!(
            "headless server did not create socket at {}",
            socket.display()
        );
    }
    // Give the server a moment to register its resolved chrome (it is set
    // before `serve()` binds, so a live socket implies it is published).
    std::thread::sleep(Duration::from_millis(100));

    // Local laptop config dir: a local overlay that overrides a key
    // binding only, not theme, so the server theme must survive.
    let local_cfg_root = dir.join("local-config");
    let local_cmux_dir = local_cfg_root.join("cmux");
    fs::create_dir_all(&local_cmux_dir).unwrap();
    fs::write(
        local_cmux_dir.join("mux.local.toml"),
        "[keys]\nprefix = \"ctrl+s\"\n",
    )
    .unwrap();

    let output = Command::new(bin())
        .args(["attach", "--socket"])
        .arg(&socket)
        .args(["--apply-local-config", "--print-resolved-config"])
        .env("XDG_CONFIG_HOME", &local_cfg_root)
        .env_remove("CMUX_LOCAL_CONFIG")
        .env_remove("CMUX_MUX_CONFIG")
        .output()
        .unwrap();

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&dir);

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "attach --print-resolved-config failed: {combined}"
    );

    let merged: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!("expected merged chrome JSON on stdout, parse failed:{e}\n{combined}")
        });

    // Server's theme colour survived: the overlay layered, not replaced.
    assert_eq!(
        merged["theme"]["sidebar_rail"].as_str(),
        Some("#112233"),
        "server theme colour lost — overlay did not layer over server config: {merged}"
    );
    // Local overlay's key binding applied on top of the server config.
    assert_eq!(
        merged["keys"]["prefix"].as_str(),
        Some("ctrl+s"),
        "local overlay key binding missing from merged config: {merged}"
    );
}

/// Issue #40 blocker 1: the `get-resolved-config` protocol verb is also
/// exposed as a standalone read-only CLI verb (`cmux get-resolved-config`)
/// so ops scripts can inspect the server's chrome without attaching. It
/// must return the server's published chrome verbatim (no local overlay).
/// We start a headless server whose config sets a distinctive theme
/// colour, then call `cmux --json get-resolved-config` against its
/// socket and assert the colour is present in the returned object.
#[test]
fn get_resolved_config_cli_verb_returns_server_chrome() {
    let dir = unique_temp_dir("get-resolved-config");

    // Server-side config: a distinctive theme colour only.
    let server_cfg_root = dir.join("server-config");
    let server_cmux_dir = server_cfg_root.join("cmux");
    fs::create_dir_all(&server_cmux_dir).unwrap();
    fs::write(
        server_cmux_dir.join("mux.json"),
        r##"{"theme": {"sidebar_rail": "#445566"}}"##,
    )
    .unwrap();

    let socket = dir.join("mux.sock");
    let mut server = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_CONFIG_HOME", &server_cfg_root)
        .env_remove("CMUX_MUX_CONFIG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !socket.exists() {
        let _ = server.kill();
        let _ = server.wait();
        panic!(
            "headless server did not create socket at {}",
            socket.display()
        );
    }
    // `set_resolved_chrome` runs before `serve()` binds, so a live socket
    // implies the chrome is published; still give it a beat to settle.
    std::thread::sleep(Duration::from_millis(100));

    let output = Command::new(bin())
        .args(["--socket"])
        .arg(&socket)
        .args(["--json", "get-resolved-config"])
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();

    let _ = server.kill();
    let _ = server.wait();
    let _ = fs::remove_dir_all(&dir);

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "cmux get-resolved-config failed: {combined}"
    );

    let chrome: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("expected chrome JSON on stdout, parse failed:{e}\n{combined}"));
    assert_eq!(
        chrome["theme"]["sidebar_rail"].as_str(),
        Some("#445566"),
        "server theme colour missing from get-resolved-config output: {chrome}"
    );
}
