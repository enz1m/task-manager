use crate::{Message, actions::Actions};
use enzui_activity::{Job, Publisher, Status};
use iced::{Subscription, Task, window};

#[derive(Default)]
pub(crate) struct ShellActivity {
    publisher: Option<Publisher>,
    identity: Option<(u64, u64)>,
    job: Option<Job>,
    window: Option<window::Id>,
    pending: Option<String>,
    token: Option<String>,
}

impl ShellActivity {
    pub(crate) fn production() -> Self {
        Self {
            publisher: Publisher::new(crate::DESKTOP_APP_ID).ok(),
            ..Self::default()
        }
    }

    pub(crate) fn sync(&mut self, actions: &Actions) {
        let identity = actions
            .running_identity()
            .filter(|_| self.publisher.is_some());
        if self.identity != identity {
            self.job = None;
            self.identity = identity;
            if identity.is_some()
                && let Some(publisher) = &self.publisher
            {
                self.job = publisher
                    .start_after(
                        Status {
                            window_token: self.token.clone(),
                            ..Status::running(crate::i18n::message("tm-shell-managing-action", &[]))
                        },
                        enzui_activity::Visibility::WhenUnfocused,
                        std::time::Duration::from_millis(800),
                    )
                    .ok();
            }
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        iced::window::open_events().map(Message::ActivityWindowOpened)
    }

    pub(crate) fn bind_window(&mut self, window: window::Id) -> Task<Message> {
        if self.publisher.is_none() || self.window == Some(window) {
            return Task::none();
        }
        self.window = Some(window);
        self.token = None;
        self.pending = enzui_activity::new_window_token().ok();
        let Some(tag) = self.pending.clone() else {
            return Task::none();
        };
        iced::window::set_toplevel_tag(window, tag.clone())
            .map(move |queued| Message::ActivityWindowTagged(window, tag.clone(), queued))
    }

    pub(crate) fn confirm_tag(&mut self, window: window::Id, tag: &str, queued: bool) {
        if self.window == Some(window) && self.pending.as_deref() == Some(tag) {
            self.token = queued.then(|| tag.to_owned());
            self.pending = None;
            if let Some(job) = &self.job {
                let _ = job.set_window_token(self.token.clone());
            }
        }
    }
}
