use super::*;
use crate::services::{ProcessData, ServiceData, Snapshot};

fn model_with(processes: Vec<ProcessData>, services: Vec<ServiceData>) -> Model {
    Model::from_snapshot(Snapshot {
        processes,
        services,
        ..Snapshot::default()
    })
}

pub(crate) fn app_on_page(page: Page) -> App {
    let mut model = model_with(vec![proc("1", "firefox", true)], vec![]);
    model.select_page(page);
    App {
        shell_activity: Default::default(),
        lifecycle: None,
        finish_then_exit: false,
        model,
        themes: test_theme_integration(enzui_theme::EnzTheme::dark()),
        nav: enzui_widgets::nav_split::NavSplitState::new(),
        desktop: enzui_widgets::single_instance::SingleInstance::standalone(),
        device_facts_source: std::sync::Arc::new(crate::device_facts::LinuxDeviceFactsSource),
        session: crate::session::store(None),
        #[cfg(feature = "center")]
        center: None,
    }
}

fn proc(pid: &str, name: &str, allowed: bool) -> ProcessData {
    ProcessData {
        pid: pid.into(),
        start_time_ticks: 100,
        name: name.into(),
        action_allowed: allowed,
        ..ProcessData::default()
    }
}

#[test]
fn a_relaunch_reopens_the_page_the_last_run_left() {
    use std::time::{Duration, Instant};
    fn settle(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.session.is_busy() {
            drop(app.reduce(Message::SessionWake));
            assert!(
                Instant::now() < deadline,
                "session load/save did not settle"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let dir = enzui_test_support::test_scratch::ScratchDir::new_restricted("tm-relaunch");
    let path = dir.join("session");
    assert_ne!(session::state_path().as_deref(), Some(path.as_path()));
    assert!(App::ambient_session_path().is_none());
    let (mut app, _task) = App::boot(Some(&path));
    settle(&mut app);
    let default_page = app.model.active;
    assert!(
        !path.exists(),
        "merely launching must not write a session file"
    );
    let target = if default_page == Page::Services {
        Page::Processes
    } else {
        Page::Services
    };
    drop(app.update(Message::SelectPage(target)));
    settle(&mut app);
    let (mut relaunched, _task) = App::boot(Some(&path));
    settle(&mut relaunched);
    assert_eq!(relaunched.model.active, target);
    let corrupt = b"not an atomic-state frame at all";
    std::fs::write(&path, corrupt).unwrap();
    let (mut broken, _task) = App::boot(Some(&path));
    settle(&mut broken);
    assert_eq!(broken.model.active, default_page);
    assert!(broken.session.is_write_blocked());
    drop(broken.update(Message::SelectPage(target)));
    settle(&mut broken);
    assert_eq!(std::fs::read(&path).unwrap(), corrupt);
}
