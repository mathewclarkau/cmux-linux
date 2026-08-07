//! Interactive pre-attach session picker for `cmux attach --session-list`
//! (issue #63, layer L1).
//!
//! Runs BEFORE `run_attach`/`app::run`, so it must not touch `App` or
//! `Session` (those need an already-chosen socket). It is a self-contained
//! raw-mode loop mirroring the terminal lifecycle in `app.rs`:
//! `enable_raw_mode` → `EnterAlternateScreen` → a tiny ratatui
//! `Terminal<CrosstermBackend>` → `crossterm::event::read()` loop →
//! `restore_terminal` on EVERY exit path (Enter/q/Esc/Ctrl-C/panic). A
//! raw-mode or alt-screen leak would corrupt the subsequent `app::run` TUI
//! or the user's shell, so the panic hook (app.rs:674 pattern) is mandatory.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal as RatatuiTerminal;

use crate::cli::{self, DiscoveredSession, GlobalArgs};

/// What the picker decided. `main()` maps this to an exit code / attach.
pub(crate) enum PickerOutcome {
    /// Attach in-process: caller sets `socket`/`session` and falls through
    /// to the existing `run_attach` path (no fork/exec).
    Attach { socket_path: PathBuf, name: String },
    /// User quit (q/Esc). `destructive` is true if a session was killed
    /// during this run, so `main()` can exit 1 instead of 0 (Claim 3).
    Quit { destructive: bool },
    /// Ctrl-C.
    CtrlC,
}

/// Interactive modal picker. Restores the terminal on every return path
/// (including via the panic hook) before yielding control back to `main`,
/// so the subsequent `app::run` (on Attach) starts from a clean screen.
pub(crate) fn run(global: &GlobalArgs) -> anyhow::Result<PickerOutcome> {
    let stdout_lock = Arc::new(Mutex::new(()));
    enable_raw_mode()?;
    // Enter the alternate screen under the lock so restore_terminal can't
    // race a concurrent write (mirrors app.rs:644-651).
    if let Err(e) = (|| -> anyhow::Result<()> {
        let _guard = stdout_lock.lock().unwrap();
        io::stdout().execute(EnterAlternateScreen)?;
        Ok(())
    })() {
        let _ = restore_terminal(Some(&stdout_lock));
        return Err(e);
    }
    // Restore on panic so a mid-frame panic can't strand the terminal in
    // raw/alt-screen mode (mirrors app.rs:674-680).
    let default_hook = std::panic::take_hook();
    let restore_lock = stdout_lock.clone();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal(Some(&restore_lock));
        default_hook(info);
    }));

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match RatatuiTerminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            let _ = std::panic::take_hook();
            let _ = restore_terminal(Some(&stdout_lock));
            return Err(e.into());
        }
    };

    let outcome = event_loop(&mut terminal, global);

    // Tear down the panic hook and restore the terminal before returning so
    // app::run (Attach) or the shell (Quit/CtrlC) inherits a clean screen.
    let _ = std::panic::take_hook();
    restore_terminal(Some(&stdout_lock))?;
    outcome
}

/// Full-screen raw-mode input loop.
fn event_loop(
    terminal: &mut RatatuiTerminal<CrosstermBackend<io::Stdout>>,
    global: &GlobalArgs,
) -> anyhow::Result<PickerOutcome> {
    let sessions = refresh(global);
    let mut state = ListState::default();
    clamp_selection(&mut state, &sessions);
    // Claim 3 flips this to true after a kill so Quit exits 1.
    let destructive = false;
    let mut status = String::new();

    draw(terminal, &mut state, &sessions, &status)?;
    loop {
        // Poll with a short timeout so a future background refresh could
        // re-run discovery; for L1 we just re-loop and redraw on key/resize.
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(k) => k,
            Event::Resize(_, _) => {
                draw(terminal, &mut state, &sessions, &status)?;
                continue;
            }
            _ => continue,
        };
        match handle_key(&key, &mut state, &sessions, &mut status) {
            Handled::Quit => return Ok(PickerOutcome::Quit { destructive }),
            Handled::CtrlC => return Ok(PickerOutcome::CtrlC),
            Handled::Attach(socket_path, name) => {
                return Ok(PickerOutcome::Attach { socket_path, name });
            }
            Handled::Continue => {}
            Handled::Redraw => draw(terminal, &mut state, &sessions, &status)?,
        }
    }
}

/// Result of dispatching a single key. Claims 3-7 add Kill/KillStale/New/Rename.
enum Handled {
    Continue,
    Redraw,
    Quit,
    CtrlC,
    Attach(PathBuf, String),
}

/// Pure key dispatch: reads `state`/`sessions`, may mutate `state`/`status`.
/// Side effects needing `global`/`sessions` mutation (kill, new, refresh) are
/// returned as `Handled` variants for `event_loop` to apply.
fn handle_key(
    key: &KeyEvent,
    state: &mut ListState,
    sessions: &[DiscoveredSession],
    status: &mut String,
) -> Handled {
    let len = sessions.len();
    let selected = state.selected();
    match key.code {
        // Navigation. vim j/k matches USAGE "h/j/k/l or arrows move focus".
        KeyCode::Up | KeyCode::Char('k') => move_cursor(state, selected, len, false),
        KeyCode::Down | KeyCode::Char('j') => move_cursor(state, selected, len, true),
        KeyCode::Enter => {
            if let Some(i) = selected {
                if let Some(s) = sessions.get(i) {
                    if s.live {
                        return Handled::Attach(s.socket_path.clone(), s.session.clone());
                    }
                    // Q2 (approved): stale stays focusable but Enter does
                    // not attach — the socket isn't connectable.
                    *status = format!(
                        "{} is unreachable (socket not connectable) — cannot attach",
                        s.session
                    );
                    return Handled::Redraw;
                }
            }
            Handled::Continue
        }
        KeyCode::Esc | KeyCode::Char('q') => Handled::Quit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Handled::CtrlC,
        // Claims 3-7 add x (kill focused), s (kill stale), n (new), r (rename stub).
        _ => Handled::Continue,
    }
}

/// Apply an up/down move, returning Redraw iff the selection changed.
fn move_cursor(state: &mut ListState, selected: Option<usize>, len: usize, down: bool) -> Handled {
    if len == 0 {
        return Handled::Continue;
    }
    let i = selected.unwrap_or(0);
    let next = if down { i + 1 } else { i.saturating_sub(1) };
    if next >= len || next == i {
        Handled::Continue
    } else {
        state.select(Some(next));
        Handled::Redraw
    }
}

/// Re-run discovery, newest-first (mtime DESC; `None` sorts last), for the
/// picker's "most recent session on top" ordering.
fn refresh(global: &GlobalArgs) -> Vec<DiscoveredSession> {
    let mut sessions = cli::discover_sessions(global);
    sessions.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    sessions
}

fn clamp_selection(state: &mut ListState, sessions: &[DiscoveredSession]) {
    if sessions.is_empty() {
        state.select(None);
    } else if state.selected().map_or(true, |i| i >= sessions.len()) {
        state.select(Some(0));
    }
}

fn draw(
    terminal: &mut RatatuiTerminal<CrosstermBackend<io::Stdout>>,
    state: &mut ListState,
    sessions: &[DiscoveredSession],
    status: &str,
) -> io::Result<()> {
    terminal.draw(|f| {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(2)])
            .split(f.area());

        let items: Vec<ListItem> = sessions
            .iter()
            .map(|s| {
                let (label, style) = if s.live {
                    (s.session.clone(), Style::default())
                } else {
                    (
                        format!("[unreachable] {}", s.session),
                        Style::default().fg(Color::DarkGray),
                    )
                };
                ListItem::new(Line::from(Span::styled(label, style)))
            })
            .collect();
        let title = if sessions.is_empty() {
            " cmux sessions — none found (q to quit) "
        } else {
            " cmux sessions — pick one to attach "
        };
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, chunks[0], state);

        // Footer: keymap hints on row 1, transient status (if any) on row 2.
        let hints = "↑/↓ or j/k move · Enter attach · q/Esc quit · Ctrl-C abort";
        let mut lines = vec![Line::from(hints)];
        if !status.is_empty() {
            lines.push(Line::from(Span::styled(
                status,
                Style::default().fg(Color::Yellow),
            )));
        }
        f.render_widget(Paragraph::new(Text::from(lines)), chunks[1]);
    })?;
    Ok(())
}

/// Mirror of app.rs:740 restore_terminal: leave the alternate screen and
/// disable raw mode. Best-effort on the screen switch so one failure can't
/// strand the terminal in raw mode.
fn restore_terminal(stdout_lock: Option<&Arc<Mutex<()>>>) -> anyhow::Result<()> {
    let _guard = stdout_lock.map(|lock| lock.lock().unwrap());
    let _ = io::stdout().execute(LeaveAlternateScreen);
    disable_raw_mode()?;
    Ok(())
}
