use super::*;
use crate::services::{ProcessData, Snapshot};

fn row(start_time_ticks: u64, uid: u32) -> ProcessData {
    ProcessData {
        pid: "424242".into(),
        start_time_ticks,
        kernel_uid: Some(uid),
        name: "editor".into(),
        action_allowed: true,
        ..Default::default()
    }
}
fn snapshot(process: ProcessData) -> Snapshot {
    Snapshot {
        processes: vec![process],
        ..Default::default()
    }
}

#[test]
fn replacing_a_selected_process_retires_its_menu_confirmation_and_captured_actions() {
    let original = row(100, 1000);
    let mut app = tests::app_on_page(Page::Processes);
    app.model = Model::from_snapshot(snapshot(original.clone()));
    app.model.select_page(Page::Processes);
    drop(app.update(Message::SelectProcess(original.target())));
    drop(app.update(Message::EndSelectedProcess));
    assert!(app.model.pending_action.is_some());
    for replacement in [row(200, 1000), row(100, 2000)] {
        app.model.apply_snapshot(snapshot(replacement));
        assert!(app.model.selected_pid.is_empty());
        assert!(app.model.pending_action.is_none());
        drop(app.update(Message::SelectProcess(original.target())));
        drop(app.update(Message::OpenContextMenu(
            model::MenuTarget::Process(original.target()),
            iced::Point::ORIGIN,
        )));
        assert!(!app.model.menu.is_open());
        assert!(app.model.selected_pid.is_empty());
    }
}

#[test]
fn sorting_and_filtering_do_not_retarget_a_captured_process_action() {
    let original = row(100, 1000);
    let other = ProcessData {
        pid: "424243".into(),
        name: "worker".into(),
        ..original.clone()
    };
    let mut app = tests::app_on_page(Page::Processes);
    app.model = Model::from_snapshot(Snapshot {
        processes: vec![original.clone(), other],
        ..Default::default()
    });
    app.model.select_page(Page::Processes);
    drop(app.update(Message::SelectProcess(original.target())));
    app.model.sort_processes_by("name");
    app.model.set_query("editor".into());
    drop(app.update(Message::EndSelectedProcess));
    assert_eq!(
        app.model
            .pending_action
            .as_ref()
            .and_then(PendingAction::process_target),
        Some(original.target())
    );
}
