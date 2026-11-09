//! Owned focus and bounded journal reads for the service Logs dialog.

use enzui_core::blocking_task::BlockingTaskError;
use enzui_core::cancellation::{CancelCause, CancelToken};
use enzui_core::modal::DismissCause;
use iced::Task;

use crate::message::Message;
use crate::model::{DialogKey, LogsRequest};
use crate::{App, logs_button_id, services};

impl App {
    pub(crate) fn arm_logs(&mut self, unit: String) -> Task<Message> {
        if self.logs_work_is_closing() {
            return Task::none();
        }
        let Some(generation) = self.model.logs_generation.checked_add(1) else {
            self.model
                .announce_outcome(services::ActionOutcome::warning(
                    "Cannot open Logs: presentation generations exhausted; restart Task Manager",
                ));
            return Task::none();
        };
        self.cancel_logs_request(CancelCause::Cancelled);
        self.model.logs_generation = generation;
        let cancellation = CancelToken::new();
        self.model.logs_request = Some(LogsRequest::new(
            generation,
            unit.clone(),
            cancellation.share(),
        ));
        let open = self
            .model
            .dialogs
            .open(DialogKey::Logs, Some(logs_button_id()));
        self.model.logs_unit.clone_from(&unit);
        self.model.logs_text.clear();
        self.model.logs_loading = true;
        self.model.menu.close();
        // Journal reads share the collection owner’s close-and-join boundary.
        let read = self.model.collections.as_ref().map(|owner| {
            let read_unit = unit.clone();
            owner.detail_dispatcher().submit(
                generation,
                crate::i18n::in_worker(
                    move |context: enzui_core::blocking_task::BlockingTaskContext| {
                        if context.is_cancelled() || cancellation.is_terminal() {
                            return "Journal read cancelled.".to_owned();
                        }
                        services::service_logs(&read_unit, &cancellation)
                    },
                ),
            )
        });
        let load = Task::perform(
            async move {
                let result = match read {
                    Some(read) => read.await.into_result(),
                    None => Err(BlockingTaskError::Shutdown),
                };
                let text = match result {
                    Ok(text) => text,
                    Err(BlockingTaskError::Busy) => {
                        "Journal reader is busy. Close this dialog and open Logs again to retry."
                            .to_owned()
                    }
                    Err(BlockingTaskError::Shutdown) => {
                        "Journal reader is unavailable or closing. Restart Task Manager to retry."
                            .to_owned()
                    }
                    Err(_) => {
                        "Could not read the journal. Close this dialog and open Logs again to retry."
                            .to_owned()
                    }
                };
                (generation, unit, text)
            },
            |(generation, unit, text)| Message::LogsLoaded(generation, unit, text),
        );
        Task::batch([open, load])
    }

    fn logs_work_is_closing(&self) -> bool {
        self.session.is_closing()
            || self.model.actions.is_closing()
            || self
                .model
                .collections
                .as_ref()
                .is_some_and(crate::collection::Collections::is_closing)
    }

    pub(crate) fn apply_logs_text(&mut self, generation: u64, unit: &str, text: String) {
        if self.logs_work_is_closing() {
            return;
        }
        let owns_request = self
            .model
            .logs_request
            .as_ref()
            .is_some_and(|request| request.accepts(generation, unit));
        if owns_request
            && generation == self.model.logs_generation
            && self.model.dialogs.is_open(&DialogKey::Logs)
            && self.model.logs_unit == unit
        {
            if let Some(request) = self.model.logs_request.take() {
                request.complete();
            }
            self.model.logs_text = text;
            self.model.logs_loading = false;
        }
    }

    fn cancel_logs_request(&mut self, cause: CancelCause) {
        if let Some(request) = self.model.logs_request.take() {
            request.cancel(cause);
        }
    }

    pub(crate) fn shutdown_logs(&mut self) {
        self.cancel_logs_request(CancelCause::Shutdown);
        self.clear_logs();
    }

    pub(crate) fn clear_logs(&mut self) {
        self.cancel_logs_request(CancelCause::Cancelled);
        self.model.dialogs.close(&DialogKey::Logs);
        self.model.logs_text.clear();
        self.model.logs_loading = false;
    }

    pub(crate) fn dismiss_logs(&mut self, cause: DismissCause) -> Task<Message> {
        let (_, focus) = self.model.dialogs.dismiss(&DialogKey::Logs, cause);
        self.clear_logs();
        focus
    }
}

#[cfg(test)]
mod tests {}
