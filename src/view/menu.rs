//! Snapshot-derived context-menu actions and the shared point host.

use enzui_theme::EnzTheme;
use enzui_widgets::context_menu_host::ContextMenuItem;
use iced::Element;

use crate::message::Message;
use crate::model::{MenuTarget, Model, PendingAction};

fn item(label: &str, enabled: bool, message: Message) -> ContextMenuItem<Message> {
    ContextMenuItem::command(message, crate::i18n::display(label)).enabled(enabled)
}

/// Build target actions using the current snapshot's permission flags.
pub(crate) fn items(model: &Model, target: &MenuTarget) -> Vec<ContextMenuItem<Message>> {
    match target {
        MenuTarget::Process(target) => {
            let p = model
                .snapshot()
                .processes
                .iter()
                .find(|process| process.matches_target(target))
                .cloned()
                .unwrap_or_else(crate::services::ProcessData::empty);
            let allowed = p.action_allowed;
            vec![
                item(
                    "Show in inspector",
                    true,
                    Message::SelectProcess(target.clone()),
                ),
                item(
                    "End",
                    allowed,
                    Message::RequestAction(PendingAction::EndProcess {
                        pid: target.pid.clone(),
                        start_time_ticks: target.start_time_ticks,
                        kernel_uid: target.kernel_uid,
                        name: p.name.clone(),
                    }),
                ),
                item(
                    "Force kill",
                    allowed,
                    Message::RequestAction(PendingAction::KillProcess {
                        pid: target.pid.clone(),
                        start_time_ticks: target.start_time_ticks,
                        kernel_uid: target.kernel_uid,
                        name: p.name.clone(),
                    }),
                ),
                item(
                    "Copy PID",
                    true,
                    Message::CopyText {
                        value: target.pid.clone(),
                        confirmation: "Copied PID",
                    },
                ),
                item(
                    "Copy name",
                    true,
                    Message::CopyText {
                        value: p.name,
                        confirmation: "Copied process name",
                    },
                ),
            ]
        }
        MenuTarget::Service(unit) => {
            let s = model
                .snapshot()
                .services
                .iter()
                .find(|s| &s.name == unit)
                .cloned()
                .unwrap_or_else(crate::services::ServiceData::empty);
            let allowed = s.action_allowed;
            let running = s.is_running();
            vec![
                item(
                    "Show in inspector",
                    true,
                    Message::SelectService(unit.clone()),
                ),
                item(
                    "Start",
                    allowed && !running,
                    Message::RunServiceAction(unit.clone(), "start"),
                ),
                item(
                    "Stop",
                    allowed && running,
                    Message::RequestAction(PendingAction::StopService { unit: unit.clone() }),
                ),
                item(
                    "Restart",
                    allowed && running,
                    Message::RunServiceAction(unit.clone(), "restart"),
                ),
                item("Logs…", true, Message::OpenLogs(unit.clone())),
                item(
                    "Copy service name",
                    true,
                    Message::CopyText {
                        value: unit.clone(),
                        confirmation: "Copied service name",
                    },
                ),
                item(
                    "Copy unit file path",
                    !s.fragment_path.is_empty(),
                    Message::CopyText {
                        value: s.fragment_path,
                        confirmation: "Copied unit file path",
                    },
                ),
            ]
        }
    }
}

pub(crate) fn present<'a>(
    model: &'a Model,
    theme: &EnzTheme,
    page: Element<'a, Message>,
) -> Element<'a, Message> {
    model
        .menu
        .host(page, Message::ContextMenuEvent)
        .label(crate::i18n::display("Actions"))
        .id_prefix("tm-context-menu")
        .menu_row_tokens(enzui_widgets::menu_row::MenuRowTokens::from_palette(
            &theme.palette,
        ))
        .into()
}
