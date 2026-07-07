//! Session tree snapshots: capturing enough of `State` to reconstruct
//! workspace/screen/pane layout (and each tab's cwd) after a restart, and
//! replaying that reconstruction against a fresh [`crate::Mux`].
//!
//! Scope, deliberately: layout (split tree shape + ratios), names, cwd,
//! and active selections restore exactly. A tab's *command* does not —
//! every restored tab is the user's default shell, `cd`'d into its
//! recorded cwd (see [`crate::Mux::restore`]). Threading an arbitrary
//! command through `new_workspace`/`new_screen`/`split`'s spawn calls
//! would be a reasonable follow-up; this keeps the first version to what
//! the existing spawn APIs already support cleanly.

use serde::{Deserialize, Serialize};

use crate::model::{Node, Screen, State};
use crate::{PaneId, SplitDir};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum DirSnapshot {
    Right,
    Down,
}

impl From<SplitDir> for DirSnapshot {
    fn from(dir: SplitDir) -> Self {
        match dir {
            SplitDir::Right => DirSnapshot::Right,
            SplitDir::Down => DirSnapshot::Down,
        }
    }
}

impl From<DirSnapshot> for SplitDir {
    fn from(dir: DirSnapshot) -> Self {
        match dir {
            DirSnapshot::Right => SplitDir::Right,
            DirSnapshot::Down => SplitDir::Down,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum LayoutSnapshot {
    /// Index into the owning [`ScreenSnapshot::panes`].
    Leaf(usize),
    Split { dir: DirSnapshot, ratio: f32, a: Box<LayoutSnapshot>, b: Box<LayoutSnapshot> },
}

#[derive(Debug, Serialize, Deserialize)]
struct TabSnapshot {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaneSnapshot {
    #[serde(default)]
    name: Option<String>,
    tabs: Vec<TabSnapshot>,
    active_tab_index: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct ScreenSnapshot {
    #[serde(default)]
    name: Option<String>,
    layout: LayoutSnapshot,
    panes: Vec<PaneSnapshot>,
    active_pane_index: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceSnapshot {
    name: String,
    screens: Vec<ScreenSnapshot>,
    active_screen: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionSnapshot {
    workspaces: Vec<WorkspaceSnapshot>,
    active_workspace: usize,
}

impl SessionSnapshot {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }

    pub fn load(path: &std::path::Path) -> Option<Self> {
        let contents = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&contents).ok()
    }

    /// Writes atomically (write-to-temp then rename) so a crash or a
    /// concurrent read never observes a truncated file.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

pub fn capture(state: &State) -> SessionSnapshot {
    SessionSnapshot {
        active_workspace: state.active_workspace,
        workspaces: state
            .workspaces
            .iter()
            .map(|ws| WorkspaceSnapshot {
                name: ws.name.clone(),
                active_screen: ws.active_screen,
                screens: ws.screens.iter().map(|screen| capture_screen(state, screen)).collect(),
            })
            .collect(),
    }
}

fn capture_screen(state: &State, screen: &Screen) -> ScreenSnapshot {
    let mut pane_ids = Vec::new();
    screen.root.pane_ids(&mut pane_ids);
    let index_of = |id: PaneId| pane_ids.iter().position(|&p| p == id).unwrap();

    let panes = pane_ids
        .iter()
        .map(|pid| {
            let pane = &state.panes[pid];
            PaneSnapshot {
                name: pane.name.clone(),
                active_tab_index: pane.active_tab,
                tabs: pane
                    .tabs
                    .iter()
                    .map(|sid| {
                        let surface = state.surfaces.get(sid);
                        TabSnapshot {
                            name: surface.and_then(|s| s.name()),
                            cwd: surface.and_then(|s| s.cwd()),
                        }
                    })
                    .collect(),
            }
        })
        .collect();

    ScreenSnapshot {
        name: screen.name.clone(),
        active_pane_index: index_of(screen.active_pane),
        layout: capture_node(&screen.root, &index_of),
        panes,
    }
}

fn capture_node(node: &Node, index_of: &impl Fn(PaneId) -> usize) -> LayoutSnapshot {
    match node {
        Node::Leaf(id) => LayoutSnapshot::Leaf(index_of(*id)),
        Node::Split { dir, ratio, a, b } => LayoutSnapshot::Split {
            dir: (*dir).into(),
            ratio: *ratio,
            a: Box::new(capture_node(a, index_of)),
            b: Box::new(capture_node(b, index_of)),
        },
    }
}

/// Quotes a path as a single POSIX shell word.
pub(crate) fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

pub(crate) struct RestoreWorkspace<'a> {
    pub name: &'a str,
    pub screens: Vec<RestoreScreen<'a>>,
    pub active_screen: usize,
}

pub(crate) struct RestoreScreen<'a> {
    pub name: Option<&'a str>,
    pub layout: RestoreLayout,
    pub panes: Vec<RestorePane<'a>>,
    pub active_pane_index: usize,
}

pub(crate) enum RestoreLayout {
    Leaf(usize),
    Split { dir: SplitDir, ratio: f32, a: Box<RestoreLayout>, b: Box<RestoreLayout> },
}

pub(crate) struct RestorePane<'a> {
    pub name: Option<&'a str>,
    pub tabs: Vec<RestoreTab<'a>>,
    pub active_tab_index: usize,
}

pub(crate) struct RestoreTab<'a> {
    pub name: Option<&'a str>,
    pub cwd: Option<&'a str>,
}

/// Borrowed, engine-agnostic view of a snapshot for `Mux::restore` to
/// replay — keeps the (de)serialization types above private to this
/// module.
pub(crate) fn workspaces(snapshot: &SessionSnapshot) -> (Vec<RestoreWorkspace<'_>>, usize) {
    let workspaces = snapshot
        .workspaces
        .iter()
        .map(|ws| RestoreWorkspace {
            name: &ws.name,
            active_screen: ws.active_screen,
            screens: ws
                .screens
                .iter()
                .map(|screen| RestoreScreen {
                    name: screen.name.as_deref(),
                    active_pane_index: screen.active_pane_index,
                    layout: restore_layout(&screen.layout),
                    panes: screen
                        .panes
                        .iter()
                        .map(|pane| RestorePane {
                            name: pane.name.as_deref(),
                            active_tab_index: pane.active_tab_index,
                            tabs: pane
                                .tabs
                                .iter()
                                .map(|tab| RestoreTab {
                                    name: tab.name.as_deref(),
                                    cwd: tab.cwd.as_deref(),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    (workspaces, snapshot.active_workspace)
}

fn restore_layout(layout: &LayoutSnapshot) -> RestoreLayout {
    match layout {
        LayoutSnapshot::Leaf(i) => RestoreLayout::Leaf(*i),
        LayoutSnapshot::Split { dir, ratio, a, b } => RestoreLayout::Split {
            dir: (*dir).into(),
            ratio: *ratio,
            a: Box::new(restore_layout(a)),
            b: Box::new(restore_layout(b)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_handles_spaces_and_single_quotes() {
        assert_eq!(shell_quote("/home/matc/Projects/cmux-linux"), "'/home/matc/Projects/cmux-linux'");
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
    }

    #[test]
    fn round_trips_through_json() {
        let snapshot = SessionSnapshot {
            active_workspace: 0,
            workspaces: vec![WorkspaceSnapshot {
                name: "work".into(),
                active_screen: 0,
                screens: vec![ScreenSnapshot {
                    name: None,
                    active_pane_index: 1,
                    layout: LayoutSnapshot::Split {
                        dir: DirSnapshot::Right,
                        ratio: 0.5,
                        a: Box::new(LayoutSnapshot::Leaf(0)),
                        b: Box::new(LayoutSnapshot::Leaf(1)),
                    },
                    panes: vec![
                        PaneSnapshot {
                            name: None,
                            active_tab_index: 0,
                            tabs: vec![TabSnapshot { name: None, cwd: Some("/a".into()) }],
                        },
                        PaneSnapshot {
                            name: Some("logs".into()),
                            active_tab_index: 0,
                            tabs: vec![TabSnapshot { name: None, cwd: Some("/b".into()) }],
                        },
                    ],
                }],
            }],
        };

        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let restored: SessionSnapshot = serde_json::from_str(&json).unwrap();
        let (workspaces, active_workspace) = self::workspaces(&restored);
        assert_eq!(active_workspace, 0);
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].name, "work");
        assert_eq!(workspaces[0].screens[0].panes.len(), 2);
        assert_eq!(workspaces[0].screens[0].panes[1].name, Some("logs"));
        assert!(matches!(workspaces[0].screens[0].layout, RestoreLayout::Split { .. }));
    }
}
