//! Read-only tree snapshots shared by the renderer and input handling,
//! plus the JSON parser for the remote `list-workspaces` shape.

use mux_core::{
    assign_short_ids, AgentState, BrowserSource, Node, PaneId, ScreenId, SplitDir, State,
    SurfaceId, SurfaceKind, WorkspaceId,
};
use ratatui::style::Color;
use serde_json::Value;

use crate::config::parse_color;

#[derive(Clone, Default)]
pub struct TreeView {
    pub workspaces: Vec<WorkspaceView>,
    pub active_workspace: usize,
}

#[derive(Clone)]
pub struct WorkspaceView {
    pub id: WorkspaceId,
    pub short_id: String,
    pub name: String,
    /// User-assigned sidebar rail color, if any.
    pub color: Option<Color>,
    pub screens: Vec<ScreenView>,
    pub active_screen: usize,
}

#[derive(Clone)]
pub struct ScreenView {
    pub id: ScreenId,
    #[allow(dead_code)]
    pub short_id: String,
    /// User-assigned name, if any (display falls back to the number).
    pub name: Option<String>,
    pub layout: Node,
    pub active_pane: PaneId,
    pub panes: Vec<PaneView>,
}

#[derive(Clone)]
pub struct PaneView {
    pub id: PaneId,
    pub short_id: String,
    /// User-assigned name, if any (display falls back to the active
    /// tab's title).
    pub name: Option<String>,
    pub tabs: Vec<TabView>,
    pub active_tab: usize,
}

#[derive(Clone)]
pub struct TabView {
    pub surface: SurfaceId,
    pub short_id: String,
    pub name: Option<String>,
    pub title: String,
    pub cwd: Option<String>,
    pub agent_state: Option<AgentState>,
    /// The agent session id reported on this surface (the same string
    /// `list-agents` reports as `session`), if any. Surfaced here so the
    /// fuzzy finder can index it for type-ahead search.
    pub agent_session: Option<String>,
    pub kind: SurfaceKind,
    pub browser_source: Option<BrowserSource>,
    pub browser_frames_stalled: bool,
}

impl TreeView {
    pub fn active_workspace(&self) -> Option<&WorkspaceView> {
        self.workspaces.get(self.active_workspace)
    }

    /// The active screen of the active workspace.
    pub fn active_screen(&self) -> Option<&ScreenView> {
        self.active_workspace()?.active_screen_ref()
    }

    pub fn pane(&self, id: PaneId) -> Option<&PaneView> {
        self.workspaces
            .iter()
            .flat_map(|ws| ws.screens.iter())
            .flat_map(|screen| screen.panes.iter())
            .find(|p| p.id == id)
    }

    /// The active surface of the active pane of the active screen.
    pub fn active_surface(&self) -> Option<SurfaceId> {
        let screen = self.active_screen()?;
        screen.pane(screen.active_pane)?.active_surface()
    }

    pub fn surface_kind(&self, id: SurfaceId) -> SurfaceKind {
        self.workspaces
            .iter()
            .flat_map(|ws| ws.screens.iter())
            .flat_map(|screen| screen.panes.iter())
            .flat_map(|pane| pane.tabs.iter())
            .find(|tab| tab.surface == id)
            .map(|tab| tab.kind)
            .unwrap_or(SurfaceKind::Pty)
    }

    /// A human label for a surface, for contexts (like desktop
    /// notifications) that need to identify a pane outside the sidebar's
    /// own rendering: `"<workspace> · <tab title or agent name>"`.
    pub fn tab_label(&self, id: SurfaceId) -> Option<String> {
        self.workspaces.iter().find_map(|ws| {
            let tab = ws
                .screens
                .iter()
                .flat_map(|screen| screen.panes.iter())
                .flat_map(|pane| pane.tabs.iter())
                .find(|tab| tab.surface == id)?;
            let title = tab.name.as_deref().filter(|s| !s.is_empty()).unwrap_or(&tab.title);
            Some(if title.is_empty() { ws.name.clone() } else { format!("{} · {title}", ws.name) })
        })
    }
}

impl WorkspaceView {
    pub fn active_screen_ref(&self) -> Option<&ScreenView> {
        self.screens.get(self.active_screen)
    }
}

impl ScreenView {
    pub fn pane(&self, id: PaneId) -> Option<&PaneView> {
        self.panes.iter().find(|p| p.id == id)
    }

    /// Display name: the user-assigned name, else "screen N" by position.
    pub fn display_name(&self, index: usize) -> String {
        match self.name.as_deref() {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => format!("{}", index + 1),
        }
    }
}

impl PaneView {
    pub fn active_surface(&self) -> Option<SurfaceId> {
        self.tabs.get(self.active_tab).map(|t| t.surface)
    }

    /// Display name: the user-assigned name, else the active tab's
    /// process title, else "shell".
    pub fn display_name(&self) -> &str {
        if let Some(name) = self.name.as_deref() {
            if !name.is_empty() {
                return name;
            }
        }
        self.tabs
            .get(self.active_tab)
            .map(|t| if t.title.is_empty() { "shell" } else { t.title.as_str() })
            .unwrap_or("shell")
    }

    /// Working directory of the active tab, if known.
    pub fn active_cwd(&self) -> Option<&str> {
        self.tabs.get(self.active_tab)?.cwd.as_deref()
    }

    /// Reported agent state of the active tab, if any.
    pub fn active_agent_state(&self) -> Option<AgentState> {
        self.tabs.get(self.active_tab)?.agent_state
    }
}

/// Snapshot a local mux state into a TreeView.
pub fn tree_from_state(state: &State) -> TreeView {
    let ids = state
        .workspaces
        .iter()
        .flat_map(|ws| {
            let mut ids = vec![ws.id];
            for screen in &ws.screens {
                ids.push(screen.id);
                screen.root.pane_ids(&mut ids);
            }
            ids
        })
        .chain(state.surfaces.keys().copied());
    let short_ids = assign_short_ids(ids);
    let pane_view = |id: &PaneId| {
        state.panes.get(id).map(|pane| PaneView {
            id: pane.id,
            short_id: short_ids.get(&pane.id).cloned().unwrap_or_default(),
            name: pane.name.clone(),
            active_tab: pane.active_tab,
            tabs: pane
                .tabs
                .iter()
                .map(|sid| TabView {
                    surface: *sid,
                    short_id: short_ids.get(sid).cloned().unwrap_or_default(),
                    name: state.surfaces.get(sid).and_then(|s| s.name()),
                    title: state.surfaces.get(sid).map(|s| s.title()).unwrap_or_default(),
                    cwd: state.surfaces.get(sid).and_then(|s| s.cwd()),
                    agent_state: state.surfaces.get(sid).and_then(|s| s.agent_report()).map(|r| r.state),
                    agent_session: state
                        .surfaces
                        .get(sid)
                        .and_then(|s| s.agent_report())
                        .and_then(|r| r.session.clone()),
                    kind: state.surfaces.get(sid).map(|s| s.kind()).unwrap_or(SurfaceKind::Pty),
                    browser_source: state.surfaces.get(sid).and_then(|s| s.browser_source()),
                    browser_frames_stalled: state
                        .surfaces
                        .get(sid)
                        .and_then(|s| s.browser_frames_stalled())
                        .unwrap_or(false),
                })
                .collect(),
        })
    };
    TreeView {
        active_workspace: state.active_workspace,
        workspaces: state
            .workspaces
            .iter()
            .map(|ws| WorkspaceView {
                id: ws.id,
                short_id: short_ids.get(&ws.id).cloned().unwrap_or_default(),
                name: ws.name.clone(),
                color: ws.color.map(|c| Color::Rgb(c.r, c.g, c.b)),
                active_screen: ws.active_screen,
                screens: ws
                    .screens
                    .iter()
                    .map(|screen| {
                        let mut pane_ids = Vec::new();
                        screen.root.pane_ids(&mut pane_ids);
                        ScreenView {
                            id: screen.id,
                            short_id: short_ids.get(&screen.id).cloned().unwrap_or_default(),
                            name: screen.name.clone(),
                            layout: screen.root.clone(),
                            active_pane: screen.active_pane,
                            panes: pane_ids.iter().filter_map(pane_view).collect(),
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn parse_layout(value: &Value) -> Option<Node> {
    match value.get("type")?.as_str()? {
        "leaf" => Some(Node::Leaf(value.get("pane")?.as_u64()?)),
        "split" => {
            let dir = match value.get("dir")?.as_str()? {
                "right" => SplitDir::Right,
                "down" => SplitDir::Down,
                _ => return None,
            };
            Some(Node::Split {
                dir,
                ratio: value.get("ratio")?.as_f64()? as f32,
                a: Box::new(parse_layout(value.get("a")?)?),
                b: Box::new(parse_layout(value.get("b")?)?),
            })
        }
        _ => None,
    }
}

fn parse_pane(value: &Value) -> Option<PaneView> {
    Some(PaneView {
        id: value.get("id")?.as_u64()?,
        short_id: value.get("short_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        name: value.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
        active_tab: value.get("active_tab").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        tabs: value
            .get("tabs")
            .and_then(|v| v.as_array())
            .map(|tabs| {
                tabs.iter()
                    .filter_map(|tab| {
                        Some(TabView {
                            surface: tab.get("surface")?.as_u64()?,
                            short_id: tab
                                .get("short_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            name: tab.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
                            title: tab
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            cwd: tab.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string()),
                            agent_state: tab
                                .get("agent_state")
                                .and_then(|v| v.as_str())
                                .and_then(AgentState::parse),
                            agent_session: tab
                                .get("agent_session")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            kind: match tab.get("kind").and_then(|v| v.as_str()) {
                                Some("browser") => SurfaceKind::Browser,
                                _ => SurfaceKind::Pty,
                            },
                            browser_source: match tab.get("browser_source").and_then(|v| v.as_str())
                            {
                                Some("external") => Some(BrowserSource::External),
                                Some("launched") => Some(BrowserSource::Launched),
                                _ => None,
                            },
                            browser_frames_stalled: tab
                                .get("browser_frames_stalled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_screen(value: &Value) -> Option<ScreenView> {
    Some(ScreenView {
        id: value.get("id")?.as_u64()?,
        short_id: value.get("short_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        name: value.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
        layout: value.get("layout").and_then(parse_layout)?,
        active_pane: value.get("active_pane").and_then(|v| v.as_u64()).unwrap_or(0),
        panes: value
            .get("panes")
            .and_then(|v| v.as_array())
            .map(|panes| panes.iter().filter_map(parse_pane).collect())
            .unwrap_or_default(),
    })
}

/// Parse the remote `list-workspaces` response.
pub fn parse_tree(data: &Value) -> TreeView {
    let mut tree = TreeView::default();
    let Some(workspaces) = data.get("workspaces").and_then(|v| v.as_array()) else {
        return tree;
    };
    for (i, ws) in workspaces.iter().enumerate() {
        if ws.get("active").and_then(|v| v.as_bool()) == Some(true) {
            tree.active_workspace = i;
        }
        let mut view = WorkspaceView {
            id: ws.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
            short_id: ws.get("short_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            name: ws.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            color: ws.get("color").and_then(|v| v.as_str()).and_then(parse_color),
            screens: Vec::new(),
            active_screen: 0,
        };
        if let Some(screens) = ws.get("screens").and_then(|v| v.as_array()) {
            for (s, screen) in screens.iter().enumerate() {
                if screen.get("active").and_then(|v| v.as_bool()) == Some(true) {
                    view.active_screen = s;
                }
                if let Some(parsed) = parse_screen(screen) {
                    view.screens.push(parsed);
                }
            }
        }
        tree.workspaces.push(view);
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_tree_reads_workspace_color_as_rgb() {
        let data = json!({
            "workspaces": [
                {"id": 1, "name": "red", "color": "#ff0000", "active": true, "screens": []},
                {"id": 2, "name": "none", "color": null, "active": false, "screens": []},
            ]
        });
        let tree = parse_tree(&data);
        assert_eq!(tree.workspaces[0].color, Some(Color::Rgb(0xff, 0x00, 0x00)));
        assert_eq!(tree.workspaces[1].color, None);
    }

    #[test]
    fn parse_tree_treats_missing_color_key_as_none() {
        let data = json!({
            "workspaces": [
                {"id": 1, "name": "no-color-key", "active": true, "screens": []},
            ]
        });
        let tree = parse_tree(&data);
        assert_eq!(tree.workspaces[0].color, None);
    }
}
