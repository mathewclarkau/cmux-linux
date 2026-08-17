//! Layout export/apply document (issue #76): the user-addressable JSON
//! snapshot of one workspace's tab + pane + agent-argv topology, and its
//! replay. The schema is versioned (`schema_version: 1`); see
//! `spec/layout-schema.md`.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::{Mux, PaneId, SurfaceId, SurfaceOptions};

    /// A mux whose default spawn is a quiet, long-lived child (mirrors
    /// `mux::tests::test_mux`), with cmux's auto-injected socket env keys
    /// present so capture's exclusion list is exercised.
    fn test_mux() -> Arc<Mux> {
        let opts = SurfaceOptions {
            command: Some(vec!["/bin/cat".to_string()]),
            extra_env: vec![
                ("CMUX_MUX_SOCKET".into(), "/tmp/cmux-layout-test.sock".into()),
                ("CMUX_SOCKET_PATH".into(), "/tmp/cmux-layout-test.sock".into()),
                ("FLEET_TIER".into(), "A".into()),
            ],
            ..Default::default()
        };
        Mux::new("layout-doc-test", opts)
    }

    fn pane_of(mux: &Mux, surface: SurfaceId) -> PaneId {
        mux.with_state(|s| s.pane_of(surface).unwrap())
    }

    // 1. `layout_doc_round_trips_through_json`
    #[test]
    fn layout_doc_round_trips_through_json() {
        let mux = test_mux();
        let s1 = mux.new_workspace(Some("fleet".into()), None).unwrap();
        let p1 = pane_of(&mux, s1.id);
        mux.split(p1, crate::SplitDir::Right, None).unwrap();
        mux.rename_pane(p1, "build".into());

        let doc = mux.with_state(|s| capture_workspace(s, 0)).unwrap();
        assert_eq!(doc.schema_version, LAYOUT_SCHEMA_VERSION);
        assert_eq!(doc.cmux_version, crate::VERSION);

        let json = serde_json::to_string_pretty(&doc).unwrap();
        assert!(json.contains("\"schema_version\": 1"), "json was: {json}");
        let restored = LayoutDocument::from_json_str(&json).unwrap();
        assert_eq!(restored, doc);
    }

    // 2. `layout_doc_rejects_unknown_schema_version`
    #[test]
    fn layout_doc_rejects_unknown_schema_version() {
        let mux = test_mux();
        mux.new_workspace(Some("fleet".into()), None).unwrap();
        let mut doc = mux.with_state(|s| capture_workspace(s, 0)).unwrap();
        doc.schema_version = 2;

        let err = doc.validate().unwrap_err().to_string();
        assert!(err.contains("schema_version"), "error was: {err}");
        assert!(err.contains('2'), "error should name the bad version: {err}");

        // The strict JSON entry point applies the same gate.
        doc.schema_version = LAYOUT_SCHEMA_VERSION;
        let json = serde_json::to_string(&doc).unwrap().replace("\"schema_version\":1", "\"schema_version\":7");
        let err = LayoutDocument::from_json_str(&json).unwrap_err().to_string();
        assert!(err.contains("schema_version"), "error was: {err}");
    }

    // 3. `layout_doc_rejects_leaf_index_out_of_range`
    #[test]
    fn layout_doc_rejects_leaf_index_out_of_range() {
        let doc = LayoutDocument {
            schema_version: LAYOUT_SCHEMA_VERSION,
            cmux_version: crate::VERSION.to_string(),
            workspace: LayoutWorkspace {
                name: "bad".into(),
                color: None,
                icon: None,
                active_screen: 0,
                screens: vec![LayoutScreen {
                    name: None,
                    active_pane: 0,
                    layout: LayoutNode::Leaf { pane: 3 },
                    panes: vec![
                        LayoutPane { name: None, active_tab: 0, tabs: vec![sample_tab()] },
                        LayoutPane { name: None, active_tab: 0, tabs: vec![sample_tab()] },
                    ],
                }],
            },
        };
        let err = doc.validate().unwrap_err().to_string();
        assert!(err.contains('3'), "error should name the bad index: {err}");
        assert!(err.contains("out of range"), "error was: {err}");
    }

    // 4. `layout_doc_rejects_unknown_tab_kind`
    #[test]
    fn layout_doc_rejects_unknown_tab_kind() {
        let json = r#"{
            "schema_version": 1,
            "cmux_version": "test",
            "workspace": {
                "name": "w",
                "active_screen": 0,
                "screens": [{
                    "active_pane": 0,
                    "layout": {"type": "leaf", "pane": 0},
                    "panes": [{"tabs": [{"kind": "matrix"}]}]
                }]
            }
        }"#;
        let err = LayoutDocument::from_json_str(json).unwrap_err().to_string();
        assert!(err.contains("matrix"), "error should name the unknown kind: {err}");
    }

    // 5. `layout_doc_capture_includes_split_geometry_names_and_selections`
    #[test]
    fn layout_doc_capture_includes_split_geometry_names_and_selections() {
        let mux = test_mux();
        let s1 = mux.new_workspace(Some("fleet".into()), None).unwrap();
        let p1 = pane_of(&mux, s1.id);
        mux.rename_pane(p1, "build".into());
        mux.rename_surface(s1.id, "api".into());
        mux.split(p1, crate::SplitDir::Right, None).unwrap();
        mux.set_ratio(p1, crate::SplitDir::Right, 0.7);
        mux.new_screen(None, None).unwrap();
        mux.focus_pane(p1);

        let doc = mux.with_state(|s| capture_workspace(s, 0)).unwrap();
        let ws = &doc.workspace;
        assert_eq!(ws.name, "fleet");
        assert_eq!(ws.screens.len(), 2);
        assert_eq!(ws.active_screen, 1, "capture should record the active screen");

        let sc = &ws.screens[0];
        let LayoutNode::Split { dir, ratio, a, b } = &sc.layout else {
            panic!("expected a split root, got {:?}", sc.layout);
        };
        assert_eq!(*dir, LayoutDir::Right);
        assert!((*ratio - 0.7).abs() < 1e-6, "ratio was {ratio}");
        assert_eq!(**a, LayoutNode::Leaf { pane: 0 });
        assert_eq!(**b, LayoutNode::Leaf { pane: 1 });
        assert_eq!(sc.panes[0].name.as_deref(), Some("build"));
        assert_eq!(sc.active_pane, 0, "focused pane is pane 0");
        match &sc.panes[0].tabs[0] {
            LayoutTab::Pty { name, .. } => assert_eq!(name.as_deref(), Some("api")),
            other => panic!("expected a pty tab, got {other:?}"),
        }
    }

    // 6. `layout_doc_capture_records_command_and_env_dropping_cmux_auto_vars`
    #[test]
    fn layout_doc_capture_records_command_and_env_dropping_cmux_auto_vars() {
        let mux = test_mux();
        let s1 = mux.new_workspace(Some("fleet".into()), None).unwrap();
        let pane = pane_of(&mux, s1.id);
        let s2 = mux
            .new_tab_with_overrides(
                Some(pane),
                None,
                None,
                Some(SpawnOverrides {
                    command: Some(vec![
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "sleep 30".to_string(),
                    ]),
                    extra_env: vec![("FLEET_WORKER".into(), "9".into())],
                    cwd: Some("/tmp".into()),
                }),
            )
            .unwrap();
        assert!(s2.id > 0);

        let doc = mux.with_state(|s| capture_workspace(s, 0)).unwrap();
        let tabs = &doc.workspace.screens[0].panes[0].tabs;
        assert_eq!(tabs.len(), 2, "pane should have the workspace tab + the override tab");
        match &tabs[1] {
            LayoutTab::Pty { command, env, cwd, .. } => {
                assert_eq!(
                    command.as_deref(),
                    Some(["/bin/sh".to_string(), "-c".to_string(), "sleep 30".to_string()].as_slice()),
                    "recorded argv should round-trip"
                );
                assert_eq!(env.get("FLEET_TIER").map(String::as_str), Some("A"));
                assert_eq!(env.get("FLEET_WORKER").map(String::as_str), Some("9"));
                assert!(!env.contains_key("CMUX_MUX_SOCKET"), "auto socket env must not be exported");
                assert!(!env.contains_key("CMUX_SOCKET_PATH"), "auto socket env must not be exported");
                assert_eq!(cwd.as_deref(), Some("/tmp"));
            }
            other => panic!("expected a pty tab, got {other:?}"),
        }
    }

    // 7. `layout_doc_rejects_bad_ratio`
    #[test]
    fn layout_doc_rejects_bad_ratio() {
        for bad in [1.5f32, 0.0, -0.25, 1.0] {
            let mut doc = sample_doc();
            doc.workspace.screens[0].layout = LayoutNode::Split {
                dir: LayoutDir::Right,
                ratio: bad,
                a: Box::new(LayoutNode::Leaf { pane: 0 }),
                b: Box::new(LayoutNode::Leaf { pane: 1 }),
            };
            doc.workspace.screens[0].panes.push(LayoutPane {
                name: None,
                active_tab: 0,
                tabs: vec![sample_tab()],
            });
            let err = doc.validate().unwrap_err().to_string();
            assert!(err.contains("ratio"), "ratio {bad} should be rejected, got: {err}");
        }
    }

    // -- fixtures ---------------------------------------------------------

    fn sample_tab() -> LayoutTab {
        LayoutTab::Pty {
            name: None,
            cwd: None,
            command: None,
            env: BTreeMap::new(),
        }
    }

    fn sample_doc() -> LayoutDocument {
        LayoutDocument {
            schema_version: LAYOUT_SCHEMA_VERSION,
            cmux_version: crate::VERSION.to_string(),
            workspace: LayoutWorkspace {
                name: "sample".into(),
                color: None,
                icon: None,
                active_screen: 0,
                screens: vec![LayoutScreen {
                    name: None,
                    active_pane: 0,
                    layout: LayoutNode::Leaf { pane: 0 },
                    panes: vec![LayoutPane { name: None, active_tab: 0, tabs: vec![sample_tab()] }],
                }],
            },
        }
    }
}
