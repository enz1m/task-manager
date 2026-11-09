//! Confirmations.

use super::*;

impl App {
    pub(crate) fn on_confirm(&mut self, event: ConfirmDialogEvent) -> Task<Message> {
        if self.model.actions.is_closing() {
            return Task::none();
        }
        if matches!(
            event,
            ConfirmDialogEvent::FocusNext | ConfirmDialogEvent::FocusPrevious
        ) {
            // The confirmation host is outermost; traversal must stay in that host.
            if let Some(op) = event.focus_operation::<Message>(true) {
                return iced::advanced::widget::operate(op);
            }
            return Task::none();
        }
        let Some(confirmed) = confirmation_decision(&event) else {
            return Task::none();
        };
        if !confirmed {
            self.model.actions.confirmation = None;
        }
        let action = match take_allowed_pending_action(&mut self.model, confirmed) {
            Ok(Some(action)) => action,
            Ok(None) => return Task::none(),
            Err(message) => {
                self.model.actions.confirmation = None;
                self.model
                    .announce_outcome(services::ActionOutcome::warning(message));
                return Task::none();
            }
        };

        if !self
            .model
            .actions
            .confirmation_matches(&action, self.model.snapshot())
        {
            self.model.actions.confirmation = None;
            self.model.announce_outcome(services::ActionOutcome::warning(
                "Blocked: captured target UID, unit identity or prior state changed; nothing dispatched",
            ));
            return Task::none();
        }
        self.model.actions.confirm(&action);
        Task::none()
    }
}
