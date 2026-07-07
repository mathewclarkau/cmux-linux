//! Forwards an OSC 9 / OSC 777 / kitty desktop notification (detected by
//! mux-core, see `MuxEvent::OscNotification`) to the host desktop via
//! `notify-send`, the de facto Linux standard (part of libnotify).
//!
//! Fire-and-forget: spawned without waiting so a slow or missing
//! `notify-send` can never stall the render loop, and a missing binary is
//! silently ignored rather than shown as an error (not every machine runs
//! a notification daemon, e.g. over SSH without X/Wayland forwarding).

use std::process::{Command, Stdio};

pub fn send(pane_label: &str, title: &str, body: &str) {
    let summary = if title.is_empty() { pane_label.to_string() } else { format!("{pane_label}: {title}") };
    let _ = Command::new("notify-send")
        .arg("--app-name=cmux-mux")
        .arg(&summary)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
