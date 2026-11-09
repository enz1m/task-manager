//! Window close.

use super::*;

impl App {
    pub(crate) fn request_close(&mut self, id: iced::window::Id) -> Task<Message> {
        if !self.session.start_close(id) {
            return Task::none();
        }
        self.shutdown_logs();
        self.model.pending_action = None;
        self.model.menu.close();
        self.model.process_details.invalidate();
        self.invalidate_collections();
        self.poll_collections()
    }

    pub(crate) fn begin_collection_close(&mut self, id: iced::window::Id) {
        self.invalidate_collections();
        let owner = self
            .model
            .collections
            .get_or_insert_with(collection::Collections::system);
        owner.close(id, std::time::Instant::now());
    }

    pub(crate) fn remember_page(&mut self) -> Task<Message> {
        if !self.model.actions.is_closing() {
            self.session.remember(session::Session {
                page: Some(self.model.active),
            });
        }
        Task::none()
    }
}
