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

#![cfg(windows)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mtyx")
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("mtyx-win-parity-{name}-{}-{stamp}", std::process::id()))
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
        let child = Command::new(bin())
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
            .unwrap();
        let server = Self { child, socket, dir };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if server.socket.exists() {
                return server;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("headless server did not create socket at {}", server.socket.display());
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(bin())
            .args(["--socket"])
            .arg(&self.socket)
            .args(args)
            .env_remove("MTYX_MUX_SOCKET")
            .env_remove("CMUX_MUX_SOCKET")
            .output()
            .unwrap()
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
/// a few seconds under ConPTY on a cold CI runner).
fn wait_for_screen(server: &HeadlessServer, surface: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while Instant::now() < deadline {
        let out = server.cli(&["read-screen", "--surface", surface]);
        last = String::from_utf8_lossy(&out.stdout).into_owned();
        if last.contains(needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    last
}

#[test]
fn identify_and_list_workspaces_round_trip() {
    let server = HeadlessServer::start("identify");

    // Session create is implicit: a headless daemon IS a session; the
    // identify verb is the canonical "session exists" probe. (--json
    // prints the reply's data object bare — no ok/data envelope — so
    // assert on the fields directly.)
    let identify = server.cli(&["--json", "identify"]);
    assert_success(&identify);
    let v: serde_json::Value = serde_json::from_slice(&identify.stdout).unwrap();
    assert!(v["protocol"].is_number(), "identify should carry protocol: {v}");
    assert!(v["pid"].is_number(), "identify should carry pid: {v}");

    // Workspace create + list.
    let created = server.cli(&["new-workspace", "--name", "parity-ws"]);
    assert_success(&created);
    let listed = server.cli(&["--json", "list-workspaces"]);
    assert_success(&listed);
    let ws: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(
        ws.to_string().contains("parity-ws"),
        "list-workspaces should name parity-ws, got {ws}"
    );
}

#[test]
fn pane_create_send_and_read_screen() {
    let server = HeadlessServer::start("pane");

    // Pane create: a new workspace's default surface is the pane.
    let created = server.cli(&["new-workspace", "--name", "parity-pane"]);
    assert_success(&created);
    let surface: String =
        String::from_utf8(created.stdout).unwrap().trim().parse().unwrap();

    // Send: portable echo probe, submitted with a real CR.
    let marker = "PARITY-MARK-7Q";
    let sent = server.cli(&[
        "send",
        "--surface",
        &surface,
        "--text",
        &format!("echo {marker}"),
        "--send-cr",
    ]);
    assert_success(&sent);

    // read-screen: the echoed marker must render on the pane.
    let screen = wait_for_screen(&server, &surface, marker);
    assert!(screen.contains(marker), "marker must appear on screen, got: {screen}");
}

#[test]
fn claude_hook_installer_round_trip() {
    // One agent-hook installer round-trip: `claude install-hooks`
    // writes/merges settings JSON (exercising the LockFileEx store path
    // when the hook records sessions), `--uninstall` removes the entry.
    let project = unique_temp_dir("claude-hook");
    std::fs::create_dir_all(&project).unwrap();

    let install = Command::new(bin())
        .args(["claude", "install-hooks"])
        .current_dir(&project)
        .env("USERPROFILE", &project)
        .output()
        .unwrap();
    assert_success(&install);
    let settings = project.join(".claude").join("settings.json");
    assert!(settings.exists(), "claude settings.json must be created at {}", settings.display());
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(text.contains("mtyx"), "settings should reference the mtyx binary: {text}");

    let uninstall = Command::new(bin())
        .args(["claude", "install-hooks", "--uninstall"])
        .current_dir(&project)
        .env("USERPROFILE", &project)
        .output()
        .unwrap();
    assert_success(&uninstall);

    let _ = std::fs::remove_dir_all(&project);
}

#[test]
fn conpty_lifecycle_spawn_write_read_resize_kill() {
    use portable_pty::{CommandBuilder, PtySize};

    // Direct ConPTY lifecycle through portable-pty (the same backend
    // the daemon uses for local panes): spawn the default shell, write
    // a marker, read it back, resize, kill.
    let pty = portable_pty::native_pty_system()
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .expect("open ConPTY");

    let mut cmd = CommandBuilder::new_default_prog();
    cmd.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(cmd).expect("spawn default shell in ConPTY");
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().expect("clone ConPTY reader");
    let mut writer = pty.master.take_writer().expect("take ConPTY writer");

    let marker = "CONPTY-MARK-7Q";
    let line = format!("echo {marker}\r");
    writer.write_all(line.as_bytes()).expect("write to ConPTY");
    writer.flush().expect("flush ConPTY");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                if seen.contains(marker) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(seen.contains(marker), "ConPTY must echo the marker, got: {seen:?}");

    // Resize: must not error (the ConPTY backend forwards it).
    pty.master
        .resize(PtySize { rows: 30, cols: 120, pixel_width: 0, pixel_height: 0 })
        .expect("resize ConPTY");

    // Kill: the child must exit.
    child.kill().expect("kill ConPTY child");
    let _ = child.wait();
}
