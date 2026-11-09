//! Task Manager.
//!
#![deny(unconditional_recursion)]

#[cfg(test)]
mod action_owner_tests;
mod actions;
#[path = "app_parts/app_view.rs"]
mod app_view;
mod collection;
#[cfg(test)]
mod collection_tests;
mod device_facts;
mod filter_menus;
mod i18n;
mod keybindings;
mod layout;
mod logs_focus;
mod message;
mod model;
mod process_details;
mod projection;
mod resource_ext;
mod sampler;
mod service_actions;
mod services;
mod session;
mod shell_activity;
mod shortcuts_focus;
mod table;
mod table_scroll;
mod theme;

#[cfg(test)]
mod process_identity_refresh_tests;

use enzui_theme::EnzTheme;
use enzui_widgets::confirm_dialog::{ConfirmDialogEvent, enz_confirm_dialog};
use iced::{Element, Subscription, Task, advanced::widget::operate};
use std::sync::Arc;

use device_facts::{DeviceFactsSource, LinuxDeviceFactsSource};
use enzui_core::confirm::ConfirmDecision;
use enzui_core::focus::{Traversal, traversal_operation as traverse};
use enzui_core::theme_file::integration::ThemeIntegration;
#[cfg(test)]
use enzui_core::theme_file::integration::ThemeIntegrationConfig;
use enzui_widgets::app_shell::{ShellLayout, enz_window_settings};
use enzui_widgets::toast_host::{ToastOverlayPlacement, enz_toast_layer};
use message::Message;
use model::{Model, Page, PendingAction};

fn capture_toast_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

pub(crate) fn logs_button_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("task-manager.inspector.logs")
}

pub(crate) const SEARCH_PLACEHOLDER: &str = "Search name, PID or user\u{2026}";

pub(crate) const SERVICES_SEARCH_PLACEHOLDER: &str = "Search name or description\u{2026}";

pub(crate) fn search_field_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("task-manager-app.search.query")
}

pub(crate) fn search_focus_operations<T>(
    _page: crate::model::Page,
) -> Vec<Box<dyn iced::advanced::widget::Operation<T>>>
where
    T: Send + 'static,
{
    vec![Box::new(
        iced::advanced::widget::operation::focusable::focus(search_field_id()),
    )]
}

#[cfg(test)]
fn test_theme_integration(theme: EnzTheme) -> ThemeIntegration {
    use enzui_core::file_watch::InMemoryBackend;
    use enzui_core::theme_file::{ProbeOverride, ThemePaths};
    ThemeIntegration::boot_with(ThemeIntegrationConfig::injected(
        ThemePaths::new(
            Some("/nonexistent/task-manager-theme"),
            Vec::<&str>::new(),
            None::<&str>,
        ),
        Arc::new(InMemoryBackend::new()),
        ProbeOverride::None,
        theme.mode(),
    ))
    .0
}

fn main() -> iced::Result {
    enzui_core::crash_report::CrashReporter::for_app("task-manager-app", env!("CARGO_PKG_VERSION"));
    let _ = enzui_widgets::monospace_fallback::install_monospace_fallback();

    enzui_core::logging::init_default();
    use enzui_widgets::single_instance::{InstanceStart, SingleInstance};
    let instance = match SingleInstance::start(DESKTOP_APP_ID) {
        InstanceStart::Forwarded => return Ok(()),
        InstanceStart::Owner(instance, _) => std::sync::Mutex::new(Some(instance)),
    };

    enzui_widgets::app_setup::run_with_services(
        iced::application(
            move || {
                let (mut app, task) = App::new();
                app.shell_activity = shell_activity::ShellActivity::production();
                app.desktop = instance
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .unwrap_or_else(SingleInstance::standalone);
                match app.desktop.serve_lifecycle(
                    enzui_core::activation::lifecycle::LifecycleCallers::installed(),
                ) {
                    Ok(control) => app.lifecycle = control,
                    Err(error) => log::warn!("Lifecycle service unavailable: {error}"),
                }
                app.publish_lifecycle();
                (app, task)
            },
            App::update,
            App::view,
        )
        .title(|_: &App| i18n::display("Task Manager"))
        .subscription(App::subscription)
        .theme(App::theme)
        .window(with_desktop_identity(iced::window::Settings {
            exit_on_close_request: false,
            ..enz_window_settings(ShellLayout::TitlebarSidebarContent)
        })),
        enzui_widgets::app_setup::AppServices::default()
            .localizer(i18n::from_environment())
            .traversal(enzui_core::focus_traversal::TraversalBinding::Disabled),
    )
}

struct App {
    lifecycle: Option<enzui_core::activation::lifecycle::StopControl>,
    finish_then_exit: bool,
    shell_activity: shell_activity::ShellActivity,
    pub(crate) desktop: enzui_widgets::single_instance::SingleInstance,
    pub(crate) model: Model,
    pub(crate) themes: ThemeIntegration,
    pub(crate) nav: enzui_widgets::nav_split::NavSplitState,
    pub(crate) device_facts_source: Arc<dyn DeviceFactsSource>,
    pub(crate) session: session::Owner,
    #[cfg(feature = "center")]
    pub(crate) center: Option<enzui_agent::center::CenterHost<crate::center::CenterCall>>,
}

#[path = "app_parts/boot.rs"]
mod boot;
#[cfg(feature = "center")]
mod center;
#[path = "app_parts/close.rs"]
mod close;
#[path = "app_parts/confirm.rs"]
mod confirm;
#[path = "app_parts/poll.rs"]
mod poll;
#[path = "app_parts/reduce.rs"]
mod reduce;
#[path = "app_parts/update.rs"]
mod update;
mod view;

fn classify_window_event(
    event: iced::Event,
    status: iced::event::Status,
    id: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(iced::window::Event::CloseRequested) => {
            Some(Message::CollectionClose(id))
        }
        _ => classify_event(&event, status),
    }
}

// Command-modal suppression belongs in reducers, which own the modal state.
fn classify_event(event: &iced::Event, status: iced::event::Status) -> Option<Message> {
    if let Some(traversal) = enzui_core::focus::traversal_from_event(event, status) {
        return Some(match traversal {
            Traversal::Next => Message::FocusNext,
            Traversal::Previous => Message::FocusPrevious,
        });
    }

    match event {
        iced::Event::Window(iced::window::Event::Resized(size)) => {
            Some(Message::WindowResized(*size))
        }
        iced::Event::Window(iced::window::Event::Focused) => {
            Some(Message::WindowVisibilityChanged(true))
        }
        iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::CheckWindowMinimized),
        // F1 is navigation rather than a command binding, so it stays out of
        // the `keybindings` table and opens Help from anywhere.
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::F1),
            ..
        }) => Some(Message::OpenShortcuts),
        iced::Event::Keyboard(key_event) => {
            if status == iced::event::Status::Captured {
                return None;
            }
            if let Some(action) = keybindings::match_event(key_event) {
                return message_for_action(action);
            }
            if let iced::keyboard::Event::KeyPressed {
                text: Some(text),
                modifiers,
                ..
            } = key_event
                && !modifiers.control()
                && !modifiers.alt()
                && !modifiers.logo()
                && !text.is_empty()
                && !text.chars().any(char::is_control)
            {
                return Some(Message::TypeToSearch(text.to_string()));
            }
            None
        }
        _ => None,
    }
}

fn message_for_action(action: keybindings::TaskManagerAction) -> Option<Message> {
    use keybindings::TaskManagerAction as A;
    Some(match action {
        A::Refresh => Message::Refresh,
        A::FocusSearch => Message::FocusSearch,
        A::EndProcess => Message::EndSelectedProcess,
    })
}

fn confirmation_decision(event: &ConfirmDialogEvent) -> Option<bool> {
    let ConfirmDialogEvent::Resolved(cause) = event else {
        return None;
    };
    Some(ConfirmDecision::from_cause(*cause).is_confirmed())
}

fn take_allowed_pending_action(
    model: &mut Model,
    confirmed: bool,
) -> Result<Option<PendingAction>, &'static str> {
    let Some(action) = model.take_pending_action(confirmed) else {
        return Ok(None);
    };
    if !model.action_is_allowed(&action) {
        return Err(match &action {
            PendingAction::EndProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            }
            | PendingAction::KillProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            } => {
                match model
                    .snapshot()
                    .processes
                    .iter()
                    .find(|row| &row.pid == pid)
                {
                    None => "Blocked: target vanished; nothing signalled",
                    Some(row) if row.start_time_ticks != *start_time_ticks => {
                        "Blocked: target PID was reused; nothing signalled"
                    }
                    Some(row) if row.kernel_uid != *kernel_uid => {
                        "Blocked: captured target UID changed; nothing signalled"
                    }
                    Some(row) if !row.action_allowed => {
                        "Blocked: target is protected; nothing signalled"
                    }
                    _ => "Blocked: process sample is stale or closing; nothing signalled",
                }
            }
            PendingAction::StopService { .. } | PendingAction::Service { .. } => {
                "Blocked: target is gone, replaced, or protected"
            }
        });
    }
    Ok(Some(action))
}

fn process_action_of(action: &PendingAction) -> Option<services::ProcessAction> {
    match action {
        PendingAction::EndProcess { .. } => Some(services::ProcessAction::End),
        PendingAction::KillProcess { .. } => Some(services::ProcessAction::ForceKill),
        PendingAction::StopService { .. } | PendingAction::Service { .. } => None,
    }
}

#[cfg(test)]
mod tests;

const DESKTOP_APP_ID: &str = "io.github.enzui.TaskManager";

fn with_desktop_identity(settings: iced::window::Settings) -> iced::window::Settings {
    enzui_widgets::app_shell::DesktopApplicationIdentity::new(DESKTOP_APP_ID)
        .expect("BUG: DESKTOP_APP_ID must be a portable desktop-entry basename")
        .apply_to_window_settings(settings)
}

#[cfg(test)]
mod unified_process_tests;
