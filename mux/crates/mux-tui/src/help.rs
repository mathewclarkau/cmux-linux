//! Floating, scrollable help overlay generated from the resolved key registry.

use mux_core::Rect;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::config::{Action, Keys};
use crate::finder::fuzzy_score;
use crate::ui::input::TextInput;
use crate::ui::truncate;

pub use crate::config::HelpCategory;

pub const ROWS_TOP_OFFSET: u16 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpEntry {
    pub chord: String,
    pub action: Action,
    pub description: &'static str,
    pub category: HelpCategory,
}

#[derive(Debug, Clone)]
pub struct HelpState {
    pub input: TextInput,
    pub cursor: usize,
    pub scroll: usize,
    pub items: Vec<HelpEntry>,
}

impl HelpState {
    pub fn new(items: Vec<HelpEntry>) -> Self {
        HelpState { input: TextInput::new(String::new()), cursor: 0, scroll: 0, items }
    }

    pub fn rows_visible(screen: Rect) -> usize {
        crate::help::rows_visible(screen)
    }

    /// Return matching item indices in the stable category order supplied by
    /// `build_entries`. A query matches the action name, config key,
    /// description, or chord.
    pub fn ranked(&self) -> Vec<(usize, u32)> {
        let query = self.input.as_str();
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let score = fuzzy_score(query, entry.action.display_name())
                    .or_else(|| fuzzy_score(query, entry.action.config_key()))
                    .or_else(|| fuzzy_score(query, entry.description))
                    .or_else(|| fuzzy_score(query, &entry.chord))?;
                Some((index, score))
            })
            .collect()
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.input = TextInput::new(query.into());
        self.cursor = 0;
        self.scroll = 0;
    }

    /// Move the selected result by a clamped number of rows.
    pub fn move_cursor(&mut self, delta: isize, rows_visible: usize) {
        let len = self.ranked().len();
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let magnitude = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        let next = if delta < 0 {
            self.cursor.saturating_sub(magnitude)
        } else {
            self.cursor.saturating_add(magnitude)
        };
        self.cursor = next.min(len - 1);
        self.clamp(rows_visible);
    }

    /// Scroll the viewport and keep the selected row visible.
    pub fn scroll_by(&mut self, delta: isize, rows_visible: usize) {
        let len = self.ranked().len();
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        let visible = rows_visible.max(1).min(len);
        let max_scroll = len - visible;
        let magnitude = delta.checked_abs().unwrap_or(isize::MAX) as usize;
        self.scroll = if delta < 0 {
            self.scroll.saturating_sub(magnitude)
        } else {
            self.scroll.saturating_add(magnitude).min(max_scroll)
        };
        if self.cursor < self.scroll {
            self.cursor = self.scroll;
        }
        let last_visible = self.scroll + visible - 1;
        if self.cursor > last_visible {
            self.cursor = last_visible;
        }
    }

    /// Clamp cursor and scroll after filtering or a terminal resize.
    pub fn clamp(&mut self, rows_visible: usize) {
        let len = self.ranked().len();
        if len == 0 {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = self.cursor.min(len - 1);
        let visible = rows_visible.max(1).min(len);
        let max_scroll = len - visible;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + visible {
            self.scroll = self.cursor + 1 - visible;
        }
        self.scroll = self.scroll.min(max_scroll);
    }
}

/// Build one help row for every resolved binding, including alternate chords.
pub fn build_entries(keys: &Keys) -> Vec<HelpEntry> {
    let mut entries: Vec<HelpEntry> = keys
        .bindings()
        .iter()
        .map(|(chord, action)| HelpEntry {
            chord: chord.display(),
            action: *action,
            description: action.description(),
            category: action.category(),
        })
        .collect();
    entries.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| left.chord.cmp(&right.chord))
            .then_with(|| left.action.display_name().cmp(right.action.display_name()))
    });
    entries
}

/// Return the centred help pane rectangle, or no rectangle when it cannot fit.
pub fn help_rect(screen: Rect) -> Option<Rect> {
    const MIN_WIDTH: u16 = 60;
    const MIN_HEIGHT: u16 = 14;
    if screen.width < MIN_WIDTH || screen.height < MIN_HEIGHT {
        return None;
    }
    let width = 90.min(screen.width);
    let height = 30.min(screen.height);
    Some(Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    })
}

pub fn rows_visible(screen: Rect) -> usize {
    help_rect(screen)
        .map(|rect| rect.height.saturating_sub(ROWS_TOP_OFFSET + 1) as usize)
        .unwrap_or(0)
}

/// Convert a click in the results region to a ranked row index.
pub fn help_row_at(screen: Rect, x: u16, y: u16, scroll: usize) -> Option<usize> {
    let rect = help_rect(screen)?;
    if !rect.contains(x, y) {
        return None;
    }
    let rows_y = rect.y + ROWS_TOP_OFFSET;
    let rows_h = rect.height.saturating_sub(ROWS_TOP_OFFSET + 1);
    if y < rows_y || y >= rows_y.saturating_add(rows_h) {
        return None;
    }
    Some(scroll + (y - rows_y) as usize)
}

/// Draw the help overlay without touching session state. Small terminals
/// simply keep the underlying frame visible rather than panicking.
pub fn draw(app: &mut crate::app::App, frame: &mut Frame) {
    let area = frame.area();
    let screen = Rect { x: area.x, y: area.y, width: area.width, height: area.height };
    let Some(rect) = help_rect(screen) else { return };
    let Some(help) = app.help.as_mut() else { return };

    let x = rect.x;
    let y = rect.y;
    let width = rect.width;
    let height = rect.height;
    let visible = rows_visible(screen);
    help.clamp(visible);
    let ranked = help.ranked();

    let base = Style::default().bg(Color::Indexed(236)).fg(Color::Indexed(252));
    let border = base.fg(Color::Indexed(244));
    let title = base.fg(Color::Indexed(255)).add_modifier(Modifier::BOLD);
    let input_style = Style::default().bg(Color::Indexed(233)).fg(Color::Indexed(255));
    let selected = Style::default()
        .bg(Color::Indexed(242))
        .fg(Color::Indexed(255))
        .add_modifier(Modifier::BOLD);
    let input_w = width.saturating_sub(4);
    let input_label = "Filter: ";
    let input_label_w = input_label.chars().count() as u16;
    let query_w = input_w.saturating_sub(input_label_w);
    let (shown, cursor_col) = help.input.visible_text_and_cursor(query_w as usize);
    let cursor_x = x + 2 + input_label_w + (cursor_col as u16).min(query_w);
    let rows_y = y + ROWS_TOP_OFFSET;
    let rows_h = height.saturating_sub(ROWS_TOP_OFFSET + 1);
    let category_legend =
        HelpCategory::ALL.iter().map(|category| category.label()).collect::<Vec<_>>().join("  ");

    {
        let buf = frame.buffer_mut();
        for dy in 0..height {
            for dx in 0..width {
                set_cell(buf, x + dx, y + dy, " ", base);
            }
        }
        draw_border(buf, x, y, width, height, border);
        buf.set_stringn(x + 2, y + 1, "Help", 4, title);
        let hint = "/ filter  j/k move  PgUp/PgDn page  Esc/q close";
        let hint_x = x + width.saturating_sub(hint.chars().count() as u16 + 2);
        buf.set_stringn(hint_x, y + 1, hint, hint.chars().count(), base.fg(Color::Indexed(244)));
        for dx in 0..input_w {
            set_cell(buf, x + 2 + dx, y + 2, " ", input_style);
        }
        buf.set_stringn(x + 2, y + 2, input_label, input_label_w as usize, input_style);
        buf.set_stringn(x + 2 + input_label_w, y + 2, &shown, query_w as usize, input_style);
        buf.set_stringn(
            x + 2,
            y + 3,
            &category_legend,
            input_w as usize,
            base.fg(Color::Indexed(244)),
        );
    }
    frame.set_cursor_position(Position::new(cursor_x, y + 2));

    let inner_w = width.saturating_sub(2);
    let category_w = 10.min(inner_w);
    let chord_w = 12.min(inner_w.saturating_sub(category_w + 1));
    let action_w = 22.min(inner_w.saturating_sub(category_w + chord_w + 2));
    let description_x = x + 1 + category_w + 1 + chord_w + 1 + action_w + 1;
    let description_w = (x + width.saturating_sub(1)).saturating_sub(description_x);

    let buf = frame.buffer_mut();
    for (ranked_i, &(item_i, _)) in
        ranked.iter().enumerate().skip(help.scroll).take(rows_h as usize)
    {
        let row_y = rows_y + (ranked_i - help.scroll) as u16;
        let entry = &help.items[item_i];
        let style = if ranked_i == help.cursor { selected } else { base };
        for dx in 0..inner_w {
            set_cell(buf, x + 1 + dx, row_y, " ", style);
        }
        let category_style = if ranked_i == help.cursor {
            style.add_modifier(Modifier::BOLD)
        } else {
            style.fg(Color::Indexed(110)).add_modifier(Modifier::BOLD)
        };
        let category = truncate(entry.category.label(), category_w as usize);
        let chord = truncate(&entry.chord, chord_w as usize);
        let action = truncate(entry.action.display_name(), action_w as usize);
        let description = truncate(entry.description, description_w as usize);
        buf.set_stringn(x + 1, row_y, &category, category_w as usize, category_style);
        buf.set_stringn(x + 1 + category_w + 1, row_y, &chord, chord_w as usize, style);
        buf.set_stringn(x + 1 + category_w + chord_w + 2, row_y, &action, action_w as usize, style);
        buf.set_stringn(description_x, row_y, &description, description_w as usize, style);
    }
    if ranked.is_empty() && rows_h > 0 {
        buf.set_stringn(
            x + 2,
            rows_y,
            "No bindings match the filter",
            inner_w.saturating_sub(2) as usize,
            base,
        );
    }
}

fn set_cell(buf: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    let cell = &mut buf[(x, y)];
    cell.reset();
    cell.set_symbol(symbol).set_style(style);
}

fn draw_border(buf: &mut Buffer, x: u16, y: u16, width: u16, height: u16, style: Style) {
    if width < 2 || height < 2 {
        return;
    }
    let right = x + width - 1;
    let bottom = y + height - 1;
    for col in x + 1..right {
        set_cell(buf, col, y, "-", style);
        set_cell(buf, col, bottom, "-", style);
    }
    for row in y + 1..bottom {
        set_cell(buf, x, row, "|", style);
        set_cell(buf, right, row, "|", style);
    }
    set_cell(buf, x, y, "+", style);
    set_cell(buf, right, y, "+", style);
    set_cell(buf, x, bottom, "+", style);
    set_cell(buf, right, bottom, "+", style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_entries_match_every_registered_binding() {
        let keys = Keys::default();
        let entries = build_entries(&keys);
        assert_eq!(entries.len(), keys.bindings().len());
        assert!(entries.iter().all(|entry| !entry.description.is_empty()));
    }

    #[test]
    fn help_filter_matches_action_name_or_chord() {
        let keys = Keys::default();
        let mut state = HelpState::new(build_entries(&keys));
        state.set_query("new");
        assert!(state.ranked().iter().any(|(index, _)| {
            let entry = &state.items[*index];
            fuzzy_score(state.input.as_str(), entry.action.display_name()).is_some()
                || fuzzy_score(state.input.as_str(), &entry.chord).is_some()
        }));

        state.set_query("?");
        assert!(state.ranked().iter().any(|(index, _)| state.items[*index].chord == "?"));
    }

    #[test]
    fn entries_are_grouped_by_category_and_chord() {
        let entries = build_entries(&Keys::default());
        let mut previous: Option<(HelpCategory, String)> = None;
        for entry in entries {
            let current = (entry.category, entry.chord.clone());
            if let Some(previous) = &previous {
                assert!(
                    *previous <= current,
                    "entries are not grouped: {previous:?} > {current:?}"
                );
            }
            previous = Some(current);
        }
        assert_eq!(HelpCategory::ALL.len(), 7);
        assert!(HelpCategory::ALL.iter().all(|category| !category.label().is_empty()));
    }

    #[test]
    fn navigation_and_hit_testing_clamp_to_the_visible_list() {
        let screen = Rect { x: 0, y: 0, width: 100, height: 40 };
        let visible = rows_visible(screen);
        assert!(visible > 0);
        let mut state = HelpState::new(build_entries(&Keys::default()));
        state.move_cursor(isize::MAX, visible);
        assert_eq!(state.cursor, state.ranked().len() - 1);
        state.move_cursor(isize::MIN, visible);
        assert_eq!(state.cursor, 0);

        let rect = help_rect(screen).unwrap();
        let rows_y = rect.y + ROWS_TOP_OFFSET;
        assert_eq!(help_row_at(screen, rect.x + 2, rows_y, 0), Some(0));
        assert_eq!(help_row_at(screen, rect.x + 2, rows_y + 1, 5), Some(6));
        assert_eq!(help_row_at(screen, rect.x + 2, rect.y + 1, 0), None);
        assert!(help_rect(Rect { x: 0, y: 0, width: 50, height: 10 }).is_none());
    }
}
