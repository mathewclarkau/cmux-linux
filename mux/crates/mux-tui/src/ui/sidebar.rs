//! Left sidebar: a "workspaces" header (with a blank line under it),
//! then two lines per workspace (name, then an agent-state dot + git
//! branch + the active pane's title) with a blank line between
//! workspaces, and a new-workspace row at the end. Uses the terminal's
//! default background so it blends with pane content; only the active
//! workspace rows get a highlight. Owns its full column including the
//! status-bar row (the status bar starts after the sidebar). Rebuilds
//! the click hit map as it draws.

use std::time::Instant;

use mux_core::{AgentState, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use super::truncate;
use crate::app::{flash_active, App, Hit};

/// Color for the sidebar's agent-state dot. `Working`/`Blocked` are the
/// two states worth a glance (busy vs. needs you); `Idle`/`Done`/`Unknown`
/// share a quiet color since none of them need attention.
fn agent_glyph_color(state: AgentState) -> Color {
    match state {
        AgentState::Working => Color::Yellow,
        AgentState::Blocked => Color::Red,
        AgentState::Done => Color::Green,
        AgentState::Idle | AgentState::Unknown => Color::Indexed(242),
    }
}

/// The sidebar rail glyph's color for a workspace, if it should be drawn
/// at all. A user-assigned workspace color always wins (shown whether or
/// not the workspace is active); otherwise the rail only shows, in the
/// theme's color, when the workspace is active.
fn rail_color(workspace_color: Option<Color>, active: bool, theme_rail: Color) -> Option<Color> {
    workspace_color.or(if active { Some(theme_rail) } else { None })
}

fn active_row_background(workspace_color: Option<Color>, theme_background: Color) -> Color {
    workspace_color.unwrap_or(theme_background)
}

/// Bright, fixed pulse color for a manual flash: deliberately ignores
/// the workspace's own color/active state so it reads as "look here"
/// regardless of context. Layered on top of `rail_color`'s result, not a
/// replacement for it.
const FLASH_COLOR: Color = Color::White;

pub fn draw(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    let width = app.sidebar_width;
    let height = area.height;
    if width < 3 || height == 0 {
        return;
    }
    let content_w = (width - 1) as usize; // last column is the border
    let rail = app.config.theme.sidebar_rail;
    let workspace_drag = app.workspace_drag();
    let buf = frame.buffer_mut();

    let now = Instant::now();
    let base = Style::default();
    let dim = base.fg(Color::Indexed(242));
    let border = base.fg(Color::Indexed(237));

    for y in 0..height {
        for x in 0..width - 1 {
            buf[(x, y)].set_symbol(" ").set_style(base);
        }
        buf[(width - 1, y)].set_symbol("│").set_style(border);
    }

    let set_line = |buf: &mut ratatui::buffer::Buffer, y: u16, text: &str, style: Style| {
        buf.set_stringn(0, y, text, content_w, style);
    };
    let set_line_from =
        |buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, text: &str, style: Style| {
            buf.set_stringn(x, y, text, content_w.saturating_sub(x as usize), style);
        };
    let row_rect = |y: u16| Rect { x: 0, y, width: width.saturating_sub(1), height: 1 };

    set_line(buf, 0, " workspaces", dim);

    // Header, a blank line, then per workspace: two reserved lines (name
    // + active pane title) and one blank separator line.
    let mut hits = Vec::new();
    let mut y: u16 = 2;
    for (i, ws) in app.tree.workspaces.iter().enumerate() {
        if y + 1 >= height {
            break;
        }
        let active = i == app.tree.active_workspace;
        let active_style = Style::default()
            .bg(active_row_background(ws.color, app.config.theme.sidebar_active_bg))
            .fg(Color::Indexed(255))
            .add_modifier(Modifier::BOLD);
        let flashing =
            app.flashing.get(&ws.id).is_some_and(|start| flash_active(now.duration_since(*start)));
        let mut style = if active { active_style } else { base };
        if workspace_drag.is_some_and(|(id, _)| id == ws.id) {
            style = style.add_modifier(Modifier::DIM);
        }
        // The active highlight paints the full rows. The rail marks BOTH
        // lines of the entry: in the workspace's own color if it has one
        // set (active or not), else in the theme color while active.
        if active {
            for x in 0..width - 1 {
                buf[(x, y)].set_style(active_style);
                buf[(x, y + 1)].set_style(active_style);
            }
        }
        let rail_pulse =
            if flashing { Some(FLASH_COLOR) } else { rail_color(ws.color, active, rail) };
        if let Some(color) = rail_pulse {
            let rail_style = (if active { active_style } else { base }).fg(color);
            buf[(0, y)].set_symbol("▎").set_style(rail_style);
            buf[(0, y + 1)].set_symbol("▎").set_style(rail_style);
        }
        let label = match ws.icon.as_deref() {
            Some(icon) => format!("{icon} {}", ws.name),
            None => ws.name.clone(),
        };
        set_line_from(buf, 1, y, &truncate(&label, content_w - 1), style);
        hits.push((row_rect(y), Hit::Workspace { index: i, id: ws.id }));

        let screen = ws.active_screen_ref();
        let pane = screen.and_then(|s| s.pane(s.active_pane));
        let title = pane.map(|p| p.display_name()).unwrap_or("shell");
        let branch = pane.and_then(|p| p.active_cwd()).and_then(|cwd| app.git_info.branch_for(cwd));
        let prefix = branch.map(|b| format!("{b} · ")).unwrap_or_default();
        let prefix_w = prefix.chars().count();
        let screen_count = ws.screens.len();
        let subtitle = if screen_count > 1 {
            format!(
                " {prefix}{} ({screen_count} screens)",
                truncate(title, content_w.saturating_sub(13 + prefix_w))
            )
        } else {
            format!(" {prefix}{}", truncate(title, content_w.saturating_sub(3 + prefix_w)))
        };
        let sub_style = if active { active_style.add_modifier(Modifier::DIM) } else { dim };
        let agent_state = pane.and_then(|p| p.active_agent_state());
        if let Some(state) = agent_state {
            let glyph_style = sub_style.fg(agent_glyph_color(state)).remove_modifier(Modifier::DIM);
            buf[(1, y + 1)].set_symbol("●").set_style(glyph_style);
        }
        set_line_from(buf, 2, y + 1, subtitle.trim_start(), sub_style);
        hits.push((row_rect(y + 1), Hit::Workspace { index: i, id: ws.id }));
        y += 3; // two content lines + one blank separator line
    }

    if let Some((_, Some(index))) = workspace_drag {
        let marker_y = 2u16.saturating_add(index as u16 * 3).saturating_sub(1);
        if marker_y < height {
            for x in 0..width - 1 {
                buf[(x, marker_y)]
                    .set_symbol("─")
                    .set_style(Style::default().fg(app.config.theme.border_active));
            }
        }
    }

    if y < height {
        set_line(buf, y, " + new workspace", dim);
        hits.push((row_rect(y), Hit::NewWorkspace));
    }
    hits.push((Rect { x: width - 1, y: 0, width: 1, height }, Hit::SidebarResize));
    app.hits.extend(hits);
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEME_RAIL: Color = Color::Indexed(1);

    #[test]
    fn active_row_background_prefers_workspace_color() {
        assert_eq!(active_row_background(Some(Color::Blue), Color::Black), Color::Blue);
        assert_eq!(active_row_background(None, Color::Black), Color::Black);
    }

    #[test]
    fn rail_color_prefers_workspace_color_whether_active_or_not() {
        assert_eq!(rail_color(Some(Color::Red), false, THEME_RAIL), Some(Color::Red));
        assert_eq!(rail_color(Some(Color::Red), true, THEME_RAIL), Some(Color::Red));
    }

    #[test]
    fn rail_color_falls_back_to_theme_color_when_active_without_workspace_color() {
        assert_eq!(rail_color(None, true, THEME_RAIL), Some(THEME_RAIL));
    }

    #[test]
    fn rail_color_hidden_when_inactive_without_workspace_color() {
        assert_eq!(rail_color(None, false, THEME_RAIL), None);
    }
}
