use super::*;
use gpui::TestAppContext;

fn snapshot() -> GamingSnapshot {
    GamingSnapshot {
        backup_json: String::new(),
        mode: 1,
        active_profile: 1,
        saved_path: None,
        profiles: vec![GamingProfile {
            sector: 1,
            enabled: true,
            checksum_valid: true,
            name: "Test profile".into(),
            report_interval_ms: 1,
            dpi: [800, 1200, 1600, 2400, 3200],
            default_dpi_slot: 2,
            shift_dpi_slot: 0,
            buttons: vec!["mouse mask 0x0001".into(), "mouse mask 0x0002".into()],
            shifted_buttons: vec!["disabled".into(); 2],
        }],
    }
}

#[gpui::test]
fn draft_preserves_other_layer_and_discard_restores_original(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(GamingPanel::new);
    view.update_in(cx, |panel, window, cx| {
        panel.snapshot = Some(snapshot());
        panel.select(1, window, cx);
        assert!(!panel.dirty(cx));
        panel.selected_button = 1;
        panel.assign(
            Some(GamingAction::Key {
                usage: 75,
                modifiers: 1,
            }),
            cx,
        );
        assert!(panel.dirty(cx));
        panel.layer = Layer::Shifted;
        panel.assign(Some(GamingAction::Consumer(0xe9)), cx);
        let draft = panel.collect_draft(cx).unwrap();
        assert_eq!(draft.buttons.len(), 2);
        assert!(!draft.buttons[0].shifted);
        assert!(draft.buttons[1].shifted);
        panel.assign(None, cx);
        assert_eq!(panel.draft.buttons.len(), 1);
        panel.select(1, window, cx);
        assert!(!panel.dirty(cx));
        assert!(panel.draft.buttons.is_empty());
    });
}

#[gpui::test]
fn invalid_dpi_and_disabled_default_cannot_be_submitted(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(GamingPanel::new);
    view.update_in(cx, |panel, window, cx| {
        panel.snapshot = Some(snapshot());
        panel.select(1, window, cx);
        for value in ["abc", "-1", "25601", "125", "0"] {
            panel.dpi[2].update(cx, |input, cx| input.set_value(value, window, cx));
            panel.collect_draft(cx).unwrap_err();
        }
        panel.select(1, window, cx);
        assert_eq!(panel.collect_draft(cx).unwrap().dpi[2], 1600);
        panel.busy = true;
        panel.assign(Some(GamingAction::Disabled), cx);
        assert!(panel.draft.buttons.is_empty());
        panel.busy = false;
        panel.needs_refresh = true;
        panel.assign(Some(GamingAction::Disabled), cx);
        assert!(panel.draft.buttons.is_empty());
    });
}
