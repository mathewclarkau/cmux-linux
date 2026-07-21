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
        &["report-agent", "--surface", &surface.to_string(), "--state", "idle", "--source", "socket"],
    );
    assert_success(&downgrade);
    let list = cli(&server, &["--json", "list-agents", "--state", "working"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["agents"].as_array().unwrap().len(), 1, "hook report should still be in effect");

    // Plain (non-JSON) output is one line per agent.
    let plain = cli(&server, &["list-agents"]);
    assert_success(&plain);
    let text = String::from_utf8(plain.stdout).unwrap();
    assert_eq!(text.trim(), format!("{surface} working hook sess-abc"));

    // Bad state/source are rejected.
    let bad = cli(
        &server,
        &["report-agent", "--surface", &surface.to_string(), "--state", "nonsense", "--source", "hook"],
    );
    assert_eq!(bad.status.code(), Some(1));
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
    let created_id: u64 = String::from_utf8(created.stdout)
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
        &["set-workspace-color", "--workspace", &workspace_id.to_string(), "--colour", "#ff8800"],
    );
    assert_success(&set);
    assert!(set.stdout.is_empty(), "set-workspace-color should be quiet on success");

    let list = cli(&server, &["--json", "list-workspaces"]);
    assert_success(&list);
    let value: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value["workspaces"][0]["color"].as_str(), Some("#ff8800"));

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
    let read_back = fs::read(&guard.target_path)
        .expect("symlink target file must still exist after refusal");
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
        &["trigger-flash", "--workspace", &workspace_id.to_string(), "--surface", &surface.to_string()],
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
    fn new() -> Self {
        const KNOWN_CONTENT: &str =
            "#!/bin/sh\necho this is a precious file that must survive install-skill\n";

        let project_dir = unique_temp_dir("install-skill-symlink");
        fs::create_dir_all(&project_dir).expect("mkdir project_dir");

        // The exact non-global path install-skill will write to.
        let symlink_path: PathBuf = project_dir
            .join(".claude")
            .join("skills")
            .join("cmux-orchestration")
            .join("SKILL.md");
        fs::create_dir_all(symlink_path.parent().expect("symlink_path has parent"))
            .expect("mkdir .claude/skills/cmux-orchestration");

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
        assert!(
            symlink_path.exists(),
            "fixture broken: symlink does not resolve to a real file"
        );
        let meta = fs::symlink_metadata(&symlink_path)
            .expect("symlink_metadata on the freshly created symlink");
        assert!(
            meta.file_type().is_symlink(),
            "fixture broken: SKILL.md is not a symlink"
        );

        Self {
            project_dir,
            symlink_path,
            target_path,
            original_content,
        }
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
