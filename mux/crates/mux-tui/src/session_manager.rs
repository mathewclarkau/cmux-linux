//! In-TUI session manager overlay (issue #63, layer L3).
//!
//! Opened by leader (Ctrl-b) + `S`, this overlay is a two-column modal:
//! the left column lists every discovered cmux session (reusing
//! [`crate::cli::discover_sessions`], the same path `list-sessions`
//! walks), and the right column previews the *focused* session's
//! workspaces, lazy-fetched over a one-shot `list-workspaces` socket RPC
//! on a worker thread (all socket I/O off the UI thread). The overlay
//! reuses the L1 kill, L2 rename, and `select-workspace` helpers — it
//! forks none of them.
//!
//! This module owns the **pure** state machine + the worker entry point;
//! the App wires up key dispatch, the `AppEvent::SessionManagerUpdate`
//! bridge, and the render branch (see `app.rs` / `ui/mod.rs`). The state
//! owns its data outright (like `FinderState`) so input handling never
//! borrows the live `Session`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cli::DiscoveredSession;
use crate::session::TreeView;
use crate::ui::input::{InputEvent, TextInput};

/// Which overlay column currently holds the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Left,
    Right,
}

/// Right-column state for a single session's workspaces.
///
/// `Debug` is implemented by hand so the (deeply-nested) `TreeView` does
/// not need to derive it.
pub enum WorkspaceColumn {
    /// Before the session ever gained focus (or before the overlay opened
    /// for a non-current session). Distinct from `Loading` so the draw
    /// path can render a hint rather than a spinner.
    NotFetched,
    /// A `list-workspaces` RPC is in flight on a worker thread.
    Loading,
    /// The RPC returned a tree.
    Ready(TreeView),
    /// The socket is present but not connectable (dead daemon / stale row).
    Unreachable,
}

impl std::fmt::Debug for WorkspaceColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceColumn::NotFetched => write!(f, "NotFetched"),
            WorkspaceColumn::Loading => write!(f, "Loading"),
            WorkspaceColumn::Ready(tree) => {
                write!(f, "Ready({} workspace(s))", tree.workspaces.len())
            }
            WorkspaceColumn::Unreachable => write!(f, "Unreachable"),
        }
    }
}

impl Clone for WorkspaceColumn {
    fn clone(&self) -> Self {
        match self {
            WorkspaceColumn::NotFetched => WorkspaceColumn::NotFetched,
            WorkspaceColumn::Loading => WorkspaceColumn::Loading,
            WorkspaceColumn::Ready(tree) => WorkspaceColumn::Ready(tree.clone()),
            WorkspaceColumn::Unreachable => WorkspaceColumn::Unreachable,
        }
    }
}

/// Inline modal the overlay can be in. Mirrors `session_picker::Mode`
/// (Browse/ConfirmKill/Rename). The `/` filter lives in
/// [`SessionManagerState::filter`] independently.
#[derive(Debug, Clone)]
pub enum Mode {
    Browse,
    /// y/N confirmation for killing a session row.
    ConfirmKill { index: usize, name: String },
    /// Inline rename of the focused LIVE session (reuses
    /// `cli::rename_session_at`). `socket_path` is the session's current
    /// socket; `old_name` pre-fills the input.
    Rename { socket_path: PathBuf, old_name: String, input: TextInput },
}

/// What `handle_key` wants the App to do after handling a key. Mirrors the
/// finder/picker action enums — the state machine decides, the App acts.
#[derive(Debug, Clone)]
pub enum SessionManagerAction {
    None,
    /// Redraw the overlay (selection/mode/status changed).
    Redraw,
    /// q / Esc / leader-S: close the overlay and restore focus.
    Close,
    /// Kill the session at this row index (App calls `cli::kill_session_at`).
    KillSession(usize),
    /// Commit the rename: App calls `cli::rename_session_at(socket, new_name)`.
    RenameSession { socket: PathBuf, new_name: String },
    /// Attach to another session (case b, left row Enter). The App does the
    /// quit+reattach (`RunOutcome::Reattach`).
    AttachSession { socket: PathBuf },
    /// Focus a workspace in the *current* session in-process (case a).
    FocusWorkspaceInPlace { index: usize },
    /// Focus a workspace in an *other* session remotely (select-workspace
    /// one-shot RPC) and reattach to it (case b, right row Enter).
    AttachOtherSessionWorkspace { socket: PathBuf, index: usize },
    /// A transient status line message (e.g. "unreachable — cannot attach").
    SetStatus(String),
    /// Re-run discovery and reset the right column (the App rebuilds the
    /// session list and clears the workspace cache).
    Refresh,
}

/// Overlay state. Constructed by the App on open (which seeds the current
/// session's right column for free from `App::tree`); mutated by
/// [`handle_key`](Self::handle_key) and by worker results landing via
/// [`set_workspaces`](Self::set_workspaces).
#[derive(Debug, Clone)]
pub struct SessionManagerState {
    pub sessions: Vec<DiscoveredSession>,
    pub workspaces: HashMap<PathBuf, WorkspaceColumn>,
    pub left_sel: usize,
    pub right_sel: usize,
    pub focus: Column,
    /// `Some` when the `/` substring filter is active.
    pub filter: Option<TextInput>,
    pub status: String,
    pub mode: Mode,
    /// The running TUI's own socket, for `[current]` tagging and the
    /// in-place vs reattach decision.
    pub own_socket: PathBuf,
    /// Set when the user picked another session to reattach to. The App
    /// drains this after `app::run` returns to drive `RunOutcome::Reattach`.
    pub pending_reattach: Option<PathBuf>,
}

impl SessionManagerState {
    pub fn new(own_socket: PathBuf, sessions: Vec<DiscoveredSession>) -> Self {
        SessionManagerState {
            sessions,
            workspaces: HashMap::new(),
            left_sel: 0,
            right_sel: 0,
            focus: Column::Left,
            filter: None,
            status: String::new(),
            mode: Mode::Browse,
            own_socket,
            pending_reattach: None,
        }
    }

    /// Indices of sessions passing the active `/` filter (or every session
    /// when the filter is inactive). Unit-testable: the predicate is pure.
    pub fn filtered_sessions(&self) -> Vec<usize> {
        let query = match &self.filter {
            Some(f) => f.as_str(),
            None => return (0..self.sessions.len()).collect(),
        };
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let titles = self.workspace_titles(&s.socket_path);
                if passes_filter(query, &s.session, &titles) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    /// The session row currently under the left-column cursor.
    pub fn focused_session(&self) -> Option<&DiscoveredSession> {
        self.sessions.get(self.left_sel)
    }

    /// The ready workspace tree for the focused session, if fetched.
    pub fn focused_workspaces(&self) -> Option<&TreeView> {
        let socket = self.focused_session()?.socket_path.clone();
        match self.workspaces.get(&socket)? {
            WorkspaceColumn::Ready(tree) => Some(tree),
            _ => None,
        }
    }

    /// Workspace titles (names) for the right-column filter when a tree is
    /// `Ready`; empty otherwise. Cheap (no clone of the tree).
    fn workspace_titles(&self, socket: &Path) -> Vec<String> {
        match self.workspaces.get(socket) {
            Some(WorkspaceColumn::Ready(tree)) => {
                tree.workspaces.iter().map(|w| w.name.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Route a key to an action. Pure (no I/O); the App interprets the
    /// returned [`SessionManagerAction`].
    pub fn handle_key(&mut self, key: KeyEvent) -> SessionManagerAction {
        // Ctrl-C aborts from any mode.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SessionManagerAction::Close;
        }
        match self.mode.clone() {
            Mode::ConfirmKill { index, name } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.mode = Mode::Browse;
                    self.status = format!("killed {name}");
                    SessionManagerAction::KillSession(index)
                }
                _ => {
                    self.mode = Mode::Browse;
                    SessionManagerAction::Redraw
                }
            },
            Mode::Rename { socket_path, old_name, mut input } => {
                match input.handle_key(&key) {
                    InputEvent::Commit => {
                        let new_name = input.as_str().trim().to_string();
                        self.mode = Mode::Browse;
                        if new_name.is_empty() {
                            self.status = "session name cannot be empty".to_string();
                            return SessionManagerAction::Redraw;
                        }
                        if new_name == old_name {
                            self.status = format!("unchanged ({old_name})");
                            return SessionManagerAction::Redraw;
                        }
                        SessionManagerAction::RenameSession { socket: socket_path, new_name }
                    }
                    InputEvent::Cancel => {
                        self.mode = Mode::Browse;
                        SessionManagerAction::Redraw
                    }
                    InputEvent::Changed | InputEvent::None => {
                        self.mode = Mode::Rename { socket_path, old_name, input };
                        SessionManagerAction::Redraw
                    }
                }
            }
            Mode::Browse => self.browse_key(key),
        }
    }

    /// Keys for the default Browse mode (left/right navigation, filter, and
    /// the row actions). The `/` filter, when active, captures typeable
    /// input; `Esc` clears an active filter first and only closes on a
    /// second press.
    fn browse_key(&mut self, key: KeyEvent) -> SessionManagerAction {
        // Filter-active: typeable input routes to the filter box.
        if let Some(input) = self.filter.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.filter = None;
                    self.left_sel = self.filtered_sessions().first().copied().unwrap_or(0);
                    return SessionManagerAction::Redraw;
                }
                KeyCode::Enter => {
                    // Keep the filter but drop into browse; Enter on a row acts.
                    return self.left_enter();
                }
                _ => {}
            }
            match input.handle_key(&key) {
                InputEvent::Cancel => {
                    self.filter = None;
                    self.left_sel = self.filtered_sessions().first().copied().unwrap_or(0);
                    SessionManagerAction::Redraw
                }
                InputEvent::Commit | InputEvent::Changed | InputEvent::None => {
                    // Clamping the cursor to the (possibly shrunk) filtered
                    // set keeps the selection valid as the query tightens.
                    let visible = self.filtered_sessions();
                    if !visible.iter().any(|&i| i == self.left_sel) {
                        self.left_sel = visible.first().copied().unwrap_or(0);
                        self.right_sel = 0;
                    }
                    SessionManagerAction::Redraw
                }
            }
        } else {
            self.unfiltered_browse_key(key)
        }
    }

    fn unfiltered_browse_key(&mut self, key: KeyEvent) -> SessionManagerAction {
        match key.code {
            KeyCode::Char('q') => SessionManagerAction::Close,
            KeyCode::Esc => SessionManagerAction::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_left(true);
                SessionManagerAction::Redraw
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_left(false);
                SessionManagerAction::Redraw
            }
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
                if self.focused_workspaces().is_some() {
                    self.focus = Column::Right;
                    return SessionManagerAction::Redraw;
                }
                SessionManagerAction::None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.focus = Column::Left;
                SessionManagerAction::Redraw
            }
            KeyCode::Char('R') => SessionManagerAction::Refresh,
            KeyCode::Char('/') => {
                self.filter = Some(TextInput::new(String::new()));
                SessionManagerAction::Redraw
            }
            KeyCode::Enter => {
                if self.focus == Column::Right {
                    self.right_enter()
                } else {
                    self.left_enter()
                }
            }
            KeyCode::Char('r') => self.start_rename(),
            KeyCode::Char('x') | KeyCode::Char('K') => self.start_kill(),
            _ => SessionManagerAction::None,
        }
    }

    /// Left-column Enter: attach to / hint at the focused session row.
    fn left_enter(&mut self) -> SessionManagerAction {
        let Some(s) = self.focused_session().cloned() else {
            return SessionManagerAction::None;
        };
        if !s.live {
            return SessionManagerAction::SetStatus(format!(
                "{} is unreachable (socket not connectable) — cannot attach",
                s.session
            ));
        }
        if s.socket_path == self.own_socket {
            return SessionManagerAction::SetStatus(
                "already attached to this session".to_string(),
            );
        }
        SessionManagerAction::AttachSession { socket: s.socket_path }
    }

    /// Right-column Enter: focus a workspace in the focused session
    /// (in-place for the current session; remote-focus + reattach otherwise).
    fn right_enter(&mut self) -> SessionManagerAction {
        let Some(s) = self.focused_session().cloned() else {
            return SessionManagerAction::None;
        };
        let index = self.right_sel;
        if s.socket_path == self.own_socket {
            SessionManagerAction::FocusWorkspaceInPlace { index }
        } else {
            SessionManagerAction::AttachOtherSessionWorkspace { socket: s.socket_path, index }
        }
    }

    fn start_rename(&mut self) -> SessionManagerAction {
        let Some(s) = self.focused_session().cloned() else {
            return SessionManagerAction::None;
        };
        if !s.live {
            return SessionManagerAction::SetStatus(
                "no live session focused to rename".to_string(),
            );
        }
        self.mode = Mode::Rename {
            socket_path: s.socket_path,
            old_name: s.session.clone(),
            input: TextInput::new(s.session),
        };
        SessionManagerAction::Redraw
    }

    fn start_kill(&mut self) -> SessionManagerAction {
        let Some(s) = self.focused_session().cloned() else {
            return SessionManagerAction::None;
        };
        self.mode = Mode::ConfirmKill { index: self.left_sel, name: s.session };
        SessionManagerAction::Redraw
    }

    /// Move the left cursor one step within the filtered-visible set.
    fn move_left(&mut self, down: bool) {
        let visible = self.filtered_sessions();
        if visible.is_empty() {
            return;
        }
        let pos = visible.iter().position(|&i| i == self.left_sel).unwrap_or(0);
        let next = if down { pos + 1 } else { pos.saturating_sub(1) };
        if next >= visible.len() {
            return;
        }
        if visible[next] != self.left_sel {
            self.left_sel = visible[next];
            self.right_sel = 0;
            self.focus = Column::Left;
        }
    }

    /// Drain the focus + one-lookahead sockets whose column is still
    /// `NotFetched`, marking each `Loading` so the App does not re-spawn a
    /// worker for the same socket. Called by the App after each redraw.
    pub fn fetch_requests(&mut self) -> Vec<PathBuf> {
        let targets = prefetch_targets(self.left_sel, 1, self.sessions.len());
        let mut out = Vec::new();
        for i in targets {
            let Some(s) = self.sessions.get(i) else { continue };
            let socket = s.socket_path.clone();
            let needs = matches!(
                self.workspaces.get(&socket),
                None | Some(WorkspaceColumn::NotFetched)
            );
            if needs {
                self.workspaces.insert(socket.clone(), WorkspaceColumn::Loading);
                out.push(socket);
            }
        }
        out
    }

    /// Drain a pending reattach target after the App decided to quit.
    pub fn take_reattach_target(&mut self) -> Option<PathBuf> {
        self.pending_reattach.take()
    }

    /// Merge a worker result into the right-column map.
    pub fn set_workspaces(&mut self, socket: PathBuf, column: WorkspaceColumn) {
        self.workspaces.insert(socket, column);
    }
}

/// Index of the session whose socket == the running TUI's socket. `None`
/// when the running session is absent from the discovered set (e.g. a
/// `--socket` override pointing outside the runtime dir). Pure.
pub fn current_index(sessions: &[DiscoveredSession], own: &Path) -> Option<usize> {
    sessions.iter().position(|s| s.socket_path == own)
}

/// Substring filter predicate (case-insensitive) on the session name OR any
/// workspace title. An empty query passes everything. Pure, so the filter
/// is testable without sockets.
pub fn passes_filter(query: &str, session: &str, ws_titles: &[String]) -> bool {
    if query.is_empty() {
        return true;
    }
    let needle = query.to_lowercase();
    if session.to_lowercase().contains(&needle) {
        return true;
    }
    ws_titles.iter().any(|t| t.to_lowercase().contains(&needle))
}

/// Which row indices should be prefetched given the current focus and a
/// lookahead count (focus + the next `lookahead` rows), clamped to `len`
/// and de-duplicated. Pure.
pub fn prefetch_targets(left_sel: usize, lookahead: usize, len: usize) -> Vec<usize> {
    if left_sel >= len {
        return Vec::new();
    }
    (left_sel..(left_sel + lookahead + 1).min(len)).collect()
}

#[cfg(test)]
mod tests {
    //! Pure state-machine + filter + discovery tests (scout-plan T1-T9).
    //! These drive the logic the App relies on without any socket I/O or
    //! PTY harness. Committed RED (commit 4) before the GREEN impl (commit 5).
    use super::*;
    use std::time::SystemTime;

    fn mk_session(name: &str, socket: &str, live: bool) -> DiscoveredSession {
        DiscoveredSession {
            session: name.to_string(),
            socket_path: PathBuf::from(socket),
            pid: Some(1000),
            live,
            mtime: Some(SystemTime::UNIX_EPOCH),
        }
    }

    /// A minimal tree with `n` workspaces named w0..wN-1, for right-column
    /// fixtures.
    fn mk_tree(n: usize) -> TreeView {
        let workspaces = (0..n)
            .map(|i| crate::session::WorkspaceView {
                id: i as u64,
                short_id: format!("w{i}"),
                name: format!("w{i}"),
                color: None,
                icon: None,
                screens: Vec::new(),
                active_screen: 0,
            })
            .collect();
        TreeView { workspaces, active_workspace: 0 }
    }

    /// T1 — AC6: `current_index` tags the running session's row.
    #[test]
    fn t1_current_index_tags_running_session() {
        let sessions = vec![
            mk_session("alpha", "/run/a.sock", true),
            mk_session("beta", "/run/b.sock", true),
            mk_session("gamma", "/run/c.sock", true),
        ];
        assert_eq!(current_index(&sessions, Path::new("/run/b.sock")), Some(1));
        assert_eq!(current_index(&sessions, Path::new("/run/zzz.sock")), None);
    }

    /// T2 — AC4: `passes_filter` is a case-insensitive substring match on
    /// the session name OR any workspace title; empty query passes.
    #[test]
    fn t2_passes_filter_matches_session_or_workspace_title() {
        assert!(passes_filter("", "alpha", &[]), "empty query always passes");
        assert!(passes_filter("ALP", "alpha", &[]), "session name matches (case-insensitive)");
        assert!(!passes_filter("xyz", "alpha", &[]), "non-matching session rejects");
        assert!(
            passes_filter("build", "alpha", &["main".into(), "build-ci".into()]),
            "workspace title matches"
        );
        assert!(
            !passes_filter("nomatch", "alpha", &["main".into(), "build-ci".into()]),
            "non-matching workspace rejects"
        );
    }

    /// T3 — AC3: `prefetch_targets` returns focus + the next `lookahead`
    /// rows, clamped to the list length.
    #[test]
    fn t3_prefetch_targets_returns_focus_plus_lookahead() {
        assert_eq!(prefetch_targets(1, 1, 4), vec![1, 2]);
        // At the end, lookahead clamps away.
        assert_eq!(prefetch_targets(3, 1, 4), vec![3]);
        // Lookahead beyond the end is fully clamped.
        assert_eq!(prefetch_targets(0, 5, 2), vec![0, 1]);
    }

    /// T4 — AC4: `filtered_sessions` excludes rows that fail the filter.
    #[test]
    fn t4_filtered_sessions_excludes_non_matches() {
        let sessions = vec![
            mk_session("alpha", "/run/a.sock", true),
            mk_session("beta", "/run/b.sock", true),
            mk_session("gamma", "/run/c.sock", true),
        ];
        let mut state = SessionManagerState::new(PathBuf::from("/run/a.sock"), sessions);
        state.filter = Some(TextInput::new("gam".to_string()));
        assert_eq!(state.filtered_sessions(), vec![2], "only gamma matches 'gam'");
    }

    /// T5 — AC8: Enter on an `[unreachable]` row sets a status hint, it
    /// does not emit an attach.
    #[test]
    fn t5_unreachable_row_is_selectable_for_kill_but_not_attach() {
        let sessions = vec![
            mk_session("live", "/run/live.sock", true),
            mk_session("dead", "/run/dead.sock", false),
        ];
        let mut state = SessionManagerState::new(PathBuf::from("/run/live.sock"), sessions);
        state.left_sel = 1; // focus the dead row
        let action = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(action, SessionManagerAction::SetStatus(_)),
            "Enter on unreachable must SetStatus, got {action:?}"
        );
    }

    /// T6 — AC5/AC6: Enter on a workspace in the *current* session focuses
    /// it in place (no reattach).
    #[test]
    fn t6_enter_on_current_session_workspace_focuses_in_place() {
        let own = PathBuf::from("/run/own.sock");
        let sessions = vec![mk_session("own", "/run/own.sock", true)];
        let mut state = SessionManagerState::new(own, sessions);
        // Seed the current session's workspaces (the App does this for free).
        state.set_workspaces(PathBuf::from("/run/own.sock"), WorkspaceColumn::Ready(mk_tree(2)));
        // Move to the right column and pick workspace index 1.
        state.focus = Column::Right;
        state.right_sel = 1;
        let action = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(action, SessionManagerAction::FocusWorkspaceInPlace { index } if index == 1),
            "Enter on current-session workspace must focus in place, got {action:?}"
        );
    }

    /// T7 — AC5: Enter on a workspace in an *other* session emits a
    /// remote-focus + reattach action carrying the target socket + index.
    #[test]
    fn t7_enter_on_other_session_workspace_emits_reattach() {
        let sessions = vec![
            mk_session("own", "/run/own.sock", true),
            mk_session("other", "/run/other.sock", true),
        ];
        let mut state = SessionManagerState::new(PathBuf::from("/run/own.sock"), sessions);
        state.left_sel = 1; // focus the other session
        state.set_workspaces(
            PathBuf::from("/run/other.sock"),
            WorkspaceColumn::Ready(mk_tree(3)),
        );
        state.focus = Column::Right;
        state.right_sel = 2;
        let action = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                action,
                SessionManagerAction::AttachOtherSessionWorkspace { ref socket, index }
                    if *socket == Path::new("/run/other.sock") && index == 2
            ),
            "Enter on other-session workspace must emit reattach, got {action:?}"
        );
    }

    /// T8 — AC7: q / Esc close the overlay.
    #[test]
    fn t8_close_keys_q_esc_restore_focus() {
        let mut state =
            SessionManagerState::new(PathBuf::from("/run/own.sock"), vec![mk_session("own", "/run/own.sock", true)]);
        for key in [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        ] {
            let action = state.handle_key(key);
            assert!(matches!(action, SessionManagerAction::Close), "close key must Close, got {action:?}");
            // Re-open for the next iteration's sake.
            state = SessionManagerState::new(
                PathBuf::from("/run/own.sock"),
                vec![mk_session("own", "/run/own.sock", true)],
            );
        }
    }

    /// T9 — AC7: the rename flow emits a `RenameSession` action shaped for
    /// `cli::rename_session_at`. (The helper itself is L2-tested.)
    #[test]
    fn t9_rename_flow_reuses_rename_session_at_contract() {
        let sessions = vec![
            mk_session("own", "/run/own.sock", true),
            mk_session("live", "/run/live.sock", true),
        ];
        let mut state = SessionManagerState::new(PathBuf::from("/run/own.sock"), sessions);
        state.left_sel = 1; // focus the live session
        // `r` enters rename mode.
        let enter = state.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(enter, SessionManagerAction::Redraw), "r must enter rename mode (Redraw), got {enter:?}");
        // Rename pre-fills with the old name; clear it (Ctrl-U) then type the new.
        state.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        state.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        // Enter commits.
        let commit = state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                commit,
                SessionManagerAction::RenameSession { ref socket, ref new_name }
                    if *socket == Path::new("/run/live.sock") && new_name == "bar"
            ),
            "Enter in rename mode must commit RenameSession, got {commit:?}"
        );
    }
}
