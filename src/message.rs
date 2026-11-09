//! User interactions and asynchronous receipts handled by the application reducer.

use enzui_widgets::confirm_dialog::ConfirmDialogEvent;
use iced::{Point, Size};

use crate::model::{MenuTarget, Page, PendingAction};
use crate::services::ProcessTarget;

#[derive(Debug, Clone)]
pub(crate) enum Message {
    DesktopActivate,
    ActivityWindowOpened(iced::window::Id),
    ActivityWindowTagged(iced::window::Id, String, bool),
    CollectionFirstFrame,
    CollectionWake,
    CollectionTick,
    CollectionClose(iced::window::Id),
    KeepActionsOpen(u64),
    RetryActionsClose(u64),
    Theme(Box<enzui_core::theme_file::integration::ThemeEvent>),
    SelectPage(Page),
    #[cfg(feature = "center")]
    Center(enzui_agent::center::ConnectorEvent),
    SessionWake,
    SessionRetry(u64),
    SessionKeepOpen(u64),
    SelectResource(String),
    /// Carries the captured process identity checked before selection.
    SelectProcess(ProcessTarget),
    ToggleAppGroup(String),
    SelectService(String),
    ToggleInspector,
    CloseInspector,
    SearchChanged(String),
    TypeToSearch(String),
    ProcessFilterMenu(enzui_widgets::menu_button::MenuButtonEvent),
    ServiceFilterMenu(enzui_widgets::menu_button::MenuButtonEvent),
    SortProcesses(String),
    SortServices(String),
    /// Requests an early collection and transient feedback; ignored while a modal is open.
    Refresh,
    /// Integrates normalized pixels against the latest model offset; positive means wheel-up.
    TableWheelScrolled(f32),
    /// Sets the absolute vertical scrollbar offset in pixels.
    TableScrolled(f32),
    WindowResized(Size),
    WindowVisibilityChanged(bool),
    CheckWindowMinimized,
    WindowMinimized(Option<bool>),
    Sidebar(enzui_widgets::nav_split::NavSplitState),

    OpenContextMenu(MenuTarget, Point),
    ContextMenuEvent(enzui_widgets::context_menu_host::ContextMenuEvent),

    RequestAction(PendingAction),
    ConfirmEvent(ConfirmDialogEvent),
    ActionConfirmEvent(u64, ConfirmDialogEvent),
    CancelOwnedAction(u64, u64),
    RetryOwnedAction(u64, u64),
    AcknowledgeActionFailure(u64, u64),
    RunServiceAction(String, &'static str),

    OpenLogs(String),
    /// Accepts journal text only for the matching presentation generation and unit.
    LogsLoaded(u64, String, String),
    CloseLogs,
    Dialog(enzui_widgets::modal_host::DialogEvent<crate::model::DialogKey>),

    FocusNext,
    FocusPrevious,

    /// Copies the payload verbatim and announces only the authored confirmation.
    CopyText {
        value: String,
        confirmation: &'static str,
    },
    CopyFinished(Result<(), iced::clipboard::Error>, &'static str),

    Window(enzui_widgets::title_bar_gesture::WindowCommand),

    OpenShortcuts,
    CloseShortcuts,

    Toast(enzui_widgets::toast_host::ToastEvent),
    /// Hide the status notice with this dismissal key until its state changes.
    DismissNotice(String),

    FocusSearch,
    ToggleTitlebarMenu,
    CloseTitlebarMenu,

    EndSelectedProcess,
}

impl enzui_core::crash_report::MessageKind for Message {
    fn message_kind(&self) -> &'static str {
        match self {
            Self::DesktopActivate => "DesktopActivate",
            Self::ActivityWindowOpened(..) => "ActivityWindowOpened",
            Self::ActivityWindowTagged(..) => "ActivityWindowTagged",
            Self::CollectionFirstFrame => "CollectionFirstFrame",
            Self::CollectionWake => "CollectionWake",
            Self::CollectionTick => "CollectionTick",
            Self::CollectionClose(_) => "CollectionClose",
            Self::KeepActionsOpen(..) => "KeepActionsOpen",
            Self::RetryActionsClose(..) => "RetryActionsClose",
            Self::Theme(..) => "Theme",
            Self::SelectPage(..) => "SelectPage",
            #[cfg(feature = "center")]
            Self::Center(..) => "Center",
            Self::SessionWake => "SessionWake",
            Self::SessionRetry(..) => "SessionRetry",
            Self::SessionKeepOpen(..) => "SessionKeepOpen",
            Self::SelectResource(..) => "SelectResource",
            Self::SelectProcess(..) => "SelectProcess",
            Self::ToggleAppGroup(..) => "ToggleAppGroup",
            Self::SelectService(..) => "SelectService",
            Self::ToggleInspector => "ToggleInspector",
            Self::CloseInspector => "CloseInspector",
            Self::SearchChanged(..) => "SearchChanged",
            Self::TypeToSearch(..) => "TypeToSearch",
            Self::ProcessFilterMenu(..) => "ProcessFilterMenu",
            Self::ServiceFilterMenu(..) => "ServiceFilterMenu",
            Self::SortProcesses(..) => "SortProcesses",
            Self::SortServices(..) => "SortServices",
            Self::Refresh => "Refresh",
            Self::TableWheelScrolled(..) => "TableWheelScrolled",
            Self::TableScrolled(..) => "TableScrolled",
            Self::WindowResized(..) => "WindowResized",
            Self::WindowVisibilityChanged(..) => "WindowVisibilityChanged",
            Self::CheckWindowMinimized => "CheckWindowMinimized",
            Self::WindowMinimized(..) => "WindowMinimized",
            Self::Sidebar(..) => "Sidebar",
            Self::OpenContextMenu(..) => "OpenContextMenu",
            Self::ContextMenuEvent(..) => "ContextMenuEvent",
            Self::RequestAction(..) => "RequestAction",
            Self::ConfirmEvent(..) => "ConfirmEvent",
            Self::ActionConfirmEvent(..) => "ActionConfirmEvent",
            Self::CancelOwnedAction(..) => "CancelOwnedAction",
            Self::RetryOwnedAction(..) => "RetryOwnedAction",
            Self::AcknowledgeActionFailure(..) => "AcknowledgeActionFailure",
            Self::RunServiceAction(..) => "RunServiceAction",
            Self::OpenLogs(..) => "OpenLogs",
            Self::LogsLoaded(..) => "LogsLoaded",
            Self::CloseLogs => "CloseLogs",
            Self::Dialog(..) => "Dialog",
            Self::FocusNext => "FocusNext",
            Self::FocusPrevious => "FocusPrevious",
            Self::CopyText { .. } => "CopyText",
            Self::CopyFinished(..) => "CopyFinished",
            Self::Window(..) => "Window",
            Self::OpenShortcuts => "OpenShortcuts",
            Self::CloseShortcuts => "CloseShortcuts",
            Self::Toast(..) => "Toast",
            Self::DismissNotice(..) => "DismissNotice",
            Self::FocusSearch => "FocusSearch",
            Self::ToggleTitlebarMenu => "ToggleTitlebarMenu",
            Self::CloseTitlebarMenu => "CloseTitlebarMenu",
            Self::EndSelectedProcess => "EndSelectedProcess",
        }
    }
}
