//! Keyboard-shortcuts presentation in the shared dialog stack.

use crate::{App, message::Message, model::DialogKey};
use iced::Task;

impl App {
    pub(crate) fn open_shortcuts(&mut self) -> Task<Message> {
        self.model.titlebar_menu_open = false;
        self.model.dialogs.open(DialogKey::Shortcuts, None)
    }

    pub(crate) fn dismiss_shortcuts(&mut self) -> Task<Message> {
        self.model
            .dialogs
            .dismiss(
                &DialogKey::Shortcuts,
                enzui_core::modal::DismissCause::CloseButton,
            )
            .1
    }

    pub(crate) fn reduce_dialog(
        &mut self,
        event: enzui_widgets::modal_host::DialogEvent<DialogKey>,
    ) -> Task<Message> {
        let (closed, focus) = self.model.dialogs.update(event);
        if closed.contains(&DialogKey::Logs) {
            self.clear_logs();
        }
        focus
    }
}
