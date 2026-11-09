//! Update entry.

use super::*;

impl App {
    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        // Record the payload-free kind before the reducer can reroute or refuse it.
        enzui_core::crash_report::observe(&message);
        let task = self.reduce(message);
        self.poll_process_details();
        // Reconcile expiry after every reducer path, including early returns.
        self.model
            .toasts
            .sync_expiry(&mut self.model.toast_expiry, std::time::Instant::now());
        self.shell_activity.sync(&self.model.actions);
        self.publish_lifecycle();
        let stop = self.lifecycle.as_ref().and_then(|control| {
            control
                .take_stop()
                .filter(|request| control.is_current(request))
        });
        let mut close = false;
        if let Some(request) = stop {
            match request.choice {
                enzui_core::activation::lifecycle::StopChoice::FinishThenExit => {
                    self.finish_then_exit = true;
                }
                enzui_core::activation::lifecycle::StopChoice::CancelAndExit => close = true,
            }
        }
        if self.finish_then_exit && !self.model.actions.blocks_close() {
            self.finish_then_exit = false;
            close = true;
        }
        let stop_task = if close {
            Task::done(Message::Window(
                enzui_widgets::title_bar_gesture::WindowCommand::Close,
            ))
        } else {
            Task::none()
        };
        Task::batch([task, self.flight_wakes(), stop_task])
    }

    pub(crate) fn publish_lifecycle(&self) {
        let Some(control) = &self.lifecycle else {
            return;
        };
        let blocked = self.model.actions.blocks_close();
        let consequences = if blocked {
            self.model
                .actions
                .status()
                .map(|(generation, attempt, title, _, _)| {
                    enzui_core::activation::lifecycle::StopConsequence::new(
                        format!("action-{generation}-{attempt}"),
                        "process-or-service",
                        title,
                    )
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        control.publish(consequences);
        control.publish_work(blocked.then_some(enzui_core::activation::lifecycle::WorkKind::Other));
    }

    pub(crate) fn flight_wakes(&mut self) -> Task<Message> {
        let wakes = self
            .session
            .take_flight_wakes()
            .into_iter()
            .chain(self.model.process_details.take_flight_wakes());
        Task::batch(wakes.map(|wake| Task::perform(wake, |()| Message::CollectionWake)))
    }
}
