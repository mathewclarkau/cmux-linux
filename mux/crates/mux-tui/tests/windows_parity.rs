//! Windows parity integration tests (cfg(windows); run by the
//! windows-latest CI leg). These exercise the milestone-C surface end
//! to end against a real headless daemon: session create, workspace +
//! pane create, send, read-screen, list-workspaces, an agent-hook
//! installer round-trip, and a ConPTY lifecycle (spawn default shell,
//! write, read output, resize, kill) via portable-pty.
//!
//! Shell probes are deliberately portable: `echo <marker>` works in
//! cmd, PowerShell and pwsh alike, and submission uses `--send-cr`
//! (Enter is CR on Windows consoles).
//!
//! Diagnosability rules for this file (PR #101 round 8): every wait is
//! bounded by a hard deadline and every failure panics inline with the
//! stage name and the evidence seen so far, because a hung sibling test
//! can otherwise suppress this binary's failure summary entirely. The
//! CI leg re-runs this file with `--nocapture --test-threads=1` so the
//! `stage:` traces stream live into the log.

#![cfg(windows)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Hard cap on any single CLI invocation. Generous (the daemon-spawning
/// verbs link wasmtime and pay first-run init on a cold runner), but
/// finite: an unbounded `.output()` here is exactly how round 8's run
/// 35394419102 produced no diagnostics for 66 minutes.
const CLI_DEADLINE: Duration = Duration::from_secs(60);

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mtyx")
}

/// Print a stage trace immediately. Under `--nocapture` (how CI runs
/// this file) it streams live, so a hang's last stage is always
/// visible even when the failure summary never prints.
fn stage(name: &str) {
    eprintln!("[parity] {}", name);
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("mtyx-win-parity-{}-{}-{}", name, std::process::id(), stamp))
}

/// Run a command to completion under [CLI_DEADLINE]; on expiry kill it
/// and panic inline with the stage name (never wait forever). Output
/// streams are drained after exit, when the child's write ends are
/// closed, so the drain cannot block either.
fn bounded_output(stage_name: &str, mut cmd: Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{}: failed to spawn {:?}: {}", stage_name, cmd.get_program(), e));
    let deadline = Instant::now() + CLI_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = drain(child.stdout.take());
                let stderr = drain(child.stderr.take());
                return Output { status, stdout, stderr };
            }
            Ok(None) => {}
            Err(e) => panic!("{}: wait failed: {}", stage_name, e),
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{}: command {:?} did not exit within {:?} (killed)",
                stage_name,
                cmd.get_program(),
                CLI_DEADLINE
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn drain(mut pipe: Option<impl Read>) -> Vec<u8> {
    // Only called on an exited (or killed) child: the pipe's write end
    // is closed, so read_to_end terminates.
    let mut out = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(&mut out);
    }
    out
}

fn assert_success(stage_name: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{}: expected success, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        stage_name,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Headless daemon on an explicit socket in a temp dir, with TEMP and
/// XDG_STATE_HOME scoped so no discovery path can reach the user's
/// real runtime dirs. Both env spellings of the socket override are
/// cleared so resolution is deterministic (the rename shim would
/// otherwise resurrect CMUX_MUX_SOCKET).
struct HeadlessServer {
    child: Child,
    socket: PathBuf,
    dir: PathBuf,
}

impl HeadlessServer {
    fn start(name: &str) -> Self {
        let dir = unique_temp_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("parity.sock");
        let mut child = Command::new(bin())
            .args(["--headless", "--socket"])
            .arg(&socket)
            .env("TEMP", &dir)
            .env("TMP", &dir)
            .env("XDG_STATE_HOME", &dir)
            .env_remove("MTYX_MUX_SOCKET")
            .env_remove("CMUX_MUX_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("server-start: failed to spawn {}: {}", bin(), e));
        let mut server = Self { child, socket, dir };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if server.socket.exists() {
                stage("server-start: socket up");
                return server;
            }
            if let Ok(Some(status)) = server.child.try_wait() {
                // Daemon died before binding: surface its stderr now,
                // inline, instead of waiting for a summary that a hung
                // sibling may never let print.
                let err = String::from_utf8_lossy(&drain(server.child.stderr.take())).into_owned();
                panic!("server-start: daemon exited early with {}; stderr:\n{}", status, err);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = server.child.kill();
        let _ = server.child.wait();
        let err = String::from_utf8_lossy(&drain(server.child.stderr.take())).into_owned();
        panic!(
            "server-start: no socket at {} within 30s; daemon stderr:\n{}",
            server.socket.display(),
            err
        );
    }

    fn cli(&self, stage_name: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(bin());
        cmd.args(["--socket"]).arg(&self.socket).args(args);
        cmd.env_remove("MTYX_MUX_SOCKET").env_remove("CMUX_MUX_SOCKET");
        bounded_output(stage_name, cmd)
    }
}

impl Drop for HeadlessServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Poll read-screen until the marker shows up (shell startup can take
/// a few seconds under ConPTY on a cold CI runner). Panics inline at
/// the deadline with the last screen and the last read-screen
/// diagnostics rather than returning quietly for the caller to guess.
fn wait_for_screen(server: &HeadlessServer, surface: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    let mut last_stderr = String::new();
    let mut last_code: Option<i32> = None;
    while Instant::now() < deadline {
        let out = server.cli("read-screen", &["read-screen", "--surface", surface]);
        last = String::from_utf8_lossy(&out.stdout).into_owned();
        last_stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        last_code = out.status.code();
        if last.contains(needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "read-screen: marker {:?} not on surface {:?} within 30s; last exit={:?}, last stderr:\n{}\nlast screen:\n{}",
        needle,
        surface,
        last_code,
        last_stderr,
        last
    );
}

#[test]
fn identify_and_list_workspaces_round_trip() {
    let server = HeadlessServer::start("identify");

    // Session create is implicit: a headless daemon IS a session; the
    // identify verb is the canonical "session exists" probe. (--json
    // prints the reply's data object bare — no ok/data envelope — so
    // assert on the fields directly.)
    let identify = server.cli("identify", &["--json", "identify"]);
    assert_success("identify", &identify);
    let v: serde_json::Value = serde_json::from_slice(&identify.stdout).unwrap_or_else(|e| {
        panic!(
            "identify: --json output not valid JSON ({}): {}",
            e,
            String::from_utf8_lossy(&identify.stdout)
        )
    });
    assert!(v["protocol"].is_number(), "identify should carry protocol: {}", v);
    assert!(v["pid"].is_number(), "identify should carry pid: {}", v);

    // Workspace create + list.
    let created = server.cli("new-workspace", &["new-workspace", "--name", "parity-ws"]);
    assert_success("new-workspace", &created);
    let listed = server.cli("list-workspaces", &["--json", "list-workspaces"]);
    assert_success("list-workspaces", &listed);
    let ws: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap_or_else(|e| {
        panic!(
            "list-workspaces: --json output not valid JSON ({}): {}",
            e,
            String::from_utf8_lossy(&listed.stdout)
        )
    });
    assert!(
        ws.to_string().contains("parity-ws"),
        "list-workspaces should name parity-ws, got {}",
        ws
    );
}

#[test]
fn pane_create_send_and_read_screen() {
    let server = HeadlessServer::start("pane");

    // Pane create: a new workspace's default surface is the pane.
    let created = server.cli("new-workspace", &["new-workspace", "--name", "parity-pane"]);
    assert_success("new-workspace", &created);
    let stdout_text = String::from_utf8_lossy(&created.stdout).into_owned();
    let surface = stdout_text.trim().to_string();
    assert!(
        !surface.is_empty(),
        "new-workspace: stdout is not a surface id: {:?}",
        stdout_text
    );
    stage(&format!("pane-create: surface {}", surface));

    // Send: portable echo probe, submitted with a real CR.
    let marker = "PARITY-MARK-7Q";
    let sent = server.cli(
        "send",
        &["send", "--surface", &surface, "--text", &format!("echo {}", marker), "--send-cr"],
    );
    assert_success("send", &sent);

    // read-screen: the echoed marker must render on the pane.
    let screen = wait_for_screen(&server, &surface, marker);
    assert!(screen.contains(marker), "marker must appear on screen, got: {}", screen);
}

#[test]
fn claude_hook_installer_round_trip() {
    // One agent-hook installer round-trip: `claude install-hooks`
    // writes/merges settings JSON (exercising the LockFileEx store path
    // when the hook records sessions), `--uninstall` removes the entry.
    let project = unique_temp_dir("claude-hook");
    std::fs::create_dir_all(&project).unwrap();
    stage(&format!("claude-install: project {}", project.display()));

    // USERPROFILE points home_dir() at the temp project; HOME is also
    // cleared because home_dir() honours an explicit $HOME FIRST
    // (Git-Bash parity, PR #101 round 7) — a runner-image HOME would
    // otherwise leak the install into the real user profile.
    let mut install_cmd = Command::new(bin());
    install_cmd
        .args(["claude", "install-hooks"])
        .current_dir(&project)
        .env("USERPROFILE", &project)
        .env_remove("HOME");
    let install = bounded_output("claude-install", install_cmd);
    assert_success("claude-install", &install);
    let settings = project.join(".claude").join("settings.json");
    assert!(
        settings.exists(),
        "claude-install: settings.json must be created at {}",
        settings.display()
    );
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(text.contains("mtyx"), "settings should reference the mtyx binary: {}", text);

    let mut uninstall_cmd = Command::new(bin());
    uninstall_cmd
        .args(["claude", "install-hooks", "--uninstall"])
        .current_dir(&project)
        .env("USERPROFILE", &project)
        .env_remove("HOME");
    let uninstall = bounded_output("claude-uninstall", uninstall_cmd);
    assert_success("claude-uninstall", &uninstall);

    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn conpty_lifecycle_spawn_write_read_resize_kill() {
    use portable_pty::{CommandBuilder, PtySize};

    // Direct ConPTY lifecycle through portable-pty (the same backend
    // the daemon uses for local panes): spawn the default shell, write
    // a marker, read it back, resize, kill.
    //
    // Round-8 lesson: the master reader is a BLOCKING pipe read. A
    // deadline checked between reads is decorative — a quiet pipe
    // parks the test inside read() forever (66-minute hang, run
    // 35394419102). The reader therefore runs on its own thread and
    // the main loop gathers chunks via recv_timeout, so the deadline
    // is enforced no matter what the pipe does. The old loop's EOF arm
    // (Ok(0) => {}) also hot-spun on EOF; EOF now ends the wait with a
    // diagnostic instead.
    stage("conpty: openpty");
    let pty = portable_pty::native_pty_system()
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .expect("conpty: open ConPTY");

    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env("TERM", "xterm-256color");
    stage("conpty: spawn default prog");
    let mut child = pty.slave.spawn_command(cmd).expect("conpty: spawn default shell in ConPTY");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("conpty: clone ConPTY reader");
    let mut writer = pty.master.take_writer().expect("conpty: take ConPTY writer");

    let marker = "CONPTY-MARK-7Q";
    let line = format!("echo {}\r", marker);
    stage("conpty: write echo line");
    writer.write_all(line.as_bytes()).expect("conpty: write to ConPTY");
    writer.flush().expect("conpty: flush ConPTY");

    stage("conpty: read until marker (30s deadline)");
    let (tx, rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    // The reader thread is intentionally detached: after the marker is
    // found (or the deadline hits) it may stay parked in a blocking
    // read until the master handle drops, and joining it would just
    // re-create the unbounded wait this rewrite removes. It exits when
    // the process does.
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // EOF: pipe closed (child died / ConPTY torn down).
                    let _ = tx.send(Ok(Vec::new()));
                    break;
                }
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break; // receiver gone (deadline hit): stop
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    let mut eof = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(bytes)) if bytes.is_empty() => {
                eof = true;
                break;
            }
            Ok(Ok(bytes)) => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains(marker) {
                    break;
                }
            }
            Ok(Err(e)) => panic!("conpty: reader errored: {}; output so far: {:?}", e, seen),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                eof = true;
                break;
            }
        }
    }
    assert!(
        seen.contains(marker),
        "conpty: marker {:?} never arrived within 30s (eof={}); ConPTY output so far: {:?}",
        marker,
        eof,
        seen
    );
    stage("conpty: marker seen");
    // Receiver dropped here so the parked reader thread stops at its
    // next send (if any) instead of accumulating output nobody reads.
    drop(rx);

    // Resize: must not error (the ConPTY backend forwards it).
    stage("conpty: resize");
    pty.master
        .resize(PtySize { rows: 30, cols: 120, pixel_width: 0, pixel_height: 0 })
        .expect("conpty: resize ConPTY");

    // Kill: the child must exit. kill() is TerminateProcess under the
    // hood, so the subsequent wait() cannot park on a live child.
    stage("conpty: kill");
    child.kill().expect("conpty: kill ConPTY child");
    let _ = child.wait();
    stage("conpty: done");
}
