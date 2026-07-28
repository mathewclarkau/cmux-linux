//! Fuzzy finder overlay (`leader G`): type-ahead search across
//! workspace names, pane names, surface titles, and agent session ids,
//! with a state filter (`B`/`W`/`I`/`D`/`A`).
//!
//! The finder is a pure presentation layer over the existing
//! [`TreeView`](crate::session::TreeView) snapshot, so it needs no new
//! protocol verbs. `build_items` walks the tree in display order; the
//! matcher is a small subsequence scorer written from scratch (no new
//! crate, keeping the Cargo.lock at version 3).

use mux_core::{AgentState, PaneId, Rect, SurfaceId, WorkspaceId};
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::session::TreeView;
use crate::ui::input::TextInput;
use crate::ui::truncate;

/// Which backing tree node a finder row points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinderTarget {
    Workspace(WorkspaceId),
    Pane(PaneId),
    Surface(SurfaceId),
}

/// Agent-state filter. `All` shows every row; the others exclude both
/// non-matching states and rows with no state at all (workspaces and
/// panes have no agent state, only surfaces do).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    All,
    Working,
    Blocked,
    Idle,
    Done,
}

impl StateFilter {
    /// Keep a row whose `agent_state` matches this filter.
    fn keeps(self, agent_state: Option<AgentState>) -> bool {
        match self {
            StateFilter::All => true,
            StateFilter::Working => agent_state == Some(AgentState::Working),
            StateFilter::Blocked => agent_state == Some(AgentState::Blocked),
            StateFilter::Idle => agent_state == Some(AgentState::Idle),
            StateFilter::Done => agent_state == Some(AgentState::Done),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StateFilter::All => "all",
            StateFilter::Working => "working",
            StateFilter::Blocked => "blocked",
            StateFilter::Idle => "idle",
            StateFilter::Done => "done",
        }
    }
}

/// One searchable row. `agent_state` is `None` for workspaces and panes
/// (they have no agent report) and `Some` for surfaces, sourced from the
/// tab's reported state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinderItem {
    pub target: FinderTarget,
    pub label: String,
    /// The agent session id reported on this row's surface, if any.
    /// Workspaces and panes carry no session; surfaces carry the same
    /// `session` string `list-agents` reports, so type-ahead matches it.
    pub agent_session: Option<String>,
    pub agent_state: Option<AgentState>,
}

/// Overlay state: the query input, the active state filter, the
/// selection cursor, and the item list (built once on open and refreshed
/// while the user types).
#[derive(Debug, Clone)]
pub struct FinderState {
    pub input: TextInput,
    pub state_filter: StateFilter,
    pub cursor: usize,
    pub items: Vec<FinderItem>,
}

impl FinderState {
    pub fn new(items: Vec<FinderItem>) -> Self {
        FinderState { input: TextInput::new(String::new()), state_filter: StateFilter::All, cursor: 0, items }
    }

    /// Rows that pass the state filter and match the current query, kept
    /// in tree order when the query is empty. Returns the index into
    /// `self.items` plus the match score (lower is better).
    pub fn ranked(&self) -> Vec<(usize, u32)> {
        let query = self.input.as_str();
        let mut out: Vec<(usize, u32)> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            if !self.state_filter.keeps(item.agent_state) {
                continue;
            }
            // A row matches if the query is a subsequence of its label OR
            // of its agent session id (when present). The label still
            // drives display; the session is purely extra searchable text
            // so type-ahead covers agent session ids per AC2.
            let score = fuzzy_score(query, &item.label).or_else(|| {
                item.agent_session
                    .as_deref()
                    .and_then(|s| fuzzy_score(query, s))
            });
            if let Some(score) = score {
                out.push((i, score));
            }
        }
        // Stable sort by score (lower is better); empty query gives every
        // row score 0, so the original tree order is preserved.
        out.sort_by_key(|(_, score)| *score);
        out
    }

    /// Move the cursor, clamped to the ranked list length.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.ranked().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let next = (self.cursor as isize + delta).rem_euclid(len as isize) as usize;
        self.cursor = next.min(len - 1);
    }

    /// The selected row's target, if any.
    pub fn selected(&self) -> Option<FinderTarget> {
        let ranked = self.ranked();
        let entry = ranked.get(self.cursor)?;
        Some(self.items[entry.0].target)
    }
}

/// Build the finder item list from a tree snapshot, walking workspaces,
/// screens, panes, then tabs in display order. Pure: identical input
/// trees produce identical item lists.
pub fn build_items(tree: &TreeView) -> Vec<FinderItem> {
    let mut items = Vec::new();
    for ws in &tree.workspaces {
        items.push(FinderItem {
            target: FinderTarget::Workspace(ws.id),
            label: format!("#{} {}", ws.short_id, ws.name),
            agent_session: None,
            agent_state: None,
        });
        for screen in &ws.screens {
            for pane in &screen.panes {
                items.push(FinderItem {
                    target: FinderTarget::Pane(pane.id),
                    label: format!(
                        "#{} {}",
                        pane.short_id,
                        pane.name.as_deref().unwrap_or(pane.display_name())
                    ),
                    agent_session: None,
                    agent_state: None,
                });
                for tab in &pane.tabs {
                    let title = tab.name.as_deref().filter(|s| !s.is_empty()).unwrap_or(&tab.title);
                    items.push(FinderItem {
                        target: FinderTarget::Surface(tab.surface),
                        label: format!("#{} {}", tab.short_id, title),
                        agent_session: tab.agent_session.clone(),
                        agent_state: tab.agent_state,
                    });
                }
            }
        }
    }
    items
}

/// Subsequence fuzzy match. Returns `None` when `query` is not a
/// subsequence of `hay` (case-insensitive), else `Some(score)` where a
/// lower score is a tighter match (consecutive matches score 0 extra;
/// each gap adds its length). An empty query matches everything with
/// score 0 so the caller keeps the tree order intact.
pub fn fuzzy_score(query: &str, hay: &str) -> Option<u32> {
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = hay.chars().collect();
    let mut qi = 0usize;
    let mut score: u32 = 0;
    let mut prev: i64 = -1;
    for (hi, hc) in hay.iter().enumerate() {
        if qi >= query.len() {
            break;
        }
        if hc.eq_ignore_ascii_case(&query[qi]) {
            if prev >= 0 {
                let gap = (hi as i64 - prev) as u32;
                score = score.saturating_add(gap.saturating_sub(1));
            }
            prev = hi as i64;
            qi += 1;
        }
    }
    if qi == query.len() { Some(score) } else { None }
}

/// The centered bordered-box rectangle the finder overlay occupies,
/// matching [`draw`]'s geometry. Pure: identical screen sizes produce
/// identical rects, so the draw path and the click hit-test path share
/// one source of truth. Returns `None` when the screen is too small.
pub fn finder_rect(screen: Rect) -> Option<Rect> {
    let width = 60u16.min(screen.width.saturating_sub(2)).max(30);
    let height = 14u16.min(screen.height.saturating_sub(2)).max(4);
    if screen.width < width || screen.height < height {
        return None;
    }
    Some(Rect {
        x: (screen.width - width) / 2,
        y: (screen.height - height) / 2,
        width,
        height,
    })
}

/// Y offset of the first results row inside the overlay box. Kept
/// alongside [`finder_rect`] so the click hit-test path maps a
/// screen-space point to a results row with the same arithmetic the
/// draw path uses.
const ROWS_TOP_OFFSET: u16 = 4;

/// Map a screen-space click to the 0-based results-row index it lands
/// on, or `None` when the click is outside the overlay box or in its
/// title/query/filter chrome (anything above the results region).
pub fn finder_row_at(screen: Rect, x: u16, y: u16) -> Option<usize> {
    let rect = finder_rect(screen)?;
    if !rect.contains(x, y) {
        return None;
    }
    let rows_y = rect.y + ROWS_TOP_OFFSET;
    let rows_h = rect.height.saturating_sub((ROWS_TOP_OFFSET + 1) as u16);
    if y < rows_y || y >= rows_y + rows_h {
        return None;
    }
    Some((y - rows_y) as usize)
}

/// Draw the finder overlay: a bordered box centered on the frame with
/// the query input on the top row, the state filter on the right of it,
/// and the ranked rows below. The finder owns the terminal cursor while
/// open. Best-effort geometry: if the screen is too small, it draws
/// nothing rather than panicking.
pub fn draw(app: &mut crate::app::App, frame: &mut Frame) {
    let ratatui_screen = frame.area();
    let screen = Rect {
        x: ratatui_screen.x,
        y: ratatui_screen.y,
        width: ratatui_screen.width,
        height: ratatui_screen.height,
    };
    let Some(rect) = finder_rect(screen) else { return };
    let Some(finder) = app.finder.as_mut() else { return };
    let x = rect.x;
    let width = rect.width;
    let height = rect.height;
    let y = rect.y;
    let base = Style::default().bg(Color::Indexed(236)).fg(Color::Indexed(252));
    let border = base.fg(Color::Indexed(244));
    let title = base.fg(Color::Indexed(255)).add_modifier(Modifier::BOLD);
    let input_style = Style::default().bg(Color::Indexed(233)).fg(Color::Indexed(255));
    let selected =
        Style::default().bg(Color::Indexed(242)).fg(Color::Indexed(255)).add_modifier(Modifier::BOLD);
    let filter_label = format!("[{}: B W I D A]", finder.state_filter.label());
    let fw = filter_label.chars().count() as u16;
    let fx = x + width.saturating_sub(fw + 2);
    let input_w = width.saturating_sub(4);
    let (shown, cursor_col) = finder.input.visible_text_and_cursor(input_w as usize);
    let cursor_x = x + 2 + (cursor_col as u16).min(input_w);
    let ranked = finder.ranked();
    let rows_y = y + ROWS_TOP_OFFSET;
    let rows_h = height.saturating_sub((ROWS_TOP_OFFSET + 1) as u16);

    // Background, border, title, filter label, and query input row. Drawn
    // in a scope so the buffer borrow ends before we move the terminal
    // cursor below.
    {
        let buf = frame.buffer_mut();
        for dy in 0..height {
            for dx in 0..width {
                set_cell(buf, x + dx, y + dy, " ", base);
            }
        }
        draw_border(buf, x, y, width, height, border);
        buf.set_stringn(x + 2, y + 1, "Find", 4, title);
        buf.set_stringn(fx, y + 1, &filter_label, fw as usize, base.fg(Color::Indexed(244)));
        for dx in 0..input_w {
            set_cell(buf, x + 2 + dx, y + 2, " ", input_style);
        }
        buf.set_stringn(x + 2, y + 2, &shown, input_w as usize, input_style);
    }
    frame.set_cursor_position(Position::new(cursor_x, y + 2));

    // Ranked rows beneath the query input.
    {
        let buf = frame.buffer_mut();
        for (row, &(item_i, _)) in ranked.iter().enumerate().take(rows_h as usize) {
            let row_y = rows_y + row as u16;
            let item = &finder.items[item_i];
            let style = if row == finder.cursor { selected } else { base };
            let label = truncate(&item.label, (width as usize).saturating_sub(4));
            let prefix = match item.agent_state {
                Some(AgentState::Working) => "W",
                Some(AgentState::Blocked) => "B",
                Some(AgentState::Idle) => "I",
                Some(AgentState::Done) => "D",
                Some(AgentState::Unknown) => "?",
                None => " ",
            };
            for dx in 0..width.saturating_sub(2) {
                set_cell(buf, x + 1 + dx, row_y, " ", style);
            }
            buf.set_stringn(x + 1, row_y, prefix, 1, style.fg(Color::Indexed(244)));
            buf.set_stringn(x + 3, row_y, &label, (width as usize).saturating_sub(4), style);
        }
    }
}

fn set_cell(buf: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    let cell = &mut buf[(x, y)];
    cell.reset();
    cell.set_symbol(symbol).set_style(style);
}

fn draw_border(buf: &mut Buffer, x: u16, y: u16, w: u16, h: u16, style: Style) {
    if w < 2 || h < 2 {
        return;
    }
    let x0 = x;
    let y0 = y;
    let x1 = x + w - 1;
    let y1 = y + h - 1;
    for col in x0 + 1..x1 {
        set_cell(buf, col, y0, "-", style);
        set_cell(buf, col, y1, "-", style);
    }
    for row in y0 + 1..y1 {
        set_cell(buf, x0, row, "|", style);
        set_cell(buf, x1, row, "|", style);
    }
    set_cell(buf, x0, y0, "+", style);
    set_cell(buf, x1, y0, "+", style);
    set_cell(buf, x0, y1, "+", style);
    set_cell(buf, x1, y1, "+", style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::AgentState;

    fn item(label: &str, state: Option<AgentState>, target: FinderTarget) -> FinderItem {
        FinderItem { target, label: label.to_string(), agent_session: None, agent_state: state }
    }

    /// Same as [`item`] but with an agent session id attached, mirroring a
    /// surface row built from a tab that carries an agent report.
    fn item_with_session(
        label: &str,
        state: Option<AgentState>,
        target: FinderTarget,
        session: &str,
    ) -> FinderItem {
        FinderItem {
            target,
            label: label.to_string(),
            agent_session: Some(session.to_string()),
            agent_state: state,
        }
    }

    /// A throwaway target id; only the label and state matter for the
    /// matcher and filter tests.
    fn surf(id: u64) -> FinderTarget {
        FinderTarget::Surface(id)
    }

    #[test]
    fn fuzzy_score_none_for_no_subsequence() {
        // No 'b' anywhere in "claude-reviewer", so "cb" cannot match.
        assert_eq!(fuzzy_score("cb", "claude-reviewer"), None);
        // A present but non-matching subsequence: 'x' not in haystack.
        assert_eq!(fuzzy_score("x", "claude-builder"), None);
        // Matching subsequence returns Some.
        assert!(fuzzy_score("cb", "claude-builder").is_some());
        // Empty query always matches.
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("", ""), Some(0));
    }

    #[test]
    fn ranks_cb_builder_before_reviewer() {
        let items = vec![
            item("claude-builder", None, surf(1)),
            item("claude-reviewer", None, surf(2)),
        ];
        let mut finder = FinderState::new(items);
        finder.input = TextInput::new("cb".to_string());
        let ranked = finder.ranked();
        // The reviewer has no 'b', so it is excluded entirely; the builder
        // is the only match and appears first.
        assert_eq!(ranked.len(), 1);
        assert_eq!(finder.items[ranked[0].0].label, "claude-builder");
    }

    #[test]
    fn state_filter_excludes_nonmatching() {
        let items = vec![
            item("working-agent", Some(AgentState::Working), surf(1)),
            item("blocked-agent", Some(AgentState::Blocked), surf(2)),
            item("idle-agent", Some(AgentState::Idle), surf(3)),
            item("done-agent", Some(AgentState::Done), surf(4)),
            item("workspace-one", None, FinderTarget::Workspace(1)),
            item("pane-one", None, FinderTarget::Pane(1)),
        ];
        // Working filter: only the working agent, no stateless rows.
        let mut working = FinderState::new(items.clone());
        working.state_filter = StateFilter::Working;
        let ranked = working.ranked();
        assert_eq!(ranked.len(), 1);
        assert_eq!(working.items[ranked[0].0].label, "working-agent");

        // Blocked filter likewise.
        let mut blocked = FinderState::new(items.clone());
        blocked.state_filter = StateFilter::Blocked;
        let ranked = blocked.ranked();
        assert_eq!(ranked.len(), 1);
        assert_eq!(blocked.items[ranked[0].0].label, "blocked-agent");

        // All filter keeps every row.
        let mut all = FinderState::new(items.clone());
        all.state_filter = StateFilter::All;
        assert_eq!(all.ranked().len(), items.len());
    }

    #[test]
    fn empty_query_keeps_all_in_tree_order() {
        let items = vec![
            item("alpha", None, surf(1)),
            item("beta", None, surf(2)),
            item("gamma", None, surf(3)),
        ];
        let finder = FinderState::new(items.clone());
        let ranked = finder.ranked();
        // Every row present, in the original (tree) order.
        assert_eq!(ranked.len(), items.len());
        for (i, (item_i, _)) in ranked.iter().enumerate() {
            assert_eq!(*item_i, i, "tree order not preserved at row {i}");
        }
    }

    #[test]
    fn agent_session_id_is_searchable() {
        // Two surfaces whose labels share no letters with the session id;
        // only the agent session id can match the typed query.
        let items = vec![
            item_with_session(
                "shell",
                Some(AgentState::Working),
                surf(1),
                "agent-7f3a-session",
            ),
            item_with_session(
                "shell",
                Some(AgentState::Idle),
                surf(2),
                "agent-7f3b-session",
            ),
        ];
        // A query fragment present only in the session id surfaces the
        // matching agent row.
        let mut finder = FinderState::new(items.clone());
        finder.input = TextInput::new("7f3b".to_string());
        let ranked = finder.ranked();
        assert_eq!(ranked.len(), 1, "only the surface whose session id contains the query matches");
        assert_eq!(finder.items[ranked[0].0].agent_session.as_deref(), Some("agent-7f3b-session"));
        assert_eq!(finder.items[ranked[0].0].target, surf(2));

        // An empty query still surfaces every row (session id or not).
        finder.input = TextInput::new(String::new());
        assert_eq!(finder.ranked().len(), items.len());
    }

    #[test]
    fn finder_rect_is_centered_and_too_small_yields_none() {
        // On a 100x40 screen the box is 60x14, centred by the screen's own
        // width/height (the overlay positions against the terminal origin,
        // matching the draw path, so `screen.x` does not shift it).
        let screen = Rect { x: 10, y: 0, width: 100, height: 40 };
        let rect = finder_rect(screen).unwrap();
        assert_eq!(rect.width, 60);
        assert_eq!(rect.height, 14);
        assert_eq!(rect.x, (100 - 60) / 2);
        assert_eq!(rect.y, (40 - 14) / 2);

        // A screen just under the minimum row height has no overlay.
        let tiny = Rect { x: 0, y: 0, width: 100, height: 3 };
        assert!(finder_rect(tiny).is_none());
    }

    #[test]
    fn finder_row_at_maps_click_to_results_row() {
        // 100x40 screen: overlay box sits at x=20, y=13, 60x14.
        let screen = Rect { x: 10, y: 0, width: 100, height: 40 };
        let rect = finder_rect(screen).unwrap();
        let rows_y = rect.y + ROWS_TOP_OFFSET;

        // Clicking on the title row (y == rect.y + 1) does NOT select a
        // results row: it is chrome, not a results row.
        assert_eq!(finder_row_at(screen, rect.x + 5, rect.y + 1), None);
        // Clicking on the query input row (y == rect.y + 2) likewise.
        assert_eq!(finder_row_at(screen, rect.x + 5, rect.y + 2), None);
        // The first results row maps to index 0.
        assert_eq!(finder_row_at(screen, rect.x + 5, rows_y), Some(0));
        // The second results row maps to index 1.
        assert_eq!(finder_row_at(screen, rect.x + 5, rows_y + 1), Some(1));
        // A click outside the box entirely selects nothing.
        assert_eq!(finder_row_at(screen, 0, 0), None);
        // A click on the bottom border row (rect.y + height - 1) is past
        // the results region (the last row is reserved for the border) and
        // so maps to no row.
        let bottom_border = rect.y + rect.height - 1;
        assert_eq!(finder_row_at(screen, rect.x + 5, bottom_border), None);
    }
}