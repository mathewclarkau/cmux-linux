use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mux_core::platform::transport;

#[test]
fn agents_list_reports_only_claude_as_installed_after_claude_install() {
    let project = unique_temp_dir("agents-list-project");
    let home = unique_temp_dir("agents-list-home");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&home).unwrap();

    let install = Command::new(bin())
        .args(["claude", "install-hooks"])
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_success(&install);

    let listed = Command::new(bin())
        .args(["agents", "list"])
        .current_dir(&project)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_success(&listed);
    let output = String::from_utf8(listed.stdout).unwrap();
    let rows = output.lines().skip(1).collect::<Vec<_>>();
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().any(|row| row.starts_with("claude\tinstalled\tv0.2.0\t")));
    for agent in ["antigravity", "codex", "aider", "pi", "grok", "opencode"] {
        let row = rows.iter().find(|row| row.starts_with(&format!("{agent}\t"))).unwrap();
        assert!(row.contains("\tnot-installed\t-\t-\t"), "unexpected row: {row}");
    }

    fs::remove_dir_all(project).unwrap();
    fs::remove_dir_all(home).unwrap();
}

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
            // Dash, not bash: Falcon GenReverseShell targets a bare
            // `/bin/bash` on a PTY. CLI tests only need printf/echo.
            .env("SHELL", "/bin/sh")
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
            .env("SHELL", "/bin/sh")
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
    // Issue #71: `identify` carried the same stale CARGO_PKG_VERSION as
    // `-V` did; both now report the build-time version.
    assert_eq!(value.get("version").and_then(|v| v.as_str()), Some(mux_core::VERSION));

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

/// Issue #35: `--shell` is accepted for known values (and doesn't corrupt
/// a plain command through the real binary/server) and rejected
/// client-side with exit 2 for unknown ones.
#[test]
fn send_shell_flag_validates_and_accepts() {
    let server = HeadlessServer::start("send-shell");
    let workspace = cli(&server, &["new-workspace", "--name", "send-shell"]);
    assert_success(&workspace);
    let surface = String::from_utf8(workspace.stdout).unwrap().trim().parse::<u64>().unwrap();

    // Unknown shell values are a client-side usage error (exit 2), like
    // any other invalid flag value.
    let bad = cli(
        &server,
        &["send", "--surface", &surface.to_string(), "--text", "echo ok", "--shell", "tcsh"],
    );
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("--shell"));

    // Known values are accepted; non-metacharacter text passes through
    // unchanged (no buffer-reset newline needed).
    let ok = cli(
        &server,
        &[
            "send",
            "--surface",
            &surface.to_string(),
            "--text",
            "echo shell-flag-ok\n",
            "--shell",
            "fish",
        ],
    );
    assert_success(&ok);
    assert!(ok.stdout.is_empty(), "send should be quiet on success");
    let screen = wait_for_screen(&server, surface, "shell-flag-ok");
    assert!(screen.contains("shell-flag-ok"), "screen did not contain marker; got {screen:?}");
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
    let surface = String::from_utf8(workspace.stdout).unwrap().trim().parse::<u64>().unwrap();

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

/// Issue #78 AC1/AC3: a visible screen marker is detected with its
/// confidence and an evidence line naming the marker, both in --json and
/// in the human one-liner.
#[test]
fn detect_agent_reports_screen_marker_evidence() {
    let server = HeadlessServer::start("detect-agent");
    let workspace = cli(&server, &["new-workspace", "--name", "detect"]);
    assert_success(&workspace);
    let surface: u64 = String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();

    let send = cli(
        &server,
        &["send", "--surface", &surface.to_string(), "--text", "printf 'codex> '\n"],
    );
    assert_success(&send);
    let _ = wait_for_screen(&server, surface, "codex>");

    let out = cli(&server, &["--json", "detect-agent", "--surface", &surface.to_string()]);
    assert_success(&out);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["surface"].as_u64(), Some(surface));
    assert_eq!(value["agent"].as_str(), Some("codex"));
    assert_eq!(value["confidence"].as_str(), Some("medium"));
    let evidence = value["evidence"].as_str().expect("evidence line");
    assert!(evidence.contains("codex>"), "evidence was {evidence:?}");

    // Human output: `<surface> <agent> <confidence> <evidence>`.
    let plain = cli(&server, &["detect-agent", "--surface", &surface.to_string()]);
    assert_success(&plain);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert!(
        text.trim().starts_with(&format!("{surface} codex medium ")),
        "human line was {text:?}"
    );
}

/// Issue #78 AC1: a bare shell detects as `unknown`, and an unknown
/// surface id is a clean exit-1 error.
#[test]
fn detect_agent_unknown_for_bare_shell_and_errors_on_unknown_surface() {
    let server = HeadlessServer::start("detect-unknown");
    let workspace = cli(&server, &["new-workspace", "--name", "bare"]);
    assert_success(&workspace);
    let surface: u64 = String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();

    let out = cli(&server, &["--json", "detect-agent", "--surface", &surface.to_string()]);
    assert_success(&out);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["agent"].as_str(), Some("unknown"));
    assert_eq!(value["confidence"], serde_json::Value::Null);
    assert!(!value["evidence"].as_str().unwrap_or("").is_empty());

    let bad = cli(&server, &["detect-agent", "--surface", "999"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown surface 999"));
}

/// Issue #78 AC2: one call returns a `{surface: agent}` entry for every
/// live pane.
#[test]
fn detect_agents_batch_returns_map_for_every_pane() {
    let server = HeadlessServer::start("detect-batch");
    let one = cli(&server, &["new-workspace", "--name", "one"]);
    assert_success(&one);
    let s1: u64 = String::from_utf8(one.stdout).unwrap().trim().parse().unwrap();
    let two = cli(&server, &["new-workspace", "--name", "two"]);
    assert_success(&two);
    let s2: u64 = String::from_utf8(two.stdout).unwrap().trim().parse().unwrap();

    let out = cli(&server, &["--json", "detect-agents"]);
    assert_success(&out);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let agents = value["agents"].as_object().expect("agents map");
    assert_eq!(agents.len(), 2, "one entry per surface, got {agents:?}");
    assert_eq!(agents.get(&s1.to_string()).and_then(|v| v.as_str()), Some("unknown"));
    assert_eq!(agents.get(&s2.to_string()).and_then(|v| v.as_str()), Some("unknown"));

    // Human output: `<surface> <agent>` rows.
    let plain = cli(&server, &["detect-agents"]);
    assert_success(&plain);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert!(text.contains(&format!("{s1} unknown")), "rows were {text:?}");
    assert!(text.contains(&format!("{s2} unknown")), "rows were {text:?}");
}

/// Issue #78 AC4: `cmux agent-pattern add <name> --pattern <marker>`
/// extends the live registry (noun form), `list` shows it, detection
/// hits it, duplicates are rejected, and `remove` drops it.
#[test]
fn agent_pattern_add_round_trip_through_socket() {
    let server = HeadlessServer::start("agent-pattern");
    let workspace = cli(&server, &["new-workspace", "--name", "patterns"]);
    assert_success(&workspace);
    let surface: u64 = String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();

    let add = cli(&server, &["agent-pattern", "add", "myagent", "--pattern", "MYMARKER>"]);
    assert_success(&add);
    assert!(add.stdout.is_empty(), "agent-pattern add should be quiet on success");

    let list = cli(&server, &["agent-pattern", "list"]);
    assert_success(&list);
    let listed = String::from_utf8(list.stdout).unwrap();
    assert!(listed.contains("myagent"), "list output: {listed}");
    assert!(listed.contains("MYMARKER>"), "list output: {listed}");
    // Bundled patterns are listed too.
    assert!(listed.contains("claude"), "list output: {listed}");

    let send = cli(
        &server,
        &["send", "--surface", &surface.to_string(), "--text", "printf 'MYMARKER> '\n"],
    );
    assert_success(&send);
    let _ = wait_for_screen(&server, surface, "MYMARKER>");

    let out = cli(&server, &["--json", "detect-agent", "--surface", &surface.to_string()]);
    assert_success(&out);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["agent"].as_str(), Some("myagent"));

    // Duplicate adds are a server error (exit 1).
    let dup = cli(&server, &["agent-pattern", "add", "myagent", "--pattern", "MYMARKER>"]);
    assert_eq!(dup.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&dup.stderr).contains("already registered"));

    // Removing the pattern drops the detection back to unknown (the
    // marker is still on screen, but nothing matches it anymore).
    let remove = cli(&server, &["agent-pattern", "remove", "myagent"]);
    assert_success(&remove);
    let out = cli(&server, &["--json", "detect-agent", "--surface", &surface.to_string()]);
    assert_success(&out);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["agent"].as_str(), Some("unknown"));

    // Removing again errors: no user pattern by that name anymore.
    let gone = cli(&server, &["agent-pattern", "remove", "myagent"]);
    assert_eq!(gone.status.code(), Some(1));
}

/// Issue #78: `pane_json` carries the cached detection (`agent_name` /
/// `agent_confidence`) per tab after a detect call, so fleet dashboards
/// can read it from `list-workspaces` alone; a surface never detected
/// reports null.
#[test]
fn list_workspaces_json_exposes_agent_name_per_tab() {
    let server = HeadlessServer::start("agent-name-rpc");
    let workspace = cli(&server, &["new-workspace", "--name", "named"]);
    assert_success(&workspace);
    let surface: u64 = String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();
    let other = cli(&server, &["new-workspace", "--name", "other"]);
    assert_success(&other);
    let other_surface: u64 = String::from_utf8(other.stdout).unwrap().trim().parse().unwrap();

    let send = cli(
        &server,
        &["send", "--surface", &surface.to_string(), "--text", "printf 'codex> '\n"],
    );
    assert_success(&send);
    let _ = wait_for_screen(&server, surface, "codex>");

    let detect = cli(&server, &["detect-agent", "--surface", &surface.to_string()]);
    assert_success(&detect);

    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let tabs = value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .flat_map(|ws| ws["screens"].as_array().into_iter().flatten())
        .flat_map(|screen| screen["panes"].as_array().into_iter().flatten())
        .flat_map(|pane| pane["tabs"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    let tab = tabs
        .iter()
        .find(|tab| tab["surface"].as_u64() == Some(surface))
        .expect("detected surface in list-workspaces");
    assert_eq!(tab["agent_name"].as_str(), Some("codex"));
    assert_eq!(tab["agent_confidence"].as_str(), Some("medium"));
    // A surface that was never detected reports null, distinct from a
    // cached unknown.
    let other_tab = tabs
        .iter()
        .find(|tab| tab["surface"].as_u64() == Some(other_surface))
        .expect("other surface in list-workspaces");
    assert_eq!(other_tab["agent_name"], serde_json::Value::Null);
    assert_eq!(other_tab["agent_confidence"], serde_json::Value::Null);
}

/// Issue #78 AC7: `[[agent_detection]] enabled = false` in cmux.toml
/// turns detection off — both detect verbs error with a clear message
/// instead of detecting.
#[test]
fn detect_agent_respects_agent_detection_config_disabled() {
    let server = HeadlessServer::start_with_config(
        "detect-disabled",
        "[[agent_detection]]\nenabled = false\n",
    );
    let workspace = cli(&server, &["new-workspace", "--name", "off"]);
    assert_success(&workspace);
    let surface: u64 = String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();

    let out = cli(&server, &["detect-agent", "--surface", &surface.to_string()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("agent detection disabled"),
        "stderr was {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let batch = cli(&server, &["detect-agents"]);
    assert_eq!(batch.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&batch.stderr).contains("agent detection disabled"));
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

#[test]
fn grok_install_hooks_writes_native_schema() {
    let project = unique_temp_dir("grok-install-hooks-project");
    fs::create_dir_all(&project).unwrap();

    let install = Command::new(bin())
        .args(["grok", "install-hooks"])
        .current_dir(&project)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&install);

    let path = project.join(".grok").join("hooks").join("cmux-agent-state.json");
    assert!(path.is_file(), "expected hooks at {}", path.display());
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let command = value["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("command string");
    assert_eq!(value["hooks"]["PreToolUse"][0]["hooks"][0]["type"], "command");
    assert!(command.contains("--source hook"), "{command}");
    assert!(!command.contains("--source grok"), "{command}");
    assert!(
        !project.join(".grok").join("hooks.json").exists(),
        "must not write the legacy ~/.grok/hooks.json path"
    );

    let listed = Command::new(bin())
        .args(["agents", "list"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert_success(&listed);
    let output = String::from_utf8(listed.stdout).unwrap();
    let grok = output.lines().find(|row| row.starts_with("grok\t")).unwrap();
    assert!(grok.contains("\tinstalled\t"), "unexpected row: {grok}");

    fs::remove_dir_all(project).unwrap();
}

#[test]
fn grok_install_hooks_cleans_legacy_file() {
    let project = unique_temp_dir("grok-install-hooks-legacy");
    fs::create_dir_all(project.join(".grok")).unwrap();
    fs::write(
        project.join(".grok").join("hooks.json"),
        r#"{
  "hooks": [
    {
      "event": "PreToolUse",
      "command": "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source grok"
    }
  ]
}"#,
    )
    .unwrap();

    let install = Command::new(bin())
        .args(["grok", "install-hooks"])
        .current_dir(&project)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&install);
    assert!(
        !project.join(".grok").join("hooks.json").exists(),
        "legacy file that only held cmux hooks should be removed"
    );

    fs::remove_dir_all(project).unwrap();
}

#[test]
fn grok_install_hooks_refuses_symlinks() {
    let project = unique_temp_dir("grok-install-hooks-symlink");
    let hook_path = project.join(".grok").join("hooks").join("cmux-agent-state.json");
    fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
    let target = project.join("target.json");
    fs::write(&target, "{\"keep\":true}\n").unwrap();
    symlink(&target, &hook_path).unwrap();

    let output = Command::new(bin())
        .args(["grok", "install-hooks"])
        .current_dir(&project)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "grok install-hooks must refuse a symlink target, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.to_lowercase().contains("symlink"),
        "error output must mention \"symlink\", got: {combined:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"keep\":true}\n");

    fs::remove_dir_all(project).unwrap();
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
// --- Per-pane git worktrees (issue #77) ---

/// A temp git repo with one commit (git worktree add needs a HEAD).
fn git_repo_fixture(name: &str) -> PathBuf {
    let dir = unique_temp_dir(name);
    fs::create_dir_all(&dir).unwrap();
    let out = Command::new("git").arg("init").arg(&dir).output().unwrap();
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("git")
        .args(["-c", "user.email=cmux@test", "-c", "user.name=cmux"])
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// The pane id holding `surface`, from `list-workspaces` JSON.
fn pane_of_surface(server: &HeadlessServer, surface: u64) -> u64 {
    let list = cli(server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .flat_map(|ws| ws["screens"].as_array().into_iter().flatten())
        .flat_map(|screen| screen["panes"].as_array().into_iter().flatten())
        .find(|pane| {
            pane["tabs"].as_array().is_some_and(|tabs| {
                tabs.iter().any(|tab| tab["surface"].as_u64() == Some(surface))
            })
        })
        .expect("surface present in list-workspaces")
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .expect("pane id")
}

/// Every pane in `list-workspaces` JSON as (pane id, active tab cwd).
fn pane_cwds(server: &HeadlessServer) -> Vec<(u64, Option<String>)> {
    let list = cli(server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .flat_map(|ws| ws["screens"].as_array().into_iter().flatten())
        .flat_map(|screen| screen["panes"].as_array().into_iter().flatten())
        .map(|pane| {
            let id = pane.get("id").and_then(serde_json::Value::as_u64).expect("pane id");
            let cwd = pane
                .get("tabs")
                .and_then(serde_json::Value::as_array)
                .and_then(|tabs| tabs.first())
                .and_then(|tab| tab.get("cwd"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            (id, cwd)
        })
        .collect()
}

#[test]
fn pane_worktree_create_list_remove_round_trip() {
    let server = HeadlessServer::start("pane-worktree");
    let repo = git_repo_fixture("pane-worktree-repo");
    let workspace = cli(&server, &["new-workspace", "--name", "wt-test"]);
    assert_success(&workspace);
    let surface: u64 =
        String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();
    // Park the pane's active tab in the repo so create can resolve it.
    let parked =
        cli(&server, &["new-tab", "--cwd", repo.to_str().unwrap()]);
    assert_success(&parked);
    let pane = pane_of_surface(&server, surface);

    // Create (AC1): the worktree path comes back on JSON stdout.
    let created = cli(
        &server,
        &[
            "--json",
            "pane-worktree-create",
            "--pane",
            &pane.to_string(),
            "--branch",
            "feat-auth",
            "--label",
            "auth",
        ],
    );
    assert_success(&created);
    let value: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(value["pane"].as_u64(), Some(pane));
    assert_eq!(value["branch"].as_str(), Some("feat-auth"));
    let path = value["path"].as_str().expect("worktree path").to_string();
    assert!(PathBuf::from(&path).is_dir(), "worktree {path} should exist on disk");
    // git itself knows the worktree.
    let out = Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("feat-auth"),
        "git worktree list should show feat-auth"
    );

    // List (AC2): JSON shape + one-line-per-record plain output.
    let listed = cli(
        &server,
        &["--json", "pane-worktree-list", "--pane", &pane.to_string()],
    );
    assert_success(&listed);
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let worktrees = value["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0]["branch"].as_str(), Some("feat-auth"));
    assert_eq!(worktrees[0]["path"].as_str(), Some(path.as_str()));
    assert_eq!(worktrees[0]["label"].as_str(), Some("auth"));
    assert!(worktrees[0]["created_at_ms"].as_u64().unwrap() > 0);

    let plain = cli(&server, &["pane-worktree-list", "--pane", &pane.to_string()]);
    assert_success(&plain);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert!(
        text.contains("feat-auth") && text.contains(&path) && text.contains("auth"),
        "plain pane-worktree-list should name branch/path/label, got: {text}"
    );

    // Plain create output is just the path.
    let plain_create = cli(
        &server,
        &["pane-worktree-create", "--pane", &pane.to_string(), "--branch", "feat-plain"],
    );
    assert_success(&plain_create);
    let plain_path = String::from_utf8(plain_create.stdout).unwrap().trim().to_string();
    assert!(plain_path.ends_with(".feat-plain"), "plain create prints the path, got {plain_path}");

    // Remove (AC3): teardown drops dir + record.
    let removed = cli(
        &server,
        &["pane-worktree-remove", "--pane", &pane.to_string(), "--branch", "feat-auth"],
    );
    assert_success(&removed);
    assert!(!PathBuf::from(&path).exists(), "worktree dir should be gone after remove");
    let listed = cli(
        &server,
        &["--json", "pane-worktree-list", "--pane", &pane.to_string()],
    );
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let remaining: Vec<&str> = value["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["branch"].as_str())
        .collect();
    assert_eq!(remaining, vec!["feat-plain"], "only feat-plain should remain");

    // Removing an unknown branch errors with exit 1.
    let missing = cli(
        &server,
        &["pane-worktree-remove", "--pane", &pane.to_string(), "--branch", "nope"],
    );
    assert_eq!(missing.status.code(), Some(1));

    // Best-effort cleanup of the second worktree (inside repo's parent).
    let _ = fs::remove_dir_all(&plain_path);
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn pane_worktree_create_failure_returns_exit_1_and_cwd_unchanged() {
    let server = HeadlessServer::start("pane-worktree-fail");
    let repo = git_repo_fixture("pane-worktree-fail-repo");
    let workspace = cli(&server, &["new-workspace", "--name", "wt-fail"]);
    assert_success(&workspace);
    let surface: u64 =
        String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();
    let parked = cli(&server, &["new-tab", "--cwd", repo.to_str().unwrap()]);
    assert_success(&parked);
    let pane = pane_of_surface(&server, surface);

    let before = pane_cwds(&server);

    let failed = cli(
        &server,
        &[
            "--json",
            "pane-worktree-create",
            "--pane",
            &pane.to_string(),
            "--branch",
            "bad..name",
        ],
    );
    assert_eq!(
        failed.status.code(),
        Some(1),
        "git failure must surface as exit 1 (AC7), got {:?}\nstderr: {}",
        failed.status.code(),
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(String::from_utf8_lossy(&failed.stderr).contains("not a valid branch name"));
    assert!(failed.stdout.is_empty(), "no JSON on failure");

    // Pane cwd unchanged (AC7) and no record was kept.
    assert_eq!(pane_cwds(&server), before, "pane cwd must be unchanged after failure");
    let listed = cli(
        &server,
        &["--json", "pane-worktree-list", "--pane", &pane.to_string()],
    );
    assert_success(&listed);
    let value: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(value["worktrees"].as_array().unwrap().len(), 0);

    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn worktree_pattern_config_overrides_default() {
    // AC6: `[[worktree_pattern]]` in mux.toml redirects where worktrees
    // are created. (The issue text says `cmux.toml`; cmux reads
    // mux.toml/mux.json — see the scout plan's config-path correction.)
    let server = HeadlessServer::start_with_config(
        "pane-worktree-config",
        "[[worktree_pattern]]\npattern = \"../cmux-wt-<repo>-<branch>\"\n",
    );
    let repo = git_repo_fixture("pane-worktree-config-repo");
    let workspace = cli(&server, &["new-workspace", "--name", "wt-config"]);
    assert_success(&workspace);
    let surface: u64 =
        String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();
    let parked = cli(&server, &["new-tab", "--cwd", repo.to_str().unwrap()]);
    assert_success(&parked);
    let pane = pane_of_surface(&server, surface);

    let created = cli(
        &server,
        &[
            "--json",
            "pane-worktree-create",
            "--pane",
            &pane.to_string(),
            "--branch",
            "feat-pattern",
        ],
    );
    assert_success(&created);
    let value: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let path = value["path"].as_str().expect("worktree path").to_string();
    // ../cmux-wt-<repo>-<branch> resolved against the repo root; the
    // <repo> placeholder keeps the path unique per run (the repo fixture
    // dir is unique), so leftovers never collide across runs.
    assert!(
        path.ends_with(&format!(
            "cmux-wt-{}-feat-pattern",
            repo.file_name().unwrap().to_str().unwrap()
        )),
        "configured pattern should win over the default, got {path}"
    );
    assert!(PathBuf::from(&path).is_dir());

    let _ = fs::remove_dir_all(&path);
    let _ = fs::remove_dir_all(&repo);
}

#[test]
fn pane_worktree_three_word_alias_matches_flat_verb() {
    let server = HeadlessServer::start("pane-worktree-alias");
    let repo = git_repo_fixture("pane-worktree-alias-repo");
    let workspace = cli(&server, &["new-workspace", "--name", "wt-alias"]);
    assert_success(&workspace);
    let surface: u64 =
        String::from_utf8(workspace.stdout).unwrap().trim().parse().unwrap();
    let parked = cli(&server, &["new-tab", "--cwd", repo.to_str().unwrap()]);
    assert_success(&parked);
    let pane = pane_of_surface(&server, surface);

    // The issue's documented three-word form works verbatim (issue #77
    // AC1; see the naming-conflict note in the scout plan §2.8).
    let created = cli(
        &server,
        &[
            "--json",
            "pane",
            "worktree",
            "create",
            "--pane",
            &pane.to_string(),
            "--branch",
            "feat-alias",
        ],
    );
    assert_success(&created);
    let value: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let path = value["path"].as_str().expect("worktree path").to_string();
    assert!(PathBuf::from(&path).is_dir());

    // Both list forms return the same JSON.
    let via_alias = cli(
        &server,
        &["--json", "pane", "worktree", "list", "--pane", &pane.to_string()],
    );
    let via_flat = cli(
        &server,
        &["--json", "pane-worktree-list", "--pane", &pane.to_string()],
    );
    assert_success(&via_alias);
    assert_success(&via_flat);
    assert_eq!(via_alias.stdout, via_flat.stdout, "alias and flat verb must agree");

    // The three-word remove form tears down too.
    let removed = cli(
        &server,
        &["pane", "worktree", "remove", "--pane", &pane.to_string(), "--branch", "feat-alias"],
    );
    assert_success(&removed);
    assert!(!PathBuf::from(&path).exists());

    let _ = fs::remove_dir_all(&repo);
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

#[test]
fn grok_install_hooks_writes_native_schema() {
    let project = unique_temp_dir("grok-install-hooks-project");
    fs::create_dir_all(&project).unwrap();

    let install = Command::new(bin())
        .args(["grok", "install-hooks"])
        .current_dir(&project)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&install);

    let path = project.join(".grok").join("hooks").join("cmux-agent-state.json");
    assert!(path.is_file(), "expected hooks at {}", path.display());
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let command = value["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("command string");
    assert_eq!(value["hooks"]["PreToolUse"][0]["hooks"][0]["type"], "command");
    assert!(command.contains("--source hook"), "{command}");
    assert!(!command.contains("--source grok"), "{command}");
    assert!(
        !project.join(".grok").join("hooks.json").exists(),
        "must not write the legacy ~/.grok/hooks.json path"
    );

    let listed = Command::new(bin())
        .args(["agents", "list"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert_success(&listed);
    let output = String::from_utf8(listed.stdout).unwrap();
    let grok = output.lines().find(|row| row.starts_with("grok\t")).unwrap();
    assert!(grok.contains("\tinstalled\t"), "unexpected row: {grok}");

    fs::remove_dir_all(project).unwrap();
}

#[test]
fn grok_install_hooks_cleans_legacy_file() {
    let project = unique_temp_dir("grok-install-hooks-legacy");
    fs::create_dir_all(project.join(".grok")).unwrap();
    fs::write(
        project.join(".grok").join("hooks.json"),
        r#"{
  "hooks": [
    {
      "event": "PreToolUse",
      "command": "cmux report-agent --surface \"$CMUX_MUX_SURFACE\" --state working --source grok"
    }
  ]
}"#,
    )
    .unwrap();

    let install = Command::new(bin())
        .args(["grok", "install-hooks"])
        .current_dir(&project)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&install);
    assert!(
        !project.join(".grok").join("hooks.json").exists(),
        "legacy file that only held cmux hooks should be removed"
    );

    fs::remove_dir_all(project).unwrap();
}

#[test]
fn grok_install_hooks_refuses_symlinks() {
    let project = unique_temp_dir("grok-install-hooks-symlink");
    let hook_path = project.join(".grok").join("hooks").join("cmux-agent-state.json");
    fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
    let target = project.join("target.json");
    fs::write(&target, "{\"keep\":true}\n").unwrap();
    symlink(&target, &hook_path).unwrap();

    let output = Command::new(bin())
        .args(["grok", "install-hooks"])
        .current_dir(&project)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "grok install-hooks must refuse a symlink target, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.to_lowercase().contains("symlink"),
        "error output must mention \"symlink\", got: {combined:?}"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"keep\":true}\n");

    fs::remove_dir_all(project).unwrap();
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

/// `cmux attach --session-list --json` lists discovered sessions with a
/// `socket_path` per entry (issue #63, layer L1). Same shape as
/// `list-sessions --json` plus `socket_path`; exit 0. Modelled on
/// `list_sessions_lists_active_headless_session` (:904).
#[test]
fn attach_session_list_json_includes_socket_path() {
    let dir = unique_temp_dir("attach-sl");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("test-attach.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid_file = dir.join("test-attach.pid");
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
        .args(["attach", "--session-list", "--json", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    let entry = sessions
        .iter()
        .find(|s| s["session"] == "test-attach")
        .expect("sessions should contain test-attach");
    assert_eq!(entry["status"], "live", "test-attach should be live, got {entry}");
    let sp = entry["socket_path"].as_str().expect("socket_path field");
    assert!(
        sp.ends_with("test-attach.sock"),
        "socket_path should end with test-attach.sock, got {sp}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// Stale entries (dead pidfile, unconnectable socket) appear with
/// `status == "stale"` and a `socket_path`, alongside live entries.
#[test]
fn attach_session_list_json_marks_stale() {
    let dir = unique_temp_dir("attach-sl-stale");
    fs::create_dir_all(&dir).unwrap();
    let live_socket = dir.join("live-att.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&live_socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let live_pid = dir.join("live-att.pid");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if live_socket.exists() && live_pid.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(live_socket.exists());
    assert!(live_pid.exists());

    // Fake stale pair (nonexistent PID 999999, unconnectable socket).
    let stale_socket = dir.join("stale-att.sock");
    let stale_pid = dir.join("stale-att.pid");
    fs::write(&stale_socket, "fake").unwrap();
    fs::write(&stale_pid, "999999\n").unwrap();

    let output = Command::new(bin())
        .args(["attach", "--session-list", "--json", "--socket"])
        .arg(&live_socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    let stale = sessions
        .iter()
        .find(|s| s["session"] == "stale-att")
        .expect("sessions should contain stale-att");
    assert_eq!(stale["status"], "stale", "stale-att should be stale, got {stale}");
    let stale_sp = stale["socket_path"].as_str().expect("socket_path field");
    assert!(stale_sp.ends_with("stale-att.sock"));
    let live = sessions
        .iter()
        .find(|s| s["session"] == "live-att")
        .expect("sessions should contain live-att");
    assert_eq!(live["status"], "live");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// An empty runtime dir yields `{"sessions":[]}` and exit 0.
#[test]
fn attach_session_list_json_empty() {
    let dir = unique_temp_dir("attach-sl-empty");
    fs::create_dir_all(&dir).unwrap();
    // Point --socket at a path inside the empty dir; discovery scans the
    // parent (the dir) and finds no *.sock.
    let socket = dir.join("nothing.sock");
    let output = Command::new(bin())
        .args(["attach", "--session-list", "--json", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    assert!(sessions.is_empty(), "expected no sessions, got {sessions:?}");
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
    fs::write(server_cmux_dir.join("mux.json"), r##"{"theme": {"sidebar_rail": "#112233"}}"##)
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
        panic!("headless server did not create socket at {}", socket.display());
    }
    // Give the server a moment to register its resolved chrome (it is set
    // before `serve()` binds, so a live socket implies it is published).
    std::thread::sleep(Duration::from_millis(100));

    // Local laptop config dir: a local overlay that overrides a key
    // binding only, not theme, so the server theme must survive.
    let local_cfg_root = dir.join("local-config");
    let local_cmux_dir = local_cfg_root.join("cmux");
    fs::create_dir_all(&local_cmux_dir).unwrap();
    fs::write(local_cmux_dir.join("mux.local.toml"), "[keys]\nprefix = \"ctrl+s\"\n").unwrap();

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
    assert!(output.status.success(), "attach --print-resolved-config failed: {combined}");

    let merged: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
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
    fs::write(server_cmux_dir.join("mux.json"), r##"{"theme": {"sidebar_rail": "#445566"}}"##)
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
        panic!("headless server did not create socket at {}", socket.display());
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
    assert!(output.status.success(), "cmux get-resolved-config failed: {combined}");

    let chrome: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("expected chrome JSON on stdout, parse failed:{e}\n{combined}"));
    assert_eq!(
        chrome["theme"]["sidebar_rail"].as_str(),
        Some("#445566"),
        "server theme colour missing from get-resolved-config output: {chrome}"
    );
}

/// Issue #42 (scoped first PR): the `cmux plugin` verb group manages
/// `cmux-plugin.toml` manifests on disk only (no execution yet). We
/// install a fixture manifest against a HeadlessServer-style temp env
/// (an isolated XDG_DATA_HOME under the server's temp dir), then list
/// it, then uninstall and confirm `list` reports empty. The server
/// itself is idle for these verbs: the spec scopes this PR to manifest
/// state only, no socket traffic.
#[test]
fn plugin_install_list_uninstall_round_trip() {
    let server = HeadlessServer::start("plugin");
    let data_home = server.dir.join("xdg-data");
    fs::create_dir_all(&data_home).unwrap();
    let manifest_dir = server.dir.join("manifest");
    fs::create_dir_all(&manifest_dir).unwrap();
    let manifest_path = manifest_dir.join("cmux-plugin.toml");
    fs::write(
        &manifest_path,
        "[plugin]\nname = \"pifactory-fleet\"\nentry = \"bin/fleet.wasm\"\nverbs = [\"deploy\", \"rollback\"]\n",
    )
    .unwrap();

    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("CMUX_MUX_SOCKET")
            .output()
            .unwrap()
    };

    let manifest_str = manifest_path.to_str().unwrap().to_string();
    let install = run(&["plugin", "install", &manifest_str]);
    assert_success(&install);
    let install_out = String::from_utf8_lossy(&install.stdout);
    assert!(
        install_out.contains("pifactory-fleet"),
        "install should report the plugin name: {install_out}"
    );

    let list = run(&["plugin", "list"]);
    assert_success(&list);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("pifactory-fleet"),
        "list should name the installed plugin: {list_out}"
    );
    assert!(list_out.contains("enabled"), "list should show the enabled state: {list_out}");
    assert!(list_out.contains("deploy,rollback"), "list should show the claimed verbs: {list_out}");

    let uninstall = run(&["plugin", "uninstall", "pifactory-fleet"]);
    assert_success(&uninstall);
    assert!(
        String::from_utf8_lossy(&uninstall.stdout).contains("pifactory-fleet"),
        "uninstall should echo the removed plugin name"
    );

    let list2 = run(&["plugin", "list"]);
    assert_success(&list2);
    let list2_out = String::from_utf8_lossy(&list2.stdout);
    assert!(
        list2_out.contains("no plugins installed"),
        "after uninstall list should report empty: {list2_out}"
    );
}

/// Issue #42 AC6: the shipped example plugin at
/// `mux/spec/plugins/pifactory-fleet/` has a valid manifest that
/// installs and lists correctly via the `cmux plugin` verb group.
/// This guards against schema drift between the example manifest and
/// the loader's `ManifestFile` parser.
#[test]
fn plugin_shipped_example_manifest_installs() {
    let server = HeadlessServer::start("plugin-ac6");
    let data_home = server.dir.join("xdg-data");
    fs::create_dir_all(&data_home).unwrap();

    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spec/plugins/pifactory-fleet/cmux-plugin.toml")
        .canonicalize()
        .unwrap_or_else(|_| {
            panic!("shipped example manifest not found relative to {}", env!("CARGO_MANIFEST_DIR"))
        });
    assert!(
        manifest_path.exists(),
        "shipped pifactory-fleet manifest must exist at {}",
        manifest_path.display()
    );

    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("CMUX_MUX_SOCKET")
            .output()
            .unwrap()
    };

    let manifest_str = manifest_path.to_str().unwrap().to_string();
    let install = run(&["plugin", "install", &manifest_str]);
    assert_success(&install);

    let list = run(&["plugin", "list"]);
    assert_success(&list);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("pifactory-fleet"),
        "shipped example plugin should list after install: {list_out}"
    );
    // The installed entry should preserve the entry path and the
    // verb allowlist, both of which the loader stores verbatim.
    // Spot-check both so a regression in `cmd_install`'s copy step
    // is caught.
    assert!(
        list_out.contains("bin/fleet.wasm"),
        "installed entry path should be preserved: {list_out}"
    );
    assert!(list_out.contains("cmux_call"), "verb allowlist should include cmux_call: {list_out}");
}

/// Issue #59: `cmux --version` / `-V` print `cmux <version>` and exit 0.
///
/// Issue #71: the version is the build-time constant, not
/// `CARGO_PKG_VERSION`. Pinning this test to the manifest was why the
/// bug survived — the assertion and the bug read the same stale
/// `0.1.0`, so it passed while `-V` was wrong for seventeen releases.
/// The independent checks below are the part that would have caught it.
#[test]
fn version_flag_prints_build_version_and_exits_zero() {
    let expected = format!("cmux {}", mux_core::VERSION);

    let run = |args: &[&str]| {
        Command::new(bin()).args(args).env_remove("CMUX_MUX_SOCKET").output().unwrap()
    };

    for args in [&["--version"][..], &["-V"][..], &["--headless", "--version"][..]] {
        let out = run(args);
        assert_success(&out);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            expected,
            "args {args:?} should print `{expected}`"
        );
        assert!(
            out.stderr.is_empty(),
            "args {args:?} should write nothing to stderr; got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Issue #71: the reported version must be a real release version, not
/// the `0.1.0` manifest placeholder the binary shipped with for
/// seventeen tagged releases. Deliberately asserts against the literal
/// rather than any constant: a check derived from the same source as
/// the value under test cannot catch this class of bug.
#[test]
fn version_is_not_the_stale_placeholder() {
    let version = mux_core::VERSION;
    assert_ne!(version, "0.1.0", "0.1.0 is the pre-#71 placeholder, not a released version");
    assert_ne!(version, "unknown", "build.rs could not resolve any version");

    // Shape is `<major>.<minor>.<patch>` for a release build; a dev
    // build appends `-<n>-g<sha>` off a tag, or just `-g<sha>` when no
    // tag is reachable (a depth-1 CI checkout), plus `-dirty` for local
    // modifications. Only the triple is pinned — every tier of build.rs
    // keeps it, and the suffix is what makes a dev build recognisable.
    let triple = version.split('-').next().unwrap_or_default();
    let parts: Vec<&str> = triple.split('.').collect();
    assert_eq!(parts.len(), 3, "version {version:?} should start with a semver triple");
    for part in parts {
        assert!(
            !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()),
            "version {version:?} has a non-numeric component {part:?}"
        );
    }
}

// =====================================================================
// cmux rename-session (issue #63 L2) — full TDD acceptance suite.
//
// These tests are written RED (cookbook Rule 5) before the feature is
// implemented. At this commit the `rename-session` verb does not exist
// yet, so every end-to-end test fails because the invocation is rejected
// (unknown verb) rather than performing the rename; the helper/unit tests
// fail against compile-scaffolding stubs. The implementation commits turn
// them green. The manual-spawn + `XDG_RUNTIME_DIR` harness mirrors
// `list_sessions_lists_active_headless_session` / `kill_session_*`.
// =====================================================================

/// Spawn a headless daemon as `--session <name>` on `<dir>/<name>.sock` in
/// an isolated `XDG_RUNTIME_DIR`, wait for `.sock`+`.pid`, return the child.
fn spawn_named_headless(dir: &std::path::Path, name: &str) -> Child {
    let socket = dir.join(format!("{name}.sock"));
    let mut child = Command::new(bin())
        .args(["--headless", "--session"])
        .arg(name)
        .args(["--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid_file = dir.join(format!("{name}.pid"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() && pid_file.exists() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("daemon {name:?} did not come up at {}", socket.display());
}

/// Read the daemon pid recorded in `<dir>/<name>.pid`.
fn read_pid_file(path: &std::path::Path) -> u32 {
    fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("pid file {} should exist", path.display()))
        .trim()
        .parse::<u32>()
        .unwrap()
}

/// Run a cmux CLI subcommand against `--socket <socket>` with CMUX_MUX_SOCKET
/// unset (so resolution is deterministic) and return its output.
fn run_against(socket: &std::path::Path, xdg: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(["--socket"]).arg(socket).args(args);
    cmd.env("XDG_RUNTIME_DIR", xdg).env_remove("CMUX_MUX_SOCKET");
    cmd.output().unwrap()
}

/// Poll `read-screen` until `needle` appears on the surface (or timeout).
fn wait_for_screen_at(socket: &std::path::Path, surface: u64, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        let out = run_against(
            socket,
            std::path::Path::new("/tmp"),
            &["read-screen", "--surface", &surface.to_string()],
        );
        last = String::from_utf8_lossy(&out.stdout).to_string();
        if last.contains(needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// AC1: rename moves `.sock`+`.pid` to the new name and the SAME daemon
/// keeps serving at the new path (pid unchanged).
#[test]
fn rename_session_moves_socket_and_pid_and_keeps_serving() {
    let dir = unique_temp_dir("rename-t1");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");
    let old_pid = dir.join("old.pid");
    let daemon_pid = read_pid_file(&old_pid);

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let new_sock = dir.join("bar.sock");
    let new_pid = dir.join("bar.pid");
    assert!(new_sock.exists(), "new socket should exist after rename");
    assert!(new_pid.exists(), "new pid file should exist after rename");
    assert!(!old_sock.exists(), "old socket should be gone after rename");
    assert!(!old_pid.exists(), "old pid file should be gone after rename");

    // Same daemon keeps serving at the new path.
    let identify = run_against(&new_sock, &dir, &["--json", "identify"]);
    assert_success(&identify);
    let v: serde_json::Value = serde_json::from_slice(&identify.stdout).unwrap();
    assert_eq!(v["session"].as_str(), Some("bar"), "identify should report the new name");
    assert_eq!(
        v["pid"].as_u64(),
        Some(daemon_pid as u64),
        "same daemon pid should serve after rename"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC2: after rename, the old socket is no longer connectable (exit 3)
/// while the new path serves the same daemon; protocol version unchanged.
#[test]
fn rename_makes_old_socket_unreachable_and_keeps_protocol() {
    let dir = unique_temp_dir("rename-t2");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");
    let daemon_pid = read_pid_file(&dir.join("old.pid"));

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    // New path serves the same daemon; protocol must NOT bump (scout Q2).
    let new_sock = dir.join("bar.sock");
    let id_new = run_against(&new_sock, &dir, &["--json", "identify"]);
    assert_success(&id_new);
    let v: serde_json::Value = serde_json::from_slice(&id_new.stdout).unwrap();
    assert_eq!(v["session"].as_str(), Some("bar"));
    assert_eq!(v["pid"].as_u64(), Some(daemon_pid as u64));
    assert_eq!(v["protocol"].as_u64(), Some(6), "rename must not bump the protocol version");

    // Old path is gone -> connect fails with exit 3 (transport convention).
    let id_old = run_against(&old_sock, &dir, &["identify"]);
    assert_eq!(id_old.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&id_old.stderr).contains("cannot connect"));

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC3: after rename, the old session name is gone from discovery and the
/// new name is listed as live.
#[test]
fn old_session_name_gone_after_rename() {
    let dir = unique_temp_dir("rename-t3");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let list = run_against(&old_sock, &dir, &["--json", "list-sessions"]);
    // old.sock is gone, so list-sessions resolves its runtime dir from
    // XDG_RUNTIME_DIR and discovers bar.sock live, old.sock absent.
    // (list-sessions does not need a connectable --socket.)
    let v: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap_or_else(|_| {
        let s = String::from_utf8_lossy(&list.stdout);
        panic!(
            "list-sessions produced non-JSON output: {s}\nstderr: {}",
            String::from_utf8_lossy(&list.stderr)
        )
    });
    let sessions = v["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["session"] == "bar" && s["status"] == "live"),
        "bar should be listed live after rename, got {v}"
    );
    assert!(
        !sessions.iter().any(|s| s["session"] == "old"),
        "old should be absent after rename, got {v}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC4: existing panes keep the `CMUX_MUX_SOCKET` they inherited at spawn
/// (the old path) for their lifetime; panes spawned AFTER the rename
/// inherit the new path. This is the lifetime guarantee (intentional, not
/// a bug) documented in USAGE and the server.rs docstring.
#[test]
fn rename_preserves_inherited_cmux_socket_in_existing_panes() {
    let dir = unique_temp_dir("rename-t4");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    // Existing pane (spawned before the rename).
    let ws = run_against(&old_sock, &dir, &["new-workspace", "--name", "pre"]);
    assert_success(&ws);
    let surface_pre: u64 = String::from_utf8(ws.stdout).unwrap().trim().parse().unwrap();
    let old_sock_str = old_sock.display().to_string();
    // Fish is the default surface shell; the trailing real `\n` submits
    // the line (no --send-cr needed — matches the cli_verbs marker probe).
    let probe = "printf 'E=%s\\n' \"$CMUX_MUX_SOCKET\"\n";
    let send = run_against(
        &old_sock,
        &dir,
        &["send", "--surface", &surface_pre.to_string(), "--text", probe],
    );
    assert_success(&send);
    // Poll for the actual path VALUE (only present after the shell expands
    // the var), not a substring of the typed command.
    let before = wait_for_screen_at(&old_sock, surface_pre, &old_sock_str);
    assert!(
        before.contains(&old_sock_str),
        "pre-rename pane should carry the old socket path; screen was {before:?}"
    );

    // Rename old -> bar.
    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);
    let new_sock = dir.join("bar.sock");
    let new_sock_str = new_sock.display().to_string();

    // Existing pane: env is unchanged for its lifetime (AC4 first half).
    // Re-probe and clear the screen's prior line by checking the LAST `E=`
    // value: it must still be the old path, never the new one.
    let send2 = run_against(
        &new_sock,
        &dir,
        &["send", "--surface", &surface_pre.to_string(), "--text", probe],
    );
    assert_success(&send2);
    // Existing pane keeps the OLD value (env inherited at spawn, unchanged).
    let after = wait_for_screen_at(&new_sock, surface_pre, &old_sock_str);
    assert!(
        after.contains(&old_sock_str) && !after.contains(&new_sock_str),
        "existing pane must keep the old CMUX_MUX_SOCKET after rename; \
         screen was {after:?}"
    );

    // New pane spawned after the rename inherits the refreshed path.
    let ws2 = run_against(&new_sock, &dir, &["new-workspace", "--name", "post"]);
    assert_success(&ws2);
    let surface_post: u64 = String::from_utf8(ws2.stdout).unwrap().trim().parse().unwrap();
    let send3 = run_against(
        &new_sock,
        &dir,
        &["send", "--surface", &surface_post.to_string(), "--text", probe],
    );
    assert_success(&send3);
    // New pane inherits the refreshed (new) path.
    let post = wait_for_screen_at(&new_sock, surface_post, &new_sock_str);
    assert!(
        post.contains(&new_sock_str),
        "post-rename pane should carry the new socket path; screen was {post:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC5: renaming onto a LIVE target session fails with exit 2
/// ("already exists") and leaves the source untouched.
#[test]
fn rename_to_existing_live_session_fails_exit_2() {
    let dir = unique_temp_dir("rename-t5");
    fs::create_dir_all(&dir).unwrap();
    let mut child_old = spawn_named_headless(&dir, "old");
    let mut child_bar = spawn_named_headless(&dir, "bar");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_eq!(
        rename.status.code(),
        Some(2),
        "rename onto a live session must exit 2; stderr: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rename.stderr).to_lowercase().contains("already exists"),
        "stderr should explain the target is in use; got {}",
        String::from_utf8_lossy(&rename.stderr)
    );

    // Source must be untouched (nothing moved).
    assert!(old_sock.exists(), "old socket must survive a refused rename");
    assert!(
        mux_core::server::is_session_socket_live(&old_sock),
        "old session must still be live after a refused rename"
    );

    let _ = child_old.kill();
    let _ = child_old.wait();
    let _ = child_bar.kill();
    let _ = child_bar.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// Target policy (mirrors `serve()`'s stale-clear): a STALE target
/// (dead pid) is cleared and the rename succeeds.
#[test]
fn rename_to_stale_target_clears_and_succeeds() {
    let dir = unique_temp_dir("rename-t6");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    // Synthetic stale target pair (dead pid).
    let stale_sock = dir.join("bar.sock");
    let stale_pid = dir.join("bar.pid");
    fs::write(&stale_sock, b"").unwrap();
    fs::write(&stale_pid, "999999\n").unwrap();

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    // bar.sock is now live under the daemon's pid; old.sock is gone.
    assert!(mux_core::server::is_session_socket_live(&stale_sock));
    assert_eq!(
        read_pid_file(&stale_pid),
        child.id(),
        "bar.pid should now record the (live) daemon pid"
    );
    assert!(!old_sock.exists(), "old socket should be gone after rename");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC6 (defence in depth): invalid `--new` names are rejected CLIENT-side
/// (exit 2, "session name") and never reach the socket. A literal NUL
/// cannot be carried by execve, so it is covered by the unit test T12
/// (`validate_session_name_table`) rather than here.
#[test]
fn rename_rejects_invalid_names() {
    let dir = unique_temp_dir("rename-t7");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let overlong = "a".repeat(256);
    let bad_names: &[&str] = &["", "a/b", "a\\b", "..", ".", " foo", "foo ", "\t", &overlong];
    for bad in bad_names {
        let out = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", bad]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "invalid name {bad:?} should exit 2; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        // Regression guard: bad names must be rejected by validate_session_name
        // (exit 2, "session name") and never fall through to the generic
        // unknown-verb/argument path. Require the validation message AND the
        // absence of the unknown-verb fallback so that a future regression —
        // where a bad name slips past validation and surfaces as an
        // unknown-verb rejection — is caught.
        assert!(
            !err.contains("unknown argument")
                && !err.contains("unknown verb")
                && !err.contains("unexpected argument"),
            "invalid name {bad:?} must not hit the unknown-verb path; got {err:?}"
        );
        assert!(
            err.contains("session name"),
            "invalid name {bad:?} should explain the session-name rejection; got {err:?}"
        );
        // Source must be untouched by every rejected attempt.
        assert!(old_sock.exists(), "old socket must survive a rejected rename ({bad:?})");
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC2/T8: after rename, `identify` reports the new session name and the
/// protocol version is still 6 (rename is an additive command variant).
#[test]
fn identify_reports_new_name_after_rename() {
    let dir = unique_temp_dir("rename-t8");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let id = run_against(&dir.join("bar.sock"), &dir, &["--json", "identify"]);
    assert_success(&id);
    let v: serde_json::Value = serde_json::from_slice(&id.stdout).unwrap();
    assert_eq!(v["session"].as_str(), Some("bar"));
    assert_eq!(v["protocol"].as_u64(), Some(6));

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC1/T9: the pid FILE contents are unchanged across the rename (only the
/// filename moves) and the daemon process is alive throughout.
#[test]
fn pid_file_contents_unchanged_after_rename() {
    let dir = unique_temp_dir("rename-t9");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_pid = dir.join("old.pid");
    let pid_before = read_pid_file(&old_pid);
    assert!(mux_core::server::is_process_alive(pid_before));

    let rename = run_against(
        &dir.join("old.sock"),
        &dir,
        &["rename-session", "--old", "old", "--new", "bar"],
    );
    assert_success(&rename);

    let pid_after = read_pid_file(&dir.join("bar.pid"));
    assert_eq!(pid_after, pid_before, "pid file contents must be identical after rename");
    assert!(mux_core::server::is_process_alive(pid_after), "daemon must stay alive across rename");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC7/T10: after rename, the session-list / kill-session / kill-stale verbs
/// work against the renamed session with no regressions.
#[test]
fn rename_no_regressions_on_list_kill_killstale() {
    let dir = unique_temp_dir("rename-t10");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let new_sock = dir.join("bar.sock");
    // list-sessions sees bar live, old absent.
    let list = run_against(&new_sock, &dir, &["--json", "list-sessions"]);
    assert_success(&list);
    let v: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let sessions = v["sessions"].as_array().expect("sessions array");
    assert!(sessions.iter().any(|s| s["session"] == "bar" && s["status"] == "live"));
    assert!(!sessions.iter().any(|s| s["session"] == "old"));

    // kill-session on the renamed name terminates the daemon & cleans up.
    let kill = run_against(&new_sock, &dir, &["kill-session", "--session", "bar"]);
    assert_success(&kill);
    let _ = child.wait();
    assert!(!new_sock.exists(), "bar.sock should be removed by kill-session");
    assert!(!dir.join("bar.pid").exists(), "bar.pid should be removed by kill-session");

    // kill-stale is a clean no-op now.
    let stale = run_against(&new_sock, &dir, &["kill-stale"]);
    assert_success(&stale);

    let _ = fs::remove_dir_all(&dir);
}

// T11 (`rename_session_at_renames_via_socket`) lives in `cli.rs`'s own
// `#[cfg(test)]` module: mux-tui is a bin-only crate, so this integration
// test file links only against the `mux-core` lib + the `cmux` binary and
// cannot import the `pub(crate)` helper. The in-process unit test there
// drives a `mux-core` server directly (no subprocess) and exercises the
// exact code path the picker's `r` flow uses.

/// T10 (issue #63 L3, scout plan): the session-manager overlay previews an
/// *other* session's workspaces with a one-shot `list-workspaces` RPC over
/// that session's control socket (the same connect→write→read path
/// `cli::one_shot_rpc` shares with `rename_rpc`, and that `cmux
/// list-workspaces` rides). `fetch_workspaces` is `pub(crate)` so this
/// bin-test cannot call it directly; instead it drives the identical wire
/// path against two named headless daemons and asserts each returns a
/// parseable workspaces tree with the expected count. If the wire verb or
/// its JSON shape regressed, the overlay's right column would break too.
#[test]
fn overlay_fetch_workspaces_parses_remote_tree() {
    let dir = unique_temp_dir("smgr-fetch");
    fs::create_dir_all(&dir).unwrap();
    let mut child_a = spawn_named_headless(&dir, "alpha");
    let mut child_b = spawn_named_headless(&dir, "beta");
    let sock_a = dir.join("alpha.sock");
    let sock_b = dir.join("beta.sock");

    // Give each daemon a distinct workspace set.
    assert_success(&run_against(&sock_a, &dir, &["new-workspace", "--name", "a-one"]));
    assert_success(&run_against(&sock_a, &dir, &["new-workspace", "--name", "a-two"]));
    assert_success(&run_against(&sock_b, &dir, &["new-workspace", "--name", "b-one"]));

    // Querying B's socket returns B's tree (not A's), proving the overlay
    // can read another session's workspaces over its own socket.
    let list_b = run_against(&sock_b, &dir, &["--json", "list-workspaces"]);
    assert_success(&list_b);
    let value: serde_json::Value = serde_json::from_slice(&list_b.stdout).unwrap();
    let names: Vec<&str> = value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .map(|ws| ws["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        names.iter().any(|n| *n == "b-one"),
        "beta socket should report its own workspace b-one, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| *n == "a-two"),
        "beta socket must NOT leak alpha's workspaces, got {names:?}"
    );

    // And querying A's socket returns A's tree with both of its workspaces.
    let list_a = run_against(&sock_a, &dir, &["--json", "list-workspaces"]);
    assert_success(&list_a);
    let value_a: serde_json::Value = serde_json::from_slice(&list_a.stdout).unwrap();
    let count_a = value_a["workspaces"].as_array().map(|a| a.len()).unwrap_or(0);
    assert!(
        count_a >= 2,
        "alpha socket should report >=2 workspaces, got {count_a}"
    );

    let _ = child_a.kill();
    let _ = child_a.wait();
    let _ = child_b.kill();
    let _ = child_b.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T11 (issue #63 L3, scout plan): focusing a workspace in an *other* session
/// from the overlay sends a one-shot `select-workspace` RPC over that
/// session's socket (the path `cli::select_workspace_remote` rides). Spawns
/// two named daemons, adds workspaces to beta, issues `select-workspace
/// --index 1` against beta's socket, and asserts beta's `list-workspaces`
/// afterwards reports workspace index 1 active. This proves the overlay's
/// right-column Enter on another session remotely moves that session's focus.
#[test]
fn overlay_select_workspace_focuses_remotely() {
    let dir = unique_temp_dir("smgr-select");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "beta");
    let sock = dir.join("beta.sock");

    // Three workspaces; the first is active by default.
    assert_success(&run_against(&sock, &dir, &["new-workspace", "--name", "one"]));
    assert_success(&run_against(&sock, &dir, &["new-workspace", "--name", "two"]));
    assert_success(&run_against(&sock, &dir, &["new-workspace", "--name", "three"]));

    // Remotely focus workspace index 1 ("two") the way the overlay does.
    let select = run_against(&sock, &dir, &["select-workspace", "--index", "1"]);
    assert_success(&select);

    // beta's tree now reports index 1 active.
    let list = run_against(&sock, &dir, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let active = value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|ws| ws["active"].as_bool() == Some(true))
        .expect("an active workspace");
    assert_eq!(
        active["name"].as_str(),
        Some("two"),
        "select-workspace --index 1 should make 'two' active"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T12 (issue #63 L3, scout plan): an `[unreachable]` socket row is reported
/// by discovery and `kill-session` cleans it without crashing. The overlay
/// drives `cli::kill_session_at` on such rows; this guards that a stale
/// `.sock`/`.pid` pair (no live process) is listed as stale and removable,
/// matching AC8 (unreachable rows must be killable and must not crash).
#[test]
fn overlay_kill_unreachable_does_not_crash_discovery() {
    let dir = unique_temp_dir("smgr-unreachable");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "live");
    let live_sock = dir.join("live.sock");

    // Create a STALE socket/pid pair with no live process behind it.
    let stale_sock = dir.join("ghost.sock");
    let stale_pid = dir.join("ghost.pid");
    std::os::unix::net::UnixListener::bind(&stale_sock)
        .expect("bind stale socket");
    fs::write(&stale_pid, "999999").unwrap(); // a pid that is not alive

    // list-sessions --json reports both, ghost as stale.
    let list = run_against(&live_sock, &dir, &["--json", "list-sessions"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let by_name: std::collections::HashMap<&str, &str> = value["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| (s["name"].as_str().unwrap_or(""), s["status"].as_str().unwrap_or("")))
        .collect();
    assert_eq!(by_name.get("live").copied(), Some("live"));
    assert_eq!(
        by_name.get("ghost").copied(),
        Some("stale"),
        "ghost should be reported stale/unreachable"
    );

    // kill-session on the stale row cleans it (no crash).
    let kill = run_against(&stale_sock, &dir, &["kill-session", "--session", "ghost"]);
    assert_success(&kill);
    assert!(!stale_sock.exists(), "stale socket removed by kill-session");
    assert!(!stale_pid.exists(), "stale pid removed by kill-session");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T13 (issue #63 L3, scout plan): the overlay's rename path and the L2
/// `rename-session` CLI verb agree — both end with the session serving at the
/// new socket and gone from the old. This is the same wire path
/// (`cli::rename_session_at` over `one_shot_rpc`); the L2 suite covers the
/// helper directly, so here we just confirm a rename issued via the verb is
/// observable through `list-sessions` the way the overlay's `r` flow expects.
#[test]
fn overlay_rename_reuses_l2_helper() {
    let dir = unique_temp_dir("smgr-rename");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "pre");
    let pre_sock = dir.join("pre.sock");

    let rename = run_against(&pre_sock, &dir, &["rename-session", "--old", "pre", "--new", "post"]);
    assert_success(&rename);
    let post_sock = dir.join("post.sock");
    assert!(post_sock.exists(), "new socket exists after rename");
    assert!(!pre_sock.exists(), "old socket gone after rename");

    // list-sessions now reports 'post' live and 'pre' absent — the shape the
    // overlay's rebuilt left column will show after a rename.
    let list = run_against(&post_sock, &dir, &["--json", "list-sessions"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let names: Vec<&str> = value["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.iter().any(|n| *n == "post"), "post should be listed: {names:?}");
    assert!(!names.iter().any(|n| *n == "pre"), "pre should be gone: {names:?}");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T10 (issue #69, scout plan §3c): REGRESSION -- a genuine first attach to a
/// dead socket must STILL exit non-zero (exit 1), both before and after the
/// swap-recovery fix. The recovery path only fires when there is a
/// last-known-good socket to fall back to (a swap); a fresh `cmux attach`
/// has no origin, so the connect error propagates to `main()` exactly as
/// today. This test pins that behavior so the fix cannot accidentally make a
/// real first-attach silently loop instead of failing.
#[test]
fn first_attach_to_dead_socket_still_exits_nonzero() {
    let dir = unique_temp_dir("attach-dead-first");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("dead-first.sock");
    // Stale file: not a listening socket, so RemoteSession::connect fails
    // (same technique as serve_recovers_from_stale_socket / attach_session_list_json_marks_stale).
    fs::write(&socket, b"").unwrap();

    let out = Command::new(bin())
        .args(["attach", "--socket"])
        .arg(&socket)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .expect("failed to spawn cmux attach");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "first-attach to a dead socket must still exit 1, got {:?}\nstderr:\n{}",
        out.status.code(),
        stderr,
    );
    assert!(
        stderr.contains("attaching to cmux session socket"),
        "stderr should carry the connect-failure context, got: {stderr:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}


// -- issue #76: layout export/apply --------------------------------------

/// Helper: the parsed `--json list-workspaces` payload.
fn list_workspaces_json(server: &HeadlessServer) -> serde_json::Value {
    let listed = cli(server, &["--json", "list-workspaces"]);
    assert_success(&listed);
    serde_json::from_slice(&listed.stdout).unwrap()
}

#[test]
fn layout_export_writes_versioned_workspace_json() {
    let server = HeadlessServer::start("layout-export");
    let ws = cli(&server, &["new-workspace", "--name", "fleet"]);
    assert_success(&ws);

    let value = list_workspaces_json(&server);
    let pane = value["workspaces"][0]["screens"][0]["panes"][0]["id"].as_u64().unwrap();
    let split = cli(&server, &["split", "--pane", &pane.to_string(), "--dir", "right"]);
    assert_success(&split);
    let ratio = cli(&server, &[
        "set-ratio",
        "--pane",
        &pane.to_string(),
        "--dir",
        "right",
        "--ratio",
        "0.6",
    ]);
    assert_success(&ratio);

    let out = server.dir.join("fleet.json");
    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "fleet",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_success(&export);
    assert_eq!(
        String::from_utf8_lossy(&export.stdout).trim(),
        out.display().to_string(),
        "plain mode prints the written path"
    );

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["schema_version"].as_u64(), Some(1), "doc was {doc}");
    assert_eq!(doc["cmux_version"].as_str(), Some(mux_core::VERSION));
    assert_eq!(doc["workspace"]["name"].as_str(), Some("fleet"));
    let layout = &doc["workspace"]["screens"][0]["layout"];
    assert_eq!(layout["type"].as_str(), Some("split"));
    assert_eq!(layout["dir"].as_str(), Some("right"));
    assert!((layout["ratio"].as_f64().unwrap() - 0.6).abs() < 1e-5);
    assert_eq!(layout["a"]["type"].as_str(), Some("leaf"));
    assert_eq!(layout["a"]["pane"].as_u64(), Some(0));
    assert_eq!(
        doc["workspace"]["screens"][0]["panes"].as_array().unwrap().len(),
        2,
        "both split panes should be recorded"
    );
}

#[test]
fn layout_apply_round_trips_topology_and_argv() {
    let server = HeadlessServer::start("layout-apply");
    let ws = cli(&server, &["new-workspace", "--name", "fleet"]);
    assert_success(&ws);

    let value = list_workspaces_json(&server);
    let pane = value["workspaces"][0]["screens"][0]["panes"][0]["id"].as_u64().unwrap();
    let ws_id = value["workspaces"][0]["id"].as_u64().unwrap();

    // An agent tab with explicit argv + env (the `--exec` spawn path).
    let marker = format!("FLEETMARKER_{}", std::process::id());
    let exec = cli(
        &server,
        &[
            "new-tab",
            "--pane",
            &pane.to_string(),
            "--env",
            "FLEET_TIER=A",
            "--exec",
            "--",
            "/bin/sh",
            "-c",
            &format!("printf '{marker}'; sleep 60"),
        ],
    );
    assert_success(&exec);
    let split = cli(&server, &["split", "--pane", &pane.to_string(), "--dir", "down"]);
    assert_success(&split);

    let out = server.dir.join("fleet.json");
    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "fleet",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_success(&export);
    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let tabs = &doc["workspace"]["screens"][0]["panes"][0]["tabs"];
    assert_eq!(tabs.as_array().unwrap().len(), 2);
    assert_eq!(
        tabs[1]["command"].as_array().map(|a| a.len()),
        Some(3),
        "recorded argv should round-trip into the file: {tabs}"
    );
    assert_eq!(tabs[1]["env"]["FLEET_TIER"].as_str(), Some("A"));

    let close = cli(&server, &["close-workspace", "--workspace", &ws_id.to_string()]);
    assert_success(&close);

    let apply = cli(&server, &[
        "layout-apply",
        "--input",
        out.to_str().unwrap(),
        "--workspace",
        "fleet",
    ]);
    assert_success(&apply);

    // Topology is back: one workspace, two panes, the exec tab re-spawned.
    let value = list_workspaces_json(&server);
    let workspaces = value["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0]["name"].as_str(), Some("fleet"));
    let layout = &workspaces[0]["screens"][0]["layout"];
    assert_eq!(layout["type"].as_str(), Some("split"), "split geometry restored");
    assert_eq!(layout["dir"].as_str(), Some("down"));
    let panes = workspaces[0]["screens"][0]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0]["tabs"].as_array().unwrap().len(), 2);

    // The re-spawned argv actually ran: poll every surface for the marker.
    let surfaces: Vec<u64> = panes
        .iter()
        .flat_map(|p| p["tabs"].as_array().unwrap().iter())
        .map(|t| t["surface"].as_u64().unwrap())
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw = false;
    while Instant::now() < deadline && !saw {
        for &sid in &surfaces {
            let read = cli(&server, &["read-screen", "--surface", &sid.to_string()]);
            if read.status.success()
                && String::from_utf8_lossy(&read.stdout).contains(&marker)
            {
                saw = true;
                break;
            }
        }
        if !saw {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(saw, "apply should re-spawn the recorded argv and print {marker}");
}

#[test]
fn layout_export_all_writes_one_file_per_workspace() {
    let server = HeadlessServer::start("layout-export-all");
    for name in ["alpha", "beta"] {
        let ws = cli(&server, &["new-workspace", "--name", name]);
        assert_success(&ws);
    }

    let dir = server.dir.join("fleet");
    let export = cli(&server, &["layout-export-all", "--output-dir", dir.to_str().unwrap()]);
    assert_success(&export);
    for name in ["alpha", "beta"] {
        let path = dir.join(format!("{name}.json"));
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["schema_version"].as_u64(), Some(1), "{name} doc: {doc}");
        assert_eq!(doc["workspace"]["name"].as_str(), Some(name));
    }
}

#[test]
fn layout_apply_rejects_unknown_schema_version_loudly() {
    let server = HeadlessServer::start("layout-apply-v2");
    let bad = server.dir.join("v2.json");
    fs::write(
        &bad,
        r#"{"schema_version":2,"cmux_version":"x","workspace":{"name":"w","active_screen":0,"screens":[{"active_pane":0,"layout":{"type":"leaf","pane":0},"panes":[{"tabs":[{"kind":"pty"}]}]}]}}"#,
    )
    .unwrap();

    let apply = cli(&server, &[
        "layout-apply",
        "--input",
        bad.to_str().unwrap(),
        "--workspace",
        "w",
    ]);
    assert_eq!(
        apply.status.code(),
        Some(1),
        "schema mismatch is a server-reported error (exit 1), got {:?}\nstderr: {}",
        apply.status.code(),
        String::from_utf8_lossy(&apply.stderr)
    );
    let stderr = String::from_utf8_lossy(&apply.stderr);
    assert!(stderr.contains("schema_version"), "stderr was: {stderr}");
    assert!(stderr.contains('2'), "stderr should name the file's version: {stderr}");
}

#[test]
fn layout_apply_creates_missing_workspace() {
    let server = HeadlessServer::start("layout-apply-create");
    let ws = cli(&server, &["new-workspace", "--name", "solo"]);
    assert_success(&ws);
    let out = server.dir.join("solo.json");
    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "solo",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_success(&export);

    // Applying under a NEW name creates that workspace (AC2).
    let apply = cli(&server, &[
        "layout-apply",
        "--input",
        out.to_str().unwrap(),
        "--workspace",
        "solo2",
    ]);
    assert_success(&apply);
    let value = list_workspaces_json(&server);
    let names: Vec<&str> =
        value["workspaces"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["solo", "solo2"], "both the original and the applied copy exist");

    // Applying onto an existing name is refused, loudly and non-destructively.
    let again = cli(&server, &[
        "layout-apply",
        "--input",
        out.to_str().unwrap(),
        "--workspace",
        "solo",
    ]);
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("already exists"));
}

#[test]
fn layout_export_refuses_symlinked_output() {
    let server = HeadlessServer::start("layout-export-symlink");
    let ws = cli(&server, &["new-workspace", "--name", "sec"]);
    assert_success(&ws);

    let target = server.dir.join("real-target.json");
    fs::write(&target, "").unwrap();
    let link = server.dir.join("link.json");
    symlink(&target, &link).unwrap();

    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "sec",
        "--output",
        link.to_str().unwrap(),
    ]);
    assert_eq!(
        export.status.code(),
        Some(1),
        "symlinked output must be refused, got {:?}",
        export.status.code()
    );
    assert!(String::from_utf8_lossy(&export.stderr).contains("symlink"));
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "",
        "the symlink target must be untouched"
    );
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

/// `cmux attach --session-list --json` lists discovered sessions with a
/// `socket_path` per entry (issue #63, layer L1). Same shape as
/// `list-sessions --json` plus `socket_path`; exit 0. Modelled on
/// `list_sessions_lists_active_headless_session` (:904).
#[test]
fn attach_session_list_json_includes_socket_path() {
    let dir = unique_temp_dir("attach-sl");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("test-attach.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let pid_file = dir.join("test-attach.pid");
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
        .args(["attach", "--session-list", "--json", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    let entry = sessions
        .iter()
        .find(|s| s["session"] == "test-attach")
        .expect("sessions should contain test-attach");
    assert_eq!(entry["status"], "live", "test-attach should be live, got {entry}");
    let sp = entry["socket_path"].as_str().expect("socket_path field");
    assert!(
        sp.ends_with("test-attach.sock"),
        "socket_path should end with test-attach.sock, got {sp}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// Stale entries (dead pidfile, unconnectable socket) appear with
/// `status == "stale"` and a `socket_path`, alongside live entries.
#[test]
fn attach_session_list_json_marks_stale() {
    let dir = unique_temp_dir("attach-sl-stale");
    fs::create_dir_all(&dir).unwrap();
    let live_socket = dir.join("live-att.sock");
    let mut child = Command::new(bin())
        .args(["--headless", "--socket"])
        .arg(&live_socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let live_pid = dir.join("live-att.pid");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if live_socket.exists() && live_pid.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(live_socket.exists());
    assert!(live_pid.exists());

    // Fake stale pair (nonexistent PID 999999, unconnectable socket).
    let stale_socket = dir.join("stale-att.sock");
    let stale_pid = dir.join("stale-att.pid");
    fs::write(&stale_socket, "fake").unwrap();
    fs::write(&stale_pid, "999999\n").unwrap();

    let output = Command::new(bin())
        .args(["attach", "--session-list", "--json", "--socket"])
        .arg(&live_socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    let stale = sessions
        .iter()
        .find(|s| s["session"] == "stale-att")
        .expect("sessions should contain stale-att");
    assert_eq!(stale["status"], "stale", "stale-att should be stale, got {stale}");
    let stale_sp = stale["socket_path"].as_str().expect("socket_path field");
    assert!(stale_sp.ends_with("stale-att.sock"));
    let live = sessions
        .iter()
        .find(|s| s["session"] == "live-att")
        .expect("sessions should contain live-att");
    assert_eq!(live["status"], "live");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// An empty runtime dir yields `{"sessions":[]}` and exit 0.
#[test]
fn attach_session_list_json_empty() {
    let dir = unique_temp_dir("attach-sl-empty");
    fs::create_dir_all(&dir).unwrap();
    // Point --socket at a path inside the empty dir; discovery scans the
    // parent (the dir) and finds no *.sock.
    let socket = dir.join("nothing.sock");
    let output = Command::new(bin())
        .args(["attach", "--session-list", "--json", "--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .unwrap();
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = value["sessions"].as_array().expect("sessions array");
    assert!(sessions.is_empty(), "expected no sessions, got {sessions:?}");
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
    fs::write(server_cmux_dir.join("mux.json"), r##"{"theme": {"sidebar_rail": "#112233"}}"##)
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
        panic!("headless server did not create socket at {}", socket.display());
    }
    // Give the server a moment to register its resolved chrome (it is set
    // before `serve()` binds, so a live socket implies it is published).
    std::thread::sleep(Duration::from_millis(100));

    // Local laptop config dir: a local overlay that overrides a key
    // binding only, not theme, so the server theme must survive.
    let local_cfg_root = dir.join("local-config");
    let local_cmux_dir = local_cfg_root.join("cmux");
    fs::create_dir_all(&local_cmux_dir).unwrap();
    fs::write(local_cmux_dir.join("mux.local.toml"), "[keys]\nprefix = \"ctrl+s\"\n").unwrap();

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
    assert!(output.status.success(), "attach --print-resolved-config failed: {combined}");

    let merged: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
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
    fs::write(server_cmux_dir.join("mux.json"), r##"{"theme": {"sidebar_rail": "#445566"}}"##)
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
        panic!("headless server did not create socket at {}", socket.display());
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
    assert!(output.status.success(), "cmux get-resolved-config failed: {combined}");

    let chrome: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("expected chrome JSON on stdout, parse failed:{e}\n{combined}"));
    assert_eq!(
        chrome["theme"]["sidebar_rail"].as_str(),
        Some("#445566"),
        "server theme colour missing from get-resolved-config output: {chrome}"
    );
}

/// Issue #42 (scoped first PR): the `cmux plugin` verb group manages
/// `cmux-plugin.toml` manifests on disk only (no execution yet). We
/// install a fixture manifest against a HeadlessServer-style temp env
/// (an isolated XDG_DATA_HOME under the server's temp dir), then list
/// it, then uninstall and confirm `list` reports empty. The server
/// itself is idle for these verbs: the spec scopes this PR to manifest
/// state only, no socket traffic.
#[test]
fn plugin_install_list_uninstall_round_trip() {
    let server = HeadlessServer::start("plugin");
    let data_home = server.dir.join("xdg-data");
    fs::create_dir_all(&data_home).unwrap();
    let manifest_dir = server.dir.join("manifest");
    fs::create_dir_all(&manifest_dir).unwrap();
    let manifest_path = manifest_dir.join("cmux-plugin.toml");
    fs::write(
        &manifest_path,
        "[plugin]\nname = \"pifactory-fleet\"\nentry = \"bin/fleet.wasm\"\nverbs = [\"deploy\", \"rollback\"]\n",
    )
    .unwrap();

    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("CMUX_MUX_SOCKET")
            .output()
            .unwrap()
    };

    let manifest_str = manifest_path.to_str().unwrap().to_string();
    let install = run(&["plugin", "install", &manifest_str]);
    assert_success(&install);
    let install_out = String::from_utf8_lossy(&install.stdout);
    assert!(
        install_out.contains("pifactory-fleet"),
        "install should report the plugin name: {install_out}"
    );

    let list = run(&["plugin", "list"]);
    assert_success(&list);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("pifactory-fleet"),
        "list should name the installed plugin: {list_out}"
    );
    assert!(list_out.contains("enabled"), "list should show the enabled state: {list_out}");
    assert!(list_out.contains("deploy,rollback"), "list should show the claimed verbs: {list_out}");

    let uninstall = run(&["plugin", "uninstall", "pifactory-fleet"]);
    assert_success(&uninstall);
    assert!(
        String::from_utf8_lossy(&uninstall.stdout).contains("pifactory-fleet"),
        "uninstall should echo the removed plugin name"
    );

    let list2 = run(&["plugin", "list"]);
    assert_success(&list2);
    let list2_out = String::from_utf8_lossy(&list2.stdout);
    assert!(
        list2_out.contains("no plugins installed"),
        "after uninstall list should report empty: {list2_out}"
    );
}

/// Issue #42 AC6: the shipped example plugin at
/// `mux/spec/plugins/pifactory-fleet/` has a valid manifest that
/// installs and lists correctly via the `cmux plugin` verb group.
/// This guards against schema drift between the example manifest and
/// the loader's `ManifestFile` parser.
#[test]
fn plugin_shipped_example_manifest_installs() {
    let server = HeadlessServer::start("plugin-ac6");
    let data_home = server.dir.join("xdg-data");
    fs::create_dir_all(&data_home).unwrap();

    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spec/plugins/pifactory-fleet/cmux-plugin.toml")
        .canonicalize()
        .unwrap_or_else(|_| {
            panic!("shipped example manifest not found relative to {}", env!("CARGO_MANIFEST_DIR"))
        });
    assert!(
        manifest_path.exists(),
        "shipped pifactory-fleet manifest must exist at {}",
        manifest_path.display()
    );

    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .env("XDG_DATA_HOME", &data_home)
            .env_remove("CMUX_MUX_SOCKET")
            .output()
            .unwrap()
    };

    let manifest_str = manifest_path.to_str().unwrap().to_string();
    let install = run(&["plugin", "install", &manifest_str]);
    assert_success(&install);

    let list = run(&["plugin", "list"]);
    assert_success(&list);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("pifactory-fleet"),
        "shipped example plugin should list after install: {list_out}"
    );
    // The installed entry should preserve the entry path and the
    // verb allowlist, both of which the loader stores verbatim.
    // Spot-check both so a regression in `cmd_install`'s copy step
    // is caught.
    assert!(
        list_out.contains("bin/fleet.wasm"),
        "installed entry path should be preserved: {list_out}"
    );
    assert!(list_out.contains("cmux_call"), "verb allowlist should include cmux_call: {list_out}");
}

/// Issue #59: `cmux --version` / `-V` print `cmux <version>` and exit 0.
///
/// Issue #71: the version is the build-time constant, not
/// `CARGO_PKG_VERSION`. Pinning this test to the manifest was why the
/// bug survived — the assertion and the bug read the same stale
/// `0.1.0`, so it passed while `-V` was wrong for seventeen releases.
/// The independent checks below are the part that would have caught it.
#[test]
fn version_flag_prints_build_version_and_exits_zero() {
    let expected = format!("cmux {}", mux_core::VERSION);

    let run = |args: &[&str]| {
        Command::new(bin()).args(args).env_remove("CMUX_MUX_SOCKET").output().unwrap()
    };

    for args in [&["--version"][..], &["-V"][..], &["--headless", "--version"][..]] {
        let out = run(args);
        assert_success(&out);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            expected,
            "args {args:?} should print `{expected}`"
        );
        assert!(
            out.stderr.is_empty(),
            "args {args:?} should write nothing to stderr; got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Issue #71: the reported version must be a real release version, not
/// the `0.1.0` manifest placeholder the binary shipped with for
/// seventeen tagged releases. Deliberately asserts against the literal
/// rather than any constant: a check derived from the same source as
/// the value under test cannot catch this class of bug.
#[test]
fn version_is_not_the_stale_placeholder() {
    let version = mux_core::VERSION;
    assert_ne!(version, "0.1.0", "0.1.0 is the pre-#71 placeholder, not a released version");
    assert_ne!(version, "unknown", "build.rs could not resolve any version");

    // Shape is `<major>.<minor>.<patch>` for a release build; a dev
    // build appends `-<n>-g<sha>` off a tag, or just `-g<sha>` when no
    // tag is reachable (a depth-1 CI checkout), plus `-dirty` for local
    // modifications. Only the triple is pinned — every tier of build.rs
    // keeps it, and the suffix is what makes a dev build recognisable.
    let triple = version.split('-').next().unwrap_or_default();
    let parts: Vec<&str> = triple.split('.').collect();
    assert_eq!(parts.len(), 3, "version {version:?} should start with a semver triple");
    for part in parts {
        assert!(
            !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()),
            "version {version:?} has a non-numeric component {part:?}"
        );
    }
}

// =====================================================================
// cmux rename-session (issue #63 L2) — full TDD acceptance suite.
//
// These tests are written RED (cookbook Rule 5) before the feature is
// implemented. At this commit the `rename-session` verb does not exist
// yet, so every end-to-end test fails because the invocation is rejected
// (unknown verb) rather than performing the rename; the helper/unit tests
// fail against compile-scaffolding stubs. The implementation commits turn
// them green. The manual-spawn + `XDG_RUNTIME_DIR` harness mirrors
// `list_sessions_lists_active_headless_session` / `kill_session_*`.
// =====================================================================

/// Spawn a headless daemon as `--session <name>` on `<dir>/<name>.sock` in
/// an isolated `XDG_RUNTIME_DIR`, wait for `.sock`+`.pid`, return the child.
fn spawn_named_headless(dir: &std::path::Path, name: &str) -> Child {
    let socket = dir.join(format!("{name}.sock"));
    let mut child = Command::new(bin())
        .args(["--headless", "--session"])
        .arg(name)
        .args(["--socket"])
        .arg(&socket)
        .env("XDG_RUNTIME_DIR", dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid_file = dir.join(format!("{name}.pid"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if socket.exists() && pid_file.exists() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("daemon {name:?} did not come up at {}", socket.display());
}

/// Read the daemon pid recorded in `<dir>/<name>.pid`.
fn read_pid_file(path: &std::path::Path) -> u32 {
    fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("pid file {} should exist", path.display()))
        .trim()
        .parse::<u32>()
        .unwrap()
}

/// Run a cmux CLI subcommand against `--socket <socket>` with CMUX_MUX_SOCKET
/// unset (so resolution is deterministic) and return its output.
fn run_against(socket: &std::path::Path, xdg: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(["--socket"]).arg(socket).args(args);
    cmd.env("XDG_RUNTIME_DIR", xdg).env_remove("CMUX_MUX_SOCKET");
    cmd.output().unwrap()
}

/// Poll `read-screen` until `needle` appears on the surface (or timeout).
fn wait_for_screen_at(socket: &std::path::Path, surface: u64, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        let out = run_against(
            socket,
            std::path::Path::new("/tmp"),
            &["read-screen", "--surface", &surface.to_string()],
        );
        last = String::from_utf8_lossy(&out.stdout).to_string();
        if last.contains(needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

/// AC1: rename moves `.sock`+`.pid` to the new name and the SAME daemon
/// keeps serving at the new path (pid unchanged).
#[test]
fn rename_session_moves_socket_and_pid_and_keeps_serving() {
    let dir = unique_temp_dir("rename-t1");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");
    let old_pid = dir.join("old.pid");
    let daemon_pid = read_pid_file(&old_pid);

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let new_sock = dir.join("bar.sock");
    let new_pid = dir.join("bar.pid");
    assert!(new_sock.exists(), "new socket should exist after rename");
    assert!(new_pid.exists(), "new pid file should exist after rename");
    assert!(!old_sock.exists(), "old socket should be gone after rename");
    assert!(!old_pid.exists(), "old pid file should be gone after rename");

    // Same daemon keeps serving at the new path.
    let identify = run_against(&new_sock, &dir, &["--json", "identify"]);
    assert_success(&identify);
    let v: serde_json::Value = serde_json::from_slice(&identify.stdout).unwrap();
    assert_eq!(v["session"].as_str(), Some("bar"), "identify should report the new name");
    assert_eq!(
        v["pid"].as_u64(),
        Some(daemon_pid as u64),
        "same daemon pid should serve after rename"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC2: after rename, the old socket is no longer connectable (exit 3)
/// while the new path serves the same daemon; protocol version unchanged.
#[test]
fn rename_makes_old_socket_unreachable_and_keeps_protocol() {
    let dir = unique_temp_dir("rename-t2");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");
    let daemon_pid = read_pid_file(&dir.join("old.pid"));

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    // New path serves the same daemon; protocol must NOT bump (scout Q2).
    let new_sock = dir.join("bar.sock");
    let id_new = run_against(&new_sock, &dir, &["--json", "identify"]);
    assert_success(&id_new);
    let v: serde_json::Value = serde_json::from_slice(&id_new.stdout).unwrap();
    assert_eq!(v["session"].as_str(), Some("bar"));
    assert_eq!(v["pid"].as_u64(), Some(daemon_pid as u64));
    assert_eq!(v["protocol"].as_u64(), Some(6), "rename must not bump the protocol version");

    // Old path is gone -> connect fails with exit 3 (transport convention).
    let id_old = run_against(&old_sock, &dir, &["identify"]);
    assert_eq!(id_old.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&id_old.stderr).contains("cannot connect"));

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC3: after rename, the old session name is gone from discovery and the
/// new name is listed as live.
#[test]
fn old_session_name_gone_after_rename() {
    let dir = unique_temp_dir("rename-t3");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let list = run_against(&old_sock, &dir, &["--json", "list-sessions"]);
    // old.sock is gone, so list-sessions resolves its runtime dir from
    // XDG_RUNTIME_DIR and discovers bar.sock live, old.sock absent.
    // (list-sessions does not need a connectable --socket.)
    let v: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap_or_else(|_| {
        let s = String::from_utf8_lossy(&list.stdout);
        panic!(
            "list-sessions produced non-JSON output: {s}\nstderr: {}",
            String::from_utf8_lossy(&list.stderr)
        )
    });
    let sessions = v["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["session"] == "bar" && s["status"] == "live"),
        "bar should be listed live after rename, got {v}"
    );
    assert!(
        !sessions.iter().any(|s| s["session"] == "old"),
        "old should be absent after rename, got {v}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC4: existing panes keep the `CMUX_MUX_SOCKET` they inherited at spawn
/// (the old path) for their lifetime; panes spawned AFTER the rename
/// inherit the new path. This is the lifetime guarantee (intentional, not
/// a bug) documented in USAGE and the server.rs docstring.
#[test]
fn rename_preserves_inherited_cmux_socket_in_existing_panes() {
    let dir = unique_temp_dir("rename-t4");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    // Existing pane (spawned before the rename).
    let ws = run_against(&old_sock, &dir, &["new-workspace", "--name", "pre"]);
    assert_success(&ws);
    let surface_pre: u64 = String::from_utf8(ws.stdout).unwrap().trim().parse().unwrap();
    let old_sock_str = old_sock.display().to_string();
    // Fish is the default surface shell; the trailing real `\n` submits
    // the line (no --send-cr needed — matches the cli_verbs marker probe).
    let probe = "printf 'E=%s\\n' \"$CMUX_MUX_SOCKET\"\n";
    let send = run_against(
        &old_sock,
        &dir,
        &["send", "--surface", &surface_pre.to_string(), "--text", probe],
    );
    assert_success(&send);
    // Poll for the actual path VALUE (only present after the shell expands
    // the var), not a substring of the typed command.
    let before = wait_for_screen_at(&old_sock, surface_pre, &old_sock_str);
    assert!(
        before.contains(&old_sock_str),
        "pre-rename pane should carry the old socket path; screen was {before:?}"
    );

    // Rename old -> bar.
    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);
    let new_sock = dir.join("bar.sock");
    let new_sock_str = new_sock.display().to_string();

    // Existing pane: env is unchanged for its lifetime (AC4 first half).
    // Re-probe and clear the screen's prior line by checking the LAST `E=`
    // value: it must still be the old path, never the new one.
    let send2 = run_against(
        &new_sock,
        &dir,
        &["send", "--surface", &surface_pre.to_string(), "--text", probe],
    );
    assert_success(&send2);
    // Existing pane keeps the OLD value (env inherited at spawn, unchanged).
    let after = wait_for_screen_at(&new_sock, surface_pre, &old_sock_str);
    assert!(
        after.contains(&old_sock_str) && !after.contains(&new_sock_str),
        "existing pane must keep the old CMUX_MUX_SOCKET after rename; \
         screen was {after:?}"
    );

    // New pane spawned after the rename inherits the refreshed path.
    let ws2 = run_against(&new_sock, &dir, &["new-workspace", "--name", "post"]);
    assert_success(&ws2);
    let surface_post: u64 = String::from_utf8(ws2.stdout).unwrap().trim().parse().unwrap();
    let send3 = run_against(
        &new_sock,
        &dir,
        &["send", "--surface", &surface_post.to_string(), "--text", probe],
    );
    assert_success(&send3);
    // New pane inherits the refreshed (new) path.
    let post = wait_for_screen_at(&new_sock, surface_post, &new_sock_str);
    assert!(
        post.contains(&new_sock_str),
        "post-rename pane should carry the new socket path; screen was {post:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC5: renaming onto a LIVE target session fails with exit 2
/// ("already exists") and leaves the source untouched.
#[test]
fn rename_to_existing_live_session_fails_exit_2() {
    let dir = unique_temp_dir("rename-t5");
    fs::create_dir_all(&dir).unwrap();
    let mut child_old = spawn_named_headless(&dir, "old");
    let mut child_bar = spawn_named_headless(&dir, "bar");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_eq!(
        rename.status.code(),
        Some(2),
        "rename onto a live session must exit 2; stderr: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rename.stderr).to_lowercase().contains("already exists"),
        "stderr should explain the target is in use; got {}",
        String::from_utf8_lossy(&rename.stderr)
    );

    // Source must be untouched (nothing moved).
    assert!(old_sock.exists(), "old socket must survive a refused rename");
    assert!(
        mux_core::server::is_session_socket_live(&old_sock),
        "old session must still be live after a refused rename"
    );

    let _ = child_old.kill();
    let _ = child_old.wait();
    let _ = child_bar.kill();
    let _ = child_bar.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// Target policy (mirrors `serve()`'s stale-clear): a STALE target
/// (dead pid) is cleared and the rename succeeds.
#[test]
fn rename_to_stale_target_clears_and_succeeds() {
    let dir = unique_temp_dir("rename-t6");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    // Synthetic stale target pair (dead pid).
    let stale_sock = dir.join("bar.sock");
    let stale_pid = dir.join("bar.pid");
    fs::write(&stale_sock, b"").unwrap();
    fs::write(&stale_pid, "999999\n").unwrap();

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    // bar.sock is now live under the daemon's pid; old.sock is gone.
    assert!(mux_core::server::is_session_socket_live(&stale_sock));
    assert_eq!(
        read_pid_file(&stale_pid),
        child.id(),
        "bar.pid should now record the (live) daemon pid"
    );
    assert!(!old_sock.exists(), "old socket should be gone after rename");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC6 (defence in depth): invalid `--new` names are rejected CLIENT-side
/// (exit 2, "session name") and never reach the socket. A literal NUL
/// cannot be carried by execve, so it is covered by the unit test T12
/// (`validate_session_name_table`) rather than here.
#[test]
fn rename_rejects_invalid_names() {
    let dir = unique_temp_dir("rename-t7");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let overlong = "a".repeat(256);
    let bad_names: &[&str] = &["", "a/b", "a\\b", "..", ".", " foo", "foo ", "\t", &overlong];
    for bad in bad_names {
        let out = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", bad]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "invalid name {bad:?} should exit 2; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let err = String::from_utf8_lossy(&out.stderr).to_lowercase();
        // Regression guard: bad names must be rejected by validate_session_name
        // (exit 2, "session name") and never fall through to the generic
        // unknown-verb/argument path. Require the validation message AND the
        // absence of the unknown-verb fallback so that a future regression —
        // where a bad name slips past validation and surfaces as an
        // unknown-verb rejection — is caught.
        assert!(
            !err.contains("unknown argument")
                && !err.contains("unknown verb")
                && !err.contains("unexpected argument"),
            "invalid name {bad:?} must not hit the unknown-verb path; got {err:?}"
        );
        assert!(
            err.contains("session name"),
            "invalid name {bad:?} should explain the session-name rejection; got {err:?}"
        );
        // Source must be untouched by every rejected attempt.
        assert!(old_sock.exists(), "old socket must survive a rejected rename ({bad:?})");
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC2/T8: after rename, `identify` reports the new session name and the
/// protocol version is still 6 (rename is an additive command variant).
#[test]
fn identify_reports_new_name_after_rename() {
    let dir = unique_temp_dir("rename-t8");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let id = run_against(&dir.join("bar.sock"), &dir, &["--json", "identify"]);
    assert_success(&id);
    let v: serde_json::Value = serde_json::from_slice(&id.stdout).unwrap();
    assert_eq!(v["session"].as_str(), Some("bar"));
    assert_eq!(v["protocol"].as_u64(), Some(6));

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC1/T9: the pid FILE contents are unchanged across the rename (only the
/// filename moves) and the daemon process is alive throughout.
#[test]
fn pid_file_contents_unchanged_after_rename() {
    let dir = unique_temp_dir("rename-t9");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_pid = dir.join("old.pid");
    let pid_before = read_pid_file(&old_pid);
    assert!(mux_core::server::is_process_alive(pid_before));

    let rename = run_against(
        &dir.join("old.sock"),
        &dir,
        &["rename-session", "--old", "old", "--new", "bar"],
    );
    assert_success(&rename);

    let pid_after = read_pid_file(&dir.join("bar.pid"));
    assert_eq!(pid_after, pid_before, "pid file contents must be identical after rename");
    assert!(mux_core::server::is_process_alive(pid_after), "daemon must stay alive across rename");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// AC7/T10: after rename, the session-list / kill-session / kill-stale verbs
/// work against the renamed session with no regressions.
#[test]
fn rename_no_regressions_on_list_kill_killstale() {
    let dir = unique_temp_dir("rename-t10");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "old");
    let old_sock = dir.join("old.sock");

    let rename = run_against(&old_sock, &dir, &["rename-session", "--old", "old", "--new", "bar"]);
    assert_success(&rename);

    let new_sock = dir.join("bar.sock");
    // list-sessions sees bar live, old absent.
    let list = run_against(&new_sock, &dir, &["--json", "list-sessions"]);
    assert_success(&list);
    let v: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let sessions = v["sessions"].as_array().expect("sessions array");
    assert!(sessions.iter().any(|s| s["session"] == "bar" && s["status"] == "live"));
    assert!(!sessions.iter().any(|s| s["session"] == "old"));

    // kill-session on the renamed name terminates the daemon & cleans up.
    let kill = run_against(&new_sock, &dir, &["kill-session", "--session", "bar"]);
    assert_success(&kill);
    let _ = child.wait();
    assert!(!new_sock.exists(), "bar.sock should be removed by kill-session");
    assert!(!dir.join("bar.pid").exists(), "bar.pid should be removed by kill-session");

    // kill-stale is a clean no-op now.
    let stale = run_against(&new_sock, &dir, &["kill-stale"]);
    assert_success(&stale);

    let _ = fs::remove_dir_all(&dir);
}

// T11 (`rename_session_at_renames_via_socket`) lives in `cli.rs`'s own
// `#[cfg(test)]` module: mux-tui is a bin-only crate, so this integration
// test file links only against the `mux-core` lib + the `cmux` binary and
// cannot import the `pub(crate)` helper. The in-process unit test there
// drives a `mux-core` server directly (no subprocess) and exercises the
// exact code path the picker's `r` flow uses.

/// T10 (issue #63 L3, scout plan): the session-manager overlay previews an
/// *other* session's workspaces with a one-shot `list-workspaces` RPC over
/// that session's control socket (the same connect→write→read path
/// `cli::one_shot_rpc` shares with `rename_rpc`, and that `cmux
/// list-workspaces` rides). `fetch_workspaces` is `pub(crate)` so this
/// bin-test cannot call it directly; instead it drives the identical wire
/// path against two named headless daemons and asserts each returns a
/// parseable workspaces tree with the expected count. If the wire verb or
/// its JSON shape regressed, the overlay's right column would break too.
#[test]
fn overlay_fetch_workspaces_parses_remote_tree() {
    let dir = unique_temp_dir("smgr-fetch");
    fs::create_dir_all(&dir).unwrap();
    let mut child_a = spawn_named_headless(&dir, "alpha");
    let mut child_b = spawn_named_headless(&dir, "beta");
    let sock_a = dir.join("alpha.sock");
    let sock_b = dir.join("beta.sock");

    // Give each daemon a distinct workspace set.
    assert_success(&run_against(&sock_a, &dir, &["new-workspace", "--name", "a-one"]));
    assert_success(&run_against(&sock_a, &dir, &["new-workspace", "--name", "a-two"]));
    assert_success(&run_against(&sock_b, &dir, &["new-workspace", "--name", "b-one"]));

    // Querying B's socket returns B's tree (not A's), proving the overlay
    // can read another session's workspaces over its own socket.
    let list_b = run_against(&sock_b, &dir, &["--json", "list-workspaces"]);
    assert_success(&list_b);
    let value: serde_json::Value = serde_json::from_slice(&list_b.stdout).unwrap();
    let names: Vec<&str> = value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .map(|ws| ws["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        names.iter().any(|n| *n == "b-one"),
        "beta socket should report its own workspace b-one, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| *n == "a-two"),
        "beta socket must NOT leak alpha's workspaces, got {names:?}"
    );

    // And querying A's socket returns A's tree with both of its workspaces.
    let list_a = run_against(&sock_a, &dir, &["--json", "list-workspaces"]);
    assert_success(&list_a);
    let value_a: serde_json::Value = serde_json::from_slice(&list_a.stdout).unwrap();
    let count_a = value_a["workspaces"].as_array().map(|a| a.len()).unwrap_or(0);
    assert!(
        count_a >= 2,
        "alpha socket should report >=2 workspaces, got {count_a}"
    );

    let _ = child_a.kill();
    let _ = child_a.wait();
    let _ = child_b.kill();
    let _ = child_b.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T11 (issue #63 L3, scout plan): focusing a workspace in an *other* session
/// from the overlay sends a one-shot `select-workspace` RPC over that
/// session's socket (the path `cli::select_workspace_remote` rides). Spawns
/// two named daemons, adds workspaces to beta, issues `select-workspace
/// --index 1` against beta's socket, and asserts beta's `list-workspaces`
/// afterwards reports workspace index 1 active. This proves the overlay's
/// right-column Enter on another session remotely moves that session's focus.
#[test]
fn overlay_select_workspace_focuses_remotely() {
    let dir = unique_temp_dir("smgr-select");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "beta");
    let sock = dir.join("beta.sock");

    // Three workspaces; the first is active by default.
    assert_success(&run_against(&sock, &dir, &["new-workspace", "--name", "one"]));
    assert_success(&run_against(&sock, &dir, &["new-workspace", "--name", "two"]));
    assert_success(&run_against(&sock, &dir, &["new-workspace", "--name", "three"]));

    // Remotely focus workspace index 1 ("two") the way the overlay does.
    let select = run_against(&sock, &dir, &["select-workspace", "--index", "1"]);
    assert_success(&select);

    // beta's tree now reports index 1 active.
    let list = run_against(&sock, &dir, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let active = value["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|ws| ws["active"].as_bool() == Some(true))
        .expect("an active workspace");
    assert_eq!(
        active["name"].as_str(),
        Some("two"),
        "select-workspace --index 1 should make 'two' active"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T12 (issue #63 L3, scout plan): an `[unreachable]` socket row is reported
/// by discovery and `kill-session` cleans it without crashing. The overlay
/// drives `cli::kill_session_at` on such rows; this guards that a stale
/// `.sock`/`.pid` pair (no live process) is listed as stale and removable,
/// matching AC8 (unreachable rows must be killable and must not crash).
#[test]
fn overlay_kill_unreachable_does_not_crash_discovery() {
    let dir = unique_temp_dir("smgr-unreachable");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "live");
    let live_sock = dir.join("live.sock");

    // Create a STALE socket/pid pair with no live process behind it.
    let stale_sock = dir.join("ghost.sock");
    let stale_pid = dir.join("ghost.pid");
    std::os::unix::net::UnixListener::bind(&stale_sock)
        .expect("bind stale socket");
    fs::write(&stale_pid, "999999").unwrap(); // a pid that is not alive

    // list-sessions --json reports both, ghost as stale.
    let list = run_against(&live_sock, &dir, &["--json", "list-sessions"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let by_name: std::collections::HashMap<&str, &str> = value["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| (s["name"].as_str().unwrap_or(""), s["status"].as_str().unwrap_or("")))
        .collect();
    assert_eq!(by_name.get("live").copied(), Some("live"));
    assert_eq!(
        by_name.get("ghost").copied(),
        Some("stale"),
        "ghost should be reported stale/unreachable"
    );

    // kill-session on the stale row cleans it (no crash).
    let kill = run_against(&stale_sock, &dir, &["kill-session", "--session", "ghost"]);
    assert_success(&kill);
    assert!(!stale_sock.exists(), "stale socket removed by kill-session");
    assert!(!stale_pid.exists(), "stale pid removed by kill-session");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T13 (issue #63 L3, scout plan): the overlay's rename path and the L2
/// `rename-session` CLI verb agree — both end with the session serving at the
/// new socket and gone from the old. This is the same wire path
/// (`cli::rename_session_at` over `one_shot_rpc`); the L2 suite covers the
/// helper directly, so here we just confirm a rename issued via the verb is
/// observable through `list-sessions` the way the overlay's `r` flow expects.
#[test]
fn overlay_rename_reuses_l2_helper() {
    let dir = unique_temp_dir("smgr-rename");
    fs::create_dir_all(&dir).unwrap();
    let mut child = spawn_named_headless(&dir, "pre");
    let pre_sock = dir.join("pre.sock");

    let rename = run_against(&pre_sock, &dir, &["rename-session", "--old", "pre", "--new", "post"]);
    assert_success(&rename);
    let post_sock = dir.join("post.sock");
    assert!(post_sock.exists(), "new socket exists after rename");
    assert!(!pre_sock.exists(), "old socket gone after rename");

    // list-sessions now reports 'post' live and 'pre' absent — the shape the
    // overlay's rebuilt left column will show after a rename.
    let list = run_against(&post_sock, &dir, &["--json", "list-sessions"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let names: Vec<&str> = value["sessions"]
        .as_array()
        .expect("sessions array")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.iter().any(|n| *n == "post"), "post should be listed: {names:?}");
    assert!(!names.iter().any(|n| *n == "pre"), "pre should be gone: {names:?}");

    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_dir_all(&dir);
}

/// T10 (issue #69, scout plan §3c): REGRESSION -- a genuine first attach to a
/// dead socket must STILL exit non-zero (exit 1), both before and after the
/// swap-recovery fix. The recovery path only fires when there is a
/// last-known-good socket to fall back to (a swap); a fresh `cmux attach`
/// has no origin, so the connect error propagates to `main()` exactly as
/// today. This test pins that behavior so the fix cannot accidentally make a
/// real first-attach silently loop instead of failing.
#[test]
fn first_attach_to_dead_socket_still_exits_nonzero() {
    let dir = unique_temp_dir("attach-dead-first");
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("dead-first.sock");
    // Stale file: not a listening socket, so RemoteSession::connect fails
    // (same technique as serve_recovers_from_stale_socket / attach_session_list_json_marks_stale).
    fs::write(&socket, b"").unwrap();

    let out = Command::new(bin())
        .args(["attach", "--socket"])
        .arg(&socket)
        .env_remove("CMUX_MUX_SOCKET")
        .output()
        .expect("failed to spawn cmux attach");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code(),
        Some(1),
        "first-attach to a dead socket must still exit 1, got {:?}\nstderr:\n{}",
        out.status.code(),
        stderr,
    );
    assert!(
        stderr.contains("attaching to cmux session socket"),
        "stderr should carry the connect-failure context, got: {stderr:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}


// -- issue #76: layout export/apply --------------------------------------

/// Helper: the parsed `--json list-workspaces` payload.
fn list_workspaces_json(server: &HeadlessServer) -> serde_json::Value {
    let listed = cli(server, &["--json", "list-workspaces"]);
    assert_success(&listed);
    serde_json::from_slice(&listed.stdout).unwrap()
}

#[test]
fn layout_export_writes_versioned_workspace_json() {
    let server = HeadlessServer::start("layout-export");
    let ws = cli(&server, &["new-workspace", "--name", "fleet"]);
    assert_success(&ws);

    let value = list_workspaces_json(&server);
    let pane = value["workspaces"][0]["screens"][0]["panes"][0]["id"].as_u64().unwrap();
    let split = cli(&server, &["split", "--pane", &pane.to_string(), "--dir", "right"]);
    assert_success(&split);
    let ratio = cli(&server, &[
        "set-ratio",
        "--pane",
        &pane.to_string(),
        "--dir",
        "right",
        "--ratio",
        "0.6",
    ]);
    assert_success(&ratio);

    let out = server.dir.join("fleet.json");
    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "fleet",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_success(&export);
    assert_eq!(
        String::from_utf8_lossy(&export.stdout).trim(),
        out.display().to_string(),
        "plain mode prints the written path"
    );

    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["schema_version"].as_u64(), Some(1), "doc was {doc}");
    assert_eq!(doc["cmux_version"].as_str(), Some(mux_core::VERSION));
    assert_eq!(doc["workspace"]["name"].as_str(), Some("fleet"));
    let layout = &doc["workspace"]["screens"][0]["layout"];
    assert_eq!(layout["type"].as_str(), Some("split"));
    assert_eq!(layout["dir"].as_str(), Some("right"));
    assert!((layout["ratio"].as_f64().unwrap() - 0.6).abs() < 1e-5);
    assert_eq!(layout["a"]["type"].as_str(), Some("leaf"));
    assert_eq!(layout["a"]["pane"].as_u64(), Some(0));
    assert_eq!(
        doc["workspace"]["screens"][0]["panes"].as_array().unwrap().len(),
        2,
        "both split panes should be recorded"
    );
}

#[test]
fn layout_apply_round_trips_topology_and_argv() {
    let server = HeadlessServer::start("layout-apply");
    let ws = cli(&server, &["new-workspace", "--name", "fleet"]);
    assert_success(&ws);

    let value = list_workspaces_json(&server);
    let pane = value["workspaces"][0]["screens"][0]["panes"][0]["id"].as_u64().unwrap();
    let ws_id = value["workspaces"][0]["id"].as_u64().unwrap();

    // An agent tab with explicit argv + env (the `--exec` spawn path).
    let marker = format!("FLEETMARKER_{}", std::process::id());
    let exec = cli(
        &server,
        &[
            "new-tab",
            "--pane",
            &pane.to_string(),
            "--env",
            "FLEET_TIER=A",
            "--exec",
            "--",
            "/bin/sh",
            "-c",
            &format!("printf '{marker}'; sleep 60"),
        ],
    );
    assert_success(&exec);
    let split = cli(&server, &["split", "--pane", &pane.to_string(), "--dir", "down"]);
    assert_success(&split);

    let out = server.dir.join("fleet.json");
    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "fleet",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_success(&export);
    let doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
    let tabs = &doc["workspace"]["screens"][0]["panes"][0]["tabs"];
    assert_eq!(tabs.as_array().unwrap().len(), 2);
    assert_eq!(
        tabs[1]["command"].as_array().map(|a| a.len()),
        Some(3),
        "recorded argv should round-trip into the file: {tabs}"
    );
    assert_eq!(tabs[1]["env"]["FLEET_TIER"].as_str(), Some("A"));

    let close = cli(&server, &["close-workspace", "--workspace", &ws_id.to_string()]);
    assert_success(&close);

    let apply = cli(&server, &[
        "layout-apply",
        "--input",
        out.to_str().unwrap(),
        "--workspace",
        "fleet",
    ]);
    assert_success(&apply);

    // Topology is back: one workspace, two panes, the exec tab re-spawned.
    let value = list_workspaces_json(&server);
    let workspaces = value["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0]["name"].as_str(), Some("fleet"));
    let layout = &workspaces[0]["screens"][0]["layout"];
    assert_eq!(layout["type"].as_str(), Some("split"), "split geometry restored");
    assert_eq!(layout["dir"].as_str(), Some("down"));
    let panes = workspaces[0]["screens"][0]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0]["tabs"].as_array().unwrap().len(), 2);

    // The re-spawned argv actually ran: poll every surface for the marker.
    let surfaces: Vec<u64> = panes
        .iter()
        .flat_map(|p| p["tabs"].as_array().unwrap().iter())
        .map(|t| t["surface"].as_u64().unwrap())
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw = false;
    while Instant::now() < deadline && !saw {
        for &sid in &surfaces {
            let read = cli(&server, &["read-screen", "--surface", &sid.to_string()]);
            if read.status.success()
                && String::from_utf8_lossy(&read.stdout).contains(&marker)
            {
                saw = true;
                break;
            }
        }
        if !saw {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    assert!(saw, "apply should re-spawn the recorded argv and print {marker}");
}

#[test]
fn layout_export_all_writes_one_file_per_workspace() {
    let server = HeadlessServer::start("layout-export-all");
    for name in ["alpha", "beta"] {
        let ws = cli(&server, &["new-workspace", "--name", name]);
        assert_success(&ws);
    }

    let dir = server.dir.join("fleet");
    let export = cli(&server, &["layout-export-all", "--output-dir", dir.to_str().unwrap()]);
    assert_success(&export);
    for name in ["alpha", "beta"] {
        let path = dir.join(format!("{name}.json"));
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["schema_version"].as_u64(), Some(1), "{name} doc: {doc}");
        assert_eq!(doc["workspace"]["name"].as_str(), Some(name));
    }
}

#[test]
fn layout_apply_rejects_unknown_schema_version_loudly() {
    let server = HeadlessServer::start("layout-apply-v2");
    let bad = server.dir.join("v2.json");
    fs::write(
        &bad,
        r#"{"schema_version":2,"cmux_version":"x","workspace":{"name":"w","active_screen":0,"screens":[{"active_pane":0,"layout":{"type":"leaf","pane":0},"panes":[{"tabs":[{"kind":"pty"}]}]}]}}"#,
    )
    .unwrap();

    let apply = cli(&server, &[
        "layout-apply",
        "--input",
        bad.to_str().unwrap(),
        "--workspace",
        "w",
    ]);
    assert_eq!(
        apply.status.code(),
        Some(1),
        "schema mismatch is a server-reported error (exit 1), got {:?}\nstderr: {}",
        apply.status.code(),
        String::from_utf8_lossy(&apply.stderr)
    );
    let stderr = String::from_utf8_lossy(&apply.stderr);
    assert!(stderr.contains("schema_version"), "stderr was: {stderr}");
    assert!(stderr.contains('2'), "stderr should name the file's version: {stderr}");
}

#[test]
fn layout_apply_creates_missing_workspace() {
    let server = HeadlessServer::start("layout-apply-create");
    let ws = cli(&server, &["new-workspace", "--name", "solo"]);
    assert_success(&ws);
    let out = server.dir.join("solo.json");
    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "solo",
        "--output",
        out.to_str().unwrap(),
    ]);
    assert_success(&export);

    // Applying under a NEW name creates that workspace (AC2).
    let apply = cli(&server, &[
        "layout-apply",
        "--input",
        out.to_str().unwrap(),
        "--workspace",
        "solo2",
    ]);
    assert_success(&apply);
    let value = list_workspaces_json(&server);
    let names: Vec<&str> =
        value["workspaces"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["solo", "solo2"], "both the original and the applied copy exist");

    // Applying onto an existing name is refused, loudly and non-destructively.
    let again = cli(&server, &[
        "layout-apply",
        "--input",
        out.to_str().unwrap(),
        "--workspace",
        "solo",
    ]);
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("already exists"));
}

#[test]
fn layout_export_refuses_symlinked_output() {
    let server = HeadlessServer::start("layout-export-symlink");
    let ws = cli(&server, &["new-workspace", "--name", "sec"]);
    assert_success(&ws);

    let target = server.dir.join("real-target.json");
    fs::write(&target, "").unwrap();
    let link = server.dir.join("link.json");
    symlink(&target, &link).unwrap();

    let export = cli(&server, &[
        "layout-export",
        "--workspace",
        "sec",
        "--output",
        link.to_str().unwrap(),
    ]);
    assert_eq!(
        export.status.code(),
        Some(1),
        "symlinked output must be refused, got {:?}",
        export.status.code()
    );
    assert!(String::from_utf8_lossy(&export.stderr).contains("symlink"));
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "",
        "the symlink target must be untouched"
    );
}
