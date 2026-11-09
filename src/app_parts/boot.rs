//! App boot.

use super::*;

impl App {
    pub(crate) fn new() -> (Self, Task<Message>) {
        #[cfg_attr(
            not(all(feature = "center", not(test))),
            allow(
                unused_mut,
                reason = "Center attaches its connector only in production builds"
            )
        )]
        let (mut app, task) = Self::boot(Self::ambient_session_path().as_deref());
        // Tests must not expose the app to the Center service.
        #[cfg(all(feature = "center", not(test)))]
        app.start_center();
        (app, task)
    }

    #[cfg(not(test))]
    pub(crate) fn ambient_session_path() -> Option<std::path::PathBuf> {
        session::state_path()
    }

    #[cfg(test)]
    pub(crate) fn ambient_session_path() -> Option<std::path::PathBuf> {
        // Tests must not read or overwrite the user's session.
        None
    }

    pub(crate) fn boot(session_path: Option<&std::path::Path>) -> (Self, Task<Message>) {
        let selection = std::env::var("ENZUI_TASK_MANAGER_SELECT").ok();
        Self::boot_with_capture_selection(session_path, selection.as_deref())
    }

    pub(crate) fn boot_with_capture_selection(
        session_path: Option<&std::path::Path>,
        capture_selection: Option<&str>,
    ) -> (Self, Task<Message>) {
        let capture_toast =
            capture_toast_enabled(std::env::var_os("ENZUI_TASK_MANAGER_CAPTURE_TOAST").as_deref());
        let mut model = Model::new();
        let session = session::store(session_path);
        let page_override = std::env::var("ENZUI_TASK_MANAGER_PAGE")
            .ok()
            .and_then(|page| Page::from_id(page.trim()));
        if let Some(page) = page_override {
            model.select_page(page);
        }
        model.capture_process_selection = capture_selection
            .map(str::trim)
            .filter(|selection| !selection.is_empty())
            .map(str::to_owned);
        let logs_unit = std::env::var("ENZUI_TASK_MANAGER_LOGS")
            .ok()
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty());

        let (themes, theme_boot) = ThemeIntegration::boot();
        let mut app = Self {
            lifecycle: None,
            finish_then_exit: false,
            shell_activity: Default::default(),
            model,
            themes,
            nav: enzui_widgets::nav_split::NavSplitState::new(),
            desktop: enzui_widgets::single_instance::SingleInstance::standalone(),
            device_facts_source: Arc::new(LinuxDeviceFactsSource),
            session,
            #[cfg(feature = "center")]
            center: None,
        };
        #[cfg(not(test))]
        if session_path.is_none() {
            app.session.refuse_missing_root();
        }
        app.session.begin_load(page_override.is_some());
        let capture_toast_task = if capture_toast {
            app.update(Message::Refresh)
        } else {
            Task::none()
        };
        app.request_collection(collection::SourceKind::Boot);
        let facts_task = app.prepare_device_facts_if_visible();
        let logs_task = match logs_unit {
            Some(unit) => Task::done(Message::OpenLogs(unit)),
            None => Task::none(),
        };
        let wakes = app.flight_wakes();
        (
            app,
            Task::batch([
                wakes,
                capture_toast_task,
                facts_task,
                logs_task,
                theme_boot.map(|event| Message::Theme(Box::new(event))),
            ]),
        )
    }
}
